//! Self-healing scope skills layer.
//!
//! A scope's `skills/` tree is a file-level mirror of `skillshub/<skill>/`
//! (`skillshub/scripts/install_skill_layer.py`): absolute symlinks into the
//! checkout or installed package, so SKILL.md and script edits are live. That
//! mirror used to be written only by operator setup, and nothing noticed when
//! it went stale:
//!
//! - a workspace created after setup had no skills layer at all, so its pinned
//!   CLIs (agent-browser above all, which the runtime refuses to take from
//!   `PATH`) were "not found";
//! - a data root copied to another machine, or a checkout that moved, left
//!   every link pointing at a path that no longer exists;
//! - an ignored, generated executable in the checkout (`browser/bin/
//!   agent-browser`) that was cleaned took every scope's copy with it. That is
//!   the 2026-09-30 incident.
//!
//! [`reconcile_scope_skills_layer`] repairs all three, offline and
//! idempotently: it installs skills a scope is missing, re-points dangling
//! links at the running skillshub, adds links for files the skill gained since
//! it was installed, and copies the pinned native executables that skillshub's
//! own setup builds into a content-addressed store under the data root,
//! linking them relatively, so a cleaned checkout or a moved data root no
//! longer breaks them. It never runs npm, a compiler or the network: when a
//! source artifact is genuinely missing it is reported as
//! [`UnresolvedLink`] with the setup command that rebuilds it.
//!
//! The store rule matches `skillshub/scripts/pinned_bin_store.py`; keep the
//! two in step. A skill a scope owns outright (its `SKILL.md` is a regular
//! file) is never touched, and neither is one the operator removed from the
//! scope ([`mark_skill_removed`]): installing whatever a scope lacks used to
//! undo every uninstall on the next boot.
//!
//! A skill whose manifest varies per scope ships `render.yaml` beside a
//! placeholder template `SKILL.md`; setup (`skillshub/scripts/skill_render.py`)
//! writes the scope's real `SKILL.md` plus a `.skill-render.json` provenance
//! marker. This reconciler never links that template or `render.yaml` into a
//! scope, and treats a marked manifest as setup-managed: the skill's other
//! files are still installed and topped up, the manifest itself is left alone.
// Symlink mirrors are a Unix layout; elsewhere reconcile is a no-op.
#![cfg_attr(not(unix), allow(dead_code, unused_imports, unused_mut))]

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

/// `<data root>/system/<this>` holds the stored executables.
pub const SKILL_BIN_STORE_DIRNAME: &str = "skill-bin";
/// `<skills>/.skill-state/<skill>/<this>`: the operator removed the skill
/// from this scope, so reconcile must not install it again. Lives in the
/// state dir because that survives both uninstall modes and every reinstall.
pub const REMOVED_MARKER: &str = "removed";
/// Real directories a replaced install carries into the new tree: `auth/`
/// and the per-skill `.skill-state/` the uninstall contract preserves.
const CARRIED_OVER: &[&str] = &["auth", ".skill-state"];

/// Present in a skill's source when its manifest is rendered per scope.
const RENDER_SPEC: &str = "render.yaml";
/// Written by the renderer beside the scope's rendered `SKILL.md`.
const RENDER_MARKER: &str = ".skill-render.json";

/// Top-level source files never linked for a render-declared skill.
fn is_render_template_file(source_root: &Path, relative: &Path, name: &str) -> bool {
    relative.as_os_str().is_empty()
        && matches!(name, "SKILL.md" | RENDER_SPEC)
        && source_root.join(RENDER_SPEC).is_file()
}

/// Directories `install-scope` never mirrors.
const INSTALL_SKIP_DIRS: &[&str] = &["_vendor"];
/// Directories the top-up pass leaves to setup (`link_runtime.py` parity):
/// package-manager trees are large and are restored by their own setup step.
const TOP_UP_SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".venv",
    ".node",
    ".runtime",
    ".pp-build",
    ".cache",
    "_vendor",
    "__pycache__",
    ".git",
];
/// Trees whose layout belongs to npm / Python. A native file inside them is
/// left a plain link (dugite's git finds its helpers next to itself).
const UNOWNED_TREES: &[&str] = &["node_modules", ".venv"];
const NATIVE_MAGIC: &[[u8; 4]] = &[
    [0xcf, 0xfa, 0xed, 0xfe],
    [0xce, 0xfa, 0xed, 0xfe],
    [0xfe, 0xed, 0xfa, 0xcf],
    [0xfe, 0xed, 0xfa, 0xce],
    [0xca, 0xfe, 0xba, 0xbe],
    [0xca, 0xfe, 0xba, 0xbf],
    [0x7f, b'E', b'L', b'F'],
];

/// A scoped link that could not be repaired from the running skillshub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedLink {
    /// The link inside the scope, e.g. `<scope>/skills/browser/bin/agent-browser`.
    pub scoped: PathBuf,
    /// Where it should point and nothing exists: usually an ignored,
    /// generated file the skill's setup target builds.
    pub expected: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeLayerReport {
    /// Skills that were absent from the scope and were installed.
    pub installed: Vec<String>,
    /// Dangling links re-pointed at the running skillshub.
    pub relinked: usize,
    /// Links added for source files the scope did not have yet.
    pub added: usize,
    /// `bin/` links (re)pointed at the pinned-executable store.
    pub stored: usize,
    pub unresolved: Vec<UnresolvedLink>,
    pub errors: Vec<String>,
}

impl ScopeLayerReport {
    pub fn changed(&self) -> bool {
        !self.installed.is_empty() || self.relinked > 0 || self.added > 0 || self.stored > 0
    }
}

/// `<root>/system/skill-bin` for a `<root>/scopes/<p>/<w>/skills` tree, else
/// `None` (a container's private layer, a fixture).
pub fn bin_store_for_scope_skills_root(scope_skills_root: &Path) -> Option<PathBuf> {
    let skills = scope_skills_root;
    let workspace = skills.parent()?;
    let principal = workspace.parent()?;
    let scopes = principal.parent()?;
    let root = scopes.parent()?;
    (skills.file_name()? == "skills" && scopes.file_name()? == "scopes")
        .then(|| root.join("system").join(SKILL_BIN_STORE_DIRNAME))
}

/// Reconcile one scope's skills layer against `skillshub_root`. See the
/// module docs. `bin_store` of `None` keeps executables as plain links.
pub fn reconcile_scope_skills_layer(
    skillshub_root: &Path,
    scope_skills_root: &Path,
    bin_store: Option<&Path>,
) -> ScopeLayerReport {
    let mut report = ScopeLayerReport::default();
    #[cfg(unix)]
    reconcile_unix(skillshub_root, scope_skills_root, bin_store, &mut report);
    #[cfg(not(unix))]
    let _ = (skillshub_root, scope_skills_root, bin_store);
    report
}

fn removed_marker(skills_root: &Path, name: &str) -> PathBuf {
    skills_root
        .join(".skill-state")
        .join(name)
        .join(REMOVED_MARKER)
}

/// Record that the operator removed `name` from this scope; reconcile skips
/// it until an install clears the mark.
pub fn mark_skill_removed(skills_root: &Path, name: &str) -> io::Result<()> {
    let marker = removed_marker(skills_root, name);
    if let Some(parent) = marker.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(marker, b"")
}

pub fn skill_marked_removed(skills_root: &Path, name: &str) -> bool {
    fs::symlink_metadata(removed_marker(skills_root, name)).is_ok()
}

/// An explicit install takes the skill back: reconcile tends it again.
pub fn clear_skill_removed(skills_root: &Path, name: &str) -> io::Result<()> {
    match fs::remove_file(removed_marker(skills_root, name)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Delete a skill's runtime-owned state (`<skills>/.skill-state/<name>`):
/// the credentials and sessions a purge promises to remove, which live
/// beside the skill folder rather than inside it.
pub fn purge_skill_state(skills_root: &Path, name: &str) -> io::Result<()> {
    match fs::remove_dir_all(skills_root.join(".skill-state").join(name)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Install (or reinstall) one skillshub skill into a scope in
/// `install_skill_layer.py`'s linked layout, replacing whatever is there: an
/// earlier linked install, a plain copy (a dropped-in folder, the old API
/// install) or an uninstall skeleton. Runtime-owned files in a real
/// `config/` move into the state dir first, real `auth/` and `.skill-state/`
/// folders are carried over, and the removal mark is cleared.
///
/// A copy is a scope-owned skill that reconcile never touches again, so
/// skillshub edits never reached it; a linked install is live and repaired.
#[cfg(unix)]
pub fn install_linked_skill(
    skillshub_root: &Path,
    skills_root: &Path,
    name: &str,
    bin_store: Option<&Path>,
) -> io::Result<()> {
    let hub = skillshub_root.canonicalize()?;
    let source = hub.join(name);
    if !source.join("SKILL.md").is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} has no SKILL.md", source.display()),
        ));
    }
    fs::create_dir_all(skills_root)?;
    let _lock = InstallLock::acquire(skills_root, name)?;
    let dest = skills_root.join(name);
    let existing = match fs::symlink_metadata(&dest) {
        Ok(meta) if meta.is_dir() => true,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "installed skill destination must be a real directory: {}",
                    dest.display()
                ),
            ))
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };
    let state_config = skills_root.join(".skill-state").join(name).join("config");
    fs::create_dir_all(&state_config)?;
    if existing {
        copy_runtime_config_into_state(&dest.join("config"), &state_config)?;
    }
    let (staging, _stored) = stage_linked_tree(&hub, &source, skills_root, name, bin_store)?;
    // Whole staged directories are exchanged or moved into place, not file
    // bytes written: the atomic-write helpers write one file, so they do not
    // apply to these renames.
    let swapped = if existing {
        exchange_paths(&staging, &dest)
    } else {
        fs::rename(&staging, &dest)
    };
    if let Err(error) = swapped {
        let _ = remove_any(&staging);
        return Err(error);
    }
    if existing {
        // After the exchange `staging` holds the replaced tree. A failed
        // carry-over leaves it in place rather than deleting what it holds.
        // Each carried entry is a directory moved whole within one parent.
        for kept in CARRIED_OVER {
            let from = staging.join(kept);
            let real_dir = fs::symlink_metadata(&from)
                .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink());
            if real_dir && fs::symlink_metadata(dest.join(kept)).is_err() {
                fs::rename(&from, dest.join(kept))?;
            }
        }
        remove_any(&staging)?;
    }
    clear_skill_removed(skills_root, name)
}

#[cfg(not(unix))]
pub fn install_linked_skill(
    _skillshub_root: &Path,
    _skills_root: &Path,
    _name: &str,
    _bin_store: Option<&Path>,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "linked skill installs need a Unix filesystem",
    ))
}

/// Re-point a scope's dangling bot bundle links
/// (`<scope>/bots/<bot>/dist/index.js`) at `<skillshub>/bots/<bot>/dist/index.js`.
///
/// `install_bot_bundles.py` writes them as absolute links into whichever
/// checkout ran it. Deleting that checkout (a merged worktree) left every bot
/// failing `MODULE_NOT_FOUND` on each restart until setup ran again — the
/// 2026-10-02 incident. A link that still resolves is left alone, even into
/// another checkout: that is a deliberate install. Offline: a bundle the
/// running skillshub has not built is reported, never built.
pub fn heal_scope_bot_bundle_links(
    skillshub_root: &Path,
    scope_bots_root: &Path,
) -> ScopeLayerReport {
    let mut report = ScopeLayerReport::default();
    #[cfg(unix)]
    if let Err(error) = heal_bot_bundles_unix(skillshub_root, scope_bots_root, &mut report) {
        report
            .errors
            .push(format!("{}: {error}", scope_bots_root.display()));
    }
    #[cfg(not(unix))]
    let _ = (skillshub_root, scope_bots_root);
    report
}

#[cfg(unix)]
fn heal_bot_bundles_unix(
    hub: &Path,
    bots_root: &Path,
    report: &mut ScopeLayerReport,
) -> io::Result<()> {
    let entries = match fs::read_dir(bots_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries.flatten() {
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let link = entry.path().join("dist").join("index.js");
        let dangling = fs::symlink_metadata(&link).is_ok_and(|meta| meta.file_type().is_symlink())
            && fs::metadata(&link).is_err();
        if !dangling {
            continue;
        }
        let expected = hub
            .join("bots")
            .join(entry.file_name())
            .join("dist")
            .join("index.js");
        if fs::metadata(&expected).is_ok_and(|meta| meta.is_file()) {
            match replace_symlink(&link, &expected) {
                Ok(()) => report.relinked += 1,
                Err(error) => report.errors.push(format!("{}: {error}", link.display())),
            }
        } else {
            report.unresolved.push(UnresolvedLink {
                scoped: link,
                expected,
            });
        }
    }
    Ok(())
}

#[cfg(unix)]
fn reconcile_unix(
    skillshub_root: &Path,
    scope_skills_root: &Path,
    bin_store: Option<&Path>,
    report: &mut ScopeLayerReport,
) {
    let Ok(hub) = skillshub_root.canonicalize() else {
        return;
    };
    let mut skills: Vec<(String, PathBuf)> = match fs::read_dir(&hub) {
        Ok(entries) => entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let path = entry.path();
                (!name.starts_with('.') && path.is_dir() && path.join("SKILL.md").is_file())
                    .then_some((name, path))
            })
            .collect(),
        Err(_) => return,
    };
    if skills.is_empty() {
        return;
    }
    skills.sort();
    if let Err(error) = fs::create_dir_all(scope_skills_root) {
        report
            .errors
            .push(format!("{}: {error}", scope_skills_root.display()));
        return;
    }
    for (name, source) in &skills {
        let dest = scope_skills_root.join(name);
        // Removed by the operator and not reinstalled since: an uninstall
        // skeleton must not be "repaired" back into a skill. A skill present
        // again despite a stale mark is still tended.
        if fs::symlink_metadata(dest.join("SKILL.md")).is_err()
            && skill_marked_removed(scope_skills_root, name)
        {
            continue;
        }
        let outcome = match fs::symlink_metadata(&dest) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                install_skill(&hub, source, scope_skills_root, name, bin_store, report)
            },
            Ok(meta) if meta.is_dir() && !scope_owns_skill(&dest) => {
                top_up_skill(&hub, source, &dest, bin_store, report)
            },
            Ok(_) => Ok(()),
            Err(error) => Err(error),
        };
        if let Err(error) = outcome {
            report.errors.push(format!("{name}: {error}"));
        }
    }
}

/// A scope's own copy of a skill (a real `SKILL.md`) is never overlaid. A
/// manifest the renderer wrote (marked by [`RENDER_MARKER`]) is setup-managed.
fn scope_owns_skill(dest: &Path) -> bool {
    matches!(fs::symlink_metadata(dest.join("SKILL.md")), Ok(meta) if !meta.file_type().is_symlink())
        && !dest.join(RENDER_MARKER).is_file()
}

/// Same layout as `install_skill_layer.py`: real directories, absolute file
/// links, `config` as a relative link to `.skill-state/<skill>/config`.
#[cfg(unix)]
fn install_skill(
    hub: &Path,
    source: &Path,
    skills_root: &Path,
    name: &str,
    bin_store: Option<&Path>,
    report: &mut ScopeLayerReport,
) -> io::Result<()> {
    let _lock = InstallLock::acquire(skills_root, name)?;
    let dest = skills_root.join(name);
    if fs::symlink_metadata(&dest).is_ok() {
        // A concurrent `make install-scope` got there first.
        return Ok(());
    }
    let (staging, stored) = stage_linked_tree(hub, source, skills_root, name, bin_store)?;
    // A whole staged directory swapped into place, not a file write: the
    // atomic-write helpers write one file's bytes, so they do not apply.
    if let Err(error) = fs::rename(&staging, &dest) {
        let _ = remove_any(&staging);
        return Err(error);
    }
    report.stored += stored;
    report.installed.push(name.to_string());
    Ok(())
}

/// Build `<skills>/<name>.new` in install-scope's layout and link the
/// source's config templates into the state dir. Returns the staging tree
/// and how many executables went to the store; the caller moves it into
/// place. Callers hold the skill's [`InstallLock`].
#[cfg(unix)]
fn stage_linked_tree(
    hub: &Path,
    source: &Path,
    skills_root: &Path,
    name: &str,
    bin_store: Option<&Path>,
) -> io::Result<(PathBuf, usize)> {
    let dest = skills_root.join(name);
    let state_config = skills_root.join(".skill-state").join(name).join("config");
    fs::create_dir_all(&state_config)?;
    link_config_templates(&source.join("config"), &state_config)?;

    let staging = skills_root.join(format!("{name}.new"));
    remove_any(&staging)?;
    let built = (|| -> io::Result<usize> {
        mirror_tree(source, &staging, true)?;
        let mut stored = 0;
        if let Some(store) = bin_store {
            for (bin_name, real) in storable_bins(hub, source) {
                let stored_file = materialize(store, &bin_name, &real)?;
                // Relative to the FINAL location; staging sits beside it.
                let target = relative_path(&dest.join("bin"), &stored_file);
                replace_symlink(&staging.join("bin").join(&bin_name), &target)?;
                stored += 1;
            }
        }
        std::os::unix::fs::symlink(
            Path::new("..")
                .join(".skill-state")
                .join(name)
                .join("config"),
            staging.join("config"),
        )?;
        Ok(stored)
    })();
    match built {
        Ok(stored) => Ok((staging, stored)),
        Err(error) => {
            let _ = remove_any(&staging);
            Err(error)
        },
    }
}

/// `install_skill_layer.py::_copy_runtime_config_into_state`: regular files
/// under a real `config/` (a copied install's secrets and sessions) move into
/// the state dir. A file the state already owns wins; a source template link
/// there yields to the runtime file.
#[cfg(unix)]
fn copy_runtime_config_into_state(config: &Path, state: &Path) -> io::Result<()> {
    match fs::symlink_metadata(config) {
        Ok(meta) if meta.is_dir() => {},
        _ => return Ok(()),
    }
    let mut stack = vec![PathBuf::new()];
    while let Some(relative) = stack.pop() {
        fs::create_dir_all(state.join(&relative))?;
        for entry in fs::read_dir(config.join(&relative))?.flatten() {
            let child = relative.join(entry.file_name());
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(child);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let target = state.join(&child);
            match fs::symlink_metadata(&target) {
                Ok(meta) if !meta.file_type().is_symlink() => continue,
                Ok(_) => fs::remove_file(&target)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {},
                Err(error) => return Err(error),
            }
            // `fs::copy` keeps the mode: a 0600 `.env` stays 0600.
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Swap two existing paths in one rename (`install_skill_layer.py`'s
/// `_atomic_exchange`): a reader resolves the old tree or the new one, never
/// a missing skill.
#[cfg(target_os = "macos")]
fn exchange_paths(left: &Path, right: &Path) -> io::Result<()> {
    let (left, right) = (c_path(left)?, c_path(right)?);
    // SAFETY: both are NUL-terminated paths that outlive the call.
    let result = unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            left.as_ptr(),
            libc::AT_FDCWD,
            right.as_ptr(),
            libc::RENAME_SWAP,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn exchange_paths(left: &Path, right: &Path) -> io::Result<()> {
    let (left, right) = (c_path(left)?, c_path(right)?);
    // SAFETY: both are NUL-terminated paths that outlive the call.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            left.as_ptr(),
            libc::AT_FDCWD,
            right.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn exchange_paths(_left: &Path, _right: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic directory exchange is unsupported",
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    Ok(std::ffi::CString::new(path.as_os_str().as_bytes())?)
}

/// Repair an installed mirror: store pinned executables, re-point dangling
/// links, add links for source files the scope does not have.
#[cfg(unix)]
fn top_up_skill(
    hub: &Path,
    source: &Path,
    dest: &Path,
    bin_store: Option<&Path>,
    report: &mut ScopeLayerReport,
) -> io::Result<()> {
    let mut handled = BTreeSet::new();
    if let Some(store) = bin_store {
        for (bin_name, real) in storable_bins(hub, source) {
            let stored_file = materialize(store, &bin_name, &real)?;
            if link_to_store(&stored_file, &dest.join("bin").join(&bin_name))? {
                report.stored += 1;
            }
            handled.insert(bin_name);
        }
    }
    heal_dangling(hub, dest, dest, &handled, report)?;
    add_missing(source, dest, Path::new(""), &handled, report)
}

#[cfg(unix)]
fn heal_dangling(
    hub: &Path,
    skill_dest: &Path,
    dir: &Path,
    handled: &BTreeSet<String>,
    report: &mut ScopeLayerReport,
) -> io::Result<()> {
    for entry in fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            heal_dangling(hub, skill_dest, &path, handled, report)?;
            continue;
        }
        if !file_type.is_symlink() || fs::metadata(&path).is_ok() {
            continue;
        }
        let in_handled_bin = dir == skill_dest.join("bin")
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| handled.contains(n));
        if in_handled_bin {
            continue;
        }
        let target = fs::read_link(&path)?;
        let candidate = target
            .is_absolute()
            .then(|| super::path_rewrite::rewrite_under_skillshub(&target, hub))
            .flatten();
        match candidate {
            Some(candidate) if fs::metadata(&candidate).is_ok() => {
                replace_symlink(&path, &candidate)?;
                report.relinked += 1;
            },
            other => report.unresolved.push(UnresolvedLink {
                scoped: path,
                expected: other.unwrap_or(target),
            }),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn add_missing(
    source_root: &Path,
    dest_root: &Path,
    relative: &Path,
    handled: &BTreeSet<String>,
    report: &mut ScopeLayerReport,
) -> io::Result<()> {
    let source_dir = source_root.join(relative);
    let mut entries: Vec<_> = fs::read_dir(&source_dir)?.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        let path = entry.path();
        let file_type = entry.file_type()?;
        let child = relative.join(&name);
        if file_type.is_dir() {
            let top_config = relative.as_os_str().is_empty() && name_str == "config";
            if !top_config && !TOP_UP_SKIP_DIRS.contains(&name_str) {
                add_missing(source_root, dest_root, &child, handled, report)?;
            }
            continue;
        }
        if file_type.is_symlink() && fs::metadata(&path).map(|m| m.is_dir()).unwrap_or(false) {
            continue;
        }
        if matches!(name_str, ".DS_Store" | ".env")
            || (relative == Path::new("bin") && handled.contains(name_str))
            || is_render_template_file(source_root, relative, name_str)
            || fs::metadata(&path).is_err()
        {
            continue;
        }
        let scoped = dest_root.join(&child);
        match fs::symlink_metadata(&scoped) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {},
            _ => continue,
        }
        let parent = scoped.parent().unwrap_or(dest_root);
        match fs::symlink_metadata(parent) {
            Ok(meta) if !meta.is_dir() => continue,
            Ok(_) => {},
            Err(_) => fs::create_dir_all(parent)?,
        }
        std::os::unix::fs::symlink(&path, &scoped)?;
        report.added += 1;
    }
    Ok(())
}

#[cfg(unix)]
fn mirror_tree(source: &Path, dest: &Path, top: bool) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(source)?.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if !INSTALL_SKIP_DIRS.contains(&name_str) && !(top && name_str == "config") {
                mirror_tree(&path, &dest.join(&name), false)?;
            }
            continue;
        }
        // A link to a directory is not descended (os.walk parity).
        if file_type.is_symlink() && fs::metadata(&path).map(|m| m.is_dir()).unwrap_or(false) {
            continue;
        }
        if name_str == ".DS_Store"
            || (top && is_render_template_file(source, Path::new(""), name_str))
        {
            continue;
        }
        std::os::unix::fs::symlink(&path, dest.join(&name))?;
    }
    Ok(())
}

/// Source-owned config templates become links in the scope's state dir;
/// runtime-owned regular files there (secrets, sessions) always win.
#[cfg(unix)]
fn link_config_templates(source: &Path, state: &Path) -> io::Result<()> {
    if !source.is_dir() {
        return Ok(());
    }
    let mut stack = vec![PathBuf::new()];
    while let Some(relative) = stack.pop() {
        fs::create_dir_all(state.join(&relative))?;
        for entry in fs::read_dir(source.join(&relative))?.flatten() {
            let name = entry.file_name();
            let child = relative.join(&name);
            if entry.file_type()?.is_dir() {
                if name != "_vendor" {
                    stack.push(child);
                }
                continue;
            }
            if name == ".DS_Store" || child == Path::new(".env") {
                continue;
            }
            let target = state.join(&child);
            match fs::symlink_metadata(&target) {
                Ok(meta) if !meta.file_type().is_symlink() => continue,
                _ => replace_symlink(&target, &entry.path())?,
            }
        }
    }
    Ok(())
}

/// `(name, real file)` for every `bin/` entry that resolves to a native
/// executable skillshub's own setup owns. `pinned_bin_store.store_candidate`.
fn storable_bins(hub: &Path, source: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = fs::read_dir(source.join("bin")) else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let real = fs::canonicalize(entry.path()).ok()?;
            let inside = real.strip_prefix(hub).ok()?;
            let unowned = inside
                .components()
                .any(|c| UNOWNED_TREES.iter().any(|tree| c.as_os_str() == *tree));
            (!unowned && is_native_executable(&real)).then_some((name, real))
        })
        .collect();
    out.sort();
    out
}

fn is_native_executable(path: &Path) -> bool {
    use std::io::Read;
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return false;
        }
    }
    if !meta.is_file() {
        return false;
    }
    let mut magic = [0u8; 4];
    fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut magic))
        .is_ok()
        && NATIVE_MAGIC.contains(&magic)
}

/// sha256 of a file, memoised by (path, length, mtime) so a boot that
/// reconciles many scopes hashes each executable once.
fn file_digest(path: &Path) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    type Key = (PathBuf, u64, Option<SystemTime>);
    static MEMO: OnceLock<Mutex<HashMap<Key, String>>> = OnceLock::new();
    let meta = fs::metadata(path)?;
    let key: Key = (path.to_path_buf(), meta.len(), meta.modified().ok());
    let memo = MEMO.get_or_init(Default::default);
    if let Some(digest) = memo.lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
        return Ok(digest.clone());
    }
    use std::io::Read;
    let mut hasher = Sha256::new();
    let mut file = fs::File::open(path)?;
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hex::encode(hasher.finalize());
    memo.lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(key, digest.clone());
    Ok(digest)
}

/// Copy `source` into `<store>/<sha256>/<name>` once; return the stored path.
fn materialize(store: &Path, name: &str, source: &Path) -> io::Result<PathBuf> {
    let digest = file_digest(source)?;
    let dir = store.join(digest);
    let stored = dir.join(name);
    let source_len = fs::metadata(source)?.len();
    if matches!(fs::metadata(&stored), Ok(meta) if meta.is_file() && meta.len() == source_len) {
        return Ok(stored);
    }
    fs::create_dir_all(&dir)?;
    // Hand-rolled on purpose: the stored file is an executable that must carry
    // its mode before it becomes visible, and the store is content-addressed
    // (a digest directory), so a torn or concurrent copy is replaced whole by
    // the next run's length check rather than read.
    let temporary = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    fs::copy(source, &temporary)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755))?;
    }
    fs::rename(&temporary, &stored)?;
    Ok(stored)
}

/// Point `scoped` at `stored` relatively. A regular file there is the
/// scope's own and is left alone. True when the link changed.
#[cfg(unix)]
fn link_to_store(stored: &Path, scoped: &Path) -> io::Result<bool> {
    let parent = scoped.parent().unwrap_or(Path::new("."));
    let desired = relative_path(parent, stored);
    match fs::symlink_metadata(scoped) {
        Ok(meta) if meta.file_type().is_symlink() => {
            if fs::read_link(scoped)? == desired {
                return Ok(false);
            }
        },
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {},
        Err(error) => return Err(error),
    }
    fs::create_dir_all(parent)?;
    replace_symlink(scoped, &desired)?;
    Ok(true)
}

#[cfg(unix)]
fn replace_symlink(path: &Path, target: &Path) -> io::Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // A symlink swapped atomically, not file bytes; the atomic-write helpers
    // do not create links.
    let temporary = parent.join(format!(".{name}.{}.new", std::process::id()));
    remove_any(&temporary)?;
    std::os::unix::fs::symlink(target, &temporary)?;
    fs::rename(&temporary, path)
}

fn remove_any(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Lexical `os.path.relpath(target, from_dir)` for absolute paths.
pub fn relative_path(from_dir: &Path, target: &Path) -> PathBuf {
    let from = normalize(from_dir);
    let to = normalize(target);
    let common = from
        .iter()
        .zip(to.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let mut out = PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    for part in &to[common..] {
        out.push(part);
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

fn normalize(path: &Path) -> Vec<std::ffi::OsString> {
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {},
            Component::ParentDir => {
                parts.pop();
            },
            other => parts.push(other.as_os_str().to_os_string()),
        }
    }
    parts
}

/// Cross-process exclusion shared with `install_skill_layer.py`
/// (`<skills>/.skill-install-locks/<skill>.lock`, `flock`).
#[cfg(unix)]
struct InstallLock(fs::File);

#[cfg(unix)]
impl InstallLock {
    fn acquire(skills_root: &Path, name: &str) -> io::Result<Self> {
        use fs2::FileExt;
        let dir = skills_root.join(".skill-install-locks");
        fs::create_dir_all(&dir)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(format!("{name}.lock")))?;
        file.lock_exclusive()?;
        Ok(Self(file))
    }
}

#[cfg(unix)]
impl Drop for InstallLock {
    fn drop(&mut self) {
        // `std::fs::File::unlock` (inherent) releases the fs2 `flock`.
        let _ = self.0.unlock();
    }
}

#[cfg(all(unix, any(test, feature = "test-fixtures")))]
mod tests {
    use super::*;

    const MACHO: &[u8] = &[0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0, 1, 2, 3, 4];

    fn write_exe(path: &Path, bytes: &[u8]) {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        hub: PathBuf,
        skills: PathBuf,
        store: PathBuf,
    }

    /// A skillshub with a `browser` skill (native `agent-browser`, a script,
    /// a config template, an npm-owned native) and an empty data root.
    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let hub = base.join("checkout/skillshub");
        let skill = hub.join("browser");
        fs::create_dir_all(skill.join("config")).unwrap();
        fs::write(skill.join("SKILL.md"), "---\nname: browser\n---\n").unwrap();
        fs::write(skill.join("config/.env.example"), "KEY=\n").unwrap();
        write_exe(&skill.join("bin/agent-browser"), MACHO);
        write_exe(&skill.join("bin/helper"), b"#!/bin/sh\nexit 0\n");
        write_exe(&hub.join("node_modules/rg/rg"), MACHO);
        std::os::unix::fs::symlink(hub.join("node_modules/rg/rg"), skill.join("bin/rg")).unwrap();
        write_exe(&skill.join("_vendor/big"), MACHO);
        let skills = base.join("data/scopes/alice/project/skills");
        let store = bin_store_for_scope_skills_root(&skills).unwrap();
        Fixture {
            _temp: temp,
            hub,
            skills,
            store,
        }
    }

    fn reconcile(f: &Fixture) -> ScopeLayerReport {
        reconcile_scope_skills_layer(&f.hub, &f.skills, Some(&f.store))
    }

    #[test]
    fn store_root_is_derived_only_for_scope_skills_trees() {
        assert_eq!(
            bin_store_for_scope_skills_root(Path::new("/d/scopes/p/w/skills")),
            Some(PathBuf::from("/d/system/skill-bin"))
        );
        assert_eq!(
            bin_store_for_scope_skills_root(Path::new("/c/skills")),
            None
        );
        assert_eq!(
            bin_store_for_scope_skills_root(Path::new("/d/other/p/w/skills")),
            None
        );
    }

    #[test]
    fn relative_path_matches_os_path_relpath() {
        assert_eq!(
            relative_path(
                Path::new("/d/scopes/p/w/skills/browser/bin"),
                Path::new("/d/system/skill-bin/abc/agent-browser")
            ),
            PathBuf::from("../../../../../../system/skill-bin/abc/agent-browser")
        );
    }

    /// A render-declared skill: placeholder template, render spec, a script.
    fn add_rendered_skill(f: &Fixture) -> PathBuf {
        let skill = f.hub.join("metabase-mcp");
        fs::create_dir_all(skill.join("config")).unwrap();
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: metabase-mcp\nendpoint: https://metabase.invalid/api/metabase-mcp\n---\n",
        )
        .unwrap();
        fs::write(
            skill.join(RENDER_SPEC),
            "schema_version: magician.skill-render.v1\n",
        )
        .unwrap();
        fs::write(skill.join("config/.env.example"), "METABASE_BASE_URL=\n").unwrap();
        fs::write(skill.join("notes.md"), "notes").unwrap();
        skill
    }

    /// The placeholder manifest never reaches a scope: setup renders it.
    #[test]
    fn a_render_declared_skill_installs_without_its_template_manifest() {
        let f = fixture();
        add_rendered_skill(&f);
        let report = reconcile(&f);
        assert!(report.errors.is_empty(), "{report:?}");
        let scoped = f.skills.join("metabase-mcp");
        assert!(
            scoped.join("notes.md").exists(),
            "other files are installed"
        );
        assert!(scoped.join("config").exists());
        assert!(fs::symlink_metadata(scoped.join("SKILL.md")).is_err());
        assert!(fs::symlink_metadata(scoped.join(RENDER_SPEC)).is_err());
    }

    /// A rendered manifest is setup-managed: topped up, never overwritten,
    /// unlike a manifest the scope owns outright.
    #[test]
    fn a_rendered_manifest_is_topped_up_but_left_as_written() {
        let f = fixture();
        let skill = add_rendered_skill(&f);
        reconcile(&f);
        let scoped = f.skills.join("metabase-mcp");
        let rendered =
            "---\nname: metabase-mcp\nendpoint: https://mb.example.com/api/metabase-mcp\n---\n";
        fs::write(scoped.join("SKILL.md"), rendered).unwrap();
        fs::write(scoped.join(RENDER_MARKER), "{}").unwrap();
        fs::write(skill.join("added_later.md"), "new").unwrap();
        let report = reconcile(&f);
        assert!(report.errors.is_empty(), "{report:?}");
        assert!(
            scoped.join("added_later.md").exists(),
            "a marked copy is still topped up"
        );
        assert_eq!(
            fs::read_to_string(scoped.join("SKILL.md")).unwrap(),
            rendered
        );
        assert!(!fs::symlink_metadata(scoped.join("SKILL.md"))
            .unwrap()
            .file_type()
            .is_symlink());

        // Without the marker the same file is scope-owned and not topped up.
        fs::remove_file(scoped.join(RENDER_MARKER)).unwrap();
        fs::write(skill.join("added_even_later.md"), "new").unwrap();
        reconcile(&f);
        assert!(!scoped.join("added_even_later.md").exists());
    }

    /// A workspace created after setup gets a complete layer on first touch,
    /// in install-scope's layout.
    #[test]
    fn a_new_scope_is_installed_with_its_pinned_cli_stored() {
        let f = fixture();
        let report = reconcile(&f);
        assert_eq!(report.installed, vec!["browser".to_string()], "{report:?}");
        assert!(
            report.unresolved.is_empty() && report.errors.is_empty(),
            "{report:?}"
        );
        let browser = f.skills.join("browser");
        let cli = browser.join("bin/agent-browser");
        let link = fs::read_link(&cli).unwrap();
        assert!(link.is_relative(), "store link must be relative: {link:?}");
        assert_eq!(fs::read(&cli).unwrap(), MACHO);
        assert!(fs::canonicalize(&cli).unwrap().starts_with(&f.store));
        // Scripts and npm-owned natives stay absolute links into skillshub.
        assert!(fs::read_link(browser.join("bin/helper"))
            .unwrap()
            .is_absolute());
        assert!(fs::read_link(browser.join("bin/rg")).unwrap().is_absolute());
        // install-scope layout: config via state, no _vendor.
        assert_eq!(
            fs::read_link(browser.join("config")).unwrap(),
            Path::new("../.skill-state/browser/config")
        );
        assert!(browser.join("config/.env.example").exists());
        assert!(!browser.join("_vendor").exists());
        // Idempotent.
        let again = reconcile(&f);
        assert!(!again.changed(), "{again:?}");
    }

    /// The 2026-09-30 incident: the checkout's generated bin is cleaned.
    #[test]
    fn a_cleaned_checkout_bin_does_not_break_the_scope() {
        let f = fixture();
        reconcile(&f);
        fs::remove_file(f.hub.join("browser/bin/agent-browser")).unwrap();
        let cli = f.skills.join("browser/bin/agent-browser");
        assert_eq!(fs::read(&cli).unwrap(), MACHO);
        let report = reconcile(&f);
        assert!(report.unresolved.is_empty(), "{report:?}");
        assert_eq!(fs::read(&cli).unwrap(), MACHO);
    }

    /// Links into a checkout that no longer exists (a copied data root, a
    /// moved repo) are re-pointed at the running skillshub.
    #[test]
    fn links_into_a_vanished_checkout_are_repointed() {
        let f = fixture();
        reconcile(&f);
        let manifest = f.skills.join("browser/SKILL.md");
        replace_symlink(
            &manifest,
            Path::new("/gone/checkout/skillshub/browser/SKILL.md"),
        )
        .unwrap();
        // An old-style absolute bin link from before the store existed.
        let cli = f.skills.join("browser/bin/agent-browser");
        replace_symlink(
            &cli,
            Path::new("/gone/checkout/skillshub/browser/bin/agent-browser"),
        )
        .unwrap();
        let report = reconcile(&f);
        assert_eq!(report.relinked, 1, "{report:?}");
        assert_eq!(report.stored, 1, "{report:?}");
        assert_eq!(
            fs::canonicalize(&manifest).unwrap(),
            f.hub.join("browser/SKILL.md")
        );
        assert_eq!(fs::read(&cli).unwrap(), MACHO);
    }

    /// When the source artifact is gone too, the link is reported with the
    /// path setup must rebuild, not silently skipped.
    #[test]
    fn a_missing_source_artifact_is_reported_unresolved() {
        let f = fixture();
        reconcile(&f);
        let tool = f.hub.join("browser/bin/tool");
        write_exe(&tool, b"#!/bin/sh\n");
        reconcile(&f);
        fs::remove_file(&tool).unwrap();
        let report = reconcile(&f);
        assert_eq!(
            report.unresolved,
            vec![UnresolvedLink {
                scoped: f.skills.join("browser/bin/tool"),
                expected: tool,
            }]
        );
    }

    #[test]
    fn files_the_skill_gained_after_install_are_added() {
        let f = fixture();
        reconcile(&f);
        fs::create_dir_all(f.hub.join("browser/scripts")).unwrap();
        fs::write(f.hub.join("browser/scripts/new.sh"), "echo").unwrap();
        let report = reconcile(&f);
        assert_eq!(report.added, 1, "{report:?}");
        assert!(f.skills.join("browser/scripts/new.sh").exists());
    }

    #[test]
    fn a_scope_owned_skill_copy_is_never_touched() {
        let f = fixture();
        let own = f.skills.join("browser");
        fs::create_dir_all(&own).unwrap();
        fs::write(own.join("SKILL.md"), "mine").unwrap();
        let report = reconcile(&f);
        assert!(!report.changed(), "{report:?}");
        assert!(!own.join("bin").exists());
    }

    /// Reconcile installs whatever a scope lacks, so an uninstall skeleton
    /// (only the kept `config` link left) used to be topped back up into the
    /// skill on the next boot.
    #[test]
    fn a_skill_the_operator_removed_stays_removed() {
        let f = fixture();
        reconcile(&f);
        let browser = f.skills.join("browser");
        for entry in fs::read_dir(&browser).unwrap().flatten() {
            if entry.file_name() != "config" {
                remove_any(&entry.path()).unwrap();
            }
        }
        mark_skill_removed(&f.skills, "browser").unwrap();
        let report = reconcile(&f);
        assert!(!report.changed(), "{report:?}");
        assert!(fs::symlink_metadata(browser.join("SKILL.md")).is_err());

        // A purge removes the folder outright; still not reinstalled.
        remove_any(&browser).unwrap();
        let report = reconcile(&f);
        assert!(!report.changed(), "{report:?}");
        assert!(!browser.exists());
    }

    /// A copied install (dropped-in folder, the old API install) becomes the
    /// linked layout; its secrets and auth survive and the removal mark goes.
    #[test]
    fn a_linked_install_replaces_a_copy_and_keeps_its_runtime_state() {
        use std::os::unix::fs::PermissionsExt;
        let f = fixture();
        let browser = f.skills.join("browser");
        fs::create_dir_all(browser.join("config")).unwrap();
        fs::create_dir_all(browser.join("auth")).unwrap();
        fs::write(browser.join("SKILL.md"), "---\nname: browser\n---\ncopy\n").unwrap();
        fs::write(browser.join("config/.env"), "KEY=1\n").unwrap();
        fs::set_permissions(
            browser.join("config/.env"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        fs::write(browser.join("auth/session.json"), "{}").unwrap();
        mark_skill_removed(&f.skills, "browser").unwrap();

        install_linked_skill(&f.hub, &f.skills, "browser", Some(&f.store)).unwrap();

        assert_eq!(
            fs::read_link(browser.join("SKILL.md")).unwrap(),
            f.hub.join("browser/SKILL.md")
        );
        assert_eq!(
            fs::read_link(browser.join("config")).unwrap(),
            Path::new("../.skill-state/browser/config")
        );
        let env = browser.join("config/.env");
        assert_eq!(fs::read_to_string(&env).unwrap(), "KEY=1\n");
        assert_eq!(
            fs::metadata(&env).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(browser.join("config/.env.example").exists());
        assert_eq!(
            fs::read_to_string(browser.join("auth/session.json")).unwrap(),
            "{}"
        );
        assert!(!skill_marked_removed(&f.skills, "browser"));
        assert!(!f.skills.join("browser.new").exists());
        let again = reconcile(&f);
        assert!(!again.changed(), "{again:?}");

        // Reinstalling a linked install is a clean swap too.
        install_linked_skill(&f.hub, &f.skills, "browser", Some(&f.store)).unwrap();
        assert_eq!(fs::read_to_string(&env).unwrap(), "KEY=1\n");
        assert!(!f.skills.join("browser.new").exists());
    }

    #[test]
    fn a_purge_drops_the_state_dir() {
        let f = fixture();
        reconcile(&f);
        assert!(f.skills.join(".skill-state/browser/config").is_dir());
        purge_skill_state(&f.skills, "browser").unwrap();
        assert!(!f.skills.join(".skill-state/browser").exists());
        purge_skill_state(&f.skills, "browser").unwrap();
    }

    #[test]
    fn without_a_store_executables_stay_plain_links() {
        let f = fixture();
        let report = reconcile_scope_skills_layer(&f.hub, &f.skills, None);
        assert_eq!(report.stored, 0);
        assert_eq!(
            fs::read_link(f.skills.join("browser/bin/agent-browser")).unwrap(),
            f.hub.join("browser/bin/agent-browser")
        );
    }

    /// A scope bots tree whose `kapso` bundle links into a checkout that was
    /// deleted, as after the 2026-10-02 worktree cleanup.
    fn bots_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let hub = base.join("checkout/skillshub");
        fs::create_dir_all(hub.join("bots/kapso/dist")).unwrap();
        fs::write(hub.join("bots/kapso/dist/index.js"), "// kapso").unwrap();
        let bots = base.join("data/scopes/alice/project/bots");
        fs::create_dir_all(bots.join("kapso/dist")).unwrap();
        std::os::unix::fs::symlink(
            base.join("deleted-worktree/skillshub/bots/kapso/dist/index.js"),
            bots.join("kapso/dist/index.js"),
        )
        .unwrap();
        (temp, hub, bots)
    }

    #[test]
    fn a_dangling_bot_bundle_link_is_repointed_at_the_running_skillshub() {
        let (_temp, hub, bots) = bots_fixture();
        let report = heal_scope_bot_bundle_links(&hub, &bots);
        assert_eq!(report.relinked, 1, "{report:?}");
        assert_eq!(
            fs::read_link(bots.join("kapso/dist/index.js")).unwrap(),
            hub.join("bots/kapso/dist/index.js")
        );
        assert_eq!(
            fs::read_to_string(bots.join("kapso/dist/index.js")).unwrap(),
            "// kapso"
        );
        assert!(
            !heal_scope_bot_bundle_links(&hub, &bots).changed(),
            "second pass is a no-op"
        );
    }

    #[test]
    fn a_bot_bundle_link_that_still_resolves_is_left_alone() {
        let (_temp, hub, bots) = bots_fixture();
        let other = hub
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("other-checkout/kapso.js");
        fs::create_dir_all(other.parent().unwrap()).unwrap();
        fs::write(&other, "// other").unwrap();
        fs::remove_file(bots.join("kapso/dist/index.js")).unwrap();
        std::os::unix::fs::symlink(&other, bots.join("kapso/dist/index.js")).unwrap();
        let report = heal_scope_bot_bundle_links(&hub, &bots);
        assert!(
            !report.changed() && report.unresolved.is_empty(),
            "{report:?}"
        );
        assert_eq!(
            fs::read_link(bots.join("kapso/dist/index.js")).unwrap(),
            other
        );
    }

    #[test]
    fn an_unbuilt_bot_bundle_is_reported_not_linked() {
        let (_temp, hub, bots) = bots_fixture();
        fs::remove_file(hub.join("bots/kapso/dist/index.js")).unwrap();
        let report = heal_scope_bot_bundle_links(&hub, &bots);
        assert_eq!(report.relinked, 0);
        assert_eq!(
            report.unresolved,
            vec![UnresolvedLink {
                scoped: bots.join("kapso/dist/index.js"),
                expected: hub.join("bots/kapso/dist/index.js"),
            }]
        );
    }
}
