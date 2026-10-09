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
  - [YouTube subtitles](#youtube-subtitles)
  - [Text notes](#text-notes)
- [Configuration](#configuration)
  - [TOML config file](#toml-config-file)
  - [Environment variables](#environment-variables)
    - [LLM providers](#llm-providers)
- [Keeping yt-dlp and its JS runtime fresh](#keeping-yt-dlp-and-its-js-runtime-fresh)
  - [JavaScript runtime (Deno)](#javascript-runtime-deno)
  - [Troubleshooting](#troubleshooting)
- [Deployment](#deployment)
  - [Security](#security)
  - [NixOS](#hosting-on-nixos)
  - [Docker](#hosting-with-docker)
- [Development](#development)
- [License](#license)

## Features

- **Social media** — YouTube (videos, shorts, playlists, channels with sync mode; subtitles saved with each video by default), X/Twitter (tweet and thread JSON + media downloads), Instagram, TikTok, Facebook, Reddit, Snapchat via yt-dlp
- **Web pages** — full self-contained HTML snapshots via SingleFile + Chromium; optional Freedium mirror for paywalled articles; reader mode
- **Local files** — import any file from disk by `file://` path
- **Deduplication** — SHA3-256 content-addressed blob store shared across all captures; identical files are stored once
- **Tags and search** — hierarchical tag tree, full-text search (including the latest completed summary and its generated JSON tags), filterable entry list
- **Multiple archives** — the server mounts any number of separate archives from a single TOML config
- **Role-based auth** — Guest / User / Admin / Owner roles; session cookies and API tokens; Argon2 passwords; the Owner can choose which roles (including custom ones) may reorder child entries
- **Quality selection** — choose video quality or audio-only per capture; a live metadata probe populates the selector before download
- **LLM summaries** — regenerable per-entry summary via the Anthropic HTTP API, an OpenAI-compatible HTTP API, a local `claude` CLI, or a local `codex` CLI; triggered manually from the entry rail, never automatically on capture; text-only by default, with an explicit `Include attached images` option; YouTube videos are summarized from their subtitles
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

X Articles are titled `<article title> — @handle` from the article's own title instead of the tweet text (usually a
bare `t.co` link). Article entries archived before this change are retitled on the next `archivr-server` start, but only
while their title still equals the old auto-generated bare-link title; a renamed entry is left alone. Titles have no
"edited" flag, so an entry you renamed back to exactly that old title is retitled too. CLI-only installs never run this
pass — it runs at server startup only.

X threads keep `Thread by @handle` at capture. **Generate title** in the entry rail (thread entries, user role and up)
asks the selected Summary provider's cheap title model for a short topic and saves `Thread about <topic> — @handle` as
the entry title (`POST /api/archives/:id/entries/:uid/thread-title`, body `{"provider": "<kind>"}`); rename it like any
title. With ≥2 entries selected and at least one X thread, the bulk panel's **Generate titles** does the same for each
selected thread (non-threads skipped; uses the Summary provider selected in the rail), two at a time, with progress and
an `X updated, Y failed` summary; changing the selection stops it picking up further entries. Models are listed under
[LLM providers](#llm-providers).

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

### YouTube subtitles

YouTube video captures save subtitles next to the video by default: single videos, shorts, and every video archived
from a YouTube playlist or channel, at any quality including audio-only. Subtitles apply to YouTube videos only —
YouTube Music, Spotify, X, TikTok, and the other yt-dlp sources are downloaded without them.

At most two tracks are saved, chosen from the metadata yt-dlp already fetches for the capture:

- English, plus the video's original language when that isn't English.
- Manual (uploader-provided) tracks are preferred. If the original language has no manual track, its auto-generated
  track is used; auto-generated English is used only when nothing else was found.
- If the metadata probe fails, yt-dlp is asked for `en` and any `-orig` (original-language) track instead.

Tracks are requested as VTT, with SRT accepted; nothing is converted, and other subtitle formats are dropped. Each file
goes through the usual SHA3-256 dedup into `store/raw/` and is recorded as a `subtitle` artifact of the entry, along
with its language, whether it was manual or auto-generated, and its format.

Subtitle failures never fail a capture. Subtitles are requested in the same yt-dlp call as the media with
`--ignore-errors`, so a missing track or a rate-limited caption request only logs a warning. If that call still fails,
the media is retried once without subtitles.

Subtitles are on unless you turn them off for a capture:

- **Web UI:** the **Download subtitles** toggle in the capture dialog.
- **API:** `"download_subtitles": false` in the `POST /api/archives/:id/captures` body. Omitting the field means `true`.
- **CLI:** `archivr archive --no-subtitles <url>`.

Videos captured without subtitles can still be summarized; see [LLM providers](#llm-providers) and
[Local transcription](#local-transcription-optional).

#### Local transcription (optional)

When a YouTube video has no subtitles at summary time, Archivr can transcribe its audio on the server. The order is
fixed and transcription never runs if an earlier step yields usable subtitles:

1. archived subtitles;
2. subtitles fetched from the original video (subtitles only, no media);
3. local transcription with the engine chosen in the Summary panel;
4. the no-subtitles error (or a transcription-specific error if step 3 ran and failed).

The feature is off until `ARCHIVR_TRANSCRIBE_ENGINES` lists at least one configured engine (env vars in
[Local transcription env](#local-transcription)). Then the Summary panel shows a second selector on YouTube videos —
**No local transcription** (default) or an enabled engine — remembered for the browser session. The API field is
`"transcribe_engine": "<kind>"` in the summary POST body; an unknown or unconfigured engine is a 400. Enabled engines
are listed by `GET /api/summary/transcription-engines`.

| Engine (`kind`) | Languages | Runs on | Install |
|---|---|---|---|
| Whisper (`whisper`) | ~99 (`*.en` models English only) | CPU, Metal, CUDA/Vulkan | whisper.cpp `whisper-cli` + a ggml model (default backend), or a faster-whisper wrapper (`script` backend) |
| NVIDIA Parakeet (`parakeet`) | v2 English; v3 25 European | NVIDIA GPU (NeMo), Apple silicon (parakeet-mlx), CPU (ONNX) | your own wrapper script; weights CC-BY-4.0 |
| Fermion Phonon-2 (`phonon2`) | **English only** (hard-coded) | Apple silicon (MLX), x86-64/Arm CPU, CUDA | `pip install fermion-research` + platform runtime; weights CC-BY-4.0, CLI licence unknown |

Setup examples:

```sh
# whisper.cpp
nix shell nixpkgs#whisper-cpp
curl -LO https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin
export ARCHIVR_TRANSCRIBE_ENGINES=whisper ARCHIVR_WHISPER_MODEL=$PWD/ggml-large-v3-turbo.bin

# Parakeet via parakeet-mlx (Apple silicon), wrapper from spec Appendix A.3
export ARCHIVR_TRANSCRIBE_ENGINES=parakeet ARCHIVR_PARAKEET_CLI=/opt/transcribe/parakeet.py

# Phonon-2 (vendor package; Apple silicon shown)
python3 -m venv /opt/transcribe && /opt/transcribe/bin/pip install fermion-research mlx mlx-audio mlx-lm soundfile scipy zstandard
export ARCHIVR_TRANSCRIBE_ENGINES=phonon2 ARCHIVR_PHONON2_CLI=/opt/transcribe/bin/fermion
```

**Script contract** (Whisper `script` backend and Parakeet). Archivr runs
`<script> --input <job>/audio.wav --output <job>/transcript.vtt --model <model> [--language <xx>]`. The script exits 0
only after writing WebVTT to `--output`, may write a detected language code to `<output>.lang`, treats `--language` as
a hint, writes nothing outside the output directory except model caches, and must tolerate SIGKILL on timeout. The
audio is already 16 kHz mono PCM WAV. Reference wrappers are in the spec's Appendix A.

Notes:

- One transcription job runs at a time per server; others wait inside their own timeout budget.
- Audio comes from the archived media or a yt-dlp audio download, converted by ffmpeg to a temp WAV (~115 MB per hour
  of audio) that is deleted afterwards.
- `ARCHIVR_TRANSCRIBE_TIMEOUT` (default 3600 s) bounds the whole job: audio, ffmpeg and the engine.
- The transcript is stored as a `subtitle` artifact with metadata `kind: "transcribed"`, `origin: "transcription"`,
  `engine` and `model` (a model path is reduced to its file name). It ranks below manual subtitles and above
  auto-generated ones, and later summaries reuse it without transcribing again.
- Python engines download weights on first use, so the server user needs a writable `HOME`/cache directory.

Design and deviations: [`superpowers/specs/2026-10-05-local-transcription-fallback.md`](superpowers/specs/2026-10-05-local-transcription-fallback.md).

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
| `ARCHIVR_DENO` | — | Pinned Deno binary offered to the JS runtime resolver; the Nix wrappers and Docker image set it |
| `ARCHIVR_JS_RUNTIME` | — | `RUNTIME[:ABS_PATH]` with `RUNTIME` one of `deno`, `node`, `bun`, `quickjs`. Forces the runtime passed to yt-dlp, bypassing resolution and version checks. Invalid values are warned about and ignored. Node needs ≥ 22; Bun needs ≥ 1.2.11 and is deprecated in yt-dlp |
| `ARCHIVR_STATE_DIR` | platform state dir | Where `archivr yt-dlp update` and Settings › Instance › yt-dlp install yt-dlp and Deno. Docker sets `/data/archivr-state` |
| `ARCHIVR_SINGLE_FILE` | `single-file` | single-file-cli binary for web page archiving |
| `ARCHIVR_CHROME` | `chromium` | Chromium executable passed to single-file |
| `ARCHIVR_CHROME_ARGS` | — | Extra space-separated Chromium flags (Docker sets `--no-sandbox`) |
| `ARCHIVR_TWITTER_CREDENTIALS_FILE` | — | Cookies file for tweet/thread scraping — required for `tweet:ID` and `x:thread:ID` inputs |
| `ARCHIVR_TWEET_SCRAPER` | `vendor/twitter/scrape_user_tweet_contents.py` | Tweet scraper script path |
| `ARCHIVR_TWEET_PYTHON` | `python3` | Python executable for the tweet scraper |

The Nix wrapper and Docker image set `ARCHIVR_STATIC_DIR`, `ARCHIVR_SINGLE_FILE`, `ARCHIVR_CHROME`, `ARCHIVR_DENO`, and
`ARCHIVR_FFMPEG` automatically.

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

**YouTube videos** are summarized from one archived subtitle track, never from the video file. The track is picked in
this order: manual English, manual original language, other manual, auto-generated original language, auto-generated
English. It is reduced to plain text: timestamps, cue settings, and markup are removed, and the repeated lines of
rolling auto-captions are collapsed. Like every summary input, the transcript is cut at 48,000 characters, so the end of
a long video is not summarized. Adding or replacing subtitles changes the input hash, so a cached summary built from
different subtitles is not reused. Other video and audio entries still can't be summarized.

If the entry has no usable subtitles (for example, it was captured with subtitles turned off, or by an older Archivr),
requesting a summary first downloads them from the original URL — subtitles only, no media — while the attempt shows as
pending. Fetched tracks are archived to the entry like captured ones. If none can be fetched, the attempt fails without
calling the provider and the Summary panel shows:

> This video can’t be summarized because no subtitles are available. Archivr found no archived subtitles and couldn’t
> download any from the original video — it may have no captions, or it may be private, deleted, or unreachable.

If a transcription engine was selected, Archivr transcribes the audio before giving up; see
[Local transcription](#local-transcription-optional).

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
| `ARCHIVR_SUMMARY_CLI_TIMEOUT` | `300` | Seconds before a CLI-provider summary is killed; also bounds each summary-time yt-dlp subtitle fetch call |

Thread-title generation uses the same provider but never its summary model (`ARCHIVR_*_MODEL`); it uses a cheap title
model instead. Admins can set a per-provider title model in **Settings › Instance › Thread title models**. Precedence:
that instance setting (blank = unset) > the env var below > the built-in default.

| Variable | Default | Description |
|---|---|---|
| `ARCHIVR_ANTHROPIC_TITLE_MODEL` | `claude-haiku-4-5` | Model used only for thread-title generation |
| `ARCHIVR_OPENAI_TITLE_MODEL` | `gpt-4o-mini` | Model used only for thread-title generation |
| `ARCHIVR_CLAUDE_TITLE_MODEL` | `haiku` | Model used only for thread-title generation |
| `ARCHIVR_CODEX_TITLE_MODEL` | `gpt-6-luna` | Model used only for thread-title generation; set this if your Codex account lacks that model |

When `ARCHIVR_CLAUDE_CLI` / `ARCHIVR_CODEX_CLI` is unset the binary is auto-discovered, in this order: the well-known
absolute paths, then `$HOME/.local/bin/<name>`, then the bare name resolved through `PATH`. Note that `PATH` is
consulted **last** — if a stale binary sits at one of the well-known paths it wins over a newer one on `PATH`, so set
the variable explicitly when you have both. The well-known paths are `/opt/homebrew/bin/claude` and
`/usr/local/bin/claude` for Claude, and `/Applications/ChatGPT.app/Contents/Resources/codex`,
`/opt/homebrew/bin/codex`, and `/usr/local/bin/codex` for Codex.

#### Local transcription

Only read when a summary request names an engine. See [Local transcription](#local-transcription-optional).

| Variable | Default | Description |
|---|---|---|
| `ARCHIVR_TRANSCRIBE_ENGINES` | *(unset: feature off)* | Comma-separated enabled engines: `whisper`, `parakeet`, `phonon2`. Unknown names are warned about and ignored |
| `ARCHIVR_WHISPER_BACKEND` | `whisper_cpp` | `whisper_cpp` or `script` |
| `ARCHIVR_WHISPER_CLI` | auto-discovered `whisper-cli` | whisper.cpp binary; **required** with the `script` backend (the wrapper script) |
| `ARCHIVR_WHISPER_MODEL` | *(required for `whisper`)* | whisper.cpp: ggml model file; script: passed as `--model` |
| `ARCHIVR_WHISPER_LANGUAGES` | *(any)* | Optional allowlist of base language codes, e.g. `en` for a `*.en` model |
| `ARCHIVR_PARAKEET_CLI` | *(required for `parakeet`)* | Wrapper script following the script contract |
| `ARCHIVR_PARAKEET_MODEL` | `nvidia/parakeet-tdt-0.6b-v3` | Passed as `--model` |
| `ARCHIVR_PARAKEET_LANGUAGES` | *(any)* | Optional allowlist; use `en` for v2 |
| `ARCHIVR_PHONON2_CLI` | auto-discovered `fermion` | The `fermion` CLI |
| `ARCHIVR_PHONON2_MODEL` | `phonon-2` | Model passed to `fermion transcribe` |
| `ARCHIVR_TRANSCRIBE_TIMEOUT` | `3600` | Seconds for one whole transcription job |
| `ARCHIVR_FFMPEG` | `ffmpeg` | ffmpeg used to extract 16 kHz mono WAV; Nix wrappers and Docker set it |

`whisper-cli` and `fermion` are auto-discovered like the Claude/Codex CLIs: `/opt/homebrew/bin/<name>`,
`/usr/local/bin/<name>`, `$HOME/.local/bin/<name>`, then `PATH`.

## Keeping yt-dlp and its JS runtime fresh

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
`~/.local/state/archivr`) elsewhere; `ARCHIVR_STATE_DIR` overrides it.

There are three ways to get a fresh version, cheapest first. From the web UI, **Settings › Instance › yt-dlp** (admins
only) shows every yt-dlp and JS runtime candidate with its version and the winner, and **Update yt-dlp & Deno** runs
the same update as the CLI. The server picks up the new binaries immediately — no restart; captures already running keep the binary
they started with. Admin API: `GET /api/admin/yt-dlp` (status) and `POST /api/admin/yt-dlp/update` (no body; per
component outcome plus fresh status; 409 while an update is running).

**1. Self-update — no rebuild required.**

```sh
archivr yt-dlp status                        # every candidate, its version, and which one wins
archivr yt-dlp update                        # download the latest zipapp and the latest Deno into the state dir
archivr yt-dlp update --version 2026.09.15   # pin a specific release tag
```

When `ARCHIVR_YT_DLP_FORCE` applies, `status` shows that forced candidate and selects it as the winner.

`update` installs yt-dlp and Deno independently and reports each (`yt-dlp: …`, `deno: …`); it exits non-zero if
either failed. `--version` applies to yt-dlp only — Deno always tracks the latest release. The CLI runs in its own
process, so restart a running server after a CLI update; an update from the web UI needs no restart.

The released artifact is a Python zipapp, so this path needs Python ≥ 3.10 on `PATH` as `python3` at run time. The web
UI update runs the installed yt-dlp once and reports it as failed, with the error, if it does not start (e.g. macOS's
system Python 3.9); `status` shows a candidate that exists but does not run as `invalid: <last error line>`.

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

**4. Docker:** yt-dlp is pinned to a specific version in the `Dockerfile` (`pip install "yt-dlp[default]==<version>"`, which also pulls the `yt-dlp-ejs` challenge solver), matching the Nix pin. To update the baked-in copy, bump the version string in the `Dockerfile` venv install step to match the new Nix version, then rebuild:

```sh
docker build -t archivr-server .
docker compose up -d
```

Self-update also works in the container: the image sets `ARCHIVR_STATE_DIR=/data/archivr-state` on the persistent
`archivr-data` volume, so the update survives restarts. Use Settings › Instance › yt-dlp (no restart), or the CLI and
then restart so the server re-resolves:

```sh
docker compose exec archivr archivr yt-dlp update
docker compose restart archivr
```

Deno is pinned in the `Dockerfile` (2.9.7, sha256 per arch). Nothing bumps it automatically: change the version and
both sha256 values together. It adds roughly 80 MB to the image.

### JavaScript runtime (Deno)

YouTube now serves player challenges that yt-dlp solves with its EJS solver, which needs a JavaScript runtime —
Deno ≥ 2.3.0 by default. Without one, downloads fail with HTTP 403.

**How the resolver picks.** `ARCHIVR_JS_RUNTIME` wins outright when valid. Otherwise archivr probes `deno --version`
on the pinned `ARCHIVR_DENO` and on `<state_dir>/deno/deno`, skips anything below 2.3.0, and uses the newest (exact
ties go to the state-dir copy). If neither qualifies it falls back to `deno` on `PATH`; if that fails too, it prints a
one-time `warn: no JavaScript runtime for yt-dlp …` and runs yt-dlp without one. Only Deno is chosen automatically;
Node, Bun and QuickJS are used only when forced. Every yt-dlp call gets the result as `--js-runtimes deno:<path>`
(non-Deno runtimes also get `--no-js-runtimes` first so a stray Deno cannot outrank them).

`archivr yt-dlp status` prints a second table after the yt-dlp one. Columns are tab-separated; locations are absolute
(the state dir is `$XDG_STATE_HOME/archivr`, default `~/.local/state/archivr`, on Linux and
`~/Library/Application Support/archivr` on macOS). Exactly one row — the role the resolver picked — is starred, even
when two roles point at the same binary. On Linux after `archivr yt-dlp update`, with an older Deno on `PATH`:

```text
JS runtime (passed to yt-dlp as --js-runtimes)
role	path	version	chosen
force (ARCHIVR_JS_RUNTIME)	—	—	
env (ARCHIVR_DENO)	—	—	
state-dir	/home/alice/.local/state/archivr/deno/deno	2.9.7	*
path (deno)	/usr/bin/deno	2.4.0	
```

Under the Nix wrappers the pinned Deno is also on `PATH`, so two rows show the same binary but only the pin is chosen:

```text
JS runtime (passed to yt-dlp as --js-runtimes)
role	path	version	chosen
force (ARCHIVR_JS_RUNTIME)	—	—	
env (ARCHIVR_DENO)	/nix/store/…-deno-2.9.4/bin/deno	2.9.4	*
state-dir	—	—	
path (deno)	/nix/store/…-deno-2.9.4/bin/deno	2.9.4	
```

An invalid `ARCHIVR_JS_RUNTIME` shows as `invalid: <reason>` in the force row.

### Troubleshooting

**`WARNING: [youtube] No supported JavaScript runtime could be found…` followed by `HTTP Error 403: Forbidden`.**
yt-dlp ran without a JS runtime.

1. Run `archivr yt-dlp status` (or open Settings › Instance › yt-dlp) and check the JS runtime table has a chosen row.
2. If not, run `archivr yt-dlp update` or **Update yt-dlp & Deno** in the UI (installs Deno into the state dir), or set
   `ARCHIVR_JS_RUNTIME=node:/abs/node` (Node ≥ 22).
3. After a CLI update, restart `archivr-server`; a UI update re-resolves automatically. An env-var change always
   needs a restart.
4. Verify by hand: `yt-dlp -v --js-runtimes deno:<path> --simulate <url>` should print `JS runtimes: deno-…`.

**`no such option: --js-runtimes`.** A yt-dlp older than 2025.11 (only reachable through the bare `PATH` fallback)
does not know the flag. Run `archivr yt-dlp update`.

**Deno runs but the solver still fails.** yt-dlp treats any Deno stderr output as a solver error. Deno writes its
cache to `DENO_DIR` (default `$HOME/.cache/deno`); with a read-only `HOME`, point `DENO_DIR` at a writable directory.

**NixOS: `update` says the prebuilt Deno cannot execute.** Upstream Deno binaries are dynamically linked and need a
standard loader. Enable `programs.nix-ld`, or rely on the pinned `ARCHIVR_DENO` from the flake — `update` then skips
Deno and exits 0 (the UI shows the Deno outcome as `skipped: …`).

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

The wrappers ship nixpkgs' Deno as `ARCHIVR_DENO`, so YouTube works out of the box. `archivr yt-dlp update` downloads
the upstream (dynamically linked) Deno, which only runs on NixOS with `programs.nix-ld.enable = true`; without it,
`update` skips Deno, keeps the pinned one, and exits 0. Run `update` as the service user so it lands in
`/var/lib/archivr-server`, then restart the unit — or use Settings › Instance › yt-dlp, which runs as the service user
and needs no restart.

Extra environment (LLM providers, local transcription) goes through two options:

```nix
services.archivr-server = {
  environment = {
    ARCHIVR_TRANSCRIBE_ENGINES = "whisper";
    ARCHIVR_WHISPER_CLI = "${pkgs.whisper-cpp}/bin/whisper-cli";
    ARCHIVR_WHISPER_MODEL = "/var/lib/archivr-server/models/ggml-large-v3-turbo.bin";
  };
  environmentFile = "/run/secrets/archivr.env";   # KEY=value lines, e.g. API keys; kept out of the Nix store
};
```

`environment` is merged over defaults (`lib.mkDefault`) that put `HOME`, `XDG_CACHE_HOME` and `HF_HOME` under
`/var/lib/archivr-server`, so Python engines can cache weights. The unit sets no `PrivateDevices`/`DeviceAllow`;
adding them would break CUDA engines.

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

**Local transcription:** the image ships no engines (it sets `ARCHIVR_FFMPEG=/usr/bin/ffmpeg`). `docker-compose.yml`
has commented `ARCHIVR_TRANSCRIBE_ENGINES`/`ARCHIVR_WHISPER_*`/`ARCHIVR_PHONON2_CLI` examples and a read-only
`./models:/models:ro` mount. For Phonon-2 on CPU, build a derived image:

```dockerfile
FROM archivr:latest
RUN python3 -m venv /opt/transcribe && \
    /opt/transcribe/bin/pip install --no-deps torch --index-url https://download.pytorch.org/whl/cpu && \
    /opt/transcribe/bin/pip install fermion-research torch safetensors soundfile scipy zstandard
ENV ARCHIVR_TRANSCRIBE_ENGINES=phonon2 ARCHIVR_PHONON2_CLI=/opt/transcribe/bin/fermion
```

Mount a cache volume at the server user's `~/.cache` so downloaded weights survive container recreation. GPU
containers need the NVIDIA container toolkit and a CUDA base image.

**Building locally:**

```sh
docker build -t archivr-server .
```

The image compiles the Rust binary in a separate build stage; only runtime dependencies (Chromium, Node.js, Python) land in the final layer.

## Development

Runtime dependencies beyond Rust and Node: `yt-dlp`, Deno (≥ 2.3.0, for YouTube), Chromium, `single-file` (Node), Python 3 with `twitter-api-client`, `ffmpeg`. `nix develop` provides the dev subset.

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
bun run build      # → crates/archivr-server/static/ (gitignored; nix build does this automatically)

# Nix
nix develop        # dev shell
nix build .#archivr-server
```

## License

MIT — see [LICENSE](../LICENSE.md).
\n

## Chrome extension

The companion `archivr-extension` checkout builds an unpacked Chrome Manifest V3 extension (Chrome 123+). Keep it next to this checkout, rebuild the current Archivr server/frontend, then run `bun install` and `bun run zip` from the extension checkout. Load its `dist` directory through Chrome’s Developer mode.

Connect by entering your server origin (localhost or a Tailscale address are supported), then log in in the server tab. The extension creates a named, revocable API token using that login and lets you choose a mounted archive. Captures remain reviewable in the shared Capture dialog. Selected text has editable title/body and optional manual title generation. Playlists/channels open this app’s prefilled capture dialog.

Capture deep links use `/?archive=<id>&capture=<encoded-locator>`. They wait for login, require a valid mounted archive, open once, and never submit automatically. The `capture` parameter is removed after opening.

Capture users can read safe defaults and configured-provider availability through `GET /api/captures/options`. `POST /api/archives/:archive_id/captures/text/title` accepts `{body, provider}` and returns `{title}` without creating or modifying archive entries. Both endpoints require the capture role; administrator settings remain administrator-only.
