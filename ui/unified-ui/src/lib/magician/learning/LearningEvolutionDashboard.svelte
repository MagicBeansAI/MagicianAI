<!-- Operator surface for the scoped skill/tool-pack evolution review chain. -->
<script lang="ts">
	import { onMount } from 'svelte';
	import { timedFetch } from '$lib/shared/fetch';

	interface BacklogItem {
		candidate_id: string;
		status: string;
		title: string;
		capability_id?: string;
		proposed_fix_type?: string;
		risk_level?: string;
		recurrence_count?: number;
		blocked_task_count?: number;
		user_pain_signal_count?: number;
		validation_failure_count?: number;
		local_validation_available?: boolean;
		rank_score?: number;
		rank_reasons?: string[];
		owner_hints?: string[];
		updated_at?: string;
	}

	interface Proposal {
		candidate_id: string;
		id: string;
		status: string;
		title: string;
		capability_id?: string;
		proposed_fix_type?: string;
		proposed_files?: string[];
		patches?: ProposalPatch[];
		eval_plan?: unknown;
		validation_plan?: unknown;
		promotion_gate?: unknown;
		updated_at?: string;
	}

	interface ProposalPatch {
		path: string;
		operation: string;
		summary?: string;
		diff?: string;
		metadata?: Record<string, unknown>;
	}

	interface Evaluation {
		id: string;
		candidate_id: string;
		status: string;
		title: string;
		case_kind: string;
		priority: string;
		focus_area?: string;
		updated_at?: string;
	}

	interface EvaluationRun {
		id: string;
		candidate_id: string;
		backlog_id: string;
		status: string;
		summary: string;
		runner: string;
		commands?: string[];
		metrics?: Record<string, unknown>;
		created_at?: string;
	}

	interface GrowthEvaluationRun {
		id: string;
		suite_id: string;
		status: string;
		summary: string;
		dimensions?: GrowthEvaluationDimension[];
		scenarios?: GrowthEvaluationScenario[];
		metrics?: Record<string, unknown>;
		created_at?: string;
	}

	interface GrowthEvaluationDimension {
		dimension: string;
		status: string;
		score: number;
		summary: string;
		metrics?: Record<string, unknown>;
	}

	interface GrowthEvaluationScenario {
		scenario: string;
		status: string;
		summary: string;
		metrics?: Record<string, unknown>;
	}

	interface Validation {
		id: string;
		candidate_id: string;
		status: string;
		summary: string;
		runner: string;
		commands?: string[];
		metrics?: Record<string, unknown>;
		created_at?: string;
	}

	interface Implementation {
		id: string;
		candidate_id: string;
		validation_id: string;
		summary: string;
		actor: string;
		applied_files?: string[];
		patches?: ProposalPatch[];
		created_at?: string;
	}

	interface Application {
		id: string;
		candidate_id: string;
		implementation_id: string;
		validation_id: string;
		mode: string;
		status: string;
		summary: string;
		changed_files?: { path: string }[];
		payload?: {
			runtime_catalog_refresh?: CatalogRefresh;
		};
		created_at?: string;
	}

	interface CatalogRefresh {
		status?: string;
		changed_skill_names?: string[];
		procedure_skill_count?: number;
		pack_definition_count_from_target_surface?: number;
		runtime_visibility?: string;
	}

	type RollbackRecommendationStatus = 'recommended' | 'dismissed' | 'superseded';

	interface RollbackRecommendation {
		id: string;
		candidate_id: string;
		proposal_id: string;
		validation_id?: string | null;
		implementation_id?: string | null;
		application_id: string;
		promotion_id?: string | null;
		capability_id?: string | null;
		status: RollbackRecommendationStatus;
		trigger_kind: string;
		severity: string;
		actor: string;
		summary: string;
		rollback_files?: string[];
		payload?: Record<string, unknown>;
		created_at?: string;
	}

	type PostPromotionMonitorStatus =
		| 'pending'
		| 'no_usage'
		| 'insufficient_evidence'
		| 'stable'
		| 'regression_detected';

	interface PostPromotionMonitor {
		id: string;
		promotion_id: string;
		candidate_id: string;
		proposal_id: string;
		validation_id: string;
		implementation_id?: string | null;
		application_id?: string | null;
		capability_id?: string | null;
		status: PostPromotionMonitorStatus;
		summary: string;
		skill_names?: string[];
		before_invocation_count: number;
		after_invocation_count: number;
		before_success_rate?: number | null;
		after_success_rate?: number | null;
		same_failure_recurrence_count: number;
		new_failure_classes?: string[];
		user_negative_feedback_count: number;
		rollback_recommendation_id?: string | null;
		follow_up_candidate_id?: string | null;
		updated_at?: string;
	}

	interface Promotion {
		id: string;
		candidate_id: string;
		validation_id: string;
		implementation_id?: string;
		application_id?: string;
		summary: string;
		actor: string;
		created_at?: string;
	}

	type TeachingAction =
		| 'remember'
		| 'forget'
		| 'correct'
		| 'make_reusable'
		| 'improve_tool'
		| 'never_do_this'
		| 'this_was_useful'
		| 'this_was_wrong';

	interface ProgramStateUpdateChange {
		program_relative_path?: string;
		relative_path?: string;
		program_path?: string;
		program?: string;
		program_section?: string;
		section?: string;
		goal_id?: string;
		patch?: Record<string, unknown>;
		state_patch?: Record<string, unknown>;
		update?: Record<string, unknown>;
		reason?: string;
	}

	interface Candidate {
		id: string;
		candidate_type: string;
		state: string;
		title: string;
		summary: string;
		proposed_change?: {
			memory?: {
				key?: string;
				target_tier?: string;
				operation?: string;
			};
			program_state_update?: ProgramStateUpdateChange;
			program_state?: ProgramStateUpdateChange;
		};
		proposed_target?: string;
		risk_level?: string;
		promotion_target?: string;
		updated_at?: string;
	}

	// Phase 4.1 auto-apply / revert activity feed entry. Sourced from the
	// per-scope learning event log (`harness_program_state_auto_applied` and
	// `harness_program_state_reverted`).
	interface HarnessProgramStateEvent {
		id: string;
		event_type: string;
		agent_id?: string;
		summary: string;
		created_at?: string;
		payload?: {
			candidate_id?: string;
			source_agent_id?: string;
			program_relative_path?: string;
			program_section?: string;
			goal_id?: string;
			state_path?: string;
			before?: Record<string, unknown>;
			after?: Record<string, unknown>;
			touches_control_field?: boolean;
			elevated?: boolean;
			priority?: string;
			revertible?: boolean;
			reverted_event_id?: string;
		};
	}

	interface Procedure {
		id: string;
		status: string;
		title: string;
		summary?: string;
		owner_agent?: string;
		activation?: {
			use_when?: string[];
			avoid_when?: string[];
			example_goals?: string[];
		};
		workflow?: string[];
		verification?: string[];
		failure_modes?: string[];
		success_count?: number;
		failure_count?: number;
		last_used_at?: string;
		updated_at?: string;
		version?: number;
	}

	interface ProcedureSkillPromotionResponse {
		outcome?: {
			eligible?: boolean;
			reason?: string;
			target_skill?: string;
			promotion_candidate_id?: string;
			eval_candidate_id?: string;
			eval_backlog_candidate_id?: string;
			reused_existing_candidate?: boolean;
		};
	}

	interface MemoryUndoSpec {
		key: string;
		target_tier: string;
	}

	type FunnelStageKey =
		| 'signals'
		| 'candidates'
		| 'backlog'
		| 'review'
		| 'validation'
		| 'apply'
		| 'promotion'
		| 'monitoring';

	interface FunnelStage {
		key: FunnelStageKey;
		label: string;
		count: number;
		summary: string;
	}

	type SkillEvolutionGate = 'proposal_review' | 'apply_gate' | 'promotion_gate';

	interface OperatorAction {
		id: string;
		stage: FunnelStageKey | 'rollback';
		stageLabel: string;
		title: string;
		summary: string;
		detail: string;
		actionLabel: string;
		priority: number;
		candidateId?: string;
		gate?: SkillEvolutionGate;
		disabled?: boolean;
		run: () => void | Promise<void>;
	}

	const TERMINAL_CANDIDATE_STATES = new Set(['promoted', 'rejected', 'superseded', 'archived']);

	let loading = false;
	let actionInFlight = false;
	let error: string | null = null;
	let status: string | null = null;
	let teachingAction: TeachingAction = 'remember';
	let teachingContent = '';
	let teachingCorrection = '';
	let teachingTargetKind = '';
	let teachingTargetName = '';
	let backlog: BacklogItem[] = [];
	let proposals: Proposal[] = [];
	let evaluations: Evaluation[] = [];
	let evaluationRuns: EvaluationRun[] = [];
	let growthRuns: GrowthEvaluationRun[] = [];
	let validations: Validation[] = [];
	let implementations: Implementation[] = [];
	let applications: Application[] = [];
	let rollbackRecommendations: RollbackRecommendation[] = [];
	let postPromotionMonitors: PostPromotionMonitor[] = [];
	let promotions: Promotion[] = [];
	let candidates: Candidate[] = [];
	let procedures: Procedure[] = [];
	let programStateAutoApplies: HarnessProgramStateEvent[] = [];
	let programStateRevertedEventIds = new Set<string>();
	let applyTargetSurface: 'scoped_skill' | 'system_skill' | 'source_skill' = 'scoped_skill';
	let highlightedRollbackId: string | null = null;
	let highlightedPostPromotionMonitorId: string | null = null;
	let highlightedCandidateId: string | null = null;
	let highlightedSkillEvolutionGate: SkillEvolutionGate | null = null;

	async function readApiError(response: Response): Promise<string> {
		const text = await response.text().catch(() => '');
		if (!text) return `Request failed (${response.status})`;
		try {
			const parsed = JSON.parse(text) as { error?: string; message?: string };
			return parsed.error || parsed.message || text;
		} catch {
			return text;
		}
	}

	async function fetchJson<T>(path: string, init?: RequestInit): Promise<T> {
		const response = await timedFetch(path, init);
		if (!response.ok) throw new Error(await readApiError(response));
		return (await response.json()) as T;
	}

	async function refresh(): Promise<void> {
		loading = true;
		error = null;
		try {
			const [
				backlogPayload,
				proposalPayload,
				evaluationPayload,
				evaluationRunsPayload,
				validationPayload,
				implementationPayload,
				applicationPayload,
				rollbackRecommendationPayload,
				postPromotionMonitorPayload,
				promotionPayload,
				growthRunsPayload,
				candidatesPayload,
				proceduresPayload,
				learningEventsPayload
			] = await Promise.all([
				fetchJson<{ capability_evolution?: BacklogItem[] }>(
					'/api/magician/v2/learning/skill-evolution?limit=25'
				),
				fetchJson<{ proposals?: Proposal[] }>(
					'/api/magician/v2/learning/skill-evolution/proposals?limit=25'
				),
				fetchJson<{ evaluations?: Evaluation[] }>('/api/magician/v2/learning/evaluations?limit=25'),
				fetchJson<{ runs?: EvaluationRun[] }>('/api/magician/v2/learning/evaluations/runs?limit=25'),
				fetchJson<{ validations?: Validation[] }>(
					'/api/magician/v2/learning/skill-evolution/validations?limit=25'
				),
				fetchJson<{ implementations?: Implementation[] }>(
					'/api/magician/v2/learning/skill-evolution/implementations?limit=25'
				),
				fetchJson<{ applications?: Application[] }>(
					'/api/magician/v2/learning/skill-evolution/applications?limit=25'
				),
				fetchJson<{ rollback_recommendations?: RollbackRecommendation[] }>(
					'/api/magician/v2/learning/skill-evolution/rollback-recommendations?limit=25'
				),
				fetchJson<{ post_promotion_monitors?: PostPromotionMonitor[] }>(
					'/api/magician/v2/learning/skill-evolution/post-promotion-monitors?limit=25'
				),
				fetchJson<{ promotions?: Promotion[] }>(
					'/api/magician/v2/learning/skill-evolution/promotions?limit=25'
				),
				fetchJson<{ runs?: GrowthEvaluationRun[] }>(
					'/api/magician/v2/learning/growth-evaluations?limit=10'
				),
				fetchJson<{ candidates?: Candidate[] }>('/api/magician/v2/learning/candidates?limit=25'),
				fetchJson<{ procedures?: Procedure[] }>('/api/magician/v2/learning/procedures?limit=25'),
				fetchJson<{ events?: HarnessProgramStateEvent[] }>(
					'/api/magician/v2/learning/events?max_lines=200'
				)
			]);
			backlog = backlogPayload.capability_evolution ?? [];
			proposals = proposalPayload.proposals ?? [];
			evaluations = evaluationPayload.evaluations ?? [];
			evaluationRuns = evaluationRunsPayload.runs ?? [];
			validations = validationPayload.validations ?? [];
			implementations = implementationPayload.implementations ?? [];
			applications = applicationPayload.applications ?? [];
			rollbackRecommendations = rollbackRecommendationPayload.rollback_recommendations ?? [];
			postPromotionMonitors = postPromotionMonitorPayload.post_promotion_monitors ?? [];
			promotions = promotionPayload.promotions ?? [];
			growthRuns = growthRunsPayload.runs ?? [];
			candidates = candidatesPayload.candidates ?? [];
			procedures = proceduresPayload.procedures ?? [];
			const learningEvents = learningEventsPayload.events ?? [];
			programStateAutoApplies = learningEvents.filter(
				(event) => event.event_type === 'harness_program_state_auto_applied'
			);
			programStateRevertedEventIds = new Set(
				learningEvents
					.filter((event) => event.event_type === 'harness_program_state_reverted')
					.map((event) => event.payload?.reverted_event_id)
					.filter((id): id is string => typeof id === 'string' && id.length > 0)
			);
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			loading = false;
		}
	}

	async function submitTeaching(): Promise<void> {
		if (!teachingContent.trim()) return;
		actionInFlight = true;
		error = null;
		status = null;
		try {
			const target =
				teachingTargetKind.trim() || teachingTargetName.trim()
					? {
							kind: teachingTargetKind.trim() || undefined,
							name: teachingTargetName.trim() || undefined
						}
					: undefined;
			const payload = await fetchJson<{
				event?: { id?: string };
				candidates?: Candidate[];
				durable_change_count?: number;
				review_required_count?: number;
			}>('/api/magician/v2/learning/teaching', {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({
					action: teachingAction,
					content: teachingContent,
					correction: teachingCorrection.trim() || undefined,
					target,
					payload: {
						source: 'learning_operator_panel'
					}
				})
			});
			status = `Teaching recorded${payload.event?.id ? ` (${payload.event.id})` : ''}; durable ${payload.durable_change_count ?? 0}, review ${payload.review_required_count ?? 0}`;
			teachingContent = '';
			teachingCorrection = '';
			await refresh();
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			actionInFlight = false;
		}
	}

	async function undoPromotedMemory(candidate: Candidate): Promise<void> {
		const spec = promotedMemorySpec(candidate);
		if (!spec) return;
		actionInFlight = true;
		error = null;
		status = null;
		try {
			const payload = await fetchJson<{
				event?: { id?: string };
				durable_change_count?: number;
				review_required_count?: number;
			}>('/api/magician/v2/learning/teaching', {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({
					action: 'forget',
					content: `Undo promoted memory ${spec.key}`,
					target: {
						kind: 'memory',
						name: spec.key,
						summary: candidate.title
					},
					payload: {
						source: 'learning_operator_panel_undo',
						key: spec.key,
						target_tier: spec.target_tier,
						undo_candidate_id: candidate.id
					}
				})
			});
			status = `Forget recorded${payload.event?.id ? ` (${payload.event.id})` : ''}; durable ${payload.durable_change_count ?? 0}, review ${payload.review_required_count ?? 0}`;
			await refresh();
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			actionInFlight = false;
		}
	}

	async function transitionProcedure(procedure: Procedure, toStatus: string): Promise<void> {
		if (procedure.status === toStatus) return;
		actionInFlight = true;
		error = null;
		status = null;
		try {
			const payload = await fetchJson<Procedure>(
				`/api/magician/v2/learning/procedures/${encodeURIComponent(procedure.id)}/status`,
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({
						to_status: toStatus,
						actor: 'operator',
						decision: `procedure_status_${toStatus}`,
						reason: `Procedure moved to ${toStatus} from the learning operator panel.`
					})
				}
			);
			status = `Procedure ${payload.id} moved to ${payload.status}`;
			await refresh();
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			actionInFlight = false;
		}
	}

	async function promoteProcedureToSkill(procedure: Procedure): Promise<void> {
		actionInFlight = true;
		error = null;
		status = null;
		try {
			const payload = await fetchJson<ProcedureSkillPromotionResponse>(
				`/api/magician/v2/learning/procedures/${encodeURIComponent(procedure.id)}/skill-promotion`,
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({
						actor: 'operator',
						reason: 'Operator requested review-gated procedure-to-skill promotion from the learning panel.'
					})
				}
			);
			const outcome = payload.outcome;
			if (!outcome?.eligible) {
				status = `Procedure ${procedure.id} is not ready for skill promotion: ${outcome?.reason ?? 'not eligible'}`;
			} else if (outcome.reused_existing_candidate) {
				status = `Reused skill promotion candidate ${outcome.promotion_candidate_id ?? 'unknown'} for ${outcome.target_skill ?? 'target skill'}`;
			} else {
				status = `Created skill promotion candidate ${outcome.promotion_candidate_id ?? 'unknown'} and eval backlog ${outcome.eval_backlog_candidate_id ?? outcome.eval_candidate_id ?? 'pending'}`;
			}
			await refresh();
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			actionInFlight = false;
		}
	}

	async function draftQueued(): Promise<void> {
		actionInFlight = true;
		error = null;
		status = null;
		try {
			const payload = await fetchJson<{ drafted_count?: number; skipped_count?: number }>(
				'/api/magician/v2/learning/skill-evolution/proposals/draft',
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({ limit: 10, ready_for_review: true })
				}
			);
			status = `Drafted ${payload.drafted_count ?? 0}; skipped ${payload.skipped_count ?? 0}`;
			await refresh();
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			actionInFlight = false;
		}
	}

	async function generateEvaluation(candidateId: string): Promise<void> {
		actionInFlight = true;
		error = null;
		status = null;
		try {
			const payload = await fetchJson<{ evaluation?: Evaluation }>(
				`/api/magician/v2/learning/skill-evolution/proposals/${encodeURIComponent(candidateId)}/evaluation/generate`,
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({
						actor: 'operator',
						overwrite: true,
						payload: {
							source: 'memory_skill_evolution_panel',
							generated_at: new Date().toISOString()
						}
					})
				}
			);
			status = `Eval case ${payload.evaluation?.id ?? ''} generated`;
			await refresh();
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			actionInFlight = false;
		}
	}

	async function approveProposal(proposal: Proposal): Promise<void> {
		actionInFlight = true;
		error = null;
		status = null;
		try {
			await fetchJson(
				`/api/magician/v2/learning/skill-evolution/proposals/${encodeURIComponent(proposal.candidate_id)}/decision`,
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({
						status: 'approved',
						actor: 'operator',
						reason: 'Approved from the skill-evolution operator panel.',
						payload: {
							source: 'memory_skill_evolution_panel',
							reviewed_at: new Date().toISOString()
						}
					})
				}
			);
			status = `Approved ${proposal.candidate_id}`;
			await refresh();
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			actionInFlight = false;
		}
	}

	async function rejectProposal(proposal: Proposal): Promise<void> {
		actionInFlight = true;
		error = null;
		status = null;
		try {
			await fetchJson(
				`/api/magician/v2/learning/skill-evolution/proposals/${encodeURIComponent(proposal.candidate_id)}/decision`,
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({
						status: 'rejected',
						actor: 'operator',
						reason: 'Rejected from the skill-evolution operator panel.',
						payload: {
							source: 'memory_skill_evolution_panel',
							reviewed_at: new Date().toISOString()
						}
					})
				}
			);
			status = `Rejected ${proposal.candidate_id}`;
			await refresh();
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			actionInFlight = false;
		}
	}

	async function revertProgramState(autoApply: HarnessProgramStateEvent): Promise<void> {
		actionInFlight = true;
		error = null;
		status = null;
		try {
			const payload = await fetchJson<{
				reverted_event_id?: string;
				state_path?: string;
				history_growth?: number;
			}>('/api/magician/v2/harness/program-state/revert', {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({
					event_id: autoApply.id,
					reason: 'Reverted from the skill-evolution operator panel.'
				})
			});
			status = `Reverted ${payload.reverted_event_id ?? autoApply.id}${
				payload.state_path ? ` on ${payload.state_path}` : ''
			}`;
			await refresh();
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			actionInFlight = false;
		}
	}

	async function runValidation(candidateId: string): Promise<void> {
		actionInFlight = true;
		error = null;
		status = null;
		try {
			const payload = await fetchJson<{ validation?: Validation }>(
				`/api/magician/v2/learning/skill-evolution/proposals/${encodeURIComponent(candidateId)}/validation/run`,
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({ include_regression: true, timeout_seconds: 120 })
				}
			);
			status = `Validation ${payload.validation?.id ?? ''} recorded as ${payload.validation?.status ?? 'unknown'}`;
			await refresh();
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			actionInFlight = false;
		}
	}

	async function runEvaluation(candidateId: string): Promise<void> {
		actionInFlight = true;
		error = null;
		status = null;
		try {
			const payload = await fetchJson<{ run?: EvaluationRun }>(
				`/api/magician/v2/learning/evaluations/${encodeURIComponent(candidateId)}/run`,
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({ include_regression: true, timeout_seconds: 120 })
				}
			);
			status = `Eval run ${payload.run?.id ?? ''} recorded as ${payload.run?.status ?? 'unknown'}`;
			await refresh();
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			actionInFlight = false;
		}
	}

	async function runGrowthEvaluation(): Promise<void> {
		actionInFlight = true;
		error = null;
		status = null;
		try {
			const payload = await fetchJson<{ run?: GrowthEvaluationRun }>(
				'/api/magician/v2/learning/growth-evaluations/run',
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({
						suite_id: 'agent_growth_phase9',
						window_days: 30,
						payload: {
							source: 'memory_skill_evolution_panel',
							requested_at: new Date().toISOString()
						}
					})
				}
			);
			status = `Growth eval ${payload.run?.id ?? ''} recorded as ${payload.run?.status ?? 'unknown'}`;
			await refresh();
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			actionInFlight = false;
		}
	}

	async function recordImplementation(proposal: Proposal): Promise<void> {
		const validation = latestPassedValidation(proposal.candidate_id);
		if (!validation) return;
		actionInFlight = true;
		error = null;
		status = null;
		try {
			const payload = await fetchJson<{ implementation?: Implementation }>(
				`/api/magician/v2/learning/skill-evolution/proposals/${encodeURIComponent(proposal.candidate_id)}/implementation`,
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({
						actor: 'operator',
						summary: 'Implementation bundle recorded from reviewed proposal.',
						validation_id: validation.id,
						applied_files: proposal.proposed_files ?? [],
						patches: proposal.patches ?? [],
						payload: {
							source: 'memory_skill_evolution_panel',
							recorded_at: new Date().toISOString()
						}
					})
				}
			);
			status = `Implementation ${payload.implementation?.id ?? ''} recorded`;
			await refresh();
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			actionInFlight = false;
		}
	}

	async function applyImplementation(implementation: Implementation, apply: boolean): Promise<void> {
		actionInFlight = true;
		error = null;
		status = null;
		try {
			const payload = await fetchJson<{ application?: Application }>(
				`/api/magician/v2/learning/skill-evolution/implementations/${encodeURIComponent(implementation.candidate_id)}/${encodeURIComponent(implementation.id)}/apply`,
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({
						actor: 'operator',
						apply,
						target_surface: applyTargetSurface,
						summary: apply
							? `Applied reviewed implementation to ${applyTargetSurface}.`
							: `Prepared dry-run for reviewed implementation against ${applyTargetSurface}.`,
						payload: {
							source: 'memory_skill_evolution_panel',
							requested_at: new Date().toISOString()
						}
					})
				}
			);
			status = `Application ${payload.application?.id ?? ''} ${payload.application?.status ?? 'recorded'}`;
			await refresh();
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			actionInFlight = false;
		}
	}

	async function promoteProposal(proposal: Proposal): Promise<void> {
		const validation = latestPassedValidation(proposal.candidate_id);
		const implementation = latestImplementation(proposal.candidate_id);
		const application = latestAppliedApplication(proposal.candidate_id);
		if (!validation) return;
		actionInFlight = true;
		error = null;
		status = null;
		try {
			const payload = await fetchJson<{ promotion?: Promotion }>(
				`/api/magician/v2/learning/skill-evolution/proposals/${encodeURIComponent(proposal.candidate_id)}/promotion`,
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({
						actor: 'operator',
						summary: 'Promoted reviewed skill/tool-pack evolution after validation evidence.',
						validation_id: validation.id,
						implementation_id: implementation?.id,
						application_id: application?.id,
						applied_files: proposal.proposed_files ?? [],
						payload: {
							source: 'memory_skill_evolution_panel',
							promoted_at: new Date().toISOString()
						}
					})
				}
			);
			status = `Promotion ${payload.promotion?.id ?? ''} recorded`;
			await refresh();
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			actionInFlight = false;
		}
	}

	async function decideRollbackRecommendation(
		recommendation: RollbackRecommendation,
		nextStatus: Exclude<RollbackRecommendationStatus, 'recommended'>
	): Promise<void> {
		actionInFlight = true;
		error = null;
		status = null;
		try {
			const payload = await fetchJson<{ rollback_recommendation?: RollbackRecommendation }>(
				`/api/magician/v2/learning/skill-evolution/rollback-recommendations/${encodeURIComponent(recommendation.candidate_id)}/${encodeURIComponent(recommendation.id)}/decision`,
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({
						status: nextStatus,
						actor: 'operator',
						summary:
							nextStatus === 'dismissed'
								? 'Operator dismissed the rollback recommendation after review.'
								: 'Operator superseded the rollback recommendation after a newer review path.',
						payload: {
							source: 'skill_evolution_panel',
							recommendation_id: recommendation.id,
							decided_at: new Date().toISOString()
						}
					})
				}
			);
			status = `Rollback recommendation ${payload.rollback_recommendation?.status ?? nextStatus}`;
			await refresh();
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			actionInFlight = false;
		}
	}

	async function runPostPromotionMonitor(promotion: Promotion): Promise<void> {
		actionInFlight = true;
		error = null;
		status = null;
		try {
			const payload = await fetchJson<{ post_promotion_monitor?: PostPromotionMonitor }>(
				`/api/magician/v2/learning/skill-evolution/promotions/${encodeURIComponent(promotion.id)}/monitor`,
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({
						actor: 'operator',
						payload: {
							source: 'skill_evolution_panel',
							run_at: new Date().toISOString()
						}
					})
				}
			);
			status = `Post-promotion monitor ${payload.post_promotion_monitor?.status ?? 'recorded'}`;
			await refresh();
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			actionInFlight = false;
		}
	}

	function formatCount(value: number): string {
		return new Intl.NumberFormat().format(value);
	}

	function formatRankScore(value: number | undefined): string {
		if (typeof value !== 'number' || !Number.isFinite(value)) return '0';
		return value.toFixed(value >= 10 ? 0 : 1);
	}

	function formatOwnerHint(value: string): string {
		return value.replace(/_/g, ' ');
	}

	function hasPlan(value: unknown): string {
		if (!value) return 'missing';
		if (Array.isArray(value)) return value.length > 0 ? 'present' : 'empty';
		if (typeof value === 'object') return Object.keys(value as Record<string, unknown>).length > 0 ? 'present' : 'empty';
		if (typeof value === 'string') return value.trim().length > 0 ? 'present' : 'empty';
		return 'empty';
	}

	function latestPassedValidation(candidateId: string): Validation | undefined {
		return validations.find(
			(validation) => validation.candidate_id === candidateId && validation.status === 'passed'
		);
	}

	function latestEvaluation(candidateId: string): Evaluation | undefined {
		return evaluations.find((evaluation) => evaluation.candidate_id === candidateId);
	}

	function latestEvaluationRun(candidateId: string): EvaluationRun | undefined {
		return evaluationRuns.find((run) => run.candidate_id === candidateId);
	}

	function latestImplementation(candidateId: string): Implementation | undefined {
		return implementations.find((implementation) => implementation.candidate_id === candidateId);
	}

	function latestAppliedApplication(candidateId: string): Application | undefined {
		return applications.find(
			(application) => application.candidate_id === candidateId && application.status === 'applied'
		);
	}

	function latestApplication(candidateId: string): Application | undefined {
		return applications.find((application) => application.candidate_id === candidateId);
	}

	function latestPromotion(candidateId: string): Promotion | undefined {
		return promotions.find((promotion) => promotion.candidate_id === candidateId);
	}

	function monitorForPromotion(promotionId: string | undefined | null): PostPromotionMonitor | undefined {
		if (!promotionId) return undefined;
		return postPromotionMonitors.find((monitor) => monitor.promotion_id === promotionId);
	}

	function applicationForRollback(recommendation: RollbackRecommendation): Application | undefined {
		return applications.find((application) => application.id === recommendation.application_id);
	}

	function catalogRefreshLabel(application: Application): string {
		const refresh = application.payload?.runtime_catalog_refresh;
		if (!refresh) return 'catalog not refreshed';
		const names = refresh.changed_skill_names?.filter(Boolean) ?? [];
		const scopeCount =
			typeof refresh.procedure_skill_count === 'number'
				? ` · ${refresh.procedure_skill_count} procedures visible`
				: '';
		return `${refresh.status ?? 'recorded'}${names.length ? ` · ${names.join(', ')}` : ''}${scopeCount}`;
	}

	function rollbackFilesLabel(recommendation: RollbackRecommendation): string {
		const files = recommendation.rollback_files?.filter(Boolean) ?? [];
		if (files.length === 0) return 'no files listed';
		if (files.length === 1) return files[0];
		return `${files.length} files · ${files.slice(0, 2).join(', ')}${files.length > 2 ? '…' : ''}`;
	}

	function successRateLabel(value: number | null | undefined): string {
		if (typeof value !== 'number' || !Number.isFinite(value)) return 'n/a';
		return `${Math.round(value * 100)}%`;
	}

	function programStateChange(candidate: Candidate): ProgramStateUpdateChange | undefined {
		return candidate.proposed_change?.program_state_update ?? candidate.proposed_change?.program_state;
	}

	function programStateDoc(candidate: Candidate): string {
		const change = programStateChange(candidate);
		return (
			change?.program_relative_path ||
			change?.relative_path ||
			change?.program_path ||
			change?.program ||
			candidate.proposed_target ||
			'unresolved program doc'
		);
	}

	function programStatePatch(candidate: Candidate): Record<string, unknown> | undefined {
		const change = programStateChange(candidate);
		return change?.patch ?? change?.state_patch ?? change?.update;
	}

	function fieldKeysLabel(patch: Record<string, unknown> | undefined): string {
		const keys = patch ? Object.keys(patch) : [];
		return keys.length ? keys.join(', ') : 'no fields';
	}

	function changedFieldKeys(
		before: Record<string, unknown> | undefined,
		after: Record<string, unknown> | undefined
	): string[] {
		const keys = new Set<string>();
		for (const key of Object.keys(before ?? {})) keys.add(key);
		for (const key of Object.keys(after ?? {})) keys.add(key);
		return [...keys];
	}

	function formatFieldValue(value: unknown): string {
		if (value === undefined || value === null) return '—';
		if (typeof value === 'string') return value.length > 120 ? `${value.slice(0, 117)}…` : value;
		return JSON.stringify(value);
	}

	function debugSnapshotJson(): string {
		return JSON.stringify(
			{
				backlog,
				proposals,
				evaluations,
				evaluationRuns,
				validations,
				implementations,
				applications,
				promotions,
				rollbackRecommendations,
				postPromotionMonitors,
				growthRuns,
				candidates,
				procedures
			},
			null,
			2
		);
	}

	function hasScopedSkillTarget(proposal: Proposal): boolean {
		const files = [...(proposal.proposed_files ?? []), ...(proposal.patches ?? []).map((patch) => patch.path)];
		return files.some((path) => path.startsWith('skills/'));
	}

	function canApply(implementation: Implementation | undefined): boolean {
		return Boolean(implementation && (implementation.patches?.length ?? 0) > 0);
	}

	function isTerminalCandidateState(state: string | undefined): boolean {
		return Boolean(state && TERMINAL_CANDIDATE_STATES.has(state));
	}

	function monitorNeedsOperatorAction(monitor: PostPromotionMonitor): boolean {
		if (monitor.status !== 'regression_detected') return false;
		if (monitor.rollback_recommendation_id) return false;
		const followUpId = monitor.follow_up_candidate_id;
		if (!followUpId) return true;
		const followUp = candidates.find((candidate) => candidate.id === followUpId);
		return !followUp || !isTerminalCandidateState(followUp.state);
	}

	function scrollToElementIfPresent(id: string): boolean {
		if (typeof document === 'undefined') return false;
		const element = document.getElementById(id);
		if (!element) return false;
		element.scrollIntoView({
			block: 'center',
			behavior: 'smooth'
		});
		return true;
	}

	function scrollToElement(id: string): void {
		scrollToElementIfPresent(id);
	}

	function scrollToCandidate(candidateId: string): boolean {
		for (const id of [`proposal-${candidateId}`, `backlog-${candidateId}`, `candidate-${candidateId}`]) {
			if (scrollToElementIfPresent(id)) return true;
		}
		return false;
	}

	function operatorActionIdForGate(candidateId: string, gate: SkillEvolutionGate): string {
		if (gate === 'proposal_review') return `approve-${candidateId}`;
		if (gate === 'apply_gate') return `apply-${candidateId}`;
		return `promote-${candidateId}`;
	}

	function isSkillEvolutionGate(value: string | null): value is SkillEvolutionGate {
		return value === 'proposal_review' || value === 'apply_gate' || value === 'promotion_gate';
	}

	function operatorActionHighlighted(action: OperatorAction): boolean {
		return Boolean(
			highlightedCandidateId &&
				highlightedSkillEvolutionGate &&
				action.candidateId === highlightedCandidateId &&
				action.gate === highlightedSkillEvolutionGate
		);
	}

	function buildFunnelStages(): FunnelStage[] {
		const passedValidationCandidates = new Set(
			validations
				.filter((validation) => validation.status === 'passed')
				.map((validation) => validation.candidate_id)
		);
		return [
			{
				key: 'signals',
				label: 'Signals',
				count: candidates.length,
				summary: `${formatCount(candidates.filter((candidate) => !isTerminalCandidateState(candidate.state)).length)} open`
			},
			{
				key: 'candidates',
				label: 'Candidates',
				count: candidates.filter((candidate) => !isTerminalCandidateState(candidate.state)).length,
				summary: `${formatCount(candidates.filter((candidate) => candidate.state === 'triaged').length)} triaged`
			},
			{
				key: 'backlog',
				label: 'Backlog',
				count: backlog.length,
				summary: `${formatCount(backlog.filter((item) => item.status === 'queued').length)} queued`
			},
			{
				key: 'review',
				label: 'Review',
				count: proposals.filter((proposal) => proposal.status === 'ready_for_review').length,
				summary: `${formatCount(proposals.filter((proposal) => proposal.status === 'approved').length)} approved`
			},
			{
				key: 'validation',
				label: 'Validated',
				count: passedValidationCandidates.size,
				summary: `${formatCount(validations.filter((validation) => validation.status !== 'passed').length)} blocked`
			},
			{
				key: 'apply',
				label: 'Applied',
				count: applications.filter((application) => application.status === 'applied').length,
				summary: `${formatCount(implementations.length)} bundles`
			},
			{
				key: 'promotion',
				label: 'Promoted',
				count: promotions.length,
				summary: `${formatCount(promotions.filter((promotion) => !monitorForPromotion(promotion.id)).length)} unmonitored`
			},
			{
				key: 'monitoring',
				label: 'Monitored',
				count: postPromotionMonitors.length,
				summary: `${formatCount(postPromotionMonitors.filter(monitorNeedsOperatorAction).length)} needs action`
			}
		];
	}

	function buildOperatorActions(): OperatorAction[] {
		const actions: OperatorAction[] = [];
		for (const recommendation of activeRollbackRecommendations.slice(0, 3)) {
			actions.push({
				id: `rollback-${recommendation.id}`,
				stage: 'rollback',
				stageLabel: 'Rollback',
				title: 'Review rollback recommendation',
				summary: recommendation.summary,
				detail: `${recommendation.severity} · ${rollbackFilesLabel(recommendation)}`,
				actionLabel: 'Jump to row',
				priority: 5,
				run: () => scrollToElement(`rollback-${recommendation.id}`)
			});
		}
		for (const monitor of postPromotionMonitors.filter(monitorNeedsOperatorAction).slice(0, 3)) {
			const promotion = promotions.find((entry) => entry.id === monitor.promotion_id);
			actions.push({
				id: `monitor-${monitor.promotion_id}`,
				stage: 'monitoring',
				stageLabel: 'Monitor',
				title: 'Review post-promotion regression',
				summary: monitor.summary,
				detail: `${monitor.same_failure_recurrence_count} repeats · ${monitor.new_failure_classes?.length ?? 0} new failure classes`,
				actionLabel: promotion ? 'Refresh monitor' : 'Jump to row',
				priority: 10,
				run: () => (promotion ? runPostPromotionMonitor(promotion) : scrollToElement(`post-promotion-monitor-${monitor.promotion_id}`))
			});
		}
		const backlogWithoutProposal = backlog.find(
			(item) => !proposals.some((proposal) => proposal.candidate_id === item.candidate_id)
		);
		if (backlogWithoutProposal) {
			actions.push({
				id: `draft-${backlogWithoutProposal.candidate_id}`,
				stage: 'backlog',
				stageLabel: 'Backlog',
				title: 'Draft proposal for queued evidence',
				summary: backlogWithoutProposal.title,
				detail: backlogWithoutProposal.rank_reasons?.[0] ?? `${backlogWithoutProposal.risk_level ?? 'unknown'} risk`,
				actionLabel: 'Draft queued',
				priority: 20,
				run: draftQueued
			});
		}
		for (const proposal of proposals) {
			const validation = latestPassedValidation(proposal.candidate_id);
			const evaluation = latestEvaluation(proposal.candidate_id);
			const implementation = latestImplementation(proposal.candidate_id);
			const application = latestApplication(proposal.candidate_id);
			const appliedApplication = latestAppliedApplication(proposal.candidate_id);
			const promotion = latestPromotion(proposal.candidate_id);
			if (proposal.status === 'ready_for_review') {
				actions.push({
					id: `approve-${proposal.candidate_id}`,
					stage: 'review',
					stageLabel: 'Review',
					title: 'Approve reviewed proposal',
					summary: proposal.title,
					detail: `${proposal.capability_id ?? 'unknown target'} · gate ${hasPlan(proposal.promotion_gate)}`,
					actionLabel: 'Approve',
					priority: 30,
					candidateId: proposal.candidate_id,
					gate: 'proposal_review',
					run: () => approveProposal(proposal)
				});
				continue;
			}
			if (proposal.status !== 'approved') continue;
			if (!evaluation && hasPlan(proposal.eval_plan) !== 'missing') {
				actions.push({
					id: `eval-${proposal.candidate_id}`,
					stage: 'validation',
					stageLabel: 'Eval',
					title: 'Generate eval case',
					summary: proposal.title,
					detail: 'Proposal has an eval plan but no eval case yet.',
					actionLabel: 'Eval case',
					priority: 40,
					run: () => generateEvaluation(proposal.candidate_id)
				});
				continue;
			}
			if (!validation) {
				actions.push({
					id: `validate-${proposal.candidate_id}`,
					stage: 'validation',
					stageLabel: 'Validate',
					title: 'Run validation gate',
					summary: proposal.title,
					detail: `validation plan ${hasPlan(proposal.validation_plan)}`,
					actionLabel: 'Validate',
					priority: 50,
					run: () => runValidation(proposal.candidate_id)
				});
				continue;
			}
			if (!implementation) {
				actions.push({
					id: `bundle-${proposal.candidate_id}`,
					stage: 'apply',
					stageLabel: 'Bundle',
					title: 'Prepare implementation bundle',
					summary: proposal.title,
					detail: `${proposal.patches?.length ?? 0} reviewed patches`,
					actionLabel: 'Bundle',
					priority: 60,
					run: () => recordImplementation(proposal)
				});
				continue;
			}
			if (canApply(implementation) && !appliedApplication) {
				actions.push({
					id: `apply-${proposal.candidate_id}`,
					stage: 'apply',
					stageLabel: application ? 'Apply' : 'Dry-run',
					title: application ? 'Apply prepared implementation' : 'Dry-run implementation',
					summary: proposal.title,
					detail: application ? catalogRefreshLabel(application) : `${implementation.patches?.length ?? 0} patches ready`,
					actionLabel: application ? 'Apply' : 'Dry-run',
					priority: 70,
					candidateId: proposal.candidate_id,
					gate: 'apply_gate',
					run: () => applyImplementation(implementation, Boolean(application))
				});
				continue;
			}
			if (!promotion) {
				actions.push({
					id: `promote-${proposal.candidate_id}`,
					stage: 'promotion',
					stageLabel: 'Promote',
					title: 'Promote validated skill change',
					summary: proposal.title,
					detail: appliedApplication ? catalogRefreshLabel(appliedApplication) : 'no scoped apply required',
					actionLabel: 'Promote',
					priority: 80,
					candidateId: proposal.candidate_id,
					gate: 'promotion_gate',
					disabled: hasScopedSkillTarget(proposal) && !appliedApplication,
					run: () => promoteProposal(proposal)
				});
				continue;
			}
			if (!monitorForPromotion(promotion.id)) {
				actions.push({
					id: `monitor-promotion-${promotion.id}`,
					stage: 'monitoring',
					stageLabel: 'Monitor',
					title: 'Run post-promotion monitor',
					summary: proposal.title,
					detail: promotion.id,
					actionLabel: 'Monitor',
					priority: 90,
					run: () => runPostPromotionMonitor(promotion)
				});
			}
		}
		return actions.sort((left, right) => left.priority - right.priority).slice(0, 8);
	}

	function promotedMemorySpec(candidate: Candidate): MemoryUndoSpec | null {
		if (candidate.state !== 'promoted' || !candidate.candidate_type.startsWith('memory_')) return null;
		const memory = candidate.proposed_change?.memory;
		const key = memory?.key?.trim();
		if (!key || memory?.operation === 'remove') return null;
		const targetTier =
			memory?.target_tier?.trim() ||
			candidate.promotion_target?.replace(/^user\./, '').split('.')[0] ||
			'knowledge';
		return { key, target_tier: targetTier };
	}

	function procedureActivationSummary(procedure: Procedure): string {
		const useWhen = procedure.activation?.use_when?.filter(Boolean) ?? [];
		if (useWhen.length > 0) return useWhen.slice(0, 2).join(' · ');
		return procedure.summary || 'No activation summary recorded';
	}

	function rollbackRecommendationParam(): string | null {
		if (typeof window === 'undefined') return null;
		return new URLSearchParams(window.location.search).get('rollback_recommendation');
	}

	function postPromotionMonitorParam(): string | null {
		if (typeof window === 'undefined') return null;
		return new URLSearchParams(window.location.search).get('post_promotion_monitor');
	}

	function candidateIdParam(): string | null {
		if (typeof window === 'undefined') return null;
		return new URLSearchParams(window.location.search).get('candidate_id');
	}

	function skillEvolutionGateParam(): SkillEvolutionGate | null {
		if (typeof window === 'undefined') return null;
		const gate = new URLSearchParams(window.location.search).get('skill_evolution_gate');
		return isSkillEvolutionGate(gate) ? gate : null;
	}

	$: approvedRunnable = proposals.filter((proposal) => proposal.status === 'approved');
	$: latestGrowthRun = growthRuns[0];
	$: activeRollbackRecommendations = rollbackRecommendations.filter(
		(recommendation) => recommendation.status === 'recommended'
	);
	$: regressionMonitors = postPromotionMonitors.filter(
		(monitor) => monitor.status === 'regression_detected'
	);
	$: funnelStages = buildFunnelStages();
	$: operatorActions = buildOperatorActions();
	$: gatedProgramStateCandidates = candidates.filter(
		(candidate) =>
			candidate.candidate_type === 'program_state_update' &&
			!isTerminalCandidateState(candidate.state)
	);

	onMount(() => {
		const rollbackId = rollbackRecommendationParam();
		const postPromotionMonitorId = postPromotionMonitorParam();
		const candidateId = candidateIdParam();
		const skillEvolutionGate = skillEvolutionGateParam();
		highlightedRollbackId = rollbackId;
		highlightedPostPromotionMonitorId = postPromotionMonitorId;
		highlightedCandidateId = candidateId;
		highlightedSkillEvolutionGate = skillEvolutionGate;
		void refresh().then(() => {
			const targetId = rollbackId
				? `rollback-${rollbackId}`
				: postPromotionMonitorId
					? `post-promotion-monitor-${postPromotionMonitorId}`
					: candidateId
						? `proposal-${candidateId}`
						: null;
			if (!targetId) return;
			window.setTimeout(() => {
				if (candidateId && skillEvolutionGate) {
					const actionId = operatorActionIdForGate(candidateId, skillEvolutionGate);
					if (scrollToElementIfPresent(`operator-action-${actionId}`)) return;
				}
				if (candidateId && !rollbackId && !postPromotionMonitorId) {
					scrollToCandidate(candidateId);
					return;
				}
				scrollToElement(targetId);
			}, 0);
		});
	});
</script>

<section class="learning-evolution">
	<header class="learning-evolution-header">
		<div>
			<h2>Skill evolution</h2>
			<p>Backlog, proposal review, validation evidence, and promotion readiness for scoped skill changes.</p>
		</div>
		<div class="learning-evolution-actions">
			<label class="learning-select-label">
				Apply target
				<select bind:value={applyTargetSurface}>
					<option value="scoped_skill">Scoped</option>
					<option value="source_skill">Source</option>
				</select>
			</label>
			<button type="button" on:click={draftQueued} disabled={actionInFlight}>
				{actionInFlight ? 'Working...' : 'Draft queued'}
			</button>
			<button type="button" on:click={runGrowthEvaluation} disabled={actionInFlight}>
				Run growth eval
			</button>
			<button type="button" on:click={refresh} disabled={loading}>Refresh</button>
		</div>
	</header>

	{#if status}
		<p class="learning-status">{status}</p>
	{/if}
	{#if error}
		<p class="learning-status learning-status-error">{error}</p>
	{/if}

	<form class="teaching-panel" on:submit|preventDefault={submitTeaching}>
		<div>
			<h3>Teach or correct</h3>
			<p>Explicit feedback becomes a learning event and a reviewable candidate.</p>
		</div>
		<div class="teaching-controls">
			<label>
				Action
				<select bind:value={teachingAction}>
					<option value="remember">Remember</option>
					<option value="forget">Forget</option>
					<option value="correct">Correct</option>
					<option value="make_reusable">Make reusable</option>
					<option value="improve_tool">Improve tool</option>
					<option value="never_do_this">Never do this</option>
					<option value="this_was_useful">This was useful</option>
					<option value="this_was_wrong">This was wrong</option>
				</select>
			</label>
			<label>
				Target kind
				<input bind:value={teachingTargetKind} placeholder="memory, tool, task" />
			</label>
			<label>
				Target name
				<input bind:value={teachingTargetName} placeholder="optional" />
			</label>
		</div>
		<textarea
			bind:value={teachingContent}
			rows="3"
			placeholder="What should the system remember, forget, correct, reuse, or evaluate?"
		></textarea>
		<textarea
			bind:value={teachingCorrection}
			rows="2"
			placeholder="Correction or expected behavior, if relevant"
		></textarea>
		<div class="teaching-actions">
			<button type="submit" disabled={actionInFlight || !teachingContent.trim()}>
				{actionInFlight ? 'Recording...' : 'Record teaching'}
			</button>
		</div>
	</form>

	<section class="learning-kpis">
		<div class="learning-kpi">
			<span>Backlog</span>
			<strong>{formatCount(backlog.length)}</strong>
		</div>
		<div class="learning-kpi">
			<span>Proposals</span>
			<strong>{formatCount(proposals.length)}</strong>
		</div>
		<div class="learning-kpi">
			<span>Eval cases</span>
			<strong>{formatCount(evaluations.length)}</strong>
		</div>
		<div class="learning-kpi">
			<span>Eval runs</span>
			<strong>{formatCount(evaluationRuns.length)}</strong>
		</div>
		<div class="learning-kpi">
			<span>Growth evals</span>
			<strong>{formatCount(growthRuns.length)}</strong>
		</div>
		<div class="learning-kpi">
			<span>Runnable validations</span>
			<strong>{formatCount(approvedRunnable.length)}</strong>
		</div>
		<div class="learning-kpi">
			<span>Validation reports</span>
			<strong>{formatCount(validations.length)}</strong>
		</div>
		<div class="learning-kpi">
			<span>Applications</span>
			<strong>{formatCount(applications.length)}</strong>
		</div>
		<div class="learning-kpi">
			<span>Rollback reviews</span>
			<strong>{formatCount(activeRollbackRecommendations.length)}</strong>
		</div>
		<div class="learning-kpi">
			<span>Promotions</span>
			<strong>{formatCount(promotions.length)}</strong>
		</div>
		<div class="learning-kpi">
			<span>Promotion regressions</span>
			<strong>{formatCount(regressionMonitors.length)}</strong>
		</div>
		<div class="learning-kpi">
			<span>Candidates</span>
			<strong>{formatCount(candidates.length)}</strong>
		</div>
		<div class="learning-kpi">
			<span>Procedures</span>
			<strong>{formatCount(procedures.length)}</strong>
		</div>
	</section>

	<section class="learning-funnel" aria-label="Skill evolution funnel">
		{#each funnelStages as stage}
			<div class="learning-funnel-step" class:learning-funnel-alert={stage.count > 0 && (stage.key === 'review' || stage.key === 'monitoring')}>
				<span>{stage.label}</span>
				<strong>{formatCount(stage.count)}</strong>
				<small>{stage.summary}</small>
			</div>
		{/each}
	</section>

	<section class="learning-panel operator-panel">
		<div class="operator-panel-heading">
			<h3>Operator next actions</h3>
			<span>{formatCount(operatorActions.length)} open</span>
		</div>
		{#if operatorActions.length === 0}
			<p class="learning-empty">No operator action is currently queued.</p>
		{:else}
			<div class="operator-action-list">
				{#each operatorActions as action}
					<div
						class="operator-action-row"
						class:operator-action-highlight={operatorActionHighlighted(action)}
						id={`operator-action-${action.id}`}
					>
						<div>
							<strong>{action.title}</strong>
							<span>{action.summary}</span>
							<span>{action.detail}</span>
						</div>
						<span class="operator-stage-label">{action.stageLabel}</span>
						<button type="button" on:click={() => action.run()} disabled={actionInFlight || action.disabled}>
							{action.actionLabel}
						</button>
					</div>
				{/each}
			</div>
		{/if}
	</section>

	<section class="learning-panel">
		<h3>Growth evaluation suite</h3>
		{#if growthRuns.length === 0}
			<p class="learning-empty">No growth evaluation rollups recorded yet.</p>
		{:else}
			<div class="growth-summary">
				<div>
					<strong>{latestGrowthRun?.status ?? 'unknown'}</strong>
					<span>{latestGrowthRun?.summary}</span>
				</div>
				<span>{latestGrowthRun?.suite_id}</span>
				<span>{latestGrowthRun?.created_at ?? 'no timestamp'}</span>
			</div>
			{#if latestGrowthRun?.dimensions?.length}
				<div class="learning-table">
					{#each latestGrowthRun?.dimensions ?? [] as dimension}
						<div class="learning-row">
							<div>
								<strong>{dimension.dimension}</strong>
								<span>{dimension.summary}</span>
							</div>
							<span>{dimension.status}</span>
							<span>{Math.round(dimension.score * 100)}%</span>
							<span>{dimension.metrics?.case_count ?? dimension.metrics?.candidate_count ?? dimension.metrics?.call_count ?? 'evidence'}</span>
							<span>dimension</span>
						</div>
					{/each}
				</div>
			{/if}
			{#if latestGrowthRun?.scenarios?.length}
				<div class="learning-table">
					{#each latestGrowthRun?.scenarios ?? [] as scenario}
						<div class="learning-row">
							<div>
								<strong>{scenario.scenario}</strong>
								<span>{scenario.summary}</span>
							</div>
							<span>{scenario.status}</span>
							<span>{scenario.metrics?.candidate_count ?? scenario.metrics?.count ?? 'evidence'}</span>
							<span>scenario</span>
							<span>{latestGrowthRun.id}</span>
						</div>
					{/each}
				</div>
			{/if}
		{/if}
	</section>

	<section class="learning-panel">
		<h3>Harness program state</h3>
		<p>
			Gated program-state updates awaiting review, plus the bookkeeping the harness loop
			auto-applied (Phase 4.1) — each revertible in one click.
		</p>

		<h4 class="program-state-subhead">
			Review queue
			<span>{formatCount(gatedProgramStateCandidates.length)} gated</span>
		</h4>
		{#if gatedProgramStateCandidates.length === 0}
			<p class="learning-empty">No gated program-state updates awaiting review.</p>
		{:else}
			<div class="learning-table">
				{#each gatedProgramStateCandidates as candidate}
					{@const patch = programStatePatch(candidate)}
					<div class="learning-row learning-program-state-row" id={`program-state-candidate-${candidate.id}`}>
						<div>
							<strong>{candidate.title}</strong>
							<span>{candidate.id} · doc {programStateDoc(candidate)}</span>
							<span>
								fields {fieldKeysLabel(patch)}
								{#if programStateChange(candidate)?.program_section}
									· section {programStateChange(candidate)?.program_section}
								{/if}
							</span>
							<span>{programStateChange(candidate)?.reason ?? candidate.summary}</span>
							{#if patch}
								<div class="program-state-fields">
									{#each Object.entries(patch) as [field, value]}
										<span class="program-state-field">
											<strong>{field}</strong>
											<span>{formatFieldValue(value)}</span>
										</span>
									{/each}
								</div>
							{/if}
						</div>
						<span>{candidate.state}</span>
						<span>{candidate.risk_level ?? 'unknown'} risk</span>
					</div>
				{/each}
			</div>
		{/if}

		<h4 class="program-state-subhead">
			Auto-applied activity
			<span>{formatCount(programStateAutoApplies.length)} applied</span>
		</h4>
		{#if programStateAutoApplies.length === 0}
			<p class="learning-empty">No harness bookkeeping has been auto-applied yet.</p>
		{:else}
			<div class="learning-table">
				{#each programStateAutoApplies as autoApply}
					{@const reverted = programStateRevertedEventIds.has(autoApply.id)}
					{@const fields = changedFieldKeys(autoApply.payload?.before, autoApply.payload?.after)}
					<div
						class="learning-row learning-row-action learning-program-state-row"
						class:program-state-reverted={reverted}
						id={`program-state-applied-${autoApply.id}`}
					>
						<div>
							<strong>{autoApply.summary}</strong>
							<span>
								{autoApply.payload?.source_agent_id ?? autoApply.agent_id ?? 'harness'}
								· {autoApply.payload?.state_path ?? autoApply.payload?.program_relative_path ?? 'state'}
								{#if autoApply.created_at}
									· {autoApply.created_at}
								{/if}
							</span>
							<div class="program-state-fields">
								{#each fields as field}
									<span class="program-state-field">
										<strong>{field}</strong>
										<span>
											{formatFieldValue(autoApply.payload?.before?.[field])} →
											{formatFieldValue(autoApply.payload?.after?.[field])}
										</span>
									</span>
								{/each}
							</div>
						</div>
						<span>{reverted ? 'reverted' : 'applied'}</span>
						<span>
							{#if autoApply.payload?.touches_control_field}
								<span class="learning-chip program-state-control-chip">control · elevated</span>
							{:else}
								{autoApply.payload?.priority ?? 'normal'}
							{/if}
						</span>
						<span>{autoApply.payload?.candidate_id ?? autoApply.payload?.goal_id ?? autoApply.id}</span>
						<div class="learning-row-buttons">
							<button
								type="button"
								on:click={() => revertProgramState(autoApply)}
								disabled={actionInFlight || reverted || autoApply.payload?.revertible === false}
							>
								{reverted ? 'Reverted' : 'Revert'}
							</button>
						</div>
					</div>
				{/each}
			</div>
		{/if}
	</section>

	<section class="learning-panel">
		<h3>Recent learning candidates</h3>
		{#if candidates.length === 0}
			<p class="learning-empty">No learning candidates recorded yet.</p>
		{:else}
			<div class="learning-table">
				{#each candidates as candidate}
					{@const undoSpec = promotedMemorySpec(candidate)}
					<div
						class="learning-row"
						class:learning-row-highlight={highlightedCandidateId === candidate.id}
						id={`candidate-${candidate.id}`}
					>
						<div>
							<strong>{candidate.title}</strong>
							<span>{candidate.id} · {candidate.summary}</span>
							{#if undoSpec}
								<button type="button" on:click={() => undoPromotedMemory(candidate)} disabled={actionInFlight}>
									Forget {undoSpec.key}
								</button>
							{/if}
						</div>
						<span>{candidate.state}</span>
						<span>{candidate.candidate_type}</span>
						<span>{candidate.risk_level ?? 'unknown'}</span>
						<span>{candidate.promotion_target ?? 'no target'}</span>
					</div>
				{/each}
			</div>
		{/if}
	</section>

	<section class="learning-panel">
		<h3>Procedure memory</h3>
		{#if procedures.length === 0}
			<p class="learning-empty">No reusable procedures recorded yet.</p>
		{:else}
			<div class="learning-table">
				{#each procedures as procedure}
					<div class="learning-row learning-row-action">
						<div>
							<strong>{procedure.title}</strong>
							<span>{procedure.id} · {procedureActivationSummary(procedure)}</span>
							<span>
								workflow {procedure.workflow?.length ?? 0} · verify {procedure.verification?.length ?? 0} · failures {procedure.failure_modes?.length ?? 0}
							</span>
							<span>
								last used {procedure.last_used_at ?? 'never'} · updated {procedure.updated_at ?? 'unknown'}
							</span>
						</div>
						<span>{procedure.status}</span>
						<span>{procedure.owner_agent ?? 'any agent'}</span>
						<span>{procedure.success_count ?? 0} ok / {procedure.failure_count ?? 0} fail</span>
						<div class="learning-row-buttons">
							<button
								type="button"
								on:click={() => transitionProcedure(procedure, 'draft')}
								disabled={actionInFlight || procedure.status === 'draft'}
							>
								Draft
							</button>
							<button
								type="button"
								on:click={() => transitionProcedure(procedure, 'active')}
								disabled={actionInFlight || procedure.status === 'active'}
							>
								Activate
							</button>
							<button
								type="button"
								on:click={() => transitionProcedure(procedure, 'deprecated')}
								disabled={actionInFlight || procedure.status === 'deprecated'}
							>
								Deprecate
							</button>
							<button
								type="button"
								on:click={() => transitionProcedure(procedure, 'archived')}
								disabled={actionInFlight || procedure.status === 'archived'}
							>
								Archive
							</button>
							<button
								type="button"
								on:click={() => promoteProcedureToSkill(procedure)}
								disabled={actionInFlight || procedure.status !== 'active'}
							>
								Promote skill
							</button>
						</div>
					</div>
				{/each}
			</div>
		{/if}
	</section>

	<section class="learning-grid">
		<div class="learning-panel">
			<h3>Backlog</h3>
			{#if backlog.length === 0}
				<p class="learning-empty">No queued skill evolution work.</p>
			{:else}
				<div class="learning-table">
					{#each backlog as item}
						<div
							class="learning-row learning-backlog-row"
							class:learning-row-highlight={highlightedCandidateId === item.candidate_id}
							id={`backlog-${item.candidate_id}`}
						>
							<div>
								<strong>{item.title}</strong>
								<span>
									{item.candidate_id}
									{#if item.recurrence_count}
										· recurred {item.recurrence_count}x
									{/if}
								</span>
								{#if item.rank_reasons?.[0]}
									<span>{item.rank_reasons[0]}</span>
								{/if}
							</div>
							<div class="learning-score">
								<strong>{formatRankScore(item.rank_score)}</strong>
								<span>priority</span>
							</div>
							<span>{item.status}</span>
							<span>{item.capability_id ?? 'unknown'}</span>
							<div class="learning-hint-list">
								<span>{item.risk_level ?? 'unknown'} risk</span>
								{#each (item.owner_hints ?? []).slice(0, 2) as hint}
									<span class="learning-chip">{formatOwnerHint(hint)}</span>
								{/each}
							</div>
						</div>
					{/each}
				</div>
			{/if}
		</div>

		<div class="learning-panel">
			<h3>Proposals</h3>
			{#if proposals.length === 0}
				<p class="learning-empty">No proposals drafted yet.</p>
			{:else}
				<div class="learning-table">
					{#each proposals as proposal}
						{@const validation = latestPassedValidation(proposal.candidate_id)}
						{@const evaluation = latestEvaluation(proposal.candidate_id)}
						{@const implementation = latestImplementation(proposal.candidate_id)}
						{@const appliedApplication = latestAppliedApplication(proposal.candidate_id)}
						{@const promotion = latestPromotion(proposal.candidate_id)}
						{@const scopedSkill = hasScopedSkillTarget(proposal)}
						<div
							class="learning-row learning-row-action"
							class:learning-row-highlight={highlightedCandidateId === proposal.candidate_id}
							id={`proposal-${proposal.candidate_id}`}
						>
							<div>
								<strong>{proposal.title}</strong>
								<span>
									{proposal.candidate_id}
									{#if promotion}
										· promoted
									{:else if appliedApplication}
										· applied
									{:else if implementation}
										· implemented
									{:else if validation}
										· validated
									{/if}
								</span>
							</div>
							<span>{proposal.status}</span>
							<span>eval {hasPlan(proposal.eval_plan)}</span>
							<span>gate {hasPlan(proposal.promotion_gate)}</span>
							<div class="learning-row-buttons">
								<button
									type="button"
									on:click={() => approveProposal(proposal)}
									disabled={actionInFlight || proposal.status !== 'ready_for_review'}
								>
									Approve
								</button>
								<button
									type="button"
									on:click={() => rejectProposal(proposal)}
									disabled={actionInFlight || proposal.status !== 'ready_for_review'}
								>
									Reject
								</button>
								<button
									type="button"
									on:click={() => generateEvaluation(proposal.candidate_id)}
									disabled={actionInFlight || hasPlan(proposal.eval_plan) === 'missing'}
								>
									{evaluation ? 'Refresh eval' : 'Eval case'}
								</button>
								<button
									type="button"
									on:click={() => runValidation(proposal.candidate_id)}
									disabled={actionInFlight || proposal.status !== 'approved'}
								>
									Validate
								</button>
								<button
									type="button"
									on:click={() => recordImplementation(proposal)}
									disabled={actionInFlight || proposal.status !== 'approved' || !validation}
								>
									Bundle
								</button>
								<button
									type="button"
									on:click={() => implementation && applyImplementation(implementation, false)}
									disabled={actionInFlight || !canApply(implementation)}
								>
									Dry-run
								</button>
								<button
									type="button"
									on:click={() => implementation && applyImplementation(implementation, true)}
									disabled={actionInFlight || !canApply(implementation)}
								>
									Apply
								</button>
								<button
									type="button"
									on:click={() => promoteProposal(proposal)}
									disabled={
										actionInFlight ||
										proposal.status !== 'approved' ||
										!validation ||
										Boolean(promotion) ||
										(scopedSkill && !appliedApplication)
									}
								>
									Promote
								</button>
							</div>
						</div>
					{/each}
				</div>
			{/if}
		</div>
	</section>

	<section class="learning-panel">
		<h3>Recent eval cases and validation evidence</h3>
		{#if evaluations.length === 0 && evaluationRuns.length === 0 && validations.length === 0}
			<p class="learning-empty">No eval cases or validation reports recorded.</p>
		{:else}
			<div class="learning-table">
				{#each evaluations as evaluation}
					{@const run = latestEvaluationRun(evaluation.candidate_id)}
					<div class="learning-row learning-row-action">
						<div>
							<strong>{evaluation.title}</strong>
							<span>
								{evaluation.candidate_id} · {evaluation.id}
								{#if run}
									· run {run.status}
								{/if}
							</span>
						</div>
						<span>{evaluation.status}</span>
						<span>{evaluation.case_kind}</span>
						<span>{evaluation.priority}</span>
						<div class="learning-row-buttons">
							<button
								type="button"
								on:click={() => runEvaluation(evaluation.candidate_id)}
								disabled={actionInFlight || evaluation.status === 'rejected' || evaluation.status === 'archived'}
							>
								Run eval
							</button>
						</div>
					</div>
				{/each}
				{#each evaluationRuns as run}
					<div class="learning-row">
						<div>
							<strong>{run.summary}</strong>
							<span>{run.candidate_id} · {run.id}</span>
						</div>
						<span>{run.status}</span>
						<span>{run.runner}</span>
						<span>{run.commands?.length ?? 0} commands</span>
						<span>regression {run.metrics?.regression_checked ? 'yes' : 'no'}</span>
					</div>
				{/each}
				{#each validations as validation}
					<div class="learning-row">
						<div>
							<strong>{validation.summary}</strong>
							<span>{validation.candidate_id} · {validation.id}</span>
						</div>
						<span>{validation.status}</span>
						<span>{validation.runner}</span>
						<span>{validation.commands?.length ?? 0} commands</span>
						<span>regression {validation.metrics?.regression_checked ? 'yes' : 'no'}</span>
					</div>
				{/each}
			</div>
		{/if}
	</section>

	<section class="learning-grid">
		<div class="learning-panel">
			<h3>Implementation bundles</h3>
			{#if implementations.length === 0}
				<p class="learning-empty">No implementation bundles recorded.</p>
			{:else}
				<div class="learning-table">
					{#each implementations as implementation}
						<div class="learning-row">
							<div>
								<strong>{implementation.summary}</strong>
								<span>{implementation.candidate_id} · {implementation.id}</span>
							</div>
							<span>{implementation.actor}</span>
							<span>{implementation.applied_files?.length ?? 0} files</span>
							<span>{implementation.patches?.length ?? 0} patches</span>
							<span>{implementation.validation_id}</span>
						</div>
					{/each}
				</div>
			{/if}
		</div>

		<div class="learning-panel">
			<h3>Rollback recommendations</h3>
			{#if rollbackRecommendations.length === 0}
				<p class="learning-empty">No rollback recommendations recorded.</p>
			{:else}
				<div class="learning-table">
					{#each rollbackRecommendations as recommendation}
						{@const application = applicationForRollback(recommendation)}
						<div
							class="learning-row learning-row-action learning-rollback-row"
							class:learning-row-highlight={highlightedRollbackId === recommendation.id}
							id={`rollback-${recommendation.id}`}
						>
							<div>
								<strong>{recommendation.summary}</strong>
								<span>{recommendation.candidate_id} · {recommendation.id}</span>
								<span>{rollbackFilesLabel(recommendation)}</span>
								{#if application}
									<span>{catalogRefreshLabel(application)}</span>
								{/if}
							</div>
							<span>{recommendation.status}</span>
							<span>{recommendation.severity}</span>
							<span>{formatOwnerHint(recommendation.trigger_kind)}</span>
							<div class="learning-row-buttons">
								<button
									type="button"
									on:click={() => decideRollbackRecommendation(recommendation, 'dismissed')}
									disabled={actionInFlight || recommendation.status !== 'recommended'}
								>
									Dismiss
								</button>
								<button
									type="button"
									on:click={() => decideRollbackRecommendation(recommendation, 'superseded')}
									disabled={actionInFlight || recommendation.status !== 'recommended'}
								>
									Supersede
								</button>
							</div>
						</div>
					{/each}
				</div>
			{/if}
		</div>

		<div class="learning-panel">
			<h3>Post-promotion monitors</h3>
			{#if postPromotionMonitors.length === 0}
				<p class="learning-empty">No post-promotion monitors recorded.</p>
			{:else}
				<div class="learning-table">
					{#each postPromotionMonitors as monitor}
						{@const promotion = promotions.find((entry) => entry.id === monitor.promotion_id)}
						<div
							class="learning-row learning-row-action"
							class:learning-row-highlight={highlightedPostPromotionMonitorId === monitor.promotion_id}
							id={`post-promotion-monitor-${monitor.promotion_id}`}
						>
							<div>
								<strong>{monitor.summary}</strong>
								<span>{monitor.promotion_id} · {monitor.skill_names?.join(', ') || monitor.capability_id || 'unknown skill'}</span>
								<span>
									before {monitor.before_invocation_count} / after {monitor.after_invocation_count} · success {successRateLabel(monitor.before_success_rate)} -> {successRateLabel(monitor.after_success_rate)}
								</span>
								{#if monitor.rollback_recommendation_id}
									<span>rollback {monitor.rollback_recommendation_id}</span>
								{:else if monitor.follow_up_candidate_id}
									<span>follow-up {monitor.follow_up_candidate_id}</span>
								{/if}
							</div>
							<span>{monitor.status}</span>
							<span>{monitor.same_failure_recurrence_count} repeats</span>
							<span>{monitor.new_failure_classes?.length ?? 0} new failures</span>
							<div class="learning-row-buttons">
								<button
									type="button"
									on:click={() => promotion && runPostPromotionMonitor(promotion)}
									disabled={actionInFlight || !promotion}
								>
									Refresh
								</button>
							</div>
						</div>
					{/each}
				</div>
			{/if}
		</div>

		<div class="learning-panel">
			<h3>Applications and promotions</h3>
			{#if applications.length === 0 && promotions.length === 0}
				<p class="learning-empty">No applications or promotions recorded.</p>
			{:else}
				<div class="learning-table">
					{#each applications as application}
						<div class="learning-row">
							<div>
								<strong>{application.summary}</strong>
								<span>{application.candidate_id} · {application.id}</span>
								<span>{catalogRefreshLabel(application)}</span>
							</div>
							<span>{application.mode}</span>
							<span>{application.status}</span>
							<span>{application.changed_files?.length ?? 0} files</span>
							<span>{application.implementation_id}</span>
						</div>
					{/each}
					{#each promotions as promotion}
						{@const monitor = monitorForPromotion(promotion.id)}
						<div class="learning-row">
							<div>
								<strong>{promotion.summary}</strong>
								<span>{promotion.candidate_id} · {promotion.id}</span>
								<span>monitor {monitor?.status ?? 'not run'}</span>
							</div>
							<span>promotion</span>
							<span>{promotion.actor}</span>
							<span>{promotion.application_id ?? 'no application'}</span>
							<div class="learning-row-buttons">
								<button
									type="button"
									on:click={() => runPostPromotionMonitor(promotion)}
									disabled={actionInFlight}
								>
									Monitor
								</button>
							</div>
						</div>
					{/each}
				</div>
			{/if}
		</div>
	</section>

	<details class="learning-debug-json">
		<summary>Debug JSON</summary>
		<pre>{debugSnapshotJson()}</pre>
	</details>
</section>

<style>
	.learning-evolution {
		display: flex;
		flex-direction: column;
		gap: 16px;
		/* Inherit the parent page's content width (1280px on .skills-page)
		   instead of pinning to 1180px which would render narrower than every
		   other section on the surrounding page. */
		width: 100%;
		margin: 0 auto 24px;
		padding: 24px;
		background: var(--theme-color-background, var(--color-bg, #f8fafc));
		color: var(--theme-color-foreground, var(--color-text, #111827));
		border: 1px solid var(--theme-color-border, rgba(15, 23, 42, 0.1));
		border-radius: 12px;
		box-sizing: border-box;
	}

	.learning-evolution-header,
	.learning-evolution-actions {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 12px;
	}

	.learning-evolution-actions {
		align-items: center;
		justify-content: flex-end;
		flex-wrap: wrap;
	}

	h2,
	h3,
	p {
		margin: 0;
	}

	h2 {
		font-size: 1.15rem;
		font-weight: 650;
		letter-spacing: 0;
	}

	h3 {
		font-size: 0.82rem;
		font-weight: 650;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--theme-color-foreground-muted, #64748b);
	}

	p,
	.learning-empty {
		margin-top: 4px;
		color: var(--theme-color-foreground-muted, #64748b);
		font-size: 0.9rem;
	}

	button {
		border: 1px solid var(--theme-color-border, rgba(15, 23, 42, 0.14));
		background: var(--theme-color-surface, #fff);
		color: inherit;
		padding: 8px 12px;
		border-radius: 8px;
		font: inherit;
		cursor: pointer;
	}

	button:disabled {
		cursor: not-allowed;
		opacity: 0.6;
	}

	.learning-select-label {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		color: var(--theme-color-foreground-muted, #64748b);
		font-size: 0.82rem;
	}

	select {
		border: 1px solid var(--theme-color-border, rgba(15, 23, 42, 0.14));
		background: var(--theme-color-surface, #fff);
		color: var(--theme-color-foreground, inherit);
		border-radius: 8px;
		padding: 8px 10px;
		font: inherit;
	}

	input,
	textarea {
		border: 1px solid var(--theme-color-border, rgba(15, 23, 42, 0.14));
		background: var(--theme-color-surface, #fff);
		color: var(--theme-color-foreground, inherit);
		border-radius: 8px;
		padding: 9px 10px;
		font: inherit;
		box-sizing: border-box;
		width: 100%;
	}

	textarea {
		resize: vertical;
		min-height: 72px;
	}

	.learning-status {
		margin: -4px 0 0;
		color: var(--theme-color-foreground-muted, #64748b);
		font-size: 0.86rem;
	}

	.learning-status-error {
		color: var(--theme-color-danger, #b91c1c);
	}

	.learning-kpis,
	.learning-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
		gap: 14px;
	}

	.learning-funnel {
		display: grid;
		grid-template-columns: repeat(8, minmax(96px, 1fr));
		gap: 8px;
		align-items: stretch;
	}

	.learning-funnel-step {
		display: flex;
		flex-direction: column;
		gap: 3px;
		min-width: 0;
		padding: 10px;
		border: 1px solid var(--theme-color-border, rgba(15, 23, 42, 0.08));
		border-radius: 8px;
		background: var(--theme-color-surface, #fff);
	}

	.learning-funnel-step span,
	.learning-funnel-step small {
		color: var(--theme-color-foreground-muted, #64748b);
		overflow-wrap: anywhere;
	}

	.learning-funnel-step span {
		font-size: 0.74rem;
		text-transform: uppercase;
		letter-spacing: 0.06em;
	}

	.learning-funnel-step strong {
		font-size: 1.25rem;
		line-height: 1.15;
	}

	.learning-funnel-step small {
		font-size: 0.76rem;
		line-height: 1.25;
	}

	.learning-funnel-alert {
		border-color: color-mix(in srgb, var(--theme-color-warning, #d97706) 34%, var(--theme-color-border, rgba(15, 23, 42, 0.08)));
		background: color-mix(in srgb, var(--theme-color-warning, #d97706) 8%, var(--theme-color-surface, #fff));
	}

	.learning-grid {
		grid-template-columns: repeat(auto-fit, minmax(420px, 1fr));
	}

	.learning-kpi,
	.learning-panel,
	.teaching-panel {
		background: var(--theme-color-surface, #fff);
		border: 1px solid var(--theme-color-border, rgba(15, 23, 42, 0.08));
		border-radius: 8px;
		min-width: 0;
	}

	.learning-kpi {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 12px;
		padding: 14px 16px;
	}

	.learning-kpi span {
		color: var(--theme-color-foreground-muted, #64748b);
		font-size: 0.86rem;
	}

	.learning-kpi strong {
		font-size: 1.35rem;
		font-weight: 700;
	}

	.learning-panel,
	.teaching-panel {
		display: flex;
		flex-direction: column;
		gap: 12px;
		padding: 16px;
	}

	.operator-panel-heading {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
	}

	.operator-panel-heading > span {
		color: var(--theme-color-foreground-muted, #64748b);
		font-size: 0.82rem;
	}

	.operator-action-list {
		display: flex;
		flex-direction: column;
		gap: 8px;
	}

	.operator-action-row {
		display: grid;
		grid-template-columns: minmax(220px, 1fr) minmax(88px, max-content) minmax(112px, max-content);
		align-items: center;
		gap: 12px;
		padding: 10px 0;
		border-top: 1px solid var(--theme-color-border, rgba(15, 23, 42, 0.08));
		font-size: 0.84rem;
	}

	.operator-action-highlight {
		margin: 0 -8px;
		padding: 10px 8px;
		border-radius: 8px;
		background: color-mix(in srgb, var(--theme-color-primary, #2563eb) 10%, transparent);
	}

	.operator-action-row > div {
		display: flex;
		flex-direction: column;
		gap: 2px;
		min-width: 0;
	}

	.operator-action-row strong,
	.operator-action-row span {
		overflow-wrap: anywhere;
	}

	.operator-action-row span {
		color: var(--theme-color-foreground-muted, #64748b);
	}

	.operator-stage-label {
		justify-self: end;
		border: 1px solid var(--theme-color-border, rgba(15, 23, 42, 0.12));
		border-radius: 999px;
		padding: 3px 8px;
		background: color-mix(in srgb, var(--theme-color-primary, #2563eb) 8%, transparent);
		color: var(--theme-color-foreground, #111827) !important;
		font-size: 0.74rem;
	}

	.operator-action-row button {
		justify-self: end;
	}

	.teaching-controls {
		display: grid;
		grid-template-columns: minmax(180px, 1fr) minmax(160px, 0.8fr) minmax(180px, 1fr);
		gap: 12px;
	}

	.teaching-controls label {
		display: flex;
		flex-direction: column;
		gap: 6px;
		color: var(--theme-color-foreground-muted, #64748b);
		font-size: 0.82rem;
	}

	.teaching-actions {
		display: flex;
		justify-content: flex-end;
	}

	.learning-table {
		display: flex;
		flex-direction: column;
		gap: 8px;
		min-width: 0;
	}

	.growth-summary {
		display: grid;
		grid-template-columns: minmax(180px, 1fr) repeat(2, minmax(120px, max-content));
		align-items: center;
		gap: 12px;
		padding: 12px;
		background: color-mix(in srgb, var(--theme-color-primary, #2563eb) 8%, transparent);
		border: 1px solid var(--theme-color-border, rgba(15, 23, 42, 0.08));
		border-radius: 8px;
		font-size: 0.84rem;
	}

	.growth-summary div {
		display: flex;
		flex-direction: column;
		gap: 2px;
		min-width: 0;
	}

	.growth-summary strong,
	.growth-summary span {
		overflow-wrap: anywhere;
	}

	.growth-summary span {
		color: var(--theme-color-foreground-muted, #64748b);
	}

	.learning-row {
		display: grid;
		grid-template-columns: minmax(180px, 1fr) repeat(4, minmax(90px, max-content));
		align-items: center;
		gap: 10px;
		padding: 10px 0;
		border-top: 1px solid var(--theme-color-border, rgba(15, 23, 42, 0.08));
		font-size: 0.84rem;
	}

	.learning-row-action {
		grid-template-columns: minmax(180px, 1fr) repeat(3, minmax(90px, max-content)) minmax(280px, max-content);
	}

	.learning-backlog-row {
		grid-template-columns: minmax(220px, 1fr) minmax(72px, max-content) repeat(2, minmax(90px, max-content)) minmax(160px, max-content);
	}

	.learning-row-highlight {
		margin: 0 -8px;
		padding: 10px 8px;
		border-radius: 8px;
		background: color-mix(in srgb, var(--theme-color-primary, #2563eb) 10%, transparent);
	}

	.learning-row div {
		display: flex;
		flex-direction: column;
		gap: 2px;
		min-width: 0;
	}

	.learning-score {
		align-items: flex-end;
		text-align: right;
	}

	.learning-hint-list {
		align-items: flex-start;
	}

	.learning-chip {
		display: inline-flex;
		width: fit-content;
		max-width: 100%;
		border: 1px solid var(--theme-color-border, rgba(15, 23, 42, 0.12));
		border-radius: 999px;
		padding: 2px 7px;
		background: color-mix(in srgb, var(--theme-color-primary, #2563eb) 8%, transparent);
		color: var(--theme-color-foreground, #111827);
		font-size: 0.74rem;
		line-height: 1.3;
	}

	.learning-row-buttons {
		display: flex;
		flex-direction: row;
		align-items: center;
		justify-content: flex-end;
		flex-wrap: wrap;
		gap: 6px;
	}

	.learning-row-buttons button {
		padding: 6px 8px;
		font-size: 0.78rem;
	}

	.learning-row > div > button {
		align-self: flex-start;
		margin-top: 4px;
		padding: 6px 8px;
		font-size: 0.78rem;
	}

	.learning-row strong,
	.learning-row span {
		overflow-wrap: anywhere;
	}

	.learning-row span {
		color: var(--theme-color-foreground-muted, #64748b);
	}

	.learning-debug-json {
		border: 1px solid var(--theme-color-border, rgba(15, 23, 42, 0.1));
		border-radius: 8px;
		background: var(--theme-color-surface, #fff);
		padding: 10px 12px;
		font-size: 0.82rem;
	}

	.learning-debug-json summary {
		cursor: pointer;
		color: var(--theme-color-foreground-muted, #64748b);
		font-weight: 700;
	}

	.learning-debug-json pre {
		max-height: 420px;
		overflow: auto;
		margin: 10px 0 0;
		padding: 12px;
		border-radius: 8px;
		background: color-mix(in srgb, var(--theme-color-foreground, #111827) 6%, transparent);
		color: var(--theme-color-foreground, #111827);
		font-size: 0.76rem;
		line-height: 1.45;
		white-space: pre-wrap;
	}

	h4 {
		margin: 0;
	}

	.program-state-subhead {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 12px;
		margin-top: 4px;
		font-size: 0.78rem;
		font-weight: 650;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--theme-color-foreground-muted, #64748b);
	}

	.program-state-subhead > span {
		font-weight: 500;
		letter-spacing: 0;
		text-transform: none;
	}

	.learning-row .program-state-fields {
		flex-direction: row;
		flex-wrap: wrap;
		gap: 6px;
		margin-top: 4px;
	}

	.program-state-field {
		display: inline-flex;
		flex-direction: column;
		gap: 1px;
		max-width: 100%;
		border: 1px solid var(--theme-color-border, rgba(15, 23, 42, 0.12));
		border-radius: 8px;
		padding: 3px 8px;
		background: color-mix(in srgb, var(--theme-color-primary, #2563eb) 6%, transparent);
	}

	.program-state-field strong {
		font-size: 0.72rem;
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}

	.program-state-field span {
		font-size: 0.78rem;
	}

	.program-state-control-chip {
		border-color: color-mix(in srgb, var(--theme-color-warning, #d97706) 40%, transparent);
		background: color-mix(in srgb, var(--theme-color-warning, #d97706) 12%, transparent);
	}

	.program-state-reverted {
		opacity: 0.6;
	}

	@media (max-width: 720px) {
		.learning-evolution {
			width: calc(100vw - 16px);
			padding: 16px;
		}

		.learning-evolution-header {
			flex-direction: column;
		}

		.learning-grid {
			grid-template-columns: 1fr;
		}

		.learning-funnel {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}

		.teaching-controls {
			grid-template-columns: 1fr;
		}

		.learning-row,
		.learning-row-action,
		.operator-action-row,
		.growth-summary {
			grid-template-columns: 1fr;
			align-items: stretch;
		}

		.operator-stage-label,
		.operator-action-row button {
			justify-self: stretch;
		}
	}
</style>
