use super::*;

struct Fixture {
    _temp: tempfile::TempDir,
    store: CampaignStore,
    scope: EnvelopeStoreScope,
    state: CampaignState,
    files: BTreeMap<String, String>,
    proof: super::super::content::ValidatedTemplate,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = CampaignStore::new(ArtifactV2Workspace::new(temp.path()));
        let scope = EnvelopeStoreScope::new("owner", "work");
        let (template, files, sources) = super::super::content::tests::sample();
        let proof = template.validate_artifact(&files, &sources).unwrap();
        let files = files
            .into_iter()
            .map(|(name, bytes)| (name, blake3::hash(&bytes).to_hex().to_string()))
            .collect();
        let spec = CampaignSpec {
            name: "Local capture policy".into(),
            goal_id: None,
            sender_identity: "sender".into(),
            channel: "email".into(),
            message_kind: "outreach".into(),
            targets: vec![CampaignTarget {
                task_id: "task".into(),
                task_context_hash: "a".repeat(64),
                identity_id: "recipient".into(),
                recipient: Some("alex@example.test".into()),
            }],
            allowed_contract_hashes: BTreeSet::from(["b".repeat(64)]),
            content_contract_hash: template.hash().unwrap(),
            content_template: Some(template),
            maximum_acts: 3,
            maximum_acts_per_day: 3,
            maximum_acts_per_recipient: 3,
            expires_at: Utc::now() + chrono::Duration::hours(1),
            approval: CaptureApprovalPolicy {
                source_document: "Owner policy".into(),
                always_review: false,
                important_identity_ids: BTreeSet::new(),
                allow_automatic_capture: true,
                clean_approvals_before_automatic_capture: Some(1),
                audit_every: 3,
            },
        };
        let state = store.activate(&scope, Uuid::new_v4(), None, spec).unwrap();
        Self {
            _temp: temp,
            store,
            scope,
            state,
            files,
            proof,
        }
    }
    fn binding(&self) -> CaptureBinding {
        let policy = &self.state.campaign;
        CaptureBinding {
            scope: magicllm::LlmScope::new("owner", "work"),
            campaign_id: policy.id,
            version_hash: policy.version_hash.clone(),
            run_id: Uuid::new_v4(),
            task_id: "task".into(),
            task_context_hash: "a".repeat(64),
            identity_id: "recipient".into(),
            recipient: "alex@example.test".into(),
            sender_identity: "sender".into(),
            channel: "email".into(),
            message_kind: "outreach".into(),
            content_contract_hash: policy.spec.content_contract_hash.clone(),
            contract_hash: "b".repeat(64),
            artifact_hash: "c".repeat(64),
            artifact_files: self.files.clone(),
            content_validation: Some(self.proof.clone()),
            content_review_reason: None,
        }
    }
}

#[test]
fn relentless_campaign_exact_review_seeds_simulated_trust_and_audit_rejection_suspends_it() {
    let f = Fixture::new();
    let id = f.state.campaign.id;
    let binding = f.binding();
    let first = f.store.capture(&f.scope, binding.clone(), false).unwrap();
    assert_eq!(first.status, CaptureStatus::AwaitingReview);
    assert!(f
        .store
        .review(&f.scope, id, &first.id, &"f".repeat(64), true)
        .is_err());
    let reviewed = f
        .store
        .review(&f.scope, id, &first.id, &first.binding_hash, true)
        .unwrap();
    assert_eq!(reviewed.status, CaptureStatus::Captured);
    assert_eq!(f.store.capture(&f.scope, binding, false).unwrap(), reviewed);
    f.store
        .review(&f.scope, id, &first.id, &first.binding_hash, true)
        .unwrap();
    let clean = f.store.load(&f.scope, id).unwrap().unwrap();
    assert_eq!(clean.captures.len(), 1);
    assert_eq!(clean.simulated_clean_approvals, 1);
    assert_eq!(clean.production_clean_approvals, 0);
    assert!(!clean.live_dispatch_enabled);
    let second = f.store.capture(&f.scope, f.binding(), false).unwrap();
    assert_eq!(second.status, CaptureStatus::Captured);
    assert!(second.owner_reviewed_by.is_none());
    let audit = f.store.capture(&f.scope, f.binding(), false).unwrap();
    assert!(audit.audit_required);
    assert_eq!(audit.status, CaptureStatus::AwaitingReview);
    f.store
        .review(&f.scope, id, &audit.id, &audit.binding_hash, false)
        .unwrap();
    let state = f.store.load(&f.scope, id).unwrap().unwrap();
    assert!(state.automatic_capture_suspended);
    assert_eq!(state.simulated_clean_approvals, 0);
    assert!(
        f.store.capture(&f.scope, f.binding(), true).is_err(),
        "rejected acts still consume the finite cap"
    );
}

#[test]
fn relentless_campaign_refuses_changed_recipient_files_scope_and_revoked_pending_review() {
    let f = Fixture::new();
    for change in ["recipient", "files", "scope", "context"] {
        let mut binding = f.binding();
        match change {
            "recipient" => binding.recipient = "different@example.test".into(),
            "files" => {
                binding
                    .artifact_files
                    .insert("message.txt".into(), "d".repeat(64));
            },
            "scope" => binding.scope = magicllm::LlmScope::new("other", "work"),
            _ => binding.task_context_hash = "e".repeat(64),
        }
        assert!(
            f.store.capture(&f.scope, binding, true).is_err(),
            "{change}"
        );
    }
    let pending = f.store.capture(&f.scope, f.binding(), false).unwrap();
    let id = f.state.campaign.id;
    f.store
        .revoke(&f.scope, id, &f.state.campaign.version_hash)
        .unwrap();
    assert!(f
        .store
        .review(&f.scope, id, &pending.id, &pending.binding_hash, true)
        .is_err());
    assert!(f.store.capture(&f.scope, f.binding(), true).is_err());
    // Revocation preserves historical inspection and permits exact rejection.
    assert!(f
        .store
        .load(&f.scope, id)
        .unwrap()
        .unwrap()
        .captures
        .contains_key(&pending.id));
    f.store
        .review(&f.scope, id, &pending.id, &pending.binding_hash, false)
        .unwrap();
}
