//! Reading append-only JSONL logs honestly.
//!
//! Every store in this crate keeps its state as an append-only log and folds it
//! on read. Two failure semantics were wrong in the first cut of several of
//! them, found by adversarial review, and this module is the single place both
//! are decided so no store can drift from the others:
//!
//! # 1. Absent is the only error that means "empty"
//!
//! The original pattern was `Err(_) => Ok(None)`: EVERY read failure — EACCES,
//! EIO, invalid UTF-8 — folded to an empty store. That fails **open**: a
//! revocation against an unreadable log reports success while revoking nothing;
//! a one-per-identity guard passes vacuously because the identity's existing
//! grant could not be read; a presentation refusal claims `UnknownSecret` when
//! the truth was a disk fault. [`read_log_if_present`] maps **only**
//! `NotFound` to `None` and propagates everything else, so an I/O fault is an
//! error the caller sees, never an absence the caller trusts.
//!
//! # 2. A torn tail is an append that never happened; a torn middle is corruption
//!
//! A completed append writes `line + \n` in one call, so a crash mid-append
//! leaves exactly one torn line, at the tail, **unterminated**. That is the one
//! shape [`parse_log_lines`] tolerates, and the trailing newline is what
//! distinguishes it: an unparseable line that IS newline-terminated was an
//! acknowledged write, so dropping it would un-record an operation the caller
//! was told succeeded. Every store's ordering (record-before-act,
//! index-before-row, debit-before-act) is what makes "the torn operation never
//! happened" the fail-closed reading of the tolerated case.
//!
//! Everything else refuses. An unparseable interior line, a terminated one, or
//! a FUSED one — the fragment plus the next append run together after a crash —
//! stops the fold. [`append_log_line`] deliberately does not repair the fused
//! case: splitting it means scanning for an offset whose suffix happens to
//! parse, and a wrong split that parses would admit a record nobody wrote. A
//! silently skipped or invented line could be a revocation, a settlement or a
//! debit, so this module refuses and asks for an operator instead.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;

use crate::magician_v2::artifact_v2::service::ArtifactV2Error;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

/// Read a log file, distinguishing "absent" from "unreadable".
///
/// `Ok(None)` means the file does not exist — the one case that legitimately
/// reads as an empty store. Every other failure propagates.
pub fn read_log_if_present(workspace: &ArtifactV2Workspace, path: &Path) -> Result<Option<String>> {
    match workspace.read_to_string_path_sync(path) {
        Ok(content) => Ok(Some(content)),
        Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| {
            format!(
                "reading {} — an unreadable log must never be treated as an empty one",
                path.display()
            )
        }),
    }
}

/// Every `.jsonl` log in one directory, or an empty list only when the
/// directory does not exist.
///
/// The third place the absent/unreadable distinction has to be made, and it
/// lives here with the other two for the same reason they do. Several stores
/// hash their log file names — a run log is `blake3(run_id)`, a negotiation
/// log is `blake3(audience_key)`, a register is `blake3(audience_key)` — so
/// the only way to answer *"what does this scope hold"* is to list the
/// directory and fold each file. A listing that failed open would answer
/// *"this scope has no runs"*, *"no negotiations"*, *"nothing is owed"* from a
/// permissions fault or a mount that had gone away, which is precisely the
/// fail-open [`read_log_if_present`] exists to close, one layer up.
///
/// `NotFound` on the directory is the one condition that reads as empty: a
/// scope that has never written a log has no directory. Everything else
/// propagates.
///
/// Directories are skipped, and so is any file that is not a `.jsonl` — a
/// store's own index subdirectory and a stray editor swap file are neither of
/// them logs, and folding one would fail the whole listing.
///
/// Sorted by file name, so two listings over an unchanged directory agree and
/// the caller's own ordering is applied to a stable input.
pub fn list_log_paths(workspace: &ArtifactV2Workspace, dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match workspace.read_dir_path_sync(dir) {
        Ok(entries) => entries,
        Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Vec::new());
        },
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "listing {} — an unlistable log directory must never be treated as a scope \
                     that holds nothing",
                    dir.display()
                )
            });
        },
    };
    let mut names: Vec<String> = entries
        .into_iter()
        .filter(|entry| !entry.is_dir && entry.file_name.ends_with(".jsonl"))
        .map(|entry| entry.file_name)
        .collect();
    names.sort();
    // Joined onto the directory the caller resolved, never onto
    // `entry.relative_path`: the listing is provider-relative and the stores
    // address logs by resolved path.
    Ok(names.into_iter().map(|name| dir.join(name)).collect())
}

/// Parse every line of an append-only log, tolerating a genuine torn tail only.
///
/// The discriminator is the trailing newline. `append_log_line` writes
/// `line + \n` as one call, so a **completed** append always leaves the file
/// ending in `\n` — which means an unparseable final line is a torn append
/// *only when the raw log does not end in a newline*. An unparseable final line
/// that IS newline-terminated was a fully acknowledged write and is corruption,
/// exactly like an interior one: dropping it would silently un-record an
/// operation the caller was told succeeded (second-round review found the first
/// cut doing precisely that).
pub fn parse_log_lines<T: DeserializeOwned>(raw: &str, path: &Path) -> Result<Vec<T>> {
    // Anchored to the last line OF THE FILE, not the last line that survives the
    // blank filter below. Those differ when the file ends in a whitespace-only
    // unterminated fragment, and the exemption would then be handed to the
    // complete, newline-terminated record above it — dropping real corruption as
    // though it were a torn write, which is the exact failure the terminated/
    // unterminated discriminator exists to prevent. If the final raw line is
    // blank, nothing parseable was torn and no surviving line has earned it.
    let tail_may_be_torn = !raw.is_empty()
        && !raw.ends_with('\n')
        && raw
            .lines()
            .next_back()
            .is_some_and(|line| !line.trim().is_empty());
    let numbered: Vec<(usize, &str)> = raw
        .lines()
        .enumerate()
        .map(|(physical, line)| (physical + 1, line.trim()))
        .filter(|(_, line)| !line.is_empty())
        .collect();

    let mut out = Vec::with_capacity(numbered.len());
    let last = numbered.len().saturating_sub(1);
    for (index, (physical_line, line)) in numbered.iter().enumerate() {
        match serde_json::from_str::<T>(line) {
            Ok(record) => out.push(record),
            Err(_) if index == last && tail_may_be_torn => break,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "unparseable record at {}:{} — a torn append is unterminated and can \
                         only be the final line; a terminated or interior failure is corruption \
                         and the fold refuses to guess past it",
                        path.display(),
                        physical_line
                    )
                });
            },
        }
    }
    Ok(out)
}

/// Append one record to a log.
///
/// One append, no read, no lock — which is what lets every store honestly say
/// its appenders are unlocked. The first cut healed a torn tail here by reading
/// the whole log and rewriting its terminated prefix. That was wrong twice, and
/// a standing review found both:
///
/// * it read the ENTIRE log on every append, turning an O(1) write into O(n)
///   across all fourteen store call sites; and
/// * a read-modify-rewrite is not lock-free, so the module promised something
///   the code had stopped doing. There is no cheap fix behind this abstraction:
///   the file provider offers no sync tail-read and no truncate, only a full
///   atomic rewrite.
///
/// # What happens after a crash, and why refusing is the right answer
///
/// A crash mid-append leaves an unterminated fragment. The NEXT append fuses
/// onto it, producing one line of `<fragment><record>` that no parser can read.
/// [`parse_log_lines`] sees a terminated line that will not parse and REFUSES
/// the whole fold, exactly as it does for corruption anywhere else.
///
/// That is deliberate, and it is the fail-closed reading. The alternative —
/// splitting the fused line and recovering the record after the fragment — is a
/// guess: it means scanning for an offset whose suffix happens to parse, and a
/// wrong split that parses would silently admit a record nobody wrote. This
/// crate refuses rather than guesses when a log says something it cannot mean,
/// and a settlement or a revocation is exactly the kind of record where a
/// plausible guess is worse than a loud stop.
///
/// The cost is real and belongs in the open: a crash mid-append leaves that one
/// log unreadable until an operator removes the fused line. Nothing is lost —
/// every record before the tear is intact and the torn one never completed.
pub fn append_log_line(workspace: &ArtifactV2Workspace, path: &Path, line: &[u8]) -> Result<()> {
    let mut bytes = Vec::with_capacity(line.len() + 1);
    bytes.extend_from_slice(line);
    if !line.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    workspace
        .append_path_sync(path, &bytes)
        .with_context(|| format!("appending {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Deserialize)]
    struct Row {
        n: u32,
    }

    /// The torn-tail rule: an UNTERMINATED partial final line is an append that
    /// never happened, and the fold reads everything before it.
    #[test]
    fn a_torn_final_line_is_dropped_not_fatal() {
        let raw = "{\"n\":1}\n{\"n\":2}\n{\"n\":3";
        let rows: Vec<Row> = parse_log_lines(raw, Path::new("log")).expect("tolerant tail");
        assert_eq!(rows, vec![Row { n: 1 }, Row { n: 2 }]);
    }

    /// The discriminator: a NEWLINE-TERMINATED final line that fails to parse
    /// was a completed, acknowledged write — corruption, not a tear. Dropping
    /// it would silently un-record an operation the caller was told succeeded
    /// (a settlement, a revocation), which is the fail-open this module exists
    /// to close.
    #[test]
    fn a_terminated_unparseable_final_line_is_corruption_not_a_tear() {
        let raw = "{\"n\":1}\ngarbage\n";
        let error = parse_log_lines::<Row>(raw, Path::new("log")).expect_err("terminated failure");
        assert!(error.to_string().contains("corruption"));
    }

    /// After a crash, the next append FUSES onto the fragment — and the fold
    /// refuses rather than guessing which half is the record.
    ///
    /// This is the case the write path deliberately does not repair. Splitting
    /// `<fragment><record>` means scanning for an offset whose suffix parses,
    /// and a wrong split that happens to parse would admit a record nobody
    /// wrote. A loud stop is the right answer for a log that may hold a
    /// settlement or a revocation.
    #[test]
    fn a_fused_line_refuses_rather_than_being_split() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace =
            crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(tmp.path());
        let path = tmp.path().join("log.jsonl");

        append_log_line(&workspace, &path, b"{\"n\":1}").expect("first");
        // A crash mid-append: an unterminated fragment.
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("open");
            file.write_all(b"{\"n\":2").expect("tear");
        }
        // The next append lands directly after it, fusing the two.
        append_log_line(&workspace, &path, b"{\"n\":3}").expect("append after the tear");

        let raw = read_log_if_present(&workspace, &path)
            .expect("read")
            .expect("present");
        let error = parse_log_lines::<Row>(&raw, &path).expect_err("a fused line must refuse");
        assert!(error.to_string().contains("corruption"), "{error}");

        // And the record written BEFORE the tear is still there to recover — a
        // refusal is not data loss, it is a demand for an operator.
        assert!(raw.starts_with("{\"n\":1}\n"), "{raw}");
    }

    /// The interior rule: the write protocol cannot tear a middle line, so one
    /// is corruption and the fold refuses rather than resurrecting whatever the
    /// unreadable record ended.
    #[test]
    fn an_unparseable_interior_line_is_an_error() {
        let raw = "{\"n\":1}\ngarbage\n{\"n\":3}\n";
        let error = parse_log_lines::<Row>(raw, Path::new("log")).expect_err("interior corruption");
        assert!(error.to_string().contains("interior"));
    }

    /// Clean logs parse completely; empty and blank-line-only logs are empty.
    #[test]
    fn clean_and_empty_logs_fold_exactly() {
        let rows: Vec<Row> =
            parse_log_lines("{\"n\":1}\n\n{\"n\":2}\n", Path::new("log")).expect("clean");
        assert_eq!(rows, vec![Row { n: 1 }, Row { n: 2 }]);
        let none: Vec<Row> = parse_log_lines("", Path::new("log")).expect("empty");
        assert!(none.is_empty());
        let blank: Vec<Row> = parse_log_lines("\n  \n", Path::new("log")).expect("blank");
        assert!(blank.is_empty());
    }

    /// REGRESSION GUARD. The tear exemption belongs to the last line of the
    /// FILE. It used to be anchored to the last line that survived the blank
    /// filter, so a log ending in a whitespace-only unterminated fragment lent
    /// it to the complete, newline-terminated record above — and a corrupt
    /// record there was silently dropped instead of refused. That is the one
    /// outcome this fold's whole terminated/unterminated rule exists to prevent:
    /// the record was fully acknowledged to its writer.
    ///
    /// Against the old code this returns `Ok` with one row.
    #[test]
    fn a_blank_final_fragment_does_not_lend_its_exemption_to_the_record_above() {
        let raw = "{\"n\":1}\ngarbage\n   ";
        let error = parse_log_lines::<Row>(raw, Path::new("log"))
            .expect_err("a terminated unparseable record is corruption, blank fragment or not");
        assert!(error.to_string().contains("corruption"), "{error}");

        // The genuine tear is still tolerated: same shape, non-blank fragment.
        let rows: Vec<Row> =
            parse_log_lines("{\"n\":1}\n{\"n\":", Path::new("log")).expect("a real torn tail");
        assert_eq!(rows, vec![Row { n: 1 }]);
    }

    /// A single torn line is a log where nothing has happened yet.
    #[test]
    fn a_log_holding_only_a_torn_line_is_empty() {
        let rows: Vec<Row> = parse_log_lines("{\"n\":", Path::new("log")).expect("tolerant");
        assert!(rows.is_empty());
    }

    /// The interior error names the PHYSICAL line, blank lines included, so the
    /// message points at what an operator will actually see in the file.
    #[test]
    fn the_corruption_error_names_the_physical_line() {
        let raw = "{\"n\":1}\n\ngarbage\n{\"n\":3}\n";
        let error = parse_log_lines::<Row>(raw, Path::new("log")).expect_err("interior");
        assert!(error.to_string().contains(":3"), "{error}");
    }

    /// A directory listing names every log and nothing else, and an absent
    /// directory is the ONLY thing that reads as empty.
    ///
    /// Pins the fail-open one layer above `read_log_if_present`: several stores
    /// hash their file names, so a listing is the only way to enumerate what a
    /// scope holds — and a listing that swallowed a fault would answer "this
    /// scope has no runs" or "nothing is owed" from a permissions error.
    #[test]
    fn a_log_listing_names_only_logs_and_absent_is_the_only_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace =
            crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(tmp.path());

        let dir = tmp.path().join("register");
        assert!(
            list_log_paths(&workspace, &dir)
                .expect("an absent directory is empty")
                .is_empty(),
            "a store that has never written has no directory, and that is empty"
        );

        std::fs::create_dir_all(dir.join("index")).expect("a nested index directory");
        append_log_line(&workspace, &dir.join("b.jsonl"), b"{\"n\":2}").expect("second log");
        append_log_line(&workspace, &dir.join("a.jsonl"), b"{\"n\":1}").expect("first log");
        std::fs::write(dir.join("notes.txt"), "not a log").expect("a stray file");

        let listed = list_log_paths(&workspace, &dir).expect("list");
        assert_eq!(
            listed,
            vec![dir.join("a.jsonl"), dir.join("b.jsonl")],
            "sorted by name, logs only — a directory or a stray file is not a log"
        );

        // The listing is a real read of real files, so the fold works on it.
        let raw = read_log_if_present(&workspace, &listed[0])
            .expect("read")
            .expect("present");
        let rows: Vec<Row> = parse_log_lines(&raw, &listed[0]).expect("fold");
        assert_eq!(rows, vec![Row { n: 1 }]);
    }
}
