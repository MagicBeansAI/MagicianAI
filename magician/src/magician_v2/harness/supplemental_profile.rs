//! Boundary D: versioned supplemental operating guidance for a harness program.
//!
//! The thing an evidence-gated revision is allowed to change. It is
//! deliberately **not** the program document: that doc, the agent's authority,
//! its tool grants, trust policies, schedules and approval rules are all
//! immutable to this path. What a harness may propose about itself is
//! operating *guidance* — how it works, not what it is permitted to do.
//!
//! Three properties make that safe rather than merely intended:
//!
//! * **An allowlist of sections, not a denylist of fields.** A revision can
//!   only address [`ALLOWED_SECTIONS`]; anything else — `persona`, `tools`,
//!   `trust_level`, `approval_rules`, `schedule`, `program` — is refused by
//!   construction rather than by remembering to forbid it. A denylist would
//!   need updating every time the harness grew a new authority-bearing field,
//!   and would be wrong the first time someone forgot.
//! * **Applying requires evidence and an approval.** [`apply`] takes a
//!   validated [`ProfileRevisionProposal`] carrying its episode evidence, its
//!   evaluation id, and an explicit owner approval. There is no path that
//!   mutates the profile from a reflection alone, which is the acceptance
//!   criterion "a reflection cannot directly change live harness behaviour".
//! * **Revert restores the exact prior text, and deletes nothing.** Every
//!   revision keeps the `before` material that produced it, so a revert is a
//!   restoration rather than a reconstruction, and it appends a new history
//!   entry rather than erasing the one it undid. Failed evaluations, rejected
//!   approvals and reverted revisions all stay auditable.
//!
//! The profile is read into the harness cycle prompt beside the program doc.
//! A versioned artifact nothing reads is decoration; the reader is what makes
//! this boundary real.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const SUPPLEMENTAL_PROFILE_SCHEMA_VERSION: u32 = 1;

/// The only sections a profile revision may address.
///
/// Each is operating guidance. None of them can widen what the agent is
/// permitted to do: a procedure reference points at an already-approved
/// procedure, a skill reference at an already-granted skill, and role guidance
/// shapes how a subagent is briefed rather than which subagents exist.
pub const ALLOWED_SECTIONS: &[&str] = &[
    "operating_notes",
    "approved_procedures",
    "skill_references",
    "subagent_role_guidance",
];

/// Field names this path must never be able to touch. Kept as an explicit
/// list purely so the refusal is *tested by name* — the allowlist above is
/// what actually enforces it.
pub const NEVER_REVISABLE: &[&str] = &[
    "persona",
    "tools",
    "tool_grants",
    "trust_level",
    "trust_policies",
    "approval_rules",
    "schedule",
    "program",
    "authority",
    "principal",
    "workspace",
];

const MAX_SECTION_CHARS: usize = 8_000;
const MAX_REASON_CHARS: usize = 1_000;
const MAX_HISTORY_ENTRIES: usize = 200;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProfileRevisionError {
    #[error("section `{0}` is not revisable; a profile revision may only address {1}")]
    SectionNotAllowed(String, String),
    #[error("section `{0}` exceeds the {MAX_SECTION_CHARS}-character bound")]
    SectionTooLong(String),
    #[error("a profile revision requires {0}")]
    MissingEvidence(&'static str),
    #[error("the proposal's `before` for section `{0}` does not match the live profile; it was written against a stale revision")]
    StaleBefore(String),
    #[error("revision {0} is not in this profile's history")]
    UnknownRevision(u32),
    #[error("revision {0} was already reverted")]
    AlreadyReverted(u32),
}

/// What one accepted revision changed, kept whole so a revert is a
/// restoration rather than a reconstruction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProfileRevisionRecord {
    pub revision: u32,
    pub applied_at: DateTime<Utc>,
    pub candidate_id: String,
    pub evaluation_id: String,
    pub approved_by: String,
    pub reason: String,
    /// Exact prior text per touched section. `None` means the section did not
    /// exist before, and reverting removes it again.
    pub before: BTreeMap<String, Option<String>>,
    pub after: BTreeMap<String, Option<String>>,
    /// Set when an owner reverted this revision. The record is never deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverted_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverted_by: Option<String>,
}

/// The durable artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HarnessSupplementalProfile {
    pub schema_version: u32,
    pub principal: String,
    pub workspace: String,
    /// The program this guidance supplements. A profile is meaningless
    /// without it — guidance for one program must never be read into another.
    pub program_relative_path: String,
    pub revision: u32,
    pub sections: BTreeMap<String, String>,
    pub updated_at: DateTime<Utc>,
    pub history: Vec<ProfileRevisionRecord>,
}

impl HarnessSupplementalProfile {
    pub fn empty(principal: &str, workspace: &str, program_relative_path: &str) -> Self {
        Self {
            schema_version: SUPPLEMENTAL_PROFILE_SCHEMA_VERSION,
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            program_relative_path: program_relative_path.to_string(),
            revision: 0,
            sections: BTreeMap::new(),
            updated_at: Utc::now(),
            history: Vec::new(),
        }
    }

    /// The prompt block. `None` when there is nothing to say — an empty
    /// heading in a cycle prompt is noise that trains the model to skim.
    pub fn render_block(&self) -> Option<String> {
        if self.sections.values().all(|body| body.trim().is_empty()) {
            return None;
        }
        let mut block = format!("### Supplemental Guidance (revision {})\n", self.revision);
        for section in ALLOWED_SECTIONS {
            let Some(body) = self.sections.get(*section) else {
                continue;
            };
            if body.trim().is_empty() {
                continue;
            }
            block.push_str(&format!("\n**{}**\n{}\n", heading(section), body.trim()));
        }
        Some(block)
    }

    /// Apply a validated, evaluated, approved revision as one transaction.
    pub fn apply(
        &mut self,
        proposal: &ProfileRevisionProposal,
        approved_by: &str,
    ) -> Result<u32, ProfileRevisionError> {
        proposal.validate()?;
        if approved_by.trim().is_empty() {
            return Err(ProfileRevisionError::MissingEvidence("an owner approval"));
        }

        // The proposal states what it believed it was editing. If that no
        // longer matches, it was written against a revision that has since
        // moved and applying it would silently discard the newer text.
        for (section, expected) in &proposal.before {
            let live = self.sections.get(section).map(String::as_str);
            if live != expected.as_deref() {
                return Err(ProfileRevisionError::StaleBefore(section.clone()));
            }
        }

        let mut before = BTreeMap::new();
        let mut after = BTreeMap::new();
        for (section, next) in &proposal.after {
            before.insert(section.clone(), self.sections.get(section).cloned());
            match next {
                Some(body) => {
                    self.sections.insert(section.clone(), body.clone());
                },
                None => {
                    self.sections.remove(section);
                },
            }
            after.insert(section.clone(), next.clone());
        }

        self.revision = self.revision.saturating_add(1);
        self.updated_at = Utc::now();
        self.history.push(ProfileRevisionRecord {
            revision: self.revision,
            applied_at: self.updated_at,
            candidate_id: proposal.candidate_id.clone(),
            evaluation_id: proposal.evaluation_id.clone(),
            approved_by: approved_by.to_string(),
            reason: proposal.expected_benefit.clone(),
            before,
            after,
            reverted_at: None,
            reverted_by: None,
        });
        // Bounded, oldest-first: the newest revisions are the ones a revert
        // can still reach, and an unbounded history would grow a prompt-
        // adjacent artifact without limit.
        if self.history.len() > MAX_HISTORY_ENTRIES {
            let excess = self.history.len() - MAX_HISTORY_ENTRIES;
            self.history.drain(0..excess);
        }
        Ok(self.revision)
    }

    /// Restore the exact text a revision replaced. Appends history; deletes
    /// none, so the reverted revision stays auditable with its evidence.
    pub fn revert(
        &mut self,
        revision: u32,
        reverted_by: &str,
    ) -> Result<u32, ProfileRevisionError> {
        if reverted_by.trim().is_empty() {
            return Err(ProfileRevisionError::MissingEvidence("a reverting owner"));
        }
        let record = self
            .history
            .iter()
            .find(|entry| entry.revision == revision)
            .cloned()
            .ok_or(ProfileRevisionError::UnknownRevision(revision))?;
        if record.reverted_at.is_some() {
            return Err(ProfileRevisionError::AlreadyReverted(revision));
        }

        let mut before = BTreeMap::new();
        let mut after = BTreeMap::new();
        for (section, original) in &record.before {
            before.insert(section.clone(), self.sections.get(section).cloned());
            match original {
                Some(body) => {
                    self.sections.insert(section.clone(), body.clone());
                },
                None => {
                    self.sections.remove(section);
                },
            }
            after.insert(section.clone(), original.clone());
        }

        let now = Utc::now();
        if let Some(entry) = self
            .history
            .iter_mut()
            .find(|entry| entry.revision == revision)
        {
            entry.reverted_at = Some(now);
            entry.reverted_by = Some(reverted_by.to_string());
        }
        self.revision = self.revision.saturating_add(1);
        self.updated_at = now;
        self.history.push(ProfileRevisionRecord {
            revision: self.revision,
            applied_at: now,
            candidate_id: record.candidate_id.clone(),
            evaluation_id: record.evaluation_id.clone(),
            approved_by: reverted_by.to_string(),
            reason: format!("owner revert of revision {revision}"),
            before,
            after,
            reverted_at: None,
            reverted_by: None,
        });
        Ok(self.revision)
    }
}

/// What a `harness_profile_revision` candidate must carry.
///
/// Every field here is required because each answers a question an owner will
/// ask at approval time, and a proposal that cannot answer them is not
/// reviewable — it is a request to trust the model's judgement, which is the
/// thing this boundary exists to avoid.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProfileRevisionProposal {
    pub candidate_id: String,
    /// The episodes that motivated this. Evidence, not narration.
    pub episode_evidence: Vec<String>,
    /// The controlled run that exercised it before approval.
    pub evaluation_id: String,
    /// Exactly what the proposal believes each touched section says today.
    pub before: BTreeMap<String, Option<String>>,
    /// Exactly what it should say instead. `None` removes the section.
    pub after: BTreeMap<String, Option<String>>,
    pub expected_benefit: String,
    pub confidence: String,
    pub risk: String,
    pub rollback_plan: String,
    pub evaluation_case: String,
    pub acceptance_condition: String,
}

impl ProfileRevisionProposal {
    pub fn validate(&self) -> Result<(), ProfileRevisionError> {
        let allowed: BTreeSet<&str> = ALLOWED_SECTIONS.iter().copied().collect();
        if self.after.is_empty() {
            return Err(ProfileRevisionError::MissingEvidence(
                "at least one section to change",
            ));
        }
        for section in self.after.keys().chain(self.before.keys()) {
            if !allowed.contains(section.as_str()) {
                return Err(ProfileRevisionError::SectionNotAllowed(
                    section.clone(),
                    ALLOWED_SECTIONS.join(", "),
                ));
            }
        }
        for (section, body) in &self.after {
            if let Some(body) = body {
                if body.chars().count() > MAX_SECTION_CHARS {
                    return Err(ProfileRevisionError::SectionTooLong(section.clone()));
                }
            }
        }
        for (value, label) in [
            (&self.candidate_id, "a candidate id"),
            (&self.evaluation_id, "an evaluation id"),
            (&self.expected_benefit, "an expected benefit"),
            (&self.confidence, "a confidence"),
            (&self.risk, "a risk"),
            (&self.rollback_plan, "a rollback plan"),
            (&self.evaluation_case, "a targeted evaluation case"),
            (
                &self.acceptance_condition,
                "a measurable acceptance condition",
            ),
        ] {
            if value.trim().is_empty() {
                return Err(ProfileRevisionError::MissingEvidence(label));
            }
        }
        if self.expected_benefit.chars().count() > MAX_REASON_CHARS {
            return Err(ProfileRevisionError::SectionTooLong(
                "expected_benefit".to_string(),
            ));
        }
        if self.episode_evidence.is_empty()
            || self
                .episode_evidence
                .iter()
                .any(|entry| entry.trim().is_empty())
        {
            return Err(ProfileRevisionError::MissingEvidence(
                "at least one episode of evidence",
            ));
        }
        Ok(())
    }
}

fn heading(section: &str) -> &str {
    match section {
        "operating_notes" => "Operating notes",
        "approved_procedures" => "Approved procedures",
        "skill_references" => "Skill references",
        "subagent_role_guidance" => "Subagent role guidance",
        other => other,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn proposal(after: BTreeMap<String, Option<String>>) -> ProfileRevisionProposal {
        ProfileRevisionProposal {
            candidate_id: "cand-1".to_string(),
            episode_evidence: vec!["episode-1".to_string()],
            evaluation_id: "eval-1".to_string(),
            before: BTreeMap::new(),
            after,
            expected_benefit: "fewer repeated no-progress cycles".to_string(),
            confidence: "medium".to_string(),
            risk: "low — guidance only".to_string(),
            rollback_plan: "revert the revision".to_string(),
            evaluation_case: "harness cycle on the stalled lane".to_string(),
            acceptance_condition: "two consecutive cycles settle with a decision".to_string(),
        }
    }

    fn section(name: &str, body: &str) -> BTreeMap<String, Option<String>> {
        BTreeMap::from([(name.to_string(), Some(body.to_string()))])
    }

    /// The immutability guard, by name. These are the fields §4 of the plan
    /// says must stay beyond this path's reach.
    #[test]
    fn authority_bearing_fields_are_refused_by_construction() {
        for forbidden in NEVER_REVISABLE {
            let attempt = proposal(section(forbidden, "anything at all"));
            let error = attempt
                .validate()
                .expect_err("an authority-bearing field must be refused");
            assert!(
                matches!(error, ProfileRevisionError::SectionNotAllowed(ref name, _) if name == forbidden),
                "`{forbidden}` was not refused as a section: {error:?}"
            );
        }
        // And an unknown future field is refused too, because the allowlist is
        // what enforces this rather than the list above.
        assert!(matches!(
            proposal(section("something_invented_later", "x")).validate(),
            Err(ProfileRevisionError::SectionNotAllowed(_, _))
        ));
    }

    #[test]
    fn every_allowed_section_is_actually_accepted() {
        for allowed in ALLOWED_SECTIONS {
            proposal(section(allowed, "guidance"))
                .validate()
                .unwrap_or_else(|error| panic!("`{allowed}` should validate: {error:?}"));
        }
    }

    /// A proposal that cannot be reviewed is not a proposal. Each missing
    /// field is its own refusal so an owner never sees a half-answered one.
    #[test]
    fn a_proposal_without_its_review_material_is_refused() {
        let base = proposal(section("operating_notes", "note"));

        let mut no_eval = base.clone();
        no_eval.evaluation_id = "  ".to_string();
        assert!(matches!(
            no_eval.validate(),
            Err(ProfileRevisionError::MissingEvidence("an evaluation id"))
        ));

        let mut no_rollback = base.clone();
        no_rollback.rollback_plan = String::new();
        assert!(matches!(
            no_rollback.validate(),
            Err(ProfileRevisionError::MissingEvidence("a rollback plan"))
        ));

        let mut no_acceptance = base.clone();
        no_acceptance.acceptance_condition = String::new();
        assert!(matches!(
            no_acceptance.validate(),
            Err(ProfileRevisionError::MissingEvidence(
                "a measurable acceptance condition"
            ))
        ));

        let mut no_evidence = base.clone();
        no_evidence.episode_evidence = Vec::new();
        assert!(matches!(
            no_evidence.validate(),
            Err(ProfileRevisionError::MissingEvidence(
                "at least one episode of evidence"
            ))
        ));

        let mut nothing_to_do = base.clone();
        nothing_to_do.after = BTreeMap::new();
        assert!(matches!(
            nothing_to_do.validate(),
            Err(ProfileRevisionError::MissingEvidence(
                "at least one section to change"
            ))
        ));
    }

    /// A reflection cannot reach live behaviour: applying requires both an
    /// evaluation id (carried on the proposal, and validated) and an owner
    /// approval passed separately.
    #[test]
    fn applying_requires_an_owner_approval_as_well_as_an_evaluation() {
        let mut profile = HarnessSupplementalProfile::empty("owner", "default", "program.md");
        let accepted = proposal(section("operating_notes", "check the ledger first"));
        assert!(matches!(
            profile.apply(&accepted, "   "),
            Err(ProfileRevisionError::MissingEvidence("an owner approval"))
        ));
        assert_eq!(
            profile.revision, 0,
            "a refused apply must not bump anything"
        );
        assert!(profile.sections.is_empty());

        let revision = profile.apply(&accepted, "owner").expect("approved apply");
        assert_eq!(revision, 1);
        assert_eq!(
            profile.sections.get("operating_notes").map(String::as_str),
            Some("check the ledger first")
        );
    }

    /// Revert restores the exact prior text — including "the section did not
    /// exist", which a reconstruction would get wrong by leaving an empty one.
    #[test]
    fn revert_restores_the_original_exactly_and_keeps_the_history() {
        let mut profile = HarnessSupplementalProfile::empty("owner", "default", "program.md");
        profile
            .apply(&proposal(section("operating_notes", "first")), "owner")
            .expect("first apply");

        let mut second = proposal(section("operating_notes", "second"));
        second.before =
            BTreeMap::from([("operating_notes".to_string(), Some("first".to_string()))]);
        second.candidate_id = "cand-2".to_string();
        profile.apply(&second, "owner").expect("second apply");
        assert_eq!(
            profile.sections.get("operating_notes").map(String::as_str),
            Some("second")
        );

        profile.revert(2, "owner").expect("revert the second");
        assert_eq!(
            profile.sections.get("operating_notes").map(String::as_str),
            Some("first"),
            "revert must restore the exact prior text"
        );

        // Nothing was deleted: both applies and the revert are all present,
        // and the reverted one is marked rather than removed.
        assert_eq!(profile.history.len(), 3);
        let reverted = profile
            .history
            .iter()
            .find(|entry| entry.revision == 2)
            .expect("the reverted revision is still in history");
        assert!(reverted.reverted_at.is_some());
        assert_eq!(reverted.reverted_by.as_deref(), Some("owner"));
        assert_eq!(reverted.candidate_id, "cand-2");
        assert_eq!(reverted.evaluation_id, "eval-1");

        // A second revert of the same revision is refused rather than
        // silently re-applying stale text.
        assert!(matches!(
            profile.revert(2, "owner"),
            Err(ProfileRevisionError::AlreadyReverted(2))
        ));
        assert!(matches!(
            profile.revert(99, "owner"),
            Err(ProfileRevisionError::UnknownRevision(99))
        ));
    }

    /// Reverting a revision that CREATED a section removes it again, rather
    /// than leaving an empty heading behind.
    #[test]
    fn reverting_a_created_section_removes_it_rather_than_emptying_it() {
        let mut profile = HarnessSupplementalProfile::empty("owner", "default", "program.md");
        profile
            .apply(
                &proposal(section("skill_references", "use the ledger skill")),
                "owner",
            )
            .expect("apply");
        assert!(profile.sections.contains_key("skill_references"));

        profile.revert(1, "owner").expect("revert");
        assert!(
            !profile.sections.contains_key("skill_references"),
            "a section that did not exist before must not exist after a revert"
        );
        assert!(profile.render_block().is_none());
    }

    /// A proposal written against text that has since moved is refused. Two
    /// harness cycles proposing on the same section must not silently
    /// clobber each other.
    #[test]
    fn a_proposal_written_against_stale_text_is_refused() {
        let mut profile = HarnessSupplementalProfile::empty("owner", "default", "program.md");
        profile
            .apply(&proposal(section("operating_notes", "current")), "owner")
            .expect("apply");

        let mut stale = proposal(section("operating_notes", "rewrite"));
        stale.before = BTreeMap::from([(
            "operating_notes".to_string(),
            Some("what it said two revisions ago".to_string()),
        )]);
        assert!(matches!(
            profile.apply(&stale, "owner"),
            Err(ProfileRevisionError::StaleBefore(_))
        ));
        assert_eq!(
            profile.sections.get("operating_notes").map(String::as_str),
            Some("current"),
            "a refused apply must leave the live profile untouched"
        );
    }

    /// The reader half: guidance that exists renders, and an empty profile
    /// contributes nothing rather than an empty heading.
    #[test]
    fn the_prompt_block_renders_only_real_guidance() {
        let mut profile = HarnessSupplementalProfile::empty("owner", "default", "program.md");
        assert!(profile.render_block().is_none());

        profile
            .apply(
                &proposal(section("operating_notes", "check the ledger")),
                "owner",
            )
            .expect("apply");
        let block = profile.render_block().expect("guidance renders");
        assert!(block.contains("Supplemental Guidance (revision 1)"));
        assert!(block.contains("Operating notes"));
        assert!(block.contains("check the ledger"));
        // Sections with no content are absent, not blank-headed.
        assert!(!block.contains("Skill references"));
    }

    #[test]
    fn the_artifact_survives_a_durable_round_trip() {
        let mut profile = HarnessSupplementalProfile::empty("owner", "default", "program.md");
        profile
            .apply(&proposal(section("operating_notes", "note")), "owner")
            .expect("apply");
        let encoded = serde_json::to_string(&profile).expect("serializes");
        let restored: HarnessSupplementalProfile =
            serde_json::from_str(&encoded).expect("deserializes");
        assert_eq!(profile, restored);
    }
}
