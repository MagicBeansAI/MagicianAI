//! Filesystem-boundary checks shared by canonical LLM analytics stores.

use std::path::{Component, Path};

use anyhow::{anyhow, Context, Result};

/// Reject symlinks or special entries below the configured runtime root. The
/// root itself may intentionally be a mounted/symlinked volume, but tenant and
/// dataset components must never redirect a scoped read or write elsewhere.
pub fn ensure_real_scoped_directory_chain(base: &Path, target: &Path) -> Result<()> {
    let relative = target.strip_prefix(base).with_context(|| {
        format!(
            "scoped LLM analytics path {} is outside runtime root {}",
            target.display(),
            base.display()
        )
    })?;
    let mut current = base.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(anyhow!(
                "scoped LLM analytics path contains a non-normal component: {}",
                target.display()
            ));
        };
        current.push(component);
        let metadata = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspecting scoped path {}", current.display()));
            },
        };
        if !metadata.file_type().is_dir() {
            return Err(anyhow!(
                "scoped LLM analytics path component must be a real directory: {}",
                current.display()
            ));
        }
    }
    Ok(())
}

/// Return whether a path exists as a real regular file. Symlinks, directories,
/// devices and other special entries fail closed instead of being followed.
pub fn ensure_regular_file_or_missing(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(anyhow!(
            "scoped LLM analytics object must be a regular file: {}",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error)
            .with_context(|| format!("inspecting scoped analytics object {}", path.display())),
    }
}
