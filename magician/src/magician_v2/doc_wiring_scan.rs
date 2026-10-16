//! Source-level wiring facts, for tests that pin what a module header claims.
//!
//! A module header that says *"this runs on a cadence"* or *"every send passes
//! through here"* is asserting a **call site**, and a call site is not something
//! a unit test over the module's own functions can observe. Several headers in
//! this tree drifted exactly there: the code kept working, the caller never
//! arrived, and the prose went on describing the intent.
//!
//! So a header that makes a wiring claim gets a test that reads the crate's own
//! source and asserts the claim as a **count**. When somebody finally wires the
//! thing up, the test fails and points at the sentence that has to change.
//!
//! # Comment lines are skipped
//!
//! A header that names the symbol it is talking about must not count as a use
//! of it. Any line whose first non-whitespace characters are `//` is ignored,
//! which covers `//`, `///` and `//!` alike. Block comments are not handled —
//! nothing in this tree uses them for prose — so a `/* … */` mention would
//! count as a hit, which is the fail-closed direction: a spurious hit fails a
//! test, a missed one would pass it.
//!
//! # An empty search is a failed search
//!
//! [`SourceScan::files_searched`] is returned alongside the hits so every caller
//! can assert it is non-zero. "No file contained the needle" and "no file was
//! read" produce the same empty `hits`, and only one of them proves anything.
//!
//! # A claim about the workspace must be checked against the workspace
//!
//! [`scan`] reads this crate only. That is the wrong scope for *"nothing calls
//! this"*, and getting it wrong is not hypothetical: the data room's own header
//! asserted it had no reader-facing path while `magician-api` was serving one
//! and `magician-bin` was mounting its routes. [`scan_workspace`] walks every
//! member crate named in the workspace manifest, so a member added later is
//! searched without anybody remembering to add it here.

use std::path::{Path, PathBuf};

/// What one scan found.
pub struct SourceScan {
    /// How many `.rs` files were actually read. Zero means the scan proved
    /// nothing, whatever `hits` says.
    pub files_searched: usize,
    /// `src`-relative paths of files holding the needle on a non-comment line,
    /// sorted, each listed once however many times it matched.
    pub hits: Vec<String>,
}

/// Whether `source` uses `needle` somewhere other than a comment.
///
/// Split out from the walk so the rule that makes every scan below meaningful
/// is itself testable without a filesystem.
fn uses_outside_comments(source: &str, needle: &str) -> bool {
    source
        .lines()
        .any(|line| !line.trim_start().starts_with("//") && line.contains(needle))
}

/// This crate's `src` directory, resolved from the manifest rather than from the
/// process's working directory — which the test harness does not promise.
fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// The workspace root — this crate's parent.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("this crate lives inside the workspace")
        .to_path_buf()
}

/// Every member crate's `src` directory, read from the workspace manifest.
///
/// Parsed rather than listed, so a member added after this file was written is
/// still searched. A member with no `src` directory is skipped; the caller's
/// `files_searched` guard is what catches a parse that found nothing.
fn member_src_dirs() -> Vec<PathBuf> {
    let manifest_path = workspace_root().join("Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|error| panic!("reading {}: {error}", manifest_path.display()));
    let Some(after) = manifest.split_once("members = [") else {
        panic!(
            "{} has no `members = [` list; a workspace-wide scan cannot be built from it",
            manifest_path.display()
        );
    };
    let Some((body, _)) = after.1.split_once(']') else {
        panic!(
            "the `members` list in {} is unterminated",
            manifest_path.display()
        );
    };
    let mut dirs = Vec::new();
    for raw in body.split(',') {
        let name = raw.trim().trim_matches('"').trim();
        if name.is_empty() {
            continue;
        }
        let src = workspace_root().join(name).join("src");
        if src.is_dir() {
            dirs.push(src);
        }
    }
    dirs
}

/// [`scan`], over every member crate in the workspace.
///
/// Paths in [`SourceScan::hits`] are workspace-relative
/// (`magician-api/src/…`), and `skip` matches against those, so an entry has to
/// name the crate as well as the file.
pub fn scan_workspace(needle: &str, skip: &[&str]) -> SourceScan {
    let roots = member_src_dirs();
    assert!(
        roots.len() >= 5,
        "only {} member crates were resolved; a workspace scan built from that proves nothing",
        roots.len()
    );
    let mut scan = SourceScan {
        files_searched: 0,
        hits: Vec::new(),
    };
    for root in &roots {
        walk_from(&workspace_root(), root, needle, skip, &mut scan);
    }
    scan.hits.sort();
    scan
}

/// Read one workspace-relative source subtree.
///
/// This is the split-crate counterpart to [`scan`]: ownership tests can keep
/// their narrow subtree boundary after a module moves to a sibling crate,
/// instead of widening the assertion to every source file in the workspace.
pub(crate) fn scan_workspace_subtree(root_rel: &str, needle: &str, skip: &[&str]) -> SourceScan {
    let root = workspace_root().join(root_rel);
    assert!(
        root.is_dir(),
        "the workspace scan root {} does not exist, so this scan would prove nothing",
        root.display()
    );
    let mut scan = SourceScan {
        files_searched: 0,
        hits: Vec::new(),
    };
    walk_from(&workspace_root(), &root, needle, skip, &mut scan);
    scan.hits.sort();
    scan
}

/// Read every `.rs` file under `src/<root_rel>` and report which ones use
/// `needle` outside a comment.
///
/// `root_rel` is `""` for the whole crate. `skip` holds `src`-relative path
/// suffixes to leave out — the needle's own definition site, and the module
/// whose header is making the claim.
pub(crate) fn scan(root_rel: &str, needle: &str, skip: &[&str]) -> SourceScan {
    let root = if root_rel.is_empty() {
        src_root()
    } else {
        src_root().join(root_rel)
    };
    assert!(
        root.is_dir(),
        "the scan root {} does not exist, so this scan would prove nothing",
        root.display()
    );
    let mut scan = SourceScan {
        files_searched: 0,
        hits: Vec::new(),
    };
    walk(&root, needle, skip, &mut scan);
    scan.hits.sort();
    scan
}

fn walk(dir: &Path, needle: &str, skip: &[&str], scan: &mut SourceScan) {
    walk_from(&src_root(), dir, needle, skip, scan);
}

fn walk_from(base: &Path, dir: &Path, needle: &str, skip: &[&str], scan: &mut SourceScan) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|error| panic!("reading {}: {error}", dir.display()));
    for entry in entries {
        let path = entry.expect("a directory entry reads").path();
        if path.is_dir() {
            walk_from(base, &path, needle, skip, scan);
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        let relative = path
            .strip_prefix(base)
            .expect("every scanned file lives under the scan base")
            .to_string_lossy()
            .replace('\\', "/");
        if skip.iter().any(|excluded| relative.ends_with(excluded)) {
            continue;
        }
        scan.files_searched += 1;
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
        if uses_outside_comments(&source, needle) {
            scan.hits.push(relative);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scanner must find a symbol that is genuinely used, or every test
    /// built on it passes vacuously.
    ///
    /// Pins the failure the whole helper would otherwise have: a walk that
    /// silently read nothing — a wrong root, a missed subdirectory — returns an
    /// empty `hits` that reads exactly like "nobody calls this".
    #[test]
    fn the_scanner_finds_a_symbol_that_is_really_used() {
        let found = scan("", "ArtifactV2Workspace", &[]);
        assert!(
            found.files_searched > 100,
            "only {} files were read; the walk is not reaching the tree",
            found.files_searched
        );
        assert!(
            found.hits.len() > 10,
            "a type used across the tree matched only {} files",
            found.hits.len()
        );
    }

    /// A name that appears only in prose is not a use of it.
    ///
    /// Without this rule every "nothing calls X" assertion in this tree would
    /// fail the moment a header mentioned X — and the natural fix would be to
    /// delete the sentence rather than the call, which is the wrong direction
    /// entirely.
    ///
    /// The fixtures use a nonsense token rather than a real symbol on purpose:
    /// a needle spelled here the way a caller would spell it turns this file
    /// into a hit for every "nothing calls X" scan in the tree.
    #[test]
    fn a_name_that_appears_only_in_a_comment_is_not_a_use() {
        const NEEDLE: &str = "ZzUnusedProbe";
        assert!(!uses_outside_comments(
            "// ZzUnusedProbe is never called\n",
            NEEDLE
        ));
        assert!(!uses_outside_comments(
            "/// [`ZzUnusedProbe`] has no caller\n",
            NEEDLE
        ));
        assert!(!uses_outside_comments(
            "//! and nothing calls ZzUnusedProbe\n",
            NEEDLE
        ));
        // An indented comment is still a comment.
        assert!(!uses_outside_comments("        // ZzUnusedProbe\n", NEEDLE));

        assert!(uses_outside_comments(
            "    ZzUnusedProbe::new(layout);\n",
            NEEDLE
        ));
        // A trailing comment does not rescue a line from being a use.
        assert!(uses_outside_comments(
            "    let it = ZzUnusedProbe; // why\n",
            NEEDLE
        ));
    }

    /// A scan rooted somewhere that does not exist must fail loudly.
    ///
    /// The whole helper reports absence, so the one thing it must never do is
    /// report absence because it looked in the wrong place.
    #[test]
    #[should_panic(expected = "does not exist")]
    fn a_root_that_is_not_there_panics_rather_than_reporting_nothing() {
        scan("magician_v2/no_such_module_dir", "anything", &[]);
    }
}
