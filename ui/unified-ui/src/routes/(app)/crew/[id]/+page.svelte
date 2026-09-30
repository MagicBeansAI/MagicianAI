<script lang="ts">
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { onDestroy, onMount } from 'svelte';
	import { get } from 'svelte/store';
	import {
		getAgentSnapshot,
		loadAgent,
		setPrimary,
		agentList,
		systemAgentList,
		type AgentSummary,
		type SystemAgentSummary
	} from '$lib/stores/agentStore';
	import {
		taskStore,
		approveTask,
		batchApproveTasks,
		type Task,
		type TaskStatus
	} from '$lib/stores/taskStore';
	import type { TaskCreatedBy } from '$lib/types/agents';
	import NativeCrewRenderer from '$lib/magician/crew/NativeCrewRenderer.svelte';
	import CrewMemberOverview from '$lib/magician/crew/CrewMemberOverview.svelte';
	import CrewHealthOverview from '$lib/magician/crew/CrewHealthOverview.svelte';
	import EffectiveToolPolicyPanel from '$lib/magician/crew/EffectiveToolPolicyPanel.svelte';
	import AgentModelPinsPanel from '$lib/magician/crew/AgentModelPinsPanel.svelte';
	import {
		fetchAgentHealth,
		type CrewAgentHealthResponse
	} from '$lib/magician/crew/health';
	import {
		buildCrewMemberOverviewComponent,
		CREW_MEMBER_OVERVIEW_COMPONENT_ID,
		type CrewMemberOverviewModel
	} from '$lib/magician/crew/overview';
	import type { CrewNativeComponent, CrewNativeInteractionEventDetail } from '$lib/magician/crew/nativeSurface';
	import { v2Events } from '$lib/realtime/v2-websocket';
	import LiveAgentSurface from '$lib/magician/components/LiveAgentSurface.svelte';
	import { reportPrestoRouteValidation } from '$lib/magician/presto/validation';
	/*
	 * An agent cycle opens **the** task panel, in the shared drawer. It is not a
	 * task and never was — it is a run, and `toExecutionPanelModel` is what maps
	 * one onto the panel's model. The Plan act is absent because a cycle has no
	 * plan, and the Output act is absent because no outputs endpoint answers for
	 * an `agent-cycle:` id; that absence is the capability model working, not a
	 * gap left open.
	 */
	import TaskPanelDrawer from '$lib/magician/tasks/TaskPanelDrawer.svelte';
	import {
		fetchExecutionPanelState,
		toExecutionPanelModel,
		type ExecutionPanelTarget
	} from '$lib/magician/tasks/executionPanelModel';
	import { streamExecutionPanelState } from '$lib/magician/tasks/executionPanelStream';
	import type { ExecutionPanelState } from '$lib/types/executionPanel';
	import {
		fetchAgentDefinitionRecord,
		serializeDefinitionYaml,
		type AgentDefinitionRecord
	} from '../definitionApi';
	import { showSuccess, showError } from '$lib/shared/stores/notifications';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { cronToHumanReadable } from '$lib/utils/cron';
	import { timedFetch } from '$lib/shared/fetch';

	type DetailTabId = 'overview' | 'memory' | 'episodes' | 'corrections' | 'config';

	const DETAIL_TABS: Array<{ id: DetailTabId; label: string }> = [
		{ id: 'overview', label: 'Overview' },
		{ id: 'memory', label: 'Memory' },
		{ id: 'episodes', label: 'History' },
		{ id: 'corrections', label: 'Corrections' },
		{ id: 'config', label: 'Settings (YAML)' }
	];

	interface AgentMemoryTierSummary {
		tier_name: string;
		scope: 'agent' | 'agent_goal' | 'user';
		description: string;
		render_format: string;
		renderer: string;
		shared: boolean;
		goal_id?: string;
		available_goal_ids?: string[];
		has_data: boolean;
		last_updated?: string;
	}

	interface AgentMemorySummaryResponse {
		agent_id: string;
		tiers: AgentMemoryTierSummary[];
		total_count: number;
	}

	interface TierHealthRow {
		name: string;
		scope: string;
		last_consolidated_at?: string;
		pending_episode_count: number;
		status: 'healthy' | 'stale' | 'error';
	}

	interface ConsolidationHealthResponse {
		agent_id: string;
		tiers: Array<{
			tier_name: string;
			scope: string;
			last_consolidated_at?: string;
			pending_episode_count: number;
			error?: string;
		}>;
	}

	interface UserKnowledgeData {
		preferences: Array<{ key: string; value: string }>;
		contacts: Array<{ name: string; relationship: string; last_mentioned?: string }>;
		expertise: string[];
	}

	interface ApiErrorPayload {
		code?: string;
		error?: string;
		message?: string;
		details?: Record<string, unknown>;
	}

	const DOUBLE_DETAIL_SCHEMA_VERSION = 'presto.double-detail-v1';

	let isLoading = false;
	let error: string | null = null;
	let routeParamError: string | null = null;
	let agentId = '';
	let summary: AgentSummary | undefined;
	let record: AgentDefinitionRecord | null = null;
	let activeRequestId = '';
	let activeRequestToken = 0;
	let lastHydratedScopeKey = '';
	let loreSummaries: AgentMemoryTierSummary[] = [];
	let loreSummariesLoading = false;
	let loreSummariesError: string | null = null;
	let loreSummariesRequestId = 0;
	let definition: Record<string, unknown> | null = null;
	let selectedTab: DetailTabId = 'overview';
	let memoryTiers: unknown[] = [];
	let memoryConsolidation: unknown[] = [];
	let feedbackLoops: unknown[] = [];
	let definitionName: string | undefined;
	let definitionAgentId: string | undefined;
	let retention: Record<string, unknown> | null = null;
	let retentionEpisodes: unknown;
	let retentionCorrections: unknown;
	let configYaml = '';
	let displayAgentName = agentId;
	let displayAgentId = agentId;
	let displayStatus = 'read-only internal';
	let sortedMemoryTiers: unknown[] = [];
	let executionPanelOpen = false;
	/**
	 * The cycle this page can open a panel on: the agent's current execution,
	 * under the synthetic `agent-cycle:<agent>:<cycle>` id that routes it to the
	 * execution-scoped panel endpoint.
	 *
	 * It used to be a synthesized `Task` — a fake record with a fake title and
	 * `Date.now()` for both timestamps — because the panel it fed only accepted
	 * one. The panel takes a `TaskPanelModel` now and a cycle maps onto that
	 * directly, so there is nothing left to pretend and no `Task` here at all.
	 */
	let executionPanelTarget: ExecutionPanelTarget | null = null;
	let executionPanelTitle = '';
	let executionPanelCycleId = '';
	let canOpenExecutionPanel = false;
	/*
	 * What the drawer renders, and the request that fills it. Keyed on the run,
	 * so a reply for a cycle the reader has left cannot land under this one.
	 */
	let executionPanelState: ExecutionPanelState | null = null;
	let executionPanelStateKey: string | null = null;
	let executionPanelRequestId = 0;
	let executionPanelLoadError: string | null = null;
	/**
	 * **Which cycle the drawer is currently set up for**, and the subscription
	 * that keeps it current.
	 *
	 * One key, read by one function, deciding one thing: has the *subject*
	 * changed? Both the fetch and the socket subscription hang off that single
	 * answer, because the two questions they would otherwise ask separately —
	 * "should I re-read?" and "should I re-subscribe?" — are the same question,
	 * and two guards over one condition is two things to keep in step.
	 */
	let executionPanelSubjectKey: string | null = null;
	let executionPanelStreamStop: (() => void) | null = null;
	let executionPanelNow = Date.now();
	let executionPanelClock: ReturnType<typeof setInterval> | null = null;
	let executionPanelLoadedAt: number | null = null;
	const EXECUTION_PANEL_CLOCK_MS = 60_000;
	let components: CrewNativeComponent[] = [];
	let headerComponents: CrewNativeComponent[] = [];
	let detailComponents: CrewNativeComponent[] = [];
	let detailComponentsBeforeOverview: CrewNativeComponent[] = [];
	let detailComponentsAfterOverview: CrewNativeComponent[] = [];
	let overviewComponentIndex = -1;
	let crewOverview: CrewMemberOverviewModel;
	let lastRouteContractDigest = '';
	let systemAgent: SystemAgentSummary | undefined;
	let isSystemAgent = false;
	let crewHealth: CrewAgentHealthResponse | null = null;
	let crewHealthLoading = false;

	// Consolidation health state (Task 4.5.10)
	let tierHealthRows: TierHealthRow[] = [];
	let tierHealthLoading = false;
	let tierHealthError: string | null = null;
	let tierHealthRequestId = 0;

	// User knowledge state (Task 4.5.12)
	let userKnowledge: UserKnowledgeData = { preferences: [], contacts: [], expertise: [] };
	let userKnowledgeLoading = false;
	let userKnowledgeError: string | null = null;
	let userKnowledgeRequestId = 0;
	let hasLocalUserMemory = false;

	// Merge to shared knowledge state (Task 4.5.13)
	let isMergingUserMemory = false;
	let mergeUserMemoryMessage: string | null = null;
	let mergeUserMemoryError: string | null = null;

	// Set primary agent state (Task 2.22)
	let isSettingPrimary = false;

	// Autonomous proposals inbox (Task 5.11)
	let autonomousProposals: Task[] = [];
	let proposalCycles: Map<string, Task[]> = new Map();
	let isApprovingAll = false;
	let proposalsLoading = false;
	let proposalsError: string | null = null;
	let proposalsRequestId = 0;

	// Autonomous schedule state (Task 5.10)
	interface BootstrapTaskData {
		id: string;
		status: string;
		last_completed_at?: string;
		outcome?: string;
		error?: string;
	}

	let autonomousConfig: {
		schedule: string;
		focus_areas: Array<{
			name: string;
			description: string;
			priority: string;
			schedule?: string;
			program?: string;
			scope?: string[];
		}>;
		max_tasks_per_cycle: number;
		max_steps_per_plan: number;
	} | null = null;
	let harnessProgramSection: string | null = null;
	let bootstrapTask: BootstrapTaskData | null = null;
	let bootstrapTaskLoading = false;
	let bootstrapTaskError: string | null = null;
	let bootstrapTaskRequestId = 0;
	let harnessEnabled = false;
	let harnessOverview: Record<string, unknown> | null = null;
	let harnessOverviewLoading = false;
	let harnessOverviewError: string | null = null;
	let harnessOverviewRequestId = 0;
	let harnessOwnerRequests: Record<string, unknown>[] = [];
	let harnessOwnerRequestHistory: Record<string, unknown>[] = [];
	let harnessOwnerRequestsLoading = false;
	let harnessOwnerRequestsError: string | null = null;
	let harnessOwnerRequestsRequestId = 0;
	let harnessStructuralProposals: Record<string, unknown>[] = [];
	let harnessStructuralProposalsLoading = false;
	let harnessStructuralProposalsError: string | null = null;
	let harnessStructuralProposalsRequestId = 0;
	let selectedStructuralProposalId: string | null = null;
	let structuralProposalActionId: string | null = null;
	let ownerRequestActionId: string | null = null;

	// New unified agentic architecture fields
	let agentKind = '';
	let capabilityPacks: string[] = [];
	let excludedPacks: string[] = [];
	let delegateTo: string[] = [];
	let maxDelegationDepth = 1;
	let readableAgents: string[] = [];
	let userMemoryIsolationValue: string = 'shared';

	$: decoded = decodeAgentIdParam(($page.params.id || '').trim());
	$: agentId = decoded.agentId;
	$: routeParamError = decoded.error;
	$: selectedTab = normalizeTab($page.url.searchParams.get('tab'));
	$: definition = isSystemAgent ? null : record?.definition || null;
	$: memoryTiers = definition ? readArray(definition, 'memory_tiers') : [];
	$: memoryConsolidation = definition ? readArray(definition, 'memory_consolidation') : [];
	$: feedbackLoops = definition ? readArray(definition, 'feedback_loops') : [];
	$: sortedMemoryTiers = [...memoryTiers].sort(sortMemoryTiersByScopeAndName);
	$: definitionName = definition ? readString(definition, 'name') : undefined;
	$: definitionAgentId = definition ? readString(definition, 'agent_id') : undefined;
	$: retention = definition ? asRecord(definition['retention']) : null;
	$: retentionEpisodes = retention ? retention['episodes'] : undefined;
	$: retentionCorrections = retention ? retention['corrections'] : undefined;
	$: configYaml = definition ? serializeDefinitionYaml(definition) : '';
	$: executionPanelTarget = buildAgentCycleTarget(agentId, summary);
	$: executionPanelTitle = `${definitionName || agentId} · cycle`;
	$: executionPanelCycleId = (summary?.current_cycle_id || '').trim();
	$: canOpenExecutionPanel = !isSystemAgent && !!(executionPanelTarget && executionPanelCycleId);

	// Extract kind, capability packs, coordination from definition
	$: agentKind = definition ? (readString(definition, 'kind') || summary?.kind || 'Personal') : (summary?.kind || 'Personal');
	$: capabilityPacks = definition ? readStringArray(definition, 'tools') : (summary?.tools || []);
	$: excludedPacks = definition ? readStringArray(definition, 'excluded_tools') : (summary?.excluded_tools || []);
	$: {
		delegateTo = definition ? readStringArray(definition, 'delegation_targets') : (summary?.delegation_targets || []);
		const coordination = definition ? asRecord(definition['coordination']) : null;
		maxDelegationDepth = coordination ? readFiniteNumber(coordination, 'max_delegation_depth', 1) : (summary?.max_delegation_depth ?? 1);
	}

	// Extract readable_agents and user_memory_isolation from definition/summary (Task 2.22)
	$: readableAgents = definition ? readStringArray(definition, 'readable_agents') : (summary?.readable_agents || []);
	$: userMemoryIsolationValue = definition ? (readString(definition, 'user_memory_isolation') || summary?.user_memory_isolation || 'shared') : (summary?.user_memory_isolation || 'shared');
	$: harnessProgramSection = definition
		? (readString(asRecord(definition['harness']) || {}, 'program_section') || null)
		: null;
	$: harnessEnabled = definition ? asRecord(definition['harness']) !== null : !!summary?.harness;

	// Extract autonomous config from definition (Task 5.10)
	$: {
		const autoRec = definition ? asRecord(definition['autonomous_config']) : null;
		if (autoRec) {
			const schedule = readString(autoRec, 'schedule') || '';
			const rawFocusAreas = Array.isArray(autoRec.focus_areas) ? autoRec.focus_areas : [];
			const focusAreas = rawFocusAreas.flatMap((fa: unknown) => {
					const faRec = asRecord(fa);
					if (!faRec) return [];
					const name = readString(faRec, 'name');
					const description = readString(faRec, 'description');
					if (!name || !description) return [];
					const scope = readStringArray(faRec, 'scope');
					return [{
						name,
						description,
						priority: readString(faRec, 'priority') || 'medium',
						schedule: readString(faRec, 'schedule') || undefined,
						program: readString(faRec, 'program') || undefined,
						scope: scope.length > 0 ? scope : undefined
					}];
				});
			autonomousConfig = {
				schedule,
				focus_areas: focusAreas,
				max_tasks_per_cycle: readFiniteNumber(autoRec, 'max_tasks_per_cycle', 5),
				max_steps_per_plan: readFiniteNumber(autoRec, 'max_steps_per_plan', 10)
			};
		} else if (summary?.autonomous_config) {
			autonomousConfig = {
				schedule: summary.autonomous_config.schedule,
				focus_areas: summary.autonomous_config.focus_areas.map((fa) => ({
					name: fa.name,
					description: fa.description,
					priority: fa.priority,
					schedule: fa.schedule,
					program: fa.program,
					scope: fa.scope
				})),
				max_tasks_per_cycle: summary.autonomous_config.max_tasks_per_cycle,
				max_steps_per_plan: summary.autonomous_config.max_steps_per_plan
			};
		} else {
			autonomousConfig = null;
		}
	}

	// Task 5.12: Reactive update of autonomous proposals when task store changes via WebSocket.
	// The task store is updated by WebSocket events (task.created, task.updated, ExecutionStatusChanged).
	// When the store mutates, we re-derive the proposals list for this agent.
	$: if ($taskStore && agentId) {
		const storeProposals = $taskStore.tasks.filter(
			(t) =>
				t.agentId === agentId &&
				t.approved === false &&
				(t.createdBy === 'delegation' || t.createdBy === 'autonomous')
		);
		// Keep proposal UI in sync when proposals are edited, removed, or cleared.
		const proposalSignature = (tasks: Task[]) =>
			tasks
				.map((task) => [task.id, task.updatedAt, task.status, task.approved ? '1' : '0'].join(':'))
				.join(',');
		const storeProposalSignature = proposalSignature(storeProposals);
		const currentProposalSignature = proposalSignature(autonomousProposals);
		if (storeProposalSignature !== currentProposalSignature) {
			autonomousProposals = storeProposals;
			proposalCycles = groupTasksBy(
				autonomousProposals,
				(t) => {
					if (t.dependsOn && t.dependsOn.length > 0) return t.dependsOn[0];
					return 'ungrouped';
				}
			);
		}
	}

	// Task 5.12: Refresh proposals when agent cycle events arrive via WebSocket.
	// AgentCycleCompleted events indicate that the autonomous executor may have
	// created new child tasks that need approval.
	let lastProcessedCycleEventCount = 0;
	let lastProcessedHarnessSignalEventCount = 0;
	let lastProcessedDefinitionEventCount = 0;
	$: {
		const currentEvents = $v2Events;
		if (currentEvents.length > lastProcessedCycleEventCount && agentId) {
			// Only check new events since last processed
			const newEvents = currentEvents.slice(lastProcessedCycleEventCount);
			lastProcessedCycleEventCount = currentEvents.length;
			const hasCycleEvent = newEvents.some(
				(e) =>
					(e.event_type === 'AgentCycleCompleted' || e.event_type === 'AgentCycleStarted') &&
					'agent_id' in e.data &&
					(e.data as unknown as Record<string, unknown>).agent_id === agentId
			);
			if (hasCycleEvent && !proposalsLoading) {
				void fetchAutonomousProposals(agentId);
			}
			if (hasCycleEvent && harnessEnabled && !harnessOverviewLoading) {
				void refreshHarnessOperatorView(agentId);
			}
		}
	}

	$: {
		const currentEvents = $v2Events;
		if (currentEvents.length > lastProcessedDefinitionEventCount && agentId) {
			const newEvents = currentEvents.slice(lastProcessedDefinitionEventCount);
			lastProcessedDefinitionEventCount = currentEvents.length;
			const hasDefinitionChange = newEvents.some((event) => {
				if (event.event_type !== 'AgentDefinitionChanged') return false;
				const payload = asRecord(event.data);
				return readString(payload || {}, 'agent_id') === agentId;
			});
			if (hasDefinitionChange) {
				void hydrateAgent(agentId);
			}
		}
	}

	$: {
		const currentEvents = $v2Events;
		if (currentEvents.length > lastProcessedHarnessSignalEventCount && agentId && harnessEnabled) {
			const newEvents = currentEvents.slice(lastProcessedHarnessSignalEventCount);
			lastProcessedHarnessSignalEventCount = currentEvents.length;
			const hasOwnerRequestSignal = newEvents.some((event) => matchesHarnessOwnerRequestSignal(event, agentId));
			const hasStructuralProposalSignal = newEvents.some((event) =>
				matchesHarnessStructuralProposalSignal(event, agentId)
			);
			const shouldRefreshOperatorView =
				(hasOwnerRequestSignal && !harnessOwnerRequestsLoading)
				|| (hasStructuralProposalSignal && !harnessOverviewLoading);
			if (shouldRefreshOperatorView) {
				void refreshHarnessOperatorView(agentId);
			}
		}
	}

	$: displayAgentName = systemAgent?.name || definitionName || agentId;
	$: displayAgentId = systemAgent?.agent_id || definitionAgentId || agentId;
	$: displayStatus = isSystemAgent ? 'read-only internal' : formatStatus(summary);
	$: components = buildDoubleDetailSurface({
		agentId,
		routeParamError,
		isLoading,
		error,
		isSystemAgent,
		displayAgentName,
		displayAgentId,
		displayStatus,
		summary,
		record,
		selectedTab,
		agentKind,
		capabilityPacks,
		excludedPacks,
		delegateTo,
		maxDelegationDepth,
		sortedMemoryTiers,
		loreSummaries,
		loreSummariesLoading,
		loreSummariesError,
		memoryConsolidation,
		retentionEpisodes,
		feedbackLoops,
		retentionCorrections,
		configYaml,
		canOpenExecutionPanel,
		tierHealthRows,
		tierHealthLoading,
		tierHealthError,
		userKnowledge,
		userKnowledgeLoading,
		userKnowledgeError,
		hasLocalUserMemory,
		isMergingUserMemory,
		mergeUserMemoryMessage,
		mergeUserMemoryError,
		isPrimary: summary?.is_primary ?? false,
		userMemoryIsolation: userMemoryIsolationValue,
		readableAgents,
		autonomousProposals,
		proposalCycles,
		proposalsLoading,
		proposalsError,
		isApprovingAll,
		harnessEnabled,
		harnessOverview,
		harnessOverviewLoading,
		harnessOverviewError,
		harnessStructuralProposals,
		harnessStructuralProposalsLoading,
		harnessStructuralProposalsError,
		harnessOwnerRequests,
		harnessOwnerRequestHistory,
		harnessOwnerRequestsLoading,
		harnessOwnerRequestsError,
		selectedStructuralProposalId,
		structuralProposalActionId,
		ownerRequestActionId,
		harnessProgramSection,
		autonomousConfig,
		bootstrapTask,
		bootstrapTaskLoading,
		bootstrapTaskError,
		schemaVersion: DOUBLE_DETAIL_SCHEMA_VERSION
	});
	$: headerComponents = components.slice(0, 1);
	$: detailComponents = components.slice(1);
	$: overviewComponentIndex = detailComponents.findIndex(
		(component) => component.id === CREW_MEMBER_OVERVIEW_COMPONENT_ID
	);
	$: detailComponentsBeforeOverview = overviewComponentIndex >= 0
		? detailComponents.slice(0, overviewComponentIndex)
		: detailComponents;
	$: detailComponentsAfterOverview = overviewComponentIndex >= 0
		? detailComponents.slice(overviewComponentIndex + 1)
		: [];
	$: crewOverview = {
		agentKind: agentKind || null,
		capabilityPacks,
		excludedPacks,
		delegationTargets: delegateTo,
		maxDelegationDepth,
		userMemoryIsolation: userMemoryIsolationValue || null,
		readableAgents
	};
	$: if (browser && import.meta.env.DEV) {
		lastRouteContractDigest = reportPrestoRouteValidation(
			$page.url.pathname,
			components,
			lastRouteContractDigest
		);
	}

	/**
	 * The agent's current cycle, as a run to inspect — or `null` when it has
	 * none.
	 *
	 * All three ids are required and none is invented. The id it builds is not a
	 * task id and no task endpoint will answer for it; `executionPanelUrl` reads
	 * the `agent-cycle:` prefix and routes to the execution-scoped panel instead,
	 * which is the only route populated for a cycle.
	 *
	 * The agent's own `status` is deliberately **not** mapped onto a task status
	 * here any more. The panel's verdict is derived from the run's status as the
	 * backend reports it, and a second translation on this page would be a second
	 * answer — visibly so, since `statusToTaskStatus` mapped `triggered` onto
	 * `running` and had nothing to say about a queued cycle.
	 */
	function buildAgentCycleTarget(
		nextAgentId: string,
		nextSummary: AgentSummary | undefined
	): ExecutionPanelTarget | null {
		const normalizedAgentId = nextAgentId.trim();
		const normalizedCycleId = (nextSummary?.current_cycle_id || '').trim();
		const normalizedExecutionId = (nextSummary?.current_execution_id || '').trim();
		if (!normalizedAgentId || !normalizedCycleId || !normalizedExecutionId) {
			return null;
		}
		return {
			taskId: `agent-cycle:${normalizedAgentId}:${normalizedCycleId}`,
			executionId: normalizedExecutionId
		};
	}

	function openExecutionPanel(): void {
		if (!summary?.current_cycle_id || !agentId) {
			return;
		}
		executionPanelOpen = true;
	}

	function closeExecutionPanel(): void {
		executionPanelOpen = false;
	}

	/** One cycle, as a key: two runs of one agent must not share an answer. */
	function cycleKey(target: ExecutionPanelTarget): string {
		return `${target.taskId}|${target.executionId}`;
	}

	/**
	 * Read the cycle's panel state. The request id guards the reply, not the
	 * request: a late answer for a cycle the reader has left is discarded rather
	 * than rendered under the one on screen.
	 */
	async function loadExecutionPanelState(target: ExecutionPanelTarget | null): Promise<void> {
		if (target === null) {
			executionPanelRequestId += 1;
			executionPanelState = null;
			executionPanelStateKey = null;
			executionPanelLoadError = null;
			return;
		}

		const key = cycleKey(target);
		const requestId = ++executionPanelRequestId;
		executionPanelState = null;
		executionPanelStateKey = null;
		executionPanelLoadError = null;
		const scope = get(scopeIdentityStore);
		const state = await fetchExecutionPanelState(target, scope.principal, scope.workspace);
		if (requestId !== executionPanelRequestId) return;
		executionPanelState = state;
		executionPanelStateKey = state === null ? null : key;
		executionPanelLoadedAt = Date.now();
		executionPanelLoadError =
			state === null ? "This cycle's run state is no longer available." : null;
	}

	/**
	 * Point the drawer at this cycle: read it once, then follow it.
	 *
	 * **The delta stream does reach an agent cycle**, and the reason is that the
	 * synthetic `agent-cycle:` id never leaves this page. `panelDeltaState` tests
	 * scope and `executionIdOf(state)` and nothing else, and the id this target
	 * carries is the agent's real `current_execution_id`. Server-side, the panel
	 * projector resolves an execution to its task and builds the pushed state with
	 * the *same* builder `/v3/executions/{id}/execution-panel` uses for the fetch
	 * below — so the precondition for a push is precisely the precondition for the
	 * fetch already succeeding. A cycle whose state this page can load is a cycle
	 * whose state it can follow.
	 *
	 * **Everything here is gated on the cycle changing, and that is the point.**
	 * This runs from a reactive statement, so it re-runs on every unrelated
	 * re-render — and `executionPanelTarget` is rebuilt (new object, same values)
	 * whenever the agent re-hydrates. Ungated, the fetch would blank the drawer and
	 * refill it a round trip later each time, and the subscription would be torn
	 * down and re-established under a live run.
	 */
	function syncExecutionPanel(target: ExecutionPanelTarget | null): void {
		const key = target === null ? null : cycleKey(target);
		if (key === executionPanelSubjectKey) return;
		executionPanelSubjectKey = key;

		executionPanelStreamStop?.();
		executionPanelStreamStop = null;
		void loadExecutionPanelState(target);
		if (target === null) return;

		const scope = get(scopeIdentityStore);
		executionPanelStreamStop = streamExecutionPanelState(target, scope, (state) => {
			// The drawer may have moved to another cycle between the push and this
			// callback. The same guard the fetch's request id is, against the same
			// failure: one run's state rendered under another run's drawer.
			if (executionPanelSubjectKey !== key) return;
			executionPanelState = state;
			executionPanelStateKey = key;
			// A push is a successful read, so it clears the staleness line the same
			// way a successful fetch does — and the `as of` instant moves with it, or
			// the drawer would report live state as minutes old.
			executionPanelLoadError = null;
			executionPanelLoadedAt = Date.now();
		});
	}

	/** Re-read the cycle, which is all its Retry can do. */
	async function handleExecutionPanelRetry(): Promise<void> {
		await loadExecutionPanelState(executionPanelTarget);
	}

	function asRecord(value: unknown): Record<string, unknown> | null {
		return typeof value === 'object' && value !== null && !Array.isArray(value)
			? (value as Record<string, unknown>)
			: null;
	}

	function asString(value: unknown): string {
		if (typeof value === 'string') return value;
		if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') return String(value);
		return '';
	}

	function readString(record: Record<string, unknown>, key: string): string | undefined {
		const value = record[key];
		return typeof value === 'string' && value.trim().length > 0 ? value : undefined;
	}

	function readBoolean(record: Record<string, unknown> | null, key: string): boolean | undefined {
		if (!record) return undefined;
		const value = record[key];
		return typeof value === 'boolean' ? value : undefined;
	}

	function readArray(record: Record<string, unknown>, key: string): unknown[] {
		const value = record[key];
		return Array.isArray(value) ? value : [];
	}

	function readRecordArray(record: Record<string, unknown> | null, key: string): Record<string, unknown>[] {
		if (!record) return [];
		return readArray(record, key)
			.map((value) => asRecord(value))
			.filter((value): value is Record<string, unknown> => value !== null);
	}

	function readStringArray(record: Record<string, unknown> | null, key: string): string[] {
		if (!record) return [];
		const value = record[key];
		if (!Array.isArray(value)) return [];
		return value.filter((item): item is string => typeof item === 'string');
	}

	function readFiniteNumber(record: Record<string, unknown> | null, key: string, fallback: number): number {
		if (!record) return fallback;
		const value = record[key];
		if (typeof value === 'number' && Number.isFinite(value)) return value;
		return fallback;
	}

	function normalizeTaskCreatedBy(value: unknown): TaskCreatedBy {
		const normalized = typeof value === 'string' ? value.trim().toLowerCase() : '';
		if (normalized === 'agent' || normalized === 'autonomous') return 'autonomous';
		if (normalized === 'delegation') return 'delegation';
		if (normalized === 'system') return 'system';
		return 'user';
	}

	function readIsoTimestamp(value: unknown): string | undefined {
		if (typeof value === 'string' && value.trim().length > 0) return value;
		if (typeof value === 'number' && Number.isFinite(value)) return new Date(value).toISOString();
		return undefined;
	}

	function formatRatio(value: unknown): string {
		if (typeof value !== 'number' || !Number.isFinite(value)) return '—';
		return `${Math.round(value * 100)}%`;
	}

	function formatSignedRatioDelta(value: unknown): string {
		if (typeof value !== 'number' || !Number.isFinite(value)) return '—';
		const percent = Math.round(value * 100);
		return `${percent > 0 ? '+' : ''}${percent}%`;
	}

	function formatMetricValue(value: unknown, unit: string): string {
		if (typeof value !== 'number' || !Number.isFinite(value)) return '—';
		if (unit === 'ratio') return formatRatio(value);
		if (Math.abs(value - Math.round(value)) < Number.EPSILON) return String(Math.round(value));
		return value.toFixed(2);
	}

	function openTaskCount(taskStats: Record<string, unknown> | null): number {
		if (!taskStats) return 0;
		return [
			'pending',
			'planning',
			'ready',
			'running',
			'paused',
			'failed',
			'deferred'
		].reduce((sum, key) => sum + readFiniteNumber(taskStats, key, 0), 0);
	}

	function flattenV3TaskRecord(value: unknown): Record<string, unknown> | null {
		const root = asRecord(value);
		if (!root) return null;
		const manifest = asRecord(root.manifest) ?? {};
		const state = asRecord(root.state) ?? {};
		return {
			id: readString(manifest, 'task_id') || readString(state, 'task_id'),
			title: readString(manifest, 'title'),
			description: readString(manifest, 'description'),
			status: readString(state, 'status'),
			agent_id: readString(manifest, 'agent_id'),
			priority: readString(manifest, 'priority'),
			due_date: readString(manifest, 'due_date'),
			tags: readArray(manifest, 'tags'),
			created_by: readString(manifest, 'created_by'),
			depends_on: readStringArray(manifest, 'depends_on'),
			approved: manifest.approved,
			is_blocked: state.is_blocked,
			created_at: readIsoTimestamp(manifest.created_at),
			updated_at: readIsoTimestamp(state.updated_at) || readIsoTimestamp(manifest.updated_at),
			active_root_execution_id: readString(state, 'active_root_execution_id'),
			latest_root_execution_id: readString(state, 'latest_root_execution_id'),
			last_completed_root_execution_id: readString(state, 'last_completed_root_execution_id')
		};
	}

	// --- Task 5.10: Cron helpers and bootstrap task fetch ---

	function cronNextFire(cron: string): string {
		const trimmed = cron.trim();
		if (!trimmed) return 'unknown';
		const now = new Date();

		// Parse simple interval patterns to estimate next fire
		const everyNMinutes = /^\*\/(\d+)\s+\*\s+\*\s+\*\s+\*$/.exec(trimmed);
		if (everyNMinutes) {
			const n = parseInt(everyNMinutes[1], 10);
			const currentMinute = now.getMinutes();
			const nextMinute = Math.ceil((currentMinute + 1) / n) * n;
			const next = new Date(now);
			if (nextMinute >= 60) {
				next.setHours(next.getHours() + 1);
				next.setMinutes(nextMinute % 60);
			} else {
				next.setMinutes(nextMinute);
			}
			next.setSeconds(0);
			next.setMilliseconds(0);
			return formatRelativeTime(next.getTime());
		}

		const everyNHours = /^0\s+\*\/(\d+)\s+\*\s+\*\s+\*$/.exec(trimmed);
		if (everyNHours) {
			const n = parseInt(everyNHours[1], 10);
			const currentHour = now.getHours();
			const nextHour = Math.ceil((currentHour + 1) / n) * n;
			const next = new Date(now);
			if (nextHour >= 24) {
				next.setDate(next.getDate() + 1);
				next.setHours(nextHour % 24);
			} else {
				next.setHours(nextHour);
			}
			next.setMinutes(0);
			next.setSeconds(0);
			next.setMilliseconds(0);
			return formatRelativeTime(next.getTime());
		}

		const everyNDays = /^0\s+0\s+\*\/(\d+)\s+\*\s+\*$/.exec(trimmed);
		if (everyNDays) {
			const n = parseInt(everyNDays[1], 10);
			const next = new Date(now);
			next.setDate(next.getDate() + n);
			next.setHours(0);
			next.setMinutes(0);
			next.setSeconds(0);
			next.setMilliseconds(0);
			return formatRelativeTime(next.getTime());
		}

		const dailyAtHour = /^0\s+(\d+)\s+\*\s+\*\s+\*$/.exec(trimmed);
		if (dailyAtHour) {
			const h = parseInt(dailyAtHour[1], 10);
			const next = new Date(now);
			next.setHours(h);
			next.setMinutes(0);
			next.setSeconds(0);
			next.setMilliseconds(0);
			if (next.getTime() <= now.getTime()) {
				next.setDate(next.getDate() + 1);
			}
			return formatRelativeTime(next.getTime());
		}

		return 'unknown';
	}

	function bootstrapTaskStatus(task: BootstrapTaskData | null, config: typeof autonomousConfig): 'active' | 'paused' | 'error' | 'unknown' {
		if (!config) return 'unknown';
		if (!task) return 'active'; // Config exists but no task run yet — assumed active/pending
		if (task.status === 'failed' || task.error) return 'error';
		if (task.status === 'paused') return 'paused';
		return 'active';
	}

	function bootstrapStatusColor(status: 'active' | 'paused' | 'error' | 'unknown'): 'success' | 'warning' | 'error' | 'default' {
		if (status === 'active') return 'success';
		if (status === 'paused') return 'warning';
		if (status === 'error') return 'error';
		return 'default';
	}

	async function fetchBootstrapTask(nextAgentId: string): Promise<void> {
		const normalizedAgentId = nextAgentId.trim();
		if (!normalizedAgentId) {
			bootstrapTask = null;
			bootstrapTaskLoading = false;
			bootstrapTaskError = null;
			return;
		}

		const requestId = ++bootstrapTaskRequestId;
		const scopeKey = currentCrewScopeKey();
		bootstrapTaskLoading = true;
		bootstrapTaskError = null;
		try {
			const bootstrapId = `bootstrap__${normalizedAgentId}`;
			const response = await timedFetch(`/api/magician/v3/tasks/${encodeURIComponent(bootstrapId)}`);
			if (
				requestId !== bootstrapTaskRequestId
				|| normalizedAgentId !== (agentId || '').trim()
				|| scopeKey !== currentCrewScopeKey()
			) {
				return;
			}
			if (response.status === 404) {
				bootstrapTask = null;
				return;
			}
			if (!response.ok) {
				let message = `Failed to load bootstrap task (${response.status})`;
				try {
					const payload = await response.json();
					const parsed = parseApiError(payload);
					message = `${message}: ${parsed.message}`;
				} catch {
					// best effort
				}
				throw new Error(message);
			}
			const payload = (await response.json()) as Record<string, unknown>;
			if (
				requestId !== bootstrapTaskRequestId
				|| normalizedAgentId !== (agentId || '').trim()
				|| scopeKey !== currentCrewScopeKey()
			) {
				return;
			}
			const taskRecord = flattenV3TaskRecord(payload.task);
			if (taskRecord) {
				bootstrapTask = {
					id: readString(taskRecord, 'id') || readString(taskRecord, 'task_id') || bootstrapId,
					status: readString(taskRecord, 'status') || 'unknown',
					last_completed_at:
						readString(taskRecord, 'last_completed_at') ||
						readString(taskRecord, 'updated_at') ||
						undefined,
					outcome: readString(taskRecord, 'outcome') || undefined,
					error: readString(taskRecord, 'error') || undefined
				};
			}
		} catch (err) {
			if (
				requestId !== bootstrapTaskRequestId
				|| normalizedAgentId !== (agentId || '').trim()
				|| scopeKey !== currentCrewScopeKey()
			) {
				return;
			}
			bootstrapTask = null;
			bootstrapTaskError = err instanceof Error ? err.message : 'Failed to load bootstrap task';
		} finally {
			if (
				requestId === bootstrapTaskRequestId
				&& normalizedAgentId === (agentId || '').trim()
				&& scopeKey === currentCrewScopeKey()
			) {
				bootstrapTaskLoading = false;
			}
		}
	}

	async function fetchHarnessOverview(nextAgentId: string, enabled = harnessEnabled): Promise<void> {
		const normalizedAgentId = nextAgentId.trim();
		if (!normalizedAgentId || !enabled) {
			harnessOverview = null;
			harnessOverviewLoading = false;
			harnessOverviewError = null;
			return;
		}

		const requestId = ++harnessOverviewRequestId;
		const scopeKey = currentCrewScopeKey();
		harnessOverviewLoading = true;
		harnessOverviewError = null;
		try {
			const response = await timedFetch(
				`/api/magician/v2/agents/${encodeURIComponent(normalizedAgentId)}/harness-overview`
			);
			if (
				requestId !== harnessOverviewRequestId
				|| normalizedAgentId !== (agentId || '').trim()
				|| scopeKey !== currentCrewScopeKey()
			) {
				return;
			}
			if (response.status === 400 || response.status === 404) {
				harnessOverview = null;
				return;
			}
			if (!response.ok) {
				let message = `Failed to load harness overview (${response.status})`;
				try {
					const payload = await response.json();
					const parsedError = parseApiError(payload);
					message = `${message}: ${parsedError.message}`;
				} catch {
					// best effort
				}
				throw new Error(message);
			}
			const payload = (await response.json()) as unknown;
			const overview = asRecord(payload);
			if (!overview) {
				throw new Error('Malformed harness overview payload');
			}
			if (
				requestId !== harnessOverviewRequestId
				|| normalizedAgentId !== (agentId || '').trim()
				|| scopeKey !== currentCrewScopeKey()
			) {
				return;
			}
			harnessOverview = overview;
		} catch (err) {
			if (
				requestId !== harnessOverviewRequestId
				|| normalizedAgentId !== (agentId || '').trim()
				|| scopeKey !== currentCrewScopeKey()
			) {
				return;
			}
			harnessOverview = null;
			harnessOverviewError = err instanceof Error ? err.message : 'Failed to load harness overview';
		} finally {
			if (
				requestId === harnessOverviewRequestId
				&& normalizedAgentId === (agentId || '').trim()
				&& scopeKey === currentCrewScopeKey()
			) {
				harnessOverviewLoading = false;
			}
		}
	}

	async function fetchHarnessOwnerRequests(nextAgentId: string, enabled = harnessEnabled): Promise<void> {
		const normalizedAgentId = nextAgentId.trim();
		if (!normalizedAgentId || !enabled) {
			harnessOwnerRequests = [];
			harnessOwnerRequestHistory = [];
			harnessOwnerRequestsLoading = false;
			harnessOwnerRequestsError = null;
			return;
		}

		const requestId = ++harnessOwnerRequestsRequestId;
		const scopeKey = currentCrewScopeKey();
		harnessOwnerRequestsLoading = true;
		harnessOwnerRequestsError = null;
		try {
			const query = new URLSearchParams({
				owner_agent_id: normalizedAgentId,
				include_history: 'true',
				history_limit: '40'
			});
			const response = await timedFetch(`/api/magician/v2/user-requests?${query.toString()}`);
			if (
				requestId !== harnessOwnerRequestsRequestId
				|| normalizedAgentId !== (agentId || '').trim()
				|| scopeKey !== currentCrewScopeKey()
			) {
				return;
			}
			if (!response.ok) {
				let message = `Failed to load owner briefings (${response.status})`;
				try {
					const payload = await response.json();
					const parsedError = parseApiError(payload);
					message = `${message}: ${parsedError.message}`;
				} catch {
					// best effort
				}
				throw new Error(message);
			}
			const payload = asRecord(await response.json());
			if (!payload) {
				throw new Error('Malformed owner-request payload');
			}
			if (
				requestId !== harnessOwnerRequestsRequestId
				|| normalizedAgentId !== (agentId || '').trim()
				|| scopeKey !== currentCrewScopeKey()
			) {
				return;
			}
			const requests = readArray(payload, 'requests')
				.map((entry) => asRecord(entry))
				.filter((entry): entry is Record<string, unknown> => entry !== null)
				.filter((entry) => matchesHarnessOwnerRequest(entry, normalizedAgentId))
				.sort((left, right) => {
					const leftCreated =
						typeof left.created_at === 'number' && Number.isFinite(left.created_at)
							? left.created_at
							: 0;
					const rightCreated =
						typeof right.created_at === 'number' && Number.isFinite(right.created_at)
							? right.created_at
							: 0;
					return rightCreated - leftCreated;
				});
			const history = readArray(payload, 'history')
				.map((entry) => asRecord(entry))
				.filter((entry): entry is Record<string, unknown> => entry !== null)
				.filter((entry) => matchesHarnessOwnerRequest(entry, normalizedAgentId))
				.sort((left, right) => ownerRequestActivityTimestamp(right) - ownerRequestActivityTimestamp(left));
			harnessOwnerRequests = requests;
			harnessOwnerRequestHistory = history;
		} catch (err) {
			if (
				requestId !== harnessOwnerRequestsRequestId
				|| normalizedAgentId !== (agentId || '').trim()
				|| scopeKey !== currentCrewScopeKey()
			) {
				return;
			}
			harnessOwnerRequests = [];
			harnessOwnerRequestHistory = [];
			harnessOwnerRequestsError =
				err instanceof Error ? err.message : 'Failed to load owner briefings';
		} finally {
			if (
				requestId === harnessOwnerRequestsRequestId
				&& normalizedAgentId === (agentId || '').trim()
				&& scopeKey === currentCrewScopeKey()
			) {
				harnessOwnerRequestsLoading = false;
			}
		}
	}

	async function fetchHarnessStructuralProposals(
		nextAgentId: string,
		enabled = harnessEnabled
	): Promise<void> {
		const normalizedAgentId = nextAgentId.trim();
		if (!normalizedAgentId || !enabled) {
			harnessStructuralProposals = [];
			harnessStructuralProposalsLoading = false;
			harnessStructuralProposalsError = null;
			return;
		}

		const requestId = ++harnessStructuralProposalsRequestId;
		const scopeKey = currentCrewScopeKey();
		harnessStructuralProposalsLoading = true;
		harnessStructuralProposalsError = null;
		try {
			const response = await timedFetch('/api/magician/v2/proposals');
			if (
				requestId !== harnessStructuralProposalsRequestId
				|| normalizedAgentId !== (agentId || '').trim()
				|| scopeKey !== currentCrewScopeKey()
			) {
				return;
			}
			if (!response.ok) {
				let message = `Failed to load structural proposals (${response.status})`;
				try {
					const payload = await response.json();
					const parsedError = parseApiError(payload);
					message = `${message}: ${parsedError.message}`;
				} catch {
					// best effort
				}
				throw new Error(message);
			}
			const payload = asRecord(await response.json());
			if (!payload) {
				throw new Error('Malformed structural proposal payload');
			}
			if (
				requestId !== harnessStructuralProposalsRequestId
				|| normalizedAgentId !== (agentId || '').trim()
				|| scopeKey !== currentCrewScopeKey()
			) {
				return;
			}
			const proposals = readArray(payload, 'proposals')
				.map((entry) => asRecord(entry))
				.filter((entry): entry is Record<string, unknown> => entry !== null)
				.filter((entry) => matchesHarnessStructuralProposal(entry, normalizedAgentId))
				.filter((entry) => isHarnessStructuralProposalActionable(entry))
				.sort((left, right) => {
					const leftCreated = Date.parse(readString(left, 'created_at') || '') || 0;
					const rightCreated = Date.parse(readString(right, 'created_at') || '') || 0;
					return rightCreated - leftCreated;
				});
			harnessStructuralProposals = proposals;
			if (
				selectedStructuralProposalId
				&& !proposals.some(
					(proposal) => readString(proposal, 'proposal_id') === selectedStructuralProposalId
				)
			) {
				selectedStructuralProposalId = null;
			}
		} catch (err) {
			if (
				requestId !== harnessStructuralProposalsRequestId
				|| normalizedAgentId !== (agentId || '').trim()
				|| scopeKey !== currentCrewScopeKey()
			) {
				return;
			}
			harnessStructuralProposals = [];
			harnessStructuralProposalsError =
				err instanceof Error ? err.message : 'Failed to load structural proposals';
		} finally {
			if (
				requestId === harnessStructuralProposalsRequestId
				&& normalizedAgentId === (agentId || '').trim()
				&& scopeKey === currentCrewScopeKey()
			) {
				harnessStructuralProposalsLoading = false;
			}
		}
	}

	async function refreshHarnessOperatorView(
		nextAgentId: string,
		enabled = harnessEnabled
	): Promise<void> {
		const normalizedAgentId = nextAgentId.trim();
		if (!normalizedAgentId) return;
		await Promise.allSettled([
			fetchHarnessOverview(normalizedAgentId, enabled),
			fetchHarnessStructuralProposals(normalizedAgentId, enabled),
			fetchHarnessOwnerRequests(normalizedAgentId, enabled)
		]);
	}

	function parseApiError(payload: unknown): ApiErrorPayload & { details: Record<string, unknown> } {
		const root = asRecord(payload);
		if (!root) return { message: 'Request failed', details: {} };
		const nested = asRecord(root['details']);
		const rootMessage =
			readString(root, 'error') ||
			readString(root, 'message') ||
			readString(nested || {}, 'reason') ||
			'Request failed';
		return {
			code: readString(root, 'code'),
			message: rootMessage,
			details: nested || {}
		};
	}

	function parseGoalIds(value: unknown): string[] {
		const raw = Array.isArray(value) ? value : [];
		const normalized: string[] = [];
		for (const candidate of raw) {
			if (typeof candidate !== 'string') continue;
			const trimmed = candidate.trim();
			if (!trimmed || normalized.includes(trimmed)) continue;
			normalized.push(trimmed);
		}
		return normalized;
	}

	function scopeSortValue(scope: string): number {
		switch ((scope || '').trim().toLowerCase()) {
			case 'agent':
				return 0;
			case 'agent_goal':
			case 'agent-goal':
				return 1;
			case 'user':
				return 2;
			default:
				return 3;
		}
	}

	function sortMemoryTiersByScopeAndName(left: unknown, right: unknown): number {
		const leftRecord = asRecord(left);
		const rightRecord = asRecord(right);
		const leftScope = readString(leftRecord || {}, 'scope') || 'agent';
		const rightScope = readString(rightRecord || {}, 'scope') || 'agent';
		const scopeComparison = scopeSortValue(leftScope) - scopeSortValue(rightScope);
		if (scopeComparison !== 0) {
			return scopeComparison;
		}
		const leftName = readString(leftRecord || {}, 'name') || '';
		const rightName = readString(rightRecord || {}, 'name') || '';
		return leftName.localeCompare(rightName, 'en', { sensitivity: 'base', numeric: true });
	}

	function findLoreSummary(tierName: string): AgentMemoryTierSummary | undefined {
		if (!tierName) return undefined;
		for (const summaryEntry of loreSummaries) {
			if (summaryEntry.tier_name === tierName) return summaryEntry;
		}
		return undefined;
	}

	function decodeAgentIdParam(rawId: string): { agentId: string; error: string | null } {
		if (!rawId) {
			return { agentId: '', error: null };
		}
		return {
			agentId: rawId,
			error: null
		};
	}

	function isSystemAgentId(nextAgentId: string): boolean {
		return nextAgentId.startsWith('system:');
	}

	function currentCrewScopeKey(): string {
		const scope = get(scopeIdentityStore);
		return `${scope.principal}:${scope.workspace}`;
	}

	function normalizeTab(raw: string | null): DetailTabId {
		const normalized = (raw || '').trim().toLowerCase();
		if (normalized === 'overview') return 'overview';
		if (normalized === 'memory') return 'memory';
		if (normalized === 'episodes') return 'episodes';
		if (normalized === 'corrections') return 'corrections';
		if (normalized === 'config') return 'config';
		return 'overview';
	}

	function memoryTierHref(tierRecord: Record<string, unknown> | null, fallbackName: string, loreSummary?: AgentMemoryTierSummary | null): string {
		const tierName = readString(tierRecord || {}, 'name') || fallbackName;
		const scope = readString(tierRecord || {}, 'scope') || 'agent';
		const base = `/crew/${encodeURIComponent(agentId)}/memory/${encodeURIComponent(tierName)}`;
		if (scope !== 'agent_goal') return base;
		const goalId =
			loreSummary?.goal_id ||
			parseGoalIds(loreSummary?.available_goal_ids).find((candidate) => candidate.length > 0) ||
			summary?.current_goal_id;
		if (!goalId) return base;
		return `${base}?goal_id=${encodeURIComponent(goalId)}`;
	}

	async function fetchLoreSummaries(nextAgentId: string, nextGoalId?: string, allowRetry = true): Promise<void> {
		const normalizedAgentId = nextAgentId.trim();
		const normalizedGoalId = (nextGoalId || '').trim();
		if (!normalizedAgentId) {
			loreSummaries = [];
			loreSummariesLoading = false;
			loreSummariesError = null;
			return;
		}

		const requestId = ++loreSummariesRequestId;
		const scopeKey = currentCrewScopeKey();
		loreSummariesLoading = true;
		loreSummariesError = null;
		try {
			const query = new URLSearchParams();
			if (normalizedGoalId.length > 0) {
				query.set('goal_id', normalizedGoalId);
			}
			const querySuffix = query.toString().length > 0 ? `?${query.toString()}` : '';
			const response = await timedFetch(`/api/magician/v2/agents/${encodeURIComponent(normalizedAgentId)}/memory${querySuffix}`);
			if (!response.ok) {
				let message = `Failed to load memory inventory (${response.status})`;
				try {
					const payload = await response.json();
					const parsedError = parseApiError(payload);
					const availableGoalIds = parseGoalIds(parsedError.details.available_goal_ids);
					if (allowRetry && parsedError.code === 'invalid_goal_id' && availableGoalIds.length > 0) {
						return fetchLoreSummaries(normalizedAgentId, availableGoalIds[0], false);
					}
					message = `${message}: ${parsedError.message}`;
				} catch {
					// best effort
				}
				throw new Error(message);
			}
			const payload = (await response.json()) as AgentMemorySummaryResponse;
			if (
				requestId !== loreSummariesRequestId
				|| normalizedAgentId !== (agentId || '').trim()
				|| scopeKey !== currentCrewScopeKey()
			) {
				return;
			}
			loreSummaries = Array.isArray(payload?.tiers) ? payload.tiers : [];
		} catch (err) {
			if (
				requestId !== loreSummariesRequestId
				|| normalizedAgentId !== (agentId || '').trim()
				|| scopeKey !== currentCrewScopeKey()
			) {
				return;
			}
			loreSummaries = [];
			loreSummariesError = err instanceof Error ? err.message : 'Failed to load memory inventory';
		} finally {
			if (
				requestId === loreSummariesRequestId
				&& normalizedAgentId === (agentId || '').trim()
				&& scopeKey === currentCrewScopeKey()
			) {
				loreSummariesLoading = false;
			}
		}
	}

	function computeTierHealthStatus(lastConsolidatedAt?: string, pendingEpisodes?: number, hasError?: boolean): 'healthy' | 'stale' | 'error' {
		if (hasError) return 'error';
		if (!lastConsolidatedAt) return 'stale';
		const lastConsolidatedMs = Date.parse(lastConsolidatedAt);
		if (!Number.isFinite(lastConsolidatedMs)) return 'error';
		const hoursSinceConsolidation = (Date.now() - lastConsolidatedMs) / (1000 * 60 * 60);
		if (hoursSinceConsolidation > 72) return 'stale';
		if (hoursSinceConsolidation > 24) return 'stale';
		const pending = typeof pendingEpisodes === 'number' ? pendingEpisodes : 0;
		if (pending > 20) return 'stale';
		if (pending >= 5) return 'stale';
		return 'healthy';
	}

	function formatRelativeTimeFromIso(isoTimestamp?: string): string {
		if (!isoTimestamp) return 'never';
		const parsed = Date.parse(isoTimestamp);
		if (!Number.isFinite(parsed)) return 'unknown';
		return formatRelativeTime(parsed);
	}

	async function fetchTierHealth(nextAgentId: string): Promise<void> {
		const normalizedAgentId = nextAgentId.trim();
		if (!normalizedAgentId) {
			tierHealthRows = [];
			tierHealthLoading = false;
			tierHealthError = null;
			return;
		}

		const requestId = ++tierHealthRequestId;
		const scopeKey = currentCrewScopeKey();
		tierHealthLoading = true;
		tierHealthError = null;
		try {
			const response = await timedFetch(`/api/magician/v2/agents/${encodeURIComponent(normalizedAgentId)}/memory/consolidation-health`);
			if (!response.ok) {
				// If endpoint doesn't exist yet, fall back to computing health from loreSummaries
				if (response.status === 404) {
					if (
						requestId !== tierHealthRequestId
						|| normalizedAgentId !== (agentId || '').trim()
						|| scopeKey !== currentCrewScopeKey()
					) {
						return;
					}
					tierHealthRows = loreSummaries.map((tier) => ({
						name: tier.tier_name,
						scope: tier.scope,
						last_consolidated_at: tier.last_updated || undefined,
						pending_episode_count: 0,
						status: computeTierHealthStatus(tier.last_updated || undefined, 0, false)
					}));
					return;
				}
				let message = `Failed to load consolidation health (${response.status})`;
				try {
					const payload = await response.json();
					const parsedError = parseApiError(payload);
					message = `${message}: ${parsedError.message}`;
				} catch {
					// best effort
				}
				throw new Error(message);
			}
			const payload = (await response.json()) as ConsolidationHealthResponse;
			if (
				requestId !== tierHealthRequestId
				|| normalizedAgentId !== (agentId || '').trim()
				|| scopeKey !== currentCrewScopeKey()
			) {
				return;
			}
			const rawTiers = Array.isArray(payload?.tiers) ? payload.tiers : [];
			tierHealthRows = rawTiers.map((tier) => ({
				name: tier.tier_name,
				scope: tier.scope,
				last_consolidated_at: tier.last_consolidated_at,
				pending_episode_count: typeof tier.pending_episode_count === 'number' ? tier.pending_episode_count : 0,
				status: computeTierHealthStatus(tier.last_consolidated_at, tier.pending_episode_count, !!tier.error)
			}));
		} catch (err) {
			if (
				requestId !== tierHealthRequestId
				|| normalizedAgentId !== (agentId || '').trim()
				|| scopeKey !== currentCrewScopeKey()
			) {
				return;
			}
			// Fallback: compute from loreSummaries
			tierHealthRows = loreSummaries.map((tier) => ({
				name: tier.tier_name,
				scope: tier.scope,
				last_consolidated_at: tier.last_updated || undefined,
				pending_episode_count: 0,
				status: computeTierHealthStatus(tier.last_updated || undefined, 0, false)
			}));
			tierHealthError = err instanceof Error ? err.message : 'Failed to load consolidation health';
		} finally {
			if (
				requestId === tierHealthRequestId
				&& normalizedAgentId === (agentId || '').trim()
				&& scopeKey === currentCrewScopeKey()
			) {
				tierHealthLoading = false;
			}
		}
	}

	async function fetchUserKnowledge(nextAgentId: string): Promise<void> {
		const normalizedAgentId = nextAgentId.trim();
		if (!normalizedAgentId) {
			userKnowledge = { preferences: [], contacts: [], expertise: [] };
			userKnowledgeLoading = false;
			userKnowledgeError = null;
			hasLocalUserMemory = false;
			return;
		}

		const requestId = ++userKnowledgeRequestId;
		const scopeKey = currentCrewScopeKey();
		userKnowledgeLoading = true;
		userKnowledgeError = null;
		try {
			const response = await timedFetch(`/api/magician/v2/agents/${encodeURIComponent(normalizedAgentId)}/memory?scope=user`);
			if (!response.ok) {
				if (response.status === 404) {
					if (
						requestId !== userKnowledgeRequestId
						|| normalizedAgentId !== (agentId || '').trim()
						|| scopeKey !== currentCrewScopeKey()
					) {
						return;
					}
					userKnowledge = { preferences: [], contacts: [], expertise: [] };
					hasLocalUserMemory = false;
					return;
				}
				let message = `Failed to load user knowledge (${response.status})`;
				try {
					const payload = await response.json();
					const parsedError = parseApiError(payload);
					message = `${message}: ${parsedError.message}`;
				} catch {
					// best effort
				}
				throw new Error(message);
			}
			const payload = (await response.json()) as AgentMemorySummaryResponse;
			if (
				requestId !== userKnowledgeRequestId
				|| normalizedAgentId !== (agentId || '').trim()
				|| scopeKey !== currentCrewScopeKey()
			) {
				return;
			}
			const userTiers = Array.isArray(payload?.tiers) ? payload.tiers : [];

			// Try to load detailed user knowledge content
			let preferences: Array<{ key: string; value: string }> = [];
			let contacts: Array<{ name: string; relationship: string; last_mentioned?: string }> = [];
			let expertise: string[] = [];
			let hasData = false;

			for (const tier of userTiers) {
				if (!tier.has_data) continue;
				hasData = true;
				try {
					const contentResp = await timedFetch(
						`/api/magician/v2/agents/${encodeURIComponent(normalizedAgentId)}/memory/${encodeURIComponent(tier.tier_name)}`
					);
					if (!contentResp.ok) continue;
					const contentPayload = (await contentResp.json()) as Record<string, unknown>;
					if (
						requestId !== userKnowledgeRequestId
						|| normalizedAgentId !== (agentId || '').trim()
						|| scopeKey !== currentCrewScopeKey()
					) {
						return;
					}

					const contentRecord = asRecord(contentPayload);
					const entries = contentRecord ? readArray(contentRecord, 'entries') : [];

					if (tier.tier_name.includes('preferences') || tier.tier_name === 'user.preferences') {
						for (const entry of entries) {
							const rec = asRecord(entry);
							if (!rec) continue;
							const key = readString(rec, 'key') || readString(rec, 'entity') || readString(rec, 'name') || '';
							const value = readString(rec, 'value') || readString(rec, 'content') || readString(rec, 'summary') || '';
							if (key || value) preferences.push({ key, value });
						}
					} else if (tier.tier_name.includes('contacts') || tier.tier_name === 'user.contacts') {
						for (const entry of entries) {
							const rec = asRecord(entry);
							if (!rec) continue;
							const name = readString(rec, 'name') || readString(rec, 'entity') || '';
							const relationship = readString(rec, 'relationship') || readString(rec, 'type') || '';
							const lastMentioned = readString(rec, 'last_mentioned') || readString(rec, 'updated_at') || undefined;
							if (name) contacts.push({ name, relationship, last_mentioned: lastMentioned });
						}
					} else if (tier.tier_name.includes('expertise') || tier.tier_name === 'user.expertise') {
						for (const entry of entries) {
							const rec = asRecord(entry);
							if (!rec) continue;
							const skill = readString(rec, 'name') || readString(rec, 'entity') || readString(rec, 'skill') || '';
							if (skill && !expertise.includes(skill)) expertise.push(skill);
						}
					} else {
						// Generic user tier: try to extract any recognizable data
						for (const entry of entries) {
							const rec = asRecord(entry);
							if (!rec) continue;
							const key = readString(rec, 'key') || readString(rec, 'entity') || readString(rec, 'name') || '';
							const value = readString(rec, 'value') || readString(rec, 'content') || readString(rec, 'summary') || '';
							if (key || value) preferences.push({ key, value });
						}
					}
				} catch {
					// best effort per-tier
				}
			}

			hasLocalUserMemory = hasData;
			userKnowledge = { preferences, contacts, expertise };
		} catch (err) {
			if (
				requestId !== userKnowledgeRequestId
				|| normalizedAgentId !== (agentId || '').trim()
				|| scopeKey !== currentCrewScopeKey()
			) {
				return;
			}
			userKnowledge = { preferences: [], contacts: [], expertise: [] };
			hasLocalUserMemory = false;
			userKnowledgeError = err instanceof Error ? err.message : 'Failed to load user knowledge';
		} finally {
			if (
				requestId === userKnowledgeRequestId
				&& normalizedAgentId === (agentId || '').trim()
				&& scopeKey === currentCrewScopeKey()
			) {
				userKnowledgeLoading = false;
			}
		}
	}

	async function mergeUserMemoryToShared(): Promise<void> {
		if (isMergingUserMemory || !agentId) return;
		const scopeKey = currentCrewScopeKey();
		const actionAgentId = agentId;
		isMergingUserMemory = true;
		mergeUserMemoryMessage = null;
		mergeUserMemoryError = null;
		try {
			const response = await timedFetch(`/api/magician/v2/agents/${encodeURIComponent(agentId)}/consolidate-user-memory`, {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' }
			});
			if (!response.ok) {
				let message = `Merge failed (${response.status})`;
				try {
					const payload = await response.json();
					const parsedError = parseApiError(payload);
					message = `${message}: ${parsedError.message}`;
				} catch {
					// best effort
				}
				throw new Error(message);
			}
			if (scopeKey !== currentCrewScopeKey() || actionAgentId !== agentId) {
				return;
			}
			mergeUserMemoryMessage = 'User memory successfully merged to shared knowledge.';
			showSuccess('Memory merged', 'User memory successfully merged to shared knowledge.');
			// Refresh user knowledge to reflect the merge
			void fetchUserKnowledge(actionAgentId);
			void fetchLoreSummaries(actionAgentId, summary?.current_goal_id);
		} catch (err) {
			if (scopeKey !== currentCrewScopeKey() || actionAgentId !== agentId) {
				return;
			}
			mergeUserMemoryError = err instanceof Error ? err.message : 'Failed to merge user memory';
			showError('Merge failed', mergeUserMemoryError);
		} finally {
			if (scopeKey === currentCrewScopeKey() && actionAgentId === agentId) {
				isMergingUserMemory = false;
			}
		}
	}

	/**
	 * Group an array of tasks by a key function, returning a Map.
	 */
	function groupTasksBy(tasks: Task[], keyFn: (t: Task) => string): Map<string, Task[]> {
		const map = new Map<string, Task[]>();
		for (const task of tasks) {
			const key = keyFn(task);
			const existing = map.get(key);
			if (existing) {
				existing.push(task);
			} else {
				map.set(key, [task]);
			}
		}
		return map;
	}

	/**
	 * Fetch autonomous proposals for this agent (unapproved delegation/autonomous tasks).
	 */
	async function fetchAutonomousProposals(nextAgentId: string): Promise<void> {
		const normalizedAgentId = nextAgentId.trim();
		if (!normalizedAgentId) {
			autonomousProposals = [];
			proposalCycles = new Map();
			proposalsLoading = false;
			proposalsError = null;
			return;
		}

		const requestId = ++proposalsRequestId;
		const scopeKey = currentCrewScopeKey();
		proposalsLoading = true;
		proposalsError = null;
		try {
			const response = await timedFetch('/api/magician/v3/tasks');
			if (!response.ok) {
				if (response.status === 404) {
					// Endpoint might not exist yet; gracefully show no proposals
					if (
						requestId !== proposalsRequestId
						|| normalizedAgentId !== (agentId || '').trim()
						|| scopeKey !== currentCrewScopeKey()
					) {
						return;
					}
					autonomousProposals = [];
					proposalCycles = new Map();
					return;
				}
				throw new Error(`Failed to load proposals (${response.status})`);
			}
			const result = await response.json();
			if (
				requestId !== proposalsRequestId
				|| normalizedAgentId !== (agentId || '').trim()
				|| scopeKey !== currentCrewScopeKey()
			) {
				return;
			}
			const rawTasks = Array.isArray(result.tasks) ? result.tasks : [];
			const allTasks: Task[] = rawTasks.map((t: Record<string, unknown>) => ({
				id: t.id as string,
				title: t.title as string,
				description: t.description as string | undefined,
				status: t.status as TaskStatus,
				priority: t.priority as Task['priority'],
				dueDate: t.due_date as string | undefined,
				tags: (t.tags || []) as Task['tags'],
				agentId: t.agent_id as string | undefined,
				agentName: t.agent_name as string | undefined,
				createdBy: normalizeTaskCreatedBy(t.created_by),
				dependsOn: (t.depends_on as string[]) || [],
				approved: (t.approved as boolean) ?? true,
				isBlocked: (t.is_blocked as boolean) ?? false,
				source: 'task' as const,
				createdAt: readIsoTimestamp(t.created_at) || new Date().toISOString(),
				updatedAt: readIsoTimestamp(t.updated_at) || new Date().toISOString()
			}));

			// Filter for delegation/autonomous proposals that are not yet approved
			autonomousProposals = allTasks.filter(
				(t) =>
					t.agentId === normalizedAgentId &&
					t.approved === false &&
					(t.createdBy === 'delegation' || t.createdBy === 'autonomous')
			);

			// Group by parent_task_id (extracted from the createdBy metadata or dependsOn chain)
			proposalCycles = groupTasksBy(
				autonomousProposals,
				(t) => {
					// Use the first dependency as a proxy for cycle grouping, or 'ungrouped'
					if (t.dependsOn && t.dependsOn.length > 0) return t.dependsOn[0];
					return 'ungrouped';
				}
			);
		} catch (err) {
			if (
				requestId !== proposalsRequestId
				|| normalizedAgentId !== (agentId || '').trim()
				|| scopeKey !== currentCrewScopeKey()
			) {
				return;
			}
			autonomousProposals = [];
			proposalCycles = new Map();
			proposalsError = err instanceof Error ? err.message : 'Failed to load proposals';
		} finally {
			if (
				requestId === proposalsRequestId
				&& normalizedAgentId === (agentId || '').trim()
				&& scopeKey === currentCrewScopeKey()
			) {
				proposalsLoading = false;
			}
		}
	}

	/**
	 * Approve a single proposal task.
	 */
	async function handleApproveProposal(taskId: string): Promise<void> {
		try {
			await approveTask(taskId);
			// Refresh proposals after approval
			void fetchAutonomousProposals(agentId);
			if (harnessEnabled) {
				void fetchHarnessOverview(agentId, true);
			}
		} catch (err) {
			const message = err instanceof Error ? err.message : 'Failed to approve proposal';
			console.error('Failed to approve proposal:', err);
			showError('Proposal approval failed', message);
		}
	}

	/**
	 * Batch-approve all proposals in a cycle group.
	 */
	async function handleApproveAllCycle(cycleKey: string): Promise<void> {
		const cycleTasks = proposalCycles.get(cycleKey);
		if (!cycleTasks || cycleTasks.length === 0) return;
		isApprovingAll = true;
		try {
			const taskIds = cycleTasks.map((t) => t.id);
			await batchApproveTasks(taskIds);
			// Refresh proposals after batch approval
			void fetchAutonomousProposals(agentId);
			if (harnessEnabled) {
				void fetchHarnessOverview(agentId, true);
			}
		} catch (err) {
			const message = err instanceof Error ? err.message : 'Failed to approve proposal cycle';
			console.error('Failed to batch approve cycle:', err);
			showError('Cycle approval failed', message);
		} finally {
			isApprovingAll = false;
		}
	}

	/**
	 * Reject a proposal task by deleting it.
	 */
	async function handleRejectProposal(taskId: string): Promise<void> {
		try {
			await taskStore.deleteTask(taskId);
			// Refresh proposals after rejection
			void fetchAutonomousProposals(agentId);
			if (harnessEnabled) {
				void fetchHarnessOverview(agentId, true);
			}
		} catch (err) {
			const message = err instanceof Error ? err.message : 'Failed to reject proposal';
			console.error('Failed to reject proposal:', err);
			showError('Proposal rejection failed', message);
		}
	}

	async function handleStructuralProposalDecision(
		proposalId: string,
		decision: 'approve' | 'reject' | 'defer'
	): Promise<void> {
		const normalizedProposalId = proposalId.trim();
		if (!normalizedProposalId || structuralProposalActionId) return;
		const scopeKey = currentCrewScopeKey();
		const actionAgentId = agentId;
		structuralProposalActionId = normalizedProposalId;
		try {
			const response = await timedFetch(
				`/api/magician/v2/proposals/${encodeURIComponent(normalizedProposalId)}/resolve`,
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({
						decision,
						channel: 'web-ui'
					})
				}
			);
			if (!response.ok) {
				let message = `Failed to ${decision} proposal (${response.status})`;
				try {
					const payload = await response.json();
					const parsedError = parseApiError(payload);
					message = `${message}: ${parsedError.message}`;
				} catch {
					// best effort
				}
				throw new Error(message);
			}
			const payload = asRecord(await response.json());
			const applicationError = readString(payload || {}, 'application_error');
			if (scopeKey !== currentCrewScopeKey() || actionAgentId !== agentId) {
				return;
			}
			const pastTense =
				decision === 'approve' ? 'approved' : decision === 'reject' ? 'rejected' : 'deferred';
			showSuccess(
				`Proposal ${pastTense}`,
				decision === 'approve'
					? 'The structural proposal was resolved and the definition apply path was triggered.'
					: 'The structural proposal state was updated.'
			);
			if (applicationError) {
				showError('Proposal application failed', applicationError);
			}
			await refreshHarnessOperatorView(actionAgentId);
		} catch (err) {
			if (scopeKey !== currentCrewScopeKey() || actionAgentId !== agentId) {
				return;
			}
			const message =
				err instanceof Error ? err.message : `Failed to ${decision} structural proposal`;
			showError(`Proposal ${decision} failed`, message);
		} finally {
			if (scopeKey === currentCrewScopeKey() && actionAgentId === agentId) {
				structuralProposalActionId = null;
			}
		}
	}

	async function handleOwnerRequestResponse(requestId: string, decision: string): Promise<void> {
		const normalizedRequestId = requestId.trim();
		const normalizedDecision = decision.trim();
		if (!normalizedRequestId || !normalizedDecision || ownerRequestActionId) return;
		const scopeKey = currentCrewScopeKey();
		const actionAgentId = agentId;
		ownerRequestActionId = normalizedRequestId;
		try {
			// Phase H7.x — canonical respond endpoint.
			const response = await timedFetch(
				`/api/magician/v2/hitl/${encodeURIComponent(normalizedRequestId)}/respond`,
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({
						source: 'user_request',
						value: { type: 'choice', selected_id: normalizedDecision },
						channel: 'web-ui'
					})
				}
			);
			if (!(response.ok || response.status === 404 || response.status === 409)) {
				let message = `Failed to respond to briefing (${response.status})`;
				try {
					const payload = await response.json();
					const parsedError = parseApiError(payload);
					message = `${message}: ${parsedError.message}`;
				} catch {
					// best effort
				}
				throw new Error(message);
			}
			if (scopeKey !== currentCrewScopeKey() || actionAgentId !== agentId) {
				return;
			}
			showSuccess('Owner request acknowledged', 'The pending harness notification was cleared.');
			await refreshHarnessOperatorView(actionAgentId);
		} catch (err) {
			if (scopeKey !== currentCrewScopeKey() || actionAgentId !== agentId) {
				return;
			}
			const message =
				err instanceof Error ? err.message : 'Failed to respond to owner request';
			showError('Owner request response failed', message);
		} finally {
			if (scopeKey === currentCrewScopeKey() && actionAgentId === agentId) {
				ownerRequestActionId = null;
			}
		}
	}

	function formatStatus(summaryEntry: AgentSummary | undefined): string {
		if (!summaryEntry) return 'unknown';
		return summaryEntry.status.replace(/_/g, ' ');
	}

	function formatRelativeTime(timestamp: number | undefined): string {
		if (!timestamp) return 'never';
		const diffMs = timestamp - Date.now();
		const diffMinutes = Math.round(diffMs / 60000);
		if (Math.abs(diffMinutes) < 1) return 'just now';
		if (Math.abs(diffMinutes) < 60) return `${Math.abs(diffMinutes)}m ${diffMinutes < 0 ? 'ago' : 'from now'}`;
		const diffHours = Math.round(diffMinutes / 60);
		if (Math.abs(diffHours) < 48) return `${Math.abs(diffHours)}h ${diffHours < 0 ? 'ago' : 'from now'}`;
		const diffDays = Math.round(diffHours / 24);
		return `${Math.abs(diffDays)}d ${diffDays < 0 ? 'ago' : 'from now'}`;
	}

	function stringOrFallback(value: unknown, fallback = '—'): string {
		return typeof value === 'string' && value.trim().length > 0 ? value : fallback;
	}

	function toJson(value: unknown): string {
		return JSON.stringify(value, null, 2);
	}

	function statusColor(status: string): 'default' | 'info' | 'warning' | 'error' | 'success' {
		if (status.includes('running') || status.includes('triggered')) return 'info';
		if (status.includes('paused')) return 'warning';
		if (status.includes('error') || status.includes('failed')) return 'error';
		if (status.includes('completed')) return 'success';
		return 'default';
	}

	function kindBadgeColor(kind: string): 'default' | 'info' | 'warning' | 'error' | 'success' {
		if (kind === 'Personal') return 'info';
		if (kind === 'Worker') return 'warning';
		return 'default';
	}

	function proposalStatusBadgeColor(status: string): 'default' | 'info' | 'warning' | 'error' | 'success' {
		const normalized = status.trim().toLowerCase();
		if (normalized === 'approved') return 'success';
		if (normalized === 'pending' || normalized === 'deferred') return 'warning';
		if (normalized === 'rejected') return 'error';
		return 'default';
	}

	function harnessSeverityBadgeColor(severity: string): 'default' | 'info' | 'warning' | 'error' | 'success' {
		const normalized = severity.trim().toLowerCase();
		if (normalized === 'critical') return 'error';
		if (normalized === 'warning') return 'warning';
		if (normalized === 'info') return 'info';
		return 'default';
	}

	function harnessRequestKindBadgeColor(kind: string): 'default' | 'info' | 'warning' | 'error' | 'success' {
		const normalized = kind.trim().toLowerCase();
		if (normalized === 'escalation') return 'error';
		if (normalized === 'question') return 'warning';
		if (normalized === 'briefing') return 'info';
		return 'default';
	}

	function ownerRequestStatusBadgeColor(status: string): 'default' | 'info' | 'warning' | 'error' | 'success' {
		const normalized = status.trim().toLowerCase();
		if (normalized === 'resolved') return 'success';
		if (normalized === 'pending') return 'warning';
		return 'default';
	}

	function isHarnessNotifyOwnerRequestType(requestType?: string): boolean {
		return (requestType || '').trim().startsWith('harness.notify_owner.');
	}

	function ownerRequestKindFromType(requestType?: string): string {
		if (!isHarnessNotifyOwnerRequestType(requestType)) return 'briefing';
		return requestType!.trim().split('.').pop() || 'briefing';
	}

	function ownerRequestStatusFromRecord(request: Record<string, unknown> | null): string {
		return readString(request || {}, 'status') || 'pending';
	}

	function ownerRequestActivityTimestamp(request: Record<string, unknown> | null): number {
		if (!request) return 0;
		if (typeof request.resolved_at === 'number' && Number.isFinite(request.resolved_at)) {
			return request.resolved_at;
		}
		if (typeof request.created_at === 'number' && Number.isFinite(request.created_at)) {
			return request.created_at;
		}
		return 0;
	}

	function humanizeIdentifier(value: string, fallback = 'unknown'): string {
		const normalized = value.trim();
		if (!normalized) return fallback;
		return normalized.replace(/[_-]+/g, ' ');
	}

	function matchesHarnessOwnerRequest(
		request: Record<string, unknown> | null,
		ownerAgentId: string
	): boolean {
		if (!request || !ownerAgentId.trim()) return false;
		const requestType = readString(request, 'request_type');
		if (!isHarnessNotifyOwnerRequestType(requestType)) return false;
		const context = asRecord(request['context']);
		return readString(context || {}, 'owner_agent_id') === ownerAgentId.trim();
	}

	function matchesHarnessOwnerRequestSignal(
		event: { event_type: string; data: unknown },
		ownerAgentId: string
	): boolean {
		if (event.event_type === 'UserRequestPending') {
			const payload = asRecord(event.data);
			const request = asRecord(payload ? payload['request'] : null);
			return matchesHarnessOwnerRequest(request, ownerAgentId);
		}
		if (event.event_type === 'UserRequestResolved') {
			const payload = asRecord(event.data);
			return (
				isHarnessNotifyOwnerRequestType(readString(payload || {}, 'request_type'))
				&& readString(payload || {}, 'owner_agent_id') === ownerAgentId.trim()
			);
		}
		return false;
	}

	function extractAgentEventEnvelope(
		event: { event_type: string; data: unknown }
	): { event_type: string; agent_id: string; payload: Record<string, unknown> | null } | null {
		if (event.event_type !== 'AgentEvent') return null;
		const container = asRecord(event.data);
		const envelope = asRecord(container ? container['event'] : null);
		const eventType = readString(envelope || {}, 'event_type');
		const agentId = readString(envelope || {}, 'agent_id');
		if (!eventType || !agentId) return null;
		return {
			event_type: eventType,
			agent_id: agentId,
			payload: asRecord(envelope ? envelope['payload'] : null)
		};
	}

	function matchesHarnessStructuralProposalSignal(
		event: { event_type: string; data: unknown },
		ownerAgentId: string
	): boolean {
		const normalizedOwnerAgentId = ownerAgentId.trim();
		if (!normalizedOwnerAgentId) return false;
		const envelope = extractAgentEventEnvelope(event);
		if (!envelope) return false;
		if (
			envelope.event_type !== 'proposal.created'
			&& envelope.event_type !== 'proposal.resolved'
			&& envelope.event_type !== 'proposal.applied'
		) {
			return false;
		}
		const proposalSource =
			readString(envelope.payload || {}, 'proposal_source')
			|| readString(envelope.payload || {}, 'source');
		return proposalSource === `harness:${normalizedOwnerAgentId}`;
	}

	function matchesHarnessStructuralProposal(
		proposal: Record<string, unknown> | null,
		ownerAgentId: string
	): boolean {
		if (!proposal || !ownerAgentId.trim()) return false;
		return readString(proposal, 'source') === `harness:${ownerAgentId.trim()}`;
	}

	function isHarnessStructuralProposalRetryable(
		proposal: Record<string, unknown> | null
	): boolean {
		if (!proposal) return false;
		return (
			(readString(proposal, 'status') || '').trim().toLowerCase() === 'approved'
			&& readFiniteNumber(proposal, 'applied_version', 0) < 1
		);
	}

	function isHarnessStructuralProposalActionable(
		proposal: Record<string, unknown> | null
	): boolean {
		if (!proposal) return false;
		const status = (readString(proposal, 'status') || '').trim().toLowerCase();
		return status === 'pending' || isHarnessStructuralProposalRetryable(proposal);
	}

	// --- Task 2.22: Set primary agent handler ---
	async function handleSetPrimary(): Promise<void> {
		if (isSettingPrimary || !agentId || isSystemAgent) return;
		const scopeKey = currentCrewScopeKey();
		const actionAgentId = agentId;
		isSettingPrimary = true;
		try {
			await setPrimary(actionAgentId);
			if (scopeKey !== currentCrewScopeKey() || actionAgentId !== agentId) {
				return;
			}
			showSuccess(`${displayAgentName} is now the primary agent`);
			// Reload the agent to refresh summary (including is_primary)
			await loadAgent(actionAgentId);
		} catch (err) {
			if (scopeKey !== currentCrewScopeKey() || actionAgentId !== agentId) {
				return;
			}
			const message = err instanceof Error ? err.message : 'Failed to set primary agent';
			showError(message);
		} finally {
			if (scopeKey === currentCrewScopeKey() && actionAgentId === agentId) {
				isSettingPrimary = false;
			}
		}
	}

	async function hydrateAgent(nextAgentId: string): Promise<void> {
		const scopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
		const requestToken = ++activeRequestToken;
		activeRequestId = nextAgentId;
		lastHydratedScopeKey = scopeKey;
		isLoading = true;
		error = null;
		record = null;
		loreSummaries = [];
		loreSummariesLoading = false;
		loreSummariesError = null;
		tierHealthRows = [];
		tierHealthLoading = false;
		tierHealthError = null;
		userKnowledge = { preferences: [], contacts: [], expertise: [] };
		userKnowledgeLoading = false;
		userKnowledgeError = null;
		hasLocalUserMemory = false;
		mergeUserMemoryMessage = null;
		mergeUserMemoryError = null;
		bootstrapTask = null;
		bootstrapTaskLoading = false;
		bootstrapTaskError = null;
		harnessOverview = null;
		harnessOverviewLoading = false;
		harnessOverviewError = null;
		harnessStructuralProposals = [];
		harnessStructuralProposalsLoading = false;
		harnessStructuralProposalsError = null;
		harnessOwnerRequests = [];
		harnessOwnerRequestHistory = [];
		harnessOwnerRequestsLoading = false;
		harnessOwnerRequestsError = null;
		selectedStructuralProposalId = null;
		structuralProposalActionId = null;
		ownerRequestActionId = null;
		autonomousProposals = [];
		proposalCycles = new Map();
		proposalsLoading = false;
		proposalsError = null;
		lastProcessedCycleEventCount = 0;
		lastProcessedHarnessSignalEventCount = 0;
		lastProcessedDefinitionEventCount = 0;
		crewHealth = null;
		crewHealthLoading = false;
		summary = getAgentSnapshot(nextAgentId);
		systemAgent = get(systemAgentList).find((entry) => entry.agent_id === nextAgentId);
		isSystemAgent = isSystemAgentId(nextAgentId);
		if (!isSystemAgent) {
			void hydrateCrewHealth(nextAgentId, requestToken, scopeKey);
		}

		try {
			if (isSystemAgent) {
				loreSummaries = [];
				isLoading = false;
				return;
			}
			const [loadedSummary, loadedRecord] = await Promise.all([loadAgent(nextAgentId), fetchAgentDefinitionRecord(nextAgentId)]);
			if (
				requestToken !== activeRequestToken
				|| activeRequestId !== nextAgentId
				|| lastHydratedScopeKey !== scopeKey
			) {
				return;
			}
			summary = loadedSummary || summary;
			record = loadedRecord;
			if (!loadedRecord) {
				error = `Crew member "${nextAgentId}" was not found`;
			} else if (!isSystemAgent) {
				const hasHarness = asRecord(loadedRecord.definition['harness']) !== null;
				void fetchLoreSummaries(nextAgentId, loadedSummary?.current_goal_id);
				void fetchTierHealth(nextAgentId);
				void fetchUserKnowledge(nextAgentId);
				void fetchBootstrapTask(nextAgentId);
				void refreshHarnessOperatorView(nextAgentId, hasHarness);
				void fetchAutonomousProposals(nextAgentId);
			}
		} catch (err) {
			if (
				requestToken !== activeRequestToken
				|| activeRequestId !== nextAgentId
				|| lastHydratedScopeKey !== scopeKey
			) {
				return;
			}
			error = err instanceof Error ? err.message : 'Failed to load crew member detail';
		} finally {
			if (
				requestToken === activeRequestToken
				&& activeRequestId === nextAgentId
				&& lastHydratedScopeKey === scopeKey
			) {
				isLoading = false;
			}
		}
	}

	async function hydrateCrewHealth(
		nextAgentId: string,
		requestToken: number,
		scopeKey: string
	): Promise<void> {
		crewHealthLoading = true;
		try {
			const response = await fetchAgentHealth(nextAgentId);
			if (
				requestToken !== activeRequestToken
				|| activeRequestId !== nextAgentId
				|| lastHydratedScopeKey !== scopeKey
			) {
				return;
			}
			crewHealth = response;
		} finally {
			if (
				requestToken === activeRequestToken
				&& activeRequestId === nextAgentId
				&& lastHydratedScopeKey === scopeKey
			) {
				crewHealthLoading = false;
			}
		}
	}

	function buildSectionTable(title: string, subtitle: string, rows: Array<Record<string, string>>, id: string): CrewNativeComponent {
		return {
			id,
			component_type: 'Card',
			props: {
				title,
				subtitle,
				body: rows.length > 0 ? `${rows.length} row(s)` : 'No entries configured.'
			},
			children:
				rows.length > 0
					? [
							{
								id: `${id}-table`,
								component_type: 'Table',
								props: {
									columns: Object.keys(rows[0]).map((key) => ({ key, label: key.replace(/_/g, ' ') })),
									rows
								}
							}
						]
					: [
							{
								id: `${id}-empty`,
								component_type: 'EmptyState',
								props: {
									title: 'No entries',
									description: 'No configured data for this section.'
								}
							}
						]
		};
	}

	function buildDoubleDetailSurface(input: {
		agentId: string;
		routeParamError: string | null;
		isLoading: boolean;
		error: string | null;
		isSystemAgent: boolean;
		displayAgentName: string;
		displayAgentId: string;
		displayStatus: string;
		summary: AgentSummary | undefined;
		record: AgentDefinitionRecord | null;
		selectedTab: DetailTabId;
		agentKind: string;
		capabilityPacks: string[];
		excludedPacks: string[];
		delegateTo: string[];
		maxDelegationDepth: number;
		sortedMemoryTiers: unknown[];
		loreSummaries: AgentMemoryTierSummary[];
		loreSummariesLoading: boolean;
		loreSummariesError: string | null;
		memoryConsolidation: unknown[];
		retentionEpisodes: unknown;
		feedbackLoops: unknown[];
		retentionCorrections: unknown;
		configYaml: string;
		canOpenExecutionPanel: boolean;
		tierHealthRows: TierHealthRow[];
		tierHealthLoading: boolean;
		tierHealthError: string | null;
		userKnowledge: UserKnowledgeData;
		userKnowledgeLoading: boolean;
		userKnowledgeError: string | null;
		hasLocalUserMemory: boolean;
		isMergingUserMemory: boolean;
		mergeUserMemoryMessage: string | null;
		mergeUserMemoryError: string | null;
		isPrimary: boolean;
		userMemoryIsolation?: string;
		readableAgents: string[];
		autonomousProposals: Task[];
		proposalCycles: Map<string, Task[]>;
		proposalsLoading: boolean;
		proposalsError: string | null;
		isApprovingAll: boolean;
		harnessEnabled: boolean;
		harnessOverview: Record<string, unknown> | null;
		harnessOverviewLoading: boolean;
		harnessOverviewError: string | null;
		harnessStructuralProposals: Record<string, unknown>[];
		harnessStructuralProposalsLoading: boolean;
		harnessStructuralProposalsError: string | null;
		harnessOwnerRequests: Record<string, unknown>[];
		harnessOwnerRequestHistory: Record<string, unknown>[];
		harnessOwnerRequestsLoading: boolean;
		harnessOwnerRequestsError: string | null;
		selectedStructuralProposalId: string | null;
		structuralProposalActionId: string | null;
		ownerRequestActionId: string | null;
		harnessProgramSection: string | null;
		autonomousConfig: typeof autonomousConfig;
		bootstrapTask: BootstrapTaskData | null;
		bootstrapTaskLoading: boolean;
		bootstrapTaskError: string | null;
		schemaVersion: string;
	}): CrewNativeComponent[] {
		const components: CrewNativeComponent[] = [
			{
				id: 'presto-double-detail-header',
				component_type: 'Card',
				props: {
					title: stringOrFallback(input.displayAgentName, 'Crew member detail'),
					subtitle: `${stringOrFallback(input.displayAgentId, input.agentId)} · ${input.isSystemAgent ? 'System internal' : (input.agentKind || 'Specialist')} · ${input.schemaVersion}`,
					body: readString(input.record?.definition || {}, 'description') || 'Operational view and runtime state for this crew member.'
				},
				children: [
					{
						id: 'presto-double-detail-actions',
						component_type: 'Stack',
						props: { direction: 'row', gap: '0.5rem', wrap: true },
						children: [
							{ id: 'presto-double-detail-action-back', component_type: 'Button', label: 'Back to Crew', props: { interactive: true, variant: 'outline', size: 'sm' } },
							{ id: 'presto-double-detail-action-edit', component_type: 'Button', label: 'Edit definition', props: { interactive: true, variant: 'secondary', size: 'sm', disabled: input.isSystemAgent || !input.agentId } },
							{ id: 'presto-double-detail-action-execution', component_type: 'Button', label: 'Open execution', props: { interactive: true, variant: 'primary', size: 'sm', disabled: !input.canOpenExecutionPanel } }
						]
					}
				]
			}
		];

		if (input.routeParamError) {
			components.push({
				id: 'presto-double-detail-route-error',
				component_type: 'Alert',
				props: { type: 'error', message: input.routeParamError, closable: false }
			});
			return components;
		}

		if (input.isLoading) {
			components.push({
				id: 'presto-double-detail-loading',
				component_type: 'EmptyState',
				props: {
					title: `Loading ${input.agentId}`,
					description: 'Fetching definition and runtime summary...'
				}
			});
			return components;
		}

		if (input.error) {
			components.push({
				id: 'presto-double-detail-error',
				component_type: 'Alert',
				props: {
					type: 'error',
					message: input.error,
					closable: false
				}
			});
			return components;
		}

		if (!input.record && !input.isSystemAgent) {
			components.push({
				id: 'presto-double-detail-no-definition',
				component_type: 'EmptyState',
				props: {
					title: 'No definition data',
					description: 'This crew member has no persisted definition yet.'
				}
			});
			return components;
		}

		// Summary card with kind badge, capability packs, coordination info
		const summaryMetaItems = [
			{ id: 'presto-double-detail-meta-cycle', key: 'Current run', value: input.summary?.current_cycle_id || 'none' },
			{ id: 'presto-double-detail-meta-approvals', key: 'Pending approvals', value: String(input.summary?.pending_approvals || 0) },
			{ id: 'presto-double-detail-meta-version', key: 'Config version', value: String(input.record?.version || 0) },
			{ id: 'presto-double-detail-meta-updated', key: 'Last update', value: formatRelativeTime(input.summary?.updated_at) },
			{ id: 'presto-double-detail-meta-delegation-depth', key: 'Max delegation depth', value: String(input.maxDelegationDepth) }
		];

		const summaryBadgeChildren: CrewNativeComponent[] = [
			{
				id: 'presto-double-detail-summary-status',
				component_type: 'Badge',
				props: {
					text: input.displayStatus,
					color: statusColor(input.displayStatus)
				}
			},
			{
				id: 'presto-double-detail-summary-kind',
				component_type: 'Badge',
				props: {
					text: input.agentKind,
					color: kindBadgeColor(input.agentKind)
				}
			}
		];

		if (input.isPrimary) {
			summaryBadgeChildren.push({
				id: 'presto-double-detail-summary-primary',
				component_type: 'Badge',
				props: {
					text: 'Primary Agent',
					color: 'success'
				}
			});
		}

		// Task 2.22: "Make Primary" button for Personal agents that are not primary
		if (!input.isPrimary && input.agentKind === 'Personal' && !input.isSystemAgent) {
			summaryBadgeChildren.push({
				id: 'presto-double-detail-action-make-primary',
				component_type: 'Button',
				label: 'Make Primary',
				props: {
					interactive: true,
					variant: 'secondary',
					size: 'sm'
				}
			});
		}

		const summaryChildren: CrewNativeComponent[] = [
			{
				id: 'presto-double-detail-summary-badges',
				component_type: 'Stack',
				props: { direction: 'row', gap: '0.5rem', wrap: true },
				children: summaryBadgeChildren
			}
		];

		// Capability packs as tag chips
		if (input.capabilityPacks.length > 0) {
			summaryChildren.push({
				id: 'presto-double-detail-capability-packs',
				component_type: 'Stack',
				props: { direction: 'row', gap: '0.35rem', wrap: true },
				children: input.capabilityPacks.map((pack, index) => ({
					id: `presto-double-detail-cap-${index}`,
					component_type: 'Tag',
					props: { text: pack, color: 'info' }
				}))
			});
		}

		// Excluded packs
		if (input.excludedPacks.length > 0) {
			summaryChildren.push({
				id: 'presto-double-detail-excluded-packs',
				component_type: 'Stack',
				props: { direction: 'row', gap: '0.35rem', wrap: true },
				children: [
					{
						id: 'presto-double-detail-excluded-label',
						component_type: 'Text',
						props: { children: 'Excluded:', variant: 'body' }
					},
					...input.excludedPacks.map((pack, index) => ({
						id: `presto-double-detail-excl-${index}`,
						component_type: 'Tag',
						props: { text: pack, color: 'error' }
					}))
				]
			});
		}

		// Delegate-to list
		if (input.delegateTo.length > 0) {
			summaryChildren.push({
				id: 'presto-double-detail-delegate-to',
				component_type: 'Stack',
				props: { direction: 'row', gap: '0.35rem', wrap: true },
				children: [
					{
						id: 'presto-double-detail-delegate-label',
						component_type: 'Text',
						props: { children: 'Delegates to:', variant: 'body' }
					},
					...input.delegateTo.map((targetId, index) => ({
						id: `presto-double-detail-delegate-${index}`,
						component_type: 'Tag',
						props: { text: targetId, color: 'success' }
					}))
				]
			});
		}

		summaryChildren.push({
			id: 'presto-double-detail-summary-meta',
			component_type: 'DataList',
			props: { items: summaryMetaItems }
		});

		components.push({
			id: 'presto-double-detail-summary',
			component_type: 'Card',
			props: {
				title: stringOrFallback(input.displayAgentName, input.agentId),
				subtitle: stringOrFallback(input.displayAgentId, input.agentId),
				body: input.isSystemAgent ? 'Read-only system agent' : 'Editable custom agent'
			},
			children: summaryChildren
		});

		// --- Task 5.10: Autonomous Schedule section ---
		if (input.autonomousConfig && !input.isSystemAgent) {
			const scheduleStatus = bootstrapTaskStatus(input.bootstrapTask, input.autonomousConfig);
			const humanSchedule = cronToHumanReadable(input.autonomousConfig.schedule);
			const nextFire = cronNextFire(input.autonomousConfig.schedule);

			const scheduleItems: Array<{ id: string; key: string; value: string }> = [
				{
					id: 'presto-double-detail-auto-schedule-cron',
					key: 'Schedule',
					value: humanSchedule ? `${input.autonomousConfig.schedule} (${humanSchedule})` : input.autonomousConfig.schedule
				},
				{
					id: 'presto-double-detail-auto-schedule-next',
					key: 'Next fire',
					value: nextFire
				},
				{
					id: 'presto-double-detail-auto-max-tasks',
					key: 'Max tasks per cycle',
					value: String(input.autonomousConfig.max_tasks_per_cycle)
				},
				{
					id: 'presto-double-detail-auto-max-steps',
					key: 'Max steps per plan',
					value: String(input.autonomousConfig.max_steps_per_plan)
				}
			];
			if (input.harnessProgramSection) {
				scheduleItems.push({
					id: 'presto-double-detail-auto-harness-program-section',
					key: 'Harness program section',
					value: input.harnessProgramSection
				});
			}

			if (input.bootstrapTask) {
				scheduleItems.push({
					id: 'presto-double-detail-auto-last-cycle',
					key: 'Last cycle',
					value: input.bootstrapTask.last_completed_at
						? `${formatRelativeTimeFromIso(input.bootstrapTask.last_completed_at)} — ${input.bootstrapTask.outcome || input.bootstrapTask.status}`
						: input.bootstrapTask.status
				});
			}

			const autoScheduleChildren: CrewNativeComponent[] = [
				{
					id: 'presto-double-detail-auto-status-badges',
					component_type: 'Stack',
					props: { direction: 'row', gap: '0.5rem', wrap: true },
					children: [
						{
							id: 'presto-double-detail-auto-status-badge',
							component_type: 'Badge',
							props: {
								text: scheduleStatus.charAt(0).toUpperCase() + scheduleStatus.slice(1),
								color: bootstrapStatusColor(scheduleStatus)
							}
						},
						{
							id: 'presto-double-detail-auto-mode-badge',
							component_type: 'Badge',
							props: {
								text: 'Autonomous',
								color: 'info'
							}
						}
					]
				},
				{
					id: 'presto-double-detail-auto-data',
					component_type: 'DataList',
					props: { items: scheduleItems }
				}
			];

			// Focus areas display
			if (input.autonomousConfig.focus_areas.length > 0) {
				const focusAreaRows = input.autonomousConfig.focus_areas.map((fa) => ({
					name: fa.name,
					description: fa.description,
					priority: fa.priority,
					schedule: fa.schedule || 'inherits agent schedule',
					program: fa.program || (input.harnessProgramSection || 'inherits harness default'),
					scope: fa.scope && fa.scope.length > 0 ? fa.scope.join(', ') : 'inherits delegation targets'
				}));
				autoScheduleChildren.push({
					id: 'presto-double-detail-auto-focus-areas',
					component_type: 'Card',
					props: {
						title: 'Focus Areas',
						subtitle: `${focusAreaRows.length} area(s)`,
						body: ''
					},
					children: [
						{
							id: 'presto-double-detail-auto-focus-table',
							component_type: 'Table',
							props: {
								columns: [
									{ key: 'name', label: 'Name' },
									{ key: 'description', label: 'Description' },
									{ key: 'priority', label: 'Priority' },
									{ key: 'schedule', label: 'Schedule' },
									{ key: 'program', label: 'Program' },
									{ key: 'scope', label: 'Scope' }
								],
								rows: focusAreaRows
							}
						}
					]
				});
			}

			if (input.bootstrapTaskError) {
				autoScheduleChildren.push({
					id: 'presto-double-detail-auto-bootstrap-error',
					component_type: 'Alert',
					props: {
						type: 'warning',
						message: input.bootstrapTaskError,
						closable: false
					}
				});
			}

			components.push({
				id: 'presto-double-detail-autonomous-schedule',
				component_type: 'Card',
				props: {
					title: 'Autonomous Schedule',
					subtitle: input.bootstrapTaskLoading ? 'Loading schedule status...' : humanSchedule || input.autonomousConfig.schedule,
					body: 'Autonomous mode configuration and last cycle status.'
				},
				children: autoScheduleChildren
			});
		}

		components.push({
			id: 'presto-double-detail-tab-actions',
			component_type: 'Stack',
			props: {
				direction: 'row',
				gap: '0.5rem',
				wrap: true
			},
			children: DETAIL_TABS.map((tab) => ({
				id: `presto-double-detail-tab:${tab.id}`,
				component_type: 'Button',
				label: tab.label,
				props: {
					interactive: true,
					variant: input.selectedTab === tab.id ? 'primary' : 'outline',
					size: 'sm',
					disabled: input.isSystemAgent
				}
			}))
		});

		if (input.isSystemAgent) {
			components.push({
				id: 'presto-double-detail-system-note',
				component_type: 'EmptyState',
				props: {
					title: 'System definition internals unavailable',
					description: 'Internal agents are managed by platform services and do not expose user-editable definition artifacts.'
				}
			});
			return components;
		}

		if (input.selectedTab === 'overview') {
			components.push(buildCrewMemberOverviewComponent({
				agentKind: input.agentKind || null,
				capabilityPacks: input.capabilityPacks,
				excludedPacks: input.excludedPacks,
				delegationTargets: input.delegateTo,
				maxDelegationDepth: input.maxDelegationDepth,
				userMemoryIsolation: input.userMemoryIsolation || null,
				readableAgents: input.readableAgents
			}));

			if (input.harnessEnabled) {
				const harnessChildren: CrewNativeComponent[] = [];

				if (input.harnessOverviewError) {
					harnessChildren.push({
						id: 'presto-harness-overview-error',
						component_type: 'Alert',
						props: { type: 'warning', message: input.harnessOverviewError, closable: false }
					});
				}

				if (input.harnessOverviewLoading) {
					harnessChildren.push({
						id: 'presto-harness-overview-loading',
						component_type: 'EmptyState',
						props: {
							title: 'Loading operator view',
							description: 'Collecting harness scope, focus-area health, and proposal signals...'
						}
					});
				} else if (!input.harnessOverview) {
					harnessChildren.push({
						id: 'presto-harness-overview-empty',
						component_type: 'EmptyState',
						props: {
							title: 'Operator view unavailable',
							description: 'This harness has no operator snapshot yet.'
						}
					});
				} else {
					const overviewContext = asRecord(input.harnessOverview['context']);
					const overviewProgram = overviewContext ? asRecord(overviewContext['program']) : null;
					const systemStatus = asRecord(input.harnessOverview['system_status']);
					const scopeAgents = readRecordArray(systemStatus, 'agents');
					const evaluation = asRecord(input.harnessOverview['evaluation']);
					const focusAreas = readRecordArray(evaluation, 'focus_areas');
					const proposalImpact = evaluation ? asRecord(evaluation['proposal_impact']) : null;
					const programMetrics = evaluation ? asRecord(evaluation['program_metrics']) : null;
					const metricDocuments = readRecordArray(programMetrics, 'documents');
					const recentProposals = readRecordArray(input.harnessOverview, 'recent_proposals');
					const selectedStructuralProposal = input.harnessStructuralProposals.find(
						(proposal) => readString(proposal, 'proposal_id') === input.selectedStructuralProposalId
					) || recentProposals.find(
						(proposal) => readString(proposal, 'proposal_id') === input.selectedStructuralProposalId
					) || null;
					const totalActiveCycles = scopeAgents.reduce(
						(sum, agent) => sum + readFiniteNumber(agent, 'active_cycles', 0),
						0
					);
					const totalOpenTasks = scopeAgents.reduce(
						(sum, agent) => sum + openTaskCount(asRecord(agent['task_stats'])),
						0
					);
					const metricsMet = readFiniteNumber(programMetrics, 'met_metrics', 0);
					const metricsTotal = readFiniteNumber(programMetrics, 'total_metrics', 0);
					const operatorItems = [
						{
							id: 'presto-harness-summary-scope',
							key: 'Scope agents',
							value: String(scopeAgents.length)
						},
						{
							id: 'presto-harness-summary-active-cycles',
							key: 'Active cycles',
							value: String(totalActiveCycles)
						},
						{
							id: 'presto-harness-summary-open-tasks',
							key: 'Open tasks',
							value: String(totalOpenTasks)
						},
						{
							id: 'presto-harness-summary-metrics',
							key: 'Program metrics',
							value: metricsTotal > 0 ? `${metricsMet}/${metricsTotal} met` : 'none defined'
						},
						{
							id: 'presto-harness-summary-applied-proposals',
							key: 'Applied proposals tracked',
							value: String(readFiniteNumber(proposalImpact, 'total_count', 0))
						},
						{
							id: 'presto-harness-summary-pending-structural',
							key: 'Pending structural proposals',
							value: input.harnessStructuralProposalsLoading
								? 'loading...'
								: String(input.harnessStructuralProposals.length)
						},
						{
							id: 'presto-harness-summary-owner-requests',
							key: 'Pending owner briefings',
							value: input.harnessOwnerRequestsLoading
								? 'loading...'
								: String(input.harnessOwnerRequests.length)
						}
					];
					const programPath = readString(overviewProgram || {}, 'relative_path');
					const programSection = readString(overviewProgram || {}, 'section');
					if (programPath) {
						operatorItems.push({
							id: 'presto-harness-summary-program',
							key: 'Current program',
							value: programSection ? `${programPath} · ${programSection}` : programPath
						});
					}

					harnessChildren.push({
						id: 'presto-harness-operator-summary',
						component_type: 'Card',
						props: {
							title: 'Operator Snapshot',
							subtitle: `${scopeAgents.length} scoped agent(s)`,
							body: 'Human-readable view over harness health, scope activity, and structural change signals.'
						},
						children: [
							{
								id: 'presto-harness-operator-badges',
								component_type: 'Stack',
								props: { direction: 'row', gap: '0.5rem', wrap: true },
								children: [
									{
										id: 'presto-harness-operator-badge-mode',
										component_type: 'Badge',
										props: { text: 'Harness', color: 'info' }
									},
									{
										id: 'presto-harness-operator-badge-scope',
										component_type: 'Badge',
										props: {
											text: `${scopeAgents.length} in scope`,
											color: scopeAgents.length > 0 ? 'success' : 'warning'
										}
									},
									{
										id: 'presto-harness-operator-badge-metrics',
										component_type: 'Badge',
										props: {
											text: metricsTotal > 0 ? `${metricsMet}/${metricsTotal} metrics met` : 'no metrics',
											color:
												metricsTotal > 0 && metricsMet === metricsTotal
													? 'success'
													: metricsTotal > 0
														? 'warning'
														: 'default'
										}
									}
								]
							},
							{
								id: 'presto-harness-operator-summary-data',
								component_type: 'DataList',
								props: { items: operatorItems }
							},
							{
								id: 'presto-harness-refresh',
								component_type: 'Button',
								label: 'Refresh operator view',
								props: {
									interactive: true,
									variant: 'secondary',
									size: 'sm'
								}
							}
						]
					});

					const focusAreaRows = focusAreas.map((focusArea, index) => {
						const episodeStats = asRecord(focusArea['episode_stats']);
						return {
							name: readString(focusArea, 'name') || `focus-${index + 1}`,
							priority: readString(focusArea, 'priority') || 'medium',
							episodes: String(readFiniteNumber(episodeStats, 'total', 0)),
							success: formatRatio(focusArea.success_rate),
							last_outcome: readString(episodeStats || {}, 'last_outcome') || '—',
							last_run: formatRelativeTimeFromIso(readString(episodeStats || {}, 'last_episode_at'))
						};
					});
					harnessChildren.push(
						buildSectionTable(
							'Focus Area Health',
							focusAreaRows.length > 0
								? `${focusAreaRows.length} focus area(s)`
								: 'No focus areas configured',
							focusAreaRows,
							'presto-harness-focus-area-health'
						)
					);

					const scopeStatusRows = scopeAgents.map((agent, index) => {
						const taskStats = asRecord(agent['task_stats']);
						const pendingTasks = readRecordArray(agent, 'pending_tasks');
						const topWork = pendingTasks
							.slice(0, 2)
							.map((task) => readString(task, 'title') || readString(task, 'task_id') || 'task')
							.join(' | ');
						return {
							agent: readString(agent, 'agent_name') || readString(agent, 'agent_id') || `agent-${index + 1}`,
							status: readString(agent, 'status') || 'unknown',
							active_cycles: String(readFiniteNumber(agent, 'active_cycles', 0)),
							open_tasks: String(openTaskCount(taskStats)),
							failed_tasks: String(readFiniteNumber(taskStats, 'failed', 0)),
							top_work: topWork || '—'
						};
					});
					harnessChildren.push(
						buildSectionTable(
							'Scope Status',
							scopeStatusRows.length > 0
								? `${scopeStatusRows.length} scoped agent(s)`
								: 'No scoped agents resolved',
							scopeStatusRows,
							'presto-harness-scope-status'
						)
					);

					const metricRows = metricDocuments.flatMap((document) => {
						const documentLabel =
							readString(document, 'relative_path')
							|| readString(document, 'title')
							|| 'program';
						return readRecordArray(document, 'metrics').map((metric) => ({
							document: documentLabel,
							metric: readString(metric, 'metric_id') || 'metric',
							current: formatMetricValue(
								metric['current'],
								readString(metric, 'unit') || 'count'
							),
							target: `${readString(metric, 'comparator') || '='} ${formatMetricValue(
								metric['target'],
								readString(metric, 'unit') || 'count'
							)}`,
							status: readBoolean(metric, 'met') === true
								? 'met'
								: readBoolean(metric, 'met') === false
									? 'missed'
									: readString(metric, 'reason') || 'n/a'
						}));
					});
					harnessChildren.push(
						buildSectionTable(
							'Program Metrics',
							metricRows.length > 0
								? `${metricsMet}/${metricsTotal} metric(s) met`
								: 'No program metrics defined',
							metricRows,
							'presto-harness-program-metrics'
						)
					);

					const metricParseErrors = metricDocuments.flatMap((document) =>
						readArray(document, 'parse_errors')
							.filter((value): value is string => typeof value === 'string' && value.trim().length > 0)
							.map((error, index) => {
								const documentLabel =
									readString(document, 'relative_path')
									|| readString(document, 'title')
									|| `document-${index + 1}`;
								return `${documentLabel}: ${error}`;
							})
					);
					if (metricParseErrors.length > 0) {
						harnessChildren.push({
							id: 'presto-harness-program-metric-errors',
							component_type: 'Alert',
							props: {
								type: 'warning',
								message: metricParseErrors.join(' | '),
								closable: false
							}
						});
					}

					const structuralReviewChildren: CrewNativeComponent[] = [];
					if (input.harnessStructuralProposalsError) {
						structuralReviewChildren.push({
							id: 'presto-harness-structural-review-error',
							component_type: 'Alert',
							props: {
								type: 'warning',
								message: input.harnessStructuralProposalsError,
								closable: false
							}
						});
					}
					if (input.harnessStructuralProposalsLoading) {
						structuralReviewChildren.push({
							id: 'presto-harness-structural-review-loading',
							component_type: 'EmptyState',
							props: {
								title: 'Loading structural review queue',
								description: 'Collecting actionable harness-owned structural proposals in this scope.'
							}
						});
					} else if (input.harnessStructuralProposals.length === 0) {
						structuralReviewChildren.push({
							id: 'presto-harness-structural-review-empty',
							component_type: 'EmptyState',
							props: {
								title: 'No actionable structural proposals',
								description: 'Pending proposals and approved proposals awaiting a successful apply will appear here.'
							}
						});
					} else {
						input.harnessStructuralProposals.forEach((proposal, index) => {
							const payload = asRecord(proposal['payload']);
							const proposalId = readString(proposal, 'proposal_id') || `proposal-${index + 1}`;
							const actionKind = readString(payload || {}, 'action_kind') || 'proposal';
							const summaryText =
								readString(payload || {}, 'summary') || humanizeIdentifier(actionKind, 'proposal');
							const targetAgent = readString(proposal, 'agent_id') || `agent-${index + 1}`;
							const rationale = readString(payload || {}, 'rationale');
							const focusArea = readString(payload || {}, 'focus_area');
							const evidenceRefs = readStringArray(payload, 'evidence_refs');
							const isRetryable = isHarnessStructuralProposalRetryable(proposal);
							const isSelected = input.selectedStructuralProposalId === proposalId;
							const isResolving = input.structuralProposalActionId === proposalId;
							const reviewItems = [
								{
									id: `presto-harness-structural-review-${proposalId}-status`,
									key: 'Status',
									value: readString(proposal, 'status') || 'unknown'
								},
								{
									id: `presto-harness-structural-review-${proposalId}-target`,
									key: 'Target',
									value: targetAgent
								},
								{
									id: `presto-harness-structural-review-${proposalId}-created`,
									key: 'Created',
									value: formatRelativeTimeFromIso(readString(proposal, 'created_at'))
								}
							];
							if (focusArea) {
								reviewItems.push({
									id: `presto-harness-structural-review-${proposalId}-focus`,
									key: 'Focus area',
									value: focusArea
								});
							}
							if (evidenceRefs.length > 0) {
								reviewItems.push({
									id: `presto-harness-structural-review-${proposalId}-evidence`,
									key: 'Evidence',
									value: evidenceRefs.join(' | ')
								});
							}

							structuralReviewChildren.push({
								id: `presto-harness-structural-review-${proposalId}`,
								component_type: 'Card',
								props: {
									title: summaryText,
									subtitle: `${humanizeIdentifier(actionKind)} · ${targetAgent}`,
									body:
										rationale
										|| (isRetryable
											? 'This proposal was already approved but has not been applied successfully yet.'
											: 'Approval-backed structural change proposed by the harness.')
								},
								children: [
									{
										id: `presto-harness-structural-review-${proposalId}-badges`,
										component_type: 'Stack',
										props: { direction: 'row', gap: '0.5rem', wrap: true },
										children: [
											{
												id: `presto-harness-structural-review-${proposalId}-badge-status`,
												component_type: 'Badge',
												props: {
													text: readString(proposal, 'status') || 'unknown',
													color: proposalStatusBadgeColor(readString(proposal, 'status') || '')
												}
											},
											{
												id: `presto-harness-structural-review-${proposalId}-badge-action`,
												component_type: 'Badge',
												props: {
													text: humanizeIdentifier(actionKind),
													color: 'info'
												}
											},
											...(isRetryable
												? [{
														id: `presto-harness-structural-review-${proposalId}-badge-retry`,
														component_type: 'Badge' as const,
														props: {
															text: 'awaiting apply',
															color: 'warning' as const
														}
													}]
												: []),
											...(isSelected
												? [{
														id: `presto-harness-structural-review-${proposalId}-badge-selected`,
														component_type: 'Badge' as const,
														props: {
															text: 'reviewing diff',
															color: 'success' as const
														}
													}]
												: [])
										]
									},
									{
										id: `presto-harness-structural-review-${proposalId}-data`,
										component_type: 'DataList',
										props: { items: reviewItems }
									},
									{
										id: `presto-harness-structural-review-${proposalId}-actions`,
										component_type: 'Stack',
										props: { direction: 'row', gap: '0.5rem', wrap: true },
										children: [
											{
												id: `review-structural-proposal-${proposalId}`,
												component_type: 'Button',
												label: isSelected ? 'Viewing diff' : 'Review diff',
												props: {
													interactive: true,
													variant: isSelected ? 'primary' : 'secondary',
													size: 'sm'
												}
											},
											{
												id: `resolve-structural-proposal-${proposalId}__decision__approve`,
												component_type: 'Button',
												label: isRetryable ? 'Retry apply' : 'Approve',
												props: {
													interactive: true,
													variant: 'primary',
													size: 'sm',
													disabled: isResolving
												}
											},
											{
												id: `resolve-structural-proposal-${proposalId}__decision__defer`,
												component_type: 'Button',
												label: 'Defer',
												props: {
													interactive: true,
													variant: 'secondary',
													size: 'sm',
													disabled: isResolving
												}
											},
											{
												id: `resolve-structural-proposal-${proposalId}__decision__reject`,
												component_type: 'Button',
												label: 'Reject',
												props: {
													interactive: true,
													variant: 'outline',
													size: 'sm',
													disabled: isResolving
												}
											}
										]
									}
								]
							});
						});
					}

					harnessChildren.push({
						id: 'presto-harness-structural-review-queue',
						component_type: 'Card',
						props: {
							title: 'Structural Review Queue',
							subtitle:
								input.harnessStructuralProposals.length > 0
									? `${input.harnessStructuralProposals.length} actionable proposal(s)`
									: 'No actionable structural proposals',
							body: 'Approve, retry, defer, or reject structural definition proposals without leaving the operator view.'
						},
						children: structuralReviewChildren
					});

					if (input.selectedStructuralProposalId) {
						if (!selectedStructuralProposal) {
							harnessChildren.push({
								id: 'presto-harness-structural-selection-missing',
								component_type: 'Alert',
								props: {
									type: 'warning',
									message: 'The selected proposal is no longer present in the recent harness snapshot. Refresh the operator view to reload the queue.',
									closable: false
								}
							});
						} else {
							const payload = asRecord(selectedStructuralProposal['payload']);
							const actionKind = readString(payload || {}, 'action_kind') || 'proposal';
							const selectedProposalId =
								readString(selectedStructuralProposal, 'proposal_id') || input.selectedStructuralProposalId;
							const selectedSummary =
								readString(payload || {}, 'summary') || humanizeIdentifier(actionKind, 'proposal');
							const selectedRationale = readString(payload || {}, 'rationale');
							const selectedEvidence = readStringArray(payload, 'evidence_refs');
							const selectionItems = [
								{
									id: 'presto-harness-structural-selection-status',
									key: 'Status',
									value: readString(selectedStructuralProposal, 'status') || 'unknown'
								},
								{
									id: 'presto-harness-structural-selection-proposal-id',
									key: 'Proposal ID',
									value: selectedProposalId
								},
								{
									id: 'presto-harness-structural-selection-target',
									key: 'Target',
									value: readString(selectedStructuralProposal, 'agent_id') || agentId
								},
								{
									id: 'presto-harness-structural-selection-created',
									key: 'Created',
									value: formatRelativeTimeFromIso(readString(selectedStructuralProposal, 'created_at'))
								}
							];
							if (selectedEvidence.length > 0) {
								selectionItems.push({
									id: 'presto-harness-structural-selection-evidence',
									key: 'Evidence',
									value: selectedEvidence.join(' | ')
								});
							}
							harnessChildren.push({
								id: 'presto-harness-structural-selection',
								component_type: 'Card',
								props: {
									title: 'Selected Proposal Diff',
									subtitle: `${humanizeIdentifier(actionKind)} · ${selectedSummary}`,
									body:
										selectedRationale
										|| 'Review the before/after YAML before resolving the proposal.'
								},
								children: [
									{
										id: 'presto-harness-structural-selection-data',
										component_type: 'DataList',
										props: { items: selectionItems }
									},
									{
										id: 'clear-structural-proposal-selection',
										component_type: 'Button',
										label: 'Clear selection',
										props: {
											interactive: true,
											variant: 'secondary',
											size: 'sm'
										}
									}
								]
							});
							harnessChildren.push({
								id: 'presto-harness-structural-selection-before',
								component_type: 'Card',
								props: {
									title: 'YAML Before',
									subtitle: 'Current definition snapshot',
									body: 'Definition state before the proposed structural change.'
								},
								children: [
									{
										id: 'presto-harness-structural-selection-before-code',
										component_type: 'CodeBlock',
										props: {
											language: 'yaml',
											code: readString(selectedStructuralProposal, 'yaml_before') || '',
											showLineNumbers: true
										}
									}
								]
							});
							harnessChildren.push({
								id: 'presto-harness-structural-selection-after',
								component_type: 'Card',
								props: {
									title: 'YAML After',
									subtitle: 'Proposed definition snapshot',
									body: 'Definition state that will be applied if this proposal is approved.'
								},
								children: [
									{
										id: 'presto-harness-structural-selection-after-code',
										component_type: 'CodeBlock',
										props: {
											language: 'yaml',
											code: readString(selectedStructuralProposal, 'yaml_after') || '',
											showLineNumbers: true
										}
									}
								]
							});
						}
					}

					const proposalRows = recentProposals.map((proposal, index) => {
						const payload = asRecord(proposal['payload']);
						return {
							created: formatRelativeTimeFromIso(readString(proposal, 'created_at')),
							target: readString(proposal, 'agent_id') || `agent-${index + 1}`,
							action: humanizeIdentifier(readString(payload || {}, 'action_kind') || 'proposal'),
							status: readString(proposal, 'status') || 'unknown',
							summary: readString(payload || {}, 'summary') || '—'
						};
					});
					harnessChildren.push(
						buildSectionTable(
							'Structural Proposals',
							proposalRows.length > 0
								? `${proposalRows.length} recent proposal(s)`
								: 'No recent structural proposals',
							proposalRows,
							'presto-harness-structural-proposals'
						)
					);

					const ownerRequestChildren: CrewNativeComponent[] = [];
					if (input.harnessOwnerRequestsError) {
						ownerRequestChildren.push({
							id: 'presto-harness-owner-requests-error',
							component_type: 'Alert',
							props: {
								type: 'warning',
								message: input.harnessOwnerRequestsError,
								closable: false
							}
						});
					}
					if (input.harnessOwnerRequestsLoading) {
						ownerRequestChildren.push({
							id: 'presto-harness-owner-requests-loading',
							component_type: 'EmptyState',
							props: {
								title: 'Loading owner briefings',
								description: 'Collecting pending harness briefings, escalations, and questions for this owner.'
							}
						});
					} else if (input.harnessOwnerRequests.length === 0) {
						ownerRequestChildren.push({
							id: 'presto-harness-owner-requests-empty',
							component_type: 'EmptyState',
							props: {
								title: 'No pending owner briefings',
								description: 'Harness notifications will appear here when the owner needs to review or acknowledge them.'
							}
						});
					} else {
						input.harnessOwnerRequests.forEach((request, index) => {
							const requestId = readString(request, 'id') || `owner-request-${index + 1}`;
							const requestType = readString(request, 'request_type');
							const requestContext = asRecord(request['context']);
							const requestKind =
								readString(requestContext || {}, 'kind') || ownerRequestKindFromType(requestType);
							const requestSeverity =
								readString(requestContext || {}, 'severity') || 'info';
							const requestTitle =
								readString(requestContext || {}, 'title')
								|| humanizeIdentifier(requestKind, 'Harness notification');
							const requestMessage =
								readString(requestContext || {}, 'message')
								|| readString(request, 'question')
								|| 'No briefing message attached.';
							const requestFocusArea = readString(requestContext || {}, 'focus_area');
							const requestTaskId = readString(requestContext || {}, 'task_id');
							const requestExecutionId = readString(requestContext || {}, 'execution_id');
							const requestGoalId = readString(requestContext || {}, 'goal_id');
							const requestCreatedAt =
								typeof request.created_at === 'number' && Number.isFinite(request.created_at)
									? formatRelativeTime(request.created_at)
									: 'unknown';
							const requestOptions = readArray(request, 'options')
								.map((option) => asRecord(option))
								.filter((option): option is Record<string, unknown> => option !== null);
							const requestItems = [
								{
									id: `presto-harness-owner-request-${requestId}-created`,
									key: 'Created',
									value: requestCreatedAt
								},
								{
									id: `presto-harness-owner-request-${requestId}-type`,
									key: 'Type',
									value: requestType || 'harness.notify_owner'
								}
							];
							if (requestFocusArea) {
								requestItems.push({
									id: `presto-harness-owner-request-${requestId}-focus`,
									key: 'Focus area',
									value: requestFocusArea
								});
							}
							if (requestGoalId) {
								requestItems.push({
									id: `presto-harness-owner-request-${requestId}-goal`,
									key: 'Goal',
									value: requestGoalId
								});
							}
							if (requestTaskId) {
								requestItems.push({
									id: `presto-harness-owner-request-${requestId}-task`,
									key: 'Task',
									value: requestTaskId
								});
							}
							if (requestExecutionId) {
								requestItems.push({
									id: `presto-harness-owner-request-${requestId}-execution`,
									key: 'Execution',
									value: requestExecutionId
								});
							}

							ownerRequestChildren.push({
								id: `presto-harness-owner-request-${requestId}`,
								component_type: 'Card',
								props: {
									title: requestTitle,
									subtitle: `${humanizeIdentifier(requestKind)} · ${requestCreatedAt}`,
									body: ''
								},
								children: [
									{
										id: `presto-harness-owner-request-${requestId}-badges`,
										component_type: 'Stack',
										props: { direction: 'row', gap: '0.5rem', wrap: true },
										children: [
											{
												id: `presto-harness-owner-request-${requestId}-kind`,
												component_type: 'Badge',
												props: {
													text: humanizeIdentifier(requestKind),
													color: harnessRequestKindBadgeColor(requestKind)
												}
											},
											{
												id: `presto-harness-owner-request-${requestId}-severity`,
												component_type: 'Badge',
												props: {
													text: requestSeverity,
													color: harnessSeverityBadgeColor(requestSeverity)
												}
											}
										]
									},
									{
										id: `presto-harness-owner-request-${requestId}-message`,
										component_type: 'Markdown',
										props: { content: requestMessage }
									},
									{
										id: `presto-harness-owner-request-${requestId}-data`,
										component_type: 'DataList',
										props: { items: requestItems }
									},
									{
										id: `presto-harness-owner-request-${requestId}-actions`,
										component_type: 'Stack',
										props: { direction: 'row', gap: '0.5rem', wrap: true },
										children:
											requestOptions.length > 0
												? requestOptions.map((option, optionIndex) => ({
														id: `respond-owner-request-${requestId}__decision__${readString(option, 'id') || `option-${optionIndex + 1}`}`,
														component_type: 'Button' as const,
														label: readString(option, 'label') || 'Respond',
														props: {
															interactive: true,
															variant: 'primary',
															size: 'sm',
															disabled: input.ownerRequestActionId === requestId
														}
													}))
												: [{
														id: `respond-owner-request-${requestId}__decision__acknowledge`,
														component_type: 'Button' as const,
														label: 'Acknowledge',
														props: {
															interactive: true,
															variant: 'primary',
															size: 'sm',
															disabled: input.ownerRequestActionId === requestId
														}
													}]
									}
								]
							});
						});
					}

					harnessChildren.push({
						id: 'presto-harness-owner-requests',
						component_type: 'Card',
						props: {
							title: 'Owner Briefings & Escalations',
							subtitle:
								input.harnessOwnerRequests.length > 0
									? `${input.harnessOwnerRequests.length} pending request(s)`
									: 'No pending owner notifications',
							body: 'Pending `notify_owner` briefings, escalations, and questions for the current harness owner.'
						},
						children: ownerRequestChildren
					});

					const ownerHistoryChildren: CrewNativeComponent[] = [];
					if (input.harnessOwnerRequestsError) {
						ownerHistoryChildren.push({
							id: 'presto-harness-owner-history-error',
							component_type: 'Alert',
							props: {
								type: 'warning',
								message: input.harnessOwnerRequestsError,
								closable: false
							}
						});
					}
					if (input.harnessOwnerRequestsLoading) {
						ownerHistoryChildren.push({
							id: 'presto-harness-owner-history-loading',
							component_type: 'EmptyState',
							props: {
								title: 'Loading briefing timeline',
								description: 'Rebuilding recent owner notifications and acknowledgements for this harness.'
							}
						});
					} else if (input.harnessOwnerRequestHistory.length === 0) {
						ownerHistoryChildren.push({
							id: 'presto-harness-owner-history-empty',
							component_type: 'EmptyState',
							props: {
								title: 'No recent briefing history',
								description: 'Resolved and pending `notify_owner` items will accumulate here as the harness runs.'
							}
						});
					} else {
						input.harnessOwnerRequestHistory.forEach((request, index) => {
							const requestId = readString(request, 'id') || `owner-history-${index + 1}`;
							const requestType = readString(request, 'request_type');
							const requestContext = asRecord(request['context']);
							const requestResponse = asRecord(request['response']);
							const requestKind =
								readString(requestContext || {}, 'kind') || ownerRequestKindFromType(requestType);
							const requestSeverity =
								readString(requestContext || {}, 'severity') || 'info';
							const requestStatus = ownerRequestStatusFromRecord(request);
							const requestTitle =
								readString(requestContext || {}, 'title')
								|| humanizeIdentifier(requestKind, 'Harness notification');
							const requestMessage =
								readString(requestContext || {}, 'message')
								|| readString(request, 'question')
								|| 'No briefing message attached.';
							const createdAt =
								typeof request.created_at === 'number' && Number.isFinite(request.created_at)
									? formatRelativeTime(request.created_at)
									: 'unknown';
							const resolvedAt =
								typeof request.resolved_at === 'number' && Number.isFinite(request.resolved_at)
									? formatRelativeTime(request.resolved_at)
									: null;
							const decision = readString(requestResponse || {}, 'decision');
							const channel = readString(requestResponse || {}, 'channel');
							const historyItems = [
								{
									id: `presto-harness-owner-history-${requestId}-created`,
									key: 'Created',
									value: createdAt
								},
								{
									id: `presto-harness-owner-history-${requestId}-status`,
									key: 'Status',
									value: humanizeIdentifier(requestStatus)
								}
							];
							if (resolvedAt) {
								historyItems.push({
									id: `presto-harness-owner-history-${requestId}-resolved`,
									key: 'Resolved',
									value: resolvedAt
								});
							}
							if (decision) {
								historyItems.push({
									id: `presto-harness-owner-history-${requestId}-decision`,
									key: 'Decision',
									value: humanizeIdentifier(decision)
								});
							}
							if (channel) {
								historyItems.push({
									id: `presto-harness-owner-history-${requestId}-channel`,
									key: 'Channel',
									value: channel
								});
							}

							ownerHistoryChildren.push({
								id: `presto-harness-owner-history-${requestId}`,
								component_type: 'Card',
								props: {
									title: requestTitle,
									subtitle: `${humanizeIdentifier(requestKind)} · ${createdAt}`,
									body: ''
								},
								children: [
									{
										id: `presto-harness-owner-history-${requestId}-badges`,
										component_type: 'Stack',
										props: { direction: 'row', gap: '0.5rem', wrap: true },
										children: [
											{
												id: `presto-harness-owner-history-${requestId}-kind`,
												component_type: 'Badge',
												props: {
													text: humanizeIdentifier(requestKind),
													color: harnessRequestKindBadgeColor(requestKind)
												}
											},
											{
												id: `presto-harness-owner-history-${requestId}-severity`,
												component_type: 'Badge',
												props: {
													text: requestSeverity,
													color: harnessSeverityBadgeColor(requestSeverity)
												}
											},
											{
												id: `presto-harness-owner-history-${requestId}-status-badge`,
												component_type: 'Badge',
												props: {
													text: humanizeIdentifier(requestStatus),
													color: ownerRequestStatusBadgeColor(requestStatus)
												}
											}
										]
									},
									{
										id: `presto-harness-owner-history-${requestId}-message`,
										component_type: 'Markdown',
										props: { content: requestMessage }
									},
									{
										id: `presto-harness-owner-history-${requestId}-data`,
										component_type: 'DataList',
										props: { items: historyItems }
									}
								]
							});
						});
					}

					harnessChildren.push({
						id: 'presto-harness-owner-history',
						component_type: 'Card',
						props: {
							title: 'Owner Briefing Timeline',
							subtitle:
								input.harnessOwnerRequestHistory.length > 0
									? `${input.harnessOwnerRequestHistory.length} recent notification(s)`
									: 'No recent owner notifications',
							body: 'Recent `notify_owner` briefings, escalations, questions, and their acknowledgement status for this harness owner.'
						},
						children: ownerHistoryChildren
					});

					const impactRows = readRecordArray(proposalImpact, 'proposals').map((proposal, index) => ({
						target: readString(proposal, 'agent_id') || `agent-${index + 1}`,
						action: humanizeIdentifier(readString(proposal, 'action_kind') || 'proposal'),
						status: readString(proposal, 'status') || 'unknown',
						delta: formatSignedRatioDelta(proposal['success_rate_delta']),
						applied: formatRelativeTimeFromIso(readString(proposal, 'applied_at'))
					}));
					harnessChildren.push(
						buildSectionTable(
							'Proposal Impact',
							impactRows.length > 0
								? `${impactRows.length} applied proposal(s)`
								: 'No applied proposals measured yet',
							impactRows,
							'presto-harness-proposal-impact'
						)
					);
				}

				components.push({
					id: 'presto-double-detail-harness-overview',
					component_type: 'Card',
					props: {
						title: 'Harness Operator View',
						subtitle: 'Thin control surface for human oversight',
						body: 'Read the live harness snapshot without dropping into raw YAML or memory internals.'
					},
					children: harnessChildren
				});
			}

			// ---------------------------------------------------------------
			// Task 5.11: Autonomous Proposals Inbox
			// Shows unapproved tasks created by the autonomous executor
			// (delegation/autonomous) for this agent, grouped by cycle.
			// ---------------------------------------------------------------
			const proposalsChildren: CrewNativeComponent[] = [];

			if (input.proposalsError) {
				proposalsChildren.push({
					id: 'presto-proposals-error',
					component_type: 'Alert',
					props: { type: 'warning', message: input.proposalsError, closable: false }
				});
			}

			if (input.proposalsLoading) {
				proposalsChildren.push({
					id: 'presto-proposals-loading',
					component_type: 'EmptyState',
					props: { title: 'Loading proposals', description: 'Fetching pending autonomous proposals...' }
				});
			} else if (input.autonomousProposals.length === 0) {
				proposalsChildren.push({
					id: 'presto-proposals-empty',
					component_type: 'EmptyState',
					props: { title: 'No pending proposals', description: 'When this agent creates autonomous tasks, they will appear here for approval.' }
				});
			} else {
				let cycleIndex = 0;
				for (const [cycleKey, cycleTasks] of input.proposalCycles) {
					const cycleLabel = cycleKey === 'ungrouped' ? 'Autonomous Cycle' : `Cycle (parent: ${cycleKey.slice(0, 12)}...)`;
					const cycleTimestamp = cycleTasks.length > 0 ? cycleTasks[0].createdAt : '';
					const cycleCardChildren: CrewNativeComponent[] = [];

					cycleCardChildren.push({
						id: `presto-proposals-cycle-${cycleIndex}-info`,
						component_type: 'DataList',
						props: {
							items: [
								{ id: `cycle-${cycleIndex}-count`, key: 'Tasks in cycle', value: String(cycleTasks.length) },
								{ id: `cycle-${cycleIndex}-time`, key: 'Created', value: cycleTimestamp ? formatRelativeTime(Date.parse(cycleTimestamp)) : 'unknown' }
							]
						}
					});

					if (cycleTasks.length > 1) {
						const chainText = cycleTasks.map((t) => t.title.length > 30 ? t.title.slice(0, 30) + '...' : t.title).join(' -> ');
						cycleCardChildren.push({
							id: `presto-proposals-cycle-${cycleIndex}-chain`,
							component_type: 'Text',
							props: { children: chainText, variant: 'body' }
						});
					}

					cycleTasks.forEach((proposalTask, taskIndex) => {
						const taskItems = [
							{ id: `proposal-${cycleIndex}-${taskIndex}-title`, key: 'Title', value: proposalTask.title },
							{ id: `proposal-${cycleIndex}-${taskIndex}-status`, key: 'Status', value: proposalTask.status },
							{ id: `proposal-${cycleIndex}-${taskIndex}-created-by`, key: 'Created by', value: asString(proposalTask.createdBy) }
						];
						if (proposalTask.description) {
							taskItems.push({ id: `proposal-${cycleIndex}-${taskIndex}-desc`, key: 'Intent', value: proposalTask.description.length > 100 ? proposalTask.description.slice(0, 100) + '...' : proposalTask.description });
						}
						if (proposalTask.dependsOn && proposalTask.dependsOn.length > 0) {
							taskItems.push({ id: `proposal-${cycleIndex}-${taskIndex}-deps`, key: 'Depends on', value: proposalTask.dependsOn.map((d) => d.slice(0, 8)).join(', ') });
						}

						cycleCardChildren.push({
							id: `presto-proposals-cycle-${cycleIndex}-task-${taskIndex}`,
							component_type: 'Card',
							props: { title: proposalTask.title, subtitle: `${proposalTask.status} | ${asString(proposalTask.createdBy)}`, body: proposalTask.description || '' },
							children: [
								{ id: `presto-proposals-cycle-${cycleIndex}-task-${taskIndex}-data`, component_type: 'DataList', props: { items: taskItems } },
								{
									id: `presto-proposals-cycle-${cycleIndex}-task-${taskIndex}-actions`,
									component_type: 'Stack',
									props: { direction: 'row', gap: '0.5rem', wrap: true },
									children: [
										{ id: `approve-proposal-${proposalTask.id}`, component_type: 'Button', label: 'Approve', props: { interactive: true, variant: 'primary', size: 'sm' } },
										{ id: `reject-proposal-${proposalTask.id}`, component_type: 'Button', label: 'Reject', props: { interactive: true, variant: 'outline', size: 'sm' } }
									]
								}
							]
						});
					});

					cycleCardChildren.push({
						id: `presto-proposals-cycle-${cycleIndex}-approve-all`,
						component_type: 'Stack',
						props: { direction: 'row', gap: '0.5rem', wrap: true },
						children: [
							{ id: `approve-all-cycle-${cycleKey}`, component_type: 'Button', label: input.isApprovingAll ? 'Approving...' : `Approve All (${cycleTasks.length})`, props: { interactive: true, variant: 'primary', size: 'sm', disabled: input.isApprovingAll } }
						]
					});

					proposalsChildren.push({
						id: `presto-proposals-cycle-${cycleIndex}`,
						component_type: 'Card',
						props: { title: cycleLabel, subtitle: `${cycleTasks.length} task(s) pending approval`, body: '' },
						children: cycleCardChildren
					});
					cycleIndex++;
				}
			}

			components.push({
				id: 'presto-double-detail-proposals',
				component_type: 'Card',
				props: {
					title: `Pending Proposals${input.autonomousProposals.length > 0 ? ` (${input.autonomousProposals.length})` : ''}`,
					subtitle: 'Autonomous tasks awaiting approval',
					body: 'Tasks created by the autonomous executor that need user approval before execution.'
				},
				children: proposalsChildren
			});
		} else if (input.selectedTab === 'memory') {
			// Primary Agent badge at Lore tab header
			if (input.isPrimary) {
				components.push({
					id: 'presto-double-detail-lore-primary-badge',
					component_type: 'Stack',
					props: { direction: 'row', gap: '0.5rem', wrap: true },
					children: [
						{
							id: 'presto-double-detail-lore-primary-badge-tag',
							component_type: 'Badge',
							props: {
								text: 'Primary Agent',
								color: 'success'
							}
						},
						{
							id: 'presto-double-detail-lore-kind-badge',
							component_type: 'Badge',
							props: {
								text: input.agentKind,
								color: kindBadgeColor(input.agentKind)
							}
						}
					]
				});
			}

			const tierRows = input.sortedMemoryTiers.map((tier, index) => {
				const tierRecord = asRecord(tier);
				const tierName = readString(tierRecord || {}, 'name') || `tier-${index + 1}`;
				const loreSummary = findLoreSummary(tierName);
				return {
					tier: tierName,
					scope: readString(tierRecord || {}, 'scope') || 'agent',
					renderer: loreSummary?.renderer || readString(tierRecord || {}, 'renderer') || 'definition-only',
					has_data: loreSummary ? (loreSummary.has_data ? 'yes' : 'no') : 'unknown',
					updated: loreSummary?.last_updated || 'n/a',
					open: {
						kind: 'link',
						href: memoryTierHref(tierRecord, tierName, loreSummary),
						label: 'Open viewer'
					}
				};
			});
			components.push({
				id: 'presto-double-detail-memory',
				component_type: 'Card',
				props: {
					title: 'Memory tiers',
					subtitle: `${tierRows.length} configured tier(s)`,
					body: input.loreSummariesLoading ? 'Loading live memory status…' : 'Tier definitions with links to memory-tier reader.'
				},
				children: [
					input.loreSummariesError
						? {
								id: 'presto-double-detail-memory-error',
								component_type: 'Alert',
								props: {
									type: 'error',
									message: input.loreSummariesError,
									closable: false
								}
							}
						: {
								id: 'presto-double-detail-memory-table',
								component_type: 'Table',
								props: {
									columns: [
										{ key: 'tier', label: 'Tier' },
										{ key: 'scope', label: 'Scope' },
										{ key: 'renderer', label: 'Renderer' },
										{ key: 'has_data', label: 'Has data' },
										{ key: 'updated', label: 'Updated' },
										{ key: 'open', label: 'Viewer' }
									],
									rows: tierRows
								}
							}
				]
			});
			components.push(buildSectionTable('Consolidation rules', `${input.memoryConsolidation.length} rule(s)`, input.memoryConsolidation.map((rule) => ({ raw: toJson(rule) })), 'presto-double-detail-consolidation'));

			// --- Task 4.5.10: Consolidation Health Table ---
			const healthTableChildren: CrewNativeComponent[] = [];
			if (input.tierHealthError) {
				healthTableChildren.push({
					id: 'presto-double-detail-tier-health-error',
					component_type: 'Alert',
					props: {
						type: 'warning',
						message: input.tierHealthError,
						closable: false
					}
				});
			}
			if (input.tierHealthRows.length > 0) {
				healthTableChildren.push({
					id: 'presto-double-detail-tier-health-table',
					component_type: 'Table',
					props: {
						columns: [
							{ key: 'tier', label: 'Tier' },
							{ key: 'scope', label: 'Scope' },
							{ key: 'last_consolidated', label: 'Last consolidated' },
							{ key: 'pending', label: 'Pending episodes' },
							{ key: 'status', label: 'Status' }
						],
						rows: input.tierHealthRows.map((row) => ({
							tier: row.name,
							scope: row.scope,
							last_consolidated: formatRelativeTimeFromIso(row.last_consolidated_at),
							pending: String(row.pending_episode_count),
							status: {
								kind: 'badge',
								text: row.status.charAt(0).toUpperCase() + row.status.slice(1),
								color: row.status === 'healthy' ? 'success' : row.status === 'stale' ? 'warning' : 'error'
							}
						}))
					}
				});
			} else if (!input.tierHealthLoading) {
				healthTableChildren.push({
					id: 'presto-double-detail-tier-health-empty',
					component_type: 'EmptyState',
					props: {
						title: 'No tier health data',
						description: 'Consolidation health data is not yet available for this agent.'
					}
				});
			}
			components.push({
				id: 'presto-double-detail-tier-health',
				component_type: 'Card',
				props: {
					title: 'Consolidation Health',
					subtitle: input.tierHealthLoading ? 'Loading health status...' : `${input.tierHealthRows.length} tier(s) monitored`,
					body: 'Real-time consolidation pipeline status for each memory tier.'
				},
				children: healthTableChildren
			});

			// --- Task 4.5.12: User Knowledge Surface ---
			const userKnowledgeChildren: CrewNativeComponent[] = [];
			if (input.userKnowledgeError) {
				userKnowledgeChildren.push({
					id: 'presto-double-detail-user-knowledge-error',
					component_type: 'Alert',
					props: {
						type: 'warning',
						message: input.userKnowledgeError,
						closable: false
					}
				});
			}

			const hasAnyUserKnowledge =
				input.userKnowledge.preferences.length > 0 ||
				input.userKnowledge.contacts.length > 0 ||
				input.userKnowledge.expertise.length > 0;

			if (input.userKnowledgeLoading) {
				userKnowledgeChildren.push({
					id: 'presto-double-detail-user-knowledge-loading',
					component_type: 'EmptyState',
					props: {
						title: 'Loading user knowledge',
						description: 'Fetching user-scoped memory data...'
					}
				});
			} else if (!hasAnyUserKnowledge) {
				userKnowledgeChildren.push({
					id: 'presto-double-detail-user-knowledge-empty',
					component_type: 'EmptyState',
					props: {
						title: 'No user knowledge collected yet',
						description: 'User knowledge will be gathered automatically during interactions.'
					}
				});
			} else {
				// Preferences table
				if (input.userKnowledge.preferences.length > 0) {
					userKnowledgeChildren.push({
						id: 'presto-double-detail-user-preferences',
						component_type: 'Card',
						props: {
							title: 'Preferences',
							subtitle: `${input.userKnowledge.preferences.length} preference(s)`,
							body: ''
						},
						children: [
							{
								id: 'presto-double-detail-user-preferences-table',
								component_type: 'Table',
								props: {
									columns: [
										{ key: 'key', label: 'Key' },
										{ key: 'value', label: 'Value' }
									],
									rows: input.userKnowledge.preferences.map((pref, index) => ({
										key: pref.key || `preference-${index + 1}`,
										value: pref.value || '—'
									}))
								}
							}
						]
					});
				}

				// Contacts list
				if (input.userKnowledge.contacts.length > 0) {
					userKnowledgeChildren.push({
						id: 'presto-double-detail-user-contacts',
						component_type: 'Card',
						props: {
							title: 'Contacts',
							subtitle: `${input.userKnowledge.contacts.length} contact(s)`,
							body: ''
						},
						children: [
							{
								id: 'presto-double-detail-user-contacts-table',
								component_type: 'Table',
								props: {
									columns: [
										{ key: 'name', label: 'Name' },
										{ key: 'relationship', label: 'Relationship' },
										{ key: 'last_mentioned', label: 'Last mentioned' }
									],
									rows: input.userKnowledge.contacts.map((contact, index) => ({
										name: contact.name || `contact-${index + 1}`,
										relationship: contact.relationship || '—',
										last_mentioned: contact.last_mentioned ? formatRelativeTimeFromIso(contact.last_mentioned) : '—'
									}))
								}
							}
						]
					});
				}

				// Expertise chips
				if (input.userKnowledge.expertise.length > 0) {
					userKnowledgeChildren.push({
						id: 'presto-double-detail-user-expertise',
						component_type: 'Card',
						props: {
							title: 'Expertise',
							subtitle: `${input.userKnowledge.expertise.length} area(s)`,
							body: ''
						},
						children: [
							{
								id: 'presto-double-detail-user-expertise-chips',
								component_type: 'Stack',
								props: { direction: 'row', gap: '0.35rem', wrap: true },
								children: input.userKnowledge.expertise.map((skill, index) => ({
									id: `presto-double-detail-user-expertise-${index}`,
									component_type: 'Tag',
									props: { text: skill, color: 'info' }
								}))
							}
						]
					});
				}
			}

			// --- Task 4.5.13: Merge to shared knowledge button ---
			// Show when isolation is anything other than 'shared' (e.g. 'fully_isolated')
			const isNonSharedIsolation = !!input.userMemoryIsolation && input.userMemoryIsolation !== 'shared';
			const showMergeButton = isNonSharedIsolation && input.hasLocalUserMemory;
			const showDisabledMergeButton = isNonSharedIsolation && !input.hasLocalUserMemory && !input.userKnowledgeLoading;

			if (showMergeButton || showDisabledMergeButton) {
				const mergeChildren: CrewNativeComponent[] = [];

				mergeChildren.push({
					id: 'presto-double-detail-merge-user-memory-btn',
					component_type: 'Button',
					label: input.isMergingUserMemory ? 'Merging...' : 'Merge to shared knowledge',
					props: {
						interactive: true,
						variant: 'primary',
						size: 'sm',
						disabled: !showMergeButton || input.isMergingUserMemory,
						tooltip: showDisabledMergeButton ? 'No local memory to merge' : undefined
					}
				});

				if (input.mergeUserMemoryMessage) {
					mergeChildren.push({
						id: 'presto-double-detail-merge-user-memory-success',
						component_type: 'Alert',
						props: {
							type: 'success',
							message: input.mergeUserMemoryMessage,
							closable: false
						}
					});
				}

				if (input.mergeUserMemoryError) {
					mergeChildren.push({
						id: 'presto-double-detail-merge-user-memory-error',
						component_type: 'Alert',
						props: {
							type: 'error',
							message: input.mergeUserMemoryError,
							closable: false
						}
					});
				}

				userKnowledgeChildren.push({
					id: 'presto-double-detail-merge-user-memory',
					component_type: 'Stack',
					props: { direction: 'column', gap: '0.5rem' },
					children: mergeChildren
				});
			}

			components.push({
				id: 'presto-double-detail-user-knowledge',
				component_type: 'Card',
				props: {
					title: 'User Knowledge',
					subtitle: input.userKnowledgeLoading ? 'Loading...' : hasAnyUserKnowledge ? 'User-scoped memory data' : 'No data yet',
					body: 'Knowledge gathered about the user from interactions.'
				},
				children: userKnowledgeChildren
			});
		} else if (input.selectedTab === 'episodes') {
			components.push({
				id: 'presto-double-detail-episodes',
				component_type: 'Card',
				props: {
					title: 'History',
					subtitle: 'Definition-backed retention view',
					body: 'Use History Browser for timeline drill-down.'
				},
				children: [
					input.retentionEpisodes
						? {
								id: 'presto-double-detail-episodes-code',
								component_type: 'CodeBlock',
								props: {
									language: 'json',
									code: toJson(input.retentionEpisodes),
									showLineNumbers: true
								}
							}
						: {
								id: 'presto-double-detail-episodes-empty',
								component_type: 'EmptyState',
								props: {
									title: 'No explicit history retention config',
									description: 'Using defaults.'
								}
							}
				]
			});
		} else if (input.selectedTab === 'corrections') {
			components.push(buildSectionTable('Feedback loops', `${input.feedbackLoops.length} loop(s)`, input.feedbackLoops.map((loop) => ({ raw: toJson(loop) })), 'presto-double-detail-feedback'));
			components.push({
				id: 'presto-double-detail-retention-corrections',
				component_type: 'Card',
				props: {
					title: 'Retention.corrections',
					subtitle: 'Correction retention policy',
					body: input.retentionCorrections ? 'Configured' : 'No explicit config found.'
				},
				children: [
					input.retentionCorrections
						? {
								id: 'presto-double-detail-retention-corrections-code',
								component_type: 'CodeBlock',
								props: {
									language: 'json',
									code: toJson(input.retentionCorrections),
									showLineNumbers: true
								}
							}
						: {
								id: 'presto-double-detail-retention-corrections-empty',
								component_type: 'EmptyState',
								props: {
									title: 'No correction retention config',
									description: 'No explicit correction retention config found.'
								}
						}
				]
			});
		} else {
			components.push({
				id: 'presto-double-detail-config',
				component_type: 'Card',
				props: {
					title: 'Config (YAML)',
					subtitle: 'Read-only serialized definition',
					body: 'Declarative surface browser available under rules tab.'
				},
				children: [
					{
						id: 'presto-double-detail-config-open-covenant',
						component_type: 'Button',
						label: 'Open declarative surface browser',
						props: {
							interactive: true,
							variant: 'secondary',
							size: 'sm'
						}
					},
					{
						id: 'presto-double-detail-config-yaml',
						component_type: 'CodeBlock',
						props: {
							language: 'yaml',
							code: input.configYaml,
							showLineNumbers: true
						}
					}
				]
			});
		}

		return components;
	}

	async function handleSurfaceInteraction(event: CustomEvent<CrewNativeInteractionEventDetail>): Promise<void> {
		const detail = event?.detail;
		if (!detail || detail.interaction !== 'action') return;

		if (detail.componentId === 'presto-double-detail-action-back') {
			await goto('/crew');
			return;
		}
		if (detail.componentId === 'presto-double-detail-action-edit' && agentId && !isSystemAgent) {
			await goto(`/crew/new?edit=${encodeURIComponent(agentId)}`);
			return;
		}
		if (detail.componentId === 'presto-double-detail-action-execution') {
			openExecutionPanel();
			return;
		}
		if (detail.componentId === 'presto-double-detail-action-make-primary') {
			void handleSetPrimary();
			return;
		}
		if (detail.componentId === 'presto-double-detail-merge-user-memory-btn') {
			void mergeUserMemoryToShared();
			return;
		}
		if (detail.componentId === 'presto-double-detail-config-open-covenant' && agentId) {
			await goto(`/crew/${encodeURIComponent(agentId)}/rules`);
			return;
		}

		// Task 5.11: Proposal approval/rejection handlers
		const approveMatch = /^approve-proposal-(.+)$/.exec(detail.componentId);
		if (approveMatch && approveMatch[1]) {
			void handleApproveProposal(approveMatch[1]);
			return;
		}
		const rejectMatch = /^reject-proposal-(.+)$/.exec(detail.componentId);
		if (rejectMatch && rejectMatch[1]) {
			void handleRejectProposal(rejectMatch[1]);
			return;
		}
		const approveAllMatch = /^approve-all-cycle-(.+)$/.exec(detail.componentId);
		if (approveAllMatch && approveAllMatch[1]) {
			void handleApproveAllCycle(approveAllMatch[1]);
			return;
		}
		if (detail.componentId === 'presto-harness-refresh' && agentId) {
			void refreshHarnessOperatorView(agentId);
			return;
		}
		const reviewStructuralMatch = /^review-structural-proposal-(.+)$/.exec(detail.componentId);
		if (reviewStructuralMatch && reviewStructuralMatch[1]) {
			selectedStructuralProposalId = reviewStructuralMatch[1];
			return;
		}
		if (detail.componentId === 'clear-structural-proposal-selection') {
			selectedStructuralProposalId = null;
			return;
		}
		const resolveStructuralMatch =
			/^resolve-structural-proposal-(.+)__decision__(approve|reject|defer)$/.exec(
				detail.componentId
			);
		if (resolveStructuralMatch && resolveStructuralMatch[1] && resolveStructuralMatch[2]) {
			void handleStructuralProposalDecision(
				resolveStructuralMatch[1],
				resolveStructuralMatch[2] as 'approve' | 'reject' | 'defer'
			);
			return;
		}
		const ownerRequestMatch =
			/^respond-owner-request-(.+)__decision__(.+)$/.exec(detail.componentId);
		if (ownerRequestMatch && ownerRequestMatch[1] && ownerRequestMatch[2]) {
			void handleOwnerRequestResponse(ownerRequestMatch[1], ownerRequestMatch[2]);
			return;
		}

		const tabMatch = /^presto-double-detail-tab:(.+)$/.exec(detail.componentId);
		if (tabMatch && tabMatch[1] && agentId && !isSystemAgent) {
			await goto(`/crew/${encodeURIComponent(agentId)}?tab=${encodeURIComponent(tabMatch[1])}`, {
				replaceState: true,
				noScroll: true
			});
		}
	}

	$: if (
		browser
		&& agentId
		&& !routeParamError
		&& (
			agentId !== activeRequestId
			|| `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}` !== lastHydratedScopeKey
		)
	) {
		void hydrateAgent(agentId);
	}

	onMount(() => {
		v2Events.connectGlobal();
		// The verdict's durations and its stall threshold are read against this,
		// so it has to keep moving while the drawer is open. One timer for the
		// page's life, as every other task surface does it.
		executionPanelNow = Date.now();
		executionPanelClock = setInterval(
			() => (executionPanelNow = Date.now()),
			EXECUTION_PANEL_CLOCK_MS
		);
	});

	onDestroy(() => {
		if (executionPanelClock !== null) clearInterval(executionPanelClock);
		executionPanelClock = null;
		// The subscription outlives the drawer's markup — closing the drawer only
		// stops the reactive statement below from re-arming it. Without this it
		// keeps writing into a destroyed component's variables.
		executionPanelStreamStop?.();
		executionPanelStreamStop = null;
		executionPanelSubjectKey = null;
	});

	/* ── the cycle's task panel ───────────────────────────────────────────
	   Keyed on what is open, so the page's own polling costs nothing and only
	   opening the drawer spends a request — and then one subscription, which
	   costs no requests at all. */
	$: if (browser) syncExecutionPanel(executionPanelOpen ? executionPanelTarget : null);

	/**
	 * The drawer's whole input, through the execution adapter.
	 *
	 * **`null` for the file list, and that is the honest value**, not a
	 * shortcut: `/v3/tasks/{id}/outputs` cannot answer for an `agent-cycle:` id,
	 * so nothing here has read what the cycle produced. `[]` would claim it
	 * produced nothing. The Output act is therefore absent, and the run's
	 * completion summary goes with it — recorded as a loss in the component doc
	 * rather than papered over.
	 *
	 * The key check is what keeps a state read for the previous cycle from
	 * rendering under this one.
	 */
	$: executionPanelModel =
		executionPanelTarget === null || executionPanelState === null
			|| executionPanelStateKey !== cycleKey(executionPanelTarget)
			? null
			: toExecutionPanelModel(executionPanelState, null);
</script>

<svelte:head>
	<title>{displayAgentName} · Crew Member · Magician</title>
</svelte:head>

<div class="agent-detail-route presto-gaui-page">
	<div class="crew-detail-breadcrumb-bar">
		<a href="/crew" class="crew-detail-back-link">
			<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
				<path d="M19 12H5M12 19l-7-7 7-7" />
			</svg>
			<span>Back to Crew Fleet</span>
		</a>
		<div class="crew-detail-status-pill" class:crew-detail-status-pill--active={summary?.status === 'triggered' || Boolean(summary?.current_cycle_id)}>
			<span class="crew-pulse-dot" class:crew-pulse-dot--active={summary?.status === 'triggered' || Boolean(summary?.current_cycle_id)}></span>
			<span>{summary?.status === 'triggered' || Boolean(summary?.current_cycle_id) ? 'Live Execution Active' : isSystemAgent ? 'System Internal' : 'Operational Ready'}</span>
		</div>
	</div>

	<NativeCrewRenderer
		components={headerComponents}
		validateRouteContract={false}
		on:interaction={handleSurfaceInteraction}
	/>
	<LiveAgentSurface agentId={agentId} />
	<div class="crew-detail-overview-slot">
		<NativeCrewRenderer
			components={detailComponentsBeforeOverview}
			validateRouteContract={false}
			on:interaction={handleSurfaceInteraction}
		/>
		{#if overviewComponentIndex >= 0}
			<div class="crew-overview-dashboard-grid">
				<div class="crew-overview-column">
					<CrewMemberOverview overview={crewOverview} idNamespace="crew-route-overview" />
					{#if !isSystemAgent}
						<EffectiveToolPolicyPanel {agentId} />
					{/if}
				</div>
				<div class="crew-overview-column">
					<CrewHealthOverview health={crewHealth} loading={crewHealthLoading} />
					{#if !isSystemAgent}
						<AgentModelPinsPanel {agentId} on:saved={(event) => (record = event.detail.record)} />
					{/if}
				</div>
			</div>
			<NativeCrewRenderer
				components={detailComponentsAfterOverview}
				validateRouteContract={false}
				on:interaction={handleSurfaceInteraction}
			/>
		{/if}
	</div>
</div>

<!--
	The cycle's run, in the shared drawer. The chrome — scrim, dialog role, focus
	capture and restore, Escape, loading skeleton, header — is
	`TaskPanelDrawer`'s and is not restated here.

	**No action slot.** Every verb the retired panel offered here acted on a
	task, and a cycle is not one: there is nothing to stop, run or reset through
	a task endpoint that will not answer for an `agent-cycle:` id. The page's own
	cycle controls stay where they are, on the rows behind this.
-->
{#if executionPanelOpen && executionPanelTarget}
	<TaskPanelDrawer
		task={executionPanelModel}
		title={executionPanelTitle}
		loadError={executionPanelLoadError}
		lastLoadedAt={executionPanelLoadedAt}
		now={executionPanelNow}
		on:close={closeExecutionPanel}
		on:retry={handleExecutionPanelRetry}
	/>
{/if}

<style>
	.agent-detail-route {
		max-width: var(--app-content-max, 1360px);
		width: 100%;
		margin: 0 auto;
		padding: 1.25rem 1.5rem 5rem;
		box-sizing: border-box;
		display: flex;
		flex-direction: column;
		gap: 1rem;
		min-width: 0;
		overflow-x: hidden;
	}

	.crew-detail-breadcrumb-bar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 1rem;
		padding: 0.2rem 0.25rem;
	}

	.crew-detail-back-link {
		display: inline-flex;
		align-items: center;
		gap: 0.45rem;
		font-size: 0.84rem;
		font-weight: 600;
		color: var(--text-secondary, #6b665e);
		text-decoration: none;
		transition: color 0.15s ease;
	}

	.crew-detail-back-link:hover {
		color: var(--accent-primary, #ff6b6b);
	}

	.crew-detail-status-pill {
		display: inline-flex;
		align-items: center;
		gap: 0.45rem;
		padding: 0.25rem 0.65rem;
		border-radius: 999px;
		font-size: 0.76rem;
		font-weight: 600;
		background: var(--bg-surface, #fbfaf8);
		border: 1px solid var(--border-soft, #ebe7e0);
		color: var(--text-secondary, #6b665e);
	}

	.crew-detail-status-pill--active {
		background: color-mix(in srgb, var(--accent-primary, #ff6b6b) 8%, var(--bg-card, #ffffff));
		border-color: color-mix(in srgb, var(--accent-primary, #ff6b6b) 30%, transparent);
		color: var(--accent-primary, #e05252);
	}

	.crew-pulse-dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: #10b981;
		flex-shrink: 0;
	}

	.crew-pulse-dot--active {
		background: var(--accent-primary, #ff6b6b);
		box-shadow: 0 0 0 0 rgba(255, 107, 107, 0.6);
		animation: pulse-ring 2s cubic-bezier(0.4, 0, 0.6, 1) infinite;
	}

	@keyframes pulse-ring {
		0% {
			box-shadow: 0 0 0 0 rgba(255, 107, 107, 0.6);
		}
		70% {
			box-shadow: 0 0 0 7px rgba(255, 107, 107, 0);
		}
		100% {
			box-shadow: 0 0 0 0 rgba(255, 107, 107, 0);
		}
	}

	.crew-detail-overview-slot {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		min-width: 0;
		width: 100%;
		max-width: 100%;
		box-sizing: border-box;
	}

	.crew-overview-dashboard-grid {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 1.25rem;
		align-items: start;
		width: 100%;
		max-width: 100%;
		min-width: 0;
		box-sizing: border-box;
	}

	.crew-overview-column {
		display: flex;
		flex-direction: column;
		gap: 1.25rem;
		min-width: 0;
		max-width: 100%;
		box-sizing: border-box;
	}

	/* Command Masthead Header Card Styling */
	:global(.agent-detail-route [data-component-id="presto-double-detail-header"]) {
		background: linear-gradient(135deg, var(--bg-card, #ffffff) 0%, color-mix(in srgb, var(--accent-primary, #ff6b6b) 4%, var(--bg-card, #ffffff)) 100%) !important;
		border: 1px solid var(--border-soft, #ebe7e0) !important;
		border-radius: 14px !important;
		padding: 1.25rem 1.6rem !important;
		box-shadow: var(--shadow-sm, 0 1px 3px rgba(0, 0, 0, 0.04)) !important;
		min-width: 0 !important;
		max-width: 100% !important;
		box-sizing: border-box !important;
		overflow-wrap: anywhere !important;
	}

	:global(.agent-detail-route [data-component-id="presto-double-detail-header"] .crew-native-card__header) {
		margin-bottom: 0.5rem;
	}

	:global(.agent-detail-route [data-component-id="presto-double-detail-header"] h2) {
		font-size: 1.65rem !important;
		font-weight: 800 !important;
		letter-spacing: -0.025em;
		color: var(--text-primary, #1e1b18) !important;
		margin: 0 !important;
		line-height: 1.2 !important;
	}

	:global(.agent-detail-route [data-component-id="presto-double-detail-header"] .crew-native-subtitle) {
		font-family: var(--font-mono, monospace) !important;
		font-size: 0.82rem !important;
		font-weight: 600 !important;
		color: var(--accent-primary, #e05252) !important;
		margin-top: 0.25rem !important;
	}

	:global(.agent-detail-route [data-component-id="presto-double-detail-header"] .crew-native-body) {
		font-size: 0.92rem !important;
		line-height: 1.5 !important;
		color: var(--text-secondary, #6b665e) !important;
		margin-top: 0.35rem !important;
		max-width: 75ch !important;
	}

	:global(.agent-detail-route [data-component-id="presto-double-detail-actions"]) {
		display: flex !important;
		flex-wrap: wrap !important;
		align-items: center !important;
		gap: 0.65rem !important;
		margin-top: 1rem !important;
		padding-top: 0.85rem !important;
		border-top: 1px solid var(--border-soft, #ebe7e0) !important;
	}

	/* Agent Identity Summary Card */
	:global(.agent-detail-route [data-component-id="presto-double-detail-summary"]) {
		background: var(--bg-card, #ffffff) !important;
		border: 1px solid var(--border-soft, #ebe7e0) !important;
		border-radius: 14px !important;
		padding: 1.25rem 1.6rem !important;
		box-shadow: var(--shadow-sm, 0 1px 3px rgba(0, 0, 0, 0.04)) !important;
		min-width: 0 !important;
		max-width: 100% !important;
		box-sizing: border-box !important;
	}

	:global(.agent-detail-route [data-component-id="presto-double-detail-summary-meta"]) {
		display: grid !important;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 140px), 1fr)) !important;
		gap: 0.85rem !important;
		padding: 1rem 1.15rem !important;
		background: var(--bg-surface, #fbfaf8) !important;
		border-radius: 10px !important;
		border: 1px solid var(--border-soft, #ebe7e0) !important;
		margin-top: 1.15rem !important;
		min-width: 0 !important;
		max-width: 100% !important;
		box-sizing: border-box !important;
	}

	:global(.agent-detail-route [data-component-id="presto-double-detail-summary-meta"] > div) {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		min-width: 0 !important;
		overflow: hidden;
	}

	:global(.agent-detail-route [data-component-id="presto-double-detail-summary-meta"] dt) {
		font-size: 0.72rem !important;
		text-transform: uppercase !important;
		letter-spacing: 0.04em !important;
		color: var(--text-muted, #948e85) !important;
		font-weight: 700 !important;
		margin-bottom: 0.2rem !important;
	}

	:global(.agent-detail-route [data-component-id="presto-double-detail-summary-meta"] dd) {
		font-size: 0.95rem !important;
		font-weight: 700 !important;
		color: var(--text-primary, #1e1b18) !important;
		font-family: var(--font-mono, monospace) !important;
		margin: 0 !important;
		word-break: break-all !important;
		overflow-wrap: anywhere !important;
		min-width: 0 !important;
	}

	/* Autonomous Schedule Card */
	:global(.agent-detail-route [data-component-id="presto-double-detail-autonomous-schedule"]) {
		background: linear-gradient(135deg, var(--bg-card, #ffffff) 0%, color-mix(in srgb, var(--accent-secondary, #4d9de0) 4%, var(--bg-card, #ffffff)) 100%) !important;
		border: 1px solid color-mix(in srgb, var(--accent-secondary, #4d9de0) 25%, var(--border-soft, #ebe7e0)) !important;
		border-radius: 14px !important;
		padding: 1.25rem 1.6rem !important;
		box-shadow: var(--shadow-sm, 0 1px 3px rgba(0, 0, 0, 0.04)) !important;
		min-width: 0 !important;
		max-width: 100% !important;
		box-sizing: border-box !important;
	}

	:global(.agent-detail-route [data-component-id="presto-double-detail-auto-data"]) {
		display: grid !important;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 160px), 1fr)) !important;
		gap: 0.85rem !important;
		padding: 1rem 1.15rem !important;
		background: var(--bg-card, #ffffff) !important;
		border-radius: 10px !important;
		border: 1px solid var(--border-soft, #ebe7e0) !important;
		margin-top: 0.85rem !important;
		min-width: 0 !important;
		max-width: 100% !important;
		box-sizing: border-box !important;
	}

	:global(.agent-detail-route [data-component-id="presto-double-detail-auto-data"] > div) {
		min-width: 0 !important;
		overflow: hidden;
	}

	:global(.agent-detail-route [data-component-id="presto-double-detail-auto-data"] dt) {
		font-size: 0.72rem !important;
		text-transform: uppercase !important;
		letter-spacing: 0.04em !important;
		color: var(--text-muted, #948e85) !important;
		font-weight: 700 !important;
	}

	:global(.agent-detail-route [data-component-id="presto-double-detail-auto-data"] dd) {
		font-size: 0.92rem !important;
		font-weight: 700 !important;
		color: var(--text-primary, #1e1b18) !important;
		font-family: var(--font-mono, monospace) !important;
		margin: 0 !important;
		word-break: break-all !important;
		overflow-wrap: anywhere !important;
		min-width: 0 !important;
	}

	/* Tab Bar Strip */
	:global(.agent-detail-route [data-component-id="presto-double-detail-tab-actions"]) {
		display: flex !important;
		flex-direction: row !important;
		gap: 0.35rem !important;
		padding: 0.35rem !important;
		margin: 0.5rem 0 1rem !important;
		background: var(--bg-surface, #fbfaf8) !important;
		border-radius: 12px !important;
		border: 1px solid var(--border-soft, #ebe7e0) !important;
		overflow-x: auto !important;
		min-width: 0 !important;
		max-width: 100% !important;
		box-sizing: border-box !important;
		scrollbar-width: none !important;
		-ms-overflow-style: none !important;
		-webkit-overflow-scrolling: touch;
	}

	:global(.agent-detail-route [data-component-id="presto-double-detail-tab-actions"]::-webkit-scrollbar) {
		display: none !important;
		width: 0 !important;
		height: 0 !important;
	}

	:global(.agent-detail-route [data-component-id^="presto-double-detail-tab:"]) {
		display: inline-flex !important;
		align-items: center !important;
		gap: 0.45rem !important;
		padding: 0.5rem 1rem !important;
		border-radius: 8px !important;
		font-size: 0.85rem !important;
		font-weight: 600 !important;
		transition: all 0.15s ease-in-out !important;
		cursor: pointer !important;
		white-space: nowrap !important;
		position: static !important;
		margin: 0 !important;
		flex-shrink: 0 !important;
	}

	:global(.agent-detail-route [data-component-id^="presto-double-detail-tab:"].crew-native-button--primary) {
		background: var(--bg-card, #ffffff) !important;
		color: var(--accent-primary, #e05252) !important;
		border: 1px solid var(--border-soft, #ebe7e0) !important;
		font-weight: 700 !important;
		box-shadow: 0 1px 3px rgba(0, 0, 0, 0.05) !important;
	}

	:global(.agent-detail-route [data-component-id^="presto-double-detail-tab:"].crew-native-button--outline) {
		background: transparent !important;
		color: var(--text-secondary, #6b665e) !important;
		border: 1px solid transparent !important;
	}

	:global(.agent-detail-route [data-component-id^="presto-double-detail-tab:"].crew-native-button--outline:hover) {
		background: color-mix(in srgb, var(--accent-primary, #ff6b6b) 6%, transparent) !important;
		color: var(--text-primary, #2d2a26) !important;
	}

	/* Crew Health Card and Stacking in Dashboard */
	:global(.agent-detail-route .crew-health) {
		width: 100% !important;
		max-width: 100% !important;
		min-width: 0 !important;
		box-sizing: border-box !important;
		overflow: hidden !important;
		background: var(--bg-card, #ffffff) !important;
		border: 1px solid var(--border-soft, #ebe7e0) !important;
		border-radius: 14px !important;
		box-shadow: var(--shadow-sm, 0 1px 3px rgba(0, 0, 0, 0.04)) !important;
	}

	:global(.agent-detail-route .crew-health__body) {
		display: flex !important;
		flex-direction: column !important;
		gap: 1rem !important;
		min-width: 0 !important;
		width: 100% !important;
	}

	:global(.agent-detail-route .crew-health__overall) {
		min-width: 0 !important;
		width: 100% !important;
	}

	:global(.agent-detail-route .crew-health__metrics) {
		display: grid !important;
		grid-template-columns: repeat(3, minmax(0, 1fr)) !important;
		gap: 0.75rem 1rem !important;
		padding: 0.85rem 0 !important;
		border-inline: 0 !important;
		border-block: 1px solid var(--border-soft, #ebe7e0) !important;
		min-width: 0 !important;
		width: 100% !important;
	}

	:global(.agent-detail-route .crew-health__trend) {
		min-width: 0 !important;
		width: 100% !important;
	}

	:global(.agent-detail-route .crew-health__trend svg) {
		width: 100% !important;
		max-width: 100% !important;
		height: auto !important;
	}

	:global(.agent-detail-route .crew-health__skeleton) {
		display: flex !important;
		flex-direction: column !important;
		gap: 0.75rem !important;
	}

	/* LiveAgentSurface streamlined styling */
	:global(.agent-detail-route .live-agent-surface) {
		margin: 0 !important;
		border: 1px solid var(--border-soft, #ebe7e0) !important;
		border-radius: 12px !important;
		box-shadow: var(--shadow-sm, 0 1px 2px rgba(0, 0, 0, 0.03)) !important;
		overflow: hidden !important;
		max-width: 100% !important;
		min-width: 0 !important;
		box-sizing: border-box !important;
	}

	:global(.agent-detail-route .live-agent-surface__header) {
		padding: 0.65rem 1.15rem !important;
		background: var(--bg-surface, #fbfaf8) !important;
		border-bottom: 1px solid var(--border-soft, #ebe7e0) !important;
	}

	:global(.agent-detail-route .live-agent-surface__empty) {
		padding: 0.8rem 1.15rem !important;
		font-size: 0.84rem !important;
		color: var(--text-muted, #8c857b) !important;
	}

	:global(.agent-detail-route .live-agent-surface h2) {
		font-size: 0.95rem !important;
		font-weight: 700 !important;
	}

	/* Card and Table styling across tabs */
	:global(.agent-detail-route .crew-native-table-section) {
		background: var(--bg-card, #ffffff) !important;
		border: 1px solid var(--border-soft, #ebe7e0) !important;
		border-radius: 12px !important;
		padding: 1.25rem 1.5rem !important;
		box-shadow: var(--shadow-sm, 0 1px 3px rgba(0, 0, 0, 0.04)) !important;
		max-width: 100% !important;
		min-width: 0 !important;
		box-sizing: border-box !important;
	}

	:global(.agent-detail-route .crew-native-table-wrap) {
		overflow-x: auto !important;
		max-width: 100% !important;
		box-sizing: border-box !important;
	}

	:global(.agent-detail-route .crew-native-code) {
		background: var(--bg-card, #ffffff) !important;
		border: 1px solid var(--border-soft, #ebe7e0) !important;
		border-radius: 12px !important;
		padding: 1.25rem 1.5rem !important;
		box-shadow: var(--shadow-sm, 0 1px 3px rgba(0, 0, 0, 0.04)) !important;
		max-width: 100% !important;
		min-width: 0 !important;
		box-sizing: border-box !important;
	}

	:global(.agent-detail-route .crew-native-code pre) {
		overflow-x: auto !important;
		max-width: 100% !important;
		box-sizing: border-box !important;
	}

	@media (max-width: 1100px) {
		.crew-overview-dashboard-grid {
			grid-template-columns: 1fr;
		}
	}

	@media (max-width: 768px) {
		.agent-detail-route {
			padding: 1rem 1rem 4rem;
		}

		.crew-detail-breadcrumb-bar {
			flex-direction: column;
			align-items: flex-start;
			gap: 0.5rem;
		}

		:global(.agent-detail-route .crew-health__metrics) {
			grid-template-columns: repeat(2, minmax(0, 1fr)) !important;
		}
	}
</style>
