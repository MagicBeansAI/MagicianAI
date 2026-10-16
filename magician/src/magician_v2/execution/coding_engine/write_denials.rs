//! Writes the coding sandbox refused, read back from an engine's own output.
//!
//! A `sandbox-exec` profile denies a write synchronously and the kernel does
//! not log it (measured 2026-10-09), so the only record is what the engine's
//! command printed: `Operation not permitted` / `Read-only file system` next
//! to a path. Those paths become the folders the owner is asked to allow for
//! the project (`vibedev::sandbox_approvals`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

/// Most paths reported for one turn; the rest are dropped.
const MAX_DENIED_PATHS: usize = 32;

fn path_then_error() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // `sh: /a/b: Operation not permitted`, `cp: /a/b: Read-only file system`
        Regex::new(
            r#"(/[^\s'"`:;,()\\]+)['"`]?\s*:\s*(?:Operation not permitted|Read-only file system)"#,
        )
        .expect("path-then-error regex")
    })
}

fn error_then_path() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // `EPERM: operation not permitted, open '/a/b'`,
        // `PermissionError: [Errno 1] Operation not permitted: '/a/b'`
        Regex::new(
            r#"(?i)(?:operation not permitted|read-only file system|\bEPERM\b|\bEROFS\b)(?:\s*\(os error \d+\))?\s*[,:]\s*(?:[a-z_]+\s+)?['"`]?(/[^\s'"`:;,()\\]+)"#,
        )
        .expect("error-then-path regex")
    })
}

fn cargo_failed_to() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // cargo names the path, then the cause a few lines down:
        // ``failed to create directory `/a/b` … Caused by: Read-only file
        // system (os error 30)``. The gap may hold real or escaped newlines.
        Regex::new(
            r#"(?s)failed to [a-z ]{1,40}`(/[^`\s]+)`.{0,300}?(?:Operation not permitted|Read-only file system)"#,
        )
        .expect("cargo failed-to regex")
    })
}

/// Absolute paths a write was refused on, as reported in `text` (an engine's
/// raw event, JSON-escaped or not).
pub fn denied_paths_in(text: &str) -> Vec<PathBuf> {
    let mut found = BTreeSet::new();
    for regex in [path_then_error(), error_then_path(), cargo_failed_to()] {
        for capture in regex.captures_iter(text) {
            if let Some(path) = capture.get(1) {
                let path = path.as_str().trim_end_matches(['.', '\'', '"']);
                if path.len() > 1 {
                    found.insert(PathBuf::from(path));
                }
            }
            if found.len() >= MAX_DENIED_PATHS {
                break;
            }
        }
    }
    found.into_iter().collect()
}

/// Key under a coding event's `raw` holding the paths its tool output said
/// were refused. Engines whose `raw` is stripped to a stub (Codex, Claude,
/// Grok, Antigravity keep no output, so no session ids leak) attach only these
/// paths, so the refusal survives without the output itself.
pub const REFUSED_WRITES_KEY: &str = "refused_writes";

/// Most bytes of one native payload scanned for refusals.
const MAX_SCAN_BYTES: usize = 64 * 1024;

/// Whether `text` mentions a refused write at all (the cheap pre-check).
pub fn mentions_denial(text: &str) -> bool {
    ["ot permitted", "ead-only file system", "EPERM", "EROFS"]
        .iter()
        .any(|needle| text.contains(needle))
}

/// Refused paths in `text`, scanning at most its first 64 KB.
pub fn denied_paths_in_prefix(text: &str) -> Vec<PathBuf> {
    if !mentions_denial(text) {
        return Vec::new();
    }
    let mut end = text.len().min(MAX_SCAN_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    denied_paths_in(&text[..end])
}

/// Attach the paths `native` (an engine's own tool payload) reports refused
/// to `raw[REFUSED_WRITES_KEY]`. Nothing is added when there are none.
pub fn attach_refused_writes(raw: &mut Value, native: &impl serde::Serialize) {
    let Ok(text) = serde_json::to_string(native) else {
        return;
    };
    let paths = denied_paths_in_prefix(&text);
    if paths.is_empty() {
        return;
    }
    if let Value::Object(map) = raw {
        map.insert(
            REFUSED_WRITES_KEY.to_string(),
            Value::Array(
                paths
                    .into_iter()
                    .map(|path| Value::String(path.display().to_string()))
                    .collect(),
            ),
        );
    }
}

/// Folders no project may be allowed to write: the filesystem and home
/// roots, credentials, login items, autostart and `PATH` folders, and system
/// locations on macOS and Linux. A folder that contains a protected one
/// (`~/.config` holds `~/.config/systemd`) is refused too.
pub fn never_allow(path: &Path, home: Option<&Path>) -> bool {
    const SYSTEM: &[&str] = &[
        "/System",
        "/usr",
        "/bin",
        "/sbin",
        "/Library",
        "/Applications",
        "/etc",
        "/private/etc",
        "/dev",
        "/var/db",
        "/private/var/db",
        "/proc",
        "/sys",
        "/boot",
        "/lib",
        "/lib32",
        "/lib64",
        "/run",
        "/root",
        "/snap",
        "/var/lib",
        "/nix",
    ];
    if path == Path::new("/") || SYSTEM.iter().any(|root| path.starts_with(root)) {
        return true;
    }
    let Some(home) = home else {
        return false;
    };
    if home.starts_with(path) {
        // The home folder itself or any of its ancestors.
        return true;
    }
    const HOME_PROTECTED: &[&str] = &[
        ".ssh",
        ".gnupg",
        ".aws",
        ".kube",
        ".docker",
        ".netrc",
        ".git-credentials",
        ".password-store",
        ".config/gh",
        ".config/systemd",
        ".config/autostart",
        ".local/bin",
        ".local/share/keyrings",
        ".cargo/bin",
        "Library/Keychains",
        "Library/LaunchAgents",
        "Library/Preferences",
        "Library",
    ];
    HOME_PROTECTED.iter().any(|rel| {
        let protected = home.join(rel);
        // `Library` itself is protected but its caches are not denied anyway.
        if *rel == "Library" {
            path == protected
        } else {
            path.starts_with(&protected) || protected.starts_with(path)
        }
    })
}

/// The folders to offer for approval: each denied path outside the turn's
/// write roots, collapsed to its nearest existing folder, minus anything on
/// the never-allow list or inside `protected` (the real repo, the Magician
/// source), deduplicated so a folder's children are not listed separately.
pub fn approvable_dirs(
    denied: &[PathBuf],
    write_roots: &[PathBuf],
    protected: &[PathBuf],
    home: Option<&Path>,
) -> Vec<PathBuf> {
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let roots: Vec<PathBuf> = write_roots.iter().map(|root| canonical(root)).collect();
    let protected: Vec<PathBuf> = protected.iter().map(|path| canonical(path)).collect();
    let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
    for path in denied {
        if !path.is_absolute() {
            continue;
        }
        let mut dir = path.clone();
        let mut missing_child = None;
        while !dir.is_dir() {
            if !dir.exists() {
                missing_child = dir.file_name().map(|name| name.to_os_string());
            }
            match dir.parent() {
                Some(parent) => dir = parent.to_path_buf(),
                None => break,
            }
        }
        let mut dir = canonical(&dir);
        // A tool creating `~/.config/tool` lands on `~/.config`, which holds
        // protected folders: offer the folder it was creating instead. Never
        // below the home folder itself (`~/.zshenv`) or a protected folder.
        if let (Some(home), Some(child)) = (home, missing_child) {
            let candidate = dir.join(child);
            if never_allow(&dir, Some(home))
                && dir.starts_with(home)
                && dir.as_path() != home
                && !never_allow(&candidate, Some(home))
            {
                dir = candidate;
            }
        }
        if roots.iter().any(|root| dir.starts_with(root))
            || protected
                .iter()
                .any(|path| dir.starts_with(path) || path.starts_with(&dir))
            || never_allow(&dir, home)
        {
            continue;
        }
        dirs.insert(dir);
    }
    let all: Vec<PathBuf> = dirs.iter().cloned().collect();
    dirs.into_iter()
        .filter(|dir| {
            !all.iter()
                .any(|other| other != dir && dir.starts_with(other))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_common_denial_shapes() {
        let text = concat!(
            "sh: /opt/tool/cache/x.lock: Operation not permitted\n",
            "Error: EPERM: operation not permitted, open '/Users/me/.config/tool/state.json'\n",
            "PermissionError: [Errno 1] Operation not permitted: '/srv/data/out.csv'\n",
            "cp: /Volumes/ext/build: Read-only file system\n",
            "Operation not permitted; see the log at /no/denial/here\n",
            "this is fine: /no/denial/either\n",
        );
        let paths = denied_paths_in(text);
        for expected in [
            "/opt/tool/cache/x.lock",
            "/Users/me/.config/tool/state.json",
            "/srv/data/out.csv",
            "/Volumes/ext/build",
        ] {
            assert!(
                paths.contains(&PathBuf::from(expected)),
                "{expected} in {paths:?}"
            );
        }
        assert!(!paths.contains(&PathBuf::from("/no/denial/here")));
    }

    #[test]
    fn zsh_refusals_and_attached_paths() {
        // zsh (Codex, Claude): lowercase, the path last.
        let text = "zsh:1: operation not permitted: /Users/me/.config/probe/marker.txt\n";
        assert_eq!(
            denied_paths_in_prefix(text),
            vec![PathBuf::from("/Users/me/.config/probe/marker.txt")]
        );
        let mut raw = serde_json::json!({ "type": "tool_execution_end" });
        attach_refused_writes(
            &mut raw,
            &serde_json::json!({ "aggregatedOutput": text, "exitCode": 1 }),
        );
        assert_eq!(
            raw[REFUSED_WRITES_KEY],
            serde_json::json!(["/Users/me/.config/probe/marker.txt"])
        );
        let mut quiet = serde_json::json!({ "type": "tool_execution_end" });
        attach_refused_writes(&mut quiet, &serde_json::json!({ "aggregatedOutput": "ok" }));
        assert!(quiet.get(REFUSED_WRITES_KEY).is_none());
    }

    #[test]
    fn reads_cargo_and_linux_read_only_refusals() {
        // cargo, the cause lines below the path; raw and JSON-escaped.
        let cargo = "error: failed to create directory `/home/me/.cache/tool/x`\n\nCaused by:\n  Read-only file system (os error 30)";
        let escaped = serde_json::to_string(&serde_json::json!({ "output": cargo })).unwrap();
        for text in [cargo, escaped.as_str()] {
            assert_eq!(
                denied_paths_in_prefix(text),
                vec![PathBuf::from("/home/me/.cache/tool/x")],
                "{text}"
            );
        }
        // bwrap's read-only mounts: coreutils, node.
        let text = concat!(
            "touch: cannot touch '/home/me/.config/tool/a': Read-only file system\n",
            "Error: EROFS: read-only file system, open '/home/me/.local/state/b.json'\n",
        );
        assert_eq!(
            denied_paths_in(text),
            vec![
                PathBuf::from("/home/me/.config/tool/a"),
                PathBuf::from("/home/me/.local/state/b.json"),
            ]
        );
        // A cargo failure for another reason names no refusal.
        assert!(denied_paths_in(
            "error: failed to read `/home/me/p/Cargo.toml`\n\nCaused by:\n  No such file or directory (os error 2)"
        )
        .is_empty());
    }

    #[test]
    fn approvable_dirs_skip_roots_protected_and_never_allow() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        let clone = root.join("clone");
        let tool = home.join(".config/tool");
        let real = root.join("real");
        for dir in [&clone, &tool, &real, &home.join(".ssh")] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let denied = vec![
            tool.join("state.json"),       // file → its folder
            tool.join("sub/new.json"),     // missing → nearest existing folder
            clone.join("x"),               // inside a write root → not asked
            real.join("y"),                // the real repo → never asked
            home.join(".ssh/known_hosts"), // credentials → never asked
            home.join("notes.txt"),        // home itself → never asked
        ];
        let dirs = approvable_dirs(&denied, &[clone], &[real], Some(&home));
        assert_eq!(dirs, vec![tool]);
    }

    #[test]
    fn a_new_folder_under_a_protected_parent_is_offered_itself() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(home.join(".config/systemd")).unwrap();
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        let denied = vec![
            home.join(".config/newtool/state.json"), // offered: .config/newtool
            home.join(".zshenv"),                    // home itself: never
            home.join(".ssh/new/key"),               // protected: never
            home.join(".config/systemd/user/x"),     // protected: never
        ];
        let dirs = approvable_dirs(&denied, &[], &[], Some(&home));
        assert_eq!(dirs, vec![home.join(".config/newtool")]);
    }

    #[test]
    fn never_allow_covers_system_and_home_roots() {
        let home = Path::new("/Users/me");
        for path in [
            "/",
            "/Users",
            "/Users/me",
            "/usr/local/bin",
            "/Users/me/.ssh/x",
            "/Users/me/Library",
        ] {
            assert!(never_allow(Path::new(path), Some(home)), "{path}");
        }
        for path in [
            "/Users/me/.config/tool",
            "/opt/homebrew/var",
            "/Users/me/Library/Caches/x",
        ] {
            assert!(!never_allow(Path::new(path), Some(home)), "{path}");
        }
    }

    #[test]
    fn never_allow_covers_linux_system_and_autostart_folders() {
        let home = Path::new("/home/me");
        for path in [
            "/proc/self",
            "/sys/fs",
            "/run/user/1000",
            "/root",
            "/var/lib/docker",
            "/home/me/.config/systemd/user",
            "/home/me/.config",
            "/home/me/.local/bin",
            "/home/me/.local",
            "/home/me/.cargo",
            "/home/me/.local/share/keyrings",
        ] {
            assert!(never_allow(Path::new(path), Some(home)), "{path}");
        }
        for path in [
            "/home/me/.config/tool",
            "/home/me/.local/share/tool",
            "/home/me/.cargo/registry",
            "/srv/data",
        ] {
            assert!(!never_allow(Path::new(path), Some(home)), "{path}");
        }
    }
}
