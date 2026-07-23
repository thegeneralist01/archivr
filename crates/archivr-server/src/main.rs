mod auth;
mod registry;
mod routes;

use anyhow::{Context, Result};
use std::{net::SocketAddr, path::PathBuf};

const DEFAULT_BIND: &str = "127.0.0.1:8080";

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
                Ok(n) if n > 0 => eprintln!("info: marked {n} stalled capture job(s) as failed in '{}'", archive.id),
                Err(e) => eprintln!("warn: stalled job cleanup failed for '{}': {e:#}", archive.id),
                _ => {}
            }
        }
    }

    // Prune staged upload dirs older than 24 h from each archive's temp/uploads/.
    // These accumulate when uploads are abandoned without being submitted for capture.
    let prune_cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(24 * 60 * 60))
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    for archive in &registry.archives {
        if let Ok(paths) = archivr_core::archive::read_archive_paths(&archive.archive_path) {
            let uploads_dir = paths.store_path.join("temp").join("uploads");
            if let Ok(entries) = std::fs::read_dir(&uploads_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        let stale = entry
                            .metadata()
                            .and_then(|m| m.modified())
                            .map(|t| t < prune_cutoff)
                            .unwrap_or(false);
                        if stale {
                            let _ = std::fs::remove_dir_all(&path);
                        }
                    }
                }
            }
        }
    }

    // Spawn maintenance task: session cleanup + staged-upload pruning, every 24 h.
    // Running pruning here (not only at startup) ensures abandoned uploads don't
    // accumulate indefinitely on a long-running server.
    let cleanup_auth_path  = auth_db_path.clone();
    let cleanup_registry   = registry.clone();
    tokio::spawn(async move {
        loop {
            // Session cleanup.
            if let Ok(conn) = archivr_core::database::open_auth_db(&cleanup_auth_path) {
                match archivr_core::database::delete_expired_sessions(&conn) {
                    Ok(n) if n > 0 => eprintln!("info: cleaned up {n} expired sessions"),
                    Err(e) => eprintln!("warn: session cleanup failed: {e:#}"),
                    _ => {}
                }
            }
            // Prune staged upload dirs older than 24 h.
            let prune_cutoff = std::time::SystemTime::now()
                .checked_sub(std::time::Duration::from_secs(24 * 60 * 60))
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            for archive in &cleanup_registry.archives {
                if let Ok(paths) = archivr_core::archive::read_archive_paths(&archive.archive_path) {
                    let uploads_dir = paths.store_path.join("temp").join("uploads");
                    if let Ok(entries) = std::fs::read_dir(&uploads_dir) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path.is_dir() {
                                let stale = entry
                                    .metadata()
                                    .and_then(|m| m.modified())
                                    .map(|t| t < prune_cutoff)
                                    .unwrap_or(false);
                                if stale {
                                    let _ = std::fs::remove_dir_all(&path);
                                }
                            }
                        }
                    }
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
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}
