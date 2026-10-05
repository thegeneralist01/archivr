pub mod cookies;
pub mod local;
pub mod store;
pub mod tweets;
pub mod ytdlp;
pub mod metadata;
pub mod http;
pub mod singlefile;
pub mod font_extractor;
pub mod text;
pub mod js_runtime;
pub mod deno_install;
pub mod ytdlp_tools;

/// Env vars are process-global; every core test that sets resolver env vars takes this lock.
#[cfg(test)]
pub(crate) static RESOLVER_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Writes an executable script to `path` (callers must use a fresh path each time), then
/// waits until it can be exec'd. A child forked by a parallel test while our write fd was
/// open keeps a copy of it until that child execs, so our own exec can fail with ETXTBSY
/// (rust-lang/rust#114554). One exec that isn't ETXTBSY proves no writer is left, and none
/// can appear later because our fd is already closed.
#[cfg(all(test, unix))]
pub(crate) fn write_script(path: &std::path::Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    for _ in 0..200 {
        match std::process::Command::new(path).arg("--version").output() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            _ => return,
        }
    }
    panic!("{} stayed busy (ETXTBSY)", path.display());
}
