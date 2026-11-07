use clap::{Parser, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Engine {
    /// gpt-live-1 : vraiment full duplex (écoute et parle en même temps, gère seul le rythme)
    Live,
    /// gpt-realtime : au tour par tour (détection de fin de phrase), avec outils
    Realtime,
}

#[derive(Parser, Debug, Clone)]
#[command(name = "fdm", about = "Agent vocal IA full duplex (OpenAI) qui participe à vos réunions Teams — audio natif PipeWire")]
pub struct Config {
    #[arg(long, env = "OPENAI_API_KEY", hide_env_values = true, default_value = "")]
    pub api_key: String,

    /// Moteur vocal
    #[arg(long, env = "FDM_ENGINE", value_enum, default_value = "live")]
    pub engine: Engine,

    /// Lien d'une réunion Teams : lance aussi `fdm-join` (navigateur du bot) une fois l'audio prêt
    #[arg(long, env = "FDM_JOIN")]
    pub join: Option<String>,

    /// Commande de l'assistant d'entrée dans Teams
    #[arg(long, env = "FDM_JOIN_CMD", default_value = "fdm-join")]
    pub join_cmd: String,

    /// Test audio sans IA : ce que le bot entend dans la réunion est rejoué dans son micro.
    /// Pratique pour vérifier le branchement dans Teams.
    #[arg(long)]
    pub echo_test: bool,

    /// Prénom de l'agent (affiché dans la réunion, et nom auquel il réagit)
    #[arg(long, env = "FDM_AGENT_NAME", default_value = "Nova")]
    pub name: String,

    /// Surnoms, nom de famille, nom complet… séparés par des virgules (« Nov,Martin,Nova Martin »)
    #[arg(long, env = "FDM_AGENT_ALIASES", default_value = "", value_delimiter = ',')]
    pub aliases: Vec<String>,

    /// Voix OpenAI (marin, cedar, alloy…)
    #[arg(long, env = "FDM_VOICE", default_value = "marin")]
    pub voice: String,

    /// Langue de la réunion
    #[arg(long, env = "FDM_LANGUAGE", default_value = "fr")]
    pub language: String,

    /// Contexte pour l'agent : rôle, ordre du jour, participants…
    #[arg(long, env = "FDM_CONTEXT", default_value = "")]
    pub context: String,

    #[arg(long, env = "FDM_API_BASE", default_value = "wss://api.openai.com/v1")]
    pub api_base: String,

    // ---------------------------------------------------------------- Live
    #[arg(long, env = "FDM_LIVE_MODEL", default_value = "gpt-live-1")]
    pub live_model: String,

    /// name = sa voix n'atteint la réunion que quand on l'a appelé par son nom ;
    /// off = on laisse le modèle juger seul quand parler
    #[arg(long, env = "FDM_GATE", default_value = "name", value_parser = ["name", "off"])]
    pub gate: String,

    /// En conversation : temps de parole des autres, sans qu'on s'adresse à lui, après lequel il
    /// considère que l'échange est fini et se remet en retrait (secondes). Ça ne limite jamais
    /// la durée de SES réponses.
    #[arg(long, env = "FDM_DISENGAGE_SECONDS", default_value_t = 90)]
    pub disengage_seconds: u64,

    /// Qui « réfléchit » quand gpt-live-1 délègue : responses = un modèle OpenAI (avec recherche
    /// web) ; client = personne pour l'instant (il répond avec ce qu'il sait)
    #[arg(long, env = "FDM_DELEGATION", default_value = "responses", value_parser = ["responses", "client"])]
    pub delegation: String,

    #[arg(long, env = "FDM_BACKEND_MODEL", default_value = "gpt-5.6-luna")]
    pub backend_model: String,

    /// Réserve audio avant de jouer la voix (ms) — anti-gigue pour gpt-live-1
    #[arg(long, env = "FDM_PREBUFFER_MS", default_value_t = 200)]
    pub prebuffer_ms: u64,

    // ---------------------------------------------------------------- Realtime
    #[arg(long, env = "FDM_REALTIME_MODEL", default_value = "gpt-realtime")]
    pub realtime_model: String,

    #[arg(long, env = "FDM_TRANSCRIPTION_MODEL", default_value = "gpt-4o-transcribe")]
    pub transcription_model: String,

    /// Realtime : durée de parole par-dessus l'agent avant qu'il se taise (ms). 0 = immédiat.
    #[arg(long, env = "FDM_BARGE_IN_MS", default_value_t = 400)]
    pub barge_in_ms: u64,
}

impl Config {
    pub fn all_names(&self) -> Vec<String> {
        std::iter::once(self.name.clone())
            .chain(self.aliases.iter().map(|a| a.trim().to_string()))
            .filter(|n| !n.is_empty())
            .collect()
    }

    pub fn instructions(&self) -> String {
        let names = self.all_names().join(", ");
        let tools = match self.engine {
            Engine::Realtime => "\n- Si une prise de parole ne t'est manifestement pas adressée (les autres se parlent entre \
eux), n'y réponds pas : appelle l'outil `rester_silencieux` au lieu de parler.\n- Si l'échange avec toi est terminé \
(on te remercie pour conclure, on passe la parole à quelqu'un d'autre), appelle l'outil `terminer_echange`.",
            Engine::Live => "\n- Si une prise de parole ne t'est manifestement pas adressée (les autres se parlent entre \
eux), tais-toi et écoute.\n- Des pensées silencieuses peuvent t'arriver pendant la réunion (par exemple « on vient \
de t'appeler », « personne ne t'a entendu ») : personne ne les a prononcées, ne les lis jamais à voix haute, \
sers-t'en pour savoir quand parler.",
        };
        let mut s = format!(
            "Tu es {name}, un participant à part entière d'une réunion Microsoft Teams en direct. Tu es une IA \
(ne prétends jamais le contraire si on te le demande), mais tu te comportes comme un collègue humain bien élevé.

Ce que tu entends : le son mixé de PLUSIEURS personnes qui parlent entre elles, parfois en même temps. \
Distingue-les au mieux par la voix, les prénoms prononcés et le contexte. Si tu ne sais pas qui a parlé, ne l'invente pas.

Quand parler :
- Tu suis toute la réunion pour en garder le contexte, mais tu ne prends la parole que si on t'appelle, comme on \
interpelle un collègue (on peut t'appeler : {names}).
- Une fois qu'on t'a adressé la parole, tu es dans l'échange : les questions et remarques suivantes te sont \
normalement destinées, même sans ton nom. Réponds-y, aussi longtemps que l'échange dure.{tools}

Comment parler :
- C'est de l'oral : phrases naturelles, pas de listes ni de markdown, pas de formules creuses.
- Adapte la longueur à la demande, librement : une phrase pour une question simple, plusieurs minutes si on te \
demande d'expliquer en détail. Il n'y a pas de limite : si quelqu'un veut intervenir, il te coupera.
- Si on te coupe la parole, arrête-toi et écoute ; reprends seulement si on te le demande.
- Adresse-toi aux gens par leur prénom quand tu le connais.
- Parle dans la langue de la réunion (par défaut : {lang}).",
            name = self.name,
            lang = self.language,
        );
        if !self.context.is_empty() {
            s.push_str("\n\nContexte de la réunion :\n");
            s.push_str(&self.context);
        }
        s
    }

    /// Prompt du modèle qui « réfléchit » quand gpt-live-1 délègue (moteur Live, délégation responses).
    pub fn backend_instructions(&self) -> String {
        let mut s = format!(
            "Tu es le cerveau de {name}, un assistant vocal qui participe à une réunion Teams. La voix (un autre \
modèle) te transmet ce qu'on lui demande ; ta réponse sera dite à l'oral par elle. Réponds juste, en français \
parlé, sans markdown, de façon complète mais sans délayer. Cherche sur le web quand il faut des faits récents.",
            name = self.name
        );
        if !self.context.is_empty() {
            s.push_str("\n\nContexte de la réunion :\n");
            s.push_str(&self.context);
        }
        s
    }
}
