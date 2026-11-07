mod audio;
mod brain;
mod config;
mod live;
mod names;
mod realtime;

use anyhow::Result;
use clap::Parser;
use tracing::{debug, info};

use config::{Config, Engine};

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

    let prebuffer = if cfg.engine == Engine::Live { cfg.prebuffer_ms } else { 0 };
    let audio = audio::start(prebuffer)?;
    info!(
        "🔊 Périphériques PipeWire créés : haut-parleur « {} », micro « {} »",
        audio::MEETING_SINK,
        audio::AI_MIC
    );

    // Le navigateur du bot (Python/Playwright) : processus enfant, tué quand fdm s'arrête.
    let _joiner = match &cfg.join {
        Some(url) => {
            info!("🚪 Lancement de « {} » pour rejoindre la réunion…", cfg.join_cmd);
            Some(
                tokio::process::Command::new(&cfg.join_cmd)
                    .arg(url)
                    .arg("--name")
                    .arg(&cfg.name)
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|e| anyhow::anyhow!("impossible de lancer {} : {e} (pip install -e join ?)", cfg.join_cmd))?,
            )
        }
        None => None,
    };

    if cfg.echo_test {
        return echo_test(audio).await;
    }
    anyhow::ensure!(!cfg.api_key.is_empty(), "OPENAI_API_KEY manquant (fichier .env ou variable d'environnement)");

    match cfg.engine {
        Engine::Live => live::run(&cfg, audio).await,
        Engine::Realtime => realtime::run(&cfg, audio).await,
    }
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
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}
