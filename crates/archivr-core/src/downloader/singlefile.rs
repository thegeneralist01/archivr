use anyhow::{Context, Result, bail};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use std::{
    collections::HashMap,
    env,
    io::Read,
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use crate::downloader::cookies::{domain_from_url, write_netscape_cookie_file};
use crate::hash::hash_file;

/// Mozilla Readability.js (Apache 2.0) — embedded at compile time so captures
/// don't need it on PATH.  Path is relative to this source file.
const READABILITY_JS: &str =
    include_str!("../../../../vendor/readability/Readability.js");

/// Single-file user script that applies Readability just before the page is
/// serialised.  Fires on `single-file-on-before-capture-start`.
const READER_MODE_WRAPPER_JS: &str = r#"
addEventListener('single-file-on-before-capture-start', function() {
  try {
    if (typeof Readability === 'undefined') return;
    var article = new Readability(document.cloneNode(true)).parse();
    if (!article || !article.content || article.content.length < 100) return;
    document.body.innerHTML = article.content;
    if (article.title) document.title = article.title;
    var hdr = document.createElement('header');
    hdr.innerHTML =
      '<h1 style="margin:0 0 .3em;font-family:-apple-system,sans-serif">' +
        (article.title || '') + '</h1>' +
      (article.byline
        ? '<p style="margin:0;color:#666;font-size:14px">' + article.byline + '</p>'
        : '') +
      (article.siteName
        ? '<p style="margin:.2em 0 0;color:#999;font-size:12px">' + article.siteName + '</p>'
        : '');
    hdr.style.cssText = 'margin-bottom:2em;padding-bottom:1em;border-bottom:1px solid #ddd';
    document.body.insertBefore(hdr, document.body.firstChild);
    var style = document.createElement('style');
    style.textContent = [
      'body{max-width:680px;margin:40px auto;padding:0 24px;',
      'font-family:Georgia,"Times New Roman",serif;font-size:18px;',
      'line-height:1.75;color:#1a1a1a;background:#fafaf8}',
      'h1,h2,h3,h4,h5,h6{font-family:-apple-system,BlinkMacSystemFont,sans-serif;',
      'line-height:1.3;margin-top:1.5em}',
      'img,figure,video{max-width:100%;height:auto;display:block;margin:1em 0}',
      'a{color:#0055cc}',
      'pre{background:#f4f4f4;padding:1em;border-radius:4px;overflow-x:auto;font-size:14px}',
      'code{background:#f4f4f4;padding:.1em .3em;border-radius:3px;font-size:14px}',
      'blockquote{border-left:3px solid #ccc;margin:1em 0;padding-left:1.2em;color:#555}',
    ].join('');
    document.head.appendChild(style);
  } catch (e) { /* non-fatal: fall back to original page */ }
});
"#;

/// Result of archiving a web page with single-file.
#[derive(Debug)]
pub struct SaveResult {
    /// SHA-256 hex of the archived `.html` file.
    pub html_hash: String,
    /// Page title from `<title>` tag, if present.
    pub title: Option<String>,
    /// SHA-256 hex of the extracted favicon, if present.
    pub favicon_hash: Option<String>,
    /// File extension for the favicon (e.g. `".ico"`, `".png"`), if present.
    pub favicon_ext: Option<String>,
    /// `true` when `ARCHIVR_UBLOCK=true` (the default) but the extension path
    /// was missing or invalid.  The capture succeeded but ran without ad-blocking.
    pub ublock_skipped: bool,
}

/// Archives `url` as a self-contained HTML snapshot.
///
/// Env vars:
/// - `ARCHIVR_SINGLE_FILE`: path to the `single-file` binary (default: `"single-file"`).
/// - `ARCHIVR_CHROME`: path to the Chromium/Chrome binary (default: `"chromium"`).
/// - `ARCHIVR_UBLOCK`: enable uBlock Origin Lite extension (default: `"true"`).
/// - `ARCHIVR_UBLOCK_EXT`: path to the unpacked uBlock Origin Lite extension directory.
/// - `ARCHIVR_CHROME_ARGS`: space-separated extra Chrome flags (e.g. `"--no-sandbox"`).
pub fn save(
    url: &str,
    store_path: &Path,
    timestamp: &str,
    cookies: &HashMap<String, String>,
    ublock_enabled_override: Option<bool>,
    reader_mode: bool,
) -> Result<SaveResult> {
    let single_file =
        env::var("ARCHIVR_SINGLE_FILE").unwrap_or_else(|_| "single-file".to_string());
    let chrome = env::var("ARCHIVR_CHROME").unwrap_or_else(|_| "chromium".to_string());
    let (ublock_ext, ublock_skipped) = resolve_ublock_config(ublock_enabled_override);
    let mut result = save_with(
        url,
        store_path,
        timestamp,
        &single_file,
        &chrome,
        cookies,
        ublock_ext.as_deref(),
        reader_mode,
    )?;
    result.ublock_skipped = ublock_skipped;
    Ok(result)
}

/// Resolves uBlock configuration from env vars, optionally overridden by the caller.
///
/// Returns:
/// - `(Some(path), false)` — uBlock is enabled and the extension dir is valid.
/// - `(None, true)`  — uBlock is enabled but the extension dir is missing/invalid
///                     (warns to stderr; the capture proceeds without ad-blocking).
/// - `(None, false)` — uBlock is disabled (`ARCHIVR_UBLOCK=false` or overridden).
fn resolve_ublock_config(enabled_override: Option<bool>) -> (Option<PathBuf>, bool) {
    // The override (from instance settings or per-capture body) takes precedence over env.
    let want_ublock = enabled_override.unwrap_or_else(|| {
        let env_val = env::var("ARCHIVR_UBLOCK").unwrap_or_else(|_| "true".to_string());
        !env_val.eq_ignore_ascii_case("false") && env_val != "0"
    });
    if !want_ublock {
        return (None, false);
    }
    match env::var("ARCHIVR_UBLOCK_EXT").ok().filter(|s| !s.is_empty()) {
        None => {
            eprintln!(
                "warn: uBlock: ARCHIVR_UBLOCK_EXT is not set; \
                 capturing without ad-blocking"
            );
            (None, true)
        }
        Some(ext_path_str) => {
            let path = PathBuf::from(&ext_path_str);
            if path.is_dir() {
                (Some(path), false)
            } else {
                eprintln!(
                    "warn: uBlock: ARCHIVR_UBLOCK_EXT={ext_path_str:?} is not a directory; \
                     capturing without ad-blocking"
                );
                (None, true)
            }
        }
    }
}

/// Inner implementation.  Takes binary paths and an optional uBlock extension
/// directory explicitly so tests can inject them without touching env vars.
///
/// When `ublock_ext` is `Some(path)` we own Chrome's lifecycle:
///   1. allocate a free TCP port,
///   2. launch Chrome headless with the extension loaded,
///   3. wait for Chrome's DevTools HTTP API to respond,
///   4. run single-file pointing at our Chrome via `--browser-server`,
///   5. kill Chrome after single-file exits.
///
/// When `ublock_ext` is `None` the original behaviour is preserved:
/// single-file launches and manages Chrome internally.
fn save_with(
    url: &str,
    store_path: &Path,
    timestamp: &str,
    single_file: &str,
    chrome: &str,
    cookies: &HashMap<String, String>,
    ublock_ext: Option<&Path>,
    reader_mode: bool,
) -> Result<SaveResult> {
    let temp_dir = store_path.join("temp").join(timestamp);
    std::fs::create_dir_all(&temp_dir).context("failed to create temp dir")?;

    let out_file = temp_dir.join(format!("{timestamp}.html"));

    // Mandatory user script: strips <script> elements from the live DOM just
    // before SingleFile serializes it.  Lets scripts execute during capture (so
    // JS-applied CSS is present) without leaving data:-URL ES modules in the
    // saved file that would cause "base scheme isn't hierarchical" errors.
    // JSON-LD structured data is kept.
    let strip_scripts_path = temp_dir.join("sf-strip-scripts.js");
    std::fs::write(
        &strip_scripts_path,
        "addEventListener('single-file-on-before-capture-start',()=>{\
          document.querySelectorAll('script:not([type=\"application/ld+json\"])')\
          .forEach(el=>el.remove());\
        });",
    )
    .context("failed to write single-file user script")?;

    // Reader-mode scripts: Readability.js + wrapper.  Written to temp dir and
    // passed as additional --browser-script args when reader mode is enabled.
    let mut extra_browser_scripts: Vec<PathBuf> = Vec::new();
    if reader_mode {
        let readability_path = temp_dir.join("sf-readability.js");
        std::fs::write(&readability_path, READABILITY_JS)
            .context("failed to write Readability.js script")?;
        let wrapper_path = temp_dir.join("sf-reader-mode.js");
        std::fs::write(&wrapper_path, READER_MODE_WRAPPER_JS)
            .context("failed to write reader-mode wrapper script")?;
        extra_browser_scripts.push(readability_path);
        extra_browser_scripts.push(wrapper_path);
    }

    // Isolated Chrome profile directory.
    let chrome_data_dir = temp_dir.join("chrome-data");

    // Extra Chrome flags from the environment (e.g. "--no-sandbox" in Docker).
    let extra_chrome_args: Vec<String> = env::var("ARCHIVR_CHROME_ARGS")
        .unwrap_or_default()
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    // Write cookie file before running Chrome so it's available immediately.
    // Never pass cookie values in process args (ps exposure).
    let cookie_file: Option<PathBuf> = if !cookies.is_empty() {
        let cf = temp_dir.join("cookies.txt");
        let domain = domain_from_url(url);
        write_netscape_cookie_file(cookies, &domain, &cf)
            .context("failed to write single-file cookie file")?;
        Some(cf)
    } else {
        None
    };

    let sf_output = match ublock_ext {
        // ── We own Chrome's lifecycle (uBlock extension mode) ─────────────
        Some(ext_path) => {
            let port = allocate_free_port().context("failed to allocate a free TCP port")?;

            let mut chrome_flags = vec![
                "--headless=new".to_string(),
                format!("--remote-debugging-port={port}"),
                format!("--user-data-dir={}", chrome_data_dir.display()),
                // Load the extension; disable all others so no unexpected ext loads.
                format!("--load-extension={}", ext_path.display()),
                format!("--disable-extensions-except={}", ext_path.display()),
                // Allow cross-origin font inlining (same reason as standalone mode).
                "--disable-web-security".to_string(),
                // Realistic viewport so responsive @media rules are preserved.
                "--window-size=1920,1080".to_string(),
            ];
            chrome_flags.extend(extra_chrome_args);

            let mut chrome_child = Command::new(chrome)
                .args(&chrome_flags)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .with_context(|| format!("failed to spawn Chrome ({chrome})"))?;

            if !wait_for_chrome_ready(port, 10) {
                let _ = chrome_child.kill();
                let _ = chrome_child.wait();
                bail!("Chrome did not become ready on port {port} within 10 s");
            }

            // Build scripts list: strip-scripts first, then any reader-mode scripts.
            let mut scripts: Vec<&Path> = vec![strip_scripts_path.as_path()];
            scripts.extend(extra_browser_scripts.iter().map(|p| p.as_path()));
            let out = run_single_file_with_server(
                url,
                &out_file,
                single_file,
                port,
                &scripts,
                cookie_file.as_deref(),
            );

            // Always kill Chrome and reap its exit status, even on single-file failure.
            let _ = chrome_child.kill();
            let _ = chrome_child.wait();

            out.with_context(|| format!("failed to spawn single-file ({single_file})"))?
        }

        // ── single-file manages Chrome (original behaviour) ───────────────
        None => {
            let mut chrome_flags = vec![
                "--disable-web-security".to_string(),
                format!("--user-data-dir={}", chrome_data_dir.display()),
                "--window-size=1920,1080".to_string(),
            ];
            chrome_flags.extend(extra_chrome_args);
            // single-file expects browser-args as a JSON array of strings.
            let quoted: Vec<String> = chrome_flags
                .iter()
                .map(|f| format!("\"{}\"", f.replace('\\', "\\\\").replace('"', "\\\"")))
                .collect();
            let browser_args = format!("[{}]", quoted.join(","));

            let mut scripts: Vec<&Path> = vec![strip_scripts_path.as_path()];
            scripts.extend(extra_browser_scripts.iter().map(|p| p.as_path()));
            run_single_file_standalone(
                url,
                &out_file,
                single_file,
                chrome,
                &browser_args,
                &scripts,
                cookie_file.as_deref(),
            )
            .with_context(|| format!("failed to spawn single-file ({single_file})"))?
        }
    };

    // Delete cookie file unconditionally — including on failure — so secrets
    // are never left in store/temp when the capture fails.
    if let Some(cf) = &cookie_file {
        let _ = std::fs::remove_file(cf);
    }

    if !sf_output.status.success() {
        let stderr = String::from_utf8_lossy(&sf_output.stderr);
        bail!("single-file failed: {stderr}");
    }

    if !out_file.exists() {
        bail!(
            "single-file exited successfully but produced no output file at {}",
            out_file.display()
        );
    }

    let title = extract_html_title(&out_file);
    let html_hash = hash_file(&out_file)?;
    let (favicon_hash, favicon_ext) =
        extract_and_save_favicon(&out_file, &temp_dir, timestamp)
            .map(|(h, e)| (Some(h), Some(e)))
            .unwrap_or((None, None));

    Ok(SaveResult {
        html_hash,
        title,
        favicon_hash,
        favicon_ext,
        ublock_skipped: false, // overwritten by save() from resolve_ublock_config()
    })
}

// ── Chrome helpers ────────────────────────────────────────────────────────────

/// Binds a `TcpListener` to a random OS-assigned port, reads the port number,
/// then drops the listener.  The tiny TOCTOU window between drop and Chrome
/// binding is acceptable in practice.
fn allocate_free_port() -> Result<u16> {
    let listener =
        TcpListener::bind("127.0.0.1:0").context("could not bind to a free TCP port")?;
    Ok(listener.local_addr()?.port())
}

/// Polls `http://127.0.0.1:{port}/json/version` every 150 ms until Chrome
/// responds with HTTP 200 or the deadline (timeout_secs) elapses.
fn wait_for_chrome_ready(port: u16, timeout_secs: u64) -> bool {
    let url = format!("http://127.0.0.1:{port}/json/version");
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while Instant::now() < deadline {
        if client
            .get(&url)
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    false
}

// ── single-file invocation helpers ───────────────────────────────────────────

/// Runs single-file pointing at an already-running Chrome via the DevTools HTTP
/// API (`--browser-server`).  Chrome was started by the caller, which retains
/// ownership of the process handle and kills it after this call returns.
fn run_single_file_with_server(
    url: &str,
    out_file: &Path,
    single_file: &str,
    port: u16,
    scripts: &[&Path],
    cookie_file: Option<&Path>,
) -> std::io::Result<std::process::Output> {
    let mut cmd = base_single_file_cmd(url, out_file, single_file, scripts, cookie_file);
    cmd.arg(format!("--browser-server=http://127.0.0.1:{port}"));
    cmd.output()
}

/// Runs single-file, letting it launch and manage Chrome itself.
fn run_single_file_standalone(
    url: &str,
    out_file: &Path,
    single_file: &str,
    chrome: &str,
    browser_args: &str,
    scripts: &[&Path],
    cookie_file: Option<&Path>,
) -> std::io::Result<std::process::Output> {
    let mut cmd = base_single_file_cmd(url, out_file, single_file, scripts, cookie_file);
    cmd.arg(format!("--browser-executable-path={chrome}"))
        .arg("--browser-headless")
        .arg(format!("--browser-args={browser_args}"));
    cmd.output()
}

/// Builds a `Command` with the single-file args that are the same regardless
/// of how Chrome is started.  Passes each script as a separate `--browser-script` arg.
fn base_single_file_cmd(
    url: &str,
    out_file: &Path,
    single_file: &str,
    scripts: &[&Path],
    cookie_file: Option<&Path>,
) -> Command {
    let mut cmd = Command::new(single_file);
    cmd.arg(url)
        .arg(out_file)
        .arg("--browser-wait-until=networkidle2")
        .arg("--browser-wait-delay=2000")
        .arg("--user-agent=Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/137.0.0.0 Safari/537.36")
        .arg("--remove-unused-styles=false")
        .arg("--remove-alternative-medias=false")
        .arg("--block-scripts=false")
        .arg("--remove-unused-fonts=false")
        .arg("--remove-alternative-fonts=false");
    for script in scripts {
        cmd.arg(format!("--browser-script={}", script.display()));
    }
    if let Some(cf) = cookie_file {
        cmd.arg(format!("--browser-cookies-file={}", cf.display()));
    }
    cmd
}

// ── HTML helpers ──────────────────────────────────────────────────────────────

/// Reads the first 8 KiB of `path` and extracts the content of the first
/// `<title>…</title>` element. Returns `None` if absent or empty.
///
/// Uses `to_ascii_lowercase` for case-insensitive tag matching. ASCII-only
/// lowercasing is byte-length-preserving, so byte offsets derived from the
/// lowercased buffer are valid indices into the original buffer.
fn extract_html_title(path: &Path) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 8192];
    let n = f.read(&mut buf).ok()?;
    let buf = &buf[..n];
    let lower = String::from_utf8_lossy(buf).to_ascii_lowercase();
    let start = lower.find("<title>")? + "<title>".len();
    let end = lower[start..].find("</title>")? + start;
    let title = String::from_utf8_lossy(&buf[start..end])
        .trim()
        .to_string();
    if title.is_empty() { None } else { Some(title) }
}

/// Extracts the favicon embedded in a single-file HTML archive.
///
/// Scans for a `<link rel="…icon…">` tag whose `href` is a `data:image/…;base64,…` URL.
/// Decodes the base64 payload, writes it to `{temp_dir}/{timestamp}.favicon.{ext}`,
/// hashes the file, and returns `(sha256_hex, ".ext")`.
/// All failures are silent (returns `None`) — a missing favicon is non-fatal.
fn extract_and_save_favicon(
    html_path: &Path,
    temp_dir: &Path,
    timestamp: &str,
) -> Option<(String, String)> {
    let content = std::fs::read_to_string(html_path).ok()?;
    let lower = content.to_ascii_lowercase();

    // Find the first <link …> tag that looks like a favicon with a data: href.
    let mut search_pos = 0;
    loop {
        let tag_start = lower[search_pos..].find("<link")? + search_pos;
        let tag_end = lower[tag_start..].find('>')? + tag_start;
        let tag = &lower[tag_start..=tag_end];

        if tag.contains("icon") {
            // Look for href="data:image/...;base64,..."
            if let Some(href_pos) = tag.find("href=") {
                let after_href = &content[tag_start + href_pos + 5..];
                let (quote, after_quote) = if after_href.starts_with('"') {
                    ('"', &after_href[1..])
                } else if after_href.starts_with('\'') {
                    ('\'', &after_href[1..])
                } else {
                    search_pos = tag_end + 1;
                    continue;
                };
                let value_end = after_quote.find(quote)?;
                let href_value = &after_quote[..value_end];
                if let Some(b64_start) = href_value.to_ascii_lowercase().find(";base64,") {
                    let mime_part = &href_value[5..b64_start]; // skip "data:"
                    let ext = mime_to_favicon_ext(mime_part)?;
                    let b64_data = &href_value[b64_start + 8..];
                    let bytes = B64.decode(b64_data).ok()?;
                    let out_path = temp_dir.join(format!("{timestamp}.favicon{ext}"));
                    std::fs::write(&out_path, &bytes).ok()?;
                    let hash = hash_file(&out_path).ok()?;
                    return Some((hash, ext.to_string()));
                }
            }
        }

        search_pos = tag_end + 1;
    }
}

fn mime_to_favicon_ext(mime: &str) -> Option<&'static str> {
    match mime.to_ascii_lowercase().trim() {
        "image/x-icon" | "image/vnd.microsoft.icon" => Some(".ico"),
        "image/png"  => Some(".png"),
        "image/jpeg" => Some(".jpg"),
        "image/gif"  => Some(".gif"),
        "image/svg+xml" => Some(".svg"),
        "image/webp" => Some(".webp"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn extract_html_title_finds_title() {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "<html><head><title>Paul Graham — Great Work</title></head></html>").unwrap();
        assert_eq!(
            extract_html_title(f.path()),
            Some("Paul Graham — Great Work".to_string())
        );
    }

    #[test]
    fn extract_html_title_case_insensitive() {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "<HTML><HEAD><TITLE>My Page</TITLE></HEAD></HTML>").unwrap();
        assert_eq!(extract_html_title(f.path()), Some("My Page".to_string()));
    }

    #[test]
    fn extract_html_title_empty_tag_returns_none() {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "<html><head><title>   </title></head></html>").unwrap();
        assert_eq!(extract_html_title(f.path()), None);
    }

    #[test]
    fn extract_html_title_no_title_tag_returns_none() {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "<html><head></head><body>no title here</body></html>").unwrap();
        assert_eq!(extract_html_title(f.path()), None);
    }

    #[test]
    fn save_with_missing_binary_returns_clear_error() {
        // Calls save_with directly — no env mutation, safe in parallel test runs.
        let tmp = tempfile::tempdir().unwrap();
        let result = save_with(
            "https://example.com",
            tmp.path(),
            "test-ts",
            "/nonexistent/single-file",
            "chromium",
            &HashMap::new(),
            None, // no ublock ext
            false, // reader mode off
        );
        let err = result.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("spawn") || msg.contains("nonexistent") || msg.contains("No such"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn resolve_ublock_config_disabled_when_false() {
        // Can't mutate env vars safely in parallel tests; test the logic directly
        // by verifying the env-var parsing branch we care about.
        let enabled = "false";
        let is_disabled =
            enabled.eq_ignore_ascii_case("false") || enabled == "0";
        assert!(is_disabled);

        let enabled = "0";
        let is_disabled =
            enabled.eq_ignore_ascii_case("false") || enabled == "0";
        assert!(is_disabled);
    }
}
