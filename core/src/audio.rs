//! Audio natif PipeWire.
//!
//! Le processus `fdm` crée lui-même deux nœuds PipeWire (ils disparaissent quand il s'arrête) :
//!
//! * `fdm_meeting` (classe `Audio/Sink`) : un « haut-parleur » virtuel. Le client Teams du bot y
//!   joue le son de la réunion ; on le reçoit directement dans le callback temps réel.
//! * `fdm_ai_mic` (classe `Audio/Source`) : un « micro » virtuel. On y écrit la voix de
//!   l'IA ; Teams l'enregistre comme n'importe quel micro.
//!
//! Format côté application : PCM S16LE mono 24 kHz (celui de l'API Realtime). PipeWire convertit
//! automatiquement depuis/vers le format du graphe (souvent 48 kHz stéréo).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use pipewire as pw;
use pw::{properties::properties, spa};
use tokio::sync::mpsc;

pub const SAMPLE_RATE: u32 = 24_000;
pub const MEETING_SINK: &str = "fdm_meeting";
pub const AI_MIC: &str = "fdm_ai_mic";
const STRIDE: usize = 2; // S16 mono

/// File d'attente de la voix de l'IA, lue par le callback PipeWire.
#[derive(Default)]
pub struct Playback {
    queue: Mutex<VecDeque<i16>>,
    /// Échantillons réellement remis au graphe audio depuis le début de l'élément courant.
    played: AtomicU64,
    /// Tampon anti-gigue : rien n'est joué tant qu'on n'a pas autant d'audio en réserve (et de
    /// nouveau après chaque panne sèche). gpt-live-1 envoie en temps réel par trames de 100 ms
    /// irrégulières : sans réserve, chaque trame en retard ferait un clic. 0 = désactivé.
    prime_samples: AtomicU64,
    primed: std::sync::atomic::AtomicBool,
}

impl Playback {
    pub fn with_prebuffer_ms(ms: u64) -> Self {
        let pb = Self::default();
        pb.prime_samples.store(ms * SAMPLE_RATE as u64 / 1000, Ordering::Relaxed);
        pb
    }
    pub fn push(&self, samples: &[i16]) {
        self.queue.lock().unwrap().extend(samples);
    }
    /// Coupe net la voix (interruption).
    pub fn flush(&self) {
        self.queue.lock().unwrap().clear();
        self.primed.store(false, Ordering::Relaxed);
    }
    pub fn is_playing(&self) -> bool {
        !self.queue.lock().unwrap().is_empty()
    }
    /// Remet à zéro le compteur au début d'une nouvelle réponse audio.
    pub fn start_item(&self) {
        self.played.store(0, Ordering::Relaxed);
    }
    pub fn played_ms(&self) -> u64 {
        self.played.load(Ordering::Relaxed) * 1000 / SAMPLE_RATE as u64
    }
    fn fill(&self, out: &mut [u8]) {
        let mut q = self.queue.lock().unwrap();
        if !self.primed.load(Ordering::Relaxed) {
            if (q.len() as u64) < self.prime_samples.load(Ordering::Relaxed).max(1) {
                out.fill(0);
                return;
            }
            self.primed.store(true, Ordering::Relaxed);
        }
        if q.is_empty() {
            self.primed.store(false, Ordering::Relaxed);
        }
        let mut n = 0u64;
        for frame in out.chunks_exact_mut(STRIDE) {
            let s = match q.pop_front() {
                Some(s) => {
                    n += 1;
                    s
                }
                None => 0,
            };
            frame.copy_from_slice(&s.to_le_bytes());
        }
        self.played.fetch_add(n, Ordering::Relaxed);
    }
}

pub struct Audio {
    /// Son de la réunion, par blocs (taille variable, ~10-20 ms).
    pub capture: mpsc::Receiver<Vec<i16>>,
    pub playback: Arc<Playback>,
}

fn format_param() -> Vec<u8> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::S16LE);
    info.set_rate(SAMPLE_RATE);
    info.set_channels(1);
    let mut pos = [0u32; spa::param::audio::MAX_CHANNELS];
    pos[0] = spa::sys::SPA_AUDIO_CHANNEL_MONO;
    info.set_position(pos);
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(spa::pod::Object {
            type_: spa::sys::SPA_TYPE_OBJECT_Format,
            id: spa::sys::SPA_PARAM_EnumFormat,
            properties: info.into(),
        }),
    )
    .expect("pod")
    .0
    .into_inner()
}

/// Démarre la boucle PipeWire dans un thread dédié et retourne les poignées audio.
pub fn start(prebuffer_ms: u64) -> Result<Audio> {
    let (tx, rx) = mpsc::channel::<Vec<i16>>(512);
    let playback = Arc::new(Playback::with_prebuffer_ms(prebuffer_ms));
    let pb = playback.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    std::thread::Builder::new()
        .name("pipewire".into())
        .spawn(move || {
            if let Err(e) = run_pipewire(tx, pb, &ready_tx) {
                let _ = ready_tx.send(Err(e.to_string()));
            }
        })?;

    ready_rx
        .recv()
        .map_err(|_| anyhow!("le thread PipeWire s'est arrêté"))?
        .map_err(|e| anyhow!("PipeWire : {e} (PipeWire est-il lancé ? `systemctl --user status pipewire`)"))?;
    Ok(Audio { capture: rx, playback })
}

fn run_pipewire(
    tx: mpsc::Sender<Vec<i16>>,
    playback: Arc<Playback>,
    ready: &std::sync::mpsc::Sender<Result<(), String>>,
) -> Result<(), pw::Error> {
    pw::init();
    let mainloop = pw::main_loop::MainLoop::new(None)?;
    let context = pw::context::Context::new(&mainloop)?;
    let core = context.connect(None)?;
    // AUTOCONNECT est indispensable : sans lui, WirePlumber n'active jamais ces nœuds (comme pw-loopback).
    let flags = pw::stream::StreamFlags::AUTOCONNECT
        | pw::stream::StreamFlags::MAP_BUFFERS
        | pw::stream::StreamFlags::RT_PROCESS;

    // --- Haut-parleur virtuel : on REÇOIT le son de la réunion ---
    let sink = pw::stream::Stream::new(
        &core,
        MEETING_SINK,
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CLASS => "Audio/Sink",
            *pw::keys::NODE_NAME => MEETING_SINK,
            *pw::keys::NODE_DESCRIPTION => "FDM Réunion (haut-parleur du bot)",
            *pw::keys::NODE_LATENCY => "480/48000",
            // Ne jamais devenir la sortie par défaut du PC (sinon VOTRE son partirait chez l'IA).
            "priority.session" => "1",
            "priority.driver" => "1",
        },
    )?;
    let _sink_listener = sink
        .add_local_listener_with_user_data(tx)
        .process(|stream, tx| {
            if let Some(mut buffer) = stream.dequeue_buffer() {
                let data = &mut buffer.datas_mut()[0];
                let size = data.chunk().size() as usize;
                if let Some(bytes) = data.data() {
                    let samples: Vec<i16> = bytes[..size.min(bytes.len())]
                        .chunks_exact(STRIDE)
                        .map(|b| i16::from_le_bytes([b[0], b[1]]))
                        .collect();
                    // Jamais bloquer le thread temps réel : on jette si l'app est en retard.
                    let _ = tx.try_send(samples);
                }
            }
        })
        .register()?;
    let p1 = format_param();
    sink.connect(
        spa::utils::Direction::Input,
        None,
        flags,
        &mut [spa::pod::Pod::from_bytes(&p1).unwrap()],
    )?;

    // --- Micro virtuel : on ÉCRIT la voix de l'IA ---
    let source = pw::stream::Stream::new(
        &core,
        AI_MIC,
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            // « Audio/Source/Virtual » n'est jamais activé par WirePlumber 0.4 (et bloque alors tous les
            // autres branchements) : « Audio/Source » fonctionne partout.
            *pw::keys::MEDIA_CLASS => "Audio/Source",
            *pw::keys::NODE_NAME => AI_MIC,
            *pw::keys::NODE_DESCRIPTION => "FDM Micro IA",
            *pw::keys::NODE_LATENCY => "480/48000",
            "priority.session" => "1",
            "priority.driver" => "1",
        },
    )?;
    let _source_listener = source
        .add_local_listener_with_user_data(playback)
        .process(|stream, pb| {
            if let Some(mut buffer) = stream.dequeue_buffer() {
                let requested = buffer.requested() as usize;
                let data = &mut buffer.datas_mut()[0];
                let n_frames = if let Some(slice) = data.data() {
                    let max = slice.len() / STRIDE;
                    let n = if requested > 0 { requested.min(max) } else { max.min(480) };
                    pb.fill(&mut slice[..n * STRIDE]);
                    n
                } else {
                    0
                };
                let chunk = data.chunk_mut();
                *chunk.offset_mut() = 0;
                *chunk.stride_mut() = STRIDE as _;
                *chunk.size_mut() = (n_frames * STRIDE) as _;
            }
        })
        .register()?;
    let p2 = format_param();
    source.connect(
        spa::utils::Direction::Output,
        None,
        flags,
        &mut [spa::pod::Pod::from_bytes(&p2).unwrap()],
    )?;

    let _ = ready.send(Ok(()));
    mainloop.run();
    Ok(())
}

/// Envoi cadencé vers l'API : un bloc de 40 ms à chaque tick, **complété par du silence**
/// quand personne ne joue rien dans la réunion. PipeWire n'appelle pas le callback d'un
/// haut-parleur que personne n'utilise : sans ça, l'API ne recevrait plus rien et ne verrait
/// jamais la fin d'une phrase. gpt-live-1, lui, exige un flux continu (il cale son débit dessus).
pub struct Uplink {
    pending: VecDeque<i16>,
}

pub const UPLINK_MS: u64 = 40;
const UPLINK_SAMPLES: usize = SAMPLE_RATE as usize * UPLINK_MS as usize / 1000;
/// Au-delà, on jette le plus ancien : mieux vaut perdre 100 ms que d'accumuler du retard.
const MAX_BACKLOG: usize = UPLINK_SAMPLES * 10;

impl Default for Uplink {
    fn default() -> Self {
        Self { pending: VecDeque::with_capacity(MAX_BACKLOG * 2) }
    }
}

impl Uplink {
    pub fn feed(&mut self, samples: &[i16]) {
        self.pending.extend(samples);
        while self.pending.len() > MAX_BACKLOG {
            self.pending.pop_front();
        }
    }

    /// Les blocs PCM16 (octets) à envoyer pour ce tick : toujours au moins un.
    pub fn tick(&mut self) -> Vec<Vec<u8>> {
        let blocks = if self.pending.len() >= UPLINK_SAMPLES * 4 { 2 } else { 1 };
        (0..blocks)
            .map(|_| {
                (0..UPLINK_SAMPLES)
                    .flat_map(|_| self.pending.pop_front().unwrap_or(0).to_le_bytes())
                    .collect()
            })
            .collect()
    }
}

/// Le bloc contient-il autre chose que du silence ?
pub fn is_audible(samples: &[i16]) -> bool {
    samples.iter().any(|s| s.unsigned_abs() > 64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uplink_complete_par_du_silence_et_rattrape_le_retard() {
        let mut up = Uplink::default();
        let b = up.tick();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].len(), UPLINK_SAMPLES * 2);
        assert!(b[0].iter().all(|x| *x == 0));
        up.feed(&vec![1000; UPLINK_SAMPLES * 5]);
        assert_eq!(up.tick().len(), 2);
        up.feed(&vec![1000; UPLINK_SAMPLES * 100]);
        assert!(up.pending.len() <= MAX_BACKLOG);
    }

    #[test]
    fn prebuffer() {
        let pb = Playback::with_prebuffer_ms(200);
        let mut out = vec![0u8; 960];
        pb.push(&[5; 2400]); // 100 ms : pas assez
        pb.fill(&mut out);
        assert!(out.iter().all(|x| *x == 0));
        pb.push(&[5; 2400]); // 200 ms : on joue
        pb.fill(&mut out);
        assert_eq!(&out[..2], &5i16.to_le_bytes());
        assert_eq!(pb.played_ms(), 20);
    }
}
