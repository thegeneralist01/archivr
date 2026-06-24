use anyhow::{Context, Result, bail};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use std::{env, io::Read, path::Path, process::Command};

use crate::hash::hash_file;

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
}

/// Archives `url` as a self-contained HTML snapshot.
///
/// Returns `(sha256_hex, title_hint)` on success.
/// - `sha256_hex`: hash of the saved `.html` file, used as the blob key.
/// - `title_hint`: page title extracted from the `<title>` tag, if present.
///
/// Reads two env vars:
/// - `ARCHIVR_SINGLE_FILE`: path to the `single-file` binary (default: `"single-file"`).
/// - `ARCHIVR_CHROME`: path to the Chromium/Chrome binary (default: `"chromium"`).
pub fn save(url: &str, store_path: &Path, timestamp: &str) -> Result<SaveResult> {
    let single_file =
        env::var("ARCHIVR_SINGLE_FILE").unwrap_or_else(|_| "single-file".to_string());
    let chrome = env::var("ARCHIVR_CHROME").unwrap_or_else(|_| "chromium".to_string());
    save_with(url, store_path, timestamp, &single_file, &chrome)
}

/// Inner implementation; takes binary paths explicitly so tests can inject them
/// without mutating process-global environment variables.
fn save_with(
    url: &str,
    store_path: &Path,
    timestamp: &str,
    single_file: &str,
    chrome: &str,
) -> Result<SaveResult> {
    let temp_dir = store_path.join("temp").join(timestamp);
    std::fs::create_dir_all(&temp_dir).context("failed to create temp dir")?;

    let out_file = temp_dir.join(format!("{timestamp}.html"));

    let out = Command::new(single_file)
        .arg(url)
        .arg(&out_file)
        .arg(format!("--browser-executable-path={chrome}"))
        .arg("--browser-headless")
        .arg("--browser-wait-until=networkidle2")
        .output()
        .with_context(|| format!("failed to spawn {single_file} process"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
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
    let (favicon_hash, favicon_ext) = extract_and_save_favicon(&out_file, &temp_dir, timestamp)
        .map(|(h, e)| (Some(h), Some(e)))
        .unwrap_or((None, None));
    Ok(SaveResult { html_hash, title, favicon_hash, favicon_ext })
}

/// Reads the first 8 KiB of `path` and extracts the content of the first
/// `<title>…</title>` element. Returns `None` if absent or empty.
///
/// Uses `to_ascii_lowercase` for case-insensitive tag matching. ASCII-only
/// lowercasing is byte-length-preserving, so byte offsets derived from the
/// lowercased buffer are valid indices into the original buffer.
fn extract_html_title(path: &Path) -> Option<String> {
    let mut buf = [0u8; 8192];
    let n = std::fs::File::open(path).ok()?.read(&mut buf).ok()?;
    // Recover a valid UTF-8 prefix if the 8 KiB boundary falls mid-character.
    let snippet = match std::str::from_utf8(&buf[..n]) {
        Ok(s) => s,
        Err(e) => std::str::from_utf8(&buf[..e.valid_up_to()]).ok()?,
    };
    // ASCII-only lowercase: A-Z -> a-z, all other bytes unchanged.
    // Byte lengths are identical to the original, so offsets are safe to reuse.
    let lower = snippet.to_ascii_lowercase();
    let tag_start = lower.find("<title>")?;
    let content_start = tag_start + 7; // len("<title>") == 7
    let content_end = content_start + lower[content_start..].find("</title>")?;
    let title = snippet[content_start..content_end].trim();
    if title.is_empty() { None } else { Some(title.to_string()) }
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
    let html = std::fs::read_to_string(html_path).ok()?;
    let lower = html.to_ascii_lowercase();

    // Find a <link> tag that has rel="...icon..." AND href="data:image/..."
    let link_start = {
        let mut found = None;
        let mut search = 0;
        while search < lower.len() {
            let off = lower[search..].find("<link")?;
            let abs = search + off;
            // Find end of this tag, respecting quoted attribute values so that
            // a '>' inside a data URL does not terminate the tag prematurely.
            let tag_slice = &lower[abs..];
            let mut in_q = false;
            let mut tag_end = None;
            for (i, c) in tag_slice.char_indices() {
                match c {
                    '"' => in_q = !in_q,
                    '>' if !in_q => { tag_end = Some(i); break; }
                    _ => {}
                }
            }
            let tag_end = match tag_end { Some(e) => e, None => break };
            let tag_s = &lower[abs..abs + tag_end];
            if tag_s.contains("rel=") && tag_s.contains("icon") && tag_s.contains("href=\"data:image") {
                found = Some(abs);
                break;
            }
            search = abs + tag_end + 1;
        }
        found?
    };

    // Extract href value from the original HTML (byte positions match because
    // to_ascii_lowercase is byte-length-preserving).
    let tag_lower = &lower[link_start..];
    let href_off = tag_lower.find("href=\"")?;
    let value_start = link_start + href_off + 6; // past href="
    let value_end = html[value_start..].find('"')?;
    let data_url = &html[value_start..value_start + value_end];

    // Parse data:<mime>;base64,<payload>
    let rest = data_url.strip_prefix("data:")?;
    let comma = rest.find(',')?;
    let meta = &rest[..comma];
    let b64 = &rest[comma + 1..];
    if !meta.to_ascii_lowercase().contains("base64") {
        return None;
    }
    let mime = meta.split(';').next()?.trim().to_ascii_lowercase();
    let ext = mime_to_favicon_ext(&mime)?;

    let bytes = B64.decode(b64.trim()).ok()?;
    let out = temp_dir.join(format!("{timestamp}.favicon{ext}"));
    std::fs::write(&out, &bytes).ok()?;
    hash_file(&out).ok().map(|h| (h, ext.to_string()))
}

fn mime_to_favicon_ext(mime: &str) -> Option<&'static str> {
    match mime {
        "image/x-icon" | "image/vnd.microsoft.icon" => Some(".ico"),
        "image/png" => Some(".png"),
        "image/svg+xml" => Some(".svg"),
        "image/jpeg" => Some(".jpg"),
        "image/gif" => Some(".gif"),
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
        );
        let err = result.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("spawn") || msg.contains("nonexistent") || msg.contains("No such"),
            "unexpected error: {msg}"
        );
    }
}
