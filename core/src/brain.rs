//! Le « savoir-vivre » de l'agent en réunion : quand prendre la parole, quand se taire.
//!
//! Comme un humain :
//! * **En retrait** (`Idle`) : il écoute tout (le modèle garde le contexte de la réunion) mais ne
//!   parle que si on l'appelle par son prénom / surnom / nom.
//! * **En conversation** (`Engaged`) : une fois interpellé, il considère que les prises de parole
//!   suivantes lui sont adressées, sans qu'on répète son nom et **sans limite de temps**. Il y
//!   répond dès la fin de chaque phrase. C'est le modèle lui-même qui juge, phrase par phrase :
//!   s'il estime qu'on ne lui parle pas, il appelle l'outil `rester_silencieux` ; si la discussion
//!   est passée à autre chose (« merci Nova », « Paul, à toi »), il appelle `terminer_echange` et
//!   repasse en retrait.
//! * **Interruption** : si quelqu'un parle par-dessus lui plus de `barge_in_ms`, il se tait
//!   aussitôt (les « hm », toux, etc. plus courts sont ignorés). Pas de limite de longueur à ses
//!   réponses : c'est aux participants de le couper.

use serde_json::{json, Value};

use crate::names::NameDetector;

pub const TOOL_SILENT: &str = "rester_silencieux";
pub const TOOL_END: &str = "terminer_echange";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Idle,
    Engaged,
}

#[derive(Debug, PartialEq)]
pub enum Action {
    /// Événement JSON à envoyer à l'API Realtime.
    Send(Value),
    /// Couper immédiatement la voix en cours.
    FlushPlayback,
    /// Début d'une nouvelle réponse audio (remise à zéro du compteur de lecture).
    StartItem,
    /// Rappeler `on_barge_in_timer(gen)` dans `barge_in_ms`.
    ArmBargeIn { gen: u64, after_ms: u64 },
    Log(String),
}

pub struct Brain {
    pub mode: Mode,
    names: NameDetector,
    agent_name: String,
    barge_in_ms: u64,
    response_active: bool,
    audio_item: Option<String>,
    someone_speaking: bool,
    barge_gen: u64,
    /// Éléments (tours de parole) pour lesquels on a déjà déclenché une réponse.
    triggered: Vec<String>,
    transcripts: std::collections::HashMap<String, String>,
}

impl Brain {
    pub fn new(agent_name: &str, names: NameDetector, barge_in_ms: u64) -> Self {
        Self {
            mode: Mode::Idle,
            names,
            agent_name: agent_name.to_string(),
            barge_in_ms,
            response_active: false,
            audio_item: None,
            someone_speaking: false,
            barge_gen: 0,
            triggered: Vec::new(),
            transcripts: Default::default(),
        }
    }

    pub fn tools() -> Value {
        json!([
            {
                "type": "function",
                "name": TOOL_SILENT,
                "description": "Appelle cet outil AU LIEU DE PARLER quand la dernière prise de parole ne t'est pas adressée \
(les participants se parlent entre eux, réfléchissent à voix haute, bruit…). Tu restes dans l'échange et tu continues d'écouter.",
                "parameters": {"type": "object", "properties": {}}
            },
            {
                "type": "function",
                "name": TOOL_END,
                "description": "Appelle cet outil AU LIEU DE PARLER quand ton échange avec les participants est clairement terminé : \
on te remercie pour clore, on s'adresse maintenant à quelqu'un d'autre, ou la réunion est passée à un autre sujet sans toi. \
Ensuite tu ne parleras plus tant qu'on ne t'appellera pas à nouveau par ton nom.",
                "parameters": {"type": "object", "properties": {}}
            }
        ])
    }

    fn create_response(&mut self, item_id: Option<&str>) -> Vec<Action> {
        if let Some(id) = item_id {
            self.triggered.push(id.to_string());
            if self.triggered.len() > 64 {
                self.triggered.remove(0);
            }
        }
        self.response_active = true;
        vec![Action::Send(json!({"type": "response.create"}))]
    }

    fn interrupt(&mut self, played_ms: u64) -> Vec<Action> {
        let mut out = vec![Action::FlushPlayback];
        if self.response_active {
            out.push(Action::Send(json!({"type": "response.cancel"})));
        }
        if let Some(item) = self.audio_item.take() {
            // Le modèle doit savoir ce qui a VRAIMENT été entendu avant la coupure.
            out.push(Action::Send(json!({
                "type": "conversation.item.truncate",
                "item_id": item,
                "content_index": 0,
                "audio_end_ms": played_ms,
            })));
        }
        self.response_active = false;
        out.push(Action::Log(format!("✋ interrompu après {played_ms} ms")));
        out
    }

    /// Le minuteur d'interruption a expiré.
    pub fn on_barge_in_timer(&mut self, gen: u64, ai_playing: bool, played_ms: u64) -> Vec<Action> {
        if gen != self.barge_gen || !self.someone_speaking || !(ai_playing || self.response_active) {
            return vec![];
        }
        self.interrupt(played_ms)
    }

    /// Traite un événement serveur. `ai_playing` : de la voix est encore en file d'attente.
    pub fn on_event(&mut self, ev: &Value, ai_playing: bool) -> Vec<Action> {
        let t = ev["type"].as_str().unwrap_or("");
        match t {
            "response.output_audio.delta" | "response.audio.delta" => {
                let id = ev["item_id"].as_str().map(String::from);
                if id.is_some() && id != self.audio_item {
                    self.audio_item = id;
                    return vec![Action::StartItem];
                }
                vec![]
            }

            "input_audio_buffer.speech_started" => {
                self.someone_speaking = true;
                self.barge_gen += 1;
                if ai_playing || self.response_active {
                    vec![Action::ArmBargeIn { gen: self.barge_gen, after_ms: self.barge_in_ms }]
                } else {
                    vec![]
                }
            }

            "input_audio_buffer.speech_stopped" => {
                self.someone_speaking = false;
                self.barge_gen += 1; // annule une interruption en attente (c'était un « hm »)
                vec![]
            }

            // Fin d'une phrase : en conversation, on répond tout de suite (latence minimale,
            // le modèle a l'audio ; il choisira de se taire via l'outil si ce n'est pas pour lui).
            "input_audio_buffer.committed" => {
                let id = ev["item_id"].as_str();
                if self.mode == Mode::Engaged && !self.response_active && !ai_playing {
                    return self.create_response(id);
                }
                vec![]
            }

            // En retrait : on guette son nom dans la transcription, mot à mot.
            "conversation.item.input_audio_transcription.delta"
            | "conversation.item.input_audio_transcription.completed" => {
                let id = ev["item_id"].as_str().unwrap_or("").to_string();
                let text = if t.ends_with("delta") {
                    let e = self.transcripts.entry(id.clone()).or_default();
                    e.push_str(ev["delta"].as_str().unwrap_or(""));
                    e.clone()
                } else {
                    let full = ev["transcript"].as_str().unwrap_or("").trim().to_string();
                    self.transcripts.remove(&id);
                    full
                };
                let mut out = vec![];
                if t.ends_with("completed") && !text.is_empty() {
                    out.push(Action::Log(format!("🎙  {text}")));
                }
                if self.mode == Mode::Idle
                    && !self.response_active
                    && !self.triggered.contains(&id)
                    && self.names.mentioned(&text)
                {
                    self.mode = Mode::Engaged;
                    out.push(Action::Log(format!("👋 on appelle {} → il entre dans la conversation", self.agent_name)));
                    out.extend(self.create_response(Some(&id)));
                }
                out
            }

            "response.created" => {
                self.response_active = true;
                vec![]
            }

            "response.done" => {
                self.response_active = false;
                let mut out = vec![];
                let items = ev["response"]["output"].as_array().cloned().unwrap_or_default();
                for item in items.iter().filter(|i| i["type"] == "function_call") {
                    let name = item["name"].as_str().unwrap_or("");
                    if name == TOOL_END {
                        self.mode = Mode::Idle;
                        out.push(Action::Log(format!("💤 fin de l'échange, {} se met en retrait", self.agent_name)));
                    }
                    // On referme l'appel d'outil dans la conversation, sans relancer de réponse.
                    out.push(Action::Send(json!({
                        "type": "conversation.item.create",
                        "item": {"type": "function_call_output", "call_id": item["call_id"], "output": "{\"ok\":true}"}
                    })));
                }
                out
            }

            "response.output_audio_transcript.done" | "response.audio_transcript.done" => {
                vec![Action::Log(format!("🤖 {} : {}", self.agent_name, ev["transcript"].as_str().unwrap_or("")))]
            }

            "error" => {
                let code = ev["error"]["code"].as_str().unwrap_or("");
                if code == "response_cancel_not_active" {
                    vec![]
                } else {
                    vec![Action::Log(format!("❌ erreur Realtime : {}", ev["error"]))]
                }
            }
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brain() -> Brain {
        Brain::new("Nova", NameDetector::new(["Nova", "Martin"]), 400)
    }
    fn sends(actions: &[Action]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Send(v) => Some(v["type"].as_str().unwrap().to_string()),
                _ => None,
            })
            .collect()
    }
    fn delta(id: &str, d: &str) -> Value {
        json!({"type": "conversation.item.input_audio_transcription.delta", "item_id": id, "delta": d})
    }

    #[test]
    fn reste_en_retrait_tant_quon_ne_lappelle_pas() {
        let mut b = brain();
        assert!(sends(&b.on_event(&json!({"type":"input_audio_buffer.committed","item_id":"a"}), false)).is_empty());
        assert!(sends(&b.on_event(&delta("a", "on reprend le budget"), false)).is_empty());
        assert_eq!(b.mode, Mode::Idle);
    }

    #[test]
    fn repond_des_quon_dit_son_nom_puis_reste_en_conversation_sans_limite() {
        let mut b = brain();
        let a = b.on_event(&delta("a", "Nova, tu"), false);
        assert_eq!(sends(&a), ["response.create"]);
        assert_eq!(b.mode, Mode::Engaged);
        // la suite de la même phrase ne relance pas une 2e réponse
        b.on_event(&json!({"type":"response.created"}), false);
        assert!(sends(&b.on_event(&delta("a", " peux résumer ?"), false)).is_empty());
        b.on_event(&json!({"type":"response.done","response":{"output":[]}}), false);
        // question suivante, sans le nom : il répond dès la fin de phrase
        let a = b.on_event(&json!({"type":"input_audio_buffer.committed","item_id":"b"}), false);
        assert_eq!(sends(&a), ["response.create"]);
        assert_eq!(b.mode, Mode::Engaged);
    }

    #[test]
    fn outil_terminer_echange_le_remet_en_retrait() {
        let mut b = brain();
        b.on_event(&delta("a", "Nova ?"), false);
        let done = json!({"type":"response.done","response":{"output":[{"type":"function_call","name":TOOL_END,"call_id":"c1"}]}});
        let a = b.on_event(&done, false);
        assert_eq!(sends(&a), ["conversation.item.create"]);
        assert_eq!(b.mode, Mode::Idle);
        assert!(sends(&b.on_event(&json!({"type":"input_audio_buffer.committed","item_id":"z"}), false)).is_empty());
    }

    #[test]
    fn outil_silence_garde_la_conversation() {
        let mut b = brain();
        b.on_event(&delta("a", "Nova ?"), false);
        let done = json!({"type":"response.done","response":{"output":[{"type":"function_call","name":TOOL_SILENT,"call_id":"c1"}]}});
        b.on_event(&done, false);
        assert_eq!(b.mode, Mode::Engaged);
    }

    #[test]
    fn interruption_seulement_si_on_parle_assez_longtemps() {
        let mut b = brain();
        b.on_event(&delta("a", "Nova ?"), false);
        b.on_event(&json!({"type":"response.created"}), false);
        assert_eq!(b.on_event(&json!({"type":"response.output_audio.delta","item_id":"r1","delta":""}), true), [Action::StartItem]);

        // un « hm » bref : pas d'interruption
        let a = b.on_event(&json!({"type":"input_audio_buffer.speech_started"}), true);
        let Action::ArmBargeIn { gen, .. } = a[0] else { panic!() };
        b.on_event(&json!({"type":"input_audio_buffer.speech_stopped"}), true);
        assert!(b.on_barge_in_timer(gen, true, 900).is_empty());
        // pendant qu'il parle, une fin de phrase ne déclenche pas de nouvelle réponse
        assert!(sends(&b.on_event(&json!({"type":"input_audio_buffer.committed","item_id":"h"}), true)).is_empty());

        // quelqu'un lui coupe vraiment la parole
        let a = b.on_event(&json!({"type":"input_audio_buffer.speech_started"}), true);
        let Action::ArmBargeIn { gen, .. } = a[0] else { panic!() };
        let a = b.on_barge_in_timer(gen, true, 1500);
        assert_eq!(a[0], Action::FlushPlayback);
        assert_eq!(sends(&a), ["response.cancel", "conversation.item.truncate"]);
        // puis on répond à la personne qui a coupé
        b.on_event(&json!({"type":"input_audio_buffer.speech_stopped"}), false);
        let a = b.on_event(&json!({"type":"input_audio_buffer.committed","item_id":"i"}), false);
        assert_eq!(sends(&a), ["response.create"]);
    }
}
