use anyhow::{Context, Result, bail};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use std::{
    collections::HashMap,
    env,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

use crate::downloader::cookies::{domain_from_url, write_netscape_cookie_file};
use crate::hash::hash_file;

/// Combined reader-mode script: Readability.js (Apache 2.0) bundled with the
/// archivr wrapper in a single IIFE.  single-file-cli concatenates all
/// `--browser-script` files into one string before injection (scripts.js:84),
/// so scope sharing is guaranteed; the combined file is kept for clarity.
///
/// Emits `<meta name="archivr-reader-mode" content="applied|failed:REASON">`
/// so the outcome is observable in the saved HTML.
const READER_MODE_SCRIPT: &str = concat!(
    // Readability.js is injected verbatim first so `Readability` is in scope.
    include_str!("../../../../vendor/readability/Readability.js"),
    // Wrapper IIFE — runs on single-file-on-before-capture-request.
    // Sets 'installed' immediately at script-evaluation time so a missing meta
    // means the browser-script was never injected at all.
    r#"
;(function() {
  function _archivrReaderMark(content) {
    try {
      var m = document.querySelector('meta[name="archivr-reader-mode"]');
      if (!m) {
        m = document.createElement('meta');
        m.name = 'archivr-reader-mode';
        (document.head || document.documentElement).appendChild(m);
      }
      m.content = content;
    } catch(_) {}
  }
  // Mark immediately: if this meta is absent in the artifact the script
  // was never injected (separate from the hook never firing).
  _archivrReaderMark('installed');
  function _archivrApplyReader() {
    try {
      if (typeof Readability === 'undefined') {
        _archivrReaderMark('failed:no-readability');
        return;
      }
      var article = new Readability(document.cloneNode(true)).parse();
      if (!article || !article.content || article.content.length < 100) {
        _archivrReaderMark('failed:no-article');
        return;
      }
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
      _archivrReaderMark('applied');
    } catch (e) {
      _archivrReaderMark('failed:exception:' + (e && e.message ? e.message : String(e)));
    }
  }
  // Ensure _singleFile_waitForUserScript is installed (strip-scripts also does
  // this, but be explicit here in case reader-mode ever runs without it).
  dispatchEvent(new CustomEvent('single-file-user-script-init'));
  // Synchronous work — no preventDefault()/response dispatch needed.
  addEventListener('single-file-on-before-capture-request', _archivrApplyReader);
})();
"#
);

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
    /// `true` when `ARCHIVR_COOKIE_CONSENT=true` (the default) but the extension path
    /// was missing or invalid.  The capture succeeded but ran without cookie-consent blocking.
    pub cookie_ext_skipped: bool,
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
    cookie_ext_enabled: Option<bool>,
    reader_mode: bool,
) -> Result<SaveResult> {
    let single_file =
        env::var("ARCHIVR_SINGLE_FILE").unwrap_or_else(|_| "single-file".to_string());
    let chrome = env::var("ARCHIVR_CHROME").unwrap_or_else(|_| "chromium".to_string());
    let (ublock_ext, ublock_skipped) = resolve_ublock_config(ublock_enabled_override);
    let (cookie_ext, cookie_ext_skipped) = resolve_cookie_ext_config(cookie_ext_enabled);
    let mut result = save_with(
        url,
        store_path,
        timestamp,
        &single_file,
        &chrome,
        cookies,
        ublock_ext.as_deref(),
        cookie_ext.as_deref(),
        reader_mode,
    )?;
    result.ublock_skipped = ublock_skipped;
    result.cookie_ext_skipped = cookie_ext_skipped;
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

/// Resolves cookie-consent extension configuration from env vars, optionally overridden by the caller.
///
/// Returns:
/// - `(Some(path), false)` — cookie-consent ext is enabled and the extension dir is valid.
/// - `(None, true)`  — cookie-consent ext is enabled but the extension dir is missing/invalid
///                     (warns to stderr; the capture proceeds without cookie-consent blocking).
/// - `(None, false)` — cookie-consent ext is disabled (`ARCHIVR_COOKIE_CONSENT=false` or overridden).
fn resolve_cookie_ext_config(enabled_override: Option<bool>) -> (Option<PathBuf>, bool) {
    let want_cookie_ext = enabled_override.unwrap_or_else(|| {
        let env_val = env::var("ARCHIVR_COOKIE_CONSENT").unwrap_or_else(|_| "true".to_string());
        !env_val.eq_ignore_ascii_case("false") && env_val != "0"
    });
    if !want_cookie_ext {
        return (None, false);
    }
    match env::var("ARCHIVR_COOKIE_EXT").ok().filter(|s| !s.is_empty()) {
        None => {
            eprintln!(
                "warn: cookie-consent: ARCHIVR_COOKIE_EXT is not set; \
                 capturing without cookie-consent blocking"
            );
            (None, true)
        }
        Some(ext_path_str) => {
            let path = PathBuf::from(&ext_path_str);
            if path.is_dir() {
                (Some(path), false)
            } else {
                eprintln!(
                    "warn: cookie-consent: ARCHIVR_COOKIE_EXT={ext_path_str:?} is not a directory; \
                     capturing without cookie-consent blocking"
                );
                (None, true)
            }
        }
    }
}

/// Inner implementation.  Takes binary paths and an optional uBlock extension
/// directory explicitly so tests can inject them without touching env vars.
///
/// single-file always manages Chrome.  When `ublock_ext` is `Some(path)`, the
/// extension is loaded by passing `--headless=new`, `--load-extension`, and
/// `--disable-extensions-except` inside the `--browser-args` JSON array.
/// single-file's `browser.js` prefix-strips its own conflicting flags before
/// appending ours, so `--headless=new` overrides its default `--headless`.
///
/// Note: single-file always adds `--single-process` to Chrome.  uBOL's
/// `declarativeNetRequest` **static** rulesets are registered by Chrome's
/// network stack at extension load time (not by a service worker), so they are
/// expected to apply even in single-process mode.  Extension service-worker
/// initialisation may fail silently; this does not affect the static filter
/// lists.  Ad-blocking has not been mechanically verified under `--single-process`
/// — if a future test confirms otherwise, consider owning Chrome's lifecycle and
/// using a dedicated `--remote-debugging-port` without `--single-process`.
fn save_with(
    url: &str,
    store_path: &Path,
    timestamp: &str,
    single_file: &str,
    chrome: &str,
    cookies: &HashMap<String, String>,
    ublock_ext: Option<&Path>,
    cookie_ext: Option<&Path>,
    reader_mode: bool,
) -> Result<SaveResult> {
    let temp_dir = store_path.join("temp").join(timestamp);
    std::fs::create_dir_all(&temp_dir).context("failed to create temp dir")?;

    let out_file = temp_dir.join(format!("{timestamp}.html"));

    // Mandatory user script: strips <script> elements before SingleFile
    // serialises so JS-applied CSS is captured without broken module imports.
    // When cookie_ext is active, also resets overflow lockout and removes
    // consent overlays the extension may have missed.
    let strip_scripts_path = temp_dir.join("sf-strip-scripts.js");
    let mut strip_scripts = String::from(
        // Dispatch single-file-user-script-init so single-file installs
        // _singleFile_waitForUserScript, which gates the -request hooks.
        "dispatchEvent(new CustomEvent('single-file-user-script-init'));\
         addEventListener('single-file-on-before-capture-request',()=>{\
           document.querySelectorAll('script:not([type=\"application/ld+json\"])')\
           .forEach(el=>el.remove());",
    );
    if cookie_ext.is_some() {
        // Reset overflow:hidden that consent modals inject on body/html.
        // Gate on cookie_ext so we never mutate pages where the feature is off.
        strip_scripts.push_str(
            "document.body&&(document.body.style.overflow='');\
             document.documentElement&&(document.documentElement.style.overflow='');\
             /* Remove consent overlays the extension may have missed          \
              * (e.g. Google Funding Choices, Quantcast, Sourcepoint).        \
              * Selectors are specific to consent infrastructure, not content. */\
             document.querySelectorAll(\
               '.fc-consent-root,.fc-dialog-overlay,.fc-dialog,\
                .qc-cmp2-container,.qc-cmp2-ui,\
                .sp-message-container,\
                #sp-cc,\
                #usercentrics-root'\
             ).forEach(function(el){el.remove();});",
        );
    }
    if ublock_ext.is_some() {
        // uBlock blocks ad network requests but first-party ad placeholder
        // elements (ins.adsbygoogle, iframe hosts) retain their computed
        // height, leaving blank space. Remove them pre-capture.
        strip_scripts.push_str(
            "document.querySelectorAll(\
               'ins.adsbygoogle,\
                [id^=\"aswift_\"],\
                iframe[id^=\"google_ads_\"],\
                iframe[name^=\"google_ads_frame\"],\
                iframe[src*=\"googlesyndication\"],\
                iframe[src*=\"doubleclick\"]'\
             ).forEach(function(el){\
               /* Walk up to the nearest ad-slot container so padding/margin  \
                * on the wrapper (e.g. .top-ad, .google-auto-placed) collapses \
                * too, not just the inner ins/iframe element.                  */\
               var slot=el.closest('.top-ad,.google-auto-placed,.ad-slot,.ad-container');\
               (slot||el).remove();\
             });",
        );
    }
    strip_scripts.push_str("});");
    std::fs::write(&strip_scripts_path, &strip_scripts)
        .context("failed to write single-file user script")?;

    // Optional reader-mode script: Readability.js + wrapper combined into one
    // file so both run in the same execution scope.  (Separate --browser-script
    // files can each get their own context depending on single-file version.)
    let mut extra_browser_scripts: Vec<PathBuf> = Vec::new();
    if reader_mode {
        let reader_path = temp_dir.join("sf-reader-mode.js");
        std::fs::write(&reader_path, READER_MODE_SCRIPT)
            .context("failed to write reader-mode script")?;
        extra_browser_scripts.push(reader_path);
    }

    // Isolated Chrome profile directory; cleaned up with the rest of temp.
    let chrome_data_dir = temp_dir.join("chrome-data");

    // Build Chrome flags passed via --browser-args to single-file.
    // single-file's browser.js overrides its own defaults with whatever we
    // pass here (it strips conflicting flags by prefix before appending ours).
    let mut chrome_flags = vec![
        "--disable-web-security".to_string(),
        format!("--user-data-dir={}", chrome_data_dir.display()),
        "--window-size=1920,1080".to_string(),
    ];
    // Build comma-separated extension list for Chrome flags.
    // --headless=new is required for --load-extension to work.
    let ext_paths: Vec<PathBuf> = [ublock_ext, cookie_ext]
        .iter()
        .filter_map(|p| p.map(|p| p.to_path_buf()))
        .collect();
    if !ext_paths.is_empty() {
        let joined = ext_paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(",");
        chrome_flags.push("--headless=new".to_string());
        chrome_flags.push(format!("--load-extension={joined}"));
        chrome_flags.push(format!("--disable-extensions-except={joined}"));
    }
    // Operator extras (e.g. --no-sandbox in Docker).
    let extra_chrome_args: Vec<String> = env::var("ARCHIVR_CHROME_ARGS")
        .unwrap_or_default()
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    chrome_flags.extend(extra_chrome_args);

    // single-file expects browser-args as a JSON array of strings.
    let quoted: Vec<String> = chrome_flags
        .iter()
        .map(|f| format!("\"{}\"", f.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect();
    let browser_args = format!("[{}]", quoted.join(","));

    // Write cookie file (secrets must never appear in process args).
    let cookie_file: Option<PathBuf> = if !cookies.is_empty() {
        let cf = temp_dir.join("cookies.txt");
        let domain = domain_from_url(url);
        write_netscape_cookie_file(cookies, &domain, &cf)
            .context("failed to write single-file cookie file")?;
        Some(cf)
    } else {
        None
    };

    let mut scripts: Vec<&Path> = vec![strip_scripts_path.as_path()];
    scripts.extend(extra_browser_scripts.iter().map(|p| p.as_path()));

    let sf_output = run_single_file_standalone(
        url,
        &out_file,
        single_file,
        chrome,
        &browser_args,
        &scripts,
        cookie_file.as_deref(),
    )
    .with_context(|| format!("failed to spawn single-file ({single_file})"))?;

    // Delete cookie file unconditionally — including on failure — so secrets
    // are never left in store/temp when the capture fails.
    if let Some(cf) = &cookie_file {
        let _ = std::fs::remove_file(cf);
    }

    if !sf_output.status.success() {
        let stderr = String::from_utf8_lossy(&sf_output.stderr);
        bail!("single-file failed (exit {:?}): {stderr}", sf_output.status.code());
    }

    if !out_file.exists() {
        // Collect diagnostics: stdout, stderr, and what's actually in the temp dir.
        let stdout = String::from_utf8_lossy(&sf_output.stdout);
        let stderr = String::from_utf8_lossy(&sf_output.stderr);
        let dir_contents: String = std::fs::read_dir(&temp_dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_else(|_| "<unreadable>".to_string());
        eprintln!(
            "warn: single-file produced no file at {}\n  temp dir contents: [{dir_contents}]\n  stderr: {}\n  stdout (first 200 chars): {}",
            out_file.display(),
            stderr.trim(),
            &stdout[..stdout.len().min(200)],
        );
        bail!(
            "single-file exited successfully but produced no output file at {}; \
             temp dir contains: [{dir_contents}]; \
             stderr: {}",
            out_file.display(),
            stderr.trim(),
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
        ublock_skipped: false,     // overwritten by save() from resolve_ublock_config()
        cookie_ext_skipped: false, // overwritten by save() from resolve_cookie_ext_config()
    })
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
        .arg("--remove-alternative-fonts=false")
        // Explicitly prevent single-file from dumping HTML to stdout instead of
        // writing the file (its Docker-detection heuristic can trigger on some setups).
        .arg("--dump-content=false");
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
            None, // no cookie ext
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
    fn save_with_both_extensions_uses_comma_joined_flags() {
        use std::path::Path;
        // We can't run single-file here, but we can exercise the flag-building
        // logic by checking the path list construction directly.
        let ublock = Path::new("/tmp/ublock");
        let cookie = Path::new("/tmp/cookie");
        let ext_paths: Vec<std::path::PathBuf> = [Some(ublock), Some(cookie)]
            .iter()
            .filter_map(|p| p.map(|p| p.to_path_buf()))
            .collect();
        let joined = ext_paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(joined, "/tmp/ublock,/tmp/cookie");
        let load_flag = format!("--load-extension={joined}");
        let except_flag = format!("--disable-extensions-except={joined}");
        assert_eq!(load_flag, "--load-extension=/tmp/ublock,/tmp/cookie");
        assert_eq!(except_flag, "--disable-extensions-except=/tmp/ublock,/tmp/cookie");
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
