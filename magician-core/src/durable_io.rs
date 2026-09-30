//! Magician's async durable-write adapter retains blocking-pool admission.
//! Neutral byte publication and retry primitives have one shared implementation.

pub use magicvault_primitives::durable_io::{
    is_transient_fd_exhaustion, publish_staged_file_durably_sync, retry_transient_io,
    retry_transient_io_blocking, sync_parent_dir_blocking, warn_cleanup_failed,
    write_bytes_durably_sync, write_bytes_durably_with_mode_sync,
};
use std::path::Path;
use tokio::fs;
use uuid::Uuid;

/// Async twin of [`write_bytes_durably_sync`], same guarantees, same reason for
/// the `std::io::Error`.
///
/// The parent sync runs on the blocking pool (via
/// [`crate::blocking_admission::spawn_blocking_with_admission`]) because
/// `File::sync_all` on a directory has no async form. The admission permit is
/// acquired *before* rename so a saturated pool cannot leave a published file
/// waiting on parent fsync. Join and fsync failures are both `io::Error`s;
/// a lost parent sync is exactly the failure this function exists to prevent.
pub async fn write_bytes_durably(path: &Path, value: &[u8]) -> std::io::Result<()> {
    write_bytes_durably_with_mode(path, value, None).await
}

async fn create_staging(path: &Path, mode: Option<u32>) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if let Some(mode) = mode {
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    retry_transient_io(|| options.open(path)).await
}

/// [`write_bytes_durably`], with `mode` applied at staging-file creation and on
/// its owned descriptor **before the first byte**, then synced before rename.
///
/// For a file whose permissions matter, applying the mode after publishing is
/// not the same thing: between the rename and the `chmod` the file is readable
/// at whatever the umask allowed. The window is short and it is still a window,
/// and the file that motivated this one — `bot_configs.yaml` — carries provider
/// tokens. Applying it only before rename also leaves the temporary file
/// readable while its contents are written. Creation must already be private.
///
/// A caller that wants to *preserve* an existing mode reads it first and passes
/// it here; the rename then publishes a file that already has it. Unlike the
/// old hand-rolled writers this replaced, creation is exclusive and permissions
/// are applied to the owned descriptor. Existing paths are never adopted.
///
/// `mode` is Unix-only and ignored elsewhere, which matches the platforms these
/// stores run on. Failing to apply it is an error rather than a warning: a
/// caller passing a mode is asking for a permission guarantee, and silently
/// publishing a more permissive file is the failure it was trying to avoid.
pub async fn write_bytes_durably_with_mode(
    path: &Path,
    value: &[u8],
    mode: Option<u32>,
) -> std::io::Result<()> {
    #[cfg(test)]
    let _admission = crate::blocking_admission::ensure_blocking_admission_test_lock().await;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        retry_transient_io(|| fs::create_dir_all(parent)).await?;
    }
    let tmp_path = path.with_file_name(format!(".artifact-write-{}.tmp", Uuid::new_v4().simple()));
    let mut owns_staging = false;
    let written = async {
        use tokio::io::AsyncWriteExt;
        let mut file = create_staging(&tmp_path, mode).await?;
        owns_staging = true;
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(mode))
                .await?;
        }
        file.write_all(value).await?;
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        // Hold the blocking slot *before* publish so a saturated admission
        // queue cannot stretch the window between rename and parent fsync.
        let permit = crate::blocking_admission::acquire_blocking_admission()
            .await
            .map_err(|error| {
                std::io::Error::other(format!("sync_parent_dir admission: {error}"))
            })?;
        retry_transient_io(|| fs::rename(&tmp_path, path)).await?;
        let target = path.to_path_buf();
        crate::blocking_admission::spawn_blocking_with_admission(permit, move || {
            sync_parent_dir_blocking(&target)
        })
        .await
        .map_err(|error| std::io::Error::other(format!("sync_parent_dir task: {error}")))??;
        Ok(())
    }
    .await;
    if written.is_err() && owns_staging {
        if let Err(cleanup) = fs::remove_file(&tmp_path).await {
            warn_cleanup_failed(&tmp_path, &cleanup);
        }
    }
    written
}

#[cfg(all(test, unix))]
mod staging_tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[tokio::test]
    async fn staging_is_private_before_writing_and_refuses_existing_paths() {
        let root = tempfile::tempdir().unwrap();
        let stage = root.path().join("stage");
        let file = create_staging(&stage, Some(0o600)).await.unwrap();
        let metadata = file.metadata().await.unwrap();
        assert_eq!(metadata.permissions().mode() & 0o077, 0);
        assert_eq!(metadata.len(), 0);
        drop(file);
        fs::write(&stage, b"existing").await.unwrap();
        assert!(create_staging(&stage, Some(0o600)).await.is_err());
        let link = root.path().join("link");
        symlink(&stage, &link).unwrap();
        assert!(create_staging(&link, Some(0o600)).await.is_err());
        assert_eq!(fs::read(stage).await.unwrap(), b"existing");
    }

    #[tokio::test]
    async fn replacement_preserves_requested_mode_and_failed_publish_cleans_only_staging() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("record");
        write_bytes_durably_with_mode(&target, b"first", Some(0o600))
            .await
            .unwrap();
        write_bytes_durably_with_mode(&target, b"second", Some(0o640))
            .await
            .unwrap();
        assert_eq!(
            fs::metadata(&target).await.unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(fs::read(&target).await.unwrap(), b"second");
        let directory = root.path().join("directory");
        fs::create_dir(&directory).await.unwrap();
        fs::write(directory.join("retained"), b"existing")
            .await
            .unwrap();
        assert!(
            write_bytes_durably_with_mode(&directory, b"replacement", Some(0o600))
                .await
                .is_err()
        );
        assert_eq!(
            fs::read(directory.join("retained")).await.unwrap(),
            b"existing"
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
    }
}
