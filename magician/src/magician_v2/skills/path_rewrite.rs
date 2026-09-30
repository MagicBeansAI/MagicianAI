//! Container-portable skill-file resolution.
//!
//! Skills are materialized as host-absolute symlinks into the runtime root
//! (`<root>/scopes/.../skills/<n>/<f>` -> `/abs/host/path/skillshub/<n>/<f>`).
//! Those follow fine on a native host, but DANGLE inside a container where
//! skillshub lives at a different absolute path (e.g. `/app/skillshub`).
//!
//! When `MAGICIAN_SKILLSHUB_ROOT` is set we read the symlink target and rewrite
//! everything up to and including the `skillshub` path segment to that root — so
//! the file resolves WITHOUT mutating the (possibly shared / mounted) on-disk
//! symlinks. Native (env unset) leaves the path untouched and the OS follows the
//! link directly. A non-symlink (a copied skill) is also left untouched.

use std::path::{Path, PathBuf};

/// Env var pointing at the skillshub root in THIS environment. The container
/// sets `MAGICIAN_SKILLSHUB_ROOT=/app/skillshub`; a native host leaves it unset.
pub const SKILLSHUB_ROOT_ENV: &str = "MAGICIAN_SKILLSHUB_ROOT";

/// Resolve a skill file path for the current environment. See module docs.
pub fn resolve_skill_path(p: &Path) -> PathBuf {
    let Some(root) = std::env::var_os(SKILLSHUB_ROOT_ENV).filter(|v| !v.is_empty()) else {
        return p.to_path_buf();
    };
    match std::fs::read_link(p) {
        Ok(target) => {
            rewrite_under_skillshub(&target, Path::new(&root)).unwrap_or_else(|| p.to_path_buf())
        },
        // Not a symlink (e.g. a real copied skill file) — use as-is.
        Err(_) => p.to_path_buf(),
    }
}

/// Resolve a skill *directory* for the current environment.
///
/// The materialized skill dir is a REAL directory (only the files inside it are
/// symlinks), so it can't be `read_link`'d directly. Instead we read a known
/// symlinked `SKILL.md` to discover the skillshub target and return that file's
/// parent. Used by the execution path
/// to rewrite `{skill_runtime_root}` so `{skill_runtime_root}/scripts/run.sh`
/// (and friends) resolve in a container. Identity on a native host (env unset)
/// or when no symlinked child is found.
pub fn rewrite_skill_dir(dir: &Path) -> PathBuf {
    if std::env::var_os(SKILLSHUB_ROOT_ENV)
        .filter(|value| !value.is_empty())
        .is_none()
    {
        return dir.to_path_buf();
    }
    let original = dir.join("SKILL.md");
    let resolved = resolve_skill_path(&original);
    if resolved != original {
        if let Some(parent) = resolved.parent() {
            return parent.to_path_buf();
        }
    }
    dir.to_path_buf()
}

/// Replace the prefix of `target` up to and including the last `skillshub`
/// path component with `new_root`. `None` if there is no `skillshub` segment.
fn rewrite_under_skillshub(target: &Path, new_root: &Path) -> Option<PathBuf> {
    let comps: Vec<_> = target.components().collect();
    let idx = comps
        .iter()
        .rposition(|c| c.as_os_str().to_str() == Some("skillshub"))?;
    let mut out = new_root.to_path_buf();
    for c in &comps[idx + 1..] {
        out.push(c.as_os_str());
    }
    Some(out)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn rewrites_skillshub_prefix() {
        let target = Path::new("/Users/me/dev/magician/skillshub/gmail/SKILL.md");
        let out = rewrite_under_skillshub(target, Path::new("/app/skillshub")).unwrap();
        assert_eq!(out, Path::new("/app/skillshub/gmail/SKILL.md"));
    }

    #[test]
    fn nested_skillshub_uses_the_last_segment() {
        let target = Path::new("/a/skillshub/b/skillshub/x/y.yaml");
        let out = rewrite_under_skillshub(target, Path::new("/app/skillshub")).unwrap();
        assert_eq!(out, Path::new("/app/skillshub/x/y.yaml"));
    }

    #[test]
    fn no_skillshub_segment_is_none() {
        assert!(
            rewrite_under_skillshub(Path::new("/a/b/c/SKILL.md"), Path::new("/app/skillshub"))
                .is_none()
        );
    }

    #[test]
    fn rewrite_skill_dir_is_identity_for_a_nonexistent_dir() {
        // No symlinked SKILL.md child (the dir doesn't exist),
        // so the dir is returned unchanged regardless of env — covers the
        // native identity case and a copied (non-symlinked) skill.
        let dir = Path::new("/some/scope/skills/gmail");
        assert_eq!(rewrite_skill_dir(dir), dir.to_path_buf());
    }

    #[test]
    fn non_symlink_path_is_returned_unchanged() {
        // A real (non-symlink) path is never rewritten: `read_link` fails so
        // `resolve_skill_path` returns the input as-is — whether or not
        // `MAGICIAN_SKILLSHUB_ROOT` is set. This covers both the native
        // identity case and a copied (non-symlinked) skill in a container.
        let p = Path::new("/some/scope/skills/gmail/SKILL.md");
        assert_eq!(resolve_skill_path(p), p.to_path_buf());
    }
}
