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
    pub archives: Vec<MountedArchive>,
}

pub fn load_registry(path: &Path) -> Result<ServerRegistry> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read server registry {}", path.display()))?;
    let registry = toml::from_str::<ServerRegistry>(&contents)
        .with_context(|| format!("failed to parse server registry {}", path.display()))?;
    validate_registry(&registry)?;
    Ok(registry)
}

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
    let mut ids = std::collections::HashSet::new();
    for archive in &registry.archives {
        if archive.id.trim().is_empty() {
            bail!("archive id must not be empty");
        }
        if !ids.insert(archive.id.as_str()) {
            bail!("duplicate archive id: {}", archive.id);
        }
        if !archive.archive_path.ends_with(".archivr") {
            bail!(
                "mounted archive path must point at a .archivr directory: {}",
                archive.archive_path.display()
            );
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
        };

        let err = validate_registry(&registry).unwrap_err().to_string();

        assert!(err.contains("duplicate archive id"));
    }
}
