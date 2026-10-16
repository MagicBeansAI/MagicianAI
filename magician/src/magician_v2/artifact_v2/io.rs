use std::path::Path;
#[cfg(any(test, feature = "test-fixtures"))]
use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;
#[cfg(any(test, feature = "test-fixtures"))]
use tokio::fs;
use uuid::Uuid;

use super::service::ArtifactV2Error;
use crate::magician_v2::json_traversal::write_canonical_json;

#[cfg(any(test, feature = "test-fixtures"))]
use magician_core::durable_io::retry_transient_io;
/// Log a failed cleanup of a staging temp instead of dropping it silently.
///
/// The write itself already failed — that error is what the caller gets — but
/// a temp that also cannot be removed is a file that nothing will ever touch
/// again: the name is unique per write, so no later attempt reuses it, and no
/// sweeper exists. The hand-rolled writers these helpers replaced logged this;
use magician_core::durable_io::warn_cleanup_failed;
pub use magician_core::durable_io::{
    publish_staged_file_durably_sync, retry_transient_io_blocking, sync_parent_dir_blocking,
    write_bytes_durably, write_bytes_durably_sync, write_bytes_durably_with_mode,
    write_bytes_durably_with_mode_sync,
};

#[cfg(any(test, feature = "test-fixtures"))]
pub async fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, ArtifactV2Error> {
    let body = retry_transient_io(|| fs::read_to_string(path)).await?;
    Ok(serde_json::from_str(&body)?)
}

#[cfg(any(test, feature = "test-fixtures"))]
pub async fn write_json_atomic<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), ArtifactV2Error> {
    let content = serde_json::to_vec_pretty(value)?;
    write_bytes_atomic(path, &content).await
}

pub async fn write_bytes_atomic(path: &Path, value: &[u8]) -> Result<(), ArtifactV2Error> {
    Ok(write_bytes_durably(path, value).await?)
}

pub fn write_bytes_atomic_sync(path: &Path, value: &[u8]) -> Result<(), ArtifactV2Error> {
    Ok(write_bytes_durably_sync(path, value)?)
}

/// Serialize JSON directly into an atomic staging file.
///
/// This is the file-backed counterpart to [`write_bytes_atomic_sync`]. Large
/// callers already own a typed/`Value` tree, so materializing a second complete
/// `Vec<u8>` merely to pass it into the byte writer doubles peak heap without
/// strengthening the atomic publish boundary.
pub fn write_json_atomic_stream_sync<T: Serialize>(
    path: &Path,
    value: &T,
    max_bytes: usize,
) -> Result<(), ArtifactV2Error> {
    write_json_atomic_stream_with_format_sync(path, value, max_bytes, false)
}

/// Pretty-serialize JSON directly into an atomic staging file without first
/// retaining a payload-sized encoded buffer. This preserves the established
/// on-disk wire for stores that were historically written with
/// `serde_json::to_vec_pretty`.
pub fn write_json_pretty_atomic_stream_sync<T: Serialize>(
    path: &Path,
    value: &T,
    max_bytes: usize,
) -> Result<(), ArtifactV2Error> {
    write_json_atomic_stream_with_format_sync(path, value, max_bytes, true)
}

fn write_json_atomic_stream_with_format_sync<T: Serialize>(
    path: &Path,
    value: &T,
    max_bytes: usize,
    pretty: bool,
) -> Result<(), ArtifactV2Error> {
    if let Some(parent) = path.parent() {
        retry_transient_io_blocking(|| std::fs::create_dir_all(parent))?;
    }
    let tmp_path = path.with_file_name(format!(
        ".artifact-json-write-{}.tmp",
        Uuid::new_v4().simple()
    ));
    let written = (|| {
        use std::io::Write;

        struct BoundedWriter<'a, W> {
            inner: &'a mut W,
            bytes: usize,
            max_bytes: usize,
            exceeded: bool,
        }

        impl<'a, W: std::io::Write> std::io::Write for BoundedWriter<'a, W> {
            fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
                if self.bytes.saturating_add(buffer.len()) > self.max_bytes {
                    self.exceeded = true;
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::FileTooLarge,
                        "atomic JSON document exceeds its byte ceiling",
                    ));
                }
                let written = self.inner.write(buffer)?;
                self.bytes = self.bytes.saturating_add(written);
                Ok(written)
            }

            fn flush(&mut self) -> std::io::Result<()> {
                self.inner.flush()
            }
        }

        let mut file = retry_transient_io_blocking(|| std::fs::File::create(&tmp_path))?;
        let mut bounded = BoundedWriter {
            inner: &mut file,
            bytes: 0,
            max_bytes,
            exceeded: false,
        };
        let serialized = if pretty {
            serde_json::to_writer_pretty(&mut bounded, value)
        } else {
            serde_json::to_writer(&mut bounded, value)
        };
        if bounded.exceeded {
            return Err(ArtifactV2Error::InvalidRequest(
                "atomic JSON document exceeds its byte ceiling".to_string(),
            ));
        }
        serialized?;
        bounded.flush()?;
        drop(bounded);
        file.flush()?;
        file.sync_all()?;
        retry_transient_io_blocking(|| std::fs::rename(&tmp_path, path))?;
        sync_parent_dir_sync(path)?;
        Ok::<(), ArtifactV2Error>(())
    })();
    if written.is_err() {
        if let Err(cleanup) = std::fs::remove_file(&tmp_path) {
            warn_cleanup_failed(&tmp_path, &cleanup);
        }
    }
    written
}

/// Stack-safe canonical-`Value` variant used by raw result payloads. Generic
/// typed records use Serde directly; recursive `Value` trees use the shared
/// heap-stack canonical writer so the filesystem boundary does not reintroduce
/// native-stack growth.
pub fn write_canonical_json_value_atomic_stream_sync(
    path: &Path,
    value: &Value,
) -> Result<(), ArtifactV2Error> {
    if let Some(parent) = path.parent() {
        retry_transient_io_blocking(|| std::fs::create_dir_all(parent))?;
    }
    let tmp_path = path.with_file_name(format!(
        ".artifact-canonical-json-write-{}.tmp",
        Uuid::new_v4().simple()
    ));
    let written = (|| {
        use std::io::Write;

        let mut file = retry_transient_io_blocking(|| std::fs::File::create(&tmp_path))?;
        write_canonical_json(value, &mut file)?;
        file.flush()?;
        file.sync_all()?;
        retry_transient_io_blocking(|| std::fs::rename(&tmp_path, path))?;
        sync_parent_dir_sync(path)?;
        Ok::<(), ArtifactV2Error>(())
    })();
    if written.is_err() {
        if let Err(cleanup) = std::fs::remove_file(&tmp_path) {
            warn_cleanup_failed(&tmp_path, &cleanup);
        }
    }
    written
}

#[cfg(any(test, feature = "test-fixtures"))]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PersistedWriteOp {
    path: PathBuf,
    bytes: Vec<u8>,
}

#[cfg(any(test, feature = "test-fixtures"))]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PersistedWriteJournal {
    version: u32,
    writes: Vec<PersistedWriteOp>,
}

#[cfg(any(test, feature = "test-fixtures"))]
pub async fn recover_multi_write_journal(journal_path: &Path) -> Result<bool, ArtifactV2Error> {
    let journal = match retry_transient_io(|| fs::read_to_string(journal_path)).await {
        Ok(body) => serde_json::from_str::<PersistedWriteJournal>(&body)?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(ArtifactV2Error::Io(err)),
    };

    for write in &journal.writes {
        write_bytes_atomic(&write.path, &write.bytes).await?;
    }

    let permit = magician_core::blocking_admission::acquire_blocking_admission()
        .await
        .map_err(|err| {
            ArtifactV2Error::Runtime(format!("journal unlink parent-sync admission: {err}"))
        })?;
    retry_transient_io(|| fs::remove_file(journal_path)).await?;
    sync_parent_dir_with_admission(journal_path, permit).await?;
    Ok(true)
}

pub async fn sync_parent_dir(path: &Path) -> Result<(), ArtifactV2Error> {
    let permit = magician_core::blocking_admission::acquire_blocking_admission()
        .await
        .map_err(|err| ArtifactV2Error::Runtime(format!("sync_parent_dir admission: {err}")))?;
    sync_parent_dir_with_admission(path, permit).await
}

/// Parent-dir fsync using a permit already held by the caller. Use this after
/// a rename that must not wait on admission while the dest is already visible.
pub async fn sync_parent_dir_with_admission(
    path: &Path,
    permit: magician_core::blocking_admission::BlockingAdmissionPermit,
) -> Result<(), ArtifactV2Error> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let parent = parent.to_path_buf();
    magician_core::blocking_admission::spawn_blocking_with_admission(permit, move || {
        retry_transient_io_blocking(|| {
            let dir = std::fs::File::open(&parent)?;
            dir.sync_all()
        })
    })
    .await
    .map_err(|err| ArtifactV2Error::Runtime(format!("sync_parent_dir task: {err}")))??;
    Ok(())
}

#[allow(dead_code)]
fn sync_parent_dir_sync(path: &Path) -> Result<(), ArtifactV2Error> {
    Ok(sync_parent_dir_blocking(path)?)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn recover_multi_write_journal_replays_pending_writes_and_cleans_up() {
        let temp = tempdir().expect("tempdir");
        let alpha_path = temp.path().join("alpha.json");
        let beta_path = temp.path().join("nested").join("beta.json");
        let journal_path = temp.path().join("journals").join("task.write_journal.json");
        let journal = PersistedWriteJournal {
            version: 1,
            writes: vec![
                PersistedWriteOp {
                    path: alpha_path.clone(),
                    bytes: br#"{"value":"alpha"}"#.to_vec(),
                },
                PersistedWriteOp {
                    path: beta_path.clone(),
                    bytes: br#"{"value":"beta"}"#.to_vec(),
                },
            ],
        };
        write_json_atomic(&journal_path, &journal)
            .await
            .expect("journal should persist");

        assert!(recover_multi_write_journal(&journal_path)
            .await
            .expect("journal recovery should succeed"));
        assert_eq!(
            tokio::fs::read_to_string(&alpha_path)
                .await
                .expect("alpha payload should exist"),
            r#"{"value":"alpha"}"#
        );
        assert_eq!(
            tokio::fs::read_to_string(&beta_path)
                .await
                .expect("beta payload should exist"),
            r#"{"value":"beta"}"#
        );
        assert!(
            tokio::fs::metadata(&journal_path).await.is_err(),
            "recovery should remove the journal"
        );
    }

    #[tokio::test]
    async fn byte_atomic_writers_remove_staging_files_after_publish_failure() {
        let temp = tempdir().expect("tempdir");
        let destination_directory = temp.path().join("destination");
        std::fs::create_dir(&destination_directory).expect("destination directory");

        write_bytes_atomic(&destination_directory, b"async")
            .await
            .expect_err("a file cannot replace the destination directory");
        write_bytes_atomic_sync(&destination_directory, b"sync")
            .expect_err("a file cannot replace the destination directory");

        let leaked_staging = std::fs::read_dir(temp.path())
            .expect("temp listing")
            .flatten()
            .any(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with(".artifact-write-") && name.ends_with(".tmp")
            });
        assert!(
            !leaked_staging,
            "failed atomic writes must remove staging files"
        );
    }
}
