use anyhow::Result;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use regex::Regex;
use std::{fs, path::Path};

use crate::hash::hash_bytes;

#[derive(Debug, Clone)]
pub struct ExtractedFont {
    /// SHA3-256 hex of the raw font bytes.
    pub sha256: String,
    /// File extension including leading dot, e.g. `".woff2"`.
    pub ext: String,
    /// Size in bytes of the decoded font.
    pub byte_size: i64,
    /// Store-relative path, e.g. `"raw/a/b/abc...woff2"`.
    pub raw_relpath: String,
}

/// Scans `html` for `@font-face` `src` attributes containing `data:font/...;base64,...`
/// URLs, decodes each font, stores it in `{store_path}/raw/`, and rewrites the src
/// to `/api/archives/{archive_id}/blobs/{sha256}`.
///
/// Returns `(rewritten_html, Vec<ExtractedFont>)`. Each occurrence is reported
/// individually; the caller deduplicates via `upsert_blob`.
pub fn extract_and_rewrite(
    html: &str,
    store_path: &Path,
    archive_id: &str,
) -> Result<(String, Vec<ExtractedFont>)> {
    // Matches: url(data:font/MIME;base64,DATA) or url("data:font/MIME;base64,DATA")
    // Also matches data:application/font-... for older MIME types.
    // Group 2 captures the full MIME type (e.g. "font/woff2"), group 3 the base64 payload.
    let re = Regex::new(
        r#"url\("?(data:((?:font|application)/[^;]+);base64,([A-Za-z0-9+/=]+))"?\)"#,
    )?;

    let mut fonts = Vec::new();
    let rewritten = re.replace_all(html, |caps: &regex::Captures| {
        let mime = caps[2].to_ascii_lowercase();
        let b64_data = &caps[3];

        let ext = match mime_to_ext(&mime) {
            Some(e) => e,
            None => return caps[0].to_string(), // unknown MIME — leave as-is
        };

        let bytes = match B64.decode(b64_data) {
            Ok(b) => b,
            Err(_) => return caps[0].to_string(), // corrupt base64 — leave as-is
        };

        let sha256 = hash_bytes(&bytes);
        let raw_relpath = font_raw_relpath(&sha256, ext);
        let abs_path = store_path.join(&raw_relpath);

        if !abs_path.exists() {
            if let Some(parent) = abs_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if fs::write(&abs_path, &bytes).is_err() {
                return caps[0].to_string(); // write failed — leave as-is
            }
        }

        fonts.push(ExtractedFont {
            sha256: sha256.clone(),
            ext: ext.to_string(),
            byte_size: bytes.len() as i64,
            raw_relpath,
        });

        format!("url(/api/archives/{archive_id}/blobs/{sha256})")
    });

    Ok((rewritten.into_owned(), fonts))
}

fn font_raw_relpath(sha256: &str, ext: &str) -> String {
    let mut chars = sha256.chars();
    let a = chars.next().unwrap_or('0');
    let b = chars.next().unwrap_or('0');
    format!("raw/{a}/{b}/{sha256}{ext}")
}

fn mime_to_ext(mime: &str) -> Option<&'static str> {
    match mime {
        "font/woff2" | "application/font-woff2" => Some(".woff2"),
        "font/woff"  | "application/font-woff"  => Some(".woff"),
        "font/ttf"   | "font/truetype" | "application/x-font-truetype" => Some(".ttf"),
        "font/otf"   | "font/opentype" | "application/x-font-opentype" => Some(".otf"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn html_with_woff2_font() -> String {
        let font_b64 = B64.encode(b"WOFF2FAKEDATA");
        format!(
            "<style>@font-face{{font-family:Test;\
             src:url(data:font/woff2;base64,{font_b64})}}</style>\
             <p>Hello</p>"
        )
    }

    #[test]
    fn replaces_data_url_with_api_url() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("raw")).unwrap();

        let html = html_with_woff2_font();
        let (rewritten, fonts) = extract_and_rewrite(&html, tmp.path(), "myarchive").unwrap();

        assert_eq!(fonts.len(), 1, "should extract one font");
        let font = &fonts[0];
        assert_eq!(font.ext, ".woff2");
        assert_eq!(font.byte_size, b"WOFF2FAKEDATA".len() as i64);
        assert!(!font.sha256.is_empty());
        assert!(!rewritten.contains("data:font"), "data URL must be gone from HTML");
        assert!(
            rewritten.contains(&format!("/api/archives/myarchive/blobs/{}", font.sha256)),
            "rewritten HTML must contain the local API URL"
        );
    }

    #[test]
    fn font_file_is_written_to_raw_store() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("raw")).unwrap();

        let html = html_with_woff2_font();
        let (_, fonts) = extract_and_rewrite(&html, tmp.path(), "myarchive").unwrap();

        let font = &fonts[0];
        let raw_path = tmp.path().join(&font.raw_relpath);
        assert!(raw_path.exists(), "font file must exist at raw_relpath");
        let written = std::fs::read(&raw_path).unwrap();
        assert_eq!(written, b"WOFF2FAKEDATA");
    }

    #[test]
    fn deduplicates_identical_fonts() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("raw")).unwrap();

        let font_b64 = B64.encode(b"SAMEDATA");
        let html = format!(
            "<style>\
             @font-face{{font-family:A;src:url(data:font/woff2;base64,{font_b64})}}\
             @font-face{{font-family:B;src:url(data:font/woff2;base64,{font_b64})}}\
             </style>"
        );
        let (_, fonts) = extract_and_rewrite(&html, tmp.path(), "x").unwrap();
        assert_eq!(fonts.len(), 2);
        assert_eq!(fonts[0].sha256, fonts[1].sha256);
        let raw_path = tmp.path().join(&fonts[0].raw_relpath);
        assert!(raw_path.exists());
    }

    #[test]
    fn html_without_font_data_urls_is_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("raw")).unwrap();

        let html = "<style>body { color: red; }</style><p>no fonts</p>";
        let (rewritten, fonts) = extract_and_rewrite(html, tmp.path(), "x").unwrap();
        assert_eq!(fonts.len(), 0);
        assert_eq!(rewritten, html);
    }

    #[test]
    fn ttf_font_gets_correct_extension() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("raw")).unwrap();

        let font_b64 = B64.encode(b"TTFDATA");
        let html = format!(
            "<style>@font-face{{src:url(data:font/ttf;base64,{font_b64})}}</style>"
        );
        let (_, fonts) = extract_and_rewrite(&html, tmp.path(), "x").unwrap();
        assert_eq!(fonts[0].ext, ".ttf");
    }
}
