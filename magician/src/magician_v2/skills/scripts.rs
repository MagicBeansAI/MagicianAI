//! Maps a skill's `scripts/` directory to ephemeral tools.
//!
//! Each script becomes an ephemeral tool named `<skill>__<stem>` with a
//! runtime inferred from extension:
//!   `.sh` / `.bash` → Bash
//!   `.py`           → Python
//!   `.js` / `.mjs`  → Node
//! Files with other extensions are skipped.

use std::path::Path;

use super::activation::{EphemeralTool, ScriptRuntime};

/// Walk the skill's `scripts/` directory; return one EphemeralTool per
/// recognized script. Files without a recognized extension are silently
/// skipped.
pub fn register_scripts(
    scripts_dir: &Path,
    skill_name: &str,
    skill_dir: &Path,
) -> anyhow::Result<Vec<EphemeralTool>> {
    let mut out = Vec::new();
    if !scripts_dir.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(scripts_dir)? {
        let entry = entry?;
        let p = entry.path();
        // Each script is a host-absolute symlink into skillshub on a
        // materialized scope; check existence via the env-resolved path
        // (identity natively) so a container doesn't skip every script as a
        // dangling symlink. The original `p` is stored and resolved at spawn.
        if !super::path_rewrite::resolve_skill_path(&p).is_file() {
            continue;
        }
        let stem = match p.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let runtime = match p.extension().and_then(|s| s.to_str()) {
            Some("sh") | Some("bash") => ScriptRuntime::Bash,
            Some("py") => ScriptRuntime::Python,
            Some("js") | Some("mjs") => ScriptRuntime::Node,
            _ => continue,
        };
        out.push(EphemeralTool {
            name: format!("{skill_name}__{stem}"),
            script_path: p.clone(),
            runtime,
            skill_dir: skill_dir.to_path_buf(),
            description: format!("Script from skill {skill_name}: {stem}"),
        });
    }
    // Stable ordering so callers (and tests) get deterministic results.
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn registers_each_script_with_runtime_inferred_from_extension() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("toolskill");
        let scripts = skill_dir.join("scripts");
        fs::create_dir_all(&scripts).unwrap();
        fs::write(scripts.join("a.sh"), "#!/bin/sh\n").unwrap();
        fs::write(scripts.join("b.py"), "#!/usr/bin/env python3\n").unwrap();
        fs::write(scripts.join("c.mjs"), "#!/usr/bin/env node\n").unwrap();
        fs::write(scripts.join("readme.txt"), "ignored\n").unwrap();

        let tools = register_scripts(&scripts, "toolskill", &skill_dir).unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["toolskill__a", "toolskill__b", "toolskill__c"]);

        let by_name: std::collections::HashMap<&str, &EphemeralTool> =
            tools.iter().map(|t| (t.name.as_str(), t)).collect();
        assert_eq!(by_name["toolskill__a"].runtime, ScriptRuntime::Bash);
        assert_eq!(by_name["toolskill__b"].runtime, ScriptRuntime::Python);
        assert_eq!(by_name["toolskill__c"].runtime, ScriptRuntime::Node);
    }

    #[test]
    fn missing_scripts_dir_returns_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let tools = register_scripts(&dir.path().join("nope"), "x", dir.path()).unwrap();
        assert!(tools.is_empty());
    }
}
