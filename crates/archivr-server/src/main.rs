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
    let app = routes::app(registry.clone());

    // Bind address priority: ARCHIVR_BIND env var > TOML bind field > default loopback.
    let bind_str = std::env::var("ARCHIVR_BIND")
        .ok()
        .or_else(|| registry.bind.clone())
        .unwrap_or_else(|| DEFAULT_BIND.to_string());

    let addr: SocketAddr = bind_str
        .parse()
        .with_context(|| format!("invalid bind address: {bind_str}"))?;

    // Warn when the server is reachable beyond localhost — it has no authentication.
    if !addr.ip().is_loopback() {
        eprintln!(
            "warn: archivr-server is bound to {addr} — \
             this server has no authentication. \
             Only expose it on a trusted network."
        );
    }

    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("archivr-server listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
