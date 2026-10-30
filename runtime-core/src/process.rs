//! Program resolution for child processes.
//!
//! On macOS the Magician process is heavily multithreaded and talks to XPC
//! (keychain, notifications). Rust's `Command::spawn` uses `posix_spawn`
//! unless the program is a bare name that needs PATH lookup while the
//! command also touches the child's `PATH` — `env("PATH", ..)`,
//! `env_remove("PATH")`, or `env_clear()` on its own — then it must `fork`
//! and resolve inside the child, and a forked copy of this process can
//! fault in the system's atfork handlers before it ever reaches `exec`,
//! leaving the parent blocked on the exec-status pipe forever. Resolving
//! the program to a path first keeps every spawn on `posix_spawn`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Resolve `program` against `path_env` (the PATH the child will see; the
/// current process PATH when `None`).
///
/// A program that already names a location (absolute, or relative with a
/// separator) is returned unchanged. A bare name found on the searched PATH
/// becomes the absolute path of the first executable file. A bare name that
/// is NOT found becomes `<first non-empty PATH dir>/<program>` — a path that
/// names no executable — so the spawn stays on `posix_spawn` and fails with
/// the ordinary not-found error instead of forking to repeat the search in
/// the child. Only when there is nothing to search (no PATH at all, or one
/// with no non-empty dir) is the bare name returned unchanged.
pub fn resolve_program(program: &OsStr, path_env: Option<&OsStr>) -> PathBuf {
    let unchanged = || PathBuf::from(program);
    if program.is_empty() || names_a_path(program) {
        return unchanged();
    }
    let search = match path_env {
        Some(path) => path.to_os_string(),
        None => match std::env::var_os("PATH") {
            Some(path) => path,
            None => return unchanged(),
        },
    };
    let dirs: Vec<PathBuf> = std::env::split_paths(&search)
        .filter(|dir| !dir.as_os_str().is_empty())
        .collect();
    let Some(first_dir) = dirs.first() else {
        return unchanged();
    };
    dirs.iter()
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable_file(candidate))
        .unwrap_or_else(|| first_dir.join(program))
}

/// [`resolve_program`] for callers holding `str` values.
pub fn resolve_program_str(program: &str, path_env: Option<&str>) -> PathBuf {
    resolve_program(OsStr::new(program), path_env.map(OsStr::new))
}

/// True when the program already names a location (absolute, or relative
/// with at least one separator) rather than a bare name for PATH lookup.
/// Mirrors the test the standard library applies before choosing `fork`.
fn names_a_path(program: &OsStr) -> bool {
    let path = Path::new(program);
    path.is_absolute()
        || path.components().count() > 1
        || program
            .to_string_lossy()
            .contains(['/', std::path::MAIN_SEPARATOR])
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Names the git binary Magician runs for its own git work, overriding the
/// lookup in [`git_program`].
pub const GIT_PROGRAM_ENV: &str = "MAGICIAN_GIT";

/// Where Magician's own git came from, for status lines and project health.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitSource {
    /// `MAGICIAN_GIT`.
    Override,
    /// The host's git on PATH.
    System,
    /// The standalone git the `dugite` skill ships under skillshub.
    Bundled,
    /// None of the above: the path names no executable.
    Missing,
}

/// The git binary Magician runs itself: trusted workspaces, checkpoints,
/// pull requests, a project's git init.
///
/// `MAGICIAN_GIT` wins. Then the host's git, because it carries the owner's
/// credential helpers for push. On macOS `/usr/bin/git` is only a shim that
/// asks to install the developer tools, so it counts only when they are
/// installed. Then the bundled git from the `dugite` skill
/// (`<skillshub>/dugite/bin/git`, or its npm install), so a host without git
/// still has one. Always an absolute path when one exists, so a spawn that
/// touches `PATH` stays on `posix_spawn`.
pub fn git_program() -> PathBuf {
    resolve_git().0
}

/// [`git_program`] and where it came from.
pub fn resolve_git() -> (PathBuf, GitSource) {
    let override_path = std::env::var_os(GIT_PROGRAM_ENV).filter(|value| !value.is_empty());
    let system = resolve_program(OsStr::new("git"), None);
    let skillshub = std::env::var_os("MAGICIAN_SKILLSHUB_ROOT")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|dir| dir.join("skillshub"))
        });
    pick_git(
        override_path.map(PathBuf::from),
        system,
        &macos_developer_dirs(),
        skillshub.as_deref(),
    )
}

/// Whether Magician has a git to run at all.
pub fn git_available() -> bool {
    resolve_git().1 != GitSource::Missing
}

fn pick_git(
    override_path: Option<PathBuf>,
    system: PathBuf,
    developer_dirs: &[PathBuf],
    skillshub: Option<&Path>,
) -> (PathBuf, GitSource) {
    if let Some(path) = override_path {
        return (path, GitSource::Override);
    }
    let system_works = is_executable_file(&system)
        && (!cfg!(target_os = "macos")
            || system != Path::new("/usr/bin/git")
            || developer_dirs
                .iter()
                .any(|dir| is_executable_file(&dir.join("usr/bin/git"))));
    if system_works {
        return (system, GitSource::System);
    }
    let bundled = skillshub.and_then(|hub| {
        [
            hub.join("dugite/bin/git"),
            hub.join("node_modules/dugite/git/bin/git"),
        ]
        .into_iter()
        .find(|candidate| is_executable_file(candidate))
    });
    match bundled {
        // Canonical, so git finds its helpers beside the real binary.
        Some(path) => (
            std::fs::canonicalize(&path).unwrap_or(path),
            GitSource::Bundled,
        ),
        None => (system, GitSource::Missing),
    }
}

/// Where macOS keeps the developer tools the `/usr/bin/git` shim forwards to,
/// read without running `xcode-select` (the shim itself would offer a
/// install dialog).
fn macos_developer_dirs() -> Vec<PathBuf> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os("DEVELOPER_DIR").filter(|value| !value.is_empty()) {
        dirs.push(PathBuf::from(dir));
    }
    if let Ok(dir) = std::fs::read_link("/var/db/xcode_select_link") {
        dirs.push(dir);
    }
    dirs.push(PathBuf::from("/Library/Developer/CommandLineTools"));
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn write_file(dir: &Path, name: &str, mode: u32) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write");
        let mut perms = fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(mode);
        fs::set_permissions(&path, perms).expect("chmod");
        path
    }

    fn joined(dirs: &[&Path]) -> std::ffi::OsString {
        std::env::join_paths(dirs.iter().copied()).expect("join_paths")
    }

    #[test]
    fn absolute_program_passes_through_untouched() {
        let temp = tempfile::tempdir().expect("tempdir");
        let program = temp.path().join("tool");
        let resolved = resolve_program(program.as_os_str(), Some(OsStr::new("/nonexistent")));
        assert_eq!(resolved, program);
    }

    #[test]
    fn relative_program_with_separator_passes_through_untouched() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_file(temp.path(), "tool", 0o755);
        let search = joined(&[temp.path()]);
        for form in ["./tool", "bin/tool", "tool/"] {
            let resolved = resolve_program(OsStr::new(form), Some(&search));
            assert_eq!(
                resolved,
                PathBuf::from(form),
                "form {form:?} must not be resolved"
            );
        }
    }

    #[test]
    fn bare_name_resolves_in_first_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::create_dir_all(&first).expect("first");
        fs::create_dir_all(&second).expect("second");
        let wanted = write_file(&first, "tool", 0o755);
        write_file(&second, "tool", 0o755);
        let resolved = resolve_program(OsStr::new("tool"), Some(&joined(&[&first, &second])));
        assert_eq!(resolved, wanted);
        assert!(resolved.is_absolute());
    }

    #[test]
    fn bare_name_resolves_in_a_later_dir_when_the_first_lacks_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::create_dir_all(&first).expect("first");
        fs::create_dir_all(&second).expect("second");
        write_file(&first, "other", 0o755);
        let wanted = write_file(&second, "tool", 0o755);
        let resolved = resolve_program(OsStr::new("tool"), Some(&joined(&[&first, &second])));
        assert_eq!(resolved, wanted);
    }

    #[test]
    fn non_executable_file_is_skipped_in_favour_of_a_later_executable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::create_dir_all(&first).expect("first");
        fs::create_dir_all(&second).expect("second");
        write_file(&first, "tool", 0o644);
        let wanted = write_file(&second, "tool", 0o755);
        let resolved = resolve_program(OsStr::new("tool"), Some(&joined(&[&first, &second])));
        assert_eq!(resolved, wanted);
    }

    #[test]
    fn directory_named_like_the_program_is_not_a_match() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(temp.path().join("tool")).expect("dir");
        let resolved = resolve_program(OsStr::new("tool"), Some(&joined(&[temp.path()])));
        // No executable matched, so the name is pinned under the first dir:
        // the spawn reports that path as not runnable instead of forking.
        assert_eq!(resolved, temp.path().join("tool"));
        assert!(!is_executable_file(&resolved));
    }

    /// A bare name absent from an overridden PATH must still reach the OS
    /// as a path: left bare, the PATH override would make std `fork` to
    /// search it in the child, so a missing CLI would hang instead of
    /// failing with not-found.
    #[test]
    fn missing_bare_name_becomes_a_nonexistent_path_under_the_first_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::create_dir_all(&first).expect("first");
        fs::create_dir_all(&second).expect("second");
        let resolved = resolve_program(
            OsStr::new("no-such-tool"),
            Some(&joined(&[&first, &second])),
        );
        assert_eq!(resolved, first.join("no-such-tool"));
        assert!(resolved.is_absolute());
        assert!(!resolved.exists());
    }

    /// The process PATH gets the same rule: callers that `env_clear()` and
    /// re-add the process PATH would otherwise fork on a missing binary.
    #[test]
    fn missing_bare_name_on_the_process_path_is_pinned_under_its_first_dir() {
        let name = "magician-no-such-tool-for-resolve-test";
        let resolved = resolve_program(OsStr::new(name), None);
        let first_dir = std::env::var_os("PATH")
            .and_then(|path| std::env::split_paths(&path).find(|dir| !dir.as_os_str().is_empty()))
            .expect("the test process has a PATH with a non-empty dir");
        assert_eq!(resolved, first_dir.join(name));
        assert!(!resolved.exists());
    }

    #[test]
    fn missing_bare_name_stays_bare_when_the_path_has_no_dir_to_pin_under() {
        for search in ["", ":", "::"] {
            let resolved = resolve_program(OsStr::new("no-such-tool"), Some(OsStr::new(search)));
            assert_eq!(resolved, PathBuf::from("no-such-tool"), "search {search:?}");
        }
    }

    #[test]
    fn none_path_env_uses_the_process_path() {
        let resolved = resolve_program(OsStr::new("sh"), None);
        assert!(
            resolved.is_absolute(),
            "sh must resolve on the process PATH: {resolved:?}"
        );
        assert!(resolved.is_file());
        assert_eq!(resolved.file_name(), Some(OsStr::new("sh")));
    }

    #[test]
    fn empty_path_env_stays_bare() {
        let resolved = resolve_program(OsStr::new("sh"), Some(OsStr::new("")));
        assert_eq!(resolved, PathBuf::from("sh"));
    }

    #[test]
    fn empty_path_segments_are_ignored() {
        let temp = tempfile::tempdir().expect("tempdir");
        let wanted = write_file(temp.path(), "tool", 0o755);
        let search = format!(":{}:", temp.path().display());
        let resolved = resolve_program_str("tool", Some(&search));
        assert_eq!(resolved, wanted);
    }

    #[test]
    fn empty_program_passes_through_untouched() {
        let resolved = resolve_program(OsStr::new(""), None);
        assert_eq!(resolved, PathBuf::new());
    }

    #[test]
    fn str_convenience_matches_the_os_str_form() {
        let temp = tempfile::tempdir().expect("tempdir");
        let wanted = write_file(temp.path(), "tool", 0o755);
        let search = temp.path().display().to_string();
        assert_eq!(resolve_program_str("tool", Some(&search)), wanted);
        assert_eq!(
            resolve_program_str("tool", Some(&search)),
            resolve_program(OsStr::new("tool"), Some(OsStr::new(&search)))
        );
    }

    #[test]
    fn git_override_wins() {
        let (path, source) = pick_git(
            Some(PathBuf::from("/opt/git")),
            PathBuf::from("/usr/bin/git"),
            &[],
            None,
        );
        assert_eq!(
            (path, source),
            (PathBuf::from("/opt/git"), GitSource::Override)
        );
    }

    #[test]
    fn a_working_system_git_comes_before_the_bundled_one() {
        let temp = tempfile::tempdir().expect("tempdir");
        let system = write_file(temp.path(), "git", 0o755);
        let hub = temp.path().join("skillshub");
        fs::create_dir_all(hub.join("dugite/bin")).expect("mkdir");
        write_file(&hub.join("dugite/bin"), "git", 0o755);
        let (path, source) = pick_git(None, system.clone(), &[], Some(&hub));
        assert_eq!((path, source), (system, GitSource::System));
    }

    #[test]
    fn without_a_system_git_the_bundled_one_is_used() {
        let temp = tempfile::tempdir().expect("tempdir");
        let hub = temp.path().join("skillshub");
        fs::create_dir_all(hub.join("node_modules/dugite/git/bin")).expect("mkdir");
        let bundled = write_file(&hub.join("node_modules/dugite/git/bin"), "git", 0o755);
        let missing = temp.path().join("bin/git");
        let (path, source) = pick_git(None, missing, &[], Some(&hub));
        assert_eq!(source, GitSource::Bundled);
        assert_eq!(path, fs::canonicalize(bundled).expect("canonical"));
    }

    #[test]
    fn no_git_anywhere_is_missing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let missing = temp.path().join("bin/git");
        let (path, source) = pick_git(None, missing.clone(), &[], Some(temp.path()));
        assert_eq!((path, source), (missing, GitSource::Missing));
    }
}
