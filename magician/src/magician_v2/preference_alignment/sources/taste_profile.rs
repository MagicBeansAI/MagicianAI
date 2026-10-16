//! The owner-edited taste profile note as intentions.
//!
//! Every non-empty directive line of the profile is one taste intention.
//! Headings give the section a line sits under (carried as its conditions);
//! blank lines, headings, code fences and thematic breaks are structure, not
//! directives.
//!
//! # Provenance
//!
//! The note has no per-line authorship record. A line is `confirmed` only when
//! it matches a directive whose latest approved decision in this scope's taste
//! proposal store is an addition — the owner approved exactly that text.
//! Every other line is `unknown_provenance`: the owner probably wrote it, but
//! nothing proves who did, so it may only advise (P0 contracts §4).
//!
//! # Reading
//!
//! [`TasteProfileLoader::load`] answers `None` both for "no profile" and for
//! "the provider could not be read", which is right for prompt assembly and
//! wrong here (P0 gap D7): an unreadable profile must never look like an
//! empty one. [`read_taste_profile_checked`] makes the same provider reads in
//! the same order through the loader's own settings and Notes boundary, but
//! keeps the failure. A timeout or failure is coverage `unavailable` with
//! source `taste_profile`; only a successful read with no note is absent.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::magician_v2::taste_capture::{
    normalize_directive, ProposalKind, ProposalStatus, TasteCaptureService, TasteProposal,
};
use crate::magician_v2::taste_profile::TasteProfileLoader;

use super::super::intention::{
    AssertionStatus, Coverage, CoverageStatus, EvidenceRef, EvidenceRole, Intention, IntentionKind,
    IntentionLifecycle, IntentionSource, INTENTION_SCHEMA_VERSION,
};
use super::SourceCollection;

/// Coverage name for the profile note.
pub const TASTE_PROFILE_SOURCE: &str = "taste_profile";
/// Coverage name for the proposal store that proves a line was approved.
pub const TASTE_PROPOSALS_SOURCE: &str = "taste_proposals";

/// A reasonable bound for an admission-path read. Prompt assembly uses a
/// 5–100 ms budget because a missing profile there costs nothing; here a
/// missing read makes the whole inventory unavailable, so the bound is wider.
/// Callers pass their own; this is the suggested default.
pub const DEFAULT_TASTE_PROFILE_ADMISSION_TIMEOUT: Duration = Duration::from_millis(1_000);

/// The profile note as read: content with surrounding whitespace trimmed, and
/// the provider's BLAKE3 content hash as its version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TasteProfileRead {
    pub content: String,
    pub version: String,
}

/// What the taste profile contributed, plus the version for
/// `StandingGeneration::profile_version`.
#[derive(Debug, Clone, PartialEq)]
pub struct TasteProfileCollection {
    pub collection: SourceCollection,
    /// `None` when there is no profile, or when it could not be read (the
    /// coverage then says so).
    pub profile_version: Option<String>,
}

impl TasteProfileCollection {
    fn absent() -> Self {
        Self {
            collection: SourceCollection::empty_complete(),
            profile_version: None,
        }
    }

    fn unavailable() -> Self {
        Self {
            collection: SourceCollection::unavailable(TASTE_PROFILE_SOURCE),
            profile_version: None,
        }
    }
}

/// Directives the owner approved into the profile, by normalized text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApprovedDirectives {
    /// Normalized directive → proposal id of its latest approved addition.
    by_directive: BTreeMap<String, String>,
}

impl ApprovedDirectives {
    /// Keep a directive only when its latest approved decision is an
    /// addition: an approved retraction after it means the owner took it out,
    /// and a line that came back since is not the text they approved.
    pub fn from_proposals(proposals: &[TasteProposal]) -> Self {
        let mut latest: BTreeMap<String, (DateTime<Utc>, ProposalKind, String)> = BTreeMap::new();
        for proposal in proposals
            .iter()
            .filter(|proposal| proposal.status == ProposalStatus::Approved)
        {
            let at = proposal.decided_at.unwrap_or(proposal.proposed_at);
            let key = normalize_directive(&proposal.directive);
            let newer = latest
                .get(&key)
                .is_none_or(|(previous, _, _)| at >= *previous);
            if newer {
                latest.insert(key, (at, proposal.kind, proposal.id.clone()));
            }
        }
        Self {
            by_directive: latest
                .into_iter()
                .filter(|(_, (_, kind, _))| *kind == ProposalKind::Add)
                .map(|(directive, (_, _, id))| (directive, id))
                .collect(),
        }
    }

    fn approval_for(&self, directive: &str) -> Option<&str> {
        self.by_directive
            .get(&normalize_directive(directive))
            .map(String::as_str)
    }
}

/// Read the profile through the loader's settings and Notes boundary,
/// keeping failures. Mirrors `TasteProfileLoader::load`: disabled loads
/// nothing, providers are tried default-first, the first note found is the
/// profile, and a blank note is no profile. A provider error with no note
/// found anywhere is an error, not absence.
pub async fn read_taste_profile_checked(
    loader: &TasteProfileLoader,
    principal: &str,
    workspace: &str,
) -> std::io::Result<Option<TasteProfileRead>> {
    let settings = loader.settings();
    if !settings.enabled {
        return Ok(None);
    }
    let note_path = settings.note_path.trim();
    if note_path.is_empty() {
        return Ok(None);
    }
    let store = loader.notes_store();
    let envelope = store.load_envelope(principal, workspace).await?;
    if !envelope.settings.enabled {
        return Ok(None);
    }
    let catalog = store.observation_catalog(principal, workspace).await?;
    let mut providers = catalog.providers;
    providers.sort_by_key(|provider| provider != &envelope.resolved.default_provider);
    let mut first_error = None;
    for provider in providers {
        let source_ref = format!("notes:{provider}:{note_path}");
        match store
            .read_observation_note(principal, workspace, &source_ref)
            .await
        {
            Ok(Some(note)) => {
                let content = note.markdown.trim().to_string();
                if content.is_empty() {
                    return Ok(None);
                }
                return Ok(Some(TasteProfileRead {
                    content,
                    version: note.content_hash,
                }));
            },
            Ok(None) => {},
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            },
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(None),
    }
}

/// Load the taste profile intentions for one scope. No loader installed
/// (tests, tools, processes that serve no prompts) means no profile can be in
/// effect: absent and complete. No capture service means no approval can be
/// proven: every line is `unknown_provenance`.
pub async fn load_taste_profile_intentions(
    loader: Option<&TasteProfileLoader>,
    capture: Option<&TasteCaptureService>,
    principal: &str,
    workspace: &str,
    timeout: Duration,
) -> TasteProfileCollection {
    let Some(loader) = loader else {
        return TasteProfileCollection::absent();
    };
    let load = read_taste_profile_checked(loader, principal, workspace);
    let proposals = async move {
        match capture {
            None => Ok(None),
            Some(capture) => capture
                .store_for(principal, workspace)
                .await
                .read()
                .await
                .map(|read| Some(read.proposals)),
        }
    };
    collect_taste_profile(load, proposals, timeout).await
}

/// Run both reads inside one deadline and project the result.
///
/// - profile read timed out or failed → `unavailable` (`taste_profile`);
/// - no profile → absent, complete;
/// - proposal read timed out or failed → lines are `unknown_provenance` and
///   coverage is `partial` (`taste_proposals`), since some may have been
///   approved.
pub async fn collect_taste_profile<L, P>(
    load: L,
    proposals: P,
    timeout: Duration,
) -> TasteProfileCollection
where
    L: Future<Output = std::io::Result<Option<TasteProfileRead>>>,
    P: Future<Output = std::io::Result<Option<Vec<TasteProposal>>>>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    let read = match tokio::time::timeout_at(deadline, load).await {
        Ok(Ok(Some(read))) => read,
        Ok(Ok(None)) => return TasteProfileCollection::absent(),
        Ok(Err(error)) => {
            tracing::warn!(
                error = %error,
                "preference alignment: taste profile could not be read"
            );
            return TasteProfileCollection::unavailable();
        },
        Err(_) => {
            tracing::warn!(
                timeout_ms = timeout.as_millis() as u64,
                "preference alignment: taste profile read timed out"
            );
            return TasteProfileCollection::unavailable();
        },
    };
    let (approved, coverage) = match tokio::time::timeout_at(deadline, proposals).await {
        Ok(Ok(proposals)) => (
            ApprovedDirectives::from_proposals(&proposals.unwrap_or_default()),
            Coverage::complete(),
        ),
        Ok(Err(_)) | Err(_) => (
            ApprovedDirectives::default(),
            Coverage {
                status: CoverageStatus::Partial,
                omitted_ids: Vec::new(),
                unavailable_sources: vec![TASTE_PROPOSALS_SOURCE.to_string()],
            },
        ),
    };
    TasteProfileCollection {
        collection: SourceCollection {
            intentions: project_taste_profile(&read, &approved),
            coverage,
        },
        profile_version: Some(read.version),
    }
}

/// One intention per directive line. The statement is the line as written;
/// identity is the normalized directive, so whitespace or case edits keep the
/// id while the profile version (the revision) moves.
pub fn project_taste_profile(
    read: &TasteProfileRead,
    approved: &ApprovedDirectives,
) -> Vec<Intention> {
    let mut section: Option<String> = None;
    let mut fenced = false;
    let mut seen = BTreeSet::new();
    let mut intentions = Vec::new();
    for raw in read.content.lines() {
        let line = raw.trim();
        if line.starts_with("```") || line.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if line.is_empty() || is_thematic_break(line) {
            continue;
        }
        // Inside a code fence a `#` line is content, not a heading.
        if let Some(heading) = line.strip_prefix('#').filter(|_| !fenced) {
            let heading = heading.trim_start_matches('#').trim();
            section = (!heading.is_empty()).then(|| heading.to_string());
            continue;
        }
        let directive = strip_list_marker(line);
        if directive.is_empty() {
            continue;
        }
        let id = line_id(directive);
        if !seen.insert(id.clone()) {
            // The same directive twice says nothing new.
            continue;
        }
        let approval = approved.approval_for(directive);
        intentions.push(Intention {
            schema_version: INTENTION_SCHEMA_VERSION.to_string(),
            id,
            revision: read.version.clone(),
            source: IntentionSource::TasteProfile,
            kind: IntentionKind::Taste,
            assertion_status: if approval.is_some() {
                AssertionStatus::Confirmed
            } else {
                AssertionStatus::UnknownProvenance
            },
            statement: line.to_string(),
            conditions: section
                .as_ref()
                .map(|section| format!("profile section: {section}")),
            scope: None,
            lifecycle: IntentionLifecycle::Active,
            valid_until: None,
            evidence_refs: approval
                .map(|proposal_id| {
                    vec![EvidenceRef {
                        source_id: format!("taste_proposal:{proposal_id}"),
                        source_revision: None,
                        role: EvidenceRole::Owner,
                    }]
                })
                .unwrap_or_default(),
            intervention_policy: None,
        });
    }
    intentions
}

/// Stable id of a profile line: a digest of its normalized directive text.
pub fn line_id(directive: &str) -> String {
    let normalized = normalize_directive(strip_list_marker(directive.trim()));
    format!(
        "taste:{}",
        &blake3::hash(normalized.as_bytes()).to_hex()[..32]
    )
}

/// Drop one leading list marker (`-`, `*`, `+`, `1.`, `1)`) and a task
/// checkbox, so `- Never use em-dashes` and `* never use em-dashes` are the
/// same directive. The approval path writes `- <directive>`.
fn strip_list_marker(line: &str) -> &str {
    let line = line.trim();
    let rest = if let Some(rest) = ["- ", "* ", "+ "]
        .iter()
        .find_map(|marker| line.strip_prefix(marker))
    {
        rest
    } else {
        let digits = line.chars().take_while(char::is_ascii_digit).count();
        let after_digits = &line[digits..];
        match after_digits
            .strip_prefix(". ")
            .or_else(|| after_digits.strip_prefix(") "))
        {
            Some(rest) if digits > 0 => rest,
            _ => line,
        }
    };
    let rest = rest.trim_start();
    ["[ ] ", "[x] ", "[X] "]
        .iter()
        .find_map(|checkbox| rest.strip_prefix(checkbox))
        .unwrap_or(rest)
        .trim()
}

fn is_thematic_break(line: &str) -> bool {
    let compact: String = line.chars().filter(|ch| !ch.is_whitespace()).collect();
    compact.len() >= 3
        && ['-', '*', '_']
            .iter()
            .any(|marker| compact.chars().all(|ch| ch == *marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(content: &str) -> TasteProfileRead {
        TasteProfileRead {
            content: content.to_string(),
            version: "v-1".to_string(),
        }
    }

    fn proposal(
        directive: &str,
        kind: ProposalKind,
        status: ProposalStatus,
        day: u32,
    ) -> TasteProposal {
        let at = DateTime::parse_from_rfc3339(&format!("2026-10-{day:02}T00:00:00Z"))
            .unwrap()
            .with_timezone(&Utc);
        TasteProposal {
            id: format!("p-{day}"),
            kind,
            directive: directive.to_string(),
            evidence: Vec::new(),
            destination: "Voice".to_string(),
            confidence: 0.9,
            status,
            source_session: "s".to_string(),
            proposed_at: at,
            decided_at: Some(at),
        }
    }

    #[test]
    fn every_directive_line_is_a_taste_with_its_section() {
        let profile = read(
            "## Voice\n- No unprompted speech.\n\n---\n## Process\nMeasure, don't eyeball.\n- No unprompted speech.\n```\n# not a heading\n```",
        );
        let intentions = project_taste_profile(&profile, &ApprovedDirectives::default());
        let statements: Vec<&str> = intentions
            .iter()
            .map(|intention| intention.statement.as_str())
            .collect();
        assert_eq!(
            statements,
            vec![
                "- No unprompted speech.",
                "Measure, don't eyeball.",
                "# not a heading"
            ]
        );
        assert_eq!(
            intentions[0].conditions.as_deref(),
            Some("profile section: Voice")
        );
        assert_eq!(
            intentions[1].conditions.as_deref(),
            Some("profile section: Process")
        );
        for intention in &intentions {
            assert_eq!(intention.kind, IntentionKind::Taste);
            assert_eq!(intention.source, IntentionSource::TasteProfile);
            assert_eq!(intention.revision, "v-1");
            assert_eq!(
                intention.assertion_status,
                AssertionStatus::UnknownProvenance
            );
        }
    }

    #[test]
    fn only_lines_matching_a_live_approved_addition_are_confirmed() {
        let approved = ApprovedDirectives::from_proposals(&[
            proposal(
                "Never use em-dashes",
                ProposalKind::Add,
                ProposalStatus::Approved,
                1,
            ),
            proposal(
                "Write in British English",
                ProposalKind::Add,
                ProposalStatus::Approved,
                1,
            ),
            proposal(
                "Write in British English",
                ProposalKind::Retract,
                ProposalStatus::Approved,
                3,
            ),
            proposal(
                "Prefer bullet lists",
                ProposalKind::Add,
                ProposalStatus::Pending,
                2,
            ),
            proposal(
                "Avoid jargon",
                ProposalKind::Add,
                ProposalStatus::Rejected,
                2,
            ),
        ]);
        let intentions = project_taste_profile(
            &read("- never use  EM-dashes\n- Write in British English\n- Prefer bullet lists\n- Avoid jargon"),
            &approved,
        );
        let status = |needle: &str| {
            intentions
                .iter()
                .find(|intention| intention.statement.contains(needle))
                .map(|intention| intention.assertion_status)
                .unwrap()
        };
        assert_eq!(status("EM-dashes"), AssertionStatus::Confirmed);
        assert_eq!(status("British"), AssertionStatus::UnknownProvenance);
        assert_eq!(status("bullet"), AssertionStatus::UnknownProvenance);
        assert_eq!(status("jargon"), AssertionStatus::UnknownProvenance);
        let confirmed = intentions
            .iter()
            .find(|intention| intention.statement.contains("EM-dashes"))
            .unwrap();
        assert_eq!(confirmed.evidence_refs[0].role, EvidenceRole::Owner);
    }

    #[test]
    fn line_ids_ignore_markers_case_and_spacing_but_not_negation() {
        assert_eq!(
            line_id("- Never use em-dashes"),
            line_id("* never   use em-dashes")
        );
        assert_ne!(line_id("Never use em-dashes"), line_id("Use em-dashes"));
        assert_eq!(line_id("1. Be brief"), line_id("- [x] be brief"));
    }

    #[tokio::test]
    async fn a_timed_out_profile_read_is_unavailable_not_empty() {
        let collected = collect_taste_profile(
            async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(Some(read("- Be brief")))
            },
            async { Ok(None) },
            Duration::from_millis(20),
        )
        .await;
        assert!(collected.collection.intentions.is_empty());
        assert_eq!(
            collected.collection.coverage.status,
            CoverageStatus::Unavailable
        );
        assert_eq!(
            collected.collection.coverage.unavailable_sources,
            vec![TASTE_PROFILE_SOURCE.to_string()]
        );
        assert_eq!(collected.profile_version, None);
    }

    #[tokio::test]
    async fn a_failed_profile_read_is_unavailable_and_an_absent_one_is_complete() {
        let failed = collect_taste_profile(
            async { Err(std::io::Error::other("provider down")) },
            async { Ok(None) },
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(
            failed.collection.coverage.status,
            CoverageStatus::Unavailable
        );

        let absent = collect_taste_profile(
            async { Ok(None) },
            async { Ok(None) },
            Duration::from_secs(1),
        )
        .await;
        assert!(absent.collection.intentions.is_empty());
        assert_eq!(absent.collection.coverage.status, CoverageStatus::Complete);
        assert_eq!(absent.profile_version, None);
    }

    #[tokio::test]
    async fn an_unreadable_proposal_store_lowers_authority_and_marks_partial() {
        let collected = collect_taste_profile(
            async { Ok(Some(read("- Never use em-dashes"))) },
            async { Err(std::io::Error::other("store unreadable")) },
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(collected.profile_version.as_deref(), Some("v-1"));
        assert_eq!(collected.collection.intentions.len(), 1);
        assert_eq!(
            collected.collection.intentions[0].assertion_status,
            AssertionStatus::UnknownProvenance
        );
        assert_eq!(
            collected.collection.coverage.status,
            CoverageStatus::Partial
        );
        assert_eq!(
            collected.collection.coverage.unavailable_sources,
            vec![TASTE_PROPOSALS_SOURCE.to_string()]
        );
    }
}
