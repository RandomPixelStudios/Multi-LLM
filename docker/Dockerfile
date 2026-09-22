# Multi LLM als Docker-App.
#
# Build-Kontext ist das Repo-Root (z. B. `docker build -f docker/Dockerfile .`),
# damit Frontend (npm) und Backend (cargo) in einem Image landen.
#
# Im Container läuft die App headless (`multi-llm --headless`) und serviert
# auf Port 5000:
#   /            -> Web-Dashboard (Status, Provider/Models, Usage)
#   /v1/*        -> OpenAI-kompatible API
# Das native Tauri-Desktopfenster gibt es im Container nicht; Dashboard + API
# sind die browserfähige Oberfläche der App.

# ---------- Stufe 1: Frontend bauen (Vite) ----------
FROM node:20-bookworm-slim AS frontend
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY index.html vite.config.ts tsconfig.json ./
COPY src/ ./src/
COPY public/ ./public/
RUN npm run build

# ---------- Stufe 2: Backend bauen (Rust/Tauri, headless-fähig) ----------
FROM rust:1-bookworm AS backend
# Tauri Build-Abhängigkeiten (Debian bookworm = webkit2gtk 4.1, wie Tauri 2 es braucht)
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential curl wget file \
    libssl-dev \
    libgtk-3-dev \
    libwebkit2gtk-4.1-dev \
    libayatana-appindicator3-dev \
    librsvg2-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/build.rs ./src-tauri/
COPY src-tauri/capabilities/ ./src-tauri/capabilities/
COPY src-tauri/icons/ ./src-tauri/icons/
COPY src-tauri/tauri.conf.json ./src-tauri/
# Frontend-Dist aus Stufe 1 (tauri-build erwartet frontendDist beim Kompilieren)
COPY --from=frontend /app/dist/ ./dist/
COPY src-tauri/src/ ./src-tauri/src/
COPY src-tauri/web/ ./src-tauri/web/
WORKDIR /app/src-tauri
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/src-tauri/target \
    cargo build --release && cp target/release/multi-llm /usr/local/bin/multi-llm

# ---------- Stufe 3: schlankes Laufzeit-Image ----------
FROM debian:bookworm-slim AS runtime
# Nur Laufzeit-Libs (kein Compiler): GTK/WebKit werden vom gelinkten Tauri-
# Binary verlangt, auch wenn headless nie ein Fenster öffnet. ca-certificates
# ist Pflicht für TLS zu den Upstream-Providern.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libgtk-3-0 \
    libwebkit2gtk-4.1-0 \
    libayatana-appindicator3-1 \
    librsvg2-2 \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=backend /usr/local/bin/multi-llm /usr/local/bin/multi-llm

# Fester Nicht-Root-User; /data hält settings/secrets/usage (Volume).
RUN useradd --create-home --uid 1000 --user-group appuser \
    && mkdir -p /data && chown appuser:appuser /data
USER appuser
VOLUME /data

# Ein Port für alle (5000); Profile werden pro API-Key/Session zugeordnet.
# MULTI_LLM_SERVER=1 schaltet den Single-Port-Multi-User-Modus an.
ENV MULTI_LLM_DATA_DIR=/data \
    MULTI_LLM_PORT=5000 \
    MULTI_LLM_EXPOSE=1 \
    MULTI_LLM_SERVER=1
EXPOSE 5000

ENTRYPOINT ["/usr/local/bin/multi-llm"]
CMD ["--serve"]
