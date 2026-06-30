# archivr

An open-source self-hosted archiving tool. Work in progress.

## Milestones

- [ ] Archiving
  - [x] Archiving media files from social media platforms
    - [x] YouTube Videos
    - [x] Twitter Videos
    - [x] Instagram
    - [x] Facebook
    - [x] TikTok
    - [x] Reddit
    - [x] Snapchat
    - [ ] YouTube Posts (postponed)
  - [x] Archiving local files
  - [x] Archiving Twitter Tweets, Threads, and Articles
  - [ ] Archiving files from cloud storage services (Google Drive, Dropbox, OneDrive) and from URLs
    - [x] URLs
    - [ ] Google Drive
    - [ ] Dropbox
    - [ ] OneDrive
    - (Some of these could be postponed for later.)
  - [x] Archive web pages (HTML, CSS, JS, images)
  - [ ] Archiving emails (???)
    - [ ] Gmail
    - [ ] Outlook
    - [ ] Yahoo Mail
- [ ] Management
  - [ ] Deduplication
  - [ ] Tagging system
  - [ ] Search functionality
  - [ ] Categorization
  - [ ] Metadata extraction and storage
- [ ] User Interface
  - [ ] Web-based UI
- [ ] Backup and Sync
  - [ ] Cloud backup (AWS S3, Google Cloud Storage)
  - [ ] Local backup

## Motivation

There are two driving factors behind this project:

- In the age of information, all data is ephemeral. Social media platforms frequently delete content, and cloud storage services can become inaccessible and unreliable. Being able to archive important data is _very important_ for preserving personal memories and digital history.
- I will be creating a small encyclopedia for my future family and kids. Therefore, I want to make sure that all the information I gather is preserved and accessible for future reference.

This project aims to provide a reliable solution for archiving important data from various sources, ensuring that users can preserve their digital assets for the long term.

## Archive Inputs

`archivr archive <path>` currently accepts three kinds of inputs:

- Local files via `file://...`
- Direct platform URLs
- Platform shorthand inputs such as `tweet:...`, `yt:...`, or `instagram:...`

## Running Archivr

Archivr currently ships as two binaries:

- `archivr`
  - The CLI for creating and writing to one archive.
  - Use this for `init` and `archive`.
- `archivr-server`
  - The web server for reading one or more existing archives through the browser UI.
  - Use this after archives already exist.

With Nix, run the CLI with:

```sh
nix run .#archivr -- init ./my-archive --name "My Archive"
nix run .#archivr -- archive file:///absolute/path/to/file.pdf
```

Run the web server with:

```sh
nix run .#archivr-server -- ./archivr-server.toml
```

The server expects a TOML registry file. If no path is passed, it reads `./archivr-server.toml`.

Example:

```toml
[[archives]]
id = "personal"
label = "Personal"
archive_path = "/absolute/path/to/my-archive/.archivr"
```

Then open:

```text
http://127.0.0.1:8080
```

When installed through Nix, `archivr-server` is wrapped so it can find the static web UI assets automatically. The wrapper sets `ARCHIVR_STATIC_DIR` to the installed static asset directory. Running from source with `cargo run -p archivr-server` falls back to `crates/archivr-server/static`.

### Security and Deployment

`archivr-server` is a **local-only tool by default**. It binds to `127.0.0.1:8080` and has no authentication or access control. Do not expose it to a public network or a shared LAN without understanding the risks.

**Changing the bind address**

You can set the bind address in your TOML config:

```toml
# Optional. Default: 127.0.0.1:8080
# Only change this if you know what you are doing — the server has no authentication.
bind = "127.0.0.1:9090"
```

Or override it with the `ARCHIVR_BIND` environment variable:

```sh
ARCHIVR_BIND=127.0.0.1:9090 nix run .#archivr-server -- ./archivr-server.toml
```

If the server is started with a non-loopback address (e.g. `0.0.0.0`), it prints a warning to stderr:

```text
warn: archivr-server is bound to 0.0.0.0:8080 — this server has no authentication. Only expose it on a trusted network.
```

**When will auth be added?**

Auth and session handling will be designed when remote or public hosting becomes a real requirement. Until then, keep the server on loopback. See `crates/archivr-server/src/routes.rs` for the route classification that will guide where middleware is applied.

### Supported Platforms

- Local files: `file:///absolute/path/to/file.ext`
- YouTube media: standard video/short URLs, plus [shorthand video inputs](#supported-shorthand-inputs)
- X/Twitter media from Tweets: normal Tweet URLs or the `tweet:media:ID` shorthand
- X/Twitter Tweet content scrape: [Tweet and Thread shorthands](#supported-shorthand-inputs). (These are saved as JSON files in `raw_tweets/`)
- Instagram, Facebook, TikTok, Reddit, Snapchat: direct URLs or platform-prefixed shorthand passed through to `yt-dlp`

### Hosting on NixOS

The flake exposes a `nixosModules.default` output. Add it to your system flake and
enable the service:

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
            # listenAddress defaults to "127.0.0.1" (loopback only)
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

The module:
- Creates an `archivr` system user and group.
- Generates the TOML config from your options and stores the auth database under
  `/var/lib/archivr-server/` (persists across upgrades).
- Runs under a hardened systemd unit (`ProtectSystem = strict`, `NoNewPrivileges`,
  `PrivateTmp`, etc.). Archive directories are whitelisted for read-write access.
- Restarts automatically on failure.

**`openFirewall`** — set to `true` to open the TCP port derived from `bind`.
Only needed when binding to a non-loopback address:

```nix
services.archivr-server = {
  listenAddress = "0.0.0.0";
  port = 8080;            # explicit, though 8080 is the default
  openFirewall = true;
};
```

**Archive directories** must be readable and writable by the `archivr` user.
Initialise them with `archivr init` first, then `chown -R archivr:archivr /srv/archivr`.


### Hosting with Docker

A `Dockerfile` and `docker-compose.yml` are provided for self-hosting without Nix.

**Quickstart**

1. Copy the example config and edit it:

   ```sh
   mkdir config
   cp docker/config.example.toml config/archivr-server.toml
   # edit config/archivr-server.toml — set archive id, label, and archive_path
   ```

2. Initialize each archive on the persistent data volume before the first start.
   The image includes the `archivr` CLI for this purpose:

   ```sh
   docker compose run --rm archivr archivr init /data/archives/main --name "Main Archive"
   ```

   This creates `/data/archives/main/.archivr/` with the metadata the server requires.
   A bare `mkdir` is not enough — the server reads `name` and `store_path` files that
   only `archivr init` writes.

3. Start the server:

   ```sh
   docker compose up -d
   ```

   Then open `http://localhost:8080`.

**Volumes**

| Mount | Purpose |
|-------|---------|
| `./config` (read-only) | Directory containing `archivr-server.toml` |
| `archivr-data` named volume | Auth database (`/data/archivr-auth.sqlite`) and archive directories |

> **Important:** `auth_db_path` must be set explicitly in `archivr-server.toml` to a
> path on the writable data volume (e.g. `/data/archivr-auth.sqlite`). If left unset,
> the server defaults to writing the auth database next to the config file — which is
> on the read-only `/config` mount and will fail. The example config sets this correctly.

**Twitter/X archiving**

Supply a cookies file inside the config volume and set `ARCHIVR_TWITTER_CREDENTIALS_FILE` in `docker-compose.yml`:

```yaml
environment:
  ARCHIVR_TWITTER_CREDENTIALS_FILE: /config/twitter-cookies.txt
```

**Building the image locally**

```sh
docker build -t archivr-server .
```

The image compiles the Rust binary in a separate build stage so only the runtime
dependencies (Chromium, Node.js, Python) land in the final layer.

### Supported Shorthand Inputs

- YouTube video/short media:
  - `yt:video/ID`
  - `youtube:video/ID`
  - `yt:short/ID`
  - `yt:shorts/ID`
  - `youtube:shorts/ID`
- X/Twitter tweet JSON content:
  - `tweet:ID`
  - `x:tweet:ID`
  - `x:x:ID`
  - `twitter:x:ID`
  - `twitter:tweet:ID`
- X/Twitter media/video download:
  - `tweet:media:ID`
- X/Twitter thread JSON content:
  - `x:thread:ID`
  - `twitter:thread:ID`
- Other platform shorthands:
  - `instagram:ID`
  - `facebook:ID`
  - `tiktok:ID`
  - `reddit:ID`
  - `snapchat:ID`

### Environment Variables

- `ARCHIVR_BIND`
  - Optional.
  - Overrides the bind address from the TOML config. Useful in Docker where you need
    `0.0.0.0:8080` without editing the config file. Default: `127.0.0.1:8080`.
- `ARCHIVR_STATIC_DIR`
  - Optional.
  - Path to the directory of pre-built frontend assets served by the web UI.
    Set automatically by the Nix wrapper and the Docker image. When running from
    source with `cargo run`, falls back to `crates/archivr-server/static`.
- `ARCHIVR_YT_DLP`
  - Optional.
  - Overrides the `yt-dlp` binary used for YouTube, X media posts, Instagram, Facebook, TikTok, Reddit, and Snapchat downloads.
- `ARCHIVR_SINGLE_FILE`
  - Optional.
  - Overrides the `single-file` binary used for web page archiving. Set automatically by the Nix wrapper and the Docker image.
- `ARCHIVR_CHROME`
  - Optional.
  - Overrides the Chromium/Chrome executable passed to `single-file` via `--browser-executable-path`. Set automatically by the Nix wrapper and the Docker image. Default: `chromium`.
- `ARCHIVR_TWITTER_CREDENTIALS_FILE`
  - Required for tweet/thread scraping inputs such as `tweet:ID` and `x:thread:ID`.
  - Must point to a cookies file for the vendored scraper.
- `ARCHIVR_TWEET_SCRAPER`
  - Optional.
  - Overrides the tweet scraper script path. Default: `vendor/twitter/scrape_user_tweet_contents.py`.
- `ARCHIVR_TWEET_PYTHON`
  - Optional.
  - Overrides the Python executable used to run the tweet scraper. Default: `python3`.

### Current Limitations

- Arbitrary `http://` or `https://` URLs that return HTML are archived as self-contained single-file HTML snapshots via `single-file-cli` (requires Chromium). Plain file URLs (PDFs, images, zips, etc.) are downloaded directly. Requires `single-file` and a Chromium binary on PATH, or the `ARCHIVR_SINGLE_FILE` / `ARCHIVR_CHROME` env vars set.
- Local files currently need to be passed as `file://...` paths.

## License

This project is licensed under the MIT License. See the [LICENSE](LICENSE.md) file for details.
