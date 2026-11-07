# Le bot dans sa propre machine : PipeWire, écran virtuel, Chromium et fdm, rien de partagé avec
# votre poste ni votre session Teams. Image de base Playwright (Ubuntu 24.04 + Chromium et ses
# dépendances), à la même version que join/package.json.
ARG PLAYWRIGHT_VERSION=1.56.1

# ---- cœur Rust (compilé sur la même base que l'image finale : mêmes bibliothèques) ----------------
FROM mcr.microsoft.com/playwright:v${PLAYWRIGHT_VERSION}-noble AS core
ARG RUST_VERSION=1.91
RUN apt-get update && apt-get install -y --no-install-recommends \
        rustc-${RUST_VERSION} cargo-${RUST_VERSION} build-essential pkg-config clang libclang-dev libpipewire-0.3-dev \
    && rm -rf /var/lib/apt/lists/*
ENV PATH=/usr/lib/rust-${RUST_VERSION}/bin:$PATH
WORKDIR /src
COPY core/ ./
RUN cargo build --release --locked

# ---- entrée dans la réunion (TypeScript) -------------------------------------------------------
FROM mcr.microsoft.com/playwright:v${PLAYWRIGHT_VERSION}-noble AS join
WORKDIR /opt/fdm-join
COPY join/package.json join/package-lock.json join/tsconfig.json ./
RUN npm ci --no-audit --no-fund
COPY join/src ./src
RUN npm run build && npm prune --omit=dev

# ---- image finale ------------------------------------------------------------------------------
FROM mcr.microsoft.com/playwright:v${PLAYWRIGHT_VERSION}-noble
RUN apt-get update && apt-get install -y --no-install-recommends \
        pipewire pipewire-pulse wireplumber pulseaudio-utils dbus xvfb x11vnc \
    && rm -rf /var/lib/apt/lists/*
COPY --from=core /src/target/release/fdm /usr/local/bin/fdm
COPY --from=join /opt/fdm-join /opt/fdm-join
RUN chmod +x /opt/fdm-join/dist/main.js && ln -s /opt/fdm-join/dist/main.js /usr/local/bin/fdm-join
COPY docker/entrypoint.sh /usr/local/bin/fdm-entrypoint
# Xvfb n'est pas root : le dossier de ses sockets doit exister d'avance.
RUN mkdir -p -m 1777 /tmp/.X11-unix
# En ENV (et pas seulement dans l'entrypoint) pour que `docker exec … pactl` voie le même PipeWire.
ENV XDG_RUNTIME_DIR=/tmp/fdm-runtime \
    DBUS_SESSION_BUS_ADDRESS=unix:path=/tmp/fdm-runtime/bus \
    DISPLAY=:99 \
    FDM_BROWSER_PROFILE=/home/pwuser/profile \
    FDM_DATA_DIR=/home/pwuser/data \
    FDM_UI_ADDR=0.0.0.0:8080
RUN mkdir -p /home/pwuser/profile /home/pwuser/data && chown -R pwuser /home/pwuser
USER pwuser
WORKDIR /home/pwuser
EXPOSE 8080 5900
ENTRYPOINT ["fdm-entrypoint"]
# Par défaut : l'interface web ; le bot attend qu'on lui donne un lien de réunion.
CMD ["--serve"]
