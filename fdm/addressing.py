"""Décide si l'IA doit répondre à une prise de parole (réunion multi-participants)."""

from __future__ import annotations

import re
import time
import unicodedata


def normalize(text: str) -> str:
    text = unicodedata.normalize("NFKD", text)
    text = "".join(c for c in text if not unicodedata.combining(c))
    text = text.lower().replace("’", "'")
    return re.sub(r"[^a-z0-9' ]+", " ", text)


class AddressDetector:
    """Mode "name" : répond seulement si on l'appelle, puis reste « en conversation » quelques secondes.
    Mode "always" : répond à chaque tour de parole (utile en tête-à-tête)."""

    def __init__(self, names: list[str], mode: str = "name", followup_seconds: float = 20.0, clock=time.monotonic):
        self.mode = mode
        self.followup_seconds = followup_seconds
        self.clock = clock
        self._last_reply_end: float | None = None
        patterns = []
        for name in names:
            n = normalize(name).strip()
            if n:
                patterns.append(r"(?<![a-z0-9])" + re.escape(n).replace(r"\ ", r"\s+") + r"(?![a-z0-9])")
        self._regex = re.compile("|".join(patterns)) if patterns else None

    def mentions_agent(self, transcript: str) -> bool:
        return bool(self._regex and self._regex.search(normalize(transcript)))

    def in_followup(self) -> bool:
        return self._last_reply_end is not None and self.clock() - self._last_reply_end < self.followup_seconds

    def should_reply(self, transcript: str) -> bool:
        if not transcript.strip():
            return False
        if self.mode == "always":
            return True
        return self.mentions_agent(transcript) or self.in_followup()

    def reply_finished(self) -> None:
        self._last_reply_end = self.clock()
