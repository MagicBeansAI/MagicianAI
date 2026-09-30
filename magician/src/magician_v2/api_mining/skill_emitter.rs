//! Capability Evolution Phase 3: emit `SKILL.md` on promotion.
//!
//! When `CapabilityPromotionBridge` promotes a mined `ApiCapability`
//! to a `CapabilityPackRecord`, it also writes the skill folder form
//! under `<scope>/skills/evolved/<capability-name>/` so the evolved
//! pack appears in the AgentSkills catalog alongside hand-authored
//! skills. This module owns the file-emission half — the bridge
//! controls *when* to call us; we control *what gets written*.
//!
//! ## Subfolder rationale
//!
//! Evolved skills live under `skills/evolved/<name>/` rather than
//! `skills/<name>/` to prevent name collision with hand-authored
//! skills. Operators can audit auto-emitted entries by listing the
//! subfolder and never have to worry about retroactive migration when
//! mining produces a skill whose name overlaps with one they wrote.
//!
//! ## Collision policy
//!
//! If a hand-authored skill at `skills/<name>/` already exists with
//! the same name an evolved skill would emit to under `evolved/`,
//! the emit path still proceeds — the subfolder isolates them. If a
//! previous emission to `evolved/<name>/` exists, it's overwritten
//! (idempotent re-emit on bridge re-run is the intended behaviour).
//!
//! ## Replay-surface policy
//!
//! Legacy API-mined replay packs used to externalize as evolved skills whose
//! `run.sh` called `magician internal-replay`. That CLI surface does not exist
//! in the active runtime; direct replay now happens through `ApiRouter` +
//! `ApiRunner` at the browser primitive boundary. Phase 4 therefore keeps
//! API-mined replay records catalog-only and removes stale evolved skill folders
//! when encountered.

use std::fs;
use std::path::PathBuf;

use crate::magician_v2::artifact_v2::io::write_bytes_durably_sync;
use crate::magician_v2::execution::{
    is_legacy_browser_api_replay_definition, CapabilityPackRecord, CapabilityPackSource,
};

use crate::magician_v2::execution::ImplementationType;

/// Outcome of an evolved-skill emission attempt. Tracked alongside
/// the `CapabilityPromotionReport` so Phase 2 instrumentation can
/// surface emit failures as a distinct counter (vs catalog-write
/// failures, which already have their own surface).
#[derive(Debug, Clone)]
pub enum EmitOutcome {
    /// New skill folder created at the expected path.
    Emitted { path: PathBuf },
    /// Existing evolved skill found; frontmatter refreshed so the
    /// lifecycle_status reflects the latest promotion. Body
    /// unchanged.
    Refreshed { path: PathBuf },
    /// Skipped — the record's implementation isn't externalizable
    /// via skills (e.g. compiled packs). Operators don't need to see
    /// these in the skills catalog.
    Skipped { reason: &'static str },
    /// Emission was attempted but the I/O layer rejected it. The
    /// promotion bridge logs the error and continues — a failed
    /// skill emit must NOT block the catalog write.
    Failed { reason: String },
}

/// Emit (or refresh) the evolved skill folder for a freshly promoted
/// `CapabilityPackRecord`. Returns the outcome so the caller can
/// instrument success/failure counts.
///
/// Layout:
///
/// ```text
/// {scope_skills_root}/evolved/<name>/
///   SKILL.md       # frontmatter + body
///   scripts/
///     run.sh       # execution-disabled placeholder for non-API replay records
/// ```
pub fn emit_evolved_skill(
    scope_skills_root: &std::path::Path,
    record: &CapabilityPackRecord,
) -> EmitOutcome {
    let evolved_root = scope_skills_root.join("evolved");
    let skill_dir = evolved_root.join(safe_skill_name(&record.definition.name));

    if record.metadata.source == CapabilityPackSource::ApiMined
        || is_legacy_browser_api_replay_definition(&record.definition)
    {
        if let Err(error) = remove_stale_skill_dir(&skill_dir) {
            return EmitOutcome::Failed {
                reason: format!(
                    "failed to remove disabled evolved API replay skill {}: {error}",
                    skill_dir.display()
                ),
            };
        }
        return EmitOutcome::Skipped {
            reason: "api_mined_replay_skill_emission_disabled",
        };
    }

    if record.metadata.source == CapabilityPackSource::TaskRecipe {
        return emit_task_recipe_skill(&skill_dir, record);
    }

    // Externalizable records: Composite + Primitive. Compiled packs
    // stay in the runtime registry only; their behaviour lives in
    // Rust code, no skill folder is meaningful.
    if matches!(
        record.definition.implementation,
        ImplementationType::Compiled { .. }
    ) {
        return EmitOutcome::Skipped {
            reason: "compiled_impl",
        };
    }

    let scripts_dir = skill_dir.join("scripts");
    let skill_md_path = skill_dir.join("SKILL.md");
    let run_sh_path = scripts_dir.join("run.sh");

    let existed = skill_md_path.exists();
    if let Err(error) = fs::create_dir_all(&scripts_dir) {
        return EmitOutcome::Failed {
            reason: format!(
                "failed to create scripts dir {}: {error}",
                scripts_dir.display()
            ),
        };
    }

    if let Err(error) = atomic_write(&skill_md_path, build_skill_md(record).as_bytes()) {
        return EmitOutcome::Failed {
            reason: format!(
                "failed to write SKILL.md at {}: {error}",
                skill_md_path.display()
            ),
        };
    }

    if let Err(error) = atomic_write(&run_sh_path, build_run_script(record).as_bytes()) {
        return EmitOutcome::Failed {
            reason: format!(
                "failed to write run.sh at {}: {error}",
                run_sh_path.display()
            ),
        };
    }

    // Best-effort chmod +x on run.sh. Falls back to 0o755 even on
    // platforms where the unix perm bits don't matter (Windows
    // ignores the bit; we still set it so cross-platform tarball
    // exports stay portable).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&run_sh_path, fs::Permissions::from_mode(0o755));
    }

    if existed {
        EmitOutcome::Refreshed { path: skill_dir }
    } else {
        EmitOutcome::Emitted { path: skill_dir }
    }
}

fn emit_task_recipe_skill(
    skill_dir: &std::path::Path,
    record: &CapabilityPackRecord,
) -> EmitOutcome {
    let scripts_dir = skill_dir.join("scripts");
    let skill_path = skill_dir.join("SKILL.md");
    let run_path = scripts_dir.join("run.sh");
    let existed = skill_path.exists();
    if let Err(error) = fs::create_dir_all(&scripts_dir) {
        return EmitOutcome::Failed {
            reason: format!("failed to create task recipe skill directory: {error}"),
        };
    }
    let recipe_id = record.metadata.source_ref.as_deref().unwrap_or_default();
    let description = record
        .definition
        .description
        .as_deref()
        .unwrap_or("Learned no-browser API task recipe");
    let lifecycle = format!("{:?}", record.metadata.status).to_ascii_lowercase();
    let recipe_version = record
        .definition
        .version
        .as_deref()
        .and_then(|version| version.split('.').nth(1))
        .and_then(|version| version.parse::<u32>().ok())
        .unwrap_or(0);
    let skill = format!(
        "---\nname: {}\ndescription: {}\nmetadata:\n  magician:\n    task_recipe_id: {}\n    lifecycle_status: {}\n---\n\n# {}\n\nRuns the learned task recipe through Magician's scoped recipe replay API. Named inputs are passed as a JSON object to `scripts/run.sh`. Write steps remain protected by replay grants and approval policy.\n",
        yaml_quote(&record.definition.name),
        yaml_quote(description),
        yaml_quote(recipe_id),
        yaml_quote(&lifecycle),
        record.definition.name,
    );
    let script = format!(
        "#!/usr/bin/env bash\nset -euo pipefail\n: \"${{MAGICIAN_PRINCIPAL:?MAGICIAN_PRINCIPAL is required}}\"\n: \"${{MAGICIAN_WORKSPACE:?MAGICIAN_WORKSPACE is required}}\"\nbase=\"${{MAGICIAN_API_BASE:-http://127.0.0.1:3002/api/magician/v2}}\"\ninputs=\"${{1:-{{}}}}\"\ncurl --fail-with-body --silent --show-error \\\n  -H \"X-Principal: $MAGICIAN_PRINCIPAL\" \\\n  -H \"X-Workspace: $MAGICIAN_WORKSPACE\" \\\n  -H 'Content-Type: application/json' \\\n  --data \"{{\\\"inputs\\\":$inputs}}\" \\\n  \"$base/api-mining/recipes/{recipe_id}/replay?published_only=true&expected_version={recipe_version}\"\n"
    );
    if let Err(error) = atomic_write(&skill_path, skill.as_bytes())
        .and_then(|_| atomic_write(&run_path, script.as_bytes()))
    {
        return EmitOutcome::Failed {
            reason: format!("failed to persist task recipe skill: {error}"),
        };
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&run_path, fs::Permissions::from_mode(0o755));
    }
    if existed {
        EmitOutcome::Refreshed {
            path: skill_dir.to_path_buf(),
        }
    } else {
        EmitOutcome::Emitted {
            path: skill_dir.to_path_buf(),
        }
    }
}

/// Write `bytes` to `path` durably. Prevents half-written files if the
/// process crashes mid-write (operator would otherwise see a malformed
/// SKILL.md or run.sh).
///
/// Delegates to the shared writer rather than hand-rolling temp + rename:
/// this used to stage through a fixed `<file>.<ext>.tmp` sibling, which
/// every concurrent emitter of the same skill folder shares, so two
/// interleaving re-emits could rename a half-written SKILL.md over the
/// real one. The shared writer stages through a per-write unique name and
/// `sync_all`s both the file and its parent directory.
fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    write_bytes_durably_sync(path, bytes)
}

fn remove_stale_skill_dir(skill_dir: &std::path::Path) -> std::io::Result<()> {
    if skill_dir.exists() {
        fs::remove_dir_all(skill_dir)?;
    }
    Ok(())
}

pub fn remove_evolved_skill_for_record(
    scope_skills_root: &std::path::Path,
    record: &CapabilityPackRecord,
) -> std::io::Result<bool> {
    let skill_dir = scope_skills_root
        .join("evolved")
        .join(safe_skill_name(&record.definition.name));
    if !skill_dir.exists() {
        return Ok(false);
    }
    fs::remove_dir_all(skill_dir)?;
    Ok(true)
}

/// Sanitize a capability name for use as a filesystem segment. Mined
/// capability names come from URL paths and may contain characters
/// that aren't safe on macOS HFS+ / Windows NTFS / ext4. We normalize
/// to ASCII alphanumeric + dashes; everything else becomes `_`.
fn safe_skill_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '.' {
            out.push(ch);
        } else if matches!(ch, '_' | ' ' | '/' | ':') {
            out.push('_');
        }
        // Drop everything else — slashes, query chars, etc.
    }
    if out.is_empty() {
        out.push_str("evolved-skill");
    }
    out
}

/// Quote a string as a double-quoted YAML 1.2 scalar. Required for
/// frontmatter values that may contain URL templates (`{id}` triggers
/// YAML flow-mapping parsing), colons followed by whitespace (turns
/// the rest into a sub-mapping), or any other YAML special character.
///
/// Escapes backslashes + double quotes; leaves everything else as-is
/// since YAML double-quoted scalars accept any character except
/// unescaped `"` and `\`.
fn yaml_quote(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Render the SKILL.md content (frontmatter + body) for an evolved
/// capability. Body is intentionally terse — operators read the
/// frontmatter for context and the run.sh for behaviour; descriptive
/// prose belongs in the originating capability record, not here.
///
/// All interpolated frontmatter values are YAML-quoted via
/// `yaml_quote` so URL templates containing `{id}` placeholders (and
/// any other YAML special characters that show up in capability
/// records) don't break SkillLoader's parse.
fn build_skill_md(record: &CapabilityPackRecord) -> String {
    let capability_id = record
        .metadata
        .source_ref
        .clone()
        .unwrap_or_else(|| record.definition.name.clone());
    let mining_origin = extract_mining_origin(record);
    let lifecycle_status = format!("{:?}", record.metadata.status).to_lowercase();
    let description = record
        .definition
        .description
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("Evolved API replay for {}", record.definition.name));
    let name_raw = record.definition.name.clone();
    format!(
        "---\n\
name: {name}\n\
description: {description}\n\
metadata:\n\
  magician:\n\
    evolved_pack_ref:\n\
      capability_id: {capability_id}\n\
      mining_origin: {mining_origin}\n\
      lifecycle_status: {lifecycle_status}\n\
---\n\
\n\
# {name_raw}\n\
\n\
Auto-mined API replay skill emitted by the Capability Evolution\n\
bridge. Direct execution of emitted API-replay skills is disabled until\n\
Capability Evolution has a real generated-pack replay provider. Live API\n\
takeover uses Magician's `ApiRouter` and `ApiRunner` directly; see the\n\
originating capability\n\
record at `api_mining/<origin_key>/capabilities/{capability_id_raw}.json`\n\
for the request template, headers, and side-effect classification.\n\
\n\
Status: **{lifecycle_status}**. If this skill misbehaves, demote it\n\
via the Forge `/skills` page or the `POST\n\
/api/magician/v2/api-mining/capabilities/<origin_key>/{capability_id_raw}/demote`\n\
endpoint (planned).\n",
        name = yaml_quote(&name_raw),
        description = yaml_quote(&description),
        capability_id = yaml_quote(&capability_id),
        mining_origin = yaml_quote(&mining_origin),
        capability_id_raw = capability_id,
        name_raw = name_raw,
        lifecycle_status = lifecycle_status,
    )
}

/// Render the wrapper script. Legacy API-mined replay records are skipped before
/// this path. If another
/// externalizable record reaches script emission before a provider exists, fail
/// closed rather than pretending a CLI replay surface exists.
fn build_run_script(record: &CapabilityPackRecord) -> String {
    let capability_id = record
        .metadata
        .source_ref
        .clone()
        .unwrap_or_else(|| record.definition.name.clone());
    let mining_origin = extract_mining_origin(record);
    format!(
        "#!/usr/bin/env bash\n\
# Auto-generated by Capability Evolution. Do not edit by hand —\n\
# changes will be overwritten on the next bridge run.\n\
#\n\
# Direct API-replay skill execution is disabled until Capability\n\
# Evolution has a real generated-pack replay provider.\n\
set -euo pipefail\n\
\n\
echo \"Evolved API replay skill execution is disabled. Use live API-mining takeover or the operator replay API for capability {capability_id} at {mining_origin}.\" >&2\n\
exit 78\n",
        capability_id = capability_id,
        mining_origin = mining_origin,
    )
}

/// Read the mining origin from the record's step parameters. The
/// bridge writes it under the `url` step param at promotion time
/// inside `ImplementationType::Composite { steps }`; strip back to
/// origin (scheme + host) for the evolved skill's frontmatter so
/// operators see at-a-glance which service it talks to without the
/// path noise.
fn extract_mining_origin(record: &CapabilityPackRecord) -> String {
    let raw_url = match &record.definition.implementation {
        ImplementationType::Composite { steps } => steps
            .iter()
            .find_map(|step| step.parameters.get("url").cloned()),
        _ => None,
    }
    .unwrap_or_default();
    if raw_url.is_empty() {
        return "unknown-origin".to_string();
    }
    // Best-effort origin extraction: take everything up to the first
    // `/` after `scheme://`. Falls back to the raw value if it
    // doesn't look like a URL.
    if let Some(scheme_end) = raw_url.find("://") {
        let after_scheme = &raw_url[scheme_end + 3..];
        if let Some(slash) = after_scheme.find('/') {
            return raw_url[..scheme_end + 3 + slash].to_string();
        }
        return raw_url;
    }
    raw_url
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn safe_skill_name_strips_unsafe_chars() {
        assert_eq!(
            safe_skill_name("api/card/12116-query"),
            "api_card_12116-query"
        );
        assert_eq!(safe_skill_name("hello"), "hello");
        assert_eq!(safe_skill_name(""), "evolved-skill");
        assert_eq!(safe_skill_name("foo:bar/baz"), "foo_bar_baz");
    }

    #[test]
    fn yaml_quote_handles_url_templates_and_colons() {
        // The original bug: `{id}` inside an unquoted YAML scalar
        // triggers flow-mapping parsing and breaks SkillLoader's
        // frontmatter load.
        assert_eq!(
            yaml_quote("https://x.com/api/card/{id}/query"),
            "\"https://x.com/api/card/{id}/query\""
        );
        // Colon-space: only break point in unquoted scalars; the
        // quoted form is unaffected.
        assert_eq!(yaml_quote("foo: bar"), "\"foo: bar\"");
        // Newlines/tabs/quotes must be escape-sequenced inside
        // double-quoted YAML scalars.
        assert_eq!(yaml_quote("line1\nline2"), "\"line1\\nline2\"");
        assert_eq!(yaml_quote("She said \"hi\""), "\"She said \\\"hi\\\"\"");
        assert_eq!(yaml_quote(r"C:\path"), r#""C:\\path""#);
        // Empty / plain values still get wrapped — operators see a
        // consistent shape regardless of content.
        assert_eq!(yaml_quote("plain"), "\"plain\"");
        assert_eq!(yaml_quote(""), "\"\"");
    }

    // Integration tests for emit_evolved_skill cover the end-to-end
    // record→SKILL.md path. The struct shape of CapabilityPackRecord
    // changes often enough that unit-test stubs here would just churn;
    // the e2e fixture exercises the real construction path.

    /// The emitted skill files must be publishable in one step: a reader
    /// that lists the skill folder sees the final bytes or nothing, never
    /// a staging sibling that a skill loader would try to parse.
    #[test]
    fn atomic_write_publishes_without_leaving_a_staging_sibling() {
        let temp = tempfile::tempdir().expect("tempdir");
        let skill_dir = temp.path().join("evolved").join("sample");
        std::fs::create_dir_all(skill_dir.join("scripts")).expect("skill dir");
        let skill_md_path = skill_dir.join("SKILL.md");
        let run_sh_path = skill_dir.join("scripts").join("run.sh");

        atomic_write(&skill_md_path, b"---\nname: \"sample\"\n---\n").expect("SKILL.md write");
        atomic_write(&run_sh_path, b"#!/usr/bin/env bash\nexit 78\n").expect("run.sh write");

        assert_eq!(
            std::fs::read_to_string(&skill_md_path).expect("SKILL.md readable"),
            "---\nname: \"sample\"\n---\n"
        );
        assert_eq!(
            std::fs::read_to_string(&run_sh_path).expect("run.sh readable"),
            "#!/usr/bin/env bash\nexit 78\n"
        );
        assert!(
            !siblings_with_tmp_suffix(&skill_dir),
            "SKILL.md publish must leave no staging sibling in the skill folder"
        );
        assert!(
            !siblings_with_tmp_suffix(&skill_dir.join("scripts")),
            "run.sh publish must leave no staging sibling in scripts/"
        );
    }

    /// A re-emit over an existing skill folder must still land in one step.
    #[test]
    fn atomic_write_overwrites_without_leaving_a_staging_sibling() {
        let temp = tempfile::tempdir().expect("tempdir");
        let skill_md_path = temp.path().join("SKILL.md");

        atomic_write(&skill_md_path, b"first").expect("first write");
        atomic_write(&skill_md_path, b"second").expect("re-emit");

        assert_eq!(
            std::fs::read_to_string(&skill_md_path).expect("SKILL.md readable"),
            "second"
        );
        assert!(
            !siblings_with_tmp_suffix(temp.path()),
            "re-emit must leave no staging sibling"
        );
    }

    fn siblings_with_tmp_suffix(directory: &std::path::Path) -> bool {
        std::fs::read_dir(directory)
            .expect("directory listing")
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
    }
}
