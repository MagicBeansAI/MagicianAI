//! `requires.bins` enforcement at activation time.
//!
//! Resolution order for a skill that declares `requires.bins: [<bin>]`:
//!   1. The skill's own `bin/<bin>` (if the skill ships its binary).
//!   2. System `$PATH` via `which`.
//!
//! When neither is available, `check_required_bins` returns an error whose
//! message includes the install hint from `metadata.magician.install_hint`.

use anyhow::Result;

use super::manifest::{MagicianMetadata, SkillManifest};

/// Verify every `requires.bins` entry resolves. No-op if the skill declares
/// no `requires.bins` (or has no `metadata.magician` block).
pub fn check_required_bins(manifest: &SkillManifest) -> Result<()> {
    let mag = match manifest.metadata.magician.as_ref() {
        Some(m) => m,
        None => return Ok(()),
    };
    for bin in &mag.requires.bins {
        if is_bin_available(bin, &manifest.source_dir) {
            continue;
        }
        anyhow::bail!(
            "skill '{}' requires '{}' (none of: {}/bin/{}, system PATH); install via: {}",
            manifest.name,
            bin,
            manifest.source_dir.display(),
            bin,
            install_hint_text(mag),
        );
    }
    Ok(())
}

/// Return true if `bin` is found at `<skill-dir>/bin/<bin>` or anywhere on PATH.
pub fn is_bin_available(bin: &str, skill_dir: &std::path::Path) -> bool {
    // The skill's `bin/<bin>` is a host-absolute symlink into skillshub on a
    // materialized scope; rewrite it for this environment (identity natively) so
    // the probe doesn't false-negative in a container.
    let local = super::path_rewrite::resolve_skill_path(&skill_dir.join("bin").join(bin));
    if local.is_file() {
        return true;
    }
    which::which(bin).is_ok()
}

fn install_hint_text(m: &MagicianMetadata) -> String {
    if let Some(b) = m.install_hint.get("brew") {
        return format!("brew install {b}");
    }
    if let Some(c) = m.install_hint.get("cargo") {
        return format!("cargo install {c}");
    }
    if let Some(n) = m.install_hint.get("npm") {
        return format!("npm install -g {n}");
    }
    if let Some(g) = m.install_hint.get("go") {
        return format!("go install {g}");
    }
    if let Some(d) = m.install_hint.get("docs") {
        return format!("see {d}");
    }
    "(no install hint provided)".into()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::skills::loader::SkillLoader;
    use std::fs;

    fn write_skill(parent: &std::path::Path, name: &str, body: &str) {
        let p = parent.join(name);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("SKILL.md"), body).unwrap();
    }

    #[test]
    fn check_succeeds_when_no_requires_block() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(
            dir.path(),
            "no-bin",
            "---\nname: no-bin\ndescription: d\n---\n",
        );
        let m = SkillLoader::new(vec![dir.path().to_path_buf()])
            .discover()
            .unwrap()[0]
            .clone();
        assert!(check_required_bins(&m).is_ok());
    }

    #[test]
    fn check_fails_when_required_bin_missing() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "---
name: needs-bin
description: d
metadata:
  magician:
    requires:
      bins: [\"definitely-not-installed-anywhere\"]
    install_hint:
      brew: \"foo-cli\"
---
";
        write_skill(dir.path(), "needs-bin", yaml);
        let m = SkillLoader::new(vec![dir.path().to_path_buf()])
            .discover()
            .unwrap()[0]
            .clone();
        let err = check_required_bins(&m).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("definitely-not-installed-anywhere"));
        assert!(msg.contains("brew install foo-cli"));
    }

    #[test]
    fn check_succeeds_when_bin_is_local_to_skill() {
        let dir = tempfile::tempdir().unwrap();
        let skill = dir.path().join("ships-its-bin");
        let bin = skill.join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("ships-its-bin"), "#!/bin/sh\n").unwrap();
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: ships-its-bin\ndescription: d\n\
             metadata:\n  magician:\n    requires:\n      bins: [\"ships-its-bin\"]\n---\n",
        )
        .unwrap();

        let m = SkillLoader::new(vec![dir.path().to_path_buf()])
            .discover()
            .unwrap()[0]
            .clone();
        assert!(check_required_bins(&m).is_ok());
    }

    #[test]
    fn check_succeeds_when_bin_on_system_path() {
        let dir = tempfile::tempdir().unwrap();
        // `sh` is universally on PATH on every supported host.
        write_skill(
            dir.path(),
            "needs-sh",
            "---\nname: needs-sh\ndescription: d\n\
             metadata:\n  magician:\n    requires:\n      bins: [\"sh\"]\n---\n",
        );
        let m = SkillLoader::new(vec![dir.path().to_path_buf()])
            .discover()
            .unwrap()[0]
            .clone();
        assert!(check_required_bins(&m).is_ok());
    }
}
