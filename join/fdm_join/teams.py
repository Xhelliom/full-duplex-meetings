"""Fait rejoindre une réunion Teams au bot, via un navigateur Chromium dédié (Playwright).

Le navigateur est lancé avec PULSE_SINK / PULSE_SOURCE pointant sur les périphériques virtuels :
il « entend » donc la voix de l'IA comme micro et joue la réunion dans fdm_meeting. C'est un
participant distinct de vous : vous pouvez rester dans la réunion avec votre propre client Teams.

L'interface de Teams change souvent : chaque étape est en « best effort ». Si une étape échoue,
la fenêtre reste ouverte et vous pouvez cliquer vous-même (le pont audio fonctionne quand même).
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import re
import subprocess
from pathlib import Path

# Noms des nœuds PipeWire créés par le binaire Rust `fdm` (qui doit tourner AVANT).
MEETING_SINK = "fdm_meeting"
AI_MIC_SOURCE = "fdm_ai_mic"

log = logging.getLogger(__name__)

PROFILE_DIR = Path(os.getenv("FDM_BROWSER_PROFILE", ".fdm-browser-profile")).resolve()

# Libellés EN / FR des boutons Teams.
CONTINUE_IN_BROWSER = re.compile(r"(continue on this browser|continuer sur ce navigateur|join on the web)", re.I)
JOIN_NOW = re.compile(r"^\s*(join now|rejoindre maintenant|join|rejoindre)\s*$", re.I)
LEAVE = re.compile(r"(leave|quitter)", re.I)


async def _click_first(page, locators, timeout_ms: int) -> bool:
    """Essaie plusieurs sélecteurs et clique sur le premier visible."""
    deadline = asyncio.get_running_loop().time() + timeout_ms / 1000
    while asyncio.get_running_loop().time() < deadline:
        for loc in locators:
            try:
                el = loc.first
                if await el.is_visible():
                    await el.click()
                    return True
            except Exception:  # noqa: BLE001 - l'UI Teams est imprévisible
                pass
        await asyncio.sleep(0.5)
    return False


class TeamsBot:
    def __init__(self, meeting_url: str, display_name: str, headless: bool = False, browser_path: str | None = None):
        self.meeting_url = meeting_url
        self.display_name = display_name
        self.headless = headless
        self.browser_path = browser_path or os.getenv("FDM_BROWSER_PATH") or None
        self._pw = None
        self.context = None
        self.page = None

    async def start(self) -> None:
        from playwright.async_api import async_playwright

        self._pw = await async_playwright().start()
        env = {**os.environ, "PULSE_SINK": MEETING_SINK, "PULSE_SOURCE": AI_MIC_SOURCE}
        PROFILE_DIR.mkdir(parents=True, exist_ok=True)
        self.context = await self._pw.chromium.launch_persistent_context(
            str(PROFILE_DIR),
            headless=self.headless,
            executable_path=self.browser_path,
            env=env,
            permissions=["microphone", "camera", "notifications"],
            args=[
                "--use-fake-ui-for-media-stream",  # accepte automatiquement micro/caméra
                "--autoplay-policy=no-user-gesture-required",
                "--disable-features=HardwareMediaKeyHandling",
            ],
            viewport={"width": 1280, "height": 800},
            locale="fr-FR",
        )
        self.page = self.context.pages[0] if self.context.pages else await self.context.new_page()

    async def join(self) -> bool:
        page = self.page
        log.info("Ouverture du lien de réunion…")
        await page.goto(self.meeting_url, wait_until="domcontentloaded")

        # 1) Page « Comment voulez-vous rejoindre ? » -> navigateur
        if await _click_first(
            page,
            [page.locator('[data-tid="joinOnWeb"]'), page.get_by_role("button", name=CONTINUE_IN_BROWSER),
             page.get_by_text(CONTINUE_IN_BROWSER)],
            timeout_ms=20_000,
        ):
            log.info("« Continuer sur ce navigateur » cliqué")

        # 2) Écran de pré-connexion : nom (si invité), caméra coupée
        name_box = None
        for loc in (
            page.locator('[data-tid="prejoin-display-name-input"]'),
            page.get_by_placeholder(re.compile(r"(name|nom)", re.I)),
        ):
            try:
                await loc.first.wait_for(state="visible", timeout=15_000)
                name_box = loc.first
                break
            except Exception:  # noqa: BLE001
                continue
        if name_box is not None:
            await name_box.fill(self.display_name)
            log.info("Nom saisi : %s", self.display_name)
        else:
            log.info("Pas de champ nom (session connectée à un compte ?)")

        await self._ensure_camera_off()

        # 3) Rejoindre
        if not await _click_first(
            page,
            [page.locator('[data-tid="prejoin-join-button"]'), page.get_by_role("button", name=JOIN_NOW)],
            timeout_ms=30_000,
        ):
            log.warning("Bouton « Rejoindre maintenant » introuvable : cliquez-le à la main dans la fenêtre.")
            return False
        log.info("Demande de participation envoyée (l'organisateur doit peut-être l'admettre depuis la salle d'attente)")

        # 4) Attendre d'être dans l'appel (bouton Quitter visible)
        try:
            await page.get_by_role("button", name=LEAVE).first.wait_for(state="visible", timeout=10 * 60_000)
            log.info("✅ Le bot est dans la réunion")
            return True
        except Exception:  # noqa: BLE001
            log.warning("Toujours pas admis dans la réunion après 10 min.")
            return False

    async def _ensure_camera_off(self) -> None:
        page = self.page
        try:
            toggle = page.locator('[data-tid="toggle-video"]').first
            if await toggle.is_visible() and (await toggle.get_attribute("aria-checked")) == "true":
                await toggle.click()
                log.info("Caméra coupée")
        except Exception:  # noqa: BLE001
            pass

    async def wait_closed(self) -> None:
        """Retourne quand l'utilisateur ferme la fenêtre du bot."""
        closed = asyncio.Event()
        self.context.on("close", lambda *_: closed.set())
        await closed.wait()

    async def stop(self) -> None:
        try:
            if self.page and not self.page.is_closed():
                await _click_first(self.page, [self.page.get_by_role("button", name=LEAVE)], timeout_ms=2_000)
            if self.context:
                await self.context.close()
        finally:
            if self._pw:
                await self._pw.stop()


# ---------------------------------------------------------------------------
# Filet de sécurité : si le navigateur ignore PULSE_SINK/PULSE_SOURCE, on déplace ses flux à la main.
# ---------------------------------------------------------------------------

def _browser_pids() -> set[int]:
    """PIDs du Chromium utilisant notre profil, et de tous ses sous-processus (le son passe par un
    processus « audio service » enfant qui n'a pas le profil dans sa ligne de commande)."""
    roots, parent = set(), {}
    for proc in Path("/proc").iterdir():
        if not proc.name.isdigit():
            continue
        try:
            cmd = (proc / "cmdline").read_bytes().replace(b"\0", b" ").decode(errors="ignore")
            stat = (proc / "stat").read_text()
        except OSError:
            continue
        pid = int(proc.name)
        parent[pid] = int(stat.rsplit(")", 1)[1].split()[1])
        if str(PROFILE_DIR) in cmd:
            roots.add(pid)
    pids = set(roots)
    changed = True
    while changed:
        changed = False
        for pid, ppid in parent.items():
            if ppid in pids and pid not in pids:
                pids.add(pid)
                changed = True
    return pids


def _pactl_json(kind: str) -> list[dict]:
    try:
        out = subprocess.run(["pactl", "-f", "json", "list", kind], check=True, capture_output=True, text=True)
        return json.loads(out.stdout or "[]")
    except (subprocess.CalledProcessError, json.JSONDecodeError, FileNotFoundError):
        return []


def route_browser_streams() -> None:
    pids = _browser_pids()
    if not pids:
        return
    sinks = {s["index"]: s["name"] for s in _pactl_json("sinks")}
    sources = {s["index"]: s["name"] for s in _pactl_json("sources")}
    for si in _pactl_json("sink-inputs"):
        pid = int(si.get("properties", {}).get("application.process.id", -1) or -1)
        if pid in pids and sinks.get(si.get("sink")) != MEETING_SINK:
            subprocess.run(["pactl", "move-sink-input", str(si["index"]), MEETING_SINK], check=False)
            log.info("Flux audio du navigateur déplacé vers %s", MEETING_SINK)
    for so in _pactl_json("source-outputs"):
        pid = int(so.get("properties", {}).get("application.process.id", -1) or -1)
        if pid in pids and sources.get(so.get("source")) != AI_MIC_SOURCE:
            subprocess.run(["pactl", "move-source-output", str(so["index"]), AI_MIC_SOURCE], check=False)
            log.info("Micro du navigateur déplacé vers %s", AI_MIC_SOURCE)


async def keep_streams_routed(interval: float = 3.0) -> None:
    while True:
        await asyncio.to_thread(route_browser_streams)
        await asyncio.sleep(interval)
