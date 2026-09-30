<script lang="ts">
	import { browser } from '$app/environment';
	import { get } from 'svelte/store';

	import Badge from '$lib/magician/components/native/Badge.svelte';
	import Button from '$lib/magician/components/native/Button.svelte';
	import JsonViewer from '$lib/magician/components/JsonViewer.svelte';
	import Spinner from '$lib/magician/components/native/Spinner.svelte';
	import { scopeIdentityStore, scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import { requestConfirmation } from '$lib/stores/confirmationStore';
	import type { PlanGraph } from '$lib/types/plangraph';
	import PlanGraphView from './PlanGraphView.svelte';
	import SlotGraphInspector from './SlotGraphInspector.svelte';
	import { timedFetch } from '$lib/shared/fetch';

	export let taskId: string | undefined = undefined;
	export let modal = false;
	export let compact = false;

	type InspectorTab =
		| 'graph'
		| 'waterfall'
		| 'analysis'
		| 'slots'
		| 'attempts'
		| 'clarifications'
		| 'history';
	type TaskPlanStatus = 'planning' | 'draft' | 'eliciting' | 'approved' | 'rejected' | 'failed';
	type JsonRecord = Record<string, unknown>;

	interface TaskPlanQuestionOption {
		value: string;
		label: string;
		description?: string | null;
	}

	interface TaskPlanQuestion {
		id: string;
		question_text: string;
		options?: TaskPlanQuestionOption[];
		source_slot_id?: string | null;
		urgency?: number | null;
	}

	interface TaskPlanRecord {
		status?: TaskPlanStatus;
		plan_graph?: PlanGraph | null;
		pending_questions?: TaskPlanQuestion[];
	}

	interface TaskplanResponse {
		plan: TaskPlanRecord;
	}

	interface TaskplanVersionSummary {
		epoch_ms: number;
		label?: string | null;
		step_count: number;
		confidence?: number | null;
	}

	interface TaskplanVersionsResponse {
		versions: TaskplanVersionSummary[];
		has_more: boolean;
	}

	interface TaskplanVersionRecord {
		epoch_ms: number;
		label?: string | null;
		plan_graph?: PlanGraph | null;
	}

	interface TaskplanVersionResponse {
		version: TaskplanVersionRecord;
	}

	interface SaveTaskplanResponse {
		plan: TaskPlanRecord;
	}

	interface TaskPlanAnalysisResponse {
		query_analysis?: JsonRecord | null;
	}

	interface InspectorSlotProvenance {
		source: string;
		timestamp: number;
	}

	interface InspectorSlotRecord {
		id: string;
		slot_type: string;
		value: unknown;
		confidence: number;
		provenance: InspectorSlotProvenance[];
		evidence_links: string[];
		created_at: number;
		updated_at: number;
	}

	interface InspectorConfidenceSummary {
		overall: number;
		min_critical_slot: number | null;
		unresolved_slots: string[];
	}

	interface InspectorEnrichmentError {
		slot_id: string;
		enricher: string;
		message: string;
	}

	interface InspectorEnrichmentSummary {
		total_slots: number;
		invocations: number;
		enrichments_applied: number;
		slots_changed: number;
		errors: InspectorEnrichmentError[];
	}

	interface TaskPlanSlotSnapshot {
		execution_id: string;
		slots: InspectorSlotRecord[];
		confidence_summary?: InspectorConfidenceSummary | null;
		enrichment_summary?: InspectorEnrichmentSummary | null;
		clarified_task?: JsonRecord | null;
		slot_trigger_mappings?: unknown[];
		confidence_boost_results?: JsonRecord | null;
	}

	interface TaskPlanSlotsResponse {
		slot_graph_snapshot?: TaskPlanSlotSnapshot | null;
	}

	interface TaskPlanAttemptRecord {
		strategy_type?: string;
		attempt_number?: number;
		failure_reason?: string | null;
		attempted_at?: number;
		exploration_result?: JsonRecord | null;
		resources_used?: JsonRecord | null;
	}

	interface TaskPlanAttemptsResponse {
		strategy_attempts: TaskPlanAttemptRecord[];
	}

	interface ClarificationHistorySlotUpdate {
		slot_id: string;
		slot_type: string;
		value: unknown;
		confidence: number;
		received_at: number;
	}

	interface ClarificationHistoryEntry {
		question_id: string;
		question_text: string;
		source_slot_id?: string | null;
		stage: string;
		answered_at?: number | null;
		slot_updates: ClarificationHistorySlotUpdate[];
	}

	interface TaskPlanClarificationsResponse {
		clarification_history: ClarificationHistoryEntry[];
	}

	interface ManualResumeSlotPayload {
		slot_id: string;
		slot_type: string;
		value: unknown;
		confidence?: number;
	}

	interface ManualResumeResponse {
		plan: TaskPlanRecord;
		workflow_resumed: boolean;
	}

	let activeTab: InspectorTab = 'graph';
	let loading = false;
	let error: string | null = null;
	let currentPlan: PlanGraph | null = null;
	let currentPlanStatus: TaskPlanStatus | null = null;
	let currentPendingQuestions: TaskPlanQuestion[] = [];
	let saving = false;
	let editable = false;
	let saveLabel = '';
	let lastLoadedTargetKey = '';
	let planGraphView: any = null;
	let currentPlanRequestId = 0;
	let versionsRequestId = 0;
	let selectedVersionRequestId = 0;
	let restoreRequestId = 0;
	let saveRequestId = 0;
	let analysisRequestId = 0;
	let slotsRequestId = 0;
	let attemptsRequestId = 0;
	let clarificationsRequestId = 0;
	let manualResumeRequestId = 0;

	let versionsLoading = false;
	let versionsError: string | null = null;
	let versions: TaskplanVersionSummary[] = [];
	let hasMoreVersions = false;
	let selectedVersionEpoch: number | null = null;
	let selectedVersionLoading = false;
	let selectedVersionError: string | null = null;
	let selectedVersionPlan: PlanGraph | null = null;
	let restoringVersion = false;

	let queryAnalysis: JsonRecord | null = null;
	let analysisLoading = false;
	let analysisError: string | null = null;
	let analysisLoadedTargetKey = '';

	let slotSnapshot: TaskPlanSlotSnapshot | null = null;
	let slotsLoading = false;
	let slotsError: string | null = null;
	let slotsLoadedTargetKey = '';

	let strategyAttempts: TaskPlanAttemptRecord[] = [];
	let attemptsLoading = false;
	let attemptsError: string | null = null;
	let attemptsLoadedTargetKey = '';

	let clarificationHistory: ClarificationHistoryEntry[] = [];
	let clarificationsLoading = false;
	let clarificationsError: string | null = null;
	let clarificationsLoadedTargetKey = '';

	let resuming = false;
	let resumeError: string | null = null;
	let resumeHandledQuestion = '';
	let resumeUpdatedConfidence = '';
	let resumeSlotsJson = '';

	// Phase H8.7 — per-question response form for pending clarifications.
	// Each entry keyed by question.id. Submission POSTs canonical
	// `/api/magician/v2/hitl/{question_id}/respond` with
	// `source: "clarification"` and the explicit task/workflow responder id.
	let clarificationResponses: Record<string, string> = {};
	// Per-question in-flight set — tracking by Set instead of a single
	// `submittingQuestionId` so concurrent submits across different
	// questions don't overwrite each other's "Sending…" indicators or
	// allow a duplicate POST when the user re-clicks Submit while the
	// first request is still in flight.
	let submittingQuestionIds: Set<string> = new Set();
	let clarificationSubmitError: Record<string, string> = {};

	async function submitPlanQuestion(questionId: string): Promise<void> {
		const responseText = (clarificationResponses[questionId] ?? '').trim();
		if (!responseText || !taskId) return;
		if (submittingQuestionIds.has(questionId)) return;
		submittingQuestionIds = new Set([...submittingQuestionIds, questionId]);
		clarificationSubmitError = { ...clarificationSubmitError, [questionId]: '' };
		try {
			const response = await timedFetch(
				`/api/magician/v2/hitl/${encodeURIComponent(questionId)}/respond`,
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({
						source: 'clarification',
						value: { type: 'text', value: responseText },
						channel: 'web',
						task_id: taskId
					})
				}
			);
			if (!(response.ok || response.status === 409)) {
				let message = `Failed to submit response (${response.status})`;
				try {
					const body = await response.json();
					if (body && typeof body === 'object' && typeof (body as Record<string, unknown>).message === 'string') {
						message = (body as { message: string }).message;
					}
				} catch {
					// best-effort
				}
				throw new Error(message);
			}
			delete clarificationResponses[questionId];
			clarificationResponses = { ...clarificationResponses };
			showSuccess('Response sent.');
			await loadCurrentPlan();
			await loadClarifications();
		} catch (err) {
			clarificationSubmitError = {
				...clarificationSubmitError,
				[questionId]: err instanceof Error ? err.message : 'Failed to submit response'
			};
			showError(clarificationSubmitError[questionId]);
		} finally {
			const next = new Set(submittingQuestionIds);
			next.delete(questionId);
			submittingQuestionIds = next;
		}
	}

	$: if (browser) {
		const nextTargetKey = currentInspectorTargetKey();
		if (nextTargetKey !== lastLoadedTargetKey) {
			lastLoadedTargetKey = nextTargetKey;
			resetInspectorState();
			if (taskId) {
				void loadCurrentPlan();
				void loadVersions();
			}
		}
	}

	$: if (!taskId) {
		editable = false;
	}

	$: currentPlanSummary = summarizePlan(currentPlan);
	$: selectedVersionSummary = summarizePlan(selectedVersionPlan);
	/* Editing was previously gated to draft/eliciting only — but the
	   "approve, then notice a step needs a tweak" flow forced users to
	   roundtrip through draft to fix anything. Approved plans are now
	   directly editable too; saving creates a new plan version per
	   the existing PlanGraphView save handler. */
	$: canEditCurrentPlan = currentPlanStatus === 'draft' || currentPlanStatus === 'eliciting' || currentPlanStatus === 'approved';
	$: compactMode = modal || compact;
	$: if (browser && taskId) {
		const currentTargetKey = currentInspectorTargetKey();
		if (activeTab === 'analysis' && analysisLoadedTargetKey !== currentTargetKey && !analysisLoading) {
			void loadAnalysis();
		}
		if (activeTab === 'slots' && slotsLoadedTargetKey !== currentTargetKey && !slotsLoading) {
			void loadSlots();
		}
		if (activeTab === 'attempts' && attemptsLoadedTargetKey !== currentTargetKey && !attemptsLoading) {
			void loadAttempts();
		}
		if (activeTab === 'clarifications' && clarificationsLoadedTargetKey !== currentTargetKey && !clarificationsLoading) {
			void loadClarifications();
		}
	}

	function currentInspectorScopeKey(): string {
		const scope = get(scopeIdentityStore);
		return `${scope.principal}:${scope.workspace}`;
	}

	function currentInspectorTargetKey(nextTaskId: string | undefined = taskId): string {
		return `${currentInspectorScopeKey()}::${nextTaskId || ''}`;
	}

	function isStaleInspectorTarget(targetKey: string): boolean {
		return targetKey !== currentInspectorTargetKey();
	}

	function scopedHeaders(headers?: HeadersInit): Headers {
		return scopedRequestHeaders(headers);
	}

	function scopedUrl(path: string, extras?: Record<string, string | number | undefined | null>): string {
		const params = new URLSearchParams();
		for (const [key, value] of Object.entries(extras || {})) {
			if (value === undefined || value === null) continue;
			params.set(key, String(value));
		}
		const query = params.toString();
		return query ? `${path}?${query}` : path;
	}

	async function fetchJson<T>(url: string, init?: RequestInit): Promise<T> {
		const response = await timedFetch(url, {
			...init,
			headers: scopedHeaders(init?.headers)
		});
		if (!response.ok) {
			const body = await response.text().catch(() => '');
			throw new Error(body || `Request failed (${response.status})`);
		}
		return response.json() as Promise<T>;
	}

	function taskplanUrl(taskId: string): string {
		return `/api/magician/v3/tasks/${encodeURIComponent(taskId)}/plan`;
	}

	function taskplanVersionsUrl(taskId: string): string {
		return `/api/magician/v3/tasks/${encodeURIComponent(taskId)}/plan/versions`;
	}

	function taskplanVersionUrl(taskId: string, epochMs: number): string {
		return `/api/magician/v3/tasks/${encodeURIComponent(taskId)}/plan/versions/${epochMs}`;
	}

	function resetInspectorState(): void {
		error = null;
		currentPlan = null;
		currentPlanStatus = null;
		currentPendingQuestions = [];
		saving = false;
		editable = false;
		saveLabel = '';
		versionsLoading = false;
		versionsError = null;
		versions = [];
		hasMoreVersions = false;
		selectedVersionEpoch = null;
		selectedVersionLoading = false;
		selectedVersionError = null;
		selectedVersionPlan = null;
		queryAnalysis = null;
		analysisLoading = false;
		analysisError = null;
		analysisLoadedTargetKey = '';
		slotSnapshot = null;
		slotsLoading = false;
		slotsError = null;
		slotsLoadedTargetKey = '';
		strategyAttempts = [];
		attemptsLoading = false;
		attemptsError = null;
		attemptsLoadedTargetKey = '';
		clarificationHistory = [];
		clarificationsLoading = false;
		clarificationsError = null;
		clarificationsLoadedTargetKey = '';
		resuming = false;
		resumeError = null;
		resumeHandledQuestion = '';
		resumeUpdatedConfidence = '';
		resumeSlotsJson = '';
	}

	function applyCurrentPlanRecord(
		plan: TaskPlanRecord | null | undefined,
		fallbacks?: {
			planGraph?: PlanGraph | null;
			status?: TaskPlanStatus | null;
			pendingQuestions?: TaskPlanQuestion[];
		}
	): void {
		if (!plan) {
			currentPlan = fallbacks?.planGraph ?? null;
			currentPlanStatus = fallbacks?.status ?? null;
			currentPendingQuestions = fallbacks?.pendingQuestions ?? [];
			return;
		}

		currentPlan = plan.plan_graph ?? null;
		currentPlanStatus = plan.status ?? null;
		currentPendingQuestions = Array.isArray(plan.pending_questions) ? plan.pending_questions : [];
	}

	async function loadCurrentPlan(): Promise<void> {
		const requestedTaskId = taskId;
		if (!requestedTaskId) return;
		const targetKey = currentInspectorTargetKey(requestedTaskId);
		const requestId = ++currentPlanRequestId;
		loading = true;
		error = null;
		try {
			const response = await fetchJson<TaskplanResponse>(scopedUrl(taskplanUrl(requestedTaskId)));
			if (requestId !== currentPlanRequestId || isStaleInspectorTarget(targetKey)) return;
			applyCurrentPlanRecord(response.plan);
		} catch (err) {
			if (requestId !== currentPlanRequestId || isStaleInspectorTarget(targetKey)) return;
			error = err instanceof Error ? err.message : 'Failed to load plan';
		} finally {
			if (requestId === currentPlanRequestId && !isStaleInspectorTarget(targetKey)) {
				loading = false;
			}
		}
	}

	async function loadVersions(): Promise<void> {
		const requestedTaskId = taskId;
		if (!requestedTaskId) return;
		const targetKey = currentInspectorTargetKey(requestedTaskId);
		const requestId = ++versionsRequestId;
		versionsLoading = true;
		versionsError = null;
		try {
			const response = await fetchJson<TaskplanVersionsResponse>(
				scopedUrl(taskplanVersionsUrl(requestedTaskId), { limit: 20 })
			);
			if (requestId !== versionsRequestId || isStaleInspectorTarget(targetKey)) return;
			versions = response.versions || [];
			hasMoreVersions = response.has_more;
		} catch (err) {
			if (requestId !== versionsRequestId || isStaleInspectorTarget(targetKey)) return;
			versionsError = err instanceof Error ? err.message : 'Failed to load plan history';
		} finally {
			if (requestId === versionsRequestId && !isStaleInspectorTarget(targetKey)) {
				versionsLoading = false;
			}
		}
	}

	async function previewVersion(epochMs: number): Promise<void> {
		const requestedTaskId = taskId;
		if (!requestedTaskId) return;
		const targetKey = currentInspectorTargetKey(requestedTaskId);
		const requestId = ++selectedVersionRequestId;
		selectedVersionEpoch = epochMs;
		selectedVersionLoading = true;
		selectedVersionError = null;
		selectedVersionPlan = null;
		try {
			const response = await fetchJson<TaskplanVersionResponse>(
				scopedUrl(taskplanVersionUrl(requestedTaskId, epochMs))
			);
			if (
				requestId !== selectedVersionRequestId
				|| isStaleInspectorTarget(targetKey)
				|| selectedVersionEpoch !== epochMs
			) {
				return;
			}
			selectedVersionPlan = response.version?.plan_graph || null;
		} catch (err) {
			if (
				requestId !== selectedVersionRequestId
				|| isStaleInspectorTarget(targetKey)
				|| selectedVersionEpoch !== epochMs
			) {
				return;
			}
			selectedVersionError = err instanceof Error ? err.message : 'Failed to load plan version';
		} finally {
			if (
				requestId === selectedVersionRequestId
				&& !isStaleInspectorTarget(targetKey)
				&& selectedVersionEpoch === epochMs
			) {
				selectedVersionLoading = false;
			}
		}
	}

	function clearVersionPreview(): void {
		selectedVersionRequestId += 1;
		selectedVersionEpoch = null;
		selectedVersionError = null;
		selectedVersionPlan = null;
	}

	async function restoreSelectedVersion(): Promise<void> {
		const requestedTaskId = taskId;
		const requestedEpoch = selectedVersionEpoch;
		if (!requestedTaskId || requestedEpoch == null || restoringVersion) return;
		const targetKey = currentInspectorTargetKey(requestedTaskId);
		const confirmed = await requestConfirmation({
			title: 'Restore historical plan version?',
			message: 'This will replace the current task plan with this historical version.',
			confirmLabel: 'Restore',
			destructive: true
		});
		if (!confirmed) return;
		const requestId = ++restoreRequestId;
		restoringVersion = true;
		selectedVersionError = null;
		try {
			const response = await fetchJson<SaveTaskplanResponse>(
				scopedUrl(`${taskplanVersionUrl(requestedTaskId, requestedEpoch)}/restore`),
				{ method: 'POST' }
			);
			if (requestId !== restoreRequestId || isStaleInspectorTarget(targetKey)) return;
			applyCurrentPlanRecord(response.plan, {
				planGraph: currentPlan,
				status: currentPlanStatus,
				pendingQuestions: currentPendingQuestions
			});
			showSuccess('Plan version restored.');
			await Promise.all([loadCurrentPlan(), loadVersions()]);
			clearVersionPreview();
			activeTab = 'waterfall';
		} catch (err) {
			if (requestId !== restoreRequestId || isStaleInspectorTarget(targetKey)) return;
			selectedVersionError = err instanceof Error ? err.message : 'Failed to restore plan version';
			showError(selectedVersionError);
		} finally {
			if (requestId === restoreRequestId && !isStaleInspectorTarget(targetKey)) {
				restoringVersion = false;
			}
		}
	}

	function toggleEditable(): void {
		if (!currentPlan || !canEditCurrentPlan) return;
		editable = !editable;
		if (!editable) {
			saveLabel = '';
		}
	}

	function triggerSave(): void {
		if (!editable || !planGraphView || saving) return;
		planGraphView.requestSave();
	}

	async function handlePlanSave(event: CustomEvent<{ plan: PlanGraph }>): Promise<void> {
		const requestedTaskId = taskId;
		if (!requestedTaskId) return;
		const targetKey = currentInspectorTargetKey(requestedTaskId);
		const requestId = ++saveRequestId;
		saving = true;
		error = null;
		try {
			const response = await fetchJson<SaveTaskplanResponse>(scopedUrl(taskplanUrl(requestedTaskId)), {
				method: 'PUT',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({
					plan: event.detail.plan,
					label: saveLabel.trim() || null
				})
			});
			if (requestId !== saveRequestId || isStaleInspectorTarget(targetKey)) return;
			applyCurrentPlanRecord(response.plan, {
				planGraph: event.detail.plan,
				status: currentPlanStatus,
				pendingQuestions: currentPendingQuestions
			});
			showSuccess('Plan saved.');
			saveLabel = '';
			editable = false;
			await Promise.all([loadCurrentPlan(), loadVersions()]);
		} catch (err) {
			if (requestId !== saveRequestId || isStaleInspectorTarget(targetKey)) return;
			error = err instanceof Error ? err.message : 'Failed to save plan';
			showError(error);
		} finally {
			if (requestId === saveRequestId && !isStaleInspectorTarget(targetKey)) {
				saving = false;
			}
		}
	}

	function summarizePlan(plan: PlanGraph | null): { steps: number; edges: number; unresolved: number; confidence: number | null } {
		if (!plan) {
			return { steps: 0, edges: 0, unresolved: 0, confidence: null };
		}
		return {
			steps: plan.steps?.length || 0,
			edges: plan.edges?.length || 0,
			unresolved: plan.unresolved_inputs?.length || 0,
			confidence: typeof plan.confidence === 'number' ? plan.confidence : null
		};
	}

	function formatTimestamp(value: number | null | undefined): string {
		if (!value) return '—';
		return new Date(value).toLocaleString();
	}

	function confidenceText(value: number | null): string {
		if (value == null) return '—';
		return `${Math.round(value * 100)}%`;
	}

	function selectedVersionLabel(version: TaskplanVersionSummary): string {
		return version.label?.trim() || 'Saved version';
	}

	async function loadAnalysis(force = false): Promise<void> {
		const requestedTaskId = taskId;
		if (!requestedTaskId) return;
		const targetKey = currentInspectorTargetKey(requestedTaskId);
		if (!force && analysisLoadedTargetKey === targetKey) return;
		const requestId = ++analysisRequestId;
		analysisLoading = true;
		analysisError = null;
		try {
			const response = await fetchJson<TaskPlanAnalysisResponse>(
				scopedUrl(`${taskplanUrl(requestedTaskId)}/analysis`)
			);
			if (requestId !== analysisRequestId || isStaleInspectorTarget(targetKey)) return;
			queryAnalysis = response.query_analysis || null;
			analysisLoadedTargetKey = targetKey;
		} catch (err) {
			if (requestId !== analysisRequestId || isStaleInspectorTarget(targetKey)) return;
			analysisError = err instanceof Error ? err.message : 'Failed to load plan analysis';
		} finally {
			if (requestId === analysisRequestId && !isStaleInspectorTarget(targetKey)) {
				analysisLoading = false;
			}
		}
	}

	async function loadSlots(force = false): Promise<void> {
		const requestedTaskId = taskId;
		if (!requestedTaskId) return;
		const targetKey = currentInspectorTargetKey(requestedTaskId);
		if (!force && slotsLoadedTargetKey === targetKey) return;
		const requestId = ++slotsRequestId;
		slotsLoading = true;
		slotsError = null;
		try {
			const response = await fetchJson<TaskPlanSlotsResponse>(
				scopedUrl(`${taskplanUrl(requestedTaskId)}/slots`)
			);
			if (requestId !== slotsRequestId || isStaleInspectorTarget(targetKey)) return;
			slotSnapshot = response.slot_graph_snapshot || null;
			slotsLoadedTargetKey = targetKey;
		} catch (err) {
			if (requestId !== slotsRequestId || isStaleInspectorTarget(targetKey)) return;
			slotsError = err instanceof Error ? err.message : 'Failed to load slot graph snapshot';
		} finally {
			if (requestId === slotsRequestId && !isStaleInspectorTarget(targetKey)) {
				slotsLoading = false;
			}
		}
	}

	async function loadAttempts(force = false): Promise<void> {
		const requestedTaskId = taskId;
		if (!requestedTaskId) return;
		const targetKey = currentInspectorTargetKey(requestedTaskId);
		if (!force && attemptsLoadedTargetKey === targetKey) return;
		const requestId = ++attemptsRequestId;
		attemptsLoading = true;
		attemptsError = null;
		try {
			const response = await fetchJson<TaskPlanAttemptsResponse>(
				scopedUrl(`${taskplanUrl(requestedTaskId)}/attempts`)
			);
			if (requestId !== attemptsRequestId || isStaleInspectorTarget(targetKey)) return;
			strategyAttempts = response.strategy_attempts || [];
			attemptsLoadedTargetKey = targetKey;
		} catch (err) {
			if (requestId !== attemptsRequestId || isStaleInspectorTarget(targetKey)) return;
			attemptsError = err instanceof Error ? err.message : 'Failed to load strategy attempts';
		} finally {
			if (requestId === attemptsRequestId && !isStaleInspectorTarget(targetKey)) {
				attemptsLoading = false;
			}
		}
	}

	async function loadClarifications(force = false): Promise<void> {
		const requestedTaskId = taskId;
		if (!requestedTaskId) return;
		const targetKey = currentInspectorTargetKey(requestedTaskId);
		if (!force && clarificationsLoadedTargetKey === targetKey) return;
		const requestId = ++clarificationsRequestId;
		clarificationsLoading = true;
		clarificationsError = null;
		try {
			const response = await fetchJson<TaskPlanClarificationsResponse>(
				scopedUrl(`${taskplanUrl(requestedTaskId)}/clarifications`)
			);
			if (requestId !== clarificationsRequestId || isStaleInspectorTarget(targetKey)) return;
			clarificationHistory = response.clarification_history || [];
			clarificationsLoadedTargetKey = targetKey;
		} catch (err) {
			if (requestId !== clarificationsRequestId || isStaleInspectorTarget(targetKey)) return;
			clarificationsError = err instanceof Error ? err.message : 'Failed to load clarification history';
		} finally {
			if (requestId === clarificationsRequestId && !isStaleInspectorTarget(targetKey)) {
				clarificationsLoading = false;
			}
		}
	}

	async function refreshActiveArtifacts(): Promise<void> {
		switch (activeTab) {
			case 'analysis':
				await loadAnalysis(true);
				break;
			case 'slots':
				await loadSlots(true);
				break;
			case 'attempts':
				await loadAttempts(true);
				break;
			case 'clarifications':
				await loadClarifications(true);
				break;
			case 'history':
				await loadVersions();
				break;
		}
	}

	function parseResumeSlots(jsonText: string): ManualResumeSlotPayload[] | undefined {
		const trimmed = jsonText.trim();
		if (!trimmed) return undefined;
		const parsed = JSON.parse(trimmed);
		if (!Array.isArray(parsed)) {
			throw new Error('Manual resume slots must be a JSON array.');
		}
		return parsed.map((entry, index) => {
			const record = entry as Record<string, unknown>;
			const slotId = typeof record.slot_id === 'string' ? record.slot_id.trim() : '';
			const slotType = typeof record.slot_type === 'string' ? record.slot_type.trim() : '';
			if (!slotId || !slotType) {
				throw new Error(`Manual resume slot ${index + 1} must include slot_id and slot_type.`);
			}
			const payload: ManualResumeSlotPayload = {
				slot_id: slotId,
				slot_type: slotType,
				value: record.value
			};
			if (typeof record.confidence === 'number' && Number.isFinite(record.confidence)) {
				payload.confidence = record.confidence;
			}
			return payload;
		});
	}

	async function handleManualResume(): Promise<void> {
		const requestedTaskId = taskId;
		if (!requestedTaskId || resuming) return;
		const targetKey = currentInspectorTargetKey(requestedTaskId);
		const requestId = ++manualResumeRequestId;
		resumeError = null;
		resuming = true;
		try {
			const payload: Record<string, unknown> = {};
			const handledQuestion = resumeHandledQuestion.trim();
			if (handledQuestion) {
				payload.handled_question = handledQuestion;
			}
			const confidenceText = resumeUpdatedConfidence.trim();
			if (confidenceText) {
				const confidence = Number.parseFloat(confidenceText);
				if (!Number.isFinite(confidence) || confidence < 0 || confidence > 1) {
					throw new Error('Updated confidence must be a number between 0 and 1.');
				}
				payload.updated_confidence = confidence;
			}
			const slots = parseResumeSlots(resumeSlotsJson);
			if (slots && slots.length > 0) {
				payload.slots = slots;
			}

			const response = await fetchJson<ManualResumeResponse>(
				scopedUrl(`${taskplanUrl(requestedTaskId)}/clarifications/resume`),
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify(payload)
				}
			);
			if (requestId !== manualResumeRequestId || isStaleInspectorTarget(targetKey)) return;
			applyCurrentPlanRecord(response.plan, {
				planGraph: currentPlan,
				status: currentPlanStatus,
				pendingQuestions: []
			});
			resumeHandledQuestion = '';
			resumeUpdatedConfidence = '';
			resumeSlotsJson = '';
			showSuccess(
				response.workflow_resumed
					? 'Planning resumed.'
					: 'Manual resume recorded. Planning is still waiting on remaining inputs.'
			);
			await Promise.all([
				loadCurrentPlan(),
				loadAnalysis(true),
				loadSlots(true),
				loadAttempts(true),
				loadClarifications(true)
			]);
		} catch (err) {
			if (requestId !== manualResumeRequestId || isStaleInspectorTarget(targetKey)) return;
			resumeError = err instanceof Error ? err.message : 'Failed to resume planning';
			showError(resumeError);
		} finally {
			if (requestId === manualResumeRequestId && !isStaleInspectorTarget(targetKey)) {
				resuming = false;
			}
		}
	}

	function attemptConfidence(attempt: TaskPlanAttemptRecord): number | null {
		const value = attempt.exploration_result?.confidence;
		return typeof value === 'number' ? value : null;
	}

	function stageLabel(stage: string | null | undefined): string {
		if (!stage) return 'Unknown';
		return stage
			.split(/[_\s]+/)
			.map((token) => token.charAt(0).toUpperCase() + token.slice(1).toLowerCase())
			.join(' ');
	}

	function formatMillisTimestamp(value: number | null | undefined): string {
		if (value == null || Number.isNaN(value)) return '—';
		return new Date(value).toLocaleString();
	}
</script>

<div
	class="plan-inspector"
	class:plan-inspector--modal={modal}
	class:plan-inspector--compact={compactMode}
>
	<div class="plan-inspector__toolbar">
		<div class="plan-inspector__tabs">
			<button class:active={activeTab === 'graph'} on:click={() => (activeTab = 'graph')}>Graph</button>
			<button class:active={activeTab === 'waterfall'} on:click={() => (activeTab = 'waterfall')}>Waterfall</button>
			<button class:active={activeTab === 'analysis'} on:click={() => (activeTab = 'analysis')}>Analysis</button>
			<button class:active={activeTab === 'slots'} on:click={() => (activeTab = 'slots')}>Slots</button>
			<button class:active={activeTab === 'attempts'} on:click={() => (activeTab = 'attempts')}>Attempts</button>
			<button class:active={activeTab === 'clarifications'} on:click={() => (activeTab = 'clarifications')}>Clarifications</button>
			<button class:active={activeTab === 'history'} on:click={() => (activeTab = 'history')}>History</button>
		</div>

		<div class="plan-inspector__toolbar-meta">
			<div class="plan-inspector__actions">
				<Button
					label={loading ? 'Refreshing…' : 'Refresh'}
					variant="outline"
					size="sm"
					disabled={loading || !taskId}
					on:click={() => {
						void loadCurrentPlan();
						void loadVersions();
						void refreshActiveArtifacts();
					}}
				/>
				{#if activeTab !== 'history' && currentPlan}
					<Button
						label={editable ? 'Stop editing' : 'Edit plan'}
						variant="outline"
						size="sm"
						disabled={!canEditCurrentPlan}
						on:click={toggleEditable}
					/>
					{#if !canEditCurrentPlan}
						<span class="plan-inspector__mode-copy">Edit is locked while the plan is running or complete.</span>
					{/if}
					{#if editable}
						<Button
							label={saving ? 'Saving…' : 'Save plan'}
							size="sm"
							disabled={saving}
							on:click={triggerSave}
						/>
					{/if}
				{/if}
			</div>

			{#if currentPlan}
				<div class="plan-inspector__summary">
					<Badge text={`${currentPlanSummary.steps} step${currentPlanSummary.steps === 1 ? '' : 's'}`} color="info" />
					<Badge text={`${currentPlanSummary.edges} edge${currentPlanSummary.edges === 1 ? '' : 's'}`} color="default" />
					<Badge text={`${currentPlanSummary.unresolved} unresolved`} color={currentPlanSummary.unresolved > 0 ? 'warning' : 'success'} />
					<Badge text={`Confidence ${confidenceText(currentPlanSummary.confidence)}`} color="default" />
					{#if currentPlan.provenance?.strategy}
						<Badge text={currentPlan.provenance.strategy} color="default" />
					{/if}
				</div>
			{/if}
		</div>
	</div>

	{#if editable && activeTab !== 'history'}
		<div class="plan-inspector__save-row">
			<input
				class="plan-inspector__save-input"
				type="text"
				bind:value={saveLabel}
				placeholder="Optional save label"
			/>
			<span class="plan-inspector__save-copy">Save captures a restorable version before writing the updated task plan.</span>
		</div>
	{/if}

	{#if error}
		<p class="plan-inspector__error">{error}</p>
	{/if}

	{#if activeTab === 'history'}
		<div class="plan-history">
			<div class="plan-history__list">
				<div class="plan-history__list-header">
					<strong>Saved versions</strong>
					{#if versionsLoading}
						<span class="plan-history__muted">Loading…</span>
					{:else if hasMoreVersions}
						<span class="plan-history__muted">Latest 20 shown</span>
					{/if}
				</div>

				{#if versionsError}
					<p class="plan-inspector__error">{versionsError}</p>
				{:else if versions.length === 0}
					<p class="plan-history__muted">No saved versions yet.</p>
				{:else}
					<div class="plan-history__items">
						{#each versions as version (version.epoch_ms)}
							<button
								class="plan-history__item"
								class:selected={selectedVersionEpoch === version.epoch_ms}
								on:click={() => void previewVersion(version.epoch_ms)}
							>
								<strong>{selectedVersionLabel(version)}</strong>
								<span>{formatTimestamp(version.epoch_ms)}</span>
								<span>{version.step_count} step{version.step_count === 1 ? '' : 's'}</span>
							</button>
						{/each}
					</div>
				{/if}
			</div>

			<div class="plan-history__preview">
				{#if selectedVersionLoading}
					<div class="plan-history__loading">
						<Spinner size="sm" label="Loading plan version" />
					</div>
				{:else if selectedVersionError}
					<p class="plan-inspector__error">{selectedVersionError}</p>
				{:else if selectedVersionPlan && selectedVersionEpoch != null}
					<div class="plan-history__preview-header">
						<div>
							<strong>Version preview</strong>
							<p>{formatTimestamp(selectedVersionEpoch)}</p>
						</div>
						<div class="plan-history__preview-actions">
							<Badge text={`${selectedVersionSummary.steps} step${selectedVersionSummary.steps === 1 ? '' : 's'}`} color="info" />
							<Button
								label={restoringVersion ? 'Restoring…' : 'Restore version'}
								size="sm"
								disabled={restoringVersion}
								on:click={restoreSelectedVersion}
							/>
							<Button
								label="Clear preview"
								variant="outline"
								size="sm"
								on:click={clearVersionPreview}
							/>
						</div>
					</div>
					<PlanGraphView planGraph={selectedVersionPlan} viewMode="waterfall" />
				{:else}
					<p class="plan-history__muted">Select a saved version to preview or restore it.</p>
				{/if}
			</div>
		</div>
	{:else if loading && !currentPlan}
		<div class="plan-history__loading">
			<Spinner size="sm" label="Loading plan inspector" />
		</div>
	{:else if activeTab === 'analysis'}
		<div class="plan-artifact-pane">
			{#if analysisLoading}
				<div class="plan-history__loading">
					<Spinner size="sm" label="Loading analysis" />
				</div>
			{:else if analysisError}
				<p class="plan-inspector__error">{analysisError}</p>
			{:else if queryAnalysis}
				<JsonViewer data={queryAnalysis} maxHeight="54vh" />
			{:else}
				<p class="plan-history__muted">No query analysis was persisted for this task plan.</p>
			{/if}
		</div>
	{:else if activeTab === 'slots'}
		<div class="plan-artifact-pane">
			{#if slotsLoading}
				<div class="plan-history__loading">
					<Spinner size="sm" label="Loading slot graph snapshot" />
				</div>
			{:else if slotsError}
				<p class="plan-inspector__error">{slotsError}</p>
			{:else if slotSnapshot}
				{#if slotSnapshot.slots?.length > 0}
					<SlotGraphInspector
						slots={slotSnapshot.slots}
						viewMode="researcher"
						confidenceSummary={slotSnapshot.confidence_summary || null}
						enrichmentSummary={slotSnapshot.enrichment_summary || null}
						executionId={slotSnapshot.execution_id}
					/>
				{:else}
					<p class="plan-history__muted">No slot records were captured for this task plan.</p>
				{/if}
				{#if slotSnapshot.clarified_task}
					<details class="plan-artifact-detail">
						<summary>Clarified task</summary>
						<JsonViewer data={slotSnapshot.clarified_task} maxHeight="22rem" />
					</details>
				{/if}
				{#if (slotSnapshot.slot_trigger_mappings?.length || 0) > 0}
					<details class="plan-artifact-detail">
						<summary>Slot trigger mappings</summary>
						<JsonViewer data={slotSnapshot.slot_trigger_mappings} maxHeight="22rem" />
					</details>
				{/if}
				{#if slotSnapshot.confidence_boost_results && Object.keys(slotSnapshot.confidence_boost_results).length > 0}
					<details class="plan-artifact-detail">
						<summary>Confidence boost results</summary>
						<JsonViewer data={slotSnapshot.confidence_boost_results} maxHeight="22rem" />
					</details>
				{/if}
			{:else}
				<p class="plan-history__muted">No slot graph snapshot was persisted for this task plan.</p>
			{/if}
		</div>
	{:else if activeTab === 'attempts'}
		<div class="plan-artifact-pane">
			{#if attemptsLoading}
				<div class="plan-history__loading">
					<Spinner size="sm" label="Loading strategy attempts" />
				</div>
			{:else if attemptsError}
				<p class="plan-inspector__error">{attemptsError}</p>
			{:else if strategyAttempts.length > 0}
				<div class="plan-attempts">
					{#each strategyAttempts as attempt, index (`attempt-${attempt.attempt_number || index}`)}
						<details class="plan-attempt">
							<summary>
								<div class="plan-attempt__summary">
									<strong>{attempt.strategy_type || 'Strategy attempt'} #{attempt.attempt_number || index + 1}</strong>
									<div class="plan-attempt__meta">
										<Badge
											text={`Confidence ${confidenceText(attemptConfidence(attempt))}`}
											color="default"
										/>
										<Badge
											text={attempt.failure_reason ? 'Failed' : 'Completed'}
											color={attempt.failure_reason ? 'error' : 'success'}
										/>
										<span>{formatMillisTimestamp(attempt.attempted_at)}</span>
									</div>
								</div>
							</summary>
							{#if attempt.failure_reason}
								<p class="plan-inspector__error">{attempt.failure_reason}</p>
							{/if}
							{#if attempt.resources_used}
								<details class="plan-artifact-detail">
									<summary>Resources used</summary>
									<JsonViewer data={attempt.resources_used} maxHeight="18rem" />
								</details>
							{/if}
							{#if attempt.exploration_result}
								<details class="plan-artifact-detail" open={index === 0}>
									<summary>Exploration result</summary>
									<JsonViewer data={attempt.exploration_result} maxHeight="28rem" />
								</details>
							{/if}
						</details>
					{/each}
				</div>
			{:else}
				<p class="plan-history__muted">No strategy attempts were persisted for this task plan.</p>
			{/if}
		</div>
	{:else if activeTab === 'clarifications'}
		<div class="plan-artifact-pane plan-artifact-pane--clarifications">
			{#if clarificationsLoading}
				<div class="plan-history__loading">
					<Spinner size="sm" label="Loading clarification history" />
				</div>
			{:else if clarificationsError}
				<p class="plan-inspector__error">{clarificationsError}</p>
			{:else}
				<div class="plan-clarification-section">
					<div class="plan-clarification-section__header">
						<strong>Pending questions</strong>
						<Badge
							text={`${currentPendingQuestions.length} pending`}
							color={currentPendingQuestions.length > 0 ? 'warning' : 'success'}
						/>
					</div>
					{#if currentPendingQuestions.length > 0}
						<div class="plan-clarification-list">
							{#each currentPendingQuestions as question (`pending-${question.id}`)}
								<div class="plan-clarification-card">
									<strong>{question.question_text}</strong>
									<div class="plan-clarification-card__meta">
										<span>ID: {question.id}</span>
										{#if question.source_slot_id}
											<span>Slot: {question.source_slot_id}</span>
										{/if}
										{#if typeof question.urgency === 'number'}
											<span>Urgency: {Math.round(question.urgency * 100)}%</span>
										{/if}
									</div>
									{#if question.options && question.options.length > 0}
										<div class="plan-question-options">
											{#each question.options as option}
												<button
													type="button"
													class="plan-question-option plan-question-option--clickable"
													disabled={submittingQuestionIds.has(question.id)}
													on:click={() => {
														clarificationResponses = {
															...clarificationResponses,
															[question.id]: option.value
														};
														void submitPlanQuestion(question.id);
													}}
												>
													{option.label}
												</button>
											{/each}
										</div>
									{/if}
									<form
										class="plan-question-response"
										on:submit|preventDefault={() => submitPlanQuestion(question.id)}
									>
										<textarea
											class="plan-question-response__input"
											rows="2"
											placeholder="Type your answer…"
											value={clarificationResponses[question.id] ?? ''}
											on:input={(event) => {
												// Always reassign the surrounding map so Svelte's
												// reactivity picks up the change for the Submit
												// disabled predicate — `bind:value` with a
												// dynamic key does not reliably mark the parent
												// object dirty across all Svelte 4 paths.
												const target = event.currentTarget as HTMLTextAreaElement;
												clarificationResponses = {
													...clarificationResponses,
													[question.id]: target.value
												};
											}}
											disabled={submittingQuestionIds.has(question.id)}
										></textarea>
										<div class="plan-question-response__actions">
											<Button
												variant="primary"
												disabled={!(clarificationResponses[question.id]?.trim())
													|| submittingQuestionIds.has(question.id)}
												on:click={() => submitPlanQuestion(question.id)}
												label={submittingQuestionIds.has(question.id) ? 'Sending…' : 'Submit'}
											/>
										</div>
										{#if clarificationSubmitError[question.id]}
											<p class="plan-question-response__error">
												{clarificationSubmitError[question.id]}
											</p>
										{/if}
									</form>
								</div>
							{/each}
						</div>
					{:else}
						<p class="plan-history__muted">There are no pending planning questions right now.</p>
					{/if}
				</div>

				<div class="plan-clarification-section">
					<div class="plan-clarification-section__header">
						<strong>Completed clarification history</strong>
						<Badge
							text={`${clarificationHistory.length} answered`}
							color={clarificationHistory.length > 0 ? 'info' : 'default'}
						/>
					</div>
					{#if clarificationHistory.length > 0}
						<div class="plan-clarification-list">
							{#each clarificationHistory as entry (`history-${entry.question_id}`)}
								<details class="plan-clarification-card">
									<summary>
										<div class="plan-clarification-card__summary">
											<strong>{entry.question_text}</strong>
											<div class="plan-clarification-card__meta">
												<span>{stageLabel(entry.stage)}</span>
												<span>{formatMillisTimestamp(entry.answered_at)}</span>
												<span>{entry.slot_updates.length} slot update{entry.slot_updates.length === 1 ? '' : 's'}</span>
											</div>
										</div>
									</summary>
									{#if entry.source_slot_id}
										<p class="plan-history__muted">Source slot: {entry.source_slot_id}</p>
									{/if}
									{#if entry.slot_updates.length > 0}
										<JsonViewer data={entry.slot_updates} maxHeight="18rem" />
									{:else}
										<p class="plan-history__muted">No slot updates were recorded for this clarification.</p>
									{/if}
								</details>
							{/each}
						</div>
					{:else}
						<p class="plan-history__muted">No clarification history has been recorded yet.</p>
					{/if}
				</div>

				{#if currentPlanStatus === 'planning' || currentPlanStatus === 'eliciting'}
					<div class="plan-clarification-section">
						<div class="plan-clarification-section__header">
							<strong>Manual resume</strong>
							<Badge text={currentPlanStatus} color="warning" />
						</div>
						<p class="plan-history__muted">
							Advanced control for resuming the planning workflow when you already know the missing slot values or want to nudge the planner forward without answering through the normal question flow.
						</p>
						<div class="plan-resume-grid">
							<input
								class="plan-inspector__save-input"
								type="text"
								bind:value={resumeHandledQuestion}
								placeholder="Optional handled question id"
							/>
							<input
								class="plan-inspector__save-input"
								type="number"
								min="0"
								max="1"
								step="0.01"
								bind:value={resumeUpdatedConfidence}
								placeholder="Optional updated confidence (0-1)"
							/>
						</div>
						<textarea
							class="plan-inspector__save-input plan-inspector__save-textarea"
							bind:value={resumeSlotsJson}
							rows="6"
							placeholder={'Optional slots JSON, e.g. [{"slot_id":"contact","slot_type":"entity","value":"Jordan","confidence":0.9}]'}
						></textarea>
						{#if resumeError}
							<p class="plan-inspector__error">{resumeError}</p>
						{/if}
						<div class="plan-inspector__actions">
							<Button
								label={resuming ? 'Resuming…' : 'Resume planning'}
								size="sm"
								disabled={resuming}
								on:click={handleManualResume}
							/>
						</div>
					</div>
				{/if}
			{/if}
		</div>
	{:else if currentPlan}
		<PlanGraphView
			bind:this={planGraphView}
			planGraph={currentPlan}
			viewMode={activeTab === 'graph' ? 'graph' : 'waterfall'}
			editable={editable && canEditCurrentPlan}
			on:save={handlePlanSave}
		/>
	{:else if taskId}
		<p class="plan-history__muted">No graph-backed task plan is available for this task.</p>
	{:else}
		<p class="plan-history__muted">A plan inspector becomes available once a task has a persisted plan.</p>
	{/if}
</div>

<style>
	.plan-inspector {
		display: grid;
		gap: 0.9rem;
	}

	.plan-inspector__toolbar {
		display: grid;
		gap: 0.75rem;
	}

	.plan-inspector--modal .plan-inspector__toolbar {
		position: sticky;
		top: 0;
		z-index: 3;
		margin: -0.2rem -0.2rem 0;
		padding: 0.2rem 0.2rem 0.35rem;
		gap: 0.45rem;
		background:
			linear-gradient(180deg, color-mix(in srgb, var(--bg-base) 96%, white) 0%, color-mix(in srgb, var(--bg-base) 88%, transparent) 100%);
	}

	.plan-inspector__toolbar-meta {
		display: flex;
		justify-content: space-between;
		align-items: center;
		flex-wrap: wrap;
		gap: 0.5rem 0.75rem;
	}

	.plan-inspector__tabs,
	.plan-inspector__actions,
	.plan-inspector__summary,
	.plan-history__preview-actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
	}

	.plan-inspector--compact {
		gap: 0.65rem;
	}

	.plan-inspector--compact .plan-inspector__tabs {
		flex-wrap: wrap;
		overflow: visible;
		gap: 0.35rem;
	}

	.plan-inspector--compact .plan-inspector__toolbar-meta {
		gap: 0.4rem 0.6rem;
	}

	.plan-inspector--compact .plan-inspector__actions,
	.plan-inspector--compact .plan-inspector__summary {
		gap: 0.35rem;
	}

	.plan-inspector__tabs button,
	.plan-history__item {
		border: 1px solid color-mix(in srgb, var(--border-soft) 72%, transparent);
		background: color-mix(in srgb, var(--bg-base) 92%, transparent);
		color: var(--text-primary);
		border-radius: 999px;
		padding: 0.45rem 0.8rem;
		font: inherit;
		cursor: pointer;
	}

	.plan-inspector--compact .plan-inspector__tabs button {
		padding: 0.36rem 0.66rem;
		font-size: 0.78rem;
		white-space: nowrap;
	}

	.plan-inspector--compact .plan-inspector__actions :global(.native-button) {
		min-height: 1.78rem !important;
		padding: 0.26rem 0.58rem !important;
		font-size: 0.74rem !important;
		line-height: 1.12 !important;
	}

	.plan-inspector--compact .plan-inspector__summary :global(.native-badge) {
		font-size: 0.72rem !important;
		padding: 0.18rem 0.42rem !important;
	}

	.plan-inspector__tabs button.active {
		background: color-mix(in srgb, var(--accent-primary) 14%, transparent);
		border-color: color-mix(in srgb, var(--accent-primary) 45%, transparent);
	}

	.plan-inspector__save-row {
		display: grid;
		gap: 0.45rem;
	}

	.plan-inspector__save-input {
		width: 100%;
		border-radius: 0.8rem;
		border: 1px solid color-mix(in srgb, var(--border-soft) 72%, transparent);
		padding: 0.65rem 0.8rem;
		background: var(--bg-base);
		color: var(--text-primary);
		font: inherit;
	}

	.plan-inspector__save-copy,
	.plan-inspector__mode-copy,
	.plan-history__muted,
	.plan-history__preview-header p {
		margin: 0;
		color: var(--text-muted);
		font-size: 0.84rem;
	}

	.plan-inspector--compact .plan-inspector__mode-copy {
		font-size: 0.75rem;
		line-height: 1.2;
		white-space: nowrap;
	}

	.plan-inspector__error {
		margin: 0;
		color: var(--color-error);
		font-size: 0.86rem;
	}

	.plan-history {
		display: grid;
		grid-template-columns: minmax(220px, 280px) minmax(0, 1fr);
		gap: 1rem;
		align-items: start;
	}

	.plan-history__list,
	.plan-history__preview {
		display: grid;
		gap: 0.75rem;
	}

	.plan-history__list-header,
	.plan-history__preview-header {
		display: flex;
		justify-content: space-between;
		gap: 0.75rem;
		align-items: flex-start;
	}

	.plan-history__items {
		display: grid;
		gap: 0.55rem;
	}

	.plan-history__item {
		display: grid;
		gap: 0.15rem;
		text-align: left;
		border-radius: 0.9rem;
		padding: 0.8rem 0.9rem;
	}

	.plan-history__item.selected {
		background: color-mix(in srgb, var(--accent-primary) 10%, transparent);
		border-color: color-mix(in srgb, var(--accent-primary) 45%, transparent);
	}

	.plan-history__loading {
		padding: 0.85rem 0;
	}

	.plan-artifact-pane,
	.plan-artifact-pane--clarifications,
	.plan-attempts,
	.plan-clarification-list {
		display: grid;
		gap: 0.85rem;
	}

	.plan-artifact-detail,
	.plan-attempt,
	.plan-clarification-card {
		border-radius: 0.9rem;
		border: 1px solid color-mix(in srgb, var(--border-soft) 72%, transparent);
		background: color-mix(in srgb, var(--bg-card) 90%, var(--bg-soft) 10%);
		padding: 0.8rem 0.9rem;
	}

	.plan-artifact-detail summary,
	.plan-attempt summary,
	.plan-clarification-card summary {
		cursor: pointer;
		font-weight: 600;
	}

	.plan-attempt__summary,
	.plan-clarification-card__summary,
	.plan-clarification-section__header {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		gap: 0.75rem;
		flex-wrap: wrap;
	}

	.plan-attempt__meta,
	.plan-clarification-card__meta,
	.plan-question-options,
	.plan-resume-grid {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
	}

	.plan-clarification-section {
		display: grid;
		gap: 0.75rem;
	}

	.plan-clarification-card__meta span,
	.plan-attempt__meta span {
		color: var(--text-muted);
		font-size: 0.82rem;
	}

	.plan-question-option {
		display: inline-flex;
		align-items: center;
		padding: 0.3rem 0.55rem;
		border-radius: 999px;
		background: color-mix(in srgb, var(--accent-primary) 10%, transparent);
		color: var(--text-primary);
		font-size: 0.78rem;
	}

	.plan-question-option--clickable {
		border: 0;
		cursor: pointer;
		transition: background 120ms ease;
	}

	.plan-question-option--clickable:hover:not(:disabled) {
		background: color-mix(in srgb, var(--accent-primary) 22%, transparent);
	}

	.plan-question-option--clickable:disabled {
		cursor: not-allowed;
		opacity: 0.6;
	}

	.plan-question-response {
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
		margin-top: 0.65rem;
	}

	.plan-question-response__input {
		font-family: inherit;
		font-size: 0.85rem;
		padding: 0.45rem 0.55rem;
		border: 1px solid var(--border-soft);
		border-radius: 0.45rem;
		background: var(--bg-soft);
		color: var(--text-primary);
		resize: vertical;
	}

	.plan-question-response__actions {
		display: flex;
		justify-content: flex-end;
	}

	.plan-question-response__error {
		color: var(--severity-error-fg);
		font-size: 0.78rem;
		margin: 0;
	}

	.plan-inspector__save-textarea {
		min-height: 8.5rem;
		resize: vertical;
		font-family: var(--font-mono);
		font-size: 0.8rem;
	}

	@media (max-width: 900px) {
		.plan-history {
			grid-template-columns: 1fr;
		}
	}
</style>
