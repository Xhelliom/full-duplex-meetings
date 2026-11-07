mod audio;
mod config;
mod live;
mod logs;
mod mcp;
mod server;
mod settings;
mod shutdown;

use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Layer;
use tracing_subscriber::util::SubscriberInitExt;

use config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let logs = logs::LogHub::new();
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "fdm=info".into());
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_target(false).with_filter(filter))
        .with(logs.clone())
        .init();
    let cfg = Config::parse();

    let audio = audio::start(cfg.prebuffer_ms)?;
    info!(
        "🔊 Périphériques PipeWire créés : haut-parleur « {} », micro « {} »",
        audio::MEETING_SINK,
        audio::AI_MIC
    );

    // Mode service : interface web, le bot attend qu'on lui donne un lien.
    if cfg.serve {
        return server::serve(cfg, audio, logs).await;
    }

    // Mode ligne de commande : une réunion (--join), puis on s'arrête.
    let mut joiner = match &cfg.join {
        Some(url) => {
            info!("🚪 Lancement de « {} » pour rejoindre la réunion…", cfg.join_cmd);
            let mut parts = cfg.join_cmd.split_whitespace();
            let program = parts.next().unwrap_or("fdm-join");
            Some(
                tokio::process::Command::new(program)
                    .args(parts)
                    .arg(url)
                    .arg("--name")
                    .arg(&cfg.name)
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|e| anyhow::anyhow!("impossible de lancer {} : {e} (npm install -g ./join ?)", cfg.join_cmd))?,
            )
        }
        None => None,
    };

    let result = if cfg.echo_test {
        echo_test(audio).await
    } else {
        let settings = settings::Settings::load(&cfg)?;
        let cfg = settings.apply(&cfg);
        anyhow::ensure!(!cfg.api_key.is_empty(), "OPENAI_API_KEY manquant");
        let hub = Arc::new(mcp::McpHub::connect_all(&cfg.mcp_servers).await);
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        tokio::spawn(async move {
            shutdown::requested().await;
            c.cancel();
        });
        let mut audio = audio;
        live::run(&cfg, &mut audio, hub, cancel).await
    };

    // Laisser le navigateur quitter la réunion proprement avant de tout couper.
    if let Some(child) = joiner.as_mut() {
        shutdown::terminate(child, std::time::Duration::from_secs(5)).await;
    }
    result
}

/// Ce qui arrive sur le haut-parleur du bot est rejoué dans son micro.
async fn echo_test(audio: audio::Audio) -> Result<()> {
    let audio::Audio { capture: mut rx, playback } = audio;
    info!(
        "🔁 Test d'écho : ce qui arrive sur « {} » est rejoué sur « {} ». Ctrl+C pour quitter.",
        audio::MEETING_SINK,
        audio::AI_MIC
    );
    let mut total = 0usize;
    loop {
        tokio::select! {
            Some(samples) = rx.recv() => {
                total += samples.len();
                playback.push(&samples);
                if total >= audio::SAMPLE_RATE as usize * 2 {
                    debug!("reçu 2 s, audible : {}", audio::is_audible(&samples));
                    total = 0;
                }
            }
            _ = shutdown::requested() => return Ok(()),
        }
    }
}
