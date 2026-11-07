//! Moteur **Realtime** (`gpt-realtime`) : au tour par tour, avec outils. C'est `Brain` qui
//! décide quand répondre (voir brain.rs).

use std::time::Duration;

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
use tracing::{debug, info, warn};

use crate::audio::{self, Audio, Uplink};
use crate::brain::{Action, Brain};
use crate::config::Config;
use crate::names::NameDetector;

fn session_update(cfg: &Config) -> Value {
    json!({
        "type": "session.update",
        "session": {
            "type": "realtime",
            "model": cfg.realtime_model,
            "instructions": cfg.instructions(),
            "output_modalities": ["audio"],
            "max_output_tokens": "inf",
            "tools": Brain::tools(),
            "tool_choice": "auto",
            "audio": {
                "input": {
                    "format": {"type": "audio/pcm", "rate": audio::SAMPLE_RATE},
                    "noise_reduction": {"type": "far_field"},
                    "transcription": {"model": cfg.transcription_model, "language": cfg.language},
                    "turn_detection": {
                        "type": "semantic_vad",
                        "eagerness": "auto",
                        // C'est `Brain` qui décide quand répondre et quand se taire.
                        "create_response": false,
                        "interrupt_response": false
                    }
                },
                "output": {
                    "format": {"type": "audio/pcm", "rate": audio::SAMPLE_RATE},
                    "voice": cfg.voice
                }
            }
        }
    })
}

pub async fn run(cfg: &Config, audio: Audio) -> Result<()> {
    let Audio { capture: mut mic_rx, playback } = audio;
    let url = format!("{}/realtime?model={}", cfg.api_base.trim_end_matches('/'), cfg.realtime_model);
    let mut req = url.as_str().into_client_request()?;
    req.headers_mut().insert("Authorization", format!("Bearer {}", cfg.api_key).parse()?);
    let (ws, _) = tokio_tungstenite::connect_async(req).await.context("connexion à l'API Realtime")?;
    info!("🌐 Connecté à {url}");
    let (mut ws_tx, mut ws_rx) = ws.split();
    ws_tx.send(Message::text(session_update(cfg).to_string())).await?;

    let mut brain = Brain::new(&cfg.name, NameDetector::new(cfg.all_names()), cfg.barge_in_ms);
    let (timer_tx, mut timer_rx) = mpsc::unbounded_channel::<u64>();
    let mut uplink = Uplink::default();
    let mut ticker = tokio::time::interval(Duration::from_millis(audio::UPLINK_MS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    info!("👂 {} écoute la réunion. Appelez-le par son nom pour lui parler. Ctrl+C pour quitter.", cfg.name);

    loop {
        let actions: Vec<Action> = tokio::select! {
            Some(samples) = mic_rx.recv() => { uplink.feed(&samples); vec![] }

            // Réunion -> OpenAI en continu (aussi pendant que l'IA parle), silence compris.
            _ = ticker.tick() => {
                for block in uplink.tick() {
                    let ev = json!({"type": "input_audio_buffer.append", "audio": B64.encode(block)});
                    ws_tx.send(Message::text(ev.to_string())).await?;
                }
                vec![]
            }

            msg = ws_rx.next() => {
                let Some(msg) = msg else { anyhow::bail!("connexion Realtime fermée") };
                let text = match msg? {
                    Message::Text(t) => t,
                    Message::Close(f) => anyhow::bail!("connexion Realtime fermée : {f:?}"),
                    _ => continue,
                };
                let ev: Value = serde_json::from_str(&text)?;
                let t = ev["type"].as_str().unwrap_or("");
                if !t.ends_with("audio.delta") {
                    debug!("← {t}");
                }
                if t == "session.updated" { info!("✅ session configurée"); }
                let acts = brain.on_event(&ev, playback.is_playing());
                if let Some(delta) = ev["delta"].as_str().filter(|_| t.ends_with("output_audio.delta") || t == "response.audio.delta") {
                    // StartItem (remise à zéro du compteur) doit précéder l'ajout de l'audio.
                    let mut rest = vec![];
                    for a in acts {
                        if a == Action::StartItem { playback.start_item(); } else { rest.push(a); }
                    }
                    let bytes = B64.decode(delta)?;
                    let samples: Vec<i16> = bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
                    playback.push(&samples);
                    rest
                } else {
                    acts
                }
            }

            Some(gen) = timer_rx.recv() => brain.on_barge_in_timer(gen, playback.is_playing(), playback.played_ms()),

            _ = tokio::signal::ctrl_c() => { info!("Au revoir 👋"); return Ok(()); }
        };

        for action in actions {
            match action {
                Action::Send(v) => {
                    debug!("→ {}", v["type"]);
                    ws_tx.send(Message::text(v.to_string())).await?;
                }
                Action::FlushPlayback => playback.flush(),
                Action::StartItem => playback.start_item(),
                Action::ArmBargeIn { gen, after_ms } => {
                    let tx = timer_tx.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(after_ms)).await;
                        let _ = tx.send(gen);
                    });
                }
                Action::Log(s) if s.starts_with('❌') => warn!("{s}"),
                Action::Log(s) => info!("{s}"),
            }
        }
    }
}
