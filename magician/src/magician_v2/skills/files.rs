//! Files a procedure playbook links to.
//!
//! A SKILL.md body can link to files beside it (`references/commands.md`).
//! Only the body is injected into the prompt, so `activate_skill` takes an
//! optional `file` and returns that file's text through [`read_skill_file`].
//!
//! Readable files are an allowlist, not a filter: exactly the relative files
//! the body links to, and the markdown files those link to in turn
//! ([`linked_files`]). A skill folder also holds runtime-owned state (the
//! `config` link with the skill's `.env`, `auth/` sessions kept across
//! installs), so a path the playbook does not link to is never read, and a
//! link through a hidden, `config` or `auth` component is dropped even when
//! the author wrote it. A linked file must resolve into the skill's folder or
//! the folder its `SKILL.md` links to (the skillshub source), stay a regular
//! file and fit [`MAX_SKILL_FILE_BYTES`].

use std::collections::{BTreeSet, VecDeque};
use std::path::{Component, Path, PathBuf};

/// Largest linked file `read_skill_file` returns.
pub const MAX_SKILL_FILE_BYTES: u64 = 256 * 1024;

/// Most linked files [`linked_files`] collects for one skill.
const MAX_LINKED_FILES: usize = 32;

/// Read `requested` from the skill folder `skill_dir`, provided `body` (the
/// skill's SKILL.md body) links to it. The error is a short reason for the
/// tool result.
pub fn read_skill_file(skill_dir: &Path, body: &str, requested: &str) -> Result<String, String> {
    let linked = linked_files(skill_dir, body);
    let Some(relative) = normalize(Path::new(""), requested.trim()) else {
        return Err(not_linked(requested, &linked));
    };
    if !linked.contains(&relative) {
        return Err(not_linked(requested, &linked));
    }
    let roots = skill_roots(skill_dir);
    let resolved = resolve(&roots, &relative)
        .ok_or_else(|| format!("the playbook's link `{relative}` no longer opens"))?;
    std::fs::read_to_string(resolved).map_err(|_| format!("`{relative}` is not a text file"))
}

/// The files the body links to that [`read_skill_file`] will open, as paths
/// relative to the skill folder, in link order. Markdown files are followed
/// for their own links, resolved relative to where each file sits.
pub fn linked_files(skill_dir: &Path, body: &str) -> Vec<String> {
    let roots = skill_roots(skill_dir);
    let mut found: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<(PathBuf, String)> = VecDeque::new();
    queue.push_back((PathBuf::new(), body.to_string()));
    while let Some((from, text)) = queue.pop_front() {
        for target in link_targets(&text) {
            if found.len() >= MAX_LINKED_FILES {
                return found;
            }
            let Some(relative) = normalize(&from, target) else {
                continue;
            };
            if relative == "SKILL.md" || !seen.insert(relative.clone()) {
                continue;
            }
            let Some(resolved) = resolve(&roots, &relative) else {
                continue;
            };
            if relative.ends_with(".md") {
                if let Ok(text) = std::fs::read_to_string(&resolved) {
                    let dir = Path::new(&relative)
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_default();
                    queue.push_back((dir, text));
                }
            }
            found.push(relative);
        }
    }
    found
}

fn not_linked(requested: &str, linked: &[String]) -> String {
    if linked.is_empty() {
        format!("`{requested}` is not a file this playbook links to; it links to none")
    } else {
        format!(
            "`{requested}` is not a file this playbook links to; it links to: {}",
            linked.join(", ")
        )
    }
}

/// Relative markdown link targets (`[x](references/a.md#part)` →
/// `references/a.md`). URLs, absolute paths and in-page anchors are skipped.
fn link_targets(text: &str) -> impl Iterator<Item = &str> {
    text.match_indices("](").filter_map(|(at, _)| {
        let rest = &text[at + 2..];
        let target = rest[..rest.find(')')?].trim();
        let target = target.split('#').next().unwrap_or_default().trim();
        let skip = target.is_empty()
            || target.starts_with('/')
            || target.contains(':')
            || target.contains(char::is_whitespace);
        (!skip).then_some(target)
    })
}

/// `target` joined onto the folder `from` (both relative to the skill root),
/// lexically normalized. `None` when it climbs out of the skill folder or
/// passes through a hidden, `config` or `auth` component.
fn normalize(from: &Path, target: &str) -> Option<String> {
    let mut parts: Vec<String> = from
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    for component in Path::new(target).components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::CurDir => {},
            Component::ParentDir => {
                parts.pop()?;
            },
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if parts.is_empty() || parts.iter().any(|part| is_private_component(part)) {
        return None;
    }
    Some(parts.join("/"))
}

/// Runtime-owned or hidden names a linked file may not pass through. Compared
/// without case: macOS file systems treat `Config` and `config` as one.
fn is_private_component(part: &str) -> bool {
    part.starts_with('.') || part.eq_ignore_ascii_case("config") || part.eq_ignore_ascii_case("auth")
}

/// The folders a linked file may resolve into: the skill folder itself and
/// the folder its `SKILL.md` links to. In a container the scope's symlinks
/// dangle, so the real skillshub folder stands in (identity natively).
fn skill_roots(skill_dir: &Path) -> Vec<PathBuf> {
    let base = super::path_rewrite::rewrite_skill_dir(skill_dir);
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(root) = base.canonicalize() {
        roots.push(root);
    }
    if let Some(parent) = base
        .join("SKILL.md")
        .canonicalize()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
    {
        if !roots.contains(&parent) {
            roots.push(parent);
        }
    }
    roots
}

/// Resolve `relative` under the first root, then check where it really lands:
/// inside a root, with no private component past it, a regular file within
/// the size limit.
fn resolve(roots: &[PathBuf], relative: &str) -> Option<PathBuf> {
    let base = roots.first()?;
    let resolved = base.join(relative).canonicalize().ok()?;
    let inside = roots.iter().any(|root| {
        resolved.strip_prefix(root).is_ok_and(|rest| {
            !rest
                .components()
                .any(|part| is_private_component(&part.as_os_str().to_string_lossy()))
        })
    });
    let metadata = std::fs::metadata(&resolved).ok()?;
    (inside && metadata.is_file() && metadata.len() <= MAX_SKILL_FILE_BYTES).then_some(resolved)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    const BODY: &str = "Pick a workflow from [the list](references/commands.md#build). \
                        Site: [x](https://x.dev), [top](#top), cite as `[name](url)`.";

    fn skill(root: &Path) -> PathBuf {
        let dir = root.join("demo");
        fs::create_dir_all(dir.join("references")).unwrap();
        fs::write(dir.join("SKILL.md"), format!("---\nname: demo\n---\n{BODY}")).unwrap();
        fs::write(
            dir.join("references/commands.md"),
            "Read [design](design.md) and [back](../SKILL.md).",
        )
        .unwrap();
        fs::write(dir.join("references/design.md"), "the design rules").unwrap();
        fs::write(dir.join("references/unlinked.md"), "not linked").unwrap();
        dir
    }

    #[test]
    fn reads_the_files_the_playbook_links_to_and_their_links() {
        let root = TempDir::new().unwrap();
        let dir = skill(root.path());
        assert_eq!(
            linked_files(&dir, BODY),
            vec!["references/commands.md", "references/design.md"]
        );
        assert_eq!(
            read_skill_file(&dir, BODY, "./references/design.md").unwrap(),
            "the design rules"
        );
    }

    #[test]
    fn refuses_files_the_playbook_does_not_link_to() {
        let root = TempDir::new().unwrap();
        let dir = skill(root.path());
        fs::write(root.path().join("secret.txt"), "no").unwrap();
        for path in [
            "references/unlinked.md",
            "../secret.txt",
            "/etc/hosts",
            ".env",
            "SKILL.md",
            "url",
            "",
        ] {
            let error = read_skill_file(&dir, BODY, path).unwrap_err();
            assert!(error.contains("references/commands.md"), "{path}: {error}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_link_into_runtime_state_is_never_read() {
        let root = TempDir::new().unwrap();
        let dir = skill(root.path());
        let state = root.path().join(".skill-state/demo/config");
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("env"), "KEY=1").unwrap();
        std::os::unix::fs::symlink(&state, dir.join("config")).unwrap();
        fs::create_dir_all(dir.join("auth")).unwrap();
        fs::write(dir.join("auth/session.json"), "{}").unwrap();
        fs::write(dir.join("auth.md"), "x").unwrap();
        std::os::unix::fs::symlink(dir.join("auth/session.json"), dir.join("references/s.md"))
            .unwrap();
        let body = "[a](config/env) [b](Config/env) [c](auth/session.json) \
                    [d](AUTH/session.json) [e](references/s.md) [f](auth.md)";
        assert_eq!(linked_files(&dir, body), vec!["auth.md"]);
        assert!(read_skill_file(&dir, body, "references/s.md").is_err());
        assert!(read_skill_file(&dir, body, "Config/env").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn follows_links_into_the_folder_skill_md_points_at() {
        // An installed skill is a real folder whose files link back to the
        // skillshub source.
        let root = TempDir::new().unwrap();
        let source = skill(root.path());
        let installed = root.path().join("scope/skills/demo");
        fs::create_dir_all(installed.join("references")).unwrap();
        std::os::unix::fs::symlink(source.join("SKILL.md"), installed.join("SKILL.md")).unwrap();
        for file in ["commands.md", "design.md"] {
            std::os::unix::fs::symlink(
                source.join("references").join(file),
                installed.join("references").join(file),
            )
            .unwrap();
        }
        assert_eq!(
            read_skill_file(&installed, BODY, "references/design.md").unwrap(),
            "the design rules"
        );
    }

    #[test]
    fn a_link_to_a_folder_or_missing_file_is_not_listed() {
        let root = TempDir::new().unwrap();
        let dir = skill(root.path());
        assert!(linked_files(&dir, "[d](references) [m](references/missing.md)").is_empty());
    }
}
