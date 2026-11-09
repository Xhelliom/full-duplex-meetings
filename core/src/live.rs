//! Session **GPT-Live** (`gpt-live-1`) : le modèle full duplex d'OpenAI.
//!
//! Pas de tours de parole ni de règles côté client : le modèle écoute la réunion en permanence et
//! décide lui-même quand parler, quand se taire, quand s'arrêter parce qu'on lui coupe la parole.
//! Ce module ne fait que le transport :
//!
//! * `wss://…/v1/live/sessions`, `session.start` puis attente de `session.started` ;
//! * entrée audio **continue** (des zéros quand personne ne parle : gpt-live cale son rythme dessus) ;
//! * sortie audio en temps réel (trames de 100 ms, silence entre ses interventions) -> micro du bot ;
//! * transcriptions par fragments, regroupées en prises de parole pour les logs et pour pouvoir
//!   recréer une session avec l'historique si elle se ferme (expiration, coupure réseau).
//!
//! Le protocole et ses pièges sont ceux mesurés dans loquivox (moteur « live »).

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
use tracing::{debug, info, warn};

use std::collections::{HashMap, HashSet};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::audio::{self, Audio, Uplink};
use crate::config::Config;
use crate::mcp::SharedHub;

/// Un fragment de transcription après un tel blanc commence une nouvelle prise de parole
/// (l'API n'envoie pas de fin de tour).
const TURN_GAP: Duration = Duration::from_millis(1200);
/// Historique texte repassé à une nouvelle session (`input` est limité à ~8 k tokens).
const HISTORY_CHARS: usize = 20_000;
const HISTORY_ITEMS: usize = 120; // l'API en accepte 128 au plus
/// Outil intégré : quitter la réunion quand on le lui demande à voix haute.
pub const LEAVE_TOOL: &str = "quitter_reunion";
/// Le temps de dire au revoir avant de partir.
const LEAVE_DELAY: Duration = Duration::from_secs(8);

/// Une réflexion déléguée qui dure plus que ça : il dit une fois où il en est, au lieu d'un long blanc.
const PROGRESS_AFTER: Duration = Duration::from_secs(8);
/// Au-delà, on n'attend plus la fin de cette délégation (événement de fin manqué).
const PROGRESS_GIVE_UP: Duration = Duration::from_secs(120);
/// Il ne coupe pas quelqu'un qui parle : silence de la réunion exigé avant de relancer.
const PROGRESS_QUIET: Duration = Duration::from_millis(1500);

/// Une délégation au backend en cours, vue de la réunion.
struct Thinking {
    since: Instant,
    /// Ce qu'il consulte (« il consulte l'historique du support »…), dans l'ordre
    activities: Vec<String>,
    /// Appels d'outils dans la réponse courante du backend (0 à la fin = réponse finale)
    calls_in_response: usize,
    told: bool,
}

/// Consigne pour annoncer, en une phrase, que la recherche est encore en cours.
fn progress_commentary(activities: &[String]) -> String {
    let doing = match activities.last() {
        Some(a) => format!(" En ce moment, {a}."),
        None => String::new(),
    };
    format!(
        "La recherche que tu as lancée prend un peu plus de temps que prévu.{doing} Dis-le en une seule phrase \
courte et naturelle, comme un collègue qui fouille encore (« je cherche encore dans l'historique des tickets, \
deux secondes »), sans jargon technique ni nom d'outil, sans donner de résultat et sans poser de question. Puis \
tais-toi jusqu'à la réponse."
    )
}

fn leave_tool() -> Value {
    json!({
        "type": "function",
        "name": LEAVE_TOOL,
        "description": "Fait quitter la réunion au participant IA. Uniquement quand quelqu'un lui demande \
explicitement, à lui, de partir (« tu peux nous laisser », « merci, tu peux quitter la réunion »), pas quand on \
parle de la fin de la réunion en général.",
        "parameters": {"type": "object", "properties": {}}
    })
}

pub fn session_config(cfg: &Config, history: &[(String, String)], mcp_tools: &[Value]) -> Value {
    let mut session = json!({
        "model": cfg.model,
        "instructions": cfg.instructions(),
        "audio": {
            "format": {"type": "audio/pcm", "rate": audio::SAMPLE_RATE},
            "output": {"voice": cfg.voice}
        },
    });
    session["delegation"] = if cfg.delegation == "responses" {
        // Les outils du backend sont fixés à l'ouverture de la session : tout est déclaré d'emblée.
        let mut tools: Vec<Value> = mcp_tools.to_vec();
        tools.push(leave_tool());
        if !cfg.no_web_search {
            tools.push(json!({"type": "web_search"}));
        }
        let mut responses = json!({"model": cfg.backend_model, "instructions": cfg.backend_instructions()});
        if !tools.is_empty() {
            responses["tools"] = Value::Array(tools);
            responses["tool_choice"] = json!("auto");
        }
        json!({"type": "responses", "responses": responses})
    } else {
        json!({"type": "client"})
    };
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

/// Fragments de transcription regroupés en prises de parole.
#[derive(Default)]
pub struct Transcript {
    turns: Vec<(String, String)>,
    current: Option<(String, String, Instant)>,
}

impl Transcript {
    /// Ajoute un fragment ; retourne la prise de parole précédente si celle-ci vient de se terminer.
    pub fn push(&mut self, role: &str, delta: &str, now: Instant) -> Option<(String, String)> {
        if let Some((r, text, at)) = &mut self.current {
            if r == role && now.duration_since(*at) <= TURN_GAP {
                text.push_str(delta);
                *at = now;
                return None;
            }
        }
        let closed = self.flush();
        self.current = Some((role.to_string(), delta.to_string(), now));
        closed
    }

    pub fn flush_if_stale(&mut self, now: Instant) -> Option<(String, String)> {
        match &self.current {
            Some((_, _, at)) if now.duration_since(*at) > TURN_GAP => self.flush(),
            _ => None,
        }
    }

    pub fn flush(&mut self) -> Option<(String, String)> {
        let (role, text, _) = self.current.take()?;
        let text = text.trim().to_string();
        if text.is_empty() {
            return None;
        }
        self.turns.push((role.clone(), text.clone()));
        Some((role, text))
    }

    /// Les dernières prises de parole qui tiennent dans le budget, dans l'ordre.
    pub fn history(&self) -> Vec<(String, String)> {
        let mut budget = HISTORY_CHARS;
        let mut out: Vec<_> = self
            .turns
            .iter()
            .rev()
            .take(HISTORY_ITEMS)
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

/// Boucle principale ; recrée une session (avec l'historique de la réunion) si elle se ferme.
/// S'arrête proprement quand `cancel` est déclenché.
pub async fn run(cfg: &Config, audio: &mut Audio, hub: SharedHub, cancel: CancellationToken) -> Result<()> {
    let mut transcript = Transcript::default();
    let mut uplink = Uplink::default();
    let mut failures = 0u32;
    // Le son accumulé avant la session (bot en attente) est périmé.
    while audio.capture.try_recv().is_ok() {}
    loop {
        if cancel.is_cancelled() {
            return Ok(());
        }
        match session(cfg, audio, &mut transcript, &mut uplink, &hub, &cancel).await {
            Ok(None) => return Ok(()), // Ctrl+C / SIGTERM
            Ok(Some(reason)) if reason == "close_requested" => return Ok(()),
            Ok(Some(reason)) => {
                warn!("Session fermée par le serveur ({reason}) : nouvelle session avec l'historique de la réunion");
                failures = 0;
            }
            Err(e) => {
                failures += 1;
                if failures > 5 {
                    return Err(e);
                }
                warn!("Session interrompue : {e:#} — nouvel essai dans {failures} s");
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(failures as u64)) => {}
                    _ = cancel.cancelled() => return Ok(()),
                }
            }
        }
    }
}

/// Une session. `Ok(None)` : arrêt demandé (Ctrl+C, SIGTERM) ; `Ok(Some(raison))` : fermée par le serveur.
async fn session(
    cfg: &Config,
    audio: &mut Audio,
    transcript: &mut Transcript,
    uplink: &mut Uplink,
    hub: &SharedHub,
    cancel: &CancellationToken,
) -> Result<Option<String>> {
    let url = format!("{}/live/sessions", cfg.api_base.trim_end_matches('/'));
    let mut req = url.as_str().into_client_request()?;
    req.headers_mut().insert("Authorization", format!("Bearer {}", cfg.api_key).parse()?);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.context("connexion à l'API GPT-Live")?;
    let (mut tx, mut rx) = ws.split();
    let start = json!({"type": "session.start", "session": session_config(cfg, &transcript.history(), &hub.tools())});
    tx.send(Message::text(start.to_string())).await?;
    info!("🌐 Connecté ({}, voix {}, délégation {})", cfg.model, cfg.voice, cfg.delegation);

    let mut started = false;
    let mut ticker = tokio::time::interval(Duration::from_millis(audio::UPLINK_MS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let playback = audio.playback.clone();
    let (internal_tx, mut internal_rx) = mpsc::unbounded_channel::<Internal>();
    let mut calls: HashMap<String, CallGroup> = HashMap::new();
    let mut last_backlog_check = Instant::now();
    let mut thinking: HashMap<String, Thinking> = HashMap::new();
    let mut last_heard = Instant::now();

    loop {
        let mut outgoing: Vec<Value> = vec![];
        tokio::select! {
            Some(samples) = audio.capture.recv() => uplink.feed(&samples),

            _ = ticker.tick() => {
                // Rien ne doit partir avant session.started.
                if started {
                    for block in uplink.tick() {
                        outgoing.push(json!({"type": "session.input_audio.append", "audio": B64.encode(block)}));
                    }
                }
                if let Some((role, text)) = transcript.flush_if_stale(Instant::now()) {
                    log_turn(cfg, &role, &text);
                }
                thinking.retain(|_, t| t.since.elapsed() < PROGRESS_GIVE_UP);
                // Personne ne parle, lui non plus : une longue recherche mérite un mot.
                if started && last_heard.elapsed() >= PROGRESS_QUIET && playback.backlog_ms() < 100 {
                    if let Some(t) = thinking.values_mut().find(|t| !t.told && t.since.elapsed() >= PROGRESS_AFTER) {
                        t.told = true;
                        info!("⏳ la réflexion dure : il le dit ({})", t.activities.last().map_or("réflexion", |a| a));
                        outgoing.push(json!({
                            "type": "session.commentary.append",
                            "delegation_id": null,
                            "content": progress_commentary(&t.activities),
                        }));
                    }
                }
                if last_backlog_check.elapsed() >= Duration::from_secs(5) {
                    last_backlog_check = Instant::now();
                    let ms = playback.backlog_ms();
                    if ms > 1000 {
                        warn!("⏳ sa voix a {ms} ms de retard (réseau lent vers OpenAI ?)");
                    } else {
                        debug!("voix en attente : {ms} ms");
                    }
                }
            }

            Some(internal) = internal_rx.recv() => match internal {
                Internal::ToolResult { delegation, call_id, output } => {
                    outgoing.push(json!({
                        "type": "response.item.create",
                        "item": {"type": "function_call_output", "call_id": call_id, "output": output}
                    }));
                    if let Some(g) = calls.get_mut(&delegation) { g.open.remove(&call_id); }
                    outgoing.extend(maybe_continue(&mut calls, &delegation, &internal_tx));
                }
                Internal::Grace { delegation, count } => {
                    if let Some(g) = calls.get_mut(&delegation) {
                        if g.count == count && g.open.is_empty() { g.done = true; }
                    }
                    outgoing.extend(maybe_continue(&mut calls, &delegation, &internal_tx));
                }
            },

            msg = rx.next() => {
                let Some(msg) = msg else { bail!("connexion fermée") };
                let text = match msg? {
                    Message::Text(t) => t,
                    Message::Close(f) => bail!("connexion fermée : {f:?}"),
                    _ => continue,
                };
                let ev: Value = serde_json::from_str(&text)?;
                let now = Instant::now();
                match ev["type"].as_str().unwrap_or("") {
                    "session.output_audio.delta" => {
                        let bytes = B64.decode(ev["delta"].as_str().unwrap_or(""))?;
                        let samples: Vec<i16> = bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
                        playback.push(&samples);
                    }
                    "session.input_transcript.delta" => {
                        hub.user_spoke();
                        last_heard = now;
                        if let Some((role, text)) = transcript.push("user", ev["delta"].as_str().unwrap_or(""), now) {
                            log_turn(cfg, &role, &text);
                        }
                    }
                    "session.output_transcript.delta" => {
                        if let Some((role, text)) = transcript.push("assistant", ev["delta"].as_str().unwrap_or(""), now) {
                            log_turn(cfg, &role, &text);
                        }
                    }
                    "session.started" => {
                        started = true;
                        info!("✅ {} est dans la conversation : il écoute la réunion en continu", cfg.name);
                    }
                    "session.delegation.created" => {
                        let target = ev["delegation"]["target"].as_str().unwrap_or("");
                        info!("🧠 il réfléchit (délégation {target})");
                        if target == "responses" {
                            let id = ev["delegation"]["id"].as_str().unwrap_or("").to_string();
                            thinking.insert(id, Thinking { since: now, activities: vec![], calls_in_response: 0, told: false });
                        }
                        if target == "client" {
                            // Pas de cerveau côté client pour l'instant : sans réponse, il attendrait.
                            outgoing.push(json!({
                                "type": "session.thinking.append",
                                "delegation_id": ev["delegation"]["id"],
                                "content": "Aucun outil n'est disponible pour cette demande : réponds avec ce que tu \
sais, ou dis simplement que tu ne peux pas le vérifier maintenant."
                            }));
                        }
                    }
                    // Événements du backend (délégation responses) : appels d'outils à exécuter.
                    "response.event" => {
                        let delegation = ev["delegation_id"].as_str().unwrap_or("").to_string();
                        let inner = &ev["event"];
                        match inner["type"].as_str().unwrap_or("") {
                            "response.output_item.done" if inner["item"]["type"] == "function_call" => {
                                let item = &inner["item"];
                                let call_id = item["call_id"].as_str().unwrap_or("").to_string();
                                let name = item["name"].as_str().unwrap_or("").to_string();
                                let args = item["arguments"].as_str().unwrap_or("{}").to_string();
                                info!("🔧 appel d'outil : {name}");
                                let g = calls.entry(delegation.clone()).or_default();
                                g.open.insert(call_id.clone());
                                g.count += 1;
                                if let Some(t) = thinking.get_mut(&delegation) {
                                    t.calls_in_response += 1;
                                    t.activities.push(hub.activity(&name, &args));
                                }
                                if name == LEAVE_TOOL {
                                    thinking.remove(&delegation); // il prend congé, pas de « je cherche encore »
                                    info!("👋 On lui demande de quitter la réunion : il part dans {} s", LEAVE_DELAY.as_secs());
                                    let output = "C'est fait : tu quittes la réunion dans quelques secondes. Si tu as déjà pris congé, ne dis \
plus rien ; sinon, prends congé en une phrase courte (« Je vous laisse, à bientôt ! »).".to_string();
                                    let _ = internal_tx.send(Internal::ToolResult { delegation, call_id, output });
                                    let cancel = cancel.clone();
                                    tokio::spawn(async move {
                                        tokio::time::sleep(LEAVE_DELAY).await;
                                        cancel.cancel(); // fin propre de la session ; le service fait sortir le navigateur
                                    });
                                    continue;
                                }
                                let (hub, tx) = (hub.clone(), internal_tx.clone());
                                // Pendant que l'outil travaille, la voix continue la conversation.
                                tokio::spawn(async move {
                                    let output = hub.dispatch(&name, &args).await;
                                    let _ = tx.send(Internal::ToolResult { delegation, call_id, output });
                                });
                            }
                            "response.output_item.added" if inner["item"]["type"] == "web_search_call" => {
                                if let Some(t) = thinking.get_mut(&delegation) {
                                    t.activities.push("il fait une recherche sur le web".into());
                                }
                            }
                            // Nouvelle étape du backend (après des résultats d'outils).
                            "response.created" => {
                                if let Some(t) = thinking.get_mut(&delegation) { t.calls_in_response = 0; }
                            }
                            // La réponse s'écrit : la voix va bientôt la dire.
                            "response.output_text.delta" => {
                                thinking.remove(&delegation);
                            }
                            "response.completed" => {
                                if thinking.get(&delegation).is_some_and(|t| t.calls_in_response == 0) {
                                    thinking.remove(&delegation);
                                }
                                if let Some(g) = calls.get_mut(&delegation) { g.done = true; }
                                outgoing.extend(maybe_continue(&mut calls, &delegation, &internal_tx));
                            }
                            t => debug!("← response.event/{t}"),
                        }
                    }
                    "session.closed" => {
                        if let Some((role, text)) = transcript.flush() { log_turn(cfg, &role, &text); }
                        let reason = ev["reason"].as_str().unwrap_or("?").to_string();
                        info!("🔚 Session fermée ({reason}) — {} s d'audio", ev["usage"]["seconds"]);
                        return Ok(Some(reason));
                    }
                    "error" => warn!("❌ erreur GPT-Live : {}", ev["error"]),
                    "info" => info!("ℹ️  {} : {}", ev["code"], ev["message"]),
                    t => debug!("← {t}"),
                }
            }

            _ = cancel.cancelled() => {
                info!("Au revoir 👋");
                // Fermeture documentée : on demande, puis on attend `session.closed` (bilan d'usage).
                let _ = tx.send(Message::text(json!({"type": "session.close"}).to_string())).await;
                let wait = async {
                    while let Some(Ok(Message::Text(t))) = rx.next().await {
                        let ev: Value = serde_json::from_str(&t).unwrap_or_default();
                        if ev["type"] == "session.closed" {
                            info!("🔚 Session fermée — {} s d'audio", ev["usage"]["seconds"]);
                            break;
                        }
                    }
                };
                let _ = tokio::time::timeout(Duration::from_secs(3), wait).await;
                let _ = tx.close().await;
                return Ok(None);
            }
        }

        for ev in outgoing {
            tx.send(Message::text(ev.to_string())).await?;
        }
    }
}

enum Internal {
    ToolResult { delegation: String, call_id: String, output: String },
    Grace { delegation: String, count: usize },
}

/// Les appels d'outils d'une réponse du backend.
#[derive(Default)]
struct CallGroup {
    open: HashSet<String>,
    count: usize,
    /// `response.completed` reçu (plus aucun appel ne viendra)
    done: bool,
}

/// Délai d'attente de `response.completed` une fois tous les appels connus servis.
const CALLS_GRACE: Duration = Duration::from_millis(1500);

/// `response.create` dès que tous les appels ont leur résultat ET que la réponse est complète.
/// L'API exige les deux : un résultat seul ne relance pas la réponse, et relancer avant d'avoir
/// tous les résultats d'appels parallèles ferme la session (`function_call_outputs_required`).
fn maybe_continue(calls: &mut HashMap<String, CallGroup>, delegation: &str, tx: &mpsc::UnboundedSender<Internal>) -> Vec<Value> {
    let Some(g) = calls.get(delegation) else { return vec![] };
    if g.count == 0 || !g.open.is_empty() {
        return vec![];
    }
    if g.done {
        calls.remove(delegation);
        return vec![json!({"type": "response.create"})];
    }
    // `response.completed` n'arrive pas toujours sur une réponse en pause : on n'en dépend pas.
    let (tx, delegation, count) = (tx.clone(), delegation.to_string(), g.count);
    tokio::spawn(async move {
        tokio::time::sleep(CALLS_GRACE).await;
        let _ = tx.send(Internal::Grace { delegation, count });
    });
    vec![]
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

    #[test]
    fn relance_d_attente() {
        let c = progress_commentary(&["il consulte l'historique du support".into()]);
        assert!(c.contains("En ce moment, il consulte l'historique du support."));
        assert!(c.contains("une seule phrase"));
        assert!(!progress_commentary(&[]).contains("En ce moment"));
    }

    #[test]
    fn config_de_session() {
        let cfg = Config::parse_from(["fdm", "--name", "Nova", "--expertise", "expert Rust", "--proactivity", "active"]);
        let mcp = vec![json!({"type": "function", "name": "agenda__list", "parameters": {"type": "object"}})];
        let s = session_config(&cfg, &[("user".into(), "Bonjour".into()), ("assistant".into(), "Salut".into())], &mcp);
        let tools = s["delegation"]["responses"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[1]["name"], LEAVE_TOOL);
        assert_eq!(tools[2]["type"], "web_search");
        assert_eq!(s["model"], "gpt-live-1");
        assert_eq!(s["audio"]["format"]["rate"], 24000);
        assert_eq!(s["delegation"]["type"], "responses");
        assert_eq!(s["input"][1]["content"][0]["type"], "output_text");
        let instr = s["instructions"].as_str().unwrap();
        assert!(instr.contains("expert Rust") && instr.contains("N'hésite pas à intervenir"));
        assert!(instr.contains("# Interruptions"));
    }

    #[tokio::test]
    async fn relance_seulement_quand_tous_les_appels_sont_servis() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut calls: HashMap<String, CallGroup> = HashMap::new();
        let g = calls.entry("d1".into()).or_default();
        g.open.extend(["a".to_string(), "b".to_string()]);
        g.count = 2;
        calls.get_mut("d1").unwrap().open.remove("a");
        assert!(maybe_continue(&mut calls, "d1", &tx).is_empty()); // « b » attend encore
        calls.get_mut("d1").unwrap().open.remove("b");
        assert!(maybe_continue(&mut calls, "d1", &tx).is_empty()); // pas encore complete : délai de grâce
        let Some(Internal::Grace { delegation, count }) = rx.recv().await else { panic!() };
        assert_eq!((delegation.as_str(), count), ("d1", 2));
        calls.get_mut("d1").unwrap().done = true;
        assert_eq!(maybe_continue(&mut calls, "d1", &tx), vec![json!({"type": "response.create"})]);
        assert!(calls.is_empty());
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
    fn historique_borne() {
        let mut tr = Transcript::default();
        let t0 = Instant::now();
        for i in 0..300 {
            tr.push(if i % 2 == 0 { "user" } else { "assistant" }, &"x".repeat(150), t0);
        }
        tr.flush();
        let h = tr.history();
        assert!(h.len() <= HISTORY_ITEMS);
        assert!(h.iter().map(|(_, t)| t.len()).sum::<usize>() <= HISTORY_CHARS);
    }
}
