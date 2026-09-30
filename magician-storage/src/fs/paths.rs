use std::path::{Path, PathBuf};

use crate::error::StorageError;
use crate::identifiers::StorageKey;

pub fn join_encoded(root: &Path, encoded: &str) -> Result<PathBuf, StorageError> {
    let mut path = root.to_path_buf();
    for part in encoded.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(StorageError::invalid_key("path segment"));
        }
        if part.contains('\\') {
            return Err(StorageError::invalid_key("path segment"));
        }
        path.push(part);
    }
    if !path.starts_with(root) {
        return Err(StorageError::invalid_key("escaped root"));
    }
    Ok(path)
}

pub fn key_path(root: &Path, key: &StorageKey) -> Result<PathBuf, StorageError> {
    join_encoded(root, &key.encode())
}

pub fn digest_of(bytes: &[u8]) -> crate::identifiers::ContentDigest {
    let hash = blake3::hash(bytes);
    crate::identifiers::ContentDigest {
        algorithm: crate::identifiers::DigestAlgorithm::Blake3,
        hex: hash.to_hex().to_string(),
    }
}
