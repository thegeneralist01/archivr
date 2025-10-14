use sha3::{Digest, Sha3_256};
use std::{fs::File, io::Read, path::Path};
use anyhow::Result;

pub fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(hash(String::from_utf8_lossy(&buf).to_string()))
}

pub fn hash(path: String) -> String {
    let mut hasher = Sha3_256::new();
    hasher.update(path.as_bytes());
    let result = hasher.finalize();
    hex::encode(result)
}
