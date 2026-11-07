//! Journal visible dans l'interface : chaque message INFO+ est gardé (les 300 derniers) et
//! diffusé en direct aux pages ouvertes.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

const KEEP: usize = 300;

#[derive(Clone)]
pub struct LogHub {
    pub tx: broadcast::Sender<String>,
    ring: Arc<Mutex<VecDeque<String>>>,
}

impl LogHub {
    pub fn new() -> Self {
        Self { tx: broadcast::channel(512).0, ring: Arc::new(Mutex::new(VecDeque::with_capacity(KEEP))) }
    }
    pub fn recent(&self) -> Vec<String> {
        self.ring.lock().unwrap().iter().cloned().collect()
    }
    fn push(&self, line: String) {
        let mut ring = self.ring.lock().unwrap();
        if ring.len() == KEEP {
            ring.pop_front();
        }
        ring.push_back(line.clone());
        let _ = self.tx.send(line);
    }
}

struct Message(String);
impl Visit for Message {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_string();
        }
    }
}

impl<S: Subscriber> Layer<S> for LogHub {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let level = *event.metadata().level();
        if level > Level::INFO || !event.metadata().target().starts_with("fdm") {
            return;
        }
        let mut msg = Message(String::new());
        event.record(&mut msg);
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let level = match level {
            Level::ERROR => "error",
            Level::WARN => "warn",
            _ => "info",
        };
        // Une ligne JSON par message : l'interface affiche l'heure locale du navigateur.
        self.push(serde_json::json!({"t": t as u64, "level": level, "msg": msg.0}).to_string());
    }
}
