use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum StoreKind {
    Transaction,
    CodeChangeProposal,
    PauseState,
}

/// Globally unique across the three stores + scope.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct TrustedRecordKey {
    pub kind: StoreKind,
    pub principal: String,
    pub workspace: String,
    pub id: String,
}

/// The authoritative, agent-unreachable copy of a record's security-bearing fields.
#[derive(Clone, Debug)]
pub struct AuthorityEntry {
    pub content_hash: blake3::Hash,
    pub apply_root: Option<PathBuf>,
    pub target_paths: Vec<PathBuf>,
}

/// Security-bearing provenance bound to the exact revision shown for review.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DecisionProvenance {
    pub principal: String,
    pub workspace: String,
    pub task_id: Option<String>,
    pub execution_id: Option<String>,
    pub review_revision: blake3::Hash,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum DecisionVerificationError {
    #[error("record was not staged by the trust authority")]
    NotStaged,
    #[error("record has no authenticated decision provenance")]
    Unauthenticated,
    #[error("decision principal does not match staged provenance")]
    PrincipalMismatch,
    #[error("decision workspace does not match staged provenance")]
    WorkspaceMismatch,
    #[error("decision task_id does not match staged provenance")]
    TaskIdMismatch,
    #[error("decision execution_id does not match staged provenance")]
    ExecutionIdMismatch,
    #[error("decision review revision does not match the staged revision")]
    ReviewRevisionMismatch,
}

#[derive(Clone, Debug)]
struct TrustedAuthorityRecord {
    entry: AuthorityEntry,
    provenance: Option<DecisionProvenance>,
}

/// In-process, per-boot map. Populated ONLY when magician itself stages a record,
/// so the shell — which cannot reach process memory — can neither create nor
/// mutate an entry. Empty after a restart (by design: non-terminal records get
/// re-asked, never auto-applied from unverifiable disk).
#[derive(Debug, Default)]
pub struct TrustAuthority {
    inner: RwLock<HashMap<TrustedRecordKey, TrustedAuthorityRecord>>,
}

impl TrustAuthority {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_stage(&self, key: TrustedRecordKey, entry: AuthorityEntry) {
        self.inner.write().expect("TrustAuthority poisoned").insert(
            key,
            TrustedAuthorityRecord {
                entry,
                provenance: None,
            },
        );
    }

    /// Record a reviewable file-edit stage with authenticated provenance.
    ///
    /// The key scope must agree with the provenance scope. Rejecting an
    /// inconsistent stage prevents callers from creating an authority entry
    /// whose lookup identity and authenticated identity disagree.
    pub fn record_authenticated_stage(
        &self,
        key: TrustedRecordKey,
        entry: AuthorityEntry,
        provenance: DecisionProvenance,
    ) -> Result<(), DecisionVerificationError> {
        if key.principal != provenance.principal {
            return Err(DecisionVerificationError::PrincipalMismatch);
        }
        if key.workspace != provenance.workspace {
            return Err(DecisionVerificationError::WorkspaceMismatch);
        }
        self.inner.write().expect("TrustAuthority poisoned").insert(
            key,
            TrustedAuthorityRecord {
                entry,
                provenance: Some(provenance),
            },
        );
        Ok(())
    }

    pub fn record_authenticated_file_edit_stage(
        &self,
        key: TrustedRecordKey,
        entry: AuthorityEntry,
        task_id: Option<String>,
        execution_id: Option<String>,
        review_revision: blake3::Hash,
    ) -> Result<(), DecisionVerificationError> {
        if entry.content_hash != review_revision {
            return Err(DecisionVerificationError::ReviewRevisionMismatch);
        }
        let provenance = DecisionProvenance {
            principal: key.principal.clone(),
            workspace: key.workspace.clone(),
            task_id,
            execution_id,
            review_revision,
        };
        self.record_authenticated_stage(key, entry, provenance)
    }

    /// The decision-time lookup. `None` ⇒ fail-closed (re-ask).
    pub fn get(&self, key: &TrustedRecordKey) -> Option<AuthorityEntry> {
        self.inner
            .read()
            .expect("TrustAuthority poisoned")
            .get(key)
            .map(|record| record.entry.clone())
    }

    /// Authenticate all decision provenance and return only the trusted entry.
    /// This is the decision-time API for `web_api`: construct `presented` from
    /// the loaded record and request scope, then map the typed mismatch to a
    /// conflict/fail-closed response.
    pub fn verify_decision(
        &self,
        key: &TrustedRecordKey,
        presented: &DecisionProvenance,
    ) -> Result<AuthorityEntry, DecisionVerificationError> {
        let inner = self.inner.read().expect("TrustAuthority poisoned");
        let record = inner.get(key).ok_or(DecisionVerificationError::NotStaged)?;
        let staged = record
            .provenance
            .as_ref()
            .ok_or(DecisionVerificationError::Unauthenticated)?;
        if staged.principal != presented.principal {
            return Err(DecisionVerificationError::PrincipalMismatch);
        }
        if staged.workspace != presented.workspace {
            return Err(DecisionVerificationError::WorkspaceMismatch);
        }
        if staged.task_id != presented.task_id {
            return Err(DecisionVerificationError::TaskIdMismatch);
        }
        if staged.execution_id != presented.execution_id {
            return Err(DecisionVerificationError::ExecutionIdMismatch);
        }
        if staged.review_revision != presented.review_revision {
            return Err(DecisionVerificationError::ReviewRevisionMismatch);
        }
        Ok(record.entry.clone())
    }

    /// Primitive-argument facade so callers do not need to construct an
    /// authority-owned provenance value.
    #[allow(clippy::too_many_arguments)]
    pub fn verify_file_edit_decision(
        &self,
        key: &TrustedRecordKey,
        principal: &str,
        workspace: &str,
        task_id: Option<&str>,
        execution_id: Option<&str>,
        review_revision: blake3::Hash,
    ) -> Result<AuthorityEntry, DecisionVerificationError> {
        self.verify_decision(
            key,
            &DecisionProvenance {
                principal: principal.to_string(),
                workspace: workspace.to_string(),
                task_id: task_id.map(str::to_string),
                execution_id: execution_id.map(str::to_string),
                review_revision,
            },
        )
    }

    /// Drop an entry once its record reaches a terminal state.
    pub fn forget(&self, key: &TrustedRecordKey) {
        self.inner
            .write()
            .expect("TrustAuthority poisoned")
            .remove(key);
    }
}

/// Process-global handle to the single `TrustAuthority` instance held by
/// `AgentApiServices`.
///
/// The authority is created once, at `AgentApiServices` construction, and stored
/// on that struct so the HTTP **decision** site (`respond_hitl_handler`) reaches
/// it directly. The **stage** site, however, runs deep inside the executor's
/// compiled handlers, which only receive `AgentResources` — a context that does
/// NOT (and, to stay non-invasive, should not) carry a reference to the API-layer
/// struct. Threading the `Arc<TrustAuthority>` through every `AgentResources`
/// construction site would be an invasive change touching many boots.
///
/// Instead, `AgentApiServices` **installs** its authority `Arc` here once (via
/// [`install_process_authority`]); the stage site reads the SAME instance via
/// [`process_authority`]. Because it is the identical `Arc`, an entry recorded at
/// stage time is visible at decision time — the property the security fix relies
/// on. Install-or-get (`OnceLock::get_or_init`): the first caller's `Arc` becomes
/// the canonical global and every later caller gets that SAME instance back, so a
/// second construction cannot end up with a field `Arc` that diverges from the
/// global the stage site records into.
static PROCESS_TRUST_AUTHORITY: OnceLock<Arc<TrustAuthority>> = OnceLock::new();

/// Install-or-get the process-global `TrustAuthority`, returning the canonical
/// instance. The FIRST caller's `Arc` wins; later callers get that same first
/// `Arc` back (their argument is dropped). Callers MUST store the RETURN value on
/// their struct — never the `Arc` they passed in — so the struct field and the
/// global are guaranteed identical. Storing the passed-in `Arc` instead would let
/// a second construction's field diverge from the global and silently fail-close
/// every approval on that instance.
#[must_use]
pub fn install_process_authority(authority: Arc<TrustAuthority>) -> Arc<TrustAuthority> {
    PROCESS_TRUST_AUTHORITY.get_or_init(|| authority).clone()
}

/// The process-global `TrustAuthority`, if one has been installed. `None` before
/// any `AgentApiServices` is built (e.g. minimal/unit-test boots) — callers at
/// the stage site treat that as "no authority to record into" and proceed exactly
/// as before (the decision site then fail-closes, which is the safe default).
pub fn process_authority() -> Option<Arc<TrustAuthority>> {
    PROCESS_TRUST_AUTHORITY.get().cloned()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn key(id: &str) -> TrustedRecordKey {
        TrustedRecordKey {
            kind: StoreKind::CodeChangeProposal,
            principal: "presto".to_string(),
            workspace: "default".to_string(),
            id: id.to_string(),
        }
    }

    fn entry_with_root(root: &str) -> AuthorityEntry {
        AuthorityEntry {
            content_hash: blake3::hash(b"record bytes"),
            apply_root: Some(PathBuf::from(root)),
            target_paths: vec![PathBuf::from(root).join("src/main.rs")],
        }
    }

    #[test]
    fn staged_record_get_returns_apply_root() {
        let authority = TrustAuthority::new();
        let k = key("abc123");
        authority.record_stage(k.clone(), entry_with_root("/repo/checkout"));

        let got = authority
            .get(&k)
            .expect("staged record must be retrievable");
        assert_eq!(got.apply_root, Some(PathBuf::from("/repo/checkout")));
    }

    #[test]
    fn unstaged_id_is_none_fail_closed() {
        let authority = TrustAuthority::new();
        authority.record_stage(key("abc123"), entry_with_root("/repo/checkout"));

        // A key magician never staged must return None so callers fail closed.
        assert!(authority.get(&key("never-staged")).is_none());
    }

    #[test]
    fn forget_removes_the_entry() {
        let authority = TrustAuthority::new();
        let k = key("abc123");
        authority.record_stage(k.clone(), entry_with_root("/repo/checkout"));
        assert!(authority.get(&k).is_some());

        authority.forget(&k);
        assert!(authority.get(&k).is_none());
    }

    /// A fresh `TrustAuthority` (a simulated reboot — the per-boot map starts
    /// empty) returns `None` for a key an earlier authority instance staged,
    /// so any pending record fails closed and is re-asked instead of
    /// auto-applied from unverifiable disk. Locks the "restart ⇒ re-ask, never
    /// auto-apply" property.
    #[test]
    fn restart_empties_authority_so_pending_fail_closes() {
        let before_restart = TrustAuthority::new();
        let k = key("abc123");
        before_restart.record_stage(k.clone(), entry_with_root("/repo/checkout"));
        assert!(
            before_restart.get(&k).is_some(),
            "staged record is retrievable before the restart"
        );

        // Simulate a reboot: a brand-new authority has an empty per-boot map.
        let after_restart = TrustAuthority::new();
        assert!(
            after_restart.get(&k).is_none(),
            "after a restart the same key MUST be absent (fail-closed re-ask)"
        );
    }

    /// The comparison the decide site makes: it recomputes the loaded record's
    /// content hash and compares against the authority entry. An entry recorded
    /// with the approved content's hash does NOT equal the hash of forged
    /// content, so a tampered record is detectable and fails closed.
    #[test]
    fn content_hash_mismatch_is_detectable() {
        let authority = TrustAuthority::new();
        let k = key("abc123");
        let entry = AuthorityEntry {
            content_hash: blake3::hash(b"approved"),
            apply_root: Some(PathBuf::from("/repo/checkout")),
            target_paths: vec![PathBuf::from("/repo/checkout/src/main.rs")],
        };
        authority.record_stage(k.clone(), entry);

        let recorded = authority
            .get(&k)
            .expect("staged record must be retrievable");
        assert_eq!(
            recorded.content_hash,
            blake3::hash(b"approved"),
            "the recorded hash matches the approved content"
        );
        assert_ne!(
            recorded.content_hash,
            blake3::hash(b"forged"),
            "the recorded hash MUST NOT match forged content (mismatch → fail closed)"
        );
    }

    #[test]
    fn decision_verifier_rejects_forged_provenance_and_revision() {
        let authority = TrustAuthority::new();
        let k = key("bound-review");
        let revision = blake3::hash(b"displayed diff revision");
        let mut entry = entry_with_root("/repo/checkout");
        entry.content_hash = revision;
        authority
            .record_authenticated_file_edit_stage(
                k.clone(),
                entry,
                Some("task-1".to_string()),
                Some("exec-1".to_string()),
                revision,
            )
            .expect("authenticated stage");

        assert!(authority
            .verify_file_edit_decision(
                &k,
                "presto",
                "default",
                Some("task-1"),
                Some("exec-1"),
                revision,
            )
            .is_ok());

        let cases = [
            (
                authority.verify_file_edit_decision(
                    &k,
                    "forged-principal",
                    "default",
                    Some("task-1"),
                    Some("exec-1"),
                    revision,
                ),
                DecisionVerificationError::PrincipalMismatch,
            ),
            (
                authority.verify_file_edit_decision(
                    &k,
                    "presto",
                    "forged-workspace",
                    Some("task-1"),
                    Some("exec-1"),
                    revision,
                ),
                DecisionVerificationError::WorkspaceMismatch,
            ),
            (
                authority.verify_file_edit_decision(
                    &k,
                    "presto",
                    "default",
                    Some("forged-task"),
                    Some("exec-1"),
                    revision,
                ),
                DecisionVerificationError::TaskIdMismatch,
            ),
            (
                authority.verify_file_edit_decision(
                    &k,
                    "presto",
                    "default",
                    Some("task-1"),
                    Some("forged-exec"),
                    revision,
                ),
                DecisionVerificationError::ExecutionIdMismatch,
            ),
            (
                authority.verify_file_edit_decision(
                    &k,
                    "presto",
                    "default",
                    Some("task-1"),
                    Some("exec-1"),
                    blake3::hash(b"forged revision"),
                ),
                DecisionVerificationError::ReviewRevisionMismatch,
            ),
        ];
        for (result, expected) in cases {
            assert_eq!(result.unwrap_err(), expected);
        }
    }

    #[test]
    fn legacy_entry_is_not_accepted_as_authenticated_file_edit() {
        let authority = TrustAuthority::new();
        let k = key("legacy");
        authority.record_stage(k.clone(), entry_with_root("/repo/checkout"));
        let result = authority.verify_file_edit_decision(
            &k,
            "presto",
            "default",
            None,
            None,
            blake3::hash(b"revision"),
        );
        assert_eq!(
            result.unwrap_err(),
            DecisionVerificationError::Unauthenticated
        );
    }

    #[test]
    fn process_authority_install_is_first_writer_wins_and_shared() {
        // Before install (in an isolated test binary this holds; when run with the
        // rest of the crate's tests another test may have installed first, which is
        // exactly the first-writer-wins semantic we assert below).
        let first = Arc::new(TrustAuthority::new());
        let installed = install_process_authority(first.clone());
        assert!(
            process_authority().is_some(),
            "an authority is installed after the first install"
        );

        // A second install must NOT replace the first, and MUST return the first
        // (canonical) instance — so a caller that stores the return value can never
        // diverge from the global. Record into the returned instance and confirm the
        // global sees it — the property the stage↔decide binding relies on.
        let second = Arc::new(TrustAuthority::new());
        let second_ret = install_process_authority(second);
        assert!(
            Arc::ptr_eq(&second_ret, &installed),
            "second install returns the FIRST canonical Arc, not its own argument"
        );
        let k = key("proc-global-abc");
        installed.record_stage(k.clone(), entry_with_root("/repo/checkout"));
        assert!(
            process_authority()
                .expect("still installed")
                .get(&k)
                .is_some(),
            "the globally-installed authority must be the same instance the stage \
             site records into"
        );
    }
}
