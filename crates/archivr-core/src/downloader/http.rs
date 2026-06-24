use anyhow::{Context, Result, bail};
use std::path::Path;

use crate::hash::hash_file;

/// Downloads a file from `url` into `store_path/temp/timestamp/`.
///
/// Returns `(sha256_hex, extension_with_leading_dot)` on success.
///
/// Errors if:
/// - The request fails or returns a non-2xx status.
/// - The response Content-Type is `text/html` (caller should use a web-page archiver instead).
/// - The body cannot be written to disk.
pub fn download(url: &str, store_path: &Path, timestamp: &str) -> Result<(String, String)> {
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent("archivr/0.1")
        .build()
        .context("failed to build HTTP client")?;

    let response = client
        .get(url)
        .send()
        .with_context(|| format!("failed to fetch {url}"))?;

    if !response.status().is_success() {
        bail!("HTTP {} fetching {url}", response.status());
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if content_type_is_html(&content_type) {
        bail!(
            "URL returned HTML (Content-Type: {content_type}); \
             use a web-page archiver for this URL"
        );
    }

    let extension = extension_from_url(url)
        .or_else(|| extension_from_content_type(&content_type))
        .unwrap_or_default();

    let temp_dir = store_path.join("temp").join(timestamp);
    std::fs::create_dir_all(&temp_dir).context("failed to create temp dir")?;

    let out_file = temp_dir.join(format!("{timestamp}{extension}"));

    let bytes = response
        .bytes()
        .with_context(|| format!("failed to read response body from {url}"))?;
    std::fs::write(&out_file, &bytes)
        .with_context(|| format!("failed to write downloaded file to {}", out_file.display()))?;

    let hash = hash_file(&out_file)?;
    Ok((hash, extension))
}

fn content_type_is_html(content_type: &str) -> bool {
    let ct = content_type.split(';').next().unwrap_or("").trim();
    ct == "text/html" || ct == "application/xhtml+xml"
}

/// Derives a file extension (e.g. `".pdf"`) from the URL path component.
/// Strips query strings first. Returns `None` if no recognizable extension found.
fn extension_from_url(url: &str) -> Option<String> {
    let path = url.split('?').next().unwrap_or(url);
    let last_segment = path.rsplit('/').next().unwrap_or("");
    if let Some(dot_pos) = last_segment.rfind('.') {
        let ext = &last_segment[dot_pos..];
        // Accept only short, alphanumeric extensions (1–5 chars after the dot)
        if ext.len() >= 2 && ext.len() <= 6 && ext[1..].chars().all(|c| c.is_ascii_alphanumeric()) {
            return Some(ext.to_ascii_lowercase());
        }
    }
    None
}

/// Maps a MIME type to a file extension. Returns `None` for unrecognized types.
fn extension_from_content_type(content_type: &str) -> Option<String> {
    let ct = content_type.split(';').next().unwrap_or("").trim();
    match ct {
        "application/pdf" => Some(".pdf".to_string()),
        "image/jpeg" | "image/jpg" => Some(".jpg".to_string()),
        "image/png" => Some(".png".to_string()),
        "image/gif" => Some(".gif".to_string()),
        "image/webp" => Some(".webp".to_string()),
        "image/svg+xml" => Some(".svg".to_string()),
        "video/mp4" => Some(".mp4".to_string()),
        "video/webm" => Some(".webm".to_string()),
        "video/ogg" => Some(".ogv".to_string()),
        "audio/mpeg" | "audio/mp3" => Some(".mp3".to_string()),
        "audio/ogg" => Some(".ogg".to_string()),
        "audio/wav" => Some(".wav".to_string()),
        "application/zip" => Some(".zip".to_string()),
        "application/gzip" => Some(".gz".to_string()),
        "application/json" => Some(".json".to_string()),
        "text/plain" => Some(".txt".to_string()),
        "text/csv" => Some(".csv".to_string()),
        "text/xml" | "application/xml" => Some(".xml".to_string()),
        "application/epub+zip" => Some(".epub".to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_from_url_pdf() {
        assert_eq!(extension_from_url("https://example.com/paper.pdf"), Some(".pdf".to_string()));
    }

    #[test]
    fn extension_from_url_strips_query() {
        assert_eq!(extension_from_url("https://example.com/file.zip?token=abc"), Some(".zip".to_string()));
    }

    #[test]
    fn extension_from_url_no_extension() {
        assert_eq!(extension_from_url("https://example.com/page"), None);
    }

    #[test]
    fn extension_from_url_rejects_long_ext() {
        assert_eq!(extension_from_url("https://example.com/file.toolongext"), None);
    }

    #[test]
    fn extension_from_content_type_pdf() {
        assert_eq!(extension_from_content_type("application/pdf"), Some(".pdf".to_string()));
    }

    #[test]
    fn extension_from_content_type_with_params() {
        assert_eq!(extension_from_content_type("application/pdf; charset=utf-8"), Some(".pdf".to_string()));
    }

    #[test]
    fn content_type_is_html_plain() {
        assert!(content_type_is_html("text/html"));
    }

    #[test]
    fn content_type_is_html_with_charset() {
        assert!(content_type_is_html("text/html; charset=utf-8"));
    }

    #[test]
    fn content_type_is_html_xhtml() {
        assert!(content_type_is_html("application/xhtml+xml"));
    }

    #[test]
    fn content_type_is_html_pdf_is_not_html() {
        assert!(!content_type_is_html("application/pdf"));
    }
}
