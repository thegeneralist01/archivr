//! JavaScript runtime resolution for yt-dlp.
//!
//! yt-dlp needs a JS runtime to solve YouTube's challenges (EJS). Without one, YouTube
//! downloads may fail with HTTP 403. This module picks the runtime archivr passes to every
//! yt-dlp process via `--js-runtimes`:
//!
//! 1. `ARCHIVR_JS_RUNTIME` (forced, `RUNTIME[:ABS_PATH]`; skips resolution and version checks).
//! 2. The newest Deno >= [`MIN_DENO_VERSION`] among the pinned `ARCHIVR_DENO` and the
//!    state-dir copy (`<state_dir>/deno/deno`); an exact tie goes to the state dir.
//! 3. `deno` on `PATH`, if new enough.
//! 4. Nothing (a warning is printed once per resolution: first use and each refresh).
//!
//! Only Deno is ever chosen automatically; Node, Bun and QuickJS are used only when forced.

use std::{
    env,
    ffi::{OsStr, OsString},
    fmt,
    path::{Path, PathBuf},
    process::Command,
    sync::RwLock,
};

use super::ytdlp::state_dir;

/// Forced runtime override, `RUNTIME[:ABS_PATH]` with RUNTIME one of deno|node|bun|quickjs.
pub const JS_RUNTIME_ENV: &str = "ARCHIVR_JS_RUNTIME";
/// Pinned Deno binary (set by the Nix wrappers and the Docker image).
pub const DENO_ENV: &str = "ARCHIVR_DENO";
/// Oldest Deno yt-dlp's EJS solver supports.
pub const MIN_DENO_VERSION: DenoVersion = DenoVersion { major: 2, minor: 3, patch: 0 };

/// Cached choice: outer `None` = not resolved yet. Swapped by [`refresh_js_runtime`].
static RESOLVED_JS_RUNTIME: RwLock<Option<Option<JsRuntime>>> = RwLock::new(None);

/// Resolves (uncached) and prints the warnings; runs on first use and on each refresh.
fn resolve_js_runtime_logged() -> Option<JsRuntime> {
    if let Err(reason) = forced_js_runtime() {
        let raw = env::var_os(JS_RUNTIME_ENV).unwrap_or_default();
        eprintln!("warn: ignoring {JS_RUNTIME_ENV}={raw:?}: {reason}");
    }
    let resolved = resolve_js_runtime_with_role().map(|(_, rt)| rt);
    if resolved.is_none() {
        eprintln!(
            "warn: no JavaScript runtime for yt-dlp (need deno >= {MIN_DENO_VERSION} via \
             {DENO_ENV}, the state dir, or PATH; or set {JS_RUNTIME_ENV}) — YouTube \
             downloads may fail with HTTP 403; run `archivr yt-dlp update` to install deno"
        );
    }
    resolved
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsRuntimeKind {
    Deno,
    Node,
    Bun,
    QuickJs,
}

impl JsRuntimeKind {
    /// Runtime name as yt-dlp's `--js-runtimes` expects it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deno => "deno",
            Self::Node => "node",
            Self::Bun => "bun",
            Self::QuickJs => "quickjs",
        }
    }

    /// ASCII case-insensitive match against the exact allowlist.
    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Deno, Self::Node, Self::Bun, Self::QuickJs]
            .into_iter()
            .find(|k| k.as_str().eq_ignore_ascii_case(name))
    }

    /// Executable name yt-dlp looks for when the runtime path is a directory
    /// (mirrors `_determine_runtime_path` in yt-dlp's JS runtime classes).
    fn executable_name(self) -> &'static str {
        match self {
            Self::QuickJs => "qjs",
            other => other.as_str(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsRuntime {
    pub kind: JsRuntimeKind,
    pub path: Option<PathBuf>,
}

impl JsRuntime {
    /// `kind` or `kind:path`, built without lossy conversion.
    pub fn spec(&self) -> OsString {
        let mut spec = OsString::from(self.kind.as_str());
        if let Some(path) = &self.path {
            spec.push(":");
            spec.push(path.as_os_str());
        }
        spec
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DenoVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl DenoVersion {
    /// Parses `2.9.7` or `v2.9.7`; anything after the patch digits (`+abc`, `-rc1`) is ignored.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let s = s.strip_prefix('v').unwrap_or(s);
        let end = s
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(s.len());
        let mut parts = s[..end].split('.').map(|p| p.parse::<u64>().ok());
        let version = Self {
            major: parts.next()??,
            minor: parts.next()??,
            patch: parts.next()??,
        };
        parts.next().is_none().then_some(version)
    }
}

impl fmt::Display for DenoVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Validates a `RUNTIME[:ABS_PATH]` spec. The path may be a file or a directory
/// (yt-dlp accepts both) but must be absolute and exist.
pub fn parse_js_runtime_spec(raw: &str) -> Result<JsRuntime, String> {
    let raw = raw.trim();
    let (name, path) = match raw.split_once(':') {
        Some((name, path)) => (name, Some(path)),
        None => (raw, None),
    };
    let kind = JsRuntimeKind::from_name(name).ok_or_else(|| {
        format!("unknown runtime {name} (expected deno, node, bun or quickjs)")
    })?;
    let path = match path {
        None => None,
        Some("") => return Err("empty path after ':'".to_string()),
        Some(p) => {
            let p = PathBuf::from(p);
            if !p.is_absolute() {
                return Err("path must be absolute".to_string());
            }
            if !p.exists() {
                return Err("path does not exist".to_string());
            }
            Some(p)
        }
    };
    Ok(JsRuntime { kind, path })
}

/// Reads `ARCHIVR_JS_RUNTIME`; `Ok(None)` if unset or empty.
pub fn forced_js_runtime() -> Result<Option<JsRuntime>, String> {
    match env::var(JS_RUNTIME_ENV) {
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err("value is not valid UTF-8".to_string()),
        Ok(raw) if raw.trim().is_empty() => Ok(None),
        Ok(raw) => parse_js_runtime_spec(&raw).map(Some),
    }
}

/// `ARCHIVR_DENO`, if it points to an existing file.
pub fn pinned_deno() -> Option<PathBuf> {
    env::var_os(DENO_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_file())
}

/// Deno slot inside the state dir (`<state_dir>/deno/deno`); not existence-filtered.
pub fn state_dir_deno() -> Option<PathBuf> {
    state_dir().map(|d| d.join("deno").join("deno"))
}

/// First `dir/name` that is a file, scanning `path_var` like a shell would.
pub fn find_on_path(name: &str, path_var: Option<&OsStr>) -> Option<PathBuf> {
    env::split_paths(path_var?)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// `deno` on the process `PATH`.
pub fn path_deno() -> Option<PathBuf> {
    find_on_path("deno", env::var_os("PATH").as_deref())
}

/// Parses `deno --version` output (`deno 2.9.7 (stable, release, ...)` on the first line).
pub fn parse_deno_version_output(stdout: &str) -> Option<DenoVersion> {
    let first = stdout.lines().next()?.trim();
    let rest = first.strip_prefix("deno ")?;
    DenoVersion::parse(rest.split_whitespace().next()?)
}

/// Runs `<binary> --version` and parses it; `None` on spawn failure, non-zero exit or junk output.
pub fn probe_deno_version(binary: &Path) -> Option<DenoVersion> {
    let output = Command::new(binary).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_deno_version_output(&String::from_utf8_lossy(&output.stdout))
}

/// Human version string for a (typically forced) runtime. Deno is probed and parsed; other
/// runtimes with a path report the first stdout line of `--version`; pathless non-Deno → `None`.
/// A directory path gets the runtime's executable name joined, as yt-dlp does.
pub fn probe_js_runtime_version(rt: &JsRuntime) -> Option<String> {
    let binary = match &rt.path {
        Some(p) if p.is_dir() => p.join(rt.kind.executable_name()),
        Some(p) => p.clone(),
        None if rt.kind == JsRuntimeKind::Deno => PathBuf::from("deno"),
        None => return None,
    };
    if rt.kind == JsRuntimeKind::Deno {
        return probe_deno_version(&binary).map(|v| v.to_string());
    }
    let output = Command::new(&binary).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
}

/// Which candidate slot a resolved runtime came from. Several slots can point at the same
/// binary (the Nix wrappers set `ARCHIVR_DENO` and also put that Deno on `PATH`), so callers
/// that need to name the winner must use the role, not compare paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsRuntimeRole {
    /// `ARCHIVR_JS_RUNTIME`.
    Forced,
    /// `ARCHIVR_DENO`.
    Pinned,
    /// `<state_dir>/deno/deno`.
    StateDir,
    /// `deno` on `PATH`.
    Path,
}

impl JsRuntimeRole {
    /// Row label used by `archivr yt-dlp status`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Forced => "force (ARCHIVR_JS_RUNTIME)",
            Self::Pinned => "env (ARCHIVR_DENO)",
            Self::StateDir => "state-dir",
            Self::Path => "path (deno)",
        }
    }

    /// Stable machine key (API `role`): "force" | "env" | "state-dir" | "path".
    pub fn key(self) -> &'static str {
        match self {
            Self::Forced => "force",
            Self::Pinned => "env",
            Self::StateDir => "state-dir",
            Self::Path => "path",
        }
    }
}

/// Existing Deno candidates, pinned first and state-dir last (so ties go to the state dir).
pub fn deno_candidates() -> Vec<(JsRuntimeRole, PathBuf)> {
    [
        (JsRuntimeRole::Pinned, pinned_deno()),
        (JsRuntimeRole::StateDir, state_dir_deno()),
    ]
    .into_iter()
    .filter_map(|(role, p)| p.filter(|p| p.is_file()).map(|p| (role, p)))
    .collect()
}

pub(crate) fn resolve_js_runtime_with_path(
    path_var: Option<&OsStr>,
) -> Option<(JsRuntimeRole, JsRuntime)> {
    if let Ok(Some(rt)) = forced_js_runtime() {
        return Some((JsRuntimeRole::Forced, rt));
    }
    let usable = |p: &Path| probe_deno_version(p).filter(|v| *v >= MIN_DENO_VERSION);
    // `max_by` keeps the last maximum, so an exact tie goes to the state dir.
    let (role, best) = deno_candidates()
        .into_iter()
        .filter_map(|(role, p)| usable(&p).map(|v| (v, role, p)))
        .max_by(|(a, ..), (b, ..)| a.cmp(b))
        .map(|(_, role, p)| (role, p))
        .or_else(|| {
            find_on_path("deno", path_var)
                .filter(|p| usable(p).is_some())
                .map(|p| (JsRuntimeRole::Path, p))
        })?;
    Some((role, JsRuntime { kind: JsRuntimeKind::Deno, path: Some(best) }))
}

/// Resolves without caching or printing, also reporting which candidate slot won
/// (used by `archivr yt-dlp status` to mark exactly one row).
pub fn resolve_js_runtime_with_role() -> Option<(JsRuntimeRole, JsRuntime)> {
    resolve_js_runtime_with_path(env::var_os("PATH").as_deref())
}

/// Cached until [`refresh_js_runtime`]; owned clone so a refresh never invalidates a caller.
pub fn resolve_js_runtime() -> Option<JsRuntime> {
    if let Some(v) = RESOLVED_JS_RUNTIME.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return v.clone();
    }
    let mut slot = RESOLVED_JS_RUNTIME.write().unwrap_or_else(|e| e.into_inner());
    slot.get_or_insert_with(resolve_js_runtime_logged).clone()
}

/// Re-resolves (outside the lock) and swaps the cache; called after a successful Deno
/// install. Commands already built keep their old runtime.
pub fn refresh_js_runtime() -> Option<JsRuntime> {
    let fresh = resolve_js_runtime_logged();
    *RESOLVED_JS_RUNTIME.write().unwrap_or_else(|e| e.into_inner()) = Some(fresh.clone());
    fresh
}

/// yt-dlp arguments selecting `runtime`.
///
/// yt-dlp builds its runtime map keyed by name, splitting each `--js-runtimes` value on the
/// first `:` (`yt_dlp/__init__.py:784-786`), so a later entry for the same name wins:
/// `deno:<path>` replaces the default `deno` entry. Non-Deno runtimes are preceded by
/// `--no-js-runtimes` (`options.py:460-479`) so a Deno yt-dlp finds on its own can't take
/// priority over the forced choice. Each value is one argv element; no shell is involved.
pub fn js_runtime_args(runtime: Option<&JsRuntime>) -> Vec<OsString> {
    let Some(rt) = runtime else {
        return Vec::new();
    };
    let mut args = Vec::with_capacity(3);
    if rt.kind != JsRuntimeKind::Deno {
        args.push(OsString::from("--no-js-runtimes"));
    }
    args.push(OsString::from("--js-runtimes"));
    args.push(rt.spec());
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::downloader::ytdlp::STATE_DIR_ENV;
    use std::{fs, sync::MutexGuard};
    use tempfile::TempDir;

    /// Serialises env-mutating tests and clears every var the resolver reads.
    fn env_guard() -> MutexGuard<'static, ()> {
        let guard = crate::downloader::RESOLVER_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for key in [JS_RUNTIME_ENV, DENO_ENV, STATE_DIR_ENV] {
            unsafe { env::remove_var(key) };
        }
        guard
    }

    /// Points the state dir at an (initially empty) dir under `tmp` and returns it.
    fn set_state(tmp: &TempDir) -> PathBuf {
        let state = tmp.path().join("state");
        fs::create_dir_all(&state).unwrap();
        unsafe { env::set_var(STATE_DIR_ENV, &state) };
        state
    }

    /// Writes a fake `deno` script to `path` (every caller uses a fresh path), then waits until
    /// it can be exec'd. A child forked by a parallel test while our write fd was open keeps a
    /// copy of it until that child execs, so our exec can fail with ETXTBSY
    /// (rust-lang/rust#114554). One successful exec proves no writer is left, and none can
    /// appear later because our fd is already closed.
    fn fake_deno(path: &Path, ver: &str) {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            format!("#!/bin/sh\necho 'deno {ver} (stable, release, test)'\necho 'v8 1.0'\n"),
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        for _ in 0..200 {
            match Command::new(path).arg("--version").output() {
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                _ => return,
            }
        }
        panic!("{} stayed busy (ETXTBSY)", path.display());
    }

    fn deno_at(role: JsRuntimeRole, p: PathBuf) -> Option<(JsRuntimeRole, JsRuntime)> {
        Some((role, JsRuntime { kind: JsRuntimeKind::Deno, path: Some(p) }))
    }

    #[test]
    fn spec_parsing_accepts_allowlisted_runtimes() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("node");
        fs::write(&file, "").unwrap();

        assert_eq!(
            parse_js_runtime_spec("deno"),
            Ok(JsRuntime { kind: JsRuntimeKind::Deno, path: None })
        );
        assert_eq!(parse_js_runtime_spec(" NODE ").unwrap().kind, JsRuntimeKind::Node);
        assert_eq!(
            parse_js_runtime_spec(&format!("node:{}", file.display())),
            Ok(JsRuntime { kind: JsRuntimeKind::Node, path: Some(file.clone()) })
        );
        assert_eq!(
            parse_js_runtime_spec(&format!("deno:{}", tmp.path().display())).unwrap().path,
            Some(tmp.path().to_path_buf())
        );
        assert_eq!(parse_js_runtime_spec("bun").unwrap().kind, JsRuntimeKind::Bun);
        assert_eq!(parse_js_runtime_spec("quickjs").unwrap().kind, JsRuntimeKind::QuickJs);
    }

    #[test]
    fn spec_parsing_rejects_bad_values() {
        assert_eq!(parse_js_runtime_spec("node:"), Err("empty path after ':'".into()));
        assert_eq!(parse_js_runtime_spec("node:rel/path"), Err("path must be absolute".into()));
        assert_eq!(
            parse_js_runtime_spec("deno:/does/not/exist"),
            Err("path does not exist".into())
        );
        for bad in ["python", "--exec", "deno,node"] {
            let err = parse_js_runtime_spec(bad).unwrap_err();
            assert!(err.starts_with("unknown runtime"), "{bad}: {err}");
        }
    }

    #[test]
    fn args_follow_runtime_kind() {
        assert!(js_runtime_args(None).is_empty());
        let deno = JsRuntime { kind: JsRuntimeKind::Deno, path: Some("/p".into()) };
        assert_eq!(js_runtime_args(Some(&deno)), ["--js-runtimes", "deno:/p"]);
        let node = JsRuntime { kind: JsRuntimeKind::Node, path: Some("/p".into()) };
        assert_eq!(
            js_runtime_args(Some(&node)),
            ["--no-js-runtimes", "--js-runtimes", "node:/p"]
        );
        let bun = JsRuntime { kind: JsRuntimeKind::Bun, path: None };
        assert_eq!(js_runtime_args(Some(&bun)), ["--no-js-runtimes", "--js-runtimes", "bun"]);
    }

    #[test]
    fn version_parsing() {
        let v297 = Some(DenoVersion { major: 2, minor: 9, patch: 7 });
        assert_eq!(
            parse_deno_version_output(
                "deno 2.9.7 (stable, release, aarch64-apple-darwin)\nv8 14.0\ntypescript 5.9\n"
            ),
            v297
        );
        assert_eq!(parse_deno_version_output("deno 2.9.7+abc123 (canary, x)\n"), v297);
        assert_eq!(parse_deno_version_output("node v22"), None);
        assert_eq!(parse_deno_version_output(""), None);
        assert_eq!(parse_deno_version_output("deno"), None);
        assert_eq!(DenoVersion::parse("v2.9.7"), v297);
        assert_eq!(DenoVersion::parse("2.9"), None);
        assert_eq!(DenoVersion::parse("2.9.7.1"), None);
        assert!(DenoVersion::parse("2.10.0") > v297);
        assert_eq!(v297.unwrap().to_string(), "2.9.7");
    }

    #[test]
    fn newer_state_dir_deno_wins() {
        let _g = env_guard();
        let tmp = TempDir::new().unwrap();
        let state = set_state(&tmp);
        let pinned = tmp.path().join("pin/deno");
        fake_deno(&pinned, "2.4.0");
        fake_deno(&state.join("deno/deno"), "2.9.7");
        unsafe { env::set_var(DENO_ENV, &pinned) };
        assert_eq!(
            resolve_js_runtime_with_path(None),
            deno_at(JsRuntimeRole::StateDir, state.join("deno/deno"))
        );
    }

    #[test]
    fn newer_pinned_deno_wins() {
        let _g = env_guard();
        let tmp = TempDir::new().unwrap();
        let state = set_state(&tmp);
        let pinned = tmp.path().join("pin/deno");
        fake_deno(&pinned, "2.10.0");
        fake_deno(&state.join("deno/deno"), "2.9.7");
        unsafe { env::set_var(DENO_ENV, &pinned) };
        assert_eq!(
            resolve_js_runtime_with_path(None),
            deno_at(JsRuntimeRole::Pinned, pinned)
        );
    }

    #[test]
    fn tie_goes_to_state_dir() {
        let _g = env_guard();
        let tmp = TempDir::new().unwrap();
        let state = set_state(&tmp);
        let pinned = tmp.path().join("pin/deno");
        fake_deno(&pinned, "2.9.7");
        fake_deno(&state.join("deno/deno"), "2.9.7");
        unsafe { env::set_var(DENO_ENV, &pinned) };
        assert_eq!(
            resolve_js_runtime_with_path(None),
            deno_at(JsRuntimeRole::StateDir, state.join("deno/deno"))
        );
    }

    #[test]
    fn too_old_pinned_falls_back_to_path() {
        let _g = env_guard();
        let tmp = TempDir::new().unwrap();
        set_state(&tmp);
        let pinned = tmp.path().join("pin/deno");
        fake_deno(&pinned, "2.2.9");
        let bin = tmp.path().join("bin");
        fake_deno(&bin.join("deno"), "2.4.0");
        unsafe { env::set_var(DENO_ENV, &pinned) };
        assert_eq!(
            resolve_js_runtime_with_path(Some(bin.as_os_str())),
            deno_at(JsRuntimeRole::Path, bin.join("deno"))
        );
    }

    #[test]
    fn valid_pinned_beats_newer_path_deno() {
        let _g = env_guard();
        let tmp = TempDir::new().unwrap();
        set_state(&tmp);
        let pinned = tmp.path().join("pin/deno");
        fake_deno(&pinned, "2.4.0");
        let bin = tmp.path().join("bin");
        fake_deno(&bin.join("deno"), "2.9.7");
        unsafe { env::set_var(DENO_ENV, &pinned) };
        assert_eq!(
            resolve_js_runtime_with_path(Some(bin.as_os_str())),
            deno_at(JsRuntimeRole::Pinned, pinned)
        );
    }

    #[test]
    fn pinned_deno_also_on_path_wins_as_pinned_only() {
        // The Nix wrappers set ARCHIVR_DENO and put the same Deno on PATH.
        let _g = env_guard();
        let tmp = TempDir::new().unwrap();
        set_state(&tmp);
        let bin = tmp.path().join("bin");
        let deno = bin.join("deno");
        fake_deno(&deno, "2.9.4");
        unsafe { env::set_var(DENO_ENV, &deno) };
        assert_eq!(find_on_path("deno", Some(bin.as_os_str())), Some(deno.clone()));
        assert_eq!(
            resolve_js_runtime_with_path(Some(bin.as_os_str())),
            deno_at(JsRuntimeRole::Pinned, deno)
        );
    }

    #[test]
    fn forced_pinned_deno_wins_as_forced_only() {
        let _g = env_guard();
        let tmp = TempDir::new().unwrap();
        set_state(&tmp);
        let bin = tmp.path().join("bin");
        let deno = bin.join("deno");
        fake_deno(&deno, "2.9.4");
        unsafe {
            env::set_var(DENO_ENV, &deno);
            env::set_var(JS_RUNTIME_ENV, format!("deno:{}", deno.display()));
        }
        assert_eq!(
            resolve_js_runtime_with_path(Some(bin.as_os_str())),
            deno_at(JsRuntimeRole::Forced, deno)
        );
    }

    #[test]
    fn too_old_path_deno_resolves_none() {
        let _g = env_guard();
        let tmp = TempDir::new().unwrap();
        set_state(&tmp);
        let bin = tmp.path().join("bin");
        fake_deno(&bin.join("deno"), "2.2.9");
        assert_eq!(resolve_js_runtime_with_path(Some(bin.as_os_str())), None);
    }

    #[test]
    fn valid_forced_runtime_wins() {
        let _g = env_guard();
        let tmp = TempDir::new().unwrap();
        let state = set_state(&tmp);
        let pinned = tmp.path().join("pin/deno");
        fake_deno(&pinned, "2.9.7");
        fake_deno(&state.join("deno/deno"), "2.9.7");
        let node = tmp.path().join("node");
        fs::write(&node, "").unwrap();
        unsafe {
            env::set_var(DENO_ENV, &pinned);
            env::set_var(JS_RUNTIME_ENV, format!("node:{}", node.display()));
        }
        assert_eq!(
            resolve_js_runtime_with_path(None),
            Some((
                JsRuntimeRole::Forced,
                JsRuntime { kind: JsRuntimeKind::Node, path: Some(node) }
            ))
        );
    }

    #[test]
    fn invalid_forced_runtime_is_ignored() {
        let _g = env_guard();
        let tmp = TempDir::new().unwrap();
        let state = set_state(&tmp);
        fake_deno(&state.join("deno/deno"), "2.9.7");
        unsafe { env::set_var(JS_RUNTIME_ENV, "python") };
        assert!(forced_js_runtime().is_err());
        assert_eq!(
            resolve_js_runtime_with_path(None),
            deno_at(JsRuntimeRole::StateDir, state.join("deno/deno"))
        );
    }

    #[test]
    fn no_candidates_resolves_none() {
        let _g = env_guard();
        let tmp = TempDir::new().unwrap();
        set_state(&tmp);
        let empty_bin = tmp.path().join("bin");
        fs::create_dir_all(&empty_bin).unwrap();
        assert!(deno_candidates().is_empty());
        assert_eq!(resolve_js_runtime_with_path(Some(empty_bin.as_os_str())), None);
    }

    #[test]
    fn forced_directory_probe_joins_runtime_name() {
        let tmp = TempDir::new().unwrap();
        fake_deno(&tmp.path().join("deno"), "2.9.7");
        let rt = JsRuntime { kind: JsRuntimeKind::Deno, path: Some(tmp.path().to_path_buf()) };
        assert_eq!(probe_js_runtime_version(&rt).as_deref(), Some("2.9.7"));
    }

    #[test]
    fn find_on_path_scans_in_order() {
        let tmp = TempDir::new().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(b.join("tool"), "").unwrap();
        let path_var = env::join_paths([&a, &b]).unwrap();
        assert_eq!(find_on_path("tool", Some(&path_var)), Some(b.join("tool")));
        assert_eq!(find_on_path("missing", Some(&path_var)), None);
    }

    #[test]
    fn refresh_js_runtime_swaps_cached_choice() {
        let _g = env_guard();
        unsafe { env::set_var(JS_RUNTIME_ENV, "node") };
        assert_eq!(refresh_js_runtime().map(|rt| rt.kind), Some(JsRuntimeKind::Node));
        assert_eq!(resolve_js_runtime().map(|rt| rt.kind), Some(JsRuntimeKind::Node));

        unsafe { env::set_var(JS_RUNTIME_ENV, "bun") };
        assert_eq!(resolve_js_runtime().map(|rt| rt.kind), Some(JsRuntimeKind::Node));
        assert_eq!(refresh_js_runtime().map(|rt| rt.kind), Some(JsRuntimeKind::Bun));

        unsafe { env::remove_var(JS_RUNTIME_ENV) };
        refresh_js_runtime();
    }
}
