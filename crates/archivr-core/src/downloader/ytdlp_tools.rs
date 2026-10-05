//! yt-dlp + Deno self-update and status shared by `archivr yt-dlp` and the admin API.
//! Sync; network via blocking reqwest.

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

use super::deno_install;
use super::js_runtime::{
    forced_js_runtime, path_deno, pinned_deno, probe_deno_version, probe_js_runtime_version,
    refresh_js_runtime, resolve_js_runtime_with_role, state_dir_deno, JsRuntimeRole,
    JS_RUNTIME_ENV,
};
use super::ytdlp::{
    forced_yt_dlp, pinned_yt_dlp, probe_version, refresh_yt_dlp, resolve_yt_dlp, state_dir,
    state_dir_yt_dlp,
};

/// GitHub release metadata endpoint for the upstream yt-dlp project.
pub const YT_DLP_LATEST_RELEASE: &str =
    "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest";

/// Every python zipapp starts with this shebang; used as a sanity check that we
/// downloaded the artifact and not an HTML error page or an LFS pointer.
const ZIPAPP_SHEBANG: &[u8] = b"#!/usr/bin/env python3";

/// One candidate slot of a `status` table.
#[derive(Debug, Clone, Serialize)]
pub struct ToolCandidate {
    /// Stable key: "force" | "env" | "state-dir" | "path".
    pub role: &'static str,
    /// Exact CLI row label.
    pub label: &'static str,
    /// `None` = empty slot (the CLI renders dashes).
    pub path: Option<String>,
    pub version: Option<String>,
    pub chosen: bool,
    /// Why the candidate can't be used: an invalid `ARCHIVR_JS_RUNTIME`, or a yt-dlp
    /// candidate that exists but whose `--version` probe fails (last stderr line).
    pub invalid: Option<String>,
}

/// The candidate the resolver picked.
#[derive(Debug, Clone, Serialize)]
pub struct ChosenTool {
    /// `None` if the cached yt-dlp path matches no row.
    pub role: Option<&'static str>,
    /// JS only: "deno" | "node" | "bun" | "quickjs".
    pub kind: Option<&'static str>,
    pub path: Option<String>,
    pub version: Option<String>,
}

/// Everything `archivr yt-dlp status` prints, as data.
#[derive(Debug, Clone, Serialize)]
pub struct ToolsStatus {
    /// force, env, state-dir, path-fallback (CLI order).
    pub yt_dlp: Vec<ToolCandidate>,
    pub yt_dlp_chosen: ChosenTool,
    /// force, env (ARCHIVR_DENO), state-dir, path (deno).
    pub js_runtime: Vec<ToolCandidate>,
    pub js_runtime_chosen: Option<ChosenTool>,
    pub state_dir: Option<String>,
    pub yt_dlp_target: Option<String>,
    pub yt_dlp_installed: bool,
    pub deno_target: Option<String>,
    pub deno_installed: bool,
}

/// Per-component outcome of [`update_tools`]; `Ok` carries a one-line human outcome.
pub struct UpdateReport {
    pub yt_dlp: Result<String>,
    pub deno: Result<String>,
}

impl UpdateReport {
    /// Names of the failed components, in `["yt-dlp", "deno"]` order.
    pub fn failed_components(&self) -> Vec<&'static str> {
        [("yt-dlp", self.yt_dlp.is_err()), ("deno", self.deno.is_err())]
            .into_iter()
            .filter_map(|(name, failed)| failed.then_some(name))
            .collect()
    }
}

/// Resolves `<state_dir>/yt-dlp/`, erroring out if there is no usable HOME.
fn yt_dlp_state_dir() -> Result<PathBuf> {
    state_dir()
        .map(|d| d.join("yt-dlp"))
        .context("could not determine a state directory (is $HOME set?)")
}

/// Asks the GitHub API for the newest yt-dlp release tag.
fn latest_yt_dlp_version(client: &reqwest::blocking::Client) -> Result<String> {
    let body = client
        .get(YT_DLP_LATEST_RELEASE)
        .send()
        .context("failed to reach the GitHub releases API")?
        .error_for_status()
        .context("GitHub releases API returned an error")?
        .text()
        .context("failed to read the GitHub releases API response")?;

    let json: serde_json::Value =
        serde_json::from_str(&body).context("GitHub releases API returned invalid JSON")?;

    json.get("tag_name")
        .and_then(|t| t.as_str())
        .map(str::to_string)
        .context("GitHub releases API response had no tag_name")
}

/// Installs or updates the yt-dlp zipapp in the state dir. Progress goes to `log`;
/// `Ok` carries a one-line human outcome.
pub fn install_yt_dlp(
    client: &reqwest::blocking::Client,
    requested_version: Option<&str>,
    log: &mut dyn FnMut(&str),
) -> Result<String> {
    let dir = yt_dlp_state_dir()?;
    let target = dir.join("yt-dlp");
    let staging = dir.join("yt-dlp.new");
    let version_file = dir.join(".version");

    let version = match requested_version {
        Some(v) => v.to_string(),
        None => latest_yt_dlp_version(client)?,
    };

    // The sibling .version file is what lets us skip a ~3MB download on a
    // no-op update; the binary itself is a zipapp with no cheap version probe
    // that doesn't cost a python startup.
    let installed = std::fs::read_to_string(&version_file).ok();
    if target.is_file() && installed.as_deref().map(str::trim) == Some(version.as_str()) {
        log(&format!("yt-dlp {version} is already installed at {}", target.display()));
        return Ok(format!("yt-dlp {version} already installed at {}", target.display()));
    }

    log(&format!("Downloading yt-dlp {version}…"));
    let url = format!("https://github.com/yt-dlp/yt-dlp/releases/download/{version}/yt-dlp");
    let bytes = client
        .get(&url)
        .send()
        .with_context(|| format!("failed to download {url}"))?
        .error_for_status()
        .with_context(|| format!("download failed — is {version} a real release tag?"))?
        .bytes()
        .context("failed to read the downloaded yt-dlp body")?;

    if !bytes.starts_with(ZIPAPP_SHEBANG) {
        bail!(
            "downloaded artifact from {url} is not a python zipapp \
             (expected it to start with `{}`) — refusing to install it",
            String::from_utf8_lossy(ZIPAPP_SHEBANG)
        );
    }

    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create {}", dir.display()))?;
    std::fs::write(&staging, &bytes)
        .with_context(|| format!("failed to write {}", staging.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("failed to chmod +x {}", staging.display()))?;
    }

    // Atomic swap: a concurrently-running archivr sees either the whole old
    // binary or the whole new one, never a half-written file.
    std::fs::rename(&staging, &target)
        .with_context(|| format!("failed to install {}", target.display()))?;
    std::fs::write(&version_file, format!("{version}\n"))
        .with_context(|| format!("failed to record version in {}", version_file.display()))?;

    // The zipapp is python source, not a native binary — installing it on a
    // host without python3 is legal (the server may run under a nix wrapper
    // with its own PATH) but worth flagging loudly. Kept as `warning:` (not
    // `warn:`) so the CLI's stderr is unchanged.
    let has_python = Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has_python {
        eprintln!(
            "warning: python3 was not found on PATH — the yt-dlp zipapp just installed \
             at {} will not run until python3 is available",
            target.display()
        );
    }
    let python_note = (!has_python)
        .then_some("; warning: python3 not found on PATH — the zipapp will not run until it is");

    log(&format!("Installed yt-dlp {version} to {}", target.display()));
    log("archivr will now prefer it whenever it is newer than the pinned binary (ARCHIVR_YT_DLP).");

    Ok(format!(
        "installed yt-dlp {version} to {}{}",
        target.display(),
        python_note.unwrap_or("")
    ))
}

/// Hint appended when the installed zipapp does not run; the zipapp is python source.
const PYTHON_HINT: &str = "yt-dlp needs Python ≥ 3.10 on the server's PATH as `python3`";

/// Installs yt-dlp and Deno independently: a Deno failure never blocks the yt-dlp
/// update (and vice versa). `Err` only if the HTTP client cannot be built.
///
/// With `refresh` (long-running server), the installed yt-dlp is probed with
/// `--version` — an install that does not run is reported as a failure naming the
/// cause — and each successful component refreshes its resolver cache, so the next
/// yt-dlp call uses the new binary. The one-shot CLI passes `false`: it has no cache
/// worth refreshing, and its output and probe count stay as before.
pub fn update_tools(
    requested_yt_dlp_version: Option<&str>,
    user_agent: &str,
    refresh: bool,
    log: &mut dyn FnMut(&str),
) -> Result<UpdateReport> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(user_agent)
        .build()
        .context("failed to build an HTTP client")?;

    let mut yt_dlp = install_yt_dlp(&client, requested_yt_dlp_version, log);
    if refresh && yt_dlp.is_ok() {
        if let Some(target) = state_dir_yt_dlp() {
            if let Err(Some(reason)) = probe_version_detail(&target) {
                yt_dlp = Err(anyhow!(unusable_install_message(&target, &reason)));
            }
        }
        refresh_yt_dlp();
    }
    let deno = deno_install::install_deno(&client, log);
    if refresh && deno.is_ok() {
        refresh_js_runtime();
    }
    Ok(UpdateReport { yt_dlp, deno })
}

/// Error text for an installed yt-dlp whose `--version` probe failed.
fn unusable_install_message(target: &Path, reason: &str) -> String {
    format!(
        "installed {} but it does not run: {reason} — {PYTHON_HINT}",
        target.display()
    )
}

/// Short reason for a failed `--version` run: the last non-empty stderr line (a Python
/// traceback ends with the actual error), else the exit status.
fn probe_failure_reason(stderr: &[u8], status: &str) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map_or_else(|| format!("--version failed ({status})"), str::to_string)
}

/// Runs `<binary> --version`. `Err(None)` = nothing to run (not found); `Err(Some(reason))`
/// = the binary exists but the probe failed.
fn probe_version_detail(binary: &Path) -> std::result::Result<String, Option<String>> {
    let out = match Command::new(binary).arg("--version").output() {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(None),
        Err(e) => return Err(Some(format!("could not run: {e}"))),
    };
    if !out.status.success() {
        return Err(Some(probe_failure_reason(&out.stderr, &out.status.to_string())));
    }
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if version.is_empty() {
        return Err(Some("--version printed nothing".into()));
    }
    Ok(version)
}

fn display(p: &Path) -> String {
    p.display().to_string()
}

/// One yt-dlp row, probing the candidate's version; a failing probe sets `invalid`.
fn yt_row(role: &'static str, label: &'static str, path: Option<&Path>, chosen: &Path) -> ToolCandidate {
    let (version, invalid) = match path.map(probe_version_detail) {
        Some(Ok(v)) => (Some(v), None),
        Some(Err(reason)) => (None, reason),
        None => (None, None),
    };
    ToolCandidate {
        role,
        label,
        path: path.map(display),
        version,
        chosen: path == Some(chosen),
        invalid,
    }
}

/// Every yt-dlp and JS runtime candidate, its version, and which one wins — the data
/// `archivr yt-dlp status` prints. Spawns `--version` probes; call off async threads.
pub fn tools_status() -> ToolsStatus {
    // yt-dlp: the cached choice, i.e. what this process actually runs.
    let chosen = resolve_yt_dlp();
    let state_candidate = state_dir_yt_dlp().filter(|p| p.is_file());
    let yt_dlp = vec![
        yt_row("force", "force (ARCHIVR_YT_DLP_FORCE)", forced_yt_dlp().as_deref(), &chosen),
        yt_row("env", "env (ARCHIVR_YT_DLP)", pinned_yt_dlp().as_deref(), &chosen),
        yt_row("state-dir", "state-dir", state_candidate.as_deref(), &chosen),
        yt_row("path", "path-fallback (yt-dlp)", Some(Path::new("yt-dlp")), &chosen),
    ];
    let yt_dlp_chosen = match yt_dlp.iter().find(|c| c.chosen) {
        Some(c) => ChosenTool {
            role: Some(c.role),
            kind: None,
            path: Some(display(&chosen)),
            version: c.version.clone(),
        },
        None => ChosenTool {
            role: None,
            kind: None,
            path: Some(display(&chosen)),
            version: probe_version(&chosen),
        },
    };

    // JS runtime: uncached and silent, so status never prints the resolver warnings.
    let js_chosen = resolve_js_runtime_with_role();
    let chosen_role = js_chosen.as_ref().map(|(role, _)| *role);
    let force_role = JsRuntimeRole::Forced;
    let force_row = match forced_js_runtime() {
        Ok(Some(rt)) => ToolCandidate {
            role: force_role.key(),
            label: force_role.label(),
            path: Some(rt.spec().to_string_lossy().into_owned()),
            version: probe_js_runtime_version(&rt),
            chosen: chosen_role == Some(force_role),
            invalid: None,
        },
        Ok(None) => ToolCandidate {
            role: force_role.key(),
            label: force_role.label(),
            path: None,
            version: None,
            chosen: false,
            invalid: None,
        },
        Err(reason) => ToolCandidate {
            role: force_role.key(),
            label: force_role.label(),
            path: Some(
                env::var_os(JS_RUNTIME_ENV)
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            ),
            version: None,
            chosen: false,
            invalid: Some(reason),
        },
    };
    let deno_row = |role: JsRuntimeRole, path: Option<PathBuf>| ToolCandidate {
        role: role.key(),
        label: role.label(),
        version: path
            .as_deref()
            .and_then(probe_deno_version)
            .map(|v| v.to_string()),
        path: path.as_deref().map(display),
        chosen: chosen_role == Some(role),
        invalid: None,
    };
    let js_runtime = vec![
        force_row,
        deno_row(JsRuntimeRole::Pinned, pinned_deno()),
        deno_row(JsRuntimeRole::StateDir, state_dir_deno().filter(|p| p.is_file())),
        deno_row(JsRuntimeRole::Path, path_deno()),
    ];
    let js_runtime_chosen = js_chosen.map(|(role, rt)| ChosenTool {
        role: Some(role.key()),
        kind: Some(rt.kind.as_str()),
        path: rt.path.as_deref().map(display),
        version: js_runtime
            .iter()
            .find(|c| c.chosen)
            .and_then(|c| c.version.clone()),
    });

    let yt_dlp_target = state_dir_yt_dlp();
    let deno_target = state_dir_deno();
    ToolsStatus {
        yt_dlp,
        yt_dlp_chosen,
        js_runtime,
        js_runtime_chosen,
        state_dir: state_dir().as_deref().map(display),
        yt_dlp_installed: yt_dlp_target.as_deref().is_some_and(Path::is_file),
        yt_dlp_target: yt_dlp_target.as_deref().map(display),
        deno_installed: deno_target.as_deref().is_some_and(Path::is_file),
        deno_target: deno_target.as_deref().map(display),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::downloader::js_runtime::DENO_ENV;
    use crate::downloader::ytdlp::{STATE_DIR_ENV, YT_DLP_ENV, YT_DLP_FORCE_ENV};
    use anyhow::anyhow;

    const RESOLVER_ENVS: [&str; 5] =
        [YT_DLP_FORCE_ENV, YT_DLP_ENV, STATE_DIR_ENV, JS_RUNTIME_ENV, DENO_ENV];

    #[cfg(unix)]
    #[test]
    fn tools_status_reports_forced_and_invalid_override() {
        let _guard = crate::downloader::RESOLVER_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for key in RESOLVER_ENVS {
            unsafe { env::remove_var(key) };
        }
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let forced = tmp.path().join("forced/yt-dlp");
        crate::downloader::write_script(&forced, "#!/bin/sh\necho 2020.01.01\n");
        unsafe {
            env::set_var(STATE_DIR_ENV, &state);
            env::set_var(YT_DLP_FORCE_ENV, &forced);
            env::set_var(JS_RUNTIME_ENV, "python");
        }
        refresh_yt_dlp();

        let status = tools_status();
        assert_eq!(status.yt_dlp[0].role, "force");
        assert!(status.yt_dlp[0].chosen);
        assert_eq!(status.yt_dlp[0].version.as_deref(), Some("2020.01.01"));
        assert_eq!(status.yt_dlp_chosen.role, Some("force"));
        let js_force = &status.js_runtime[0];
        assert!(
            js_force
                .invalid
                .as_deref()
                .is_some_and(|r| r.contains("unknown runtime python")),
            "{js_force:?}"
        );
        assert!(!js_force.chosen);
        assert!(!status.yt_dlp_installed);
        assert!(status
            .yt_dlp_target
            .as_deref()
            .is_some_and(|t| t.ends_with("yt-dlp/yt-dlp")));
        let json = serde_json::to_value(&status).unwrap();
        for key in ["yt_dlp", "js_runtime", "state_dir", "deno_target"] {
            assert!(json.get(key).is_some(), "missing {key}");
        }

        for key in RESOLVER_ENVS {
            unsafe { env::remove_var(key) };
        }
        refresh_yt_dlp();
    }

    #[test]
    fn probe_failure_reason_prefers_last_stderr_line() {
        assert_eq!(
            probe_failure_reason(
                b"Traceback (most recent call last):\n  File \"yt_dlp/__main__.py\", line 13\n\
                  ImportError: You are using an unsupported version of Python. Only Python \
                  versions 3.10 and above are supported by yt-dlp\n\n",
                "exit status: 1"
            ),
            "ImportError: You are using an unsupported version of Python. Only Python \
             versions 3.10 and above are supported by yt-dlp"
        );
        assert_eq!(
            probe_failure_reason(b"\n  boom: too old  \n", "exit status: 1"),
            "boom: too old"
        );
        assert_eq!(
            probe_failure_reason(b"  \n", "exit status: 2"),
            "--version failed (exit status: 2)"
        );
    }

    #[test]
    fn unusable_install_message_names_cause_and_hint() {
        let msg = unusable_install_message(Path::new("/s/yt-dlp/yt-dlp"), "Only Python 3.10+");
        assert!(msg.contains("/s/yt-dlp/yt-dlp"), "{msg}");
        assert!(msg.contains("Only Python 3.10+"), "{msg}");
        assert!(msg.contains("Python ≥ 3.10"), "{msg}");
    }

    #[cfg(unix)]
    #[test]
    fn probe_version_detail_classifies_outcomes() {
        let tmp = tempfile::tempdir().unwrap();
        let ok = tmp.path().join("ok");
        crate::downloader::write_script(&ok, "#!/bin/sh\necho 2024.01.01\n");
        assert_eq!(probe_version_detail(&ok), Ok("2024.01.01".into()));
        let bad = tmp.path().join("bad");
        crate::downloader::write_script(
            &bad,
            "#!/bin/sh\necho 'Only Python versions 3.10 and above are supported' >&2\nexit 1\n",
        );
        assert_eq!(
            probe_version_detail(&bad),
            Err(Some("Only Python versions 3.10 and above are supported".into()))
        );
        assert_eq!(probe_version_detail(&tmp.path().join("missing")), Err(None));
    }

    #[cfg(unix)]
    #[test]
    fn tools_status_reports_unusable_candidate_reason() {
        let _guard = crate::downloader::RESOLVER_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for key in RESOLVER_ENVS {
            unsafe { env::remove_var(key) };
        }
        let tmp = tempfile::tempdir().unwrap();
        let pinned = tmp.path().join("pinned/yt-dlp");
        crate::downloader::write_script(&pinned, "#!/bin/sh\necho 'boom: too old' >&2\nexit 1\n");
        unsafe {
            env::set_var(STATE_DIR_ENV, tmp.path().join("state"));
            env::set_var(YT_DLP_ENV, &pinned);
        }
        refresh_yt_dlp();

        let status = tools_status();
        let env_row = &status.yt_dlp[1];
        assert_eq!(env_row.role, "env");
        assert_eq!(env_row.version, None);
        assert_eq!(env_row.invalid.as_deref(), Some("boom: too old"));

        for key in RESOLVER_ENVS {
            unsafe { env::remove_var(key) };
        }
        refresh_yt_dlp();
    }

    #[test]
    fn failed_components_lists_only_failures() {
        let report = |y: bool, d: bool| UpdateReport {
            yt_dlp: if y { Ok("ok".into()) } else { Err(anyhow!("boom")) },
            deno: if d { Ok("ok".into()) } else { Err(anyhow!("boom")) },
        };
        assert!(report(true, true).failed_components().is_empty());
        assert_eq!(report(false, true).failed_components(), ["yt-dlp"]);
        assert_eq!(report(true, false).failed_components(), ["deno"]);
        assert_eq!(report(false, false).failed_components(), ["yt-dlp", "deno"]);
    }
}
