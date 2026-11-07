"""Point d'entrée : `fdm join <lien>`, `fdm listen`, `fdm audio-setup`, `fdm audio-teardown`, `fdm doctor`."""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import logging
import signal
import sys

from . import audio
from .agent import MeetingAgent
from .config import AI_MIC_SOURCE, MEETING_SINK, Config

log = logging.getLogger("fdm")


def _apply_overrides(cfg: Config, args: argparse.Namespace) -> Config:
    for attr in ("agent_name", "voice", "model", "language", "reply_mode"):
        value = getattr(args, attr, None)
        if value:
            setattr(cfg, attr, value)
    if getattr(args, "context", None):
        cfg.extra_instructions = (cfg.extra_instructions + "\n" + args.context).strip()
    if getattr(args, "no_barge_in", False):
        cfg.barge_in = False
    return cfg


async def _run(args: argparse.Namespace) -> None:
    cfg = _apply_overrides(Config.from_env(), args)
    missing = audio.check_tools()
    if missing:
        sys.exit(f"Outils manquants : {', '.join(missing)} → sudo pacman -S libpulse pipewire-pulse")

    devices = audio.VirtualDevices(monitor=args.monitor)
    devices.setup()
    bot = None
    tasks: list[asyncio.Task] = []
    try:
        agent = MeetingAgent(cfg)
        tasks.append(asyncio.create_task(agent.run(), name="agent"))

        if args.command == "join":
            from .teams import TeamsBot, keep_streams_routed

            bot = TeamsBot(args.url, cfg.agent_name, headless=args.headless)
            await bot.start()
            tasks.append(asyncio.create_task(keep_streams_routed(), name="routing"))
            tasks.append(asyncio.create_task(bot.join(), name="join"))
            tasks.append(asyncio.create_task(bot.wait_closed(), name="browser-closed"))
        else:
            print(
                f"\nDans Teams : Paramètres → Périphériques\n"
                f"  • Haut-parleur : « FDM-Reunion » ({MEETING_SINK})\n"
                f"  • Micro        : « FDM-Micro-IA » ({AI_MIC_SOURCE})\n"
                "Ctrl+C pour arrêter.\n"
            )

        stop = asyncio.Event()
        loop = asyncio.get_running_loop()
        for sig in (signal.SIGINT, signal.SIGTERM):
            loop.add_signal_handler(sig, stop.set)
        stopper = asyncio.create_task(stop.wait(), name="signal")

        # On s'arrête quand : Ctrl+C, l'agent plante, ou la fenêtre du bot est fermée.
        watch = [stopper, *(t for t in tasks if t.get_name() in {"agent", "browser-closed"})]
        done, _ = await asyncio.wait(watch, return_when=asyncio.FIRST_COMPLETED)
        for t in done:
            if t.get_name() == "agent" and t.exception():
                log.error("L'agent s'est arrêté : %r", t.exception())
    finally:
        for t in tasks:
            t.cancel()
        for t in tasks:
            with contextlib.suppress(BaseException):
                await t
        if bot:
            with contextlib.suppress(Exception):
                await bot.stop()
        devices.teardown()
        log.info("Terminé, périphériques virtuels supprimés.")


def _doctor() -> int:
    ok = True
    missing = audio.check_tools()
    print("pactl/parec/pacat :", "OK" if not missing else f"MANQUANT {missing} → sudo pacman -S libpulse pipewire-pulse")
    ok &= not missing
    if not missing:
        import subprocess

        info = subprocess.run(["pactl", "info"], capture_output=True, text=True).stdout
        server = next((line for line in info.splitlines() if line.startswith("Server Name")), "Server Name: ?")
        print(server, "(PipeWire recommandé)")
    cfg = Config.from_env()
    print("OPENAI_API_KEY    :", "OK" if cfg.api_key else "MANQUANT (fichier .env)")
    ok &= bool(cfg.api_key)
    try:
        import playwright  # noqa: F401

        print("playwright        : OK (si besoin : python -m playwright install chromium)")
    except ImportError:
        print("playwright        : MANQUANT → pip install playwright && python -m playwright install chromium")
        ok = False
    return 0 if ok else 1


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="fdm", description="Agent vocal IA (OpenAI Realtime) dans Microsoft Teams")
    p.add_argument("-v", "--verbose", action="store_true")
    sub = p.add_subparsers(dest="command", required=True)

    def agent_opts(sp):
        sp.add_argument("--name", dest="agent_name", help="Nom du bot dans la réunion (et nom auquel il réagit)")
        sp.add_argument("--voice", help="Voix OpenAI (marin, cedar, alloy, …)")
        sp.add_argument("--model", help="Modèle Realtime (défaut gpt-realtime)")
        sp.add_argument("--language", help="Langue de la réunion (fr, en, …)")
        sp.add_argument("--reply-mode", choices=["name", "always"], help="name = répond quand on l'appelle")
        sp.add_argument("--context", help="Contexte/rôle supplémentaire (ordre du jour, etc.)")
        sp.add_argument("--no-barge-in", action="store_true", help="Ne pas se taire quand on lui coupe la parole")
        sp.add_argument("--monitor", action="store_true", help="Entendre aussi la réunion + l'IA sur vos enceintes")

    j = sub.add_parser("join", help="Le bot rejoint la réunion Teams (navigateur dédié) et parle")
    j.add_argument("url", help="Lien de la réunion Teams")
    j.add_argument("--headless", action="store_true", help="Navigateur sans fenêtre (moins fiable avec Teams)")
    agent_opts(j)

    listen = sub.add_parser("listen", help="Seulement le pont audio : vous branchez Teams vous-même sur les périphériques FDM")
    agent_opts(listen)

    sub.add_parser("audio-setup", help="Crée les périphériques virtuels (et les laisse)")
    sub.add_parser("audio-teardown", help="Supprime tous les périphériques virtuels FDM")
    sub.add_parser("doctor", help="Vérifie l'installation")
    return p


def main(argv: list[str] | None = None) -> None:
    args = build_parser().parse_args(argv)
    logging.basicConfig(
        level=logging.DEBUG if args.verbose else logging.INFO,
        format="%(asctime)s %(levelname)-7s %(name)s: %(message)s",
        datefmt="%H:%M:%S",
    )
    if args.command == "doctor":
        sys.exit(_doctor())
    if args.command == "audio-setup":
        audio.VirtualDevices().setup()
        return
    if args.command == "audio-teardown":
        audio.teardown_all()
        return
    asyncio.run(_run(args))


if __name__ == "__main__":
    main()
