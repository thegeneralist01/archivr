//! Browser session data forwarded from the extension's in-page capture dialog.
//!
//! `BrowserSession` carries cookies and localStorage from the user's real browser
//! tab so Archivr can archive the page as the authenticated user sees it.
//!
//! Security invariants:
//! - Names and values are never printed in error messages or logs.
//! - The struct's `Debug` impl redacts all names and values, showing only counts.
//! - The session is validated before use and is never persisted.

use std::{collections::HashMap, fmt};

/// Browser session forwarded from the extension's in-page capture dialog.
#[derive(Clone, serde::Deserialize)]
pub struct BrowserSession {
    /// The exact URL the session data was read from.
    pub url: String,
    /// Browser cookies for the page, name → value.
    #[serde(default)]
    pub cookies: HashMap<String, String>,
    /// Page localStorage entries, key → value.
    #[serde(default)]
    pub local_storage: HashMap<String, String>,
}

/// Redacting Debug: never prints names or values, only counts.
impl fmt::Debug for BrowserSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserSession")
            .field("url", &self.url)
            .field("cookies_count", &self.cookies.len())
            .field("local_storage_count", &self.local_storage.len())
            .finish()
    }
}

// ── Limits (mirrored in extension core.ts and frontend browserData.js) ───────

const MAX_COOKIES: usize = 300;
const MAX_COOKIE_NAME_BYTES: usize = 256;
const MAX_COOKIE_VALUE_BYTES: usize = 8192;
const MAX_COOKIES_TOTAL_BYTES: usize = 65536;
const MAX_STORAGE_ITEMS: usize = 2000;
const MAX_STORAGE_KEY_BYTES: usize = 1024;
const MAX_STORAGE_TOTAL_BYTES: usize = 1_048_576;
const MAX_URL_BYTES: usize = 10_000;

impl BrowserSession {
    /// Validates all limits and character rules for the session data.
    /// Returns an `Err(String)` containing the exact 400-error string on failure.
    pub fn validate(&self) -> Result<(), String> {
        // ── URL ──────────────────────────────────────────────────────────────
        if self.url.len() > MAX_URL_BYTES {
            return Err("browser_session.url must be an http(s) URL".to_string());
        }
        let parsed_url = reqwest::Url::parse(&self.url)
            .map_err(|_| "browser_session.url must be an http(s) URL".to_string())?;
        if !matches!(parsed_url.scheme(), "http" | "https") {
            return Err("browser_session.url must be an http(s) URL".to_string());
        }
        if parsed_url.host_str().is_none() {
            return Err("browser_session.url must be an http(s) URL".to_string());
        }

        // ── Cookies ──────────────────────────────────────────────────────────
        if self.cookies.len() > MAX_COOKIES {
            return Err(format!(
                "browser_session has too many cookies (max {MAX_COOKIES})"
            ));
        }
        let mut cookie_total_bytes: usize = 0;
        for (name, value) in &self.cookies {
            if name.is_empty()
                || name.len() > MAX_COOKIE_NAME_BYTES
                || name.bytes().any(|b| b < 0x20 || b == 0x7f || b == b' ' || b == b';' || b == b'=')
            {
                let display = truncate_debug(name, 64);
                return Err(format!(
                    "browser_session cookie {display} has an invalid name"
                ));
            }
            if value.len() > MAX_COOKIE_VALUE_BYTES
                || value.bytes().any(|b| b < 0x20 || b == 0x7f || b == b';')
            {
                let display = truncate_debug(name, 64);
                return Err(format!(
                    "browser_session cookie {display} has an invalid value"
                ));
            }
            cookie_total_bytes += name.len() + value.len();
        }
        if cookie_total_bytes > MAX_COOKIES_TOTAL_BYTES {
            return Err("browser_session cookies exceed 64 KiB".to_string());
        }

        // ── localStorage ─────────────────────────────────────────────────────
        if self.local_storage.len() > MAX_STORAGE_ITEMS {
            return Err(format!(
                "browser_session has too many localStorage items (max {MAX_STORAGE_ITEMS})"
            ));
        }
        let mut storage_total_bytes: usize = 0;
        for (key, value) in &self.local_storage {
            if key.is_empty() || key.len() > MAX_STORAGE_KEY_BYTES {
                let display = truncate_debug(key, 64);
                return Err(format!(
                    "browser_session localStorage key {display} is empty or longer than 1024 bytes"
                ));
            }
            storage_total_bytes += key.len() + value.len();
        }
        if storage_total_bytes > MAX_STORAGE_TOTAL_BYTES {
            return Err("browser_session localStorage exceeds 1 MiB".to_string());
        }

        Ok(())
    }

    /// Returns `true` when the session URL matches the target described by `locator`.
    ///
    /// - Tweet/TweetThread sources: session host must be X-family (`x.com`, `twitter.com`,
    ///   or a subdomain of either).
    /// - Everything else: the expanded locator URL must have the same exact hostname as
    ///   the session URL.
    pub fn applies_to(&self, locator: &str) -> bool {
        use crate::{
            capture::{determine_source, expand_shorthand_to_url, Source},
            downloader::cookies::domain_from_url,
        };

        let source = determine_source(locator);
        match source {
            Source::Tweet | Source::TweetThread => {
                let host = domain_from_url(&self.url).to_lowercase();
                is_x_family_host(&host)
            }
            _ => {
                let locator_url = expand_shorthand_to_url(locator, &source);
                let locator_host = domain_from_url(&locator_url).to_lowercase();
                let session_host = domain_from_url(&self.url).to_lowercase();
                !locator_host.is_empty() && locator_host == session_host
            }
        }
    }

    /// Returns the ASCII-serialized origin of `url`, e.g. `"https://www.example.com"`.
    pub fn storage_origin(&self) -> Option<String> {
        reqwest::Url::parse(&self.url)
            .ok()
            .map(|u| u.origin().ascii_serialization())
    }
}

/// True when `host` is exactly `x.com` or `twitter.com`, or a subdomain of either.
fn is_x_family_host(host: &str) -> bool {
    host == "x.com"
        || host == "twitter.com"
        || host.ends_with(".x.com")
        || host.ends_with(".twitter.com")
}

/// Extends `base` cookies with the browser session's cookies.
/// Session values win on name collision (browser cookie overrides rule cookie).
pub(crate) fn merge_cookies(
    mut base: HashMap<String, String>,
    session: Option<&BrowserSession>,
) -> HashMap<String, String> {
    if let Some(s) = session {
        for (k, v) in &s.cookies {
            base.insert(k.clone(), v.clone());
        }
    }
    base
}

/// Returns `format!("{:?}", s)` truncated to `max_chars` chars (with `…` appended if cut).
fn truncate_debug(s: &str, max_chars: usize) -> String {
    let repr = format!("{s:?}");
    let chars: Vec<char> = repr.chars().collect();
    if chars.len() > max_chars {
        let truncated: String = chars[..max_chars].iter().collect();
        format!("{truncated}…")
    } else {
        repr
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session(url: &str) -> BrowserSession {
        BrowserSession {
            url: url.to_string(),
            cookies: HashMap::new(),
            local_storage: HashMap::new(),
        }
    }

    // ── applies_to ────────────────────────────────────────────────────────────

    #[test]
    fn applies_to_same_host() {
        let s = make_session("https://www.example.com/a");
        assert!(s.applies_to("https://www.example.com/b"));
    }

    #[test]
    fn applies_to_different_subdomain_is_false() {
        let s = make_session("https://www.example.com/a");
        assert!(!s.applies_to("https://example.com/b"));
    }

    #[test]
    fn applies_to_twitter_session_with_tweet_locator() {
        let s = make_session("https://twitter.com/home");
        // tweet:tweet:123 → Source::Tweet
        assert!(s.applies_to("tweet:tweet:123456789012345678"));
    }

    #[test]
    fn applies_to_x_session_with_tweet_locator() {
        let s = make_session("https://x.com/home");
        assert!(s.applies_to("x:123456789012345678"));
    }

    #[test]
    fn applies_to_non_x_session_with_tweet_locator_is_false() {
        let s = make_session("https://example.com/");
        assert!(!s.applies_to("tweet:123456789012345678"));
    }

    #[test]
    fn applies_to_youtube_session_with_yt_shorthand() {
        let s = make_session("https://www.youtube.com/feed");
        assert!(s.applies_to("yt:dQw4w9WgXcQ"));
    }

    #[test]
    fn applies_to_youtube_session_with_wrong_shorthand_is_false() {
        let s = make_session("https://www.youtube.com/feed");
        // ytm: expands to music.youtube.com, not www.youtube.com
        assert!(!s.applies_to("ytm:dQw4w9WgXcQ"));
    }

    // ── validate ──────────────────────────────────────────────────────────────

    #[test]
    fn validate_rejects_name_with_space() {
        let mut s = make_session("https://example.com/");
        s.cookies.insert("a b".to_string(), "value".to_string());
        let err = s.validate().unwrap_err();
        assert!(err.contains("invalid name"), "got: {err}");
        assert!(!err.contains("value"), "must not leak value: {err}");
    }

    #[test]
    fn validate_rejects_value_with_newline() {
        let mut s = make_session("https://example.com/");
        s.cookies.insert("name".to_string(), "val\nue".to_string());
        let err = s.validate().unwrap_err();
        assert!(err.contains("invalid value"), "got: {err}");
        assert!(!err.contains("val\nue"), "must not leak value: {err}");
    }

    #[test]
    fn validate_rejects_too_many_cookies() {
        let mut s = make_session("https://example.com/");
        for i in 0..=300 {
            s.cookies.insert(format!("k{i}"), "v".to_string());
        }
        let err = s.validate().unwrap_err();
        assert!(err.contains("too many cookies"), "got: {err}");
    }

    #[test]
    fn validate_rejects_oversized_storage() {
        let mut s = make_session("https://example.com/");
        // 1 MiB + 1 byte total
        let large_val = "x".repeat(MAX_STORAGE_TOTAL_BYTES + 1);
        s.local_storage.insert("k".to_string(), large_val);
        let err = s.validate().unwrap_err();
        assert!(err.contains("localStorage exceeds 1 MiB"), "got: {err}");
    }

    #[test]
    fn validate_accepts_realistic_map() {
        let mut s = make_session("https://example.com/");
        s.cookies.insert("session_id".to_string(), "abc123xyz".to_string());
        s.cookies.insert("theme".to_string(), "dark".to_string());
        s.local_storage.insert("pref".to_string(), "1".to_string());
        assert!(s.validate().is_ok());
    }

    // ── merge_cookies ─────────────────────────────────────────────────────────

    #[test]
    fn merge_cookies_session_wins_on_collision() {
        let mut base = HashMap::new();
        base.insert("session".to_string(), "old_value".to_string());
        base.insert("other".to_string(), "kept".to_string());

        let mut s = make_session("https://example.com/");
        s.cookies.insert("session".to_string(), "new_value".to_string());

        let merged = merge_cookies(base, Some(&s));
        assert_eq!(merged["session"], "new_value");
        assert_eq!(merged["other"], "kept");
    }

    #[test]
    fn merge_cookies_with_no_session_returns_base() {
        let mut base = HashMap::new();
        base.insert("k".to_string(), "v".to_string());
        let merged = merge_cookies(base.clone(), None);
        assert_eq!(merged, base);
    }

    // ── debug redaction ───────────────────────────────────────────────────────

    #[test]
    fn debug_does_not_leak_cookie_values() {
        let mut s = make_session("https://example.com/");
        s.cookies
            .insert("session".to_string(), "super_secret_token".to_string());
        s.local_storage
            .insert("key".to_string(), "stored_secret".to_string());
        let repr = format!("{s:?}");
        assert!(!repr.contains("super_secret_token"), "value leaked in Debug: {repr}");
        assert!(!repr.contains("stored_secret"), "value leaked in Debug: {repr}");
    }
}
