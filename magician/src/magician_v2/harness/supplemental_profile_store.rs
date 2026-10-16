//! File-backed persistence for Boundary D's supplemental guidance.
//!
//! One profile per program document, at
//! `programs/supplemental_profiles/<program key>.json` under the scope
//! programs root. Mirrors the anomaly/backlog stores: atomic write, tolerant
//! read, sync so both sync and async harness contexts can call it, and a
//! per-path lock so two cycles applying revisions cannot interleave a
//! read-modify-write.
//!
//! Loading a profile whose `program_relative_path` disagrees with the one
//! asked for is treated as absent rather than returned. Guidance written for
//! one program must never be read into another, and a mismatch here means the
//! file was hand-moved or the key scheme changed — either way the safe answer
//! is "this program has no guidance yet", not someone else's.

use crate::magician_v2::artifact_v2::{workspace::ArtifactV2Workspace, ArtifactV2Error};

use super::supplemental_profile::{
    HarnessSupplementalProfile, ProfileRevisionError, ProfileRevisionProposal,
};

pub struct SupplementalProfileStore {
    ws: ArtifactV2Workspace,
}

impl SupplementalProfileStore {
    pub fn new(ws: ArtifactV2Workspace) -> Self {
        Self { ws }
    }

    /// The profile for a program, or an empty one when none has been written.
    /// Never an error for "not yet": a program with no guidance is the normal
    /// starting state, and the caller would only have to translate it back.
    pub fn load(
        &self,
        principal: &str,
        workspace: &str,
        program_relative_path: &str,
    ) -> HarnessSupplementalProfile {
        let path =
            self.ws
                .programs_supplemental_profile_path(principal, workspace, program_relative_path);
        match self
            .ws
            .read_json_path_sync::<HarnessSupplementalProfile, _>(&path)
        {
            Ok(profile)
                if profile.program_relative_path == program_relative_path
                    && profile.principal == principal
                    && profile.workspace == workspace =>
            {
                profile
            },
            _ => HarnessSupplementalProfile::empty(principal, workspace, program_relative_path),
        }
    }

    pub fn save(&self, profile: &HarnessSupplementalProfile) -> Result<(), ArtifactV2Error> {
        let path = self.ws.programs_supplemental_profile_path(
            &profile.principal,
            &profile.workspace,
            &profile.program_relative_path,
        );
        self.ws.write_json_atomic_path_sync(&path, profile)
    }

    /// Apply a proposal to the live profile as one locked read-modify-write.
    ///
    /// The lock is the point: `apply` refuses a proposal whose `before` no
    /// longer matches the live text, so two concurrent applications without it
    /// would not corrupt the profile but would make the loser's refusal depend
    /// on timing rather than on staleness.
    pub fn apply_revision(
        &self,
        principal: &str,
        workspace: &str,
        program_relative_path: &str,
        proposal: &ProfileRevisionProposal,
        approved_by: &str,
    ) -> Result<HarnessSupplementalProfile, ApplyRevisionError> {
        let path =
            self.ws
                .programs_supplemental_profile_path(principal, workspace, program_relative_path);
        let lock = super::harness_record_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        let mut profile = self.load(principal, workspace, program_relative_path);
        profile.apply(proposal, approved_by)?;
        self.save(&profile)?;
        Ok(profile)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApplyRevisionError {
    #[error(transparent)]
    Revision(#[from] ProfileRevisionError),
    #[error(transparent)]
    Storage(#[from] ArtifactV2Error),
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    fn workspace() -> (ArtifactV2Workspace, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = ArtifactV2Workspace::new(dir.path());
        (ws, dir)
    }

    fn proposal(body: &str) -> ProfileRevisionProposal {
        ProfileRevisionProposal {
            candidate_id: "cand-1".to_string(),
            episode_evidence: vec!["episode-1".to_string()],
            evaluation_id: "eval-1".to_string(),
            before: BTreeMap::new(),
            after: BTreeMap::from([("operating_notes".to_string(), Some(body.to_string()))]),
            expected_benefit: "fewer repeated no-progress cycles".to_string(),
            confidence: "medium".to_string(),
            risk: "low — guidance only".to_string(),
            rollback_plan: "revert the revision".to_string(),
            evaluation_case: "harness cycle on the stalled lane".to_string(),
            acceptance_condition: "two consecutive cycles settle with a decision".to_string(),
        }
    }

    /// A program with no guidance yet is the normal starting state, not an
    /// error the caller has to translate back into one.
    #[test]
    fn a_program_with_no_guidance_loads_as_an_empty_profile() {
        let (ws, _dir) = workspace();
        let store = SupplementalProfileStore::new(ws);

        let profile = store.load("owner", "default", "program.md");

        assert_eq!(profile.revision, 0);
        assert!(profile.sections.is_empty());
        assert_eq!(profile.program_relative_path, "program.md");
        assert!(profile.render_block().is_none());
    }

    /// The round trip the reader depends on: what was applied is what the next
    /// cycle loads, at the revision it was given.
    #[test]
    fn an_applied_revision_survives_a_reload() {
        let (ws, _dir) = workspace();
        let store = SupplementalProfileStore::new(ws);

        let applied = store
            .apply_revision(
                "owner",
                "default",
                "program.md",
                &proposal("Check the backlog before opening a new loop."),
                "owner",
            )
            .expect("apply");
        assert_eq!(applied.revision, 1);

        let reloaded = store.load("owner", "default", "program.md");
        assert_eq!(reloaded.revision, 1);
        assert_eq!(
            reloaded.sections.get("operating_notes").map(String::as_str),
            Some("Check the backlog before opening a new loop.")
        );
        assert_eq!(reloaded.history.len(), 1);
        assert_eq!(reloaded.history[0].evaluation_id, "eval-1");
        assert_eq!(reloaded.history[0].approved_by, "owner");
    }

    /// Guidance is keyed by the program it supplements, so two programs in one
    /// scope cannot read each other's.
    #[test]
    fn guidance_for_one_program_is_not_visible_to_another() {
        let (ws, _dir) = workspace();
        let store = SupplementalProfileStore::new(ws);

        store
            .apply_revision(
                "owner",
                "default",
                "program.md",
                &proposal("Guidance for the first program."),
                "owner",
            )
            .expect("apply");

        let other = store.load("owner", "default", "harness_reliability.md");
        assert_eq!(other.revision, 0);
        assert!(other.sections.is_empty());
    }

    /// A proposal written against text that has since moved is refused rather
    /// than silently discarding the newer guidance.
    #[test]
    fn a_proposal_written_against_stale_text_is_refused() {
        let (ws, _dir) = workspace();
        let store = SupplementalProfileStore::new(ws);
        store
            .apply_revision(
                "owner",
                "default",
                "program.md",
                &proposal("first"),
                "owner",
            )
            .expect("first apply");

        let mut stale = proposal("second");
        stale.before = BTreeMap::from([("operating_notes".to_string(), None)]);
        let error = store
            .apply_revision("owner", "default", "program.md", &stale, "owner")
            .expect_err("stale before must refuse");

        assert!(matches!(
            error,
            ApplyRevisionError::Revision(ProfileRevisionError::StaleBefore(_))
        ));
        assert_eq!(
            store
                .load("owner", "default", "program.md")
                .sections
                .get("operating_notes")
                .map(String::as_str),
            Some("first"),
            "the refused apply must not have moved the live text"
        );
    }

    /// The whole boundary's premise: no path here mutates a profile without an
    /// owner's name on it.
    #[test]
    fn applying_without_an_approver_is_refused() {
        let (ws, _dir) = workspace();
        let store = SupplementalProfileStore::new(ws);

        let error = store
            .apply_revision("owner", "default", "program.md", &proposal("body"), "   ")
            .expect_err("an empty approver must refuse");

        assert!(matches!(
            error,
            ApplyRevisionError::Revision(ProfileRevisionError::MissingEvidence(_))
        ));
        assert_eq!(store.load("owner", "default", "program.md").revision, 0);
    }
}
