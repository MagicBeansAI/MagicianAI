//! How to install, asked before what to install.
//!
//! The two paths differ only in where four binaries come from — `magician`,
//! `magicutor`, `magic-supervisor` and the desktop app. Everything after that
//! is host-side and identical, which is why this is one question and not two
//! installers.
//!
//! A path this machine cannot take is offered and disabled rather than hidden.
//! A missing choice reads as a product that does not do the thing; a disabled
//! one with a reason reads as a product that does not do it *yet*, and tells
//! the reader which it is.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMode {
    /// Nothing compiles. Binaries come from a published release.
    Prebuilt,
    /// Install a toolchain if needed, build, then install what was built.
    FromSource,
    /// The runtime in a container image.
    Container,
}

/// The machine, as far as this question is concerned. Gathered by `detect`, so
/// the offer rules below can be tested against machines nobody owns.
#[derive(Debug, Clone)]
pub struct Machine {
    pub os: String,
    /// A checkout is here, so building is even conceivable.
    pub source_present: bool,
    /// Where a package can be got from, if anywhere. `None` is the honest
    /// state until releases are published; a local path is how the path is
    /// exercised before then, and how an air-gapped install works after.
    pub prebuilt_source: Option<String>,
    /// Binaries are already built and not older than the sources they came
    /// from. This is what makes a second run of the from-source path quiet.
    pub artifacts_fresh: bool,
    /// The version already installed at the prefix, if anything is. Every route
    /// ends at the same prefix, so this is one question with one answer rather
    /// than one per route.
    pub installed: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfferState {
    Ready,
    /// Not available here, and why — shown beside the greyed choice.
    Unavailable(String),
}

impl OfferState {
    pub fn is_ready(&self) -> bool {
        matches!(self, OfferState::Ready)
    }
}

#[derive(Debug, Clone)]
pub struct Offer {
    pub mode: InstallMode,
    pub label: &'static str,
    /// One line under the label: what taking this path means.
    pub detail: String,
    pub state: OfferState,
}

/// Every path, in the order they should be read, each with its state.
///
/// Always the same three entries. A list whose length depends on the machine
/// makes "what can this product do" unanswerable from any single screen.
pub fn offers(machine: &Machine) -> Vec<Offer> {
    // Said once, on every route, because it is a fact about the machine rather
    // than about the path taken to it.
    let replacing = match &machine.installed {
        Some(version) => format!(" Replaces the installed {version}."),
        None => String::new(),
    };
    let prebuilt = match &machine.prebuilt_source {
        Some(source) => Offer {
            mode: InstallMode::Prebuilt,
            label: "Prebuilt",
            detail: format!(
                "Install what is already built, from {source}. Nothing compiles.{replacing}"
            ),
            state: OfferState::Ready,
        },
        None => Offer {
            mode: InstallMode::Prebuilt,
            label: "Prebuilt",
            detail: "Install what is already built. Nothing compiles.".into(),
            state: OfferState::Unavailable(format!(
                "no published release for {} yet — set MAGICIAN_PACKAGE to a local one",
                machine.os
            )),
        },
    };

    let from_source = {
        // Missing tools are what this path installs, so their absence is never
        // the reason it is unavailable. Missing source is.
        let state = if !machine.source_present {
            OfferState::Unavailable(
                "no source here — clone the repository first, or install prebuilt".into(),
            )
        } else if machine.os != "macos" {
            OfferState::Unavailable("Linux support is coming; macOS only for now".into())
        } else {
            OfferState::Ready
        };
        let detail = if machine.artifacts_fresh {
            format!("Already built here. It will reuse that rather than build again.{replacing}")
        } else {
            format!("Installs whatever is missing to build, then builds. Takes a while.{replacing}")
        };
        Offer {
            mode: InstallMode::FromSource,
            label: "From source",
            detail,
            state,
        }
    };

    let container = Offer {
        mode: InstallMode::Container,
        label: "Container",
        detail: "The whole runtime in one image.".into(),
        state: OfferState::Unavailable("coming soon".into()),
    };

    vec![prebuilt, from_source, container]
}

/// Ask the machine the three questions the offers depend on.
pub fn detect(repo_root: &Path) -> Machine {
    Machine {
        os: std::env::consts::OS.to_string(),
        source_present: source_present(repo_root),
        // Nothing is published yet, so the only package is one you already
        // have. This is the single place that grows a release lookup when
        // assets exist; until then the greyed reason is the honest state, and
        // MAGICIAN_PACKAGE is how the path gets exercised at all.
        prebuilt_source: std::env::var("MAGICIAN_PACKAGE")
            .ok()
            .filter(|p| !p.trim().is_empty())
            .filter(|p| Path::new(p).exists()),
        artifacts_fresh: artifacts_fresh(repo_root),
        installed: installed_version(),
    }
}

/// What the install prefix says is there. Read from the manifest the installer
/// left rather than by probing binaries: the manifest records what was meant to
/// be installed, and a half-removed prefix should not read as a healthy one.
pub fn installed_version() -> Option<String> {
    let prefix = std::env::var("MAGICIAN_PREFIX")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_default()
                .join(".magician")
        });
    let manifest = std::fs::read_to_string(prefix.join("MANIFEST.yaml")).ok()?;
    manifest
        .lines()
        .find_map(|line| line.strip_prefix("version:"))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// A workspace root with the crate that produces the runtime binary. Checking
/// for a directory called `magician` would match any parent folder someone
/// happened to name that.
fn source_present(repo_root: &Path) -> bool {
    repo_root.join("Cargo.toml").is_file() && repo_root.join("magician-bin/Cargo.toml").is_file()
}

/// Whether every runtime binary exists and is at least as new as the newest
/// source it could have been built from. Missing or stale is the same answer
/// here — both mean a build has to happen — and saying "fresh" when unsure
/// would skip the build and install something older than the code.
fn artifacts_fresh(repo_root: &Path) -> bool {
    let target = std::env::var("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| repo_root.join("target"));
    let release = target.join("release");

    let binaries = ["magician", "magicutor", "magic-supervisor"];
    let mut oldest_binary: Option<std::time::SystemTime> = None;
    for name in binaries {
        let Ok(meta) = std::fs::metadata(release.join(name)) else {
            return false;
        };
        let Ok(modified) = meta.modified() else {
            return false;
        };
        oldest_binary = Some(match oldest_binary {
            None => modified,
            Some(current) => current.min(modified),
        });
    }
    let Some(oldest_binary) = oldest_binary else {
        return false;
    };

    !any_source_newer_than(repo_root, oldest_binary)
}

/// Walks the crate sources only. Walking the whole root would descend into the
/// build directory and compare the binaries against themselves.
fn any_source_newer_than(repo_root: &Path, when: std::time::SystemTime) -> bool {
    let mut stack: Vec<std::path::PathBuf> = ["magician-bin", "magicutor", "magic-supervisor"]
        .iter()
        .map(|c| repo_root.join(c).join("src"))
        .collect();

    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let newer = entry
                .metadata()
                .and_then(|m| m.modified())
                .map(|m| m > when)
                .unwrap_or(false);
            if newer {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests;
