mod audio;
mod config;
mod live;
mod shutdown;

use anyhow::Result;
use clap::Parser;
use tracing::{debug, info};

use config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "fdm=info".into()),
        )
        .with_target(false)
        .init();
    let cfg = Config::parse();

    let audio = audio::start(cfg.prebuffer_ms)?;
    info!(
        "🔊 Périphériques PipeWire créés : haut-parleur « {} », micro « {} »",
        audio::MEETING_SINK,
        audio::AI_MIC
    );

    // Le navigateur du bot (Python/Playwright) : processus enfant, tué quand fdm s'arrête.
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
    } else if cfg.api_key.is_empty() {
        Err(anyhow::anyhow!("OPENAI_API_KEY manquant (fichier .env ou variable d'environnement)"))
    } else {
        live::run(&cfg, audio).await
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
