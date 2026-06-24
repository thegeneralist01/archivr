use anyhow::{Context, Result, bail};
use std::path::Path;

use crate::hash::hash_file;

/// Whether a URL resolves to an HTML document or a downloadable file.
#[derive(Debug, PartialEq, Eq)]
pub enum UrlKind {
    Html,
    File,
}

/// Probes `url` with a HEAD request and inspects the `Content-Type` header.
/// Falls back to a GET request (body not read) if the server returns 405.
///
/// Returns `Err` if the probe fails (network error, non-2xx/405 status).
/// Redirects (3xx) are followed automatically by reqwest; only the final
/// response status is checked.
pub fn probe_url_kind(url: &str) -> Result<UrlKind> {
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent("archivr/0.1")
        .build()
        .context("failed to build HTTP client")?;

    // Prefer HEAD: no body transfer.
    let head = client
        .head(url)
        .send()
        .with_context(|| format!("failed to probe {url}"))?;

    if head.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED {
        // Server rejected HEAD — do a GET but only inspect headers.
        let get = client
            .get(url)
            .send()
            .with_context(|| format!("failed to probe {url}"))?;
        if !get.status().is_success() {
            bail!("HTTP {} probing {url}", get.status());
        }
        let ct = get
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        return Ok(if content_type_is_html(ct) {
            UrlKind::Html
        } else {
            UrlKind::File
        });
    }

    if !head.status().is_success() {
        bail!("HTTP {} probing {url}", head.status());
    }

    let ct = head
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    Ok(if content_type_is_html(ct) {
        UrlKind::Html
    } else {
        UrlKind::File
    })
}

/// Returns `(sha256_hex, extension_with_leading_dot, title_hint)` on success.
/// `title_hint` is derived from the `Content-Disposition` filename header, or the
/// last path segment of the final URL after redirects.
///
/// Errors if:
/// - The request fails or returns a non-2xx status.
/// - The response Content-Type is `text/html` (caller should use a web-page archiver instead).
/// - The body cannot be written to disk.
pub fn download(url: &str, store_path: &Path, timestamp: &str) -> Result<(String, String, Option<String>)> {
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

    // Extract title before consuming the response body.
    let title_hint = title_from_content_disposition(
        response
            .headers()
            .get(reqwest::header::CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
    )
    .or_else(|| title_from_url(response.url().path()));

    let temp_dir = store_path.join("temp").join(timestamp);
    std::fs::create_dir_all(&temp_dir).context("failed to create temp dir")?;

    let out_file = temp_dir.join(format!("{timestamp}{extension}"));

    let bytes = response
        .bytes()
        .with_context(|| format!("failed to read response body from {url}"))?;
    std::fs::write(&out_file, &bytes)
        .with_context(|| format!("failed to write downloaded file to {}", out_file.display()))?;

    let hash = hash_file(&out_file)?;
    Ok((hash, extension, title_hint))
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

/// Extracts a filename from a `Content-Disposition` header value.
///
/// Prefers `filename*=` (RFC 5987 percent-encoded, e.g. `filename*=UTF-8''Report%20Final.pdf`)
/// over plain `filename=`. Returns `None` if neither is present or the value is empty.
fn title_from_content_disposition(cd: &str) -> Option<String> {
    // RFC 5987: filename*=charset'language'encoded
    for part in cd.split(';') {
        let part = part.trim();
        if let Some(val) = part.strip_prefix("filename*=") {
            let val = val.trim().trim_matches('"');
            // encoded portion is after the second apostrophe
            if let Some(encoded) = val.splitn(3, '\'').nth(2) {
                let name = percent_decode(encoded);
                if !name.is_empty() {
                    return Some(name);
                }
            }
        }
    }
    // Plain filename=
    for part in cd.split(';') {
        let part = part.trim();
        if let Some(val) = part.strip_prefix("filename=") {
            let val = val.trim().trim_matches('"');
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

/// Derives a title from the last non-empty path segment of a URL path string.
///
/// Input is the raw percent-encoded path from the final URL after redirects
/// (e.g. `/papers/Facharbeit.pdf`). Returns `None` if the path has no meaningful segment.
fn title_from_url(path: &str) -> Option<String> {
    let segment = path
        .split('?')
        .next()
        .unwrap_or(path)
        .rsplit('/')
        .find(|s| !s.is_empty())?;
    let decoded = percent_decode(segment);
    if decoded.is_empty() { None } else { Some(decoded) }
}

/// Percent-decodes a string. Handles ASCII percent-encoded sequences; multi-byte
/// UTF-8 sequences are decoded correctly when each byte is encoded as `%XX`.
fn percent_decode(s: &str) -> String {
    let mut bytes: Vec<u8> = Vec::with_capacity(s.len());
    let src = s.as_bytes();
    let mut i = 0;
    while i < src.len() {
        if src[i] == b'%' && i + 2 < src.len() {
            let hi = src[i + 1];
            let lo = src[i + 2];
            let nibble = |b: u8| -> Option<u8> {
                match b {
                    b'0'..=b'9' => Some(b - b'0'),
                    b'a'..=b'f' => Some(b - b'a' + 10),
                    b'A'..=b'F' => Some(b - b'A' + 10),
                    _ => None,
                }
            };
            if let (Some(h), Some(l)) = (nibble(hi), nibble(lo)) {
                bytes.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        if src[i] == b'+' {
            bytes.push(b' ');
        } else {
            bytes.push(src[i]);
        }
        i += 1;
    }
    String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
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

    #[test]
    fn title_from_url_plain_filename() {
        assert_eq!(
            title_from_url("/papers/Facharbeit.pdf"),
            Some("Facharbeit.pdf".to_string())
        );
    }

    #[test]
    fn title_from_url_encoded() {
        assert_eq!(
            title_from_url("/files/My%20Report%202026.pdf"),
            Some("My Report 2026.pdf".to_string())
        );
    }

    #[test]
    fn title_from_url_root_is_none() {
        assert_eq!(title_from_url("/"), None);
    }

    #[test]
    fn title_from_url_no_slash() {
        assert_eq!(title_from_url("data.csv"), Some("data.csv".to_string()));
    }

    #[test]
    fn title_from_content_disposition_plain() {
        assert_eq!(
            title_from_content_disposition("attachment; filename=\"Facharbeit.pdf\""),
            Some("Facharbeit.pdf".to_string())
        );
    }

    #[test]
    fn title_from_content_disposition_rfc5987() {
        assert_eq!(
            title_from_content_disposition("attachment; filename*=UTF-8''Facharbeit%20Final.pdf"),
            Some("Facharbeit Final.pdf".to_string())
        );
    }

    #[test]
    fn title_from_content_disposition_empty() {
        assert_eq!(title_from_content_disposition(""), None);
    }

    #[test]
    fn title_from_content_disposition_prefers_rfc5987() {
        assert_eq!(
            title_from_content_disposition(
                "attachment; filename=\"old.pdf\"; filename*=UTF-8''new.pdf"
            ),
            Some("new.pdf".to_string())
        );
    }

    #[test]
    fn percent_decode_utf8() {
        // ä = 0xC3 0xA4
        assert_eq!(percent_decode("%C3%A4"), "ä");
    }

    #[test]
    fn percent_decode_plus_is_space() {
        assert_eq!(percent_decode("hello+world"), "hello world");
    }

    #[test]
    fn url_kind_html_variants() {
        // content_type_is_html is already tested; verify UrlKind is wired correctly
        // by checking the enum values exist and are distinct.
        assert_ne!(UrlKind::Html, UrlKind::File);
    }

    #[test]
    fn probe_url_kind_fails_on_unreachable_host() {
        // 127.0.0.1:1 is guaranteed to refuse connections.
        let err = probe_url_kind("http://127.0.0.1:1/").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("probe") || msg.contains("connect") || msg.contains("refused"),
            "unexpected error message: {msg}"
        );
    }
}
