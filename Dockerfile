# syntax=docker/dockerfile:1

###############################################################################
# Stage 1 – Build the Rust server and CLI binaries
###############################################################################
FROM rust:1.88-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Layer the dependency build separately for better cache reuse.
# Stub out every crate so Cargo can resolve and compile all dependencies
# before we copy the real source.
COPY Cargo.toml Cargo.lock ./
COPY crates/archivr-core/Cargo.toml   crates/archivr-core/Cargo.toml
COPY crates/archivr-server/Cargo.toml crates/archivr-server/Cargo.toml
COPY crates/archivr-cli/Cargo.toml    crates/archivr-cli/Cargo.toml

RUN mkdir -p \
        crates/archivr-core/src \
        crates/archivr-server/src \
        crates/archivr-cli/src \
    && touch crates/archivr-core/src/lib.rs \
    && echo 'fn main() {}' > crates/archivr-server/src/main.rs \
    && echo 'fn main() {}' > crates/archivr-cli/src/main.rs \
    && cargo build --release -p archivr-server -p archivr-cli || true

# Build the real binaries; touch source files to force Cargo to relink.
COPY crates/ crates/
RUN touch \
    crates/archivr-core/src/lib.rs \
    crates/archivr-server/src/main.rs \
    crates/archivr-cli/src/main.rs \
    && cargo build --release -p archivr-server -p archivr-cli

###############################################################################
# Stage 2 – Runtime image
###############################################################################
FROM debian:bookworm-slim

ARG TARGETARCH

# Runtime dependencies:
#   chromium              used by single-file-cli for full-page archiving
#   nodejs (20+)          runtime for single-file-cli (requires Node >=20; Debian
#                         bookworm ships 18, so we install from the NodeSource repo)
#   ffmpeg                required by yt-dlp to merge separate audio/video streams
#                         (e.g. YouTube bestvideo+bestaudio format selection)
#   python3 + pip + venv  twitter scraper
#   ca-certificates       outbound HTTPS from the server and NodeSource HTTPS
#   libssl3               OpenSSL linked by the Rust binary
#   unzip                 unpacks the Chromium extensions and the Deno release zip
#   deno (pinned, below)  JS runtime for yt-dlp's YouTube challenge solver. Node 20
#                         is not usable: yt-dlp needs Node >= 22 and enables only
#                         Deno by default.
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl \
    ca-certificates \
    unzip \
    && curl -fsSL https://deb.nodesource.com/setup_20.x | bash - \
    && apt-get install -y --no-install-recommends \
    chromium \
    nodejs \
    ffmpeg \
    python3 \
    python3-pip \
    python3-venv \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Install single-file-cli globally so `single-file` is on PATH.
RUN npm install -g single-file-cli

# Install yt-dlp and twitter-api-client into an isolated venv to avoid
# conflicts with Debian's system Python packages.
# The [default] extra pulls in yt-dlp-ejs (the EJS challenge-solver library);
# without it Deno alone cannot solve YouTube challenges.
RUN python3 -m venv /opt/archivr-venv \
    && /opt/archivr-venv/bin/pip install --no-cache-dir \
        "yt-dlp[default]==2026.8.19" \
        "twitter-api-client==0.10.22"

# Pinned Deno — fallback JS runtime for yt-dlp's YouTube challenge solver.
# A newer copy installed by `archivr yt-dlp update` into ARCHIVR_STATE_DIR
# takes precedence. No auto-bump workflow exists: bump DENO_VERSION and BOTH
# sha256 values together. Adds roughly 80 MB to the image.
RUN set -eu; \
    DENO_VERSION=2.9.7; \
    DENO_SHA256_AMD64=c6527f24f4b16031d3ae4fa9f658d5f11534c8d84ce7dc8502420280919c3490; \
    DENO_SHA256_ARM64=c832298b1ad4422481334855f6003e0f54145762c5a134f20a489511d2f65bbf; \
    arch="${TARGETARCH:-$(dpkg --print-architecture)}"; \
    case "$arch" in \
        amd64) triple=x86_64-unknown-linux-gnu; sha="$DENO_SHA256_AMD64" ;; \
        arm64) triple=aarch64-unknown-linux-gnu; sha="$DENO_SHA256_ARM64" ;; \
        *) echo "ERROR: unsupported architecture for Deno: $arch"; exit 1 ;; \
    esac; \
    command -v unzip >/dev/null; \
    curl -fsSL "https://github.com/denoland/deno/releases/download/v${DENO_VERSION}/deno-${triple}.zip" \
        -o /tmp/deno.zip; \
    echo "${sha}  /tmp/deno.zip" | sha256sum -c -; \
    mkdir -p /usr/local/lib/archivr/deno; \
    unzip -q -o /tmp/deno.zip deno -d /usr/local/lib/archivr/deno; \
    rm /tmp/deno.zip; \
    chmod 0755 /usr/local/lib/archivr/deno/deno; \
    /usr/local/lib/archivr/deno/deno --version

# Download Chromium extensions used during headless captures.
# uBlock Origin Lite (MV3) — ad/tracker blocking.
# I Still Don't Care About Cookies (MV3) — cookie-banner dismissal.
RUN mkdir -p \
        /usr/local/lib/archivr/extensions/ublock-origin-lite \
        /usr/local/lib/archivr/extensions/istilldontcareaboutcookies \
    && curl -fsSL \
        "https://github.com/uBlockOrigin/uBOL-home/releases/download/2026.705.2152/uBOLite_2026.705.2152.chromium.zip" \
        -o /tmp/ublock.zip \
    && echo "e136ef0d86e43a40ee54ad7b4de01b2c305c81ff4ee9ffef8766ee19b2eee174  /tmp/ublock.zip" | sha256sum -c - \
    && unzip -q /tmp/ublock.zip -d /usr/local/lib/archivr/extensions/ublock-origin-lite \
    && rm /tmp/ublock.zip \
    && curl -fsSL \
        "https://github.com/OhMyGuus/I-Still-Dont-Care-About-Cookies/releases/download/v1.1.9/ISDCAC-chrome-source.zip" \
        -o /tmp/isdcac.zip \
    && echo "8f70ab947cb2d274f4022a970f5dd3cecd8ec02b060e05187bef9ee3cb18bbcb  /tmp/isdcac.zip" | sha256sum -c - \
    && unzip -q /tmp/isdcac.zip -d /usr/local/lib/archivr/extensions/istilldontcareaboutcookies \
    && rm /tmp/isdcac.zip \
    && test -f /usr/local/lib/archivr/extensions/istilldontcareaboutcookies/manifest.json \
        || { echo "ERROR: manifest.json not at ISDCAC extension root; zip structure may have changed"; exit 1; }

# Server and CLI binaries (CLI is needed to run `archivr init` on first setup)
COPY --from=builder /build/target/release/archivr-server /usr/local/bin/archivr-server
COPY --from=builder /build/target/release/archivr       /usr/local/bin/archivr

# Pre-built frontend assets (already compiled; no Vite build step needed)
COPY crates/archivr-server/static/ /usr/share/archivr-server/static/

# Twitter scraper script
COPY vendor/twitter/scrape_user_tweet_contents.py \
     /usr/local/lib/archivr/scrape_user_tweet_contents.py

# Wire up env vars that the server (and archivr-core) read at runtime.
# ARCHIVR_BIND and ARCHIVR_TWITTER_CREDENTIALS_FILE are intentionally left
# unset here — set them in docker-compose.yml or at `docker run` time.
ENV ARCHIVR_STATIC_DIR=/usr/share/archivr-server/static \
    ARCHIVR_CHROME=/usr/bin/chromium \
    ARCHIVR_SINGLE_FILE=/usr/local/bin/single-file \
    ARCHIVR_TWEET_PYTHON=/opt/archivr-venv/bin/python3 \
    ARCHIVR_TWEET_SCRAPER=/usr/local/lib/archivr/scrape_user_tweet_contents.py \
    ARCHIVR_YT_DLP=/opt/archivr-venv/bin/yt-dlp \
    ARCHIVR_DENO=/usr/local/lib/archivr/deno/deno \
    ARCHIVR_FFMPEG=/usr/bin/ffmpeg \
    ARCHIVR_STATE_DIR=/data/archivr-state \
    ARCHIVR_UBLOCK_EXT=/usr/local/lib/archivr/extensions/ublock-origin-lite \
    ARCHIVR_COOKIE_EXT=/usr/local/lib/archivr/extensions/istilldontcareaboutcookies \
    ARCHIVR_CHROME_ARGS=--no-sandbox

EXPOSE 8080

# Expects the TOML config at /config/archivr-server.toml (mount a volume).
# Copy docker/config.example.toml as a starting point.
# Using CMD (not ENTRYPOINT) so `docker compose run archivr archivr init …`
# can override the whole command for first-time archive initialisation.
CMD ["archivr-server", "/config/archivr-server.toml"]
