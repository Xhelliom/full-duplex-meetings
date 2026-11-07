//! Aides IA de l'interface, avec la clé OpenAI du bot : proposer un nom, rédiger un prompt système à
//! partir de quelques mots, et faire écouter chaque voix (extrait généré une fois, puis gardé).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::config::Config;

/// Synthèse vocale de la question posée à gpt-live pour enregistrer un extrait de sa voix.
const TTS_MODEL: &str = "gpt-4o-mini-tts";

/// Ce que l'interface envoie : le brief et les champs du formulaire, enregistrés ou non.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Draft {
    pub brief: String,
    pub name: String,
    pub language: String,
    pub expertise: String,
    pub proactivity: String,
    pub behavior: String,
    /// Noms déjà proposés, à ne pas reproposer.
    pub avoid: Vec<String>,
}

impl Draft {
    /// Les champs du formulaire priment sur les réglages enregistrés.
    fn apply(&self, cfg: &mut Config) {
        let set = |v: &mut String, d: &str| {
            if !d.trim().is_empty() {
                *v = d.trim().to_string();
            }
        };
        set(&mut cfg.name, &self.name);
        set(&mut cfg.language, &self.language);
        set(&mut cfg.expertise, &self.expertise);
        set(&mut cfg.proactivity, &self.proactivity);
        set(&mut cfg.behavior, &self.behavior);
    }
}

fn http_base(cfg: &Config) -> String {
    let b = cfg.api_base.trim_end_matches('/');
    match (b.strip_prefix("wss://"), b.strip_prefix("ws://")) {
        (Some(rest), _) => format!("https://{rest}"),
        (_, Some(rest)) => format!("http://{rest}"),
        _ => b.to_string(),
    }
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder().timeout(Duration::from_secs(120)).build()?)
}

fn need_key(cfg: &Config) -> Result<()> {
    if cfg.api_key.trim().is_empty() {
        bail!("clé API OpenAI manquante : renseignez-la et enregistrez");
    }
    Ok(())
}

async fn openai_error(res: reqwest::Response) -> anyhow::Error {
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    let msg = body["error"]["message"].as_str().map(String::from).unwrap_or_else(|| status.to_string());
    anyhow::anyhow!("OpenAI : {msg}")
}

/// Le texte d'une réponse de l'API Responses.
fn output_text(body: &Value) -> String {
    let mut out = String::new();
    for item in body["output"].as_array().into_iter().flatten() {
        for c in item["content"].as_array().into_iter().flatten() {
            if c["type"] == "output_text" {
                out.push_str(c["text"].as_str().unwrap_or_default());
            }
        }
    }
    out.trim().to_string()
}

/// Un appel simple au modèle de réflexion (`backend_model`), sans outils.
async fn respond(cfg: &Config, instructions: &str, input: &str) -> Result<String> {
    need_key(cfg)?;
    let res = client()?
        .post(format!("{}/responses", http_base(cfg)))
        .bearer_auth(cfg.api_key.trim())
        .json(&json!({"model": cfg.backend_model, "instructions": instructions, "input": input}))
        .send()
        .await
        .context("API OpenAI injoignable")?;
    if !res.status().is_success() {
        return Err(openai_error(res).await);
    }
    let text = output_text(&res.json().await.context("réponse OpenAI illisible")?);
    if text.is_empty() {
        bail!("OpenAI n'a rien renvoyé");
    }
    Ok(text)
}

/// Un nom court, facile à dire et à reconnaître à l'oral.
pub async fn suggest_name(mut cfg: Config, d: &Draft) -> Result<String> {
    d.apply(&mut cfg);
    let instructions = format!(
        "Tu proposes le nom d'un participant IA aux réunions en ligne d'une équipe. On l'interpelle à voix haute : \
le nom doit être court (une ou deux syllabes de préférence), facile à prononcer et à reconnaître à l'oral en {lang}, \
sans ambiguïté avec des mots courants, et pas un prénom très répandu, pour qu'on ne le confonde pas avec un \
collègue. Il peut évoquer son rôle, sans jeu de mots lourd. Réponds uniquement par le nom : pas de guillemets, pas \
de ponctuation, pas d'explication.",
        lang = cfg.language
    );
    let mut avoid = d.avoid.clone();
    avoid.push(cfg.name.clone());
    let input = format!(
        "Rôle et domaines : {}\nCe qu'en dit l'organisateur : {}\nNoms à ne pas reproposer : {}",
        or_none(&cfg.expertise),
        or_none(&d.brief),
        avoid.join(", ")
    );
    let raw = respond(&cfg, &instructions, &input).await?;
    let name: String = raw
        .lines()
        .next()
        .unwrap_or_default()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .chars()
        .take(40)
        .collect();
    if name.is_empty() {
        bail!("pas de nom proposé, réessayez");
    }
    Ok(name)
}

fn or_none(s: &str) -> &str {
    if s.trim().is_empty() {
        "(non précisé)"
    } else {
        s.trim()
    }
}

/// Un prompt système complet, rédigé d'après quelques mots, sur la structure du prompt par défaut.
pub async fn write_system_prompt(mut cfg: Config, d: &Draft) -> Result<String> {
    d.apply(&mut cfg);
    if d.brief.trim().is_empty() {
        bail!("décrivez en quelques mots ce que vous voulez");
    }
    // Le prompt par défaut, avec les marqueurs à la place du nom et de la langue, sert de modèle.
    let mut reference = cfg.clone();
    reference.system_prompt.clear();
    reference.context.clear();
    reference.behavior.clear();
    reference.name = "{name}".into();
    reference.language = "{language}".into();
    let reference = reference.instructions();

    let instructions = "Tu rédiges le prompt système d'un modèle vocal temps réel (OpenAI GPT-Live, full duplex) qui \
participe à des réunions en ligne. Pars du prompt de référence : garde ses sections et ce qui fait qu'il se comporte \
bien en réunion (quand parler et se taire, pas d'acquiescements, s'arrêter quand on lui coupe la parole, déléguer \
les recherches et les calculs), et adapte le rôle, le ton, les moments où il intervient et la façon de parler à ce \
que veut l'organisateur. Style du guide de prompting GPT-Live : des sections courtes avec \
des titres « # … », des phrases simples et concrètes, des exemples brefs entre guillemets quand c'est utile, pas \
de listes à rallonge ni de majuscules d'insistance. C'est un prompt pour l'oral. Écris le prompt en français, et \
garde tels quels les marqueurs {name} et {language} (ils sont remplacés par le nom et la langue). N'y recopie ni les \
consignes de comportement ni le contexte de la réunion : ils sont ajoutés automatiquement à la fin du prompt. Réponds uniquement par le prompt, sans bloc de code ni \
commentaire.";
    let input = format!(
        "Ce que veut l'organisateur, en quelques mots :\n{brief}\n\nRéglages actuels\n- rôle et domaines : {exp}\n- \
prise de parole spontanée : {pro}\n- consignes de comportement (ajoutées à la fin, ne pas recopier) : {beh}\n\nPrompt de référence :\n{reference}",
        brief = d.brief.trim(),
        exp = or_none(&cfg.expertise),
        pro = cfg.proactivity,
        beh = or_none(&cfg.behavior),
    );
    let text = respond(&cfg, instructions, &input).await?;
    Ok(strip_fence(&text))
}

/// Des idées notées en vrac -> des consignes de comportement claires, sans en ajouter.
pub async fn rewrite_behavior(mut cfg: Config, d: &Draft) -> Result<String> {
    d.apply(&mut cfg);
    if d.brief.trim().is_empty() {
        bail!("notez d'abord vos idées dans le champ");
    }
    let instructions = "Tu reformules les consignes de comportement qu'un organisateur donne à un participant IA \
vocal de ses réunions en ligne. Ces consignes sont ajoutées à son prompt système. Reprends toutes les idées de \
l'organisateur et seulement elles : n'en ajoute pas, n'en retire pas, ne change pas leur sens. Corrige la langue, \
lève les ambiguïtés, et écris des phrases courtes à l'impératif, adressées au participant en le tutoyant, une idée \
par phrase, sans liste à puces ni titre, sans majuscules d'insistance. Quand une consigne est vague, rends-la \
concrète en restant fidèle à l'intention (par exemple « sois pas trop bavard » devient « Fais des réponses courtes, \
sauf si on te demande de détailler. »). Écris en français. Réponds uniquement par les consignes.";
    let input = format!(
        "Rôle du participant : {}\n\nConsignes de l'organisateur, telles qu'il les a notées :\n{}",
        or_none(&cfg.expertise),
        d.brief.trim()
    );
    Ok(strip_fence(&respond(&cfg, instructions, &input).await?))
}

/// Enlève un éventuel bloc de code autour du texte.
fn strip_fence(s: &str) -> String {
    let t = s.trim();
    if let Some(rest) = t.strip_prefix("```") {
        let body = rest.split_once('\n').map(|(_, b)| b).unwrap_or("");
        return body.trim_end().trim_end_matches("```").trim().to_string();
    }
    t.to_string()
}

pub fn valid_voice(v: &str) -> bool {
    !v.is_empty() && v.len() <= 32 && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// (code de langue, question posée, phrase qu'il doit dire).
fn sample(language: &str) -> (&'static str, &'static str, &'static str) {
    if language.trim().to_lowercase().starts_with("fr") {
        ("fr", "Bonjour ! Tu peux te présenter en une phrase ?",
         "Bonjour à tous ! Je suis là pour suivre la réunion avec vous. Si vous avez une question, ou si vous voulez \
que je vérifie quelque chose, n'hésitez pas à m'interpeller.")
    } else {
        ("en", "Hi! Can you introduce yourself in one sentence?",
         "Hi everyone! I'm here to follow the meeting with you. If you have a question, or want me to check \
something, just ask.")
    }
}

fn voice_path(cfg: &Config, voice: &str, lang: &str) -> PathBuf {
    cfg.data_dir().join("voices").join(format!("{voice}-{lang}.wav"))
}

async fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

/// L'extrait d'une voix (wav), enregistré au premier appel puis gardé avec les réglages.
///
/// Les voix de gpt-live n'existent que dans gpt-live : on lui fait vraiment dire une phrase. Une
/// courte session entend une question (synthèse vocale, gardée elle aussi) et on enregistre sa réponse.
pub async fn voice_sample(cfg: &Config, voice: &str) -> Result<Vec<u8>> {
    if !valid_voice(voice) {
        bail!("nom de voix invalide");
    }
    let (lang, question, line) = sample(&cfg.language);
    let path = voice_path(cfg, voice, lang);
    if let Ok(bytes) = tokio::fs::read(&path).await {
        return Ok(bytes);
    }
    need_key(cfg)?;
    let question = question_audio(cfg, lang, question).await?;
    let pcm = record_live(cfg, voice, line, &question).await?;
    let wav = wav(&trim_silence(&pcm));
    write_atomic(&path, &wav).await?;
    Ok(wav)
}

/// La question posée à gpt-live, en PCM 24 kHz (format de sortie `pcm` de l'API audio/speech).
async fn question_audio(cfg: &Config, lang: &str, text: &str) -> Result<Vec<i16>> {
    let path = cfg.data_dir().join("voices").join(format!("_question-{lang}.pcm"));
    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(_) => {
            let res = client()?
                .post(format!("{}/audio/speech", http_base(cfg)))
                .bearer_auth(cfg.api_key.trim())
                .json(&json!({"model": TTS_MODEL, "voice": "alloy", "input": text, "response_format": "pcm"}))
                .send()
                .await
                .context("API OpenAI injoignable")?;
            if !res.status().is_success() {
                return Err(openai_error(res).await);
            }
            let b = res.bytes().await?.to_vec();
            write_atomic(&path, &b).await?;
            b
        }
    };
    Ok(bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect())
}

/// Une session gpt-live d'une dizaine de secondes : on lui joue la question, on enregistre sa voix.
async fn record_live(base: &Config, voice: &str, line: &str, question: &[i16]) -> Result<Vec<i16>> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    use futures_util::{SinkExt, StreamExt};
    use tokio::time::{timeout, Instant};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

    let mut cfg = base.clone();
    cfg.voice = voice.to_string();
    cfg.delegation = "client".into();
    cfg.context.clear();
    cfg.system_prompt = format!(
        "Tu fais écouter ta voix. Quand on te demande de te présenter, réponds exactement par cette phrase, sans \
rien ajouter ni changer, d'un ton naturel et chaleureux, puis tais-toi : « {line} »"
    );
    let url = format!("{}/live/sessions", cfg.api_base.trim_end_matches('/'));
    let mut req = url.as_str().into_client_request()?;
    req.headers_mut().insert("Authorization", format!("Bearer {}", cfg.api_key.trim()).parse()?);
    let (ws, _) = timeout(Duration::from_secs(10), tokio_tungstenite::connect_async(req))
        .await
        .context("gpt-live ne répond pas")?
        .context("connexion à gpt-live")?;
    let (mut tx, mut rx) = ws.split();
    let start = json!({"type": "session.start", "session": crate::live::session_config(&cfg, &[], &[])});
    tx.send(Message::text(start.to_string())).await?;

    // Ce qu'on lui fait entendre : un temps de silence, la question, puis du silence (flux continu).
    let block = (crate::audio::SAMPLE_RATE as u64 * crate::audio::UPLINK_MS / 1000) as usize;
    let lead = vec![0i16; crate::audio::SAMPLE_RATE as usize / 2];
    let mut input = lead.into_iter().chain(question.iter().copied());
    let mut ticker = tokio::time::interval(Duration::from_millis(crate::audio::UPLINK_MS));
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut started = false;
    let mut out: Vec<i16> = Vec::new();
    let mut last_voice: Option<Instant> = None;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if Instant::now() > deadline {
                    break;
                }
                // Il a fini sa phrase : 1,5 s sans rien d'audible après avoir parlé.
                if last_voice.is_some_and(|t| t.elapsed() > Duration::from_millis(1500)) {
                    break;
                }
                if started {
                    let chunk: Vec<i16> = (0..block).map(|_| input.next().unwrap_or(0)).collect();
                    let bytes: Vec<u8> = chunk.iter().flat_map(|s| s.to_le_bytes()).collect();
                    tx.send(Message::text(json!({"type": "session.input_audio.append", "audio": B64.encode(bytes)}).to_string())).await?;
                }
            }
            msg = rx.next() => {
                let Some(msg) = msg else { bail!("gpt-live a fermé la connexion") };
                let Message::Text(text) = msg? else { continue };
                let ev: Value = serde_json::from_str(&text)?;
                match ev["type"].as_str().unwrap_or("") {
                    "session.started" => started = true,
                    "session.output_audio.delta" => {
                        let bytes = B64.decode(ev["delta"].as_str().unwrap_or(""))?;
                        let samples: Vec<i16> = bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
                        if crate::audio::is_audible(&samples) {
                            last_voice = Some(Instant::now());
                        }
                        if last_voice.is_some() {
                            out.extend(samples);
                        }
                    }
                    "error" => bail!("gpt-live : {}", ev["error"]["message"].as_str().unwrap_or(&ev["error"].to_string())),
                    "session.closed" => break,
                    _ => {}
                }
            }
        }
    }
    let _ = tx.send(Message::text(json!({"type": "session.close"}).to_string())).await;
    let _ = tx.close().await;
    if out.is_empty() {
        bail!("gpt-live n'a rien dit avec la voix « {voice} » (voix inconnue ?)");
    }
    Ok(out)
}

/// Garde 100 ms de silence avant et après la voix.
fn trim_silence(pcm: &[i16]) -> Vec<i16> {
    let margin = crate::audio::SAMPLE_RATE as usize / 10;
    let first = pcm.iter().position(|s| s.unsigned_abs() > 64).unwrap_or(0);
    let last = pcm.iter().rposition(|s| s.unsigned_abs() > 64).unwrap_or(pcm.len().saturating_sub(1));
    pcm[first.saturating_sub(margin)..(last + 1 + margin).min(pcm.len())].to_vec()
}

/// PCM 16 bits mono 24 kHz -> fichier WAV.
fn wav(pcm: &[i16]) -> Vec<u8> {
    let rate = crate::audio::SAMPLE_RATE;
    let data_len = (pcm.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // taille du bloc fmt
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes()); // octets par seconde
    out.extend_from_slice(&2u16.to_le_bytes()); // octets par trame
    out.extend_from_slice(&16u16.to_le_bytes()); // bits
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn base_http_et_texte_de_reponse() {
        let cfg = Config::parse_from(["fdm"]);
        assert_eq!(http_base(&cfg), "https://api.openai.com/v1");
        let body = json!({"output": [
            {"type": "reasoning", "summary": []},
            {"type": "message", "content": [{"type": "output_text", "text": " Lyra \n"}]}
        ]});
        assert_eq!(output_text(&body), "Lyra");
        assert_eq!(strip_fence("```text\n# Rôle\nTu es {name}.\n```"), "# Rôle\nTu es {name}.");
        assert_eq!(strip_fence("# Rôle"), "# Rôle");
    }

    #[test]
    fn voix_et_formulaire() {
        assert!(valid_voice("marin") && !valid_voice("../x") && !valid_voice(""));
        let mut cfg = Config::parse_from(["fdm"]);
        Draft { name: " Iris ".into(), expertise: "juriste".into(), ..Default::default() }.apply(&mut cfg);
        assert_eq!((cfg.name.as_str(), cfg.expertise.as_str(), cfg.language.as_str()), ("Iris", "juriste", "français"));
        assert_eq!(sample("Français").0, "fr");
        assert_eq!(sample("english").0, "en");
    }

    #[test]
    fn extrait_wav_sans_blancs_superflus() {
        let mut pcm = vec![0i16; 24_000];
        pcm.extend([1000i16; 2400]);
        pcm.extend(vec![0i16; 24_000]);
        let trimmed = trim_silence(&pcm);
        assert_eq!(trimmed.len(), 2400 + 2 * 2400);
        let w = wav(&trimmed);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(&w[8..16], b"WAVEfmt ");
        assert_eq!(w.len(), 44 + trimmed.len() * 2);
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 24_000);
    }
}
