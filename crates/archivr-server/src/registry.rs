use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MountedArchive {
    pub id: String,
    pub label: String,
    pub archive_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ServerRegistry {
    #[serde(default)]
    pub archives: Vec<MountedArchive>,
    /// Optional bind address. Defaults to `127.0.0.1:8080`.
    #[serde(default)]
    pub bind: Option<String>,
    /// Path to the server-level auth database.
    /// Defaults to `archivr-auth.sqlite` in the same directory as the config file.
    #[serde(default)]
    pub auth_db_path: Option<std::path::PathBuf>,
}

pub fn load_registry(path: &Path) -> Result<ServerRegistry> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read server registry {}", path.display()))?;
    let registry = toml::from_str::<ServerRegistry>(&contents)
        .with_context(|| format!("failed to parse server registry {}", path.display()))?;
    validate_registry(&registry)?;
    Ok(registry)
}

#[cfg(test)]
pub fn save_registry(path: &Path, registry: &ServerRegistry) -> Result<()> {
    validate_registry(registry)?;
    let contents = toml::to_string_pretty(registry)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    Ok(())
}

pub fn validate_registry(registry: &ServerRegistry) -> Result<()> {
    let mut seen_ids = std::collections::HashSet::new();
    for archive in &registry.archives {
        if archive.id.trim().is_empty() {
            bail!("archive id must not be empty");
        }
        if !seen_ids.insert(archive.id.clone()) {
            bail!("duplicate archive id: {}", archive.id);
        }
        if archive.label.trim().is_empty() {
            bail!("archive label must not be empty for id={}", archive.id);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_round_trips_archives_from_toml() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("personal").join(".archivr");
        fs::create_dir_all(&archive_path).unwrap();
        fs::write(archive_path.join("name"), "Personal").unwrap();
        fs::write(
            archive_path.join("store_path"),
            temp.path().join("store").display().to_string(),
        )
        .unwrap();

        let registry = ServerRegistry {
            archives: vec![MountedArchive {
                id: "personal".to_string(),
                label: "Personal".to_string(),
                archive_path: archive_path.clone(),
            }],
            bind: None,
            auth_db_path: None,
        };
        let path = temp.path().join("server.toml");
        save_registry(&path, &registry).unwrap();

        let loaded = load_registry(&path).unwrap();

        assert_eq!(loaded, registry);
    }

    #[test]
    fn registry_rejects_duplicate_archive_ids() {
        let registry = ServerRegistry {
            archives: vec![
                MountedArchive {
                    id: "personal".to_string(),
                    label: "Personal".to_string(),
                    archive_path: PathBuf::from("/tmp/a/.archivr"),
                },
                MountedArchive {
                    id: "personal".to_string(),
                    label: "Duplicate".to_string(),
                    archive_path: PathBuf::from("/tmp/b/.archivr"),
                },
            ],
            bind: None,
            auth_db_path: None,
        };

        let err = validate_registry(&registry).unwrap_err().to_string();

        assert!(err.contains("duplicate archive id"));
    }

    #[test]
    fn registry_bind_field_round_trips() {
        let toml = r#"bind = "127.0.0.1:9090""#;
        let registry: ServerRegistry = toml::from_str(toml).unwrap();
        assert_eq!(registry.bind.as_deref(), Some("127.0.0.1:9090"));
        assert!(registry.archives.is_empty());
    }

    #[test]
    fn registry_bind_field_defaults_to_none_when_absent() {
        let toml = r#""#;
        let registry: ServerRegistry = toml::from_str(toml).unwrap();
        assert!(registry.bind.is_none());
    }

    #[test]
    fn registry_bind_field_does_not_affect_archive_validation() {
        let registry = ServerRegistry {
            archives: vec![],
            bind: Some("0.0.0.0:8080".to_string()),
            auth_db_path: None,
        };
        // validate_registry does not reject non-loopback bind — that's main's concern.
        assert!(validate_registry(&registry).is_ok());
    }
}
