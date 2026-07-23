mod auth;
mod registry;
mod routes;

use anyhow::{Context, Result};
use std::{net::SocketAddr, path::PathBuf};

const DEFAULT_BIND: &str = "127.0.0.1:8080";

/// Prune abandoned staged-upload UUID dirs under `uploads_dir` whose content
/// is older than `cutoff`.
///
/// `cleanup_stale_sentinels`: pass `true` at startup (before the server begins
/// accepting connections) so crash-leftover `.uploading` markers are removed and
/// those dirs are subject to the normal age check.  Pass `false` from the
/// in-process periodic task so dirs with a live sentinel (active XHR) are
/// skipped entirely.
///
/// Staleness is measured from the newest non-sentinel child file's mtime so a
/// just-finished slow upload is not pruned before the user submits it for
/// capture.  Empty dirs fall back to the directory mtime.
fn prune_stale_upload_dirs(
    uploads_dir: &std::path::Path,
    cutoff: std::time::SystemTime,
    cleanup_stale_sentinels: bool,
) {
    let Ok(entries) = std::fs::read_dir(uploads_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let sentinel = path.join(".uploading");
        if sentinel.exists() {
            if cleanup_stale_sentinels {
                // Server just started — no uploads are in flight, so any
                // sentinel is a crash remnant.  Remove it and fall through
                // to the age check below.
                let _ = std::fs::remove_file(&sentinel);
            } else {
                // An active XHR is writing to this dir — leave it alone.
                continue;
            }
        }
        // Measure staleness from the newest non-sentinel child file so a
        // completed slow upload isn't pruned while the user is still on the
        // capture form.  Fall back to dir mtime only when the dir is empty.
        let newest_child = std::fs::read_dir(&path)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name() != std::ffi::OsStr::new(".uploading"))
            .filter_map(|e| e.metadata().ok())
            .filter_map(|m| m.modified().ok())
            .max();
        let reference = newest_child
            .or_else(|| entry.metadata().and_then(|m| m.modified()).ok())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        if reference < cutoff {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("archivr-server.toml"));

    let registry = registry::load_registry(&config_path)?;

    // Auth DB lives next to the config file unless overridden in the TOML.
    let auth_db_path = registry.auth_db_path.clone().unwrap_or_else(|| {
        config_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("archivr-auth.sqlite")
    });

    let app = routes::app(registry.clone(), auth_db_path.clone());

    // On startup, mark any jobs that were 'running' when the server last stopped as 'failed'.
    for archive in &registry.archives {
        if let Ok(conn) = archivr_core::database::open_or_initialize(&archive.archive_path) {
            match archivr_core::database::fail_stalled_capture_jobs(&conn) {
                Ok(n) if n > 0 => eprintln!(
                    "info: marked {n} stalled capture job(s) as failed in '{}'",
                    archive.id
                ),
                Err(e) => eprintln!(
                    "warn: stalled job cleanup failed for '{}': {e:#}",
                    archive.id
                ),
                _ => {}
            }
        }
    }

    // Prune staged upload dirs older than 24 h.  cleanup_stale_sentinels=true
    // because no uploads are in flight before the server starts listening.
    let prune_cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(24 * 60 * 60))
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    for archive in &registry.archives {
        if let Ok(paths) = archivr_core::archive::read_archive_paths(&archive.archive_path) {
            let uploads_dir = paths.store_path.join("temp").join("uploads");
            prune_stale_upload_dirs(&uploads_dir, prune_cutoff, true);
        }
    }

    // Spawn maintenance task: session cleanup + staged-upload pruning, every 24 h.
    let cleanup_auth_path = auth_db_path.clone();
    let cleanup_registry = registry.clone();
    tokio::spawn(async move {
        loop {
            if let Ok(conn) = archivr_core::database::open_auth_db(&cleanup_auth_path) {
                match archivr_core::database::delete_expired_sessions(&conn) {
                    Ok(n) if n > 0 => eprintln!("info: cleaned up {n} expired sessions"),
                    Err(e) => eprintln!("warn: session cleanup failed: {e:#}"),
                    _ => {}
                }
            }
            // cleanup_stale_sentinels=false: server is live, respect active uploads.
            let prune_cutoff = std::time::SystemTime::now()
                .checked_sub(std::time::Duration::from_secs(24 * 60 * 60))
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            for archive in &cleanup_registry.archives {
                if let Ok(paths) = archivr_core::archive::read_archive_paths(&archive.archive_path)
                {
                    let uploads_dir = paths.store_path.join("temp").join("uploads");
                    prune_stale_upload_dirs(&uploads_dir, prune_cutoff, false);
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(24 * 60 * 60)).await;
        }
    });

    let bind_str = std::env::var("ARCHIVR_BIND")
        .ok()
        .or_else(|| registry.bind.clone())
        .unwrap_or_else(|| DEFAULT_BIND.to_string());

    let addr: SocketAddr = bind_str
        .parse()
        .with_context(|| format!("invalid bind address: {bind_str}"))?;

    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("archivr-server listening on http://{addr}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}
