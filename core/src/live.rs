//! Moteur **Live** : `gpt-live-1`, le modèle vraiment full duplex d'OpenAI.
//!
//! Contrairement à Realtime, il n'y a pas de « tours » : le modèle écoute et parle en même temps,
//! gère lui-même le rythme, les relances et les interruptions. Le protocole (repris de loquivox) :
//! `wss://api.openai.com/v1/live/sessions`, `session.start` puis attendre `session.started`, un
//! flux audio d'entrée **continu** (des zéros plutôt que rien), une sortie audio en temps réel par
//! trames de 100 ms (silence entre les réponses), des transcriptions par fragments sans fin de tour.
//!
//! En réunion, on garde la main avec une **porte** sur sa voix (`--gate name`) :
//! * **En retrait** : sa voix n'est pas transmise à la réunion. S'il parle quand même, on lui
//!   glisse en pensée silencieuse que personne ne l'a entendu et qu'il attende son nom.
//! * **Appelé par son prénom / surnom / nom** (repéré au fil de la transcription) : la porte
//!   s'ouvre, on lui signale que c'est à lui. Elle reste ouverte **sans limite de temps** tant que
//!   l'échange continue : les questions suivantes n'ont pas besoin de son nom, et il parle aussi
//!   longtemps qu'il le juge utile — on l'interrompt si besoin (le modèle s'arrête de lui-même).
//! * **Fin de l'échange** comme entre humains : on le congédie (« merci Nova », « c'est bon
//!   Nova »), ou les autres continuent entre eux un bon moment sans lui adresser la parole.

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
use tracing::{debug, info, warn};

use crate::audio::{self, Audio, Uplink};
use crate::config::Config;
use crate::names::{normalize, NameDetector};

/// Un fragment de transcription après un tel blanc commence une nouvelle prise de parole.
const TURN_GAP: Duration = Duration::from_millis(1200);
/// Au plus une pensée « personne ne t'a entendu » toutes les…
const HUSH_EVERY: Duration = Duration::from_secs(20);
/// Après « merci Nova », on le laisse encore répondre brièvement (« avec plaisir »).
const DISMISS_GRACE: Duration = Duration::from_secs(4);
/// Limite d'un `session.thinking.append` (500 tokens) : budget pessimiste en caractères.
const APPEND_CHARS: usize = 1500;
/// Historique texte repassé à une nouvelle session après une reconnexion (≈ 8 k tokens max).
const HISTORY_CHARS: usize = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Idle,
    Engaged,
}

#[derive(Debug, PartialEq)]
pub enum LiveAction {
    /// Pensée silencieuse pour le modèle (`session.thinking.append`).
    Think(String),
    Log(String),
}

/// Décide quand la voix de l'agent atteint la réunion. Pure (horloge injectée) : testable.
pub struct LiveBrain {
    pub mode: Mode,
    gate: bool,
    names: NameDetector,
    dismiss: Vec<Vec<String>>,
    agent_name: String,
    disengage: Duration,
    /// Ce que dit la personne en train de parler (fenêtre glissante).
    heard: String,
    last_heard: Option<Instant>,
    /// Temps de parole des autres depuis la dernière fois que l'agent a parlé.
    others_since_agent: Duration,
    idle_at: Option<Instant>,
    last_hush: Option<Instant>,
}

impl LiveBrain {
    pub fn new(cfg: &Config) -> Self {
        // « merci Nova », « c'est bon Nova », « ça ira Nova »… : on le congédie poliment.
        let dismiss = ["merci", "c est bon", "ca ira", "ca suffit", "c est tout", "stop", "chut", "thanks", "thank you"]
            .iter()
            .map(|p| p.split(' ').map(String::from).collect())
            .collect();
        Self {
            mode: Mode::Idle,
            gate: cfg.gate == "name",
            names: NameDetector::new(cfg.all_names()),
            dismiss,
            agent_name: cfg.name.clone(),
            disengage: Duration::from_secs(cfg.disengage_seconds),
            heard: String::new(),
            last_heard: None,
            others_since_agent: Duration::ZERO,
            idle_at: None,
            last_hush: None,
        }
    }

    fn is_dismissal(&self, text: &str) -> bool {
        let norm = normalize(text);
        let words: Vec<&str> = norm.split(' ').collect();
        self.dismiss.iter().any(|p| words.windows(p.len()).any(|w| w.iter().zip(p).all(|(a, b)| a == b)))
    }

    fn go_idle(&mut self, why: &str) -> Vec<LiveAction> {
        self.mode = Mode::Idle;
        self.idle_at = None;
        if !self.gate {
            return vec![];
        }
        vec![
            LiveAction::Log(format!("💤 {} se met en retrait ({why})", self.agent_name)),
            LiveAction::Think(format!(
                "L'échange avec toi est terminé ({why}). Les participants continuent entre eux : reste \
silencieux et écoute, jusqu'à ce que quelqu'un t'appelle de nouveau par ton nom."
            )),
        ]
    }

    /// Fragment de transcription de la réunion (ce que disent les participants).
    pub fn on_heard(&mut self, delta: &str, span_ms: u64, now: Instant) -> Vec<LiveAction> {
        let mut out = vec![];
        if self.last_heard.is_none_or(|t| now.duration_since(t) > TURN_GAP) {
            self.heard.clear();
        }
        self.last_heard = Some(now);
        self.heard.push_str(delta);
        if self.heard.len() > 400 {
            let cut = self.heard.len() - 300;
            let cut = (cut..self.heard.len()).find(|i| self.heard.is_char_boundary(*i)).unwrap_or(0);
            self.heard.drain(..cut);
        }

        let named = self.names.mentioned(&self.heard);
        match self.mode {
            Mode::Idle if named && self.idle_at.is_none() => {
                // Pas d'entrée en conversation sur « merci Nova » : c'est un au revoir.
                if !self.is_dismissal(&self.heard) {
                    self.mode = Mode::Engaged;
                    self.others_since_agent = Duration::ZERO;
                    self.heard.clear();
                    if self.gate {
                        out.push(LiveAction::Log(format!("👋 on appelle {} → il prend part à l'échange", self.agent_name)));
                        out.push(LiveAction::Think(format!(
                            "Quelqu'un vient de t'appeler par ton nom : c'est à toi qu'on s'adresse, la réunion \
t'entend maintenant. Réponds-lui."
                        )));
                    }
                }
            }
            Mode::Engaged => {
                if named && self.is_dismissal(&self.heard) {
                    self.idle_at = Some(now + DISMISS_GRACE);
                    self.heard.clear();
                } else {
                    self.others_since_agent += Duration::from_millis(span_ms);
                    if self.others_since_agent >= self.disengage {
                        out.extend(self.go_idle("les autres poursuivent sans lui"));
                    }
                }
            }
            _ => {}
        }
        out
    }

    /// Une trame audio de l'agent : la transmettre à la réunion ?
    pub fn on_agent_audio(&mut self, audible: bool, now: Instant) -> (bool, Vec<LiveAction>) {
        let mut out = self.tick(now);
        if !self.gate || self.mode == Mode::Engaged {
            if audible {
                self.others_since_agent = Duration::ZERO;
            }
            return (true, out);
        }
        if audible && self.last_hush.is_none_or(|t| now.duration_since(t) > HUSH_EVERY) {
            self.last_hush = Some(now);
            out.push(LiveAction::Log("🤫 il a voulu parler sans qu'on l'appelle : coupé".into()));
            out.push(LiveAction::Think(
                "Personne ne t'a adressé la parole : ce que tu viens de dire n'a PAS été entendu par la réunion. \
Reste silencieux et continue d'écouter jusqu'à ce qu'on t'appelle par ton nom."
                    .into(),
            ));
        }
        (false, out)
    }

    pub fn tick(&mut self, now: Instant) -> Vec<LiveAction> {
        match self.idle_at {
            Some(t) if now >= t => self.go_idle("on l'a remercié"),
            _ => vec![],
        }
    }
}

// --------------------------------------------------------------------------------- session

fn session_config(cfg: &Config, history: &[(String, String)]) -> Value {
    let mut session = json!({
        "model": cfg.live_model,
        "instructions": cfg.instructions(),
        "audio": {
            "format": {"type": "audio/pcm", "rate": audio::SAMPLE_RATE},
            "output": {"voice": cfg.voice}
        },
    });
    if cfg.delegation == "responses" {
        session["delegation"] = json!({
            "type": "responses",
            "responses": {
                "model": cfg.backend_model,
                "instructions": cfg.backend_instructions(),
                "tools": [{"type": "web_search"}],
                "tool_choice": "auto"
            }
        });
    } else {
        session["delegation"] = json!({"type": "client"});
    }
    if !history.is_empty() {
        let items: Vec<Value> = history
            .iter()
            .map(|(role, text)| {
                let part = if role == "assistant" { "output_text" } else { "input_text" };
                json!({"type": "message", "role": role, "content": [{"type": part, "text": text}]})
            })
            .collect();
        session["input"] = Value::Array(items);
    }
    session
}

/// Transcriptions regroupées en prises de parole (pour les logs et l'historique de reconnexion).
#[derive(Default)]
struct Transcript {
    turns: Vec<(String, String)>,
    current: Option<(String, String, Instant)>,
}

impl Transcript {
    fn push(&mut self, role: &str, delta: &str, now: Instant) -> Option<(String, String)> {
        let mut closed = None;
        match &mut self.current {
            Some((r, text, at)) if r == role && now.duration_since(*at) <= TURN_GAP => {
                text.push_str(delta);
                *at = now;
            }
            _ => {
                closed = self.flush();
                self.current = Some((role.to_string(), delta.to_string(), now));
            }
        }
        closed
    }
    fn flush_if_stale(&mut self, now: Instant) -> Option<(String, String)> {
        match &self.current {
            Some((_, _, at)) if now.duration_since(*at) > TURN_GAP => self.flush(),
            _ => None,
        }
    }
    fn flush(&mut self) -> Option<(String, String)> {
        let (role, text, _) = self.current.take()?;
        let text = text.trim().to_string();
        if text.is_empty() {
            return None;
        }
        self.turns.push((role.clone(), text.clone()));
        Some((role, text))
    }
    /// Les dernières prises de parole qui tiennent dans le budget.
    fn history(&self) -> Vec<(String, String)> {
        let mut budget = HISTORY_CHARS;
        let mut out: Vec<_> = self
            .turns
            .iter()
            .rev()
            .take(120)
            .take_while(|(_, t)| {
                let ok = t.len() <= budget;
                budget = budget.saturating_sub(t.len());
                ok
            })
            .cloned()
            .collect();
        out.reverse();
        out
    }
}

fn chunks(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    for sentence in text.split_inclusive(['.', '!', '?', '\n']) {
        if cur.len() + sentence.len() > APPEND_CHARS && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.push_str(sentence);
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

fn think_events(text: &str) -> Vec<Value> {
    chunks(text)
        .into_iter()
        .map(|c| json!({"type": "session.thinking.append", "content": c, "delegation_id": null}))
        .collect()
}

/// Boucle principale ; se reconnecte (avec l'historique) si la session expire ou se coupe.
pub async fn run(cfg: &Config, mut audio: Audio) -> Result<()> {
    let mut brain = LiveBrain::new(cfg);
    let mut transcript = Transcript::default();
    let mut uplink = Uplink::default();
    let mut attempts = 0u32;
    loop {
        let closed_reason = session(cfg, &mut audio, &mut brain, &mut transcript, &mut uplink).await;
        match closed_reason {
            Ok(Some(reason)) if reason == "close_requested" => return Ok(()),
            Ok(None) => return Ok(()), // Ctrl+C
            Ok(Some(reason)) => {
                warn!("Session Live fermée ({reason}) : reconnexion avec l'historique de la réunion…");
                attempts = 0;
            }
            Err(e) => {
                attempts += 1;
                if attempts > 5 {
                    return Err(e);
                }
                warn!("Session Live interrompue : {e:#} — nouvel essai dans {attempts} s");
                tokio::time::sleep(Duration::from_secs(attempts as u64)).await;
            }
        }
    }
}

/// Une session. `Ok(None)` = arrêt demandé (Ctrl+C), `Ok(Some(raison))` = fermée par le serveur.
async fn session(
    cfg: &Config,
    audio: &mut Audio,
    brain: &mut LiveBrain,
    transcript: &mut Transcript,
    uplink: &mut Uplink,
) -> Result<Option<String>> {
    let url = format!("{}/live/sessions", cfg.api_base.trim_end_matches('/'));
    let mut req = url.as_str().into_client_request()?;
    req.headers_mut().insert("Authorization", format!("Bearer {}", cfg.api_key).parse()?);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.context("connexion à l'API Live")?;
    let (mut tx, mut rx) = ws.split();
    let history = transcript.history();
    tx.send(Message::text(json!({"type": "session.start", "session": session_config(cfg, &history)}).to_string()))
        .await?;
    info!("🌐 Connecté à {url} ({}, voix {}, délégation {})", cfg.live_model, cfg.voice, cfg.delegation);

    let mut started = false;
    let mut ticker = tokio::time::interval(Duration::from_millis(audio::UPLINK_MS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let playback = audio.playback.clone();

    loop {
        let mut outgoing: Vec<Value> = vec![];
        let actions: Vec<LiveAction> = tokio::select! {
            Some(samples) = audio.capture.recv() => { uplink.feed(&samples); vec![] }

            _ = ticker.tick() => {
                let now = Instant::now();
                if started {
                    for block in uplink.tick() {
                        outgoing.push(json!({"type": "session.input_audio.append", "audio": B64.encode(block)}));
                    }
                }
                if let Some((role, text)) = transcript.flush_if_stale(now) { log_turn(cfg, &role, &text); }
                brain.tick(now)
            }

            msg = rx.next() => {
                let Some(msg) = msg else { bail!("connexion fermée") };
                let text = match msg? {
                    Message::Text(t) => t,
                    Message::Close(f) => bail!("connexion fermée : {f:?}"),
                    _ => continue,
                };
                let ev: Value = serde_json::from_str(&text)?;
                let now = Instant::now();
                let t = ev["type"].as_str().unwrap_or("");
                match t {
                    "session.output_audio.delta" => {
                        let bytes = B64.decode(ev["delta"].as_str().unwrap_or(""))?;
                        let samples: Vec<i16> = bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
                        let (forward, acts) = brain.on_agent_audio(audio::is_audible(&samples), now);
                        if forward { playback.push(&samples); }
                        acts
                    }
                    "session.input_transcript.delta" => {
                        let delta = ev["delta"].as_str().unwrap_or("");
                        let span = ev["end_ms"].as_u64().unwrap_or(0).saturating_sub(ev["start_ms"].as_u64().unwrap_or(0));
                        if let Some((role, text)) = transcript.push("user", delta, now) { log_turn(cfg, &role, &text); }
                        brain.on_heard(delta, span, now)
                    }
                    "session.output_transcript.delta" => {
                        // Ce qu'il a dit en retrait n'a pas été entendu : on ne l'inscrit pas.
                        if brain.mode == Mode::Engaged || cfg.gate != "name" {
                            if let Some((role, text)) = transcript.push("assistant", ev["delta"].as_str().unwrap_or(""), now) {
                                log_turn(cfg, &role, &text);
                            }
                        }
                        vec![]
                    }
                    "session.started" => {
                        started = true;
                        info!("✅ Session Live ouverte — {} écoute la réunion (appelez-le par son nom)", cfg.name);
                        vec![]
                    }
                    "session.delegation.created" => {
                        let target = ev["delegation"]["target"].as_str().unwrap_or("");
                        info!("🧠 réflexion déléguée ({target})");
                        if target == "client" {
                            // Pas de cerveau côté client pour l'instant : on le dit au modèle,
                            // sinon il attendrait une réponse indéfiniment.
                            outgoing.push(json!({
                                "type": "session.thinking.append",
                                "delegation_id": ev["delegation"]["id"],
                                "content": "Aucun outil n'est disponible pour cette demande : réponds avec ce que tu sais, \
ou dis simplement que tu ne peux pas le vérifier maintenant."
                            }));
                        }
                        vec![]
                    }
                    "session.closed" => {
                        if let Some((role, text)) = transcript.flush() { log_turn(cfg, &role, &text); }
                        let reason = ev["reason"].as_str().unwrap_or("?").to_string();
                        info!("🔚 Session Live fermée ({reason}) — {} s d'audio", ev["usage"]["seconds"]);
                        return Ok(Some(reason));
                    }
                    "error" => vec![LiveAction::Log(format!("❌ erreur Live : {}", ev["error"]))],
                    "info" => { info!("ℹ️  {} : {}", ev["code"], ev["message"]); vec![] }
                    _ => { debug!("← {t}"); vec![] }
                }
            }

            _ = tokio::signal::ctrl_c() => {
                info!("Au revoir 👋");
                // Fermeture documentée : on demande, puis on attend `session.closed` (bilan d'usage).
                let _ = tx.send(Message::text(json!({"type": "session.close"}).to_string())).await;
                let wait = async {
                    while let Some(Ok(Message::Text(t))) = rx.next().await {
                        let ev: Value = serde_json::from_str(&t).unwrap_or_default();
                        if ev["type"] == "session.closed" {
                            info!("🔚 Session Live fermée — {} s d'audio", ev["usage"]["seconds"]);
                            break;
                        }
                    }
                };
                let _ = tokio::time::timeout(Duration::from_secs(3), wait).await;
                let _ = tx.close().await;
                return Ok(None);
            }
        };

        for a in actions {
            match a {
                LiveAction::Think(text) => outgoing.extend(think_events(&text)),
                LiveAction::Log(s) if s.starts_with('❌') => warn!("{s}"),
                LiveAction::Log(s) => info!("{s}"),
            }
        }
        for ev in outgoing {
            tx.send(Message::text(ev.to_string())).await?;
        }
    }
}

fn log_turn(cfg: &Config, role: &str, text: &str) {
    if role == "assistant" {
        info!("🤖 {} : {text}", cfg.name);
    } else {
        info!("🎙  {text}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn cfg(gate: &str) -> Config {
        Config::parse_from(["fdm", "--name", "Nova", "--aliases", "Martin", "--gate", gate, "--disengage-seconds", "60"])
    }
    fn thinks(a: &[LiveAction]) -> usize {
        a.iter().filter(|x| matches!(x, LiveAction::Think(_))).count()
    }

    #[test]
    fn porte_fermee_tant_quon_ne_lappelle_pas() {
        let mut b = LiveBrain::new(&cfg("name"));
        let t0 = Instant::now();
        b.on_heard("On reprend le budget.", 1500, t0);
        let (fwd, acts) = b.on_agent_audio(true, t0);
        assert!(!fwd);
        assert_eq!(thinks(&acts), 1); // on lui dit que personne ne l'a entendu
        let (_, acts) = b.on_agent_audio(true, t0 + Duration::from_secs(1));
        assert_eq!(thinks(&acts), 0); // pas à chaque trame
        assert!(b.on_agent_audio(false, t0).0 == false);
    }

    #[test]
    fn son_nom_ouvre_la_porte_sans_limite_de_temps() {
        let mut b = LiveBrain::new(&cfg("name"));
        let t0 = Instant::now();
        let acts = b.on_heard("Dis-moi Mar", 500, t0);
        assert!(acts.is_empty());
        let acts = b.on_heard("tin, tu en penses quoi ?", 900, t0 + Duration::from_millis(300));
        assert_eq!(b.mode, Mode::Engaged);
        assert_eq!(thinks(&acts), 1);
        // il parle 10 minutes d'affilée : toujours transmis
        assert!(b.on_agent_audio(true, t0 + Duration::from_secs(600)).0);
        // question suivante sans son nom, beaucoup plus tard : toujours en conversation
        b.on_heard("Et pour le planning ?", 1200, t0 + Duration::from_secs(1200));
        assert_eq!(b.mode, Mode::Engaged);
        assert!(b.on_agent_audio(true, t0 + Duration::from_secs(1201)).0);
    }

    #[test]
    fn merci_nova_le_remet_en_retrait_apres_un_court_delai() {
        let mut b = LiveBrain::new(&cfg("name"));
        let t0 = Instant::now();
        b.on_heard("Nova, une question", 900, t0);
        b.on_heard("Merci Nova, c'est parfait", 900, t0 + Duration::from_secs(10));
        assert_eq!(b.mode, Mode::Engaged);
        assert!(b.on_agent_audio(true, t0 + Duration::from_secs(11)).0); // « avec plaisir »
        let (fwd, _) = b.on_agent_audio(true, t0 + Duration::from_secs(15));
        assert!(!fwd);
        assert_eq!(b.mode, Mode::Idle);
        // « merci Nova » en retrait ne le réveille pas
        b.on_heard("merci Nova", 500, t0 + Duration::from_secs(30));
        assert_eq!(b.mode, Mode::Idle);
    }

    #[test]
    fn les_autres_continuent_longtemps_sans_lui() {
        let mut b = LiveBrain::new(&cfg("name"));
        let t0 = Instant::now();
        b.on_heard("Nova ?", 500, t0);
        let mut t = t0;
        for _ in 0..50 {
            t += Duration::from_secs(2);
            b.on_heard("blabla entre collègues", 1500, t);
        }
        assert_eq!(b.mode, Mode::Idle);
    }

    #[test]
    fn porte_desactivee() {
        let mut b = LiveBrain::new(&cfg("off"));
        let (fwd, acts) = b.on_agent_audio(true, Instant::now());
        assert!(fwd && acts.is_empty());
    }

    #[test]
    fn transcript_regroupe_et_historique() {
        let mut tr = Transcript::default();
        let t0 = Instant::now();
        assert!(tr.push("user", "Bonjour ", t0).is_none());
        assert!(tr.push("user", "à tous", t0 + Duration::from_millis(500)).is_none());
        let closed = tr.push("assistant", "Salut", t0 + Duration::from_millis(600));
        assert_eq!(closed, Some(("user".into(), "Bonjour à tous".into())));
        assert_eq!(tr.flush_if_stale(t0 + Duration::from_secs(5)), Some(("assistant".into(), "Salut".into())));
        assert_eq!(tr.history().len(), 2);
    }

    #[test]
    fn decoupe_les_pensees_trop_longues() {
        let long = "Une phrase assez longue pour remplir. ".repeat(100);
        let c = chunks(&long);
        assert!(c.len() >= 3 && c.iter().all(|x| x.len() <= APPEND_CHARS));
        assert_eq!(c.concat(), long);
    }
}
