"""Pont full-duplex : audio de la réunion <-> API OpenAI Realtime <-> micro virtuel."""

from __future__ import annotations

import asyncio
import base64
import json
import logging

import websockets

from .addressing import AddressDetector
from .audio import Capture, Player
from .config import SAMPLE_RATE, Config

log = logging.getLogger(__name__)

# Le nom des événements a changé entre la bêta et la version GA de l'API : on accepte les deux.
AUDIO_DELTA = {"response.output_audio.delta", "response.audio.delta"}
AUDIO_TRANSCRIPT_DONE = {"response.output_audio_transcript.done", "response.audio_transcript.done"}


class MeetingAgent:
    def __init__(self, cfg: Config, capture: Capture | None = None, player: Player | None = None):
        self.cfg = cfg
        self.capture = capture or Capture()
        self.player = player or Player()
        self.detector = AddressDetector(
            [cfg.agent_name, *cfg.aliases], mode=cfg.reply_mode, followup_seconds=cfg.followup_seconds
        )
        self.ws = None
        self.response_active = False
        self.current_item_id: str | None = None

    # ------------------------------------------------------------------ session
    def session_update(self) -> dict:
        return {
            "type": "session.update",
            "session": {
                "type": "realtime",
                "model": self.cfg.model,
                "instructions": self.cfg.instructions(),
                "output_modalities": ["audio"],
                "audio": {
                    "input": {
                        "format": {"type": "audio/pcm", "rate": SAMPLE_RATE},
                        # Le son vient d'un mix de micros distants : réduction de bruit « far field ».
                        "noise_reduction": {"type": "far_field"},
                        "transcription": {"model": self.cfg.transcription_model, "language": self.cfg.language},
                        "turn_detection": {
                            "type": "semantic_vad",
                            "eagerness": "auto",
                            # On décide nous-mêmes quand répondre (sinon l'IA répondrait à TOUT le monde).
                            "create_response": self.cfg.reply_mode == "always",
                            "interrupt_response": self.cfg.barge_in,
                        },
                    },
                    "output": {
                        "format": {"type": "audio/pcm", "rate": SAMPLE_RATE},
                        "voice": self.cfg.voice,
                    },
                },
            },
        }

    async def send(self, event: dict) -> None:
        await self.ws.send(json.dumps(event))

    # ------------------------------------------------------------------ boucles
    async def run(self) -> None:
        if not self.cfg.api_key:
            raise SystemExit("OPENAI_API_KEY manquant (voir .env.example)")
        url = f"{self.cfg.realtime_url}?model={self.cfg.model}"
        headers = {"Authorization": f"Bearer {self.cfg.api_key}"}
        log.info("Connexion à %s", url)
        async with websockets.connect(url, additional_headers=headers, max_size=None) as ws:
            self.ws = ws
            await self.send(self.session_update())
            await self.capture.start()
            await self.player.start()
            try:
                await asyncio.gather(self._uplink(), self._downlink())
            finally:
                await self.capture.stop()
                await self.player.stop()

    async def _uplink(self) -> None:
        """Réunion -> OpenAI (en continu, même pendant que l'IA parle : full duplex)."""
        async for chunk in self.capture.chunks():
            await self.send({"type": "input_audio_buffer.append", "audio": base64.b64encode(chunk).decode()})

    async def _downlink(self) -> None:
        async for raw in self.ws:
            await self.handle_event(json.loads(raw))

    # ------------------------------------------------------------------ événements
    async def handle_event(self, ev: dict) -> None:
        t = ev.get("type", "")

        if t in AUDIO_DELTA:
            self.current_item_id = ev.get("item_id", self.current_item_id)
            await self.player.write(base64.b64decode(ev["delta"]))

        elif t == "input_audio_buffer.speech_started":
            log.debug("Quelqu'un parle…")
            if self.cfg.barge_in and (self.response_active or self.player.is_playing()):
                await self.interrupt()

        elif t == "conversation.item.input_audio_transcription.completed":
            text = ev.get("transcript", "").strip()
            if not text:
                return
            log.info("🎙  Réunion : %s", text)
            if self.cfg.reply_mode != "always" and not self.response_active and self.detector.should_reply(text):
                log.info("→ l'IA est interpellée, elle répond")
                self.response_active = True
                await self.send({"type": "response.create"})

        elif t == "response.created":
            self.response_active = True

        elif t in AUDIO_TRANSCRIPT_DONE:
            log.info("🤖 %s : %s", self.cfg.agent_name, ev.get("transcript", ""))

        elif t == "response.done":
            self.response_active = False
            self.detector.reply_finished()

        elif t == "error":
            err = ev.get("error", {})
            # Annuler une réponse déjà finie n'est pas grave.
            if err.get("code") != "response_cancel_not_active":
                log.error("Erreur Realtime : %s", err)

        elif t in {"session.created", "session.updated"}:
            log.info("Session Realtime : %s", t)

    async def interrupt(self) -> None:
        """Quelqu'un coupe la parole à l'IA : on se tait et on dit au modèle ce qui a vraiment été entendu."""
        played = self.player.played_ms()
        await self.player.flush()
        if self.response_active:
            # Avec interrupt_response=true le serveur annule déjà ; on l'envoie quand même par sécurité.
            await self.send({"type": "response.cancel"})
        if self.current_item_id:
            await self.send(
                {
                    "type": "conversation.item.truncate",
                    "item_id": self.current_item_id,
                    "content_index": 0,
                    "audio_end_ms": played,
                }
            )
        log.info("✋ Interrompue après %d ms", played)
        self.response_active = False
        self.current_item_id = None
        self.detector.reply_finished()
