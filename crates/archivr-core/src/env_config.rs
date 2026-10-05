//! Env-var resolution helpers shared by the summary providers and the local
//! transcription engines. External tools are configured by `ARCHIVR_*` env
//! vars only, never TOML.

use anyhow::{Result, bail};
use std::{
    env,
    path::{Path, PathBuf},
};

/// Reads a required env var, failing with the *exact variable name* so the
/// server can hand a caller an actionable 400 rather than "not configured".
pub(crate) fn required_env(name: &str) -> Result<String> {
    match env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v),
        _ => bail!("missing required environment variable: {name}"),
    }
}

pub(crate) fn env_or(name: &str, default: &str) -> String {
    env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

pub(crate) fn optional_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|v| !v.trim().is_empty())
}

pub(crate) fn env_timeout(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

/// Resolve a CLI executable path.
///
/// Priority: `env_name` override → first `well_known_absolute` path that
/// exists → `HOME/.local/bin/<bare>` if it exists → bare name (relies on the
/// server's PATH). The macOS defaults matter for `codex`, which the ChatGPT
/// desktop app installs at `/Applications/ChatGPT.app/Contents/Resources/codex`
/// and does not add to PATH.
pub(crate) fn resolve_cli(env_name: &str, well_known_absolute: &[&str], bare: &str) -> PathBuf {
    if let Some(explicit) = optional_env(env_name) {
        return PathBuf::from(explicit);
    }
    for candidate in well_known_absolute {
        let p = Path::new(candidate);
        if p.is_file() {
            return p.to_path_buf();
        }
    }
    if let Some(home) = env::var_os("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".local/bin");
        p.push(bare);
        if p.is_file() {
            return p;
        }
    }
    PathBuf::from(bare)
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    const VAR: &str = "ARCHIVR_TEST_RESOLVE_CLI";

    #[test]
    fn resolve_cli_prefers_env_then_absolute_then_bare() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let absolute = dir.path().join("tool");
        std::fs::write(&absolute, b"").unwrap();
        let absolute_str = absolute.to_str().unwrap();
        let bare = "archivr-test-resolve-cli-surely-not-installed";

        unsafe { env::set_var(VAR, "/explicit/tool") };
        assert_eq!(
            resolve_cli(VAR, &[absolute_str], bare),
            PathBuf::from("/explicit/tool")
        );

        unsafe { env::remove_var(VAR) };
        assert_eq!(resolve_cli(VAR, &["/nonexistent/x", absolute_str], bare), absolute);
        assert_eq!(
            resolve_cli(VAR, &["/nonexistent/x"], bare),
            PathBuf::from(bare)
        );
    }

    #[test]
    fn env_timeout_ignores_zero_and_garbage() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        const T: &str = "ARCHIVR_TEST_ENV_TIMEOUT";
        unsafe { env::set_var(T, "0") };
        assert_eq!(env_timeout(T, 7), 7);
        unsafe { env::set_var(T, "abc") };
        assert_eq!(env_timeout(T, 7), 7);
        unsafe { env::set_var(T, " 12 ") };
        assert_eq!(env_timeout(T, 7), 12);
        unsafe { env::remove_var(T) };
    }
}
