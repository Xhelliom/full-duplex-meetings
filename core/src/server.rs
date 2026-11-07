//! Mode service (`fdm --serve`) : une interface web pour régler le bot (clé API, nom, consignes,
//! outils MCP) et lui donner un lien de réunion. Le bot attend, rejoint, quitte, sans redémarrer.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use axum::extract::{Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream::{self, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::{mpsc, watch, Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::audio::Audio;
use crate::config::Config;
use crate::logs::LogHub;
use crate::mcp::{McpHub, ServerStatus};
use crate::settings::Settings;
use crate::{live, shutdown};

const UI: &str = include_str!("ui.html");

#[derive(Debug, Clone, Serialize, Default)]
pub struct Status {
    /// idle | joining | in_meeting | error
    pub state: String,
    pub meeting_url: Option<String>,
    pub since: Option<u64>,
    pub error: Option<String>,
    pub mcp: Vec<ServerStatus>,
}

enum Command {
    Join(String),
    Leave,
}

struct App {
    base: Config,
    settings: RwLock<Settings>,
    logs: LogHub,
    status: watch::Sender<Status>,
    ctl: mpsc::Sender<Command>,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub async fn serve(base: Config, audio: Audio, logs: LogHub) -> Result<()> {
    let settings = Settings::load(&base)?;
    let (ctl_tx, ctl_rx) = mpsc::channel(8);
    let app = Arc::new(App {
        base: base.clone(),
        settings: RwLock::new(settings),
        logs,
        status: watch::channel(Status { state: "idle".into(), ..Default::default() }).0,
        ctl: ctl_tx,
    });

    let api = Router::new()
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/prompt", get(get_prompt))
        .route("/api/status", get(get_status))
        .route("/api/join", post(join))
        .route("/api/leave", post(leave))
        .route("/api/logs", get(logs_sse))
        .route_layer(middleware::from_fn_with_state(app.clone(), auth));
    let router = Router::new().route("/", get(|| async { Html(UI) })).merge(api).with_state(app.clone());

    let listener = tokio::net::TcpListener::bind(&base.ui_addr).await?;
    info!("🖥️  Interface : http://{}{}", base.ui_addr, if base.ui_token.is_empty() { "" } else { " (jeton requis)" });
    if !base.ui_token.is_empty() {
        info!("    → ouvrez http://{}/?token=…", base.ui_addr);
    }

    let stop = CancellationToken::new();
    let controller = tokio::spawn(controller(app.clone(), audio, ctl_rx, stop.clone()));
    let s = stop.clone();
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown::requested().await;
            s.cancel();
        })
        .await?;
    stop.cancel();
    let _ = controller.await;
    Ok(())
}

// ------------------------------------------------------------------------------------- pilote

struct Running {
    cancel: CancellationToken,
    session: JoinHandle<Result<()>>,
    joiner: Option<Child>,
}

async fn controller(app: Arc<App>, audio: Audio, mut rx: mpsc::Receiver<Command>, stop: CancellationToken) {
    enum Ev {
        Cmd(Option<Command>),
        SessionEnded(Result<()>),
        JoinerExited,
        Stop,
    }
    let audio = Arc::new(Mutex::new(audio));
    let mut running: Option<Running> = None;
    loop {
        let ev = {
            // Ce qui peut mettre fin à la réunion en cours : la session, ou le navigateur du bot.
            let meeting = async {
                match running.as_mut() {
                    Some(r) => {
                        let joiner = async {
                            match r.joiner.as_mut() {
                                Some(j) => {
                                    let _ = j.wait().await;
                                }
                                None => std::future::pending::<()>().await,
                            }
                        };
                        tokio::select! {
                            res = &mut r.session => Ev::SessionEnded(res.unwrap_or_else(|e| Err(anyhow::anyhow!("{e}")))),
                            _ = joiner => Ev::JoinerExited,
                        }
                    }
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                cmd = rx.recv() => Ev::Cmd(cmd),
                ev = meeting => ev,
                _ = stop.cancelled() => Ev::Stop,
            }
        };
        match ev {
            Ev::Cmd(Some(Command::Join(url))) => {
                if let Some(r) = running.take() {
                    stop_meeting(&app, r, "nouvelle réunion").await;
                }
                running = start_meeting(&app, &audio, url).await;
            }
            Ev::Cmd(Some(Command::Leave)) => {
                if let Some(r) = running.take() {
                    stop_meeting(&app, r, "demandé depuis l'interface").await;
                }
            }
            Ev::JoinerExited => {
                if let Some(mut r) = running.take() {
                    r.joiner = None;
                    stop_meeting(&app, r, "le navigateur du bot s'est fermé").await;
                }
            }
            Ev::SessionEnded(result) => {
                if let Some(mut r) = running.take() {
                    if let Some(j) = r.joiner.as_mut() {
                        shutdown::terminate(j, Duration::from_secs(5)).await;
                    }
                    let err = result.err().map(|e| format!("{e:#}"));
                    if let Some(e) = &err {
                        error!("Session arrêtée : {e}");
                    }
                    app.status.send_modify(|s| {
                        s.state = if err.is_some() { "error".into() } else { "idle".into() };
                        s.error = err.clone();
                        s.meeting_url = None;
                        s.since = None;
                    });
                }
            }
            Ev::Stop | Ev::Cmd(None) => {
                if let Some(r) = running.take() {
                    stop_meeting(&app, r, "arrêt du service").await;
                }
                return;
            }
        }
    }
}

async fn start_meeting(app: &Arc<App>, audio: &Arc<Mutex<Audio>>, url: String) -> Option<Running> {
    let cfg = app.settings.read().await.apply(&app.base);
    if cfg.api_key.trim().is_empty() {
        app.status.send_modify(|s| {
            s.state = "error".into();
            s.error = Some("Clé API OpenAI manquante : renseignez-la dans les réglages.".into());
        });
        warn!("Impossible de rejoindre : clé API manquante");
        return None;
    }
    app.status.send_modify(|s| {
        s.state = "joining".into();
        s.meeting_url = Some(url.clone());
        s.since = Some(now_ms());
        s.error = None;
    });
    info!("🚪 {} rejoint {url}", cfg.name);

    let hub = Arc::new(McpHub::connect_all(&cfg.mcp_servers).await);
    app.status.send_modify(|s| s.mcp = hub.status.clone());

    let joiner = spawn_joiner(&cfg, &url);
    let cancel = CancellationToken::new();
    let (c, audio) = (cancel.clone(), audio.clone());
    let session = tokio::spawn(async move {
        let mut audio = audio.lock().await;
        live::run(&cfg, &mut audio, hub, c).await
    });
    app.status.send_modify(|s| s.state = "in_meeting".into());
    Some(Running { cancel, session, joiner })
}

/// Lance le navigateur du bot ; ses messages arrivent dans le journal de l'interface.
fn spawn_joiner(cfg: &Config, url: &str) -> Option<Child> {
    let mut parts = cfg.join_cmd.split_whitespace();
    let program = parts.next()?;
    let child = tokio::process::Command::new(program)
        .args(parts)
        .arg(url)
        .arg("--name")
        .arg(&cfg.name)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    match child {
        Ok(mut child) => {
            // stdout : les messages de fdm-join ; stderr : surtout du bruit de Chromium, sauf erreurs.
            if let Some(out) = child.stdout.take() {
                tokio::spawn(async move {
                    let mut lines = BufReader::new(out).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let msg = line.split_once("[fdm-join] ").map(|(_, m)| m).unwrap_or(&line).trim().to_string();
                        if !msg.is_empty() {
                            info!("🌍 {msg}");
                        }
                    }
                });
            }
            if let Some(err) = child.stderr.take() {
                tokio::spawn(async move {
                    let mut lines = BufReader::new(err).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if line.contains("Error") || line.contains("error:") {
                            warn!("🌍 {}", line.trim());
                        } else {
                            tracing::debug!("navigateur : {line}");
                        }
                    }
                });
            }
            Some(child)
        }
        Err(e) => {
            warn!("Navigateur du bot non lancé ({} : {e}) — branchez un client Teams sur les périphériques FDM", cfg.join_cmd);
            None
        }
    }
}

async fn stop_meeting(app: &Arc<App>, mut r: Running, why: &str) {
    info!("👋 Le bot quitte la réunion ({why})");
    r.cancel.cancel();
    if tokio::time::timeout(Duration::from_secs(5), &mut r.session).await.is_err() {
        r.session.abort();
    }
    if let Some(j) = r.joiner.as_mut() {
        shutdown::terminate(j, Duration::from_secs(5)).await;
    }
    app.status.send_modify(|s| {
        s.state = "idle".into();
        s.meeting_url = None;
        s.since = None;
    });
}

// ------------------------------------------------------------------------------------- HTTP

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

async fn auth(State(app): State<Arc<App>>, Query(q): Query<TokenQuery>, req: Request, next: Next) -> Response {
    let expected = &app.base.ui_token;
    if !expected.is_empty() {
        let given = req
            .headers()
            .get("x-fdm-token")
            .and_then(|v| v.to_str().ok())
            .map(String::from)
            .or(q.token);
        if given.as_deref() != Some(expected.as_str()) {
            return (StatusCode::UNAUTHORIZED, Json(json!({"error": "jeton invalide"}))).into_response();
        }
    }
    next.run(req).await
}

fn public_settings(s: &Settings) -> Value {
    let mut v = serde_json::to_value(s).unwrap_or_default();
    v["api_key"] = json!("");
    v["api_key_set"] = json!(!s.api_key.trim().is_empty());
    v["api_key_masked"] = json!(s.masked_key());
    v
}

async fn get_settings(State(app): State<Arc<App>>) -> Json<Value> {
    Json(public_settings(&*app.settings.read().await))
}

fn bad(msg: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": msg}))).into_response()
}

async fn put_settings(State(app): State<Arc<App>>, Json(mut new): Json<Settings>) -> Response {
    if !["reserve", "normal", "active"].contains(&new.proactivity.as_str()) {
        return bad("proactivité : reserve, normal ou active");
    }
    if !["responses", "client"].contains(&new.delegation.as_str()) {
        return bad("délégation : responses ou client");
    }
    if new.name.trim().is_empty() {
        return bad("le bot a besoin d'un nom");
    }
    for m in &new.mcp_servers {
        if m.name.trim().is_empty() {
            return bad("chaque serveur MCP a besoin d'un nom");
        }
        if m.enabled && m.command.trim().is_empty() && m.url.trim().is_empty() {
            return bad(&format!("serveur MCP « {} » : il faut une commande ou une URL", m.name));
        }
        if m.enabled && m.run_on == "openai" && !m.url.starts_with("https://") {
            return bad(&format!("serveur MCP « {} » : OpenAI ne peut appeler qu'une URL https publique", m.name));
        }
    }
    let mut current = app.settings.write().await;
    if new.api_key.trim().is_empty() {
        new.api_key = current.api_key.clone(); // champ laissé vide : on garde la clé
    } else {
        new.api_key = new.api_key.trim().to_string();
    }
    if let Err(e) = new.save(&Settings::path(&app.base)) {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("{e:#}")}))).into_response();
    }
    *current = new;
    info!("💾 Réglages enregistrés (appliqués à la prochaine réunion)");
    Json(public_settings(&current)).into_response()
}

async fn get_prompt(State(app): State<Arc<App>>) -> Json<Value> {
    let cfg = app.settings.read().await.apply(&app.base);
    Json(json!({"instructions": cfg.instructions(), "backend_instructions": cfg.backend_instructions()}))
}

async fn get_status(State(app): State<Arc<App>>) -> Json<Status> {
    Json(app.status.borrow().clone())
}

#[derive(Deserialize)]
struct JoinBody {
    url: String,
}

async fn join(State(app): State<Arc<App>>, Json(body): Json<JoinBody>) -> Response {
    let url = body.url.trim().to_string();
    if !url.starts_with("https://") {
        return bad("collez le lien complet de la réunion (https://…)");
    }
    let _ = app.ctl.send(Command::Join(url)).await;
    (StatusCode::ACCEPTED, Json(json!({"ok": true}))).into_response()
}

async fn leave(State(app): State<Arc<App>>) -> Response {
    let _ = app.ctl.send(Command::Leave).await;
    (StatusCode::ACCEPTED, Json(json!({"ok": true}))).into_response()
}

async fn logs_sse(State(app): State<Arc<App>>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let recent = app.logs.recent();
    let rx = app.logs.tx.subscribe();
    let history = stream::iter(recent.into_iter().map(|l| Ok(Event::default().data(l))));
    let live = stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(line) => return Some((Ok(Event::default().data(line)), rx)),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return None,
            }
        }
    });
    Sse::new(history.chain(live)).keep_alive(KeepAlive::default())
}
