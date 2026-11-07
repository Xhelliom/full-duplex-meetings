#!/usr/bin/env bash
# Démarre la « machine » du bot : bus D-Bus, PipeWire + WirePlumber + pipewire-pulse, écran
# virtuel (Xvfb) — puis fdm, qui lance lui-même le navigateur du bot si FDM_JOIN est défini.
set -euo pipefail

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp/fdm-runtime}"
mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
# État WirePlumber neuf à chaque démarrage : il mémorise les cibles audio des applications, et une
# cible disparue bloque ses branchements.
rm -rf "${XDG_STATE_HOME:-$HOME/.local/state}/wireplumber"

export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"
dbus-daemon --session --address="$DBUS_SESSION_BUS_ADDRESS" --fork --nopidfile

pipewire &
for _ in $(seq 50); do [ -S "$XDG_RUNTIME_DIR/pipewire-0" ] && break; sleep 0.1; done
wireplumber &
pipewire-pulse &
for _ in $(seq 50); do pactl info >/dev/null 2>&1 && break; sleep 0.1; done
pactl info >/dev/null || { echo "PipeWire n'a pas démarré" >&2; exit 1; }

# Écran virtuel pour Chromium ; FDM_VNC=1 pour le regarder (et cliquer) sur le port 5900.
export DISPLAY="${DISPLAY:-:99}"
Xvfb "$DISPLAY" -screen 0 1280x800x24 -nolisten tcp &
for _ in $(seq 50); do [ -e "/tmp/.X11-unix/X${DISPLAY#:}" ] && break; sleep 0.1; done
if [ "${FDM_VNC:-0}" = "1" ]; then
  x11vnc -display "$DISPLAY" -forever -shared -nopw -quiet -rfbport 5900 &
  echo "🖥️  écran du bot : vnc://localhost:5900"
fi

exec fdm "$@"
