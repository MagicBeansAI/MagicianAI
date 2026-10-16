//! Durable qualification/adoption and stale worker projection regressions.
use super::*;
use crate::magician_v2::learning::{LearningProcedureBridge, LearningProcedureStatus};
use serde_json::json;

struct Fixture {
    _temp: tempfile::TempDir,
    workspace: ArtifactV2Workspace,
    policies: GuidancePolicyStore,
    learning: LearningStore,
    policy: FrozenGuidancePolicy,
}

fn assessed_pairs(scores: &[(f64, f64)]) -> (Fixture, GuidancePolicyState) {
    let f = Fixture::new();
    let mut state = f
        .policies
        .load(&f.policy.scope, f.policy.id)
        .unwrap()
        .unwrap();
    let case = state.policy.evaluation[0].clone();
    state.policy.evaluation.clear();
    for (index, (before, after)) in scores.iter().enumerate() {
        let mut case = case.clone();
        case.id = format!("case-{index}");
        for (variant, quality) in [
            (GuidanceVariant::Baseline, before),
            (GuidanceVariant::Candidate, after),
        ] {
            let run_id = state
                .policy
                .trial_id(QualificationPhase::Evaluation, &case.id, variant)
                .unwrap();
            state.observations.insert(
                run_id,
                TrialObservation {
                    run_id,
                    contract_hash: case.contract(variant).hash().unwrap(),
                    source_epoch: 0,
                    status: RunStatus::Accepted,
                    complete: true,
                    feasible: true,
                    quality: Some(*quality),
                    accepted: true,
                    operator_redirects: 0,
                    actual_cost_microusd: 1,
                    evidence_hash: digest(&run_id).unwrap(),
                },
            );
        }
        state.policy.evaluation.push(case);
    }
    (f, state)
}

#[test]
fn decimal_qualification_includes_exact_gain_and_regression_boundaries() {
    let (_fixture, mut state) = assessed_pairs(&[(1.0, 1.0), (0.9, 1.0)]);
    state.policy.spec.evaluation_requirements.minimum_mean_gain = 0.05;
    assert!(
        state
            .assess(QualificationPhase::Evaluation)
            .unwrap()
            .unwrap()
            .0
    );
    state.policy.spec.evaluation_requirements.minimum_mean_gain = 0.05000000000000001;
    assert!(
        !state
            .assess(QualificationPhase::Evaluation)
            .unwrap()
            .unwrap()
            .0,
        "a genuinely higher threshold must still fail"
    );

    let (_fixture, mut state) = assessed_pairs(&[(0.8, 0.7), (0.5, 0.8)]);
    state
        .policy
        .spec
        .evaluation_requirements
        .maximum_case_regression = 0.1;
    assert!(
        state
            .assess(QualificationPhase::Evaluation)
            .unwrap()
            .unwrap()
            .0
    );
    state
        .policy
        .spec
        .evaluation_requirements
        .maximum_case_regression = 0.09999999999999999;
    assert!(
        !state
            .assess(QualificationPhase::Evaluation)
            .unwrap()
            .unwrap()
            .0
    );
}

#[test]
fn decimal_qualification_keeps_acceptance_and_unknown_evidence_strict() {
    let (_fixture, mut state) = assessed_pairs(&[(0.9, 1.0)]);
    let id = state
        .policy
        .trial_id(
            QualificationPhase::Evaluation,
            "case-0",
            GuidanceVariant::Candidate,
        )
        .unwrap();
    state.observations.get_mut(&id).unwrap().accepted = false;
    assert!(
        !state
            .assess(QualificationPhase::Evaluation)
            .unwrap()
            .unwrap()
            .0
    );
    let observation = state.observations.get_mut(&id).unwrap();
    observation.accepted = true;
    observation.complete = false;
    assert!(
        !state
            .assess(QualificationPhase::Evaluation)
            .unwrap()
            .unwrap()
            .0
    );
    assert!(
        qualification_decimal(1e-100).is_err(),
        "unrepresentable numbers fail closed"
    );
}

#[test]
fn legacy_policy_arithmetic_and_sealed_decisions_survive_replay() {
    let (_fixture, mut state) = assessed_pairs(&[(1.0, 1.0), (0.9, 1.0)]);
    let observation_policy = state.policy.clone();
    state.policy.spec.evaluation_requirements.minimum_mean_gain = 0.05;
    state.policy.arithmetic = QualificationArithmetic::BinaryV1;
    state.policy.version_hash = state.policy.content_hash().unwrap();
    // Trial identities are bound to the frozen policy version, including arithmetic.
    for case in &state.policy.evaluation {
        for variant in [GuidanceVariant::Baseline, GuidanceVariant::Candidate] {
            let old_id = observation_policy
                .trial_id(QualificationPhase::Evaluation, &case.id, variant)
                .unwrap();
            let new_id = state
                .policy
                .trial_id(QualificationPhase::Evaluation, &case.id, variant)
                .unwrap();
            let mut observation = state.observations.remove(&old_id).unwrap();
            observation.run_id = new_id;
            state.observations.insert(new_id, observation);
        }
    }
    let serialized = serde_json::to_value(&state.policy).unwrap();
    assert!(
        serialized.get("arithmetic").is_none(),
        "legacy policy bytes keep their original shape"
    );
    let old: FrozenGuidancePolicy = serde_json::from_value(serialized).unwrap();
    old.validate_binding().unwrap();
    assert_eq!(old.arithmetic, QualificationArithmetic::BinaryV1);
    let (passed, reason) = state
        .assess(QualificationPhase::Evaluation)
        .unwrap()
        .unwrap();
    assert!(
        !passed,
        "an old frozen decision is never reinterpreted as a promotion"
    );
    let decision = Record::PhaseDecided {
        phase: QualificationPhase::Evaluation,
        passed,
        reason,
    };
    state.apply(&decision).unwrap();
    assert_eq!(state.status, GuidancePolicyStatus::Rejected);
    state.apply(&decision).unwrap();
    let old_hash = state.policy.version_hash.clone();
    state.policy.arithmetic = QualificationArithmetic::DecimalV2;
    assert_ne!(state.policy.content_hash().unwrap(), old_hash);
    assert!(
        state.policy.validate_binding().is_err(),
        "arithmetic belongs to the frozen version"
    );
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let learning = LearningStore::new(workspace.clone());
        let scope = RunScope {
            principal: "owner".into(),
            workspace: "work".into(),
        };
        let guidance = json!({"title":"Verify evidence", "workflow":["Check claims against frozen references"],
            "decision_points":[], "verification":["Review every required criterion"], "failure_modes":["Unsupported claims"]});
        let candidate = learning.create_candidate(LearningScope::new("owner", "work"),
            serde_json::from_value(json!({"candidate_type":"memory_procedure", "title":"Verify evidence",
                "summary":"Reusable procedure", "proposed_change":guidance, "source_agent_id":"engineer"})).unwrap()).unwrap();
        let mut config = magicllm::LLMRouterConfig::default();
        config.default_profile = "actor".into();
        for (name, provider, model, family) in [
            ("actor", "openai", "gpt-6.1-sol", "openai"),
            ("critic", "anthropic", "claude-sonnet-5-5", "claude"),
        ] {
            config.profiles.insert(
                name.into(),
                serde_json::from_value(json!({
                "provider":provider,"model":model,"api_key_env":"__RELENTLESS_TEST_KEY_ABSENT__",
                "context_window_tokens":128000,"max_output_tokens":4096,"timeout_secs":30,
                "metadata":{"model_family":family}}))
                .unwrap(),
            );
        }
        let router = OperationLlmRouter::new(Some(config));
        let proposal = |goal: &str| {
            json!({"goal":goal,"artifact_file":"draft.md","maximum_artifact_bytes":4096,
            "actor_profile":"actor","critic_profiles":["critic","critic","critic"],"sources":{"brief":goal},
            "criteria":[{"id":"quality","specification":{"kind":"rubric","instructions":"Complete and clear",
                "anchors":[{"score":0,"description":"Incomplete"},{"score":1,"description":"Complete"}]},
                "weight":1,"must_pass":false,"pass_score":null}],"score_at_least":0.8,
            "review":{"panel_size":3,"score_quorum":3,"min_confidence":0.8,"max_score_spread":0.2,
                "noise_margin":0.02,"max_retries_per_critic":1},
            "budget":{"max_attempts":2,"max_wall_clock_secs":600,"max_cost_microusd":40000000,"plateau_attempts":2},
            "human_review":"required","handoff_kind":"internal_draft"})
        };
        let requirements = json!({"metric":"weighted_quality","minimum_mean_gain":0.1,"maximum_case_regression":0,
            "minimum_candidate_acceptance_rate":1,"maximum_acceptance_regressions":0});
        let spec = serde_json::from_value(json!({"name":"Evidence policy","candidate_id":candidate.id,
            "agent_id":"engineer","ui_thread_id":"thread","baseline_guidance":null,"training_run_ids":[Uuid::new_v4()],
            "evaluation_cases":[{"id":"eval","proposal":proposal("Fresh held-out brief")}],
            "canary_cases":[{"id":"canary","proposal":proposal("Separate internal canary brief")}],
            "evaluation_requirements":requirements,"canary_requirements":requirements,
            "maximum_evaluation_cost_microusd":80000000,"maximum_canary_cost_microusd":80000000,
            "maximum_guidance_bytes":16384,"targets":[{"task_id":"target","task_context_hash":"a".repeat(64),
                "baseline_contract_hash":"b".repeat(64),"minimum_quality":0.8}],"maximum_adopted_runs":2,
            "disable_after_regressions":1,"unknown_monitor_disables_adoption":true,"monitor_nonacceptance_is_regression":true,
            "expires_at":Utc::now()+chrono::Duration::hours(1)})).unwrap();
        let policy =
            FrozenGuidancePolicy::resolve(workspace.clone(), &router, scope, Uuid::new_v4(), spec)
                .unwrap();
        let policies = GuidancePolicyStore::new(workspace.clone());
        policies.activate(policy.clone()).unwrap();
        Self {
            _temp: temp,
            workspace,
            policies,
            learning,
            policy,
        }
    }
    fn qualify(&self) -> GuidancePolicyState {
        let p = &self.policy;
        for phase in [QualificationPhase::Evaluation, QualificationPhase::Canary] {
            assert!(
                self.policies
                    .decide(&p.scope, p.id, &p.version_hash, phase)
                    .is_err(),
                "missing trials cannot qualify"
            );
            for case in p.cases(phase) {
                for variant in [GuidanceVariant::Baseline, GuidanceVariant::Candidate] {
                    let run_id = p.trial_id(phase, &case.id, variant).unwrap();
                    let contract_hash = case.contract(variant).hash().unwrap();
                    self.policies
                        .reserve(
                            &p.scope,
                            p.id,
                            &p.version_hash,
                            TrialReservation {
                                run_id,
                                phase,
                                case_id: case.id.clone(),
                                variant,
                                contract_hash: contract_hash.clone(),
                                proposal_hash: digest(case.proposal(variant)).unwrap(),
                                maximum_cost_microusd: case
                                    .proposal(variant)
                                    .budget
                                    .max_cost_microusd,
                            },
                        )
                        .unwrap();
                    self.policies
                        .observe(
                            &p.scope,
                            p.id,
                            &p.version_hash,
                            TrialObservation {
                                run_id,
                                contract_hash,
                                source_epoch: 0,
                                status: RunStatus::Accepted,
                                complete: true,
                                feasible: true,
                                quality: Some(if variant == GuidanceVariant::Baseline {
                                    0.8
                                } else {
                                    0.95
                                }),
                                accepted: true,
                                operator_redirects: 0,
                                actual_cost_microusd: 10,
                                evidence_hash: digest(&run_id).unwrap(),
                            },
                        )
                        .unwrap();
                }
            }
            self.policies
                .decide(&p.scope, p.id, &p.version_hash, phase)
                .unwrap();
        }
        let state = self.policies.load(&p.scope, p.id).unwrap().unwrap();
        assert_eq!(state.status, GuidancePolicyStatus::Qualified);
        state
    }
    fn project(&self, state: &GuidancePolicyState) {
        LearningProcedureBridge::new(self.workspace.clone())
            .sync_relentless_policy(&self.learning, state)
            .unwrap();
    }
    fn adoption(&self) -> AdoptionReservation {
        AdoptionReservation {
            run_id: Uuid::new_v4(),
            guided_contract_hash: "c".repeat(64),
            origin: GuidanceRunOrigin::Adoption {
                policy_id: self.policy.id,
                version_hash: self.policy.version_hash.clone(),
                task_id: "target".into(),
                baseline_contract_hash: "b".repeat(64),
            },
        }
    }
}

#[test]
fn paired_trials_promote_exact_guidance_then_unknown_monitor_disables_adoption() {
    let f = Fixture::new();
    let p = &f.policy;
    let qualified = f.qualify();
    f.project(&qualified);
    let promoted = f.policies.promote(&p.scope, p.id, &p.version_hash).unwrap();
    f.project(&promoted);
    let scope = LearningScope::new("owner", "work");
    assert_eq!(
        f.learning
            .read_procedure(&scope, &p.procedure_id())
            .unwrap()
            .status,
        LearningProcedureStatus::Active
    );
    let adopted = f.adoption();
    f.policies
        .adopt(&p.scope, p.id, &p.version_hash, adopted.clone())
        .unwrap();
    let disabled = f
        .policies
        .monitor(
            &p.scope,
            p.id,
            &p.version_hash,
            AdoptionObservation {
                run_id: adopted.run_id,
                regressed: false,
                unknown: true,
                evidence_hash: "d".repeat(64),
            },
        )
        .unwrap();
    assert_eq!(disabled.status, GuidancePolicyStatus::Regressed);
    assert!(f
        .policies
        .adopt(&p.scope, p.id, &p.version_hash, f.adoption())
        .is_err());
    f.project(&promoted); // Late worker must project the current disabled state.
    assert_eq!(
        f.learning
            .read_procedure(&scope, &p.procedure_id())
            .unwrap()
            .status,
        LearningProcedureStatus::Archived
    );
}

#[test]
fn stale_promoted_worker_cannot_create_a_procedure_after_revocation() {
    let f = Fixture::new();
    let p = &f.policy;
    f.qualify();
    let stale = f.policies.promote(&p.scope, p.id, &p.version_hash).unwrap();
    let revoked = f.policies.revoke(&p.scope, p.id, &p.version_hash).unwrap();
    f.project(&revoked); // Missing procedure is intentionally a no-op.
    f.project(&stale); // Previously this created and activated a fresh procedure.
    let error = f
        .learning
        .read_procedure(&LearningScope::new("owner", "work"), &p.procedure_id())
        .unwrap_err();
    assert!(f.learning.error_is_not_found(&error));
    assert!(f
        .policies
        .adopt(&p.scope, p.id, &p.version_hash, f.adoption())
        .is_err());
}

#[test]
fn stale_qualified_projection_preserves_current_promotion_and_held_out_inputs_cannot_be_reused() {
    let f = Fixture::new();
    let p = &f.policy;
    let stale = f.qualify();
    f.project(&stale);
    let promoted = f.policies.promote(&p.scope, p.id, &p.version_hash).unwrap();
    f.project(&promoted);
    f.project(&stale);
    assert_eq!(
        f.learning
            .read_procedure(&LearningScope::new("owner", "work"), &p.procedure_id())
            .unwrap()
            .status,
        LearningProcedureStatus::Active
    );
    let mut reused = p.clone();
    reused.id = Uuid::new_v4();
    reused.spec.targets[0].task_id = "different-target".into();
    reused.version_hash = reused.content_hash().unwrap();
    assert!(f
        .policies
        .activate(reused)
        .unwrap_err()
        .to_string()
        .contains("held-out inputs"));
}
