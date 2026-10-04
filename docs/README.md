<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="branding/assets/banner-dark.png">
    <img alt="Archivr — Preserve what matters. Forever." src="branding/assets/banner-light.png" width="860">
  </picture>
</p>

<p align="center">
  <a href="../LICENSE.md"><img src="https://img.shields.io/badge/license-MIT-8d3f30?style=flat-square" alt="MIT License"></a>
  <img src="https://img.shields.io/badge/rust-2024_edition-b78342?style=flat-square&logo=rust&logoColor=white" alt="Rust 2024">
  <img src="https://img.shields.io/badge/self--hosted-yes-245f72?style=flat-square" alt="Self-hosted">
</p>

---

Archivr is a self-hosted tool for capturing and preserving digital content — YouTube videos and playlists, tweets and threads, Instagram, TikTok, web pages, and local files — into self-contained, locally-owned archives. Content is stored in SQLite with SHA3-256 blob deduplication, hierarchical tags, a browser-based UI, and role-based auth.

## Table of Contents

- [Features](#features)
- [Quick Start](#quick-start)
  - [With Nix](#with-nix)
  - [With Docker](#with-docker-1)
- [Architecture](#architecture)
- [Supported Inputs](#supported-inputs)
  - [YouTube playlists and channels](#youtube-playlists-and-channels)
  - [Video quality and audio-only](#video-quality-and-audio-only)
  - [Text notes](#text-notes)
- [Configuration](#configuration)
  - [TOML config file](#toml-config-file)
  - [Environment variables](#environment-variables)
    - [LLM providers](#llm-providers)
- [Keeping yt-dlp fresh](#keeping-yt-dlp-fresh)
- [Deployment](#deployment)
  - [Security](#security)
  - [NixOS](#hosting-on-nixos)
  - [Docker](#hosting-with-docker)
- [Development](#development)
- [License](#license)

## Features

- **Social media** — YouTube (videos, shorts, playlists, channels with sync mode), X/Twitter (tweet and thread JSON + media downloads), Instagram, TikTok, Facebook, Reddit, Snapchat via yt-dlp
- **Web pages** — full self-contained HTML snapshots via SingleFile + Chromium; optional Freedium mirror for paywalled articles; reader mode
- **Local files** — import any file from disk by `file://` path
- **Deduplication** — SHA3-256 content-addressed blob store shared across all captures; identical files are stored once
- **Tags and search** — hierarchical tag tree, full-text search (including the latest completed summary and its generated JSON tags), filterable entry list
- **Multiple archives** — the server mounts any number of separate archives from a single TOML config
- **Role-based auth** — Guest / User / Admin / Owner roles; session cookies and API tokens; Argon2 passwords; the Owner can choose which roles (including custom ones) may reorder child entries
- **Quality selection** — choose video quality or audio-only per capture; a live metadata probe populates the selector before download
- **LLM summaries** — regenerable per-entry summary via the Anthropic HTTP API, an OpenAI-compatible HTTP API, a local `claude` CLI, or a local `codex` CLI; triggered manually from the entry rail, never automatically on capture; text-only by default, with an explicit `Include attached images` option
- **Text notes** — capture a plain-text or Markdown note with a title and no URL; the byte-preserving note is stored as a normal deduplicated blob and opens in the usual entry-rail preview
- **In-progress capture indicator** — running captures appear as a compact spinner row in the entries list until they finish, replacing the earlier grey skeleton block

## Quick Start

### With Nix

```sh
# Create an archive
nix run github:thegeneralist/archivr#archivr -- init ./my-archive --name "My Archive"

# Archive something
nix run github:thegeneralist/archivr#archivr -- archive https://www.youtube.com/watch?v=dQw4w9WgXcQ

# Start the web UI (reads ./archivr-server.toml)
nix run github:thegeneralist/archivr#archivr-server
```

Create `archivr-server.toml` next to where you run the command:

```toml
auth_db_path = "/absolute/path/to/archivr-auth.sqlite"

[[archives]]
id = "personal"
label = "Personal"
archive_path = "/absolute/path/to/my-archive/.archivr"
```

Then open `http://127.0.0.1:8080`. On the first visit you will be prompted to create the owner account.

### With Docker

```sh
mkdir config
cp docker/config.example.toml config/archivr-server.toml
# Edit config/archivr-server.toml

# Initialize the archive on the persistent volume (run once)
docker compose run --rm archivr archivr init \
  /data/archives/main /data/archives/main/.archivr/store \
  --name "Main Archive"

docker compose up -d
```

Open `http://localhost:8080`. See [Hosting with Docker](#hosting-with-docker) for volume layout and Twitter/X credential setup.

## Architecture

Two binaries:

| Binary | Purpose |
|---|---|
| `archivr` | CLI — create archives (`init`) and add content (`archive`) |
| `archivr-server` | Web server — browse and search one or more archives via browser UI |

Archive layout created by `archivr init`:

```
my-archive/
├── .archivr/          # metadata: name, store_path, archivr.sqlite
└── store/
    ├── raw/           # deduplicated blobs: raw/A/B/<sha3-256>.ext
    ├── raw_tweets/    # tweet and thread JSON
    ├── structured/    # structured metadata outputs
    └── temp/          # staging area during capture
```

A separate auth database (`archivr-auth.sqlite`, path set in TOML) holds users, sessions, API tokens, and role bits. It is independent of individual archives.

## Supported Inputs

`archivr archive <locator>` accepts URLs and platform shorthands:

| Platform | Input examples |
|---|---|
| Local file | `file:///absolute/path/to/file.pdf` |
| YouTube video / short | `https://youtube.com/watch?v=ID` · `yt:video/ID` · `yt:short/ID` |
| YouTube playlist | `https://youtube.com/playlist?list=ID` |
| YouTube channel | `https://youtube.com/@handle` |
| X/Twitter tweet (JSON) | `tweet:ID` · `x:tweet:ID` · `twitter:tweet:ID` |
| X/Twitter thread (JSON) | `x:thread:ID` · `twitter:thread:ID` |
| X/Twitter media download | `tweet:media:ID` |
| Instagram | Direct URL · `instagram:ID` |
| TikTok | Direct URL · `tiktok:ID` |
| Facebook | Direct URL · `facebook:ID` |
| Reddit | Direct URL · `reddit:ID` |
| Snapchat | Direct URL · `snapchat:ID` |
| Arbitrary URL / web page | Any `https://` URL |

### YouTube playlists and channels

Capturing a playlist or channel creates a **container entry** with each video archived as a child beneath it. Before downloading, the UI probes each video for available quality options — set quality per-video or apply one to the whole batch. Individual videos can be excluded with the remove button.

**Sync mode:** when re-archiving a playlist or channel, enable sync mode in the capture dialog to skip videos that are already in the archive. Only new videos are downloaded; the existing container is reused.

**Reordering:** if your role is allowed (by default Admin and Owner), expand a container on the main page and drag a video by its handle to change its position. On touch screens and phone-sized windows, where dragging isn't available, ↑/↓ buttons appear instead. Alt+↑/↓ on a selected row works everywhere. The order is saved to the archive. Videos added later by sync mode appear at the end. The Owner chooses which roles, including custom roles, may reorder under **Settings → Instance → Permissions**.

### Video quality and audio-only

When capturing a yt-dlp-backed source through the web UI, a metadata probe runs first and populates the quality selector with heights actually available in that video:

| `qualities` | `has_audio` | UI shows |
|---|---|---|
| `["1080p", "720p", …]` | `true` | Best / heights / Audio only |
| `["1080p", …]` | `false` | Best / heights |
| `[]` | `true` | Audio only (pre-selected) |
| `[]` | `false` | "No media detected" |
| probe fails (502) | — | picker hidden; capture still submittable |

The `POST /api/archives/:id/captures` endpoint accepts an optional `quality` field:

```json
{ "locator": "https://www.youtube.com/watch?v=...", "quality": "720p" }
{ "locator": "https://www.youtube.com/watch?v=...", "quality": "audio" }
```

`"audio"` selects the most efficient native audio track without re-encoding (Opus/WebM preferred, then AAC/M4A). Omitting `quality` or passing `"best"` downloads at the highest available quality.

### Text notes

Not every capture has a URL. **Add text** in the capture dialog takes a title and a body and turns them into a
self-contained entry — useful for a scrap of prose, a quote, or a note attached to the surrounding archive. The
body is stored verbatim; no network fetch happens.

Two body types are accepted: `text/markdown` (saved as `.md`) and `text/plain` (saved as `.txt`). Anything else is
rejected. The body lands in `store/raw/…` under its SHA3-256 content hash, exactly like every other capture, so an
identical note captured twice is stored once.

Text notes have no synthetic source URL: the original-URL field stays empty rather than inventing a `text:` locator.

## Configuration

### TOML config file

```toml
# Optional. Default: 127.0.0.1:8080
bind = "127.0.0.1:8080"

# Required. Persists across upgrades; must be on a writable path.
auth_db_path = "/var/lib/archivr/archivr-auth.sqlite"

[[archives]]
id = "personal"
label = "Personal"
archive_path = "/srv/archivr/personal/.archivr"

[[archives]]
id = "work"
label = "Work"
archive_path = "/srv/archivr/work/.archivr"
```

See `docker/config.example.toml` for a complete annotated example.

### Environment variables

| Variable | Default | Description |
|---|---|---|
| `ARCHIVR_BIND` | `127.0.0.1:8080` | Bind address; overrides `bind` in TOML |
| `ARCHIVR_STATIC_DIR` | `crates/archivr-server/static` | Pre-built frontend asset directory |
| `ARCHIVR_YT_DLP` | `yt-dlp` | yt-dlp binary used for video and social downloads; the Nix wrappers point this at the pinned release |
| `ARCHIVR_YT_DLP_FORCE` | — | Absolute path to a yt-dlp binary that MUST be used, bypassing the resolver. Prefer `ARCHIVR_YT_DLP` unless you are overriding for a specific run |
| `ARCHIVR_SINGLE_FILE` | `single-file` | single-file-cli binary for web page archiving |
| `ARCHIVR_CHROME` | `chromium` | Chromium executable passed to single-file |
| `ARCHIVR_CHROME_ARGS` | — | Extra space-separated Chromium flags (Docker sets `--no-sandbox`) |
| `ARCHIVR_TWITTER_CREDENTIALS_FILE` | — | Cookies file for tweet/thread scraping — required for `tweet:ID` and `x:thread:ID` inputs |
| `ARCHIVR_TWEET_SCRAPER` | `vendor/twitter/scrape_user_tweet_contents.py` | Tweet scraper script path |
| `ARCHIVR_TWEET_PYTHON` | `python3` | Python executable for the tweet scraper |

The Nix wrapper and Docker image set `ARCHIVR_STATIC_DIR`, `ARCHIVR_SINGLE_FILE`, and `ARCHIVR_CHROME` automatically.

#### LLM providers

Summaries are manual and provider-agnostic. Only the variables for the provider you actually select are read; the two
HTTP providers refuse to start without their API key. They are text-only by default. Selecting `Include attached images`
explicitly sends eligible archived image data to the chosen provider; it is never attached automatically.

| Provider | Attached images |
|---|---|
| Anthropic HTTP | Supported |
| OpenAI-compatible HTTP | Supported |
| Codex CLI | Supported |
| Claude CLI | Not supported |

Image inclusion considers only `media` artifacts with `jpg`, `jpeg`, `png`, `webp`, `gif`, or `avif` files. At most four
images are sent, each no larger than 5 MiB and no more than 12 MiB in total.

Free-text entry search also matches the latest completed summary text and its generated JSON tags. Entries with no
summary, or only a pending or failed summary, get no summary-derived match.

Each request is cached under the provider and the **requested** model identifier. If a provider reports a more precise
resolved model (for example, an alias's concrete version), the UI displays that resolved name as attribution without
changing the cache identity.

Summary attempts move from `pending` to `running` and then to `completed` or `failed`. On server startup, interrupted
pending or running attempts are marked failed. Regenerating does not replace an earlier completed summary until the
replacement succeeds, and public readers receive completed content only—never pending state or diagnostic errors.

| Variable | Default | Description |
|---|---|---|
| `ARCHIVR_ANTHROPIC_API_KEY` | *(required for `anthropic_http`)* | API key for the Anthropic Messages API |
| `ARCHIVR_ANTHROPIC_URL` | `https://api.anthropic.com/v1/messages` | Endpoint override, e.g. an internal proxy |
| `ARCHIVR_ANTHROPIC_MODEL` | `claude-3-5-sonnet-latest` | Model id used for Anthropic summaries |
| `ARCHIVR_OPENAI_API_KEY` | *(required for `openai_compatible`)* | API key for any OpenAI-compatible endpoint |
| `ARCHIVR_OPENAI_URL` | `https://api.openai.com/v1/chat/completions` | Endpoint override; point this at a local server to run offline |
| `ARCHIVR_OPENAI_MODEL` | `gpt-4o-mini` | Model id used for OpenAI-compatible summaries |
| `ARCHIVR_CLAUDE_CLI` | *(auto-discovered)* | Path to a local `claude` binary |
| `ARCHIVR_CLAUDE_MODEL` | *(the CLI's own default)* | Optional model override for the local Claude CLI |
| `ARCHIVR_CODEX_CLI` | *(auto-discovered)* | Path to a local `codex` binary |
| `ARCHIVR_CODEX_MODEL` | *(the CLI's own default)* | Optional model override for the local Codex CLI |
| `ARCHIVR_SUMMARY_HTTP_TIMEOUT` | `120` | Seconds before an HTTP-provider summary is killed |
| `ARCHIVR_SUMMARY_CLI_TIMEOUT` | `300` | Seconds before a CLI-provider summary is killed |

When `ARCHIVR_CLAUDE_CLI` / `ARCHIVR_CODEX_CLI` is unset the binary is auto-discovered, in this order: the well-known
absolute paths, then `$HOME/.local/bin/<name>`, then the bare name resolved through `PATH`. Note that `PATH` is
consulted **last** — if a stale binary sits at one of the well-known paths it wins over a newer one on `PATH`, so set
the variable explicitly when you have both. The well-known paths are `/opt/homebrew/bin/claude` and
`/usr/local/bin/claude` for Claude, and `/Applications/ChatGPT.app/Contents/Resources/codex`,
`/opt/homebrew/bin/codex`, and `/usr/local/bin/codex` for Codex.

## Keeping yt-dlp fresh

yt-dlp is the download engine behind every video and social capture. YouTube rotates its player-signature and API
surfaces on a days-to-weeks cadence, so a binary that worked last month starts returning HTTP 403 on downloads. Keeping
it current is ordinary maintenance, not an emergency.

**What ships.** `flake.nix` pins a specific yt-dlp release fetched straight from `github.com/yt-dlp/yt-dlp/releases`,
not from nixpkgs — that channel usually lags months behind. The `archivr-server` and `archivr` wrappers set
`ARCHIVR_YT_DLP` to that pinned binary.

**How the resolver picks.** At runtime archivr probes `--version` on each candidate — the pinned binary from
`ARCHIVR_YT_DLP` and any user-installed binary at `<state_dir>/yt-dlp/yt-dlp` — and runs the newest. An exact version
tie resolves in favour of your own install. Setting `ARCHIVR_YT_DLP_FORCE=/path/to/yt-dlp` bypasses the comparison
entirely. The state dir is `~/Library/Application Support/archivr` on macOS, and `$XDG_STATE_HOME/archivr` (default
`~/.local/state/archivr`) elsewhere.

There are three ways to get a fresh version, cheapest first.

**1. Self-update — no rebuild required.**

```sh
archivr yt-dlp status                        # every candidate, its version, and which one wins
archivr yt-dlp update                        # download the latest zipapp into the state dir
archivr yt-dlp update --version 2026.09.15   # pin a specific release tag
```

When `ARCHIVR_YT_DLP_FORCE` applies, `status` shows that forced candidate and selects it as the winner.

The released artifact is a Python zipapp, so this path needs `python3` on `PATH` at run time.

**2. Automatic weekly bump.** `.github/workflows/update-ytdlp.yml` runs every Monday at 06:00 UTC, queries GitHub for
the latest release, and opens a PR bumping `version` and `hash` in `flake.nix` via `peter-evans/create-pull-request`.
It also accepts `workflow_dispatch` for an on-demand run.

**3. Manual bump**, when you need it now and do not want to wait for the weekly:

```sh
NEW=$(curl -s https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest | jq -r .tag_name)
HASH=$(nix hash file --sri --type sha256 <(curl -sL "https://github.com/yt-dlp/yt-dlp/releases/download/${NEW}/yt-dlp"))

# In flake.nix, inside the `ytDlp = pkgs.stdenv.mkDerivation { … }` block:
#   version = "OLD";                   → version = "$NEW";
#   url  = ".../download/OLD/yt-dlp";  → .../download/$NEW/yt-dlp
#   hash = "sha256-OLD…";              → hash = "$HASH";

nix build .#archivr-server
./result/bin/archivr yt-dlp status   # the env row should report the new version
git commit -am "chore(nix): yt-dlp OLD → $NEW"
```

**4. Docker:** yt-dlp is pinned to a specific version in the `Dockerfile` (`pip install "yt-dlp==<version>"`), matching the Nix pin. To update, bump the version string in the `Dockerfile` venv install step to match the new Nix version, then rebuild:

```sh
docker build -t archivr-server .
docker compose up -d
```

There is no in-container self-update path — `archivr yt-dlp update` writes to a host state directory that does not survive container restarts. Rebuild the image when captures start returning HTTP 403.

## Deployment

### Security

`archivr-server` binds to `127.0.0.1:8080` by default. Do not expose it to a public network without understanding the risks. When started on a non-loopback address the server logs a warning to stderr.

### Hosting on NixOS

The flake exposes `nixosModules.default`:

```nix
# flake.nix (your system flake)
{
  inputs.archivr.url = "github:thegeneralist/archivr";

  outputs = { nixpkgs, archivr, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      modules = [
        archivr.nixosModules.default
        {
          services.archivr-server = {
            enable = true;
            # listenAddress defaults to "127.0.0.1"
            # port defaults to 8080
            archives = [
              { id = "personal"; label = "Personal"; path = "/srv/archivr/personal/.archivr"; }
              { id = "work";     label = "Work";     path = "/srv/archivr/work/.archivr"; }
            ];
          };
        }
      ];
    };
  };
}
```

The module creates an `archivr` system user and group, generates the TOML config from your options, stores the auth database at `/var/lib/archivr-server/` (persists across upgrades), and runs under a hardened systemd unit (`ProtectSystem = strict`, `NoNewPrivileges`, `PrivateTmp`). Archive directories are whitelisted for read-write access.

Set `openFirewall = true` with a non-loopback `listenAddress` only when LAN or remote access is required.

Archive directories must be owned by the `archivr` user. Initialise them with `archivr init` first, then `chown -R archivr:archivr /srv/archivr`.

### Hosting with Docker

```sh
# 1. Configure
mkdir config
cp docker/config.example.toml config/archivr-server.toml
# Edit archivr-server.toml — set id, label, archive_path, and auth_db_path

# 2. Initialize each archive (run once per archive)
docker compose run --rm archivr archivr init \
  /data/archives/main /data/archives/main/.archivr/store \
  --name "Main Archive"

# 3. Start
docker compose up -d
```

| Mount | Purpose |
|---|---|
| `./config` (read-only) | Directory containing `archivr-server.toml` |
| `archivr-data` named volume | Auth database (`/data/archivr-auth.sqlite`) and archive directories |

> **Important:** `auth_db_path` must point to a path on the writable data volume (e.g. `/data/archivr-auth.sqlite`). The example config sets this correctly. A bare `mkdir` is not enough to initialise an archive — `archivr init` writes metadata files the server requires.

**Twitter/X archiving:** supply a cookies file inside the config volume and reference it in `docker-compose.yml`:

```yaml
environment:
  ARCHIVR_TWITTER_CREDENTIALS_FILE: /config/twitter-cookies.txt
```

**Building locally:**

```sh
docker build -t archivr-server .
```

The image compiles the Rust binary in a separate build stage; only runtime dependencies (Chromium, Node.js, Python) land in the final layer.

## Development

Runtime dependencies beyond Rust and Node: `yt-dlp`, Chromium, `single-file` (Node), Python 3 with `twitter-api-client`, `ffmpeg`. `nix develop` provides the dev subset.

Entry summaries are served by one of four interchangeable providers — `anthropic_http`, `openai_compatible`,
`claude_cli`, or `codex_cli` — each configured entirely through the environment; see
[LLM providers](#llm-providers) for the full variable list. The `archivr` CLI itself exposes `archive`, `init`, and
`yt-dlp status` / `yt-dlp update`; summaries are triggered from the web UI rather than the command line.

```sh
# Rust (workspace root)
cargo build
cargo test
cargo test -p archivr-core
cargo run -p archivr-server -- ./archivr-server.toml

# Frontend (from frontend/)
bun install
bun run dev        # Vite dev server
bun run build      # → crates/archivr-server/static/

# Nix
nix develop        # dev shell
nix build .#archivr-server
```

## License

MIT — see [LICENSE](../LICENSE.md).
\n
