use std::{collections::HashMap, io::Write, path::Path};

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
