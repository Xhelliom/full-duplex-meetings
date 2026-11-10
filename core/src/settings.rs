//! Réglages persistants (`settings.json`), modifiables depuis l'interface web. Ils priment sur
//! les valeurs par défaut de la ligne de commande / de l'environnement.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::mcp::McpServer;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub api_key: String,
    pub name: String,
    pub voice: String,
    pub language: String,
    pub expertise: String,
    pub proactivity: String,
    pub pace: String,
    pub behavior: String,
    pub context: String,
    pub system_prompt: String,
    pub model: String,
    pub delegation: String,
    pub backend_model: String,
    pub web_search: bool,
    pub mcp_servers: Vec<McpServer>,
}

impl Settings {
    pub fn path(cfg: &Config) -> PathBuf {
        cfg.data_dir().join("settings.json")
    }

    pub fn from_config(cfg: &Config) -> Self {
        Self {
            api_key: cfg.api_key.clone(),
            name: cfg.name.clone(),
            voice: cfg.voice.clone(),
            language: cfg.language.clone(),
            expertise: cfg.expertise.clone(),
            proactivity: cfg.proactivity.clone(),
            pace: cfg.pace.clone(),
            behavior: cfg.behavior.clone(),
            context: cfg.context.clone(),
            system_prompt: cfg.system_prompt.clone(),
            model: cfg.model.clone(),
            delegation: cfg.delegation.clone(),
            backend_model: cfg.backend_model.clone(),
            web_search: !cfg.no_web_search,
            mcp_servers: cfg.mcp_servers.clone(),
        }
    }

    /// Réglages du fichier s'il existe, sinon ceux de la configuration. Une clé API vide dans le
    /// fichier laisse passer celle de l'environnement.
    pub fn load(cfg: &Config) -> Result<Self> {
        let path = Self::path(cfg);
        if !path.exists() {
            return Ok(Self::from_config(cfg));
        }
        let text = std::fs::read_to_string(&path).with_context(|| format!("lecture de {}", path.display()))?;
        let mut s: Settings = serde_json::from_str(&text).with_context(|| format!("{} illisible", path.display()))?;
        if s.api_key.is_empty() {
            s.api_key = cfg.api_key.clone();
        }
        Ok(s.with_defaults(cfg))
    }

    /// Champs vides -> valeurs par défaut (un fichier partiel reste utilisable).
    fn with_defaults(mut self, cfg: &Config) -> Self {
        let fill = |v: &mut String, d: &str| {
            if v.trim().is_empty() {
                *v = d.to_string();
            }
        };
        fill(&mut self.name, &cfg.name);
        fill(&mut self.voice, &cfg.voice);
        fill(&mut self.language, &cfg.language);
        fill(&mut self.proactivity, &cfg.proactivity);
        fill(&mut self.pace, &cfg.pace);
        fill(&mut self.model, &cfg.model);
        fill(&mut self.delegation, &cfg.delegation);
        fill(&mut self.backend_model, &cfg.backend_model);
        self
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        // Contient la clé API : lisible par le seul propriétaire.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// La configuration effective d'une session.
    pub fn apply(&self, base: &Config) -> Config {
        let mut c = base.clone();
        c.api_key = self.api_key.clone();
        c.name = self.name.clone();
        c.voice = self.voice.clone();
        c.language = self.language.clone();
        c.expertise = self.expertise.clone();
        c.proactivity = self.proactivity.clone();
        c.pace = self.pace.clone();
        c.behavior = self.behavior.clone();
        c.context = self.context.clone();
        c.system_prompt = self.system_prompt.clone();
        c.model = self.model.clone();
        c.delegation = self.delegation.clone();
        c.backend_model = self.backend_model.clone();
        c.no_web_search = !self.web_search;
        c.mcp_servers = self.mcp_servers.clone();
        c
    }

    /// Clé API affichable (« sk-…abcd »).
    pub fn masked_key(&self) -> String {
        let k = self.api_key.trim();
        if k.len() <= 8 {
            return if k.is_empty() { String::new() } else { "…".into() };
        }
        format!("{}…{}", &k[..3], &k[k.len() - 4..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn aller_retour_fichier_et_fusion() {
        let dir = std::env::temp_dir().join(format!("fdm-test-{}", std::process::id()));
        let cfg = Config::parse_from(["fdm", "--data-dir", dir.to_str().unwrap(), "--api-key", "sk-env-1234567890"]);
        let mut s = Settings::load(&cfg).unwrap();
        assert_eq!(s.name, "Nova");
        assert!(s.web_search);
        s.name = "Iris".into();
        s.api_key = String::new(); // vide : la clé d'environnement reste utilisée
        s.behavior = "Tutoie tout le monde.".into();
        s.save(&Settings::path(&cfg)).unwrap();
        let back = Settings::load(&cfg).unwrap();
        assert_eq!(back.name, "Iris");
        assert_eq!(back.api_key, "sk-env-1234567890");
        assert_eq!(back.masked_key(), "sk-…7890");
        let eff = back.apply(&cfg);
        assert!(eff.instructions().contains("Tutoie tout le monde."));
        assert!(eff.instructions().contains("Tu es Iris"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn debit_de_parole() {
        let mut cfg = Config::parse_from(["fdm"]);
        assert!(!cfg.instructions().contains("débit soutenu"));
        cfg.pace = "rapide".into();
        assert!(cfg.instructions().contains("Parle français. Parle avec un débit soutenu"));
        cfg.system_prompt = "Tu es {name}.".into();
        assert!(cfg.instructions().contains("# Débit\nParle avec un débit soutenu"));
    }

    #[test]
    fn prompt_systeme_complet() {
        let mut cfg = Config::parse_from(["fdm"]);
        cfg.system_prompt = "Tu es {name}. Parle {language}.".into();
        cfg.context = "Revue de sprint".into();
        cfg.behavior = "Tutoie tout le monde.".into();
        let p = cfg.instructions();
        assert!(p.starts_with("Tu es Nova. Parle français."));
        assert!(p.contains("Revue de sprint"));
        // les consignes restent ajoutées, avant le contexte
        assert!(p.find("Tutoie tout le monde.").unwrap() < p.find("Revue de sprint").unwrap());
    }
}
