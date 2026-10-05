//! Deno half of the shared yt-dlp tools update (CLI `archivr yt-dlp update` and the admin
//! UI): installs the latest official Deno release into
//! `<state_dir>/deno/deno`, the JS runtime yt-dlp uses to solve YouTube's challenges.
//!
//! The flow mirrors the yt-dlp zipapp install: download, extract into a staging file,
//! verify it runs (`--version` must report exactly the release version), then rename it
//! over the target so a concurrently-running archivr never sees a half-written binary.
//! The release `.sha256sum` is not checked: it comes from the same TLS origin as the zip,
//! and the zip's CRC32 already catches corruption.

use anyhow::{bail, Context, Result};
use super::js_runtime::{
    parse_deno_version_output, pinned_deno, probe_deno_version, state_dir_deno, DenoVersion,
    MIN_DENO_VERSION,
};
use std::{
    env, fs,
    io::{self, Cursor},
    path::Path,
    process::Command,
    time::Duration,
};

/// GitHub release metadata endpoint for the upstream Deno project.
pub const DENO_LATEST_RELEASE: &str =
    "https://api.github.com/repos/denoland/deno/releases/latest";

/// The Deno zip is ~40 MB; reqwest's blocking client defaults to a 30s total timeout,
/// which is too short on slow links. Applies to the zip download only.
const DENO_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// Why a staged prebuilt Deno can fail to spawn even though the file exists: the official
/// binaries are dynamically linked against a glibc loader NixOS doesn't provide.
const NO_LOADER: &str = "prebuilt deno cannot execute on this host \
     (missing dynamic loader — on NixOS enable programs.nix-ld)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenoRelease {
    pub tag: String,
    pub version: DenoVersion,
    pub download_url: String,
}

/// Official release asset for a `std::env::consts::{OS, ARCH}` pair.
pub fn deno_release_asset(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("deno-aarch64-apple-darwin.zip"),
        ("macos", "x86_64") => Some("deno-x86_64-apple-darwin.zip"),
        ("linux", "x86_64") => Some("deno-x86_64-unknown-linux-gnu.zip"),
        ("linux", "aarch64") => Some("deno-aarch64-unknown-linux-gnu.zip"),
        _ => None,
    }
}

/// Extracts the version and download URL from a GitHub "latest release" response.
/// The URL is built from the tag and asset name rather than taken from the response.
pub fn parse_deno_release(json: &serde_json::Value, asset: &str) -> Result<DenoRelease> {
    let tag = json
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .context("GitHub releases API response had no tag_name")?;
    // The tag ends up in a URL path; only accept plain version-ish characters.
    if tag.is_empty()
        || !tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
    {
        bail!("unexpected deno release tag {tag:?}");
    }
    let version = DenoVersion::parse(tag)
        .with_context(|| format!("could not parse a version from deno release tag {tag:?}"))?;
    let has_asset = json
        .get("assets")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|assets| {
            assets
                .iter()
                .any(|a| a.get("name").and_then(serde_json::Value::as_str) == Some(asset))
        });
    if !has_asset {
        bail!("deno release {tag} has no {asset} asset");
    }
    Ok(DenoRelease {
        tag: tag.to_string(),
        version,
        download_url: format!(
            "https://github.com/denoland/deno/releases/download/{tag}/{asset}"
        ),
    })
}

/// Installs or updates Deno in the state dir. `Ok` carries a one-line human outcome.
pub fn install_deno(client: &reqwest::blocking::Client, log: &mut dyn FnMut(&str)) -> Result<String> {
    let target = state_dir_deno().context("could not determine a state directory (is $HOME set?)")?;
    let dir = target
        .parent()
        .context("state-dir deno path has no parent directory")?;
    let staging = dir.join("deno.new");

    let (os, arch) = (env::consts::OS, env::consts::ARCH);
    let asset = deno_release_asset(os, arch)
        .with_context(|| format!("unsupported platform {os}/{arch}"))?;

    let body = client
        .get(DENO_LATEST_RELEASE)
        .send()
        .context("failed to reach the GitHub releases API")?
        .error_for_status()
        .context("GitHub releases API returned an error")?
        .text()
        .context("failed to read the GitHub releases API response")?;
    let json: serde_json::Value =
        serde_json::from_str(&body).context("GitHub releases API returned invalid JSON")?;
    let release = parse_deno_release(&json, asset)?;

    if let Some(installed) = probe_deno_version(&target).filter(|v| *v >= release.version) {
        return Ok(format!("deno {installed} already installed at {}", target.display()));
    }

    log(&format!("Downloading deno {}…", release.version));
    let bytes = client
        .get(&release.download_url)
        .timeout(DENO_DOWNLOAD_TIMEOUT)
        .send()
        .with_context(|| format!("failed to download {}", release.download_url))?
        .error_for_status()
        .with_context(|| format!("download of {} failed", release.download_url))?
        .bytes()
        .context("failed to read the downloaded deno zip")?;

    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;

    let staged = extract_deno(&bytes, &staging)
        .and_then(|()| verify_staged(&staging, release.version))
        .and_then(|runs| {
            if runs {
                fs::rename(&staging, &target)
                    .with_context(|| format!("failed to install {}", target.display()))?;
            }
            Ok(runs)
        });
    let runs = staged.inspect_err(|_| {
        let _ = fs::remove_file(&staging);
    })?;

    if !runs {
        let _ = fs::remove_file(&staging);
        let pinned_usable = pinned_deno()
            .and_then(|p| probe_deno_version(&p))
            .is_some_and(|v| v >= MIN_DENO_VERSION);
        if pinned_usable {
            return Ok(format!("skipped: {NO_LOADER}; using pinned ARCHIVR_DENO"));
        }
        bail!("{NO_LOADER}, and no usable pinned ARCHIVR_DENO is set");
    }

    Ok(format!("installed deno {} to {}", release.version, target.display()))
}

/// Writes the zip's `deno` entry to `staging` and makes it executable.
fn extract_deno(zip_bytes: &[u8], staging: &Path) -> Result<()> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(zip_bytes)).context("downloaded deno zip is invalid")?;
    let mut entry = archive
        .by_name("deno")
        .context("downloaded deno zip has no `deno` entry")?;
    let mut out = fs::File::create(staging)
        .with_context(|| format!("failed to create {}", staging.display()))?;
    io::copy(&mut entry, &mut out)
        .with_context(|| format!("failed to extract deno to {}", staging.display()))?;
    out.sync_all()
        .with_context(|| format!("failed to flush {}", staging.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(staging, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("failed to chmod +x {}", staging.display()))?;
    }
    Ok(())
}

/// Runs `staging --version` and requires it to report exactly `expected`.
///
/// `Ok(false)` means the binary exists but the OS could not execute it at all
/// (spawn failed with `NotFound` — e.g. the ELF interpreter is missing on NixOS);
/// any other failure is an error.
fn verify_staged(staging: &Path, expected: DenoVersion) -> Result<bool> {
    let output = match Command::new(staging).arg("--version").output() {
        Ok(output) => output,
        Err(e) if e.kind() == io::ErrorKind::NotFound && staging.is_file() => return Ok(false),
        Err(e) => {
            return Err(e).with_context(|| format!("failed to run {} --version", staging.display()));
        }
    };
    let got = output
        .status
        .success()
        .then(|| parse_deno_version_output(&String::from_utf8_lossy(&output.stdout)))
        .flatten();
    if got != Some(expected) {
        let got = got.map_or_else(|| "no parseable version".to_string(), |v| v.to_string());
        bail!("downloaded deno failed verification (expected {expected}, got {got})");
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{deno_release_asset, parse_deno_release, verify_staged};
    #[cfg(unix)]
    use crate::downloader::write_script;
    use crate::downloader::js_runtime::DenoVersion;
    use serde_json::json;

    #[test]
    fn supported_platforms_map_to_assets() {
        assert_eq!(
            deno_release_asset("macos", "aarch64"),
            Some("deno-aarch64-apple-darwin.zip")
        );
        assert_eq!(
            deno_release_asset("macos", "x86_64"),
            Some("deno-x86_64-apple-darwin.zip")
        );
        assert_eq!(
            deno_release_asset("linux", "x86_64"),
            Some("deno-x86_64-unknown-linux-gnu.zip")
        );
        assert_eq!(
            deno_release_asset("linux", "aarch64"),
            Some("deno-aarch64-unknown-linux-gnu.zip")
        );
    }

    #[test]
    fn unsupported_platforms_have_no_asset() {
        assert_eq!(deno_release_asset("windows", "x86_64"), None);
        assert_eq!(deno_release_asset("linux", "riscv64"), None);
    }

    #[test]
    fn release_json_yields_version_and_url() {
        let json = json!({
            "tag_name": "v2.9.7",
            "assets": [
                {"name": "deno-x86_64-unknown-linux-gnu.zip"},
                {"name": "deno-aarch64-apple-darwin.zip"},
            ],
        });
        let release = parse_deno_release(&json, "deno-aarch64-apple-darwin.zip").unwrap();
        assert_eq!(release.tag, "v2.9.7");
        assert_eq!(
            release.version,
            DenoVersion { major: 2, minor: 9, patch: 7 }
        );
        assert_eq!(
            release.download_url,
            "https://github.com/denoland/deno/releases/download/v2.9.7/deno-aarch64-apple-darwin.zip"
        );
    }

    #[test]
    fn missing_asset_is_an_error_naming_it() {
        let json = json!({
            "tag_name": "v2.9.7",
            "assets": [{"name": "deno-x86_64-unknown-linux-gnu.zip"}],
        });
        let err = parse_deno_release(&json, "deno-aarch64-apple-darwin.zip").unwrap_err();
        assert!(
            err.to_string().contains("deno-aarch64-apple-darwin.zip"),
            "{err:#}"
        );
    }

    #[test]
    fn missing_or_bad_tag_is_an_error() {
        let assets = json!([{"name": "deno-aarch64-apple-darwin.zip"}]);
        for json in [
            json!({"assets": assets}),
            json!({"tag_name": 297, "assets": assets}),
            json!({"tag_name": "nightly", "assets": assets}),
            json!({"tag_name": "v2.9.7/../../evil", "assets": assets}),
        ] {
            assert!(
                parse_deno_release(&json, "deno-aarch64-apple-darwin.zip").is_err(),
                "{json}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn staged_binary_must_report_the_release_version() {
        let tmp = tempfile::tempdir().unwrap();
        let expected = DenoVersion { major: 2, minor: 9, patch: 7 };

        // A fresh path per script: rewriting one path that was just exec'd invites ETXTBSY.
        let good = tmp.path().join("good/deno.new");
        write_script(&good, "#!/bin/sh\necho 'deno 2.9.7 (stable, release, test)'\n");
        assert!(verify_staged(&good, expected).unwrap());

        let wrong = tmp.path().join("wrong/deno.new");
        write_script(&wrong, "#!/bin/sh\necho 'deno 2.9.6 (stable, release, test)'\n");
        let err = verify_staged(&wrong, expected).unwrap_err();
        assert!(err.to_string().contains("expected 2.9.7, got 2.9.6"), "{err:#}");

        let failing = tmp.path().join("failing/deno.new");
        write_script(&failing, "#!/bin/sh\nexit 1\n");
        assert!(verify_staged(&failing, expected).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn existing_but_unexecutable_binary_is_reported_as_cannot_run() {
        // A script whose interpreter is missing fails to spawn with NotFound even though
        // the file exists — the same shape as a glibc ELF on NixOS without nix-ld.
        let tmp = tempfile::tempdir().unwrap();
        let staged = tmp.path().join("deno.new");
        write_script(&staged, "#!/nonexistent/ld-linux.so\n");
        let expected = DenoVersion { major: 2, minor: 9, patch: 7 };
        assert!(!verify_staged(&staged, expected).unwrap());

        // A genuinely missing file is an error, not "cannot execute".
        let missing = tmp.path().join("missing");
        assert!(verify_staged(&missing, expected).is_err());
    }
}
