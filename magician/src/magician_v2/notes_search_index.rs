//! One-shot read of the notes files.
//!
//! Search does not keep this text. The caller uses it to see which notes
//! changed, then drops it. The searchable index is the LanceDB table.

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use super::notes::{is_markdown_note_path, is_observation_internal_directory, path_to_string};

const MAX_NOTE_BYTES: u64 = 256 * 1024;
const MAX_SCAN_ENTRIES: usize = 20_000;
/// One read stops here instead of copying an unbounded amount of note text.
const MAX_READ_TEXT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct NotesIndexRoot {
    pub provider: String,
    pub root: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct IndexedNote {
    pub provider: String,
    pub relative_path: String,
    pub absolute_path: PathBuf,
    pub markdown: String,
    pub modified_at_ms: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct NotesSearchSnapshot {
    pub scanned_notes: usize,
    pub scan_truncated: bool,
    pub notes: Vec<IndexedNote>,
}

struct ListedNote {
    provider: String,
    canonical_root: PathBuf,
    absolute_path: PathBuf,
    relative_path: String,
    modified_at_ms: i64,
}

struct ListedSpace {
    scanned_notes: usize,
    scan_truncated: bool,
    notes: Vec<ListedNote>,
}

/// Cheap identity of the markdown files under `roots`.
///
/// The stamp changes when a note is added, removed, resized, or touched. It
/// does not read file text. The watcher uses it to skip a quiet tree.
pub(crate) fn tree_stamp(roots: &[NotesIndexRoot]) -> io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    let listed = list_space(roots)?;
    hasher.update(&(listed.notes.len() as u64).to_le_bytes());
    for note in &listed.notes {
        hasher.update(note.provider.as_bytes());
        hasher.update(b"\n");
        hasher.update(note.relative_path.as_bytes());
        hasher.update(b"\0");
        hasher.update(&note.modified_at_ms.to_le_bytes());
    }
    if listed.scan_truncated {
        hasher.update(b"truncated");
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Read the notes once. The result is not retained after the caller drops it.
pub(crate) async fn snapshot_for(
    roots: Vec<NotesIndexRoot>,
) -> io::Result<Arc<NotesSearchSnapshot>> {
    tokio::task::spawn_blocking(move || snapshot_for_blocking(roots))
        .await
        .map_err(|error| io::Error::new(io::ErrorKind::Other, error.to_string()))?
}

fn snapshot_for_blocking(roots: Vec<NotesIndexRoot>) -> io::Result<Arc<NotesSearchSnapshot>> {
    let listed = list_space(&roots)?;
    Ok(Arc::new(read_listed(
        listed.notes,
        listed.scanned_notes,
        listed.scan_truncated,
    )))
}

fn list_space(roots: &[NotesIndexRoot]) -> io::Result<ListedSpace> {
    let mut notes = Vec::new();
    let mut scanned_notes = 0usize;
    let mut scan_truncated = false;
    let mut scanned_entries = 0usize;
    for root in roots {
        if scan_truncated {
            break;
        }
        let canonical_root = match fs::canonicalize(&root.root) {
            Ok(path) => path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let mut pending = VecDeque::from([canonical_root.clone()]);
        while let Some(directory) = pending.pop_front() {
            if scanned_entries >= MAX_SCAN_ENTRIES {
                scan_truncated = true;
                break;
            }
            let mut entries = fs::read_dir(&directory)?.collect::<Result<Vec<_>, _>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                if scanned_entries >= MAX_SCAN_ENTRIES {
                    scan_truncated = true;
                    break;
                }
                scanned_entries = scanned_entries.saturating_add(1);
                let file_type = entry.file_type()?;
                if file_type.is_symlink() {
                    continue;
                }
                if file_type.is_dir() {
                    if !is_observation_internal_directory(&entry.file_name()) {
                        pending.push_back(entry.path());
                    }
                    continue;
                }
                if !file_type.is_file() || !is_markdown_note_path(&entry.path()) {
                    continue;
                }
                let metadata = entry.metadata()?;
                if metadata.len() > MAX_NOTE_BYTES {
                    continue;
                }
                let relative = match entry.path().strip_prefix(&canonical_root) {
                    Ok(path) => path_to_string(path),
                    Err(_) => continue,
                };
                if relative.is_empty() || relative.chars().any(char::is_control) {
                    continue;
                }
                scanned_notes = scanned_notes.saturating_add(1);
                notes.push(ListedNote {
                    provider: root.provider.clone(),
                    canonical_root: canonical_root.clone(),
                    absolute_path: entry.path(),
                    relative_path: relative,
                    modified_at_ms: modified_at_ms(&metadata),
                });
            }
        }
    }

    Ok(ListedSpace {
        scanned_notes,
        scan_truncated,
        notes,
    })
}

fn read_listed(
    listed: Vec<ListedNote>,
    scanned_notes: usize,
    mut scan_truncated: bool,
) -> NotesSearchSnapshot {
    let mut notes = Vec::with_capacity(listed.len());
    let mut read_bytes = 0usize;
    for note in listed {
        let Some(markdown) = read_note(&note) else {
            continue;
        };
        let next_bytes = read_bytes.saturating_add(markdown.1.len());
        if next_bytes > MAX_READ_TEXT_BYTES {
            scan_truncated = true;
            break;
        }
        read_bytes = next_bytes;
        notes.push(IndexedNote {
            provider: note.provider,
            relative_path: note.relative_path,
            absolute_path: markdown.0,
            markdown: markdown.1,
            modified_at_ms: note.modified_at_ms,
        });
    }
    NotesSearchSnapshot {
        scanned_notes,
        scan_truncated,
        notes,
    }
}

fn read_note(note: &ListedNote) -> Option<(PathBuf, String)> {
    let metadata = fs::symlink_metadata(&note.absolute_path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_NOTE_BYTES {
        return None;
    }
    let canonical = fs::canonicalize(&note.absolute_path).ok()?;
    if !canonical.starts_with(&note.canonical_root) {
        return None;
    }
    let bytes = fs::read(&canonical).ok()?;
    if bytes.len() as u64 > MAX_NOTE_BYTES {
        return None;
    }
    let markdown = String::from_utf8(bytes).ok()?;
    Some((canonical, markdown))
}

fn modified_at_ms(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(1)
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_is_not_kept_and_an_edit_is_visible_on_the_next_read() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("a.md"), b"# A\n\nhello").unwrap();
        let roots = vec![NotesIndexRoot {
            provider: "local_markdown".into(),
            root: temp.path().to_path_buf(),
        }];

        let first = snapshot_for_blocking(roots.clone()).unwrap();
        let second = snapshot_for_blocking(roots.clone()).unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(first.scanned_notes, 1);
        assert_eq!(second.notes[0].markdown, "# A\n\nhello");

        fs::write(temp.path().join("a.md"), b"# A\n\nhello!").unwrap();
        let third = snapshot_for_blocking(roots).unwrap();
        assert_eq!(third.notes[0].markdown, "# A\n\nhello!");
    }
}
