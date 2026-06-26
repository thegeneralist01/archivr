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

    let bind_str = std::env::var("ARCHIVR_BIND")
        .ok()
        .or_else(|| registry.bind.clone())
        .unwrap_or_else(|| DEFAULT_BIND.to_string());

    let addr: SocketAddr = bind_str
        .parse()
        .with_context(|| format!("invalid bind address: {bind_str}"))?;

    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("archivr-server listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
