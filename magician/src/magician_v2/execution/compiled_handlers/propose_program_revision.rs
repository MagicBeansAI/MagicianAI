//! `propose_program_revision` — an agent proposes a full revised version of an
//! existing program document; the owner approves or rejects it.
//!
//! Living shared documents: program docs (an agent's own program, or a shared
//! document such as positioning, facts or policies listed in
//! `harness.shared_program_docs`) reach every cycle with their SHA-256, so an
//! agent that learns something can propose the document's next version
//! against exactly the version it read.
//!
//! The approval gate is NOT in this handler. An agent definition grants the
//! tool and adds a `requires_approval` rule for it, so the call pauses for the
//! owner's decision and this handler runs only after an approval. It then
//! publishes through `ProgramDocEditor::save_document` with the proposal's base
//! hash: the prior document is snapshotted to `programs/.history/` first (the
//! owner's revert point), and a document that changed after the agent read it
//! is refused with a typed conflict instead of being overwritten.
//!
//! Guards live in the tool contract: an existing program only (never creates a
//! file), a full non-blank document within `PROGRAM_DOC_MAX_BYTES`, a
//! non-blank rationale for the approval card, a hex base hash, and at most
//! `MAX_EVIDENCE` evidence entries.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{json, Value};
use tracing::info;

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::approval_preview::{
    line_diff_preview, truncate_preview_text, ApprovalPreview, ApprovalPreviewContext,
    ApprovalPreviewWarning,
};
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::harness::program_doc::{
    read_program_doc, read_program_doc_blocking, ProgramDoc, ProgramDocEditor, ProgramSaveConflict,
    PROGRAM_DOC_MAX_BYTES,
};

use super::shared::require_scope_str;

const TOOL: &str = "propose_program_revision";
const MAX_EVIDENCE: usize = 10;
const MAX_EVIDENCE_ENTRY_BYTES: usize = 1024;
/// A base may be the full hex SHA-256 or a prefix of at least this many hex
/// characters — the shared-context header prints the first 12.
const MIN_BASE_HEX: usize = 12;

#[derive(Debug, Deserialize)]
struct ReviseArgs {
    program: String,
    content: String,
    base_sha256: String,
    rationale: String,
    #[serde(default)]
    evidence: Vec<String>,
}

fn invalid(msg: impl Into<String>) -> ExecutionError {
    ExecutionError::Step(format!("{TOOL}: {}", msg.into()))
}

/// Normalized (trimmed, lowercase) base hash, or why it is unusable.
fn normalize_base(base: &str) -> Result<String, String> {
    let base = base.trim().to_ascii_lowercase();
    if base.len() < MIN_BASE_HEX || base.len() > 64 {
        return Err(format!(
            "base_sha256 must be the document's sha256 as shown in the cycle context \
             ({MIN_BASE_HEX} to 64 hex characters, got {})",
            base.len()
        ));
    }
    if !base.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("base_sha256 must be hexadecimal".to_string());
    }
    Ok(base)
}

/// Contract validation — pure, unit-tested below. Returns the normalized base.
fn validate(args: &ReviseArgs) -> Result<String, String> {
    if args.program.trim().is_empty() {
        return Err("program is required".to_string());
    }
    if args.rationale.trim().is_empty() {
        return Err("rationale is required (it is shown on the owner's approval card)".to_string());
    }
    if args.content.trim().is_empty() {
        return Err("content must be the full revised document, not empty".to_string());
    }
    if args.content.len() > PROGRAM_DOC_MAX_BYTES {
        return Err(format!(
            "content exceeds {PROGRAM_DOC_MAX_BYTES} bytes ({})",
            args.content.len()
        ));
    }
    if args.evidence.len() > MAX_EVIDENCE {
        return Err(format!(
            "evidence holds at most {MAX_EVIDENCE} entries (got {})",
            args.evidence.len()
        ));
    }
    if let Some(index) = args
        .evidence
        .iter()
        .position(|entry| entry.trim().is_empty() || entry.len() > MAX_EVIDENCE_ENTRY_BYTES)
    {
        return Err(format!(
            "evidence entry {} must be a non-blank link or note of at most \
             {MAX_EVIDENCE_ENTRY_BYTES} bytes",
            index + 1
        ));
    }
    normalize_base(&args.base_sha256)
}

/// Most rationale text an approval card carries.
const MAX_PREVIEW_RATIONALE_BYTES: usize = 4 * 1024;
const STALE_BASE_MESSAGE: &str = "This document changed after the agent read it — approving will \
                                  be refused; reject and let the agent re-propose.";

/// The approval card for a proposed revision (registered in
/// `execution::approval_preview`). Reads the current document and diffs the
/// proposal against it; deterministic, no model call.
pub fn approval_preview(ctx: &ApprovalPreviewContext<'_>, args: &Value) -> Option<ApprovalPreview> {
    let text = |key: &str| args.get(key).and_then(Value::as_str).unwrap_or("");
    let program = text("program").trim();
    if program.is_empty() {
        return None;
    }
    let evidence: Vec<String> = args
        .get("evidence")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let current = match (ctx.principal, ctx.workspace) {
        (Some(principal), Some(workspace)) => {
            read_program_doc_blocking(ctx.artifact_workspace, principal, workspace, program)
                .map_err(|err| err.to_string())
        },
        _ => Err("the call carries no scope".to_string()),
    };
    Some(build_revision_preview(&RevisionPreviewInput {
        agent_label: ctx.agent_label(),
        program,
        current,
        content: text("content"),
        base_sha256: text("base_sha256"),
        rationale: text("rationale"),
        evidence,
    }))
}

struct RevisionPreviewInput<'a> {
    agent_label: String,
    program: &'a str,
    /// The document as it is now: `Ok(None)` = does not exist, `Err` = the
    /// name or scope is unusable.
    current: Result<Option<ProgramDoc>, String>,
    content: &'a str,
    base_sha256: &'a str,
    rationale: &'a str,
    evidence: Vec<String>,
}

/// Pure preview assembly — unit-tested below.
fn build_revision_preview(input: &RevisionPreviewInput<'_>) -> ApprovalPreview {
    let rationale = input.rationale.trim();
    let mut preview = ApprovalPreview {
        title: format!("{} wants to change `{}`", input.agent_label, input.program),
        why_label: (!rationale.is_empty()).then(|| "Why (agent's words)".to_string()),
        why: (!rationale.is_empty())
            .then(|| truncate_preview_text(rationale, MAX_PREVIEW_RATIONALE_BYTES)),
        diff: None,
        evidence: input
            .evidence
            .iter()
            .map(|entry| entry.trim())
            .filter(|entry| !entry.is_empty())
            .take(MAX_EVIDENCE)
            .map(|entry| truncate_preview_text(entry, MAX_EVIDENCE_ENTRY_BYTES))
            .collect(),
        warnings: Vec::new(),
        notes: Vec::new(),
    };
    let current = match &input.current {
        Ok(Some(current)) => current,
        Ok(None) => {
            preview.warnings.push(ApprovalPreviewWarning::new(
                "missing_target",
                format!(
                    "`{}` does not exist in this scope's programs/ — this tool only revises \
                     existing documents, so approving will be refused.",
                    input.program
                ),
            ));
            return preview;
        },
        Err(err) => {
            preview.warnings.push(ApprovalPreviewWarning::new(
                "invalid_target",
                format!(
                    "`{}` cannot be read ({err}); approving will be refused.",
                    input.program
                ),
            ));
            return preview;
        },
    };
    match normalize_base(input.base_sha256) {
        Ok(base) if current.sha256.starts_with(&base) => {},
        Ok(_) => preview.warnings.push(ApprovalPreviewWarning::new(
            "stale_base",
            STALE_BASE_MESSAGE,
        )),
        Err(reason) => preview.warnings.push(ApprovalPreviewWarning::new(
            "invalid_base",
            format!("{reason} — approving will be refused."),
        )),
    }
    let diff = line_diff_preview(input.program, &current.content, input.content);
    if diff.added == 0 && diff.removed == 0 {
        preview
            .notes
            .push("The proposed text is identical to the current document.".to_string());
    }
    preview.diff = Some(diff);
    preview
}

fn changed_since_proposed(program: &str) -> ExecutionError {
    invalid(format!(
        "program {program:?} changed after this revision was proposed, so it was NOT applied. \
         Re-read the current document (the next cycle context shows it with its new sha256) \
         and, if the revision still applies, propose it again against that version"
    ))
}

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", TOOL)?;
    let workspace = require_scope_str(&args, "__workspace", TOOL)?;
    let agent_id = require_scope_str(&args, "__agent_id", TOOL)?;

    let parsed: ReviseArgs =
        serde_json::from_value(args).map_err(|err| invalid(format!("bad arguments: {err}")))?;
    let base = validate(&parsed).map_err(invalid)?;
    let program = parsed.program.trim();

    let ws = &resources.artifact_workspace;
    // The target must be a REAL program in this scope (never creates files).
    let current = read_program_doc(ws, &principal, &workspace, program)
        .await
        .map_err(|err| invalid(format!("invalid program: {err}")))?
        .ok_or_else(|| {
            invalid(format!(
                "program {program:?} does not exist in this scope's programs/"
            ))
        })?;
    // A short base resolves against the current document only; the editor
    // then re-checks the full hash at publish time, so a change in between is
    // still refused.
    if !current.sha256.starts_with(&base) {
        return Err(changed_since_proposed(program));
    }

    let editor = ProgramDocEditor::new(ws.clone());
    let saved = editor
        .save_document(
            &principal,
            &workspace,
            program,
            &parsed.content,
            Some(&current.sha256),
        )
        .await
        .map_err(|err| {
            if err.downcast_ref::<ProgramSaveConflict>().is_some() {
                changed_since_proposed(program)
            } else {
                ExecutionError::Step(format!("{TOOL} apply failed: {err}"))
            }
        })?;

    info!(
        agent_id = %agent_id,
        principal = %principal,
        workspace = %workspace,
        program = %saved.program,
        unchanged = saved.unchanged,
        evidence = parsed.evidence.len(),
        "[propose_program_revision] approved program revision applied"
    );
    Ok(json!({
        "applied": true,
        "program": saved.program,
        "history_snapshot": saved.history_snapshot,
        "sha256": saved.sha256,
        "unchanged": saved.unchanged,
    }))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn args() -> ReviseArgs {
        ReviseArgs {
            program: "positioning.md".into(),
            content: "# Positioning\n\nCalm, exact, owner-first.\n".into(),
            base_sha256: "AbCdEf0123456789".into(),
            rationale: "three replies this week contradicted the old line".into(),
            evidence: vec!["task t-1".into()],
        }
    }

    #[test]
    fn accepts_a_complete_proposal_and_normalizes_the_base() {
        assert_eq!(validate(&args()).as_deref(), Ok("abcdef0123456789"));
        let mut full = args();
        full.base_sha256 = "a".repeat(64);
        assert!(validate(&full).is_ok());
        let mut no_evidence = args();
        no_evidence.evidence.clear();
        assert!(validate(&no_evidence).is_ok());
    }

    #[test]
    fn rejects_blank_rationale_content_or_program() {
        let mut blank = args();
        blank.rationale = "  ".into();
        assert!(validate(&blank).unwrap_err().contains("rationale"));

        let mut empty = args();
        empty.content = "\n\n".into();
        assert!(validate(&empty).unwrap_err().contains("content"));

        let mut nameless = args();
        nameless.program = String::new();
        assert!(validate(&nameless).unwrap_err().contains("program"));
    }

    #[test]
    fn rejects_oversized_content() {
        let mut huge = args();
        huge.content = "x".repeat(PROGRAM_DOC_MAX_BYTES + 1);
        assert!(validate(&huge).unwrap_err().contains("exceeds"));
    }

    #[test]
    fn rejects_short_long_or_non_hex_bases() {
        let too_long = "a".repeat(65);
        for base in ["abc", "", too_long.as_str(), "zzzzzzzzzzzzzzzz"] {
            let mut bad = args();
            bad.base_sha256 = base.to_string();
            assert!(validate(&bad).is_err(), "base {base:?} must be refused");
        }
    }

    #[test]
    fn caps_evidence_count_and_entries() {
        let mut many = args();
        many.evidence = (0..=MAX_EVIDENCE).map(|i| format!("note {i}")).collect();
        assert!(validate(&many).unwrap_err().contains("at most"));

        let mut blank_entry = args();
        blank_entry.evidence = vec!["ok".into(), " ".into()];
        assert!(validate(&blank_entry).unwrap_err().contains("entry 2"));

        let mut long_entry = args();
        long_entry.evidence = vec!["y".repeat(MAX_EVIDENCE_ENTRY_BYTES + 1)];
        assert!(validate(&long_entry).is_err());
    }

    fn doc(content: &str) -> ProgramDoc {
        ProgramDoc {
            name: "positioning.md".into(),
            title: "Positioning".into(),
            content: content.into(),
            missions_section: None,
            sha256: crate::magician_v2::harness::program_doc::content_sha256(content),
        }
    }

    fn preview_input<'a>(
        current: Result<Option<ProgramDoc>, String>,
        content: &'a str,
        base: &'a str,
    ) -> RevisionPreviewInput<'a> {
        RevisionPreviewInput {
            agent_label: "Mira".into(),
            program: "positioning.md",
            current,
            content,
            base_sha256: base,
            rationale: "  three replies contradicted the old line <b>really</b>  ",
            evidence: vec!["task t-1".into(), "  ".into()],
        }
    }

    #[test]
    fn preview_shows_title_rationale_evidence_and_the_changed_lines() {
        let old = "# Positioning\n\nCalm.\nExact.\nOwner-first.\n";
        let new = "# Positioning\n\nCalm.\nExact and honest.\nOwner-first.\n";
        let current = doc(old);
        let base = current.sha256[..12].to_string();
        let preview = build_revision_preview(&preview_input(Ok(Some(current)), new, &base));
        assert_eq!(preview.title, "Mira wants to change `positioning.md`");
        assert_eq!(preview.why_label.as_deref(), Some("Why (agent's words)"));
        // Agent text is carried verbatim (trimmed) — the renderer escapes it.
        assert_eq!(
            preview.why.as_deref(),
            Some("three replies contradicted the old line <b>really</b>")
        );
        assert_eq!(preview.evidence, vec!["task t-1".to_string()]);
        assert!(preview.warnings.is_empty(), "{:?}", preview.warnings);
        let diff = preview.diff.expect("diff");
        assert_eq!((diff.added, diff.removed), (1, 1));
        assert!(diff.unified_diff.contains("-Exact.\n+Exact and honest.\n"));
        // Two lines of context, not the whole document.
        assert!(!diff.unified_diff.contains("# Positioning"));
    }

    #[test]
    fn preview_warns_when_the_document_changed_after_the_agent_read_it() {
        let current = doc("# Positioning\n\nNew owner text.\n");
        let stale = crate::magician_v2::harness::program_doc::content_sha256("older text");
        let preview = build_revision_preview(&preview_input(
            Ok(Some(current)),
            "# Positioning\n\nAgent text.\n",
            &stale,
        ));
        assert_eq!(preview.warnings.len(), 1);
        assert_eq!(preview.warnings[0].code, "stale_base");
        assert!(preview.warnings[0]
            .message
            .contains("changed after the agent read it"));
        // The diff is still shown, against the CURRENT document.
        assert!(preview.diff.is_some());
    }

    #[test]
    fn preview_warns_about_an_unusable_base() {
        let current = doc("x\n");
        let preview = build_revision_preview(&preview_input(Ok(Some(current)), "y\n", "abc"));
        assert_eq!(preview.warnings[0].code, "invalid_base");
    }

    #[test]
    fn preview_says_when_the_document_does_not_exist() {
        let preview = build_revision_preview(&preview_input(Ok(None), "# New\n", "abcdef012345"));
        assert!(preview.diff.is_none());
        assert_eq!(preview.warnings[0].code, "missing_target");
        assert!(preview.warnings[0].message.contains("does not exist"));

        let preview = build_revision_preview(&preview_input(
            Err("invalid program name".into()),
            "# New\n",
            "abcdef012345",
        ));
        assert_eq!(preview.warnings[0].code, "invalid_target");
    }

    #[test]
    fn preview_notes_an_identical_proposal_and_handles_multi_byte_text() {
        let text = "# Positionierung\n\nRuhig — für Eigentümer 🧭.\n";
        let current = doc(text);
        let base = current.sha256.clone();
        let preview = build_revision_preview(&preview_input(Ok(Some(current)), text, &base));
        assert_eq!(
            preview.diff.as_ref().map(|d| (d.added, d.removed)),
            Some((0, 0))
        );
        assert_eq!(preview.notes.len(), 1);

        let current = doc(text);
        let base = current.sha256.clone();
        let edited = "# Positionierung\n\nRuhig — für Eigentümer 🧭 und Kunden.\n";
        let preview = build_revision_preview(&preview_input(Ok(Some(current)), edited, &base));
        let diff = preview.diff.expect("diff");
        assert!(diff
            .unified_diff
            .contains("+Ruhig — für Eigentümer 🧭 und Kunden."));
    }

    #[test]
    fn registered_builder_reads_the_document_from_the_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace =
            crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(tmp.path());
        let root = workspace.programs_root("alpha", "default");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("positioning.md"), "# Positioning\n\nCalm.\n").unwrap();
        let sha =
            crate::magician_v2::harness::program_doc::content_sha256("# Positioning\n\nCalm.\n");
        let ctx = ApprovalPreviewContext {
            artifact_workspace: &workspace,
            principal: Some("alpha"),
            workspace: Some("default"),
            agent_id: Some("mira"),
            agent_display_name: Some("Mira"),
        };
        let mut params = std::collections::HashMap::new();
        params.insert("program".to_string(), json!("positioning.md"));
        params.insert(
            "content".to_string(),
            json!("# Positioning\n\nCalm, exact.\n"),
        );
        params.insert("base_sha256".to_string(), json!(sha[..12]));
        params.insert("rationale".to_string(), json!("tighter"));
        params.insert("evidence".to_string(), json!(["task t-9"]));
        let action = crate::magician_v2::execution::actions::ExecutableAction::Pack {
            capability_name: TOOL.to_string(),
            implementation:
                crate::magician_v2::execution::capability::ImplementationType::Compiled {
                    provider_name: TOOL.to_string(),
                },
            resolved_params: params,
        };
        let preview = crate::magician_v2::execution::approval_preview::approval_preview_for_action(
            &ctx, &action,
        )
        .expect("registered tool has a preview");
        assert_eq!(preview.title, "Mira wants to change `positioning.md`");
        assert!(preview.warnings.is_empty(), "{:?}", preview.warnings);
        assert_eq!(
            preview.diff.as_ref().map(|d| (d.added, d.removed)),
            Some((1, 1))
        );
        assert_eq!(preview.evidence, vec!["task t-9".to_string()]);
    }
}
