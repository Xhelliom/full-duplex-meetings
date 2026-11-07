"""Configuration (variables d'environnement / .env, surchargées par la CLI)."""

from __future__ import annotations

import os
from dataclasses import dataclass, field

from dotenv import load_dotenv

# Noms des périphériques virtuels PipeWire/PulseAudio créés par l'app.
MEETING_SINK = "fdm_meeting"      # Teams (navigateur du bot) JOUE le son de la réunion ici
AI_VOICE_SINK = "fdm_ai_voice"    # l'app JOUE la voix de l'IA ici
AI_MIC_SOURCE = "fdm_ai_mic"      # ... et Teams l'ENTEND comme un micro

SAMPLE_RATE = 24_000  # PCM16 mono 24 kHz : format natif de l'API Realtime
CHANNELS = 1
BYTES_PER_SAMPLE = 2


def _bool(value: str | None, default: bool) -> bool:
    if value is None or value == "":
        return default
    return value.strip().lower() in {"1", "true", "yes", "on", "oui"}


@dataclass
class Config:
    api_key: str = ""
    model: str = "gpt-realtime"
    voice: str = "marin"
    agent_name: str = "Assistant IA"
    aliases: list[str] = field(default_factory=list)
    language: str = "fr"
    reply_mode: str = "name"  # "name" | "always"
    followup_seconds: float = 20.0
    barge_in: bool = True
    extra_instructions: str = ""
    transcription_model: str = "gpt-4o-transcribe"
    realtime_url: str = "wss://api.openai.com/v1/realtime"

    @classmethod
    def from_env(cls) -> "Config":
        load_dotenv()
        aliases = [a.strip() for a in os.getenv("FDM_AGENT_ALIASES", "").split(",") if a.strip()]
        return cls(
            api_key=os.getenv("OPENAI_API_KEY", ""),
            model=os.getenv("FDM_MODEL", "gpt-realtime"),
            voice=os.getenv("FDM_VOICE", "marin"),
            agent_name=os.getenv("FDM_AGENT_NAME", "Assistant IA"),
            aliases=aliases,
            language=os.getenv("FDM_LANGUAGE", "fr"),
            reply_mode=os.getenv("FDM_REPLY_MODE", "name"),
            followup_seconds=float(os.getenv("FDM_FOLLOWUP_SECONDS", "20")),
            barge_in=_bool(os.getenv("FDM_BARGE_IN"), True),
            extra_instructions=os.getenv("FDM_EXTRA_INSTRUCTIONS", ""),
            transcription_model=os.getenv("FDM_TRANSCRIPTION_MODEL", "gpt-4o-transcribe"),
            realtime_url=os.getenv("FDM_REALTIME_URL", "wss://api.openai.com/v1/realtime"),
        )

    def instructions(self) -> str:
        names = ", ".join([self.agent_name, *self.aliases])
        base = f"""Tu es « {self.agent_name} », un assistant IA qui participe EN DIRECT à une réunion Microsoft Teams, \
comme un participant à part entière. Tu entends le son de la réunion : c'est un mélange de PLUSIEURS personnes \
humaines qui parlent entre elles, parfois en même temps.

Règles :
- Il y a plusieurs interlocuteurs. Distingue-les au mieux par la voix, les prénoms prononcés et le contexte \
(« comme disait Paul… »). Si tu ne sais pas qui parle, ne l'invente pas ; tu peux demander « qui a posé la question ? ».
- Quand tu prends la parole, adresse-toi à la personne concernée par son prénom si tu le connais.
- Tu es à l'oral : réponses courtes (1 à 3 phrases), naturelles, sans listes ni markdown. Va droit au but.
- Tu peux être interrompu à tout moment : arrête-toi alors et laisse la parole.
- Tu suis toute la conversation pour garder le contexte, mais tu n'interviens que lorsqu'on s'adresse à toi \
(on t'appelle : {names}) ou quand on te pose clairement une question.
- Parle dans la langue de la réunion (par défaut : {self.language}).
- Tu es une IA : ne prétends jamais être humain si on te le demande."""
        if self.extra_instructions:
            base += f"\n\nContexte supplémentaire fourni par l'organisateur :\n{self.extra_instructions}"
        return base
