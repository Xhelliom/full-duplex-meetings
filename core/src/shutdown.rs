//! Arrêt propre sur Ctrl+C **et** SIGTERM (`docker stop`, systemd…).

use std::time::Duration;

use tokio::process::Child;
use tokio::signal::unix::{signal, SignalKind};
use tracing::warn;

/// Se termine à la première demande d'arrêt.
pub async fn requested() {
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

/// Demande poliment à un processus enfant de s'arrêter (SIGTERM), puis le tue s'il traîne.
pub async fn terminate(child: &mut Child, grace: Duration) {
    if let Some(pid) = child.id() {
        let _ = std::process::Command::new("kill").args(["-TERM", &pid.to_string()]).status();
        if tokio::time::timeout(grace, child.wait()).await.is_ok() {
            return;
        }
        warn!("le navigateur du bot ne s'arrête pas : arrêt forcé");
    }
    let _ = child.kill().await;
}
