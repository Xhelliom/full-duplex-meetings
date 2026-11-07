"""Routage audio sous PipeWire (via la couche de compatibilité PulseAudio : pactl / parec / pacat).

Graphe créé :

    [Navigateur du bot Teams] --(haut-parleurs)--> fdm_meeting (null sink)
                                                      └─ .monitor ──> parec ──> l'app ──> OpenAI Realtime

    OpenAI Realtime ──> l'app ──> pacat ──> fdm_ai_voice (null sink)
                                                └─ .monitor ──> fdm_ai_mic (remap-source) ──(micro)──> [Navigateur du bot Teams]

Optionnel (--monitor) : loopbacks vers vos vraies enceintes pour entendre ce que le bot entend / dit.
"""

from __future__ import annotations

import asyncio
import logging
import shutil
import subprocess
import time

from .config import AI_MIC_SOURCE, AI_VOICE_SINK, BYTES_PER_SAMPLE, CHANNELS, MEETING_SINK, SAMPLE_RATE

log = logging.getLogger(__name__)

REQUIRED_TOOLS = ("pactl", "parec", "pacat")


def check_tools() -> list[str]:
    """Retourne la liste des outils manquants."""
    return [t for t in REQUIRED_TOOLS if shutil.which(t) is None]


def _pactl(*args: str) -> str:
    return subprocess.run(["pactl", *args], check=True, capture_output=True, text=True).stdout.strip()


def _existing_names(kind: str) -> set[str]:
    out = _pactl("list", "short", kind)
    return {line.split("\t")[1] for line in out.splitlines() if "\t" in line}


class VirtualDevices:
    """Crée (et supprime à la fin) les périphériques virtuels."""

    def __init__(self, monitor: bool = False):
        self.monitor = monitor
        self.module_ids: list[str] = []

    def _load(self, *args: str) -> None:
        mid = _pactl("load-module", *args)
        self.module_ids.append(mid)
        log.debug("module chargé %s: %s", mid, " ".join(args))

    def setup(self) -> None:
        sinks = _existing_names("sinks")
        sources = _existing_names("sources")
        if MEETING_SINK not in sinks:
            self._load(
                "module-null-sink",
                f"sink_name={MEETING_SINK}",
                "sink_properties=device.description=FDM-Reunion-(haut-parleurs-du-bot)",
            )
        if AI_VOICE_SINK not in sinks:
            self._load(
                "module-null-sink",
                f"sink_name={AI_VOICE_SINK}",
                "sink_properties=device.description=FDM-Voix-IA",
            )
        if AI_MIC_SOURCE not in sources:
            self._load(
                "module-remap-source",
                f"master={AI_VOICE_SINK}.monitor",
                f"source_name={AI_MIC_SOURCE}",
                "source_properties=device.description=FDM-Micro-IA",
            )
        if self.monitor:
            self._load("module-loopback", f"source={MEETING_SINK}.monitor", "latency_msec=60")
            self._load("module-loopback", f"source={AI_VOICE_SINK}.monitor", "latency_msec=60")
        log.info("Périphériques virtuels prêts : sink=%s, micro=%s", MEETING_SINK, AI_MIC_SOURCE)

    def teardown(self) -> None:
        for mid in reversed(self.module_ids):
            try:
                _pactl("unload-module", mid)
            except subprocess.CalledProcessError as e:  # pragma: no cover
                log.warning("Impossible de décharger le module %s: %s", mid, e.stderr)
        self.module_ids.clear()


def teardown_all() -> None:
    """Supprime tous les modules fdm_* (même ceux laissés par une session plantée)."""
    out = _pactl("list", "short", "modules")
    for line in out.splitlines():
        parts = line.split("\t")
        if len(parts) >= 3 and "fdm_" in parts[2]:
            _pactl("unload-module", parts[0])
            log.info("Module %s déchargé (%s)", parts[0], parts[1])


_PCM_ARGS = ["--format=s16le", f"--rate={SAMPLE_RATE}", f"--channels={CHANNELS}", "--raw"]


class Capture:
    """Lit le son de la réunion (monitor du sink fdm_meeting) en PCM16 24 kHz mono."""

    def __init__(self, device: str = f"{MEETING_SINK}.monitor", chunk_ms: int = 40):
        self.device = device
        self.chunk_bytes = SAMPLE_RATE * BYTES_PER_SAMPLE * CHANNELS * chunk_ms // 1000
        self.proc: asyncio.subprocess.Process | None = None

    async def start(self) -> None:
        self.proc = await asyncio.create_subprocess_exec(
            "parec", f"--device={self.device}", "--latency-msec=20", *_PCM_ARGS,
            stdout=asyncio.subprocess.PIPE,
        )

    async def chunks(self):
        assert self.proc and self.proc.stdout
        while True:
            data = await self.proc.stdout.readexactly(self.chunk_bytes)
            yield data

    async def stop(self) -> None:
        if self.proc and self.proc.returncode is None:
            self.proc.kill()
            await self.proc.wait()


class Player:
    """Joue la voix de l'IA dans fdm_ai_voice. `flush()` coupe immédiatement (interruption)."""

    def __init__(self, device: str = AI_VOICE_SINK):
        self.device = device
        self.proc: asyncio.subprocess.Process | None = None
        self._bytes_written = 0
        self._started_at: float | None = None

    async def _spawn(self) -> None:
        self.proc = await asyncio.create_subprocess_exec(
            "pacat", "--playback", f"--device={self.device}", "--latency-msec=60", *_PCM_ARGS,
            stdin=asyncio.subprocess.PIPE,
        )

    async def start(self) -> None:
        await self._spawn()

    async def write(self, pcm: bytes) -> None:
        if self.proc is None or self.proc.returncode is not None:
            await self._spawn()
        assert self.proc and self.proc.stdin
        now = time.monotonic()
        # Si la lecture précédente est terminée, on repart d'une nouvelle « horloge ».
        if self._started_at is None or now > self._started_at + self.audio_ms_written() / 1000 + 0.5:
            self._started_at = now
            self._bytes_written = 0
        self.proc.stdin.write(pcm)
        self._bytes_written += len(pcm)
        await self.proc.stdin.drain()

    def audio_ms_written(self) -> int:
        return self._bytes_written * 1000 // (SAMPLE_RATE * BYTES_PER_SAMPLE * CHANNELS)

    def played_ms(self) -> int:
        """Estimation de ce qui a réellement été entendu par la réunion."""
        if self._started_at is None:
            return 0
        elapsed = int((time.monotonic() - self._started_at) * 1000)
        return max(0, min(elapsed, self.audio_ms_written()))

    def is_playing(self) -> bool:
        return self._started_at is not None and self.played_ms() < self.audio_ms_written()

    async def flush(self) -> None:
        """Vide le tampon audio en tuant pacat (le prochain write le relance)."""
        if self.proc and self.proc.returncode is None:
            self.proc.kill()
            await self.proc.wait()
        self.proc = None
        self._started_at = None
        self._bytes_written = 0

    async def stop(self) -> None:
        await self.flush()
