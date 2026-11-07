import argparse
import asyncio
import contextlib
import logging
import subprocess
import sys

from .teams import AI_MIC_SOURCE, MEETING_SINK, TeamsBot, keep_streams_routed

log = logging.getLogger("fdm-join")


def devices_present() -> bool:
    try:
        sinks = subprocess.run(["pactl", "list", "short", "sinks"], capture_output=True, text=True).stdout
        sources = subprocess.run(["pactl", "list", "short", "sources"], capture_output=True, text=True).stdout
    except FileNotFoundError:
        sys.exit("pactl introuvable → sudo pacman -S libpulse")
    return MEETING_SINK in sinks and AI_MIC_SOURCE in sources


async def run(args) -> None:
    if not devices_present():
        sys.exit(f"Périphériques « {MEETING_SINK} » / « {AI_MIC_SOURCE} » absents : lancez d'abord `fdm` dans un autre terminal.")
    bot = TeamsBot(args.url, args.name, headless=args.headless)
    await bot.start()
    routing = asyncio.create_task(keep_streams_routed())
    try:
        await bot.join()
        await bot.wait_closed()  # jusqu'à ce que vous fermiez la fenêtre (ou Ctrl+C)
    finally:
        routing.cancel()
        with contextlib.suppress(Exception):
            await bot.stop()


def main() -> None:
    p = argparse.ArgumentParser(prog="fdm-join", description="Fait rejoindre une réunion Teams au bot (navigateur dédié)")
    p.add_argument("url", help="Lien de la réunion Teams")
    p.add_argument("--name", default="Nova", help="Nom affiché dans la réunion (mettez le même que FDM_AGENT_NAME)")
    p.add_argument("--headless", action="store_true", help="Sans fenêtre (moins fiable avec Teams)")
    p.add_argument("-v", "--verbose", action="store_true")
    args = p.parse_args()
    logging.basicConfig(level=logging.DEBUG if args.verbose else logging.INFO,
                        format="%(asctime)s %(levelname)-7s %(message)s", datefmt="%H:%M:%S")
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(run(args))


if __name__ == "__main__":
    main()
