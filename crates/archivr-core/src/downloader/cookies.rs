use std::{collections::HashMap, io::Write, path::Path};
use serde_json::json;

/// Writes a Netscape-format HTTP cookie file suitable for yt-dlp `--cookies` and
/// single-file `--browser-cookies-file`. The file is created with mode 0o600
/// (owner read/write only) to protect cookie secrets.
///
/// `domain` is the exact hostname of the target URL (as returned by
/// `domain_from_url`). No leading dot is added — cookies are scoped to the
/// exact host only, not all subdomains. This is intentionally conservative:
/// `www.youtube.com` cookies will not leak to `music.youtube.com`.
pub fn write_netscape_cookie_file(
    cookies: &HashMap<String, String>,
    domain: &str,
    path: &Path,
) -> std::io::Result<()> {
    #[cfg(unix)]
    let mut f = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?
    };
    #[cfg(not(unix))]
    let mut f = std::fs::File::create(path)?;

    writeln!(f, "# Netscape HTTP Cookie File")?;
    // Exact hostname — no leading dot, no subdomain wildcard.
    // The user controls scope via their URL patterns.
    let cookie_domain = domain.to_string();
    for (name, value) in cookies {
        // Fields: domain  include_subdomains  path  secure  expiry  name  value
        // include_subdomains is FALSE because there is no leading dot.
        writeln!(f, "{cookie_domain}\tFALSE\t/\tFALSE\t0\t{name}\t{value}")?;
    }
    Ok(())
}

/// Writes a JSON-format cookie file for single-file `--browser-cookies-file`.
///
/// Single-file accepts both Netscape text and JSON; JSON gives us per-cookie
/// field control, which is required for `__Host-`/`__Secure-` cookies.
///
/// - `__Host-` cookies: Chrome CDP `Network.setCookies` rejects these when a
///   `domain` attribute is present.  They must be set via `url` (no `domain`),
///   `path: "/"`, and `secure: true`.
/// - `__Secure-` cookies: require `secure: true`; `domain` is still fine.
/// - All other cookies: use `domain` as before; `secure` left false because
///   we have no per-cookie scheme information from the user's input map.
///
/// The file is created 0o600 (owner read/write only) to protect cookie secrets.
pub fn write_json_cookie_file(
    cookies: &HashMap<String, String>,
    domain: &str,
    page_url: &str,
    path: &Path,
) -> std::io::Result<()> {
    #[cfg(unix)]
    let mut f = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?
    };
    #[cfg(not(unix))]
    let mut f = std::fs::File::create(path)?;

    // Build a canonical https root URL for __Host- cookies (must be https).
    // If the page_url itself is https we use it directly; otherwise synthesise one.
    let host_url = if page_url.starts_with("https://") {
        page_url.to_string()
    } else {
        format!("https://{}/", domain)
    };

    let entries: Vec<serde_json::Value> = cookies
        .iter()
        .map(|(name, value)| {
            if name.starts_with("__Host-") {
                // No `domain` field — Chrome rejects __Host- cookies that carry one.
                json!({
                    "name": name,
                    "value": value,
                    "url": host_url,
                    "path": "/",
                    "secure": true
                })
            } else if name.starts_with("__Secure-") {
                json!({
                    "name": name,
                    "value": value,
                    "domain": domain,
                    "path": "/",
                    "secure": true
                })
            } else {
                json!({
                    "name": name,
                    "value": value,
                    "domain": domain,
                    "path": "/"
                })
            }
        })
        .collect();

    let json_str = serde_json::to_string(&entries)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    f.write_all(json_str.as_bytes())?;
    Ok(())
}


/// Extracts the exact hostname from a URL, suitable for use as the Netscape
/// cookie domain.
///
/// Uses `reqwest::Url::parse` so IPv6 addresses, auth-in-URL, ports, and
/// non-http schemes are handled correctly. Returns an empty string on parse
/// failure or when the URL has no host component.
///
/// Examples:
/// - `"https://www.youtube.com/watch?v=x"` → `"www.youtube.com"`
/// - `"https://music.youtube.com/"` → `"music.youtube.com"`
/// - `"https://twitter.com/"` → `"twitter.com"`
pub fn domain_from_url(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default()
}

/// Formats cookies as a single `Cookie:` header value ("name=value; name2=value2").
pub fn cookies_to_header(cookies: &HashMap<String, String>) -> String {
    cookies
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn read_json(f: &NamedTempFile) -> Vec<serde_json::Value> {
        let s = std::fs::read_to_string(f.path()).unwrap();
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn host_prefix_cookie_has_no_domain_uses_url() {
        let mut cookies = HashMap::new();
        cookies.insert("__Host-session".to_string(), "abc123".to_string());
        let f = NamedTempFile::new().unwrap();
        write_json_cookie_file(
            &cookies,
            "example.com",
            "https://example.com/page",
            f.path(),
        )
        .unwrap();
        let entries = read_json(&f);
        assert_eq!(entries.len(), 1);
        let c = &entries[0];
        assert_eq!(c["name"], "__Host-session");
        assert_eq!(c["value"], "abc123");
        assert!(c.get("domain").is_none(), "__Host- must not have domain field");
        assert_eq!(c["url"], "https://example.com/page");
        assert_eq!(c["path"], "/");
        assert_eq!(c["secure"], true);
    }

    #[test]
    fn secure_prefix_cookie_has_domain_and_secure_true() {
        let mut cookies = HashMap::new();
        cookies.insert("__Secure-token".to_string(), "xyz".to_string());
        let f = NamedTempFile::new().unwrap();
        write_json_cookie_file(
            &cookies,
            "example.com",
            "https://example.com/",
            f.path(),
        )
        .unwrap();
        let entries = read_json(&f);
        let c = &entries[0];
        assert_eq!(c["domain"], "example.com");
        assert_eq!(c["secure"], true);
        assert!(c.get("url").is_none(), "__Secure- should not carry url field");
    }

    #[test]
    fn regular_cookie_uses_domain_no_secure() {
        let mut cookies = HashMap::new();
        cookies.insert("cf_bm".to_string(), "val".to_string());
        let f = NamedTempFile::new().unwrap();
        write_json_cookie_file(
            &cookies,
            "example.com",
            "https://example.com/",
            f.path(),
        )
        .unwrap();
        let entries = read_json(&f);
        let c = &entries[0];
        assert_eq!(c["domain"], "example.com");
        assert!(c.get("secure").map(|v| v != true).unwrap_or(true), "regular cookie should not be forced secure");
        assert!(c.get("url").is_none());
    }

    #[test]
    fn host_prefix_falls_back_to_https_url_when_page_url_is_http() {
        let mut cookies = HashMap::new();
        cookies.insert("__Host-x".to_string(), "1".to_string());
        let f = NamedTempFile::new().unwrap();
        write_json_cookie_file(
            &cookies,
            "marketing.creativedirector.net",
            "http://marketing.creativedirector.net/course",
            f.path(),
        )
        .unwrap();
        let entries = read_json(&f);
        let c = &entries[0];
        assert!(c["url"].as_str().unwrap().starts_with("https://"), "must use https url for __Host- even when page is http");
        assert!(c.get("domain").is_none());
    }
}
