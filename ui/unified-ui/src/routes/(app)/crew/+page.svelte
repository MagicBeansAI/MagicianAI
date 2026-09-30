<script lang="ts">
	import { browser } from '$app/environment';
	import { goto, replaceState } from '$app/navigation';
	import { page } from '$app/stores';
	import { onMount } from 'svelte';
	import { get } from 'svelte/store';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import {
		type AgentEventEnvelope,
		type ConnectionStatus,
		type V2WebSocketEvent,
		v2Events
	} from '$lib/realtime/v2-websocket';
	import {
		agentList,
		agentStatusCounts,
		agentStoreState,
		agentsNeedingAttention,
		agentsWithPendingApprovals,
		isAgentMutating,
		loadHarnessRuntimeStatus,
		pauseAgent,
		refreshAgents,
		resumeAgent,
		runningAgentCount,
		setHarnessRuntimeEnabled,
		systemAgentList,
		triggerAgent,
		type AgentSummary,
		type HarnessRuntimeStatus,
		type SystemAgentSummary
	} from '$lib/stores/agentStore';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import { timedFetch } from '$lib/shared/fetch';
	import CrewLeaderboard from '$lib/magician/crew/CrewLeaderboard.svelte';
	import EffectiveToolPolicyPanel from '$lib/magician/crew/EffectiveToolPolicyPanel.svelte';
	import ServerPager from '$lib/shared/components/ServerPager.svelte';
	import {
		fetchCrewHealthCached,
		healthByAgent,
		healthBand,
		type AgentHealthProjection
	} from '$lib/magician/crew/health';
	import { crewLeaderboardRowsFromAgents } from '$lib/magician/crew/leaderboard';
	import { fetchFleetState, type FleetStateSnapshot } from '$lib/magician/square/fleetState';
	import { groupNeedsByAgent } from '$lib/magician/square/attentionGlue';
	import { attentionStore } from '$lib/stores/attentionStore';

	type BadgeColor = 'default' | 'success' | 'warning' | 'error' | 'info';
	type FailureCounts = Record<string, number>;
	type FailureCycleEntry = { key: string; timestamp: number };
	type CrewTabId = 'members' | 'leaderboard' | 'hierarchy' | 'pipeline';
	type StatusFilter = 'all' | 'active' | 'attention' | 'paused' | 'disabled';
	type ViewMode = 'table' | 'grid';

	interface AgentRenderRecord {
		agentId: string;
		rowKey: string;
		name: string;
		roleLabel: string;
		description: string;
		statusLabel: string;
		statusColor: BadgeColor;
		availabilityLabel: string;
		availabilityColor: BadgeColor;
		configuredDisabled: boolean;
		lastActive: string;
		kind: string;
		currentCycle: string;
		pendingApprovals: string;
		capabilityPacks: string;
		delegatesTo: string[];
		delegatedBy: string[];
		isSystem: boolean;
		isReadOnly: boolean;
	}

	interface HierarchyRow {
		rowId: string;
		depth: number;
		agentId: string;
		member: string;
		roleLabel: string;
		parent: string;
		availabilityLabel: string;
		availabilityColor: BadgeColor;
		statusLabel: string;
		statusColor: BadgeColor;
		delegatesTo: string;
		path: string;
		note: string;
	}

	interface DelegationGraph {
		rows: HierarchyRow[];
		edgeCount: number;
		rootCount: number;
		missingTargetCount: number;
		cycleCount: number;
	}

	const STATUS_BUCKETS: AgentSummary['status'][] = ['idle', 'triggered', 'running', 'paused', 'completed', 'partial', 'error', 'disabled'];
	const FAILURE_OUTCOMES = new Set([
		'failure',
		'failed',
		'goal_failed',
		'error',
		'budget_exhausted',
		'user_intervened',
		'cannot_proceed',
		'loop_detected',
		'max_iterations'
	]);
	const FAILURE_DEDUP_TTL_MS = 24 * 60 * 60 * 1000;
	const FAILURE_DEDUP_MAX_ENTRIES = 4000;

	let activeTab: CrewTabId = 'members';
	let statusFilter: StatusFilter = 'all';
	let viewMode: ViewMode = 'table';

	let interactionBusy = false;
	let reloadingDefinitions = false;
	let pageError: string | null = null;
	let lastUpdatedAt: number | null = null;
	let listRefreshToken = 0;
	let crewMounted = false;
	let currentCrewScopeKey = '';
	let lastCrewScopeKey = '';
	let connectionStatus: ConnectionStatus = 'disconnected';
	let failureCounts: FailureCounts = {};
	let customFilter = '';
	let hierarchyFilter = '';
	let systemFilter = '';
	let expandedCustomRows = new Set<string>();
	let expandedHierarchyRows = new Set<string>();
	let expandedSystemRows = new Set<string>();
	let harnessRuntime: HarnessRuntimeStatus | null = null;
	let harnessRuntimeLoading = true;
	let harnessRuntimeMutating = false;
	let harnessRuntimeError: string | null = null;
	let harnessRuntimeRequestToken = 0;
	let harnessStatusLabel = 'Loading';
	let harnessStatusColor: BadgeColor = 'default';
	let leaderboardHealthMap: Map<string, AgentHealthProjection> | null = null;
	let leaderboardHealthTimer: ReturnType<typeof setInterval> | null = null;
	let leaderboardFleetState: FleetStateSnapshot | null = null;
	let leaderboardFleetStateTimer: ReturnType<typeof setInterval> | null = null;

	const countedFailureCycles = new Map<string, number>();
	const failureCycleIndex: FailureCycleEntry[] = [];

	$: routeError = pageError || $agentStoreState.error;
	$: currentCrewScopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: customRecords = $agentList.map(mapCustomAgent).sort(sortAgentsByName);
	$: customRecordsById = new Map(customRecords.map((entry) => [entry.agentId, entry]));
	$: systemRecords = $systemAgentList.map(mapSystemAgent).sort(sortAgentsByName);
	$: delegationGraph = buildDelegationGraph(customRecords);
	$: filteredSystemRecords = filterAgents(systemRecords, systemFilter);
	$: filteredHierarchyRows = filterHierarchyRows(delegationGraph.rows, hierarchyFilter);
	$: totalAgents = customRecords.length + systemRecords.length;
	$: activeNow = $runningAgentCount;
	$: knownFailures = customRecords.reduce((total, agent) => total + totalFailureCount(agent.agentId), 0);
	$: harnessEnabled = harnessRuntime?.enabled === true;
	$: harnessStatusLabel = harnessRuntimeLoading
		? 'Loading'
		: !harnessRuntime
			? 'Unavailable'
			: harnessEnabled
				? 'On'
				: 'Off';
	$: harnessStatusColor = harnessRuntimeLoading
		? 'default'
		: !harnessRuntime
			? 'error'
			: harnessEnabled
				? 'success'
				: 'warning';
	$: leaderboardNeedsByAgent = groupNeedsByAgent($attentionStore);
	$: leaderboardRows = crewLeaderboardRowsFromAgents(
		$agentList,
		leaderboardHealthMap,
		leaderboardFleetState,
		new Set(leaderboardNeedsByAgent.keys())
	);
	$: leaderboardRowById = new Map(leaderboardRows.map((row) => [row.id, row]));

	$: attentionSet = new Set([
		...$agentsNeedingAttention.map((entry) => entry.agent_id),
		...$agentsWithPendingApprovals.map((entry) => entry.agent_id)
	]);
	$: attentionCount = customRecords.filter((agent) =>
		attentionSet.has(agent.agentId) ||
		totalFailureCount(agent.agentId) > 0 ||
		agent.statusLabel === 'error' ||
		agent.statusLabel === 'partial'
	).length;
	$: pausedCount = customRecords.filter((agent) => agent.statusLabel === 'paused').length;
	$: disabledCount = customRecords.filter((agent) => agent.availabilityLabel === 'disabled' || agent.configuredDisabled).length;

	$: displayedCustomRecords = customRecords.filter((agent) => {
		if (statusFilter === 'active') {
			if (agent.statusLabel !== 'running' && agent.statusLabel !== 'triggered') return false;
		} else if (statusFilter === 'attention') {
			const needsAttn =
				attentionSet.has(agent.agentId) ||
				totalFailureCount(agent.agentId) > 0 ||
				agent.statusLabel === 'error' ||
				agent.statusLabel === 'partial';
			if (!needsAttn) return false;
		} else if (statusFilter === 'paused') {
			if (agent.statusLabel !== 'paused') return false;
		} else if (statusFilter === 'disabled') {
			if (agent.availabilityLabel !== 'disabled' && !agent.configuredDisabled) return false;
		}

		if (!customFilter.trim()) return true;
		const query = customFilter.trim().toLowerCase();
		return [
			agent.name,
			agent.agentId,
			agent.roleLabel,
			agent.description,
			agent.kind,
			agent.statusLabel,
			agent.availabilityLabel,
			agent.capabilityPacks,
			agent.delegatesTo.join(' '),
			agent.delegatedBy.join(' ')
		]
			.join(' ')
			.toLowerCase()
			.includes(query);
	});

	const PAGE_SIZE_OPTIONS = [12, 24, 48];
	let pageSize = PAGE_SIZE_OPTIONS[0];
	let currentPage = 1;

	let lastPaginationFilterKey = '';
	$: currentPaginationFilterKey = `${customFilter}::${statusFilter}::${pageSize}`;
	$: if (currentPaginationFilterKey !== lastPaginationFilterKey) {
		lastPaginationFilterKey = currentPaginationFilterKey;
		currentPage = 1;
	}

	$: totalCustomItems = displayedCustomRecords.length;
	$: pageCount = Math.max(1, Math.ceil(totalCustomItems / pageSize));
	$: if (currentPage > pageCount) {
		currentPage = Math.max(1, pageCount);
	}
	$: startItem = totalCustomItems === 0 ? 0 : (currentPage - 1) * pageSize + 1;
	$: endItem = totalCustomItems === 0 ? 0 : Math.min(totalCustomItems, (currentPage - 1) * pageSize + pageSize);
	$: pagedCustomRecords = displayedCustomRecords.slice(
		(currentPage - 1) * pageSize,
		currentPage * pageSize
	);

	$: tabParam = $page?.url?.searchParams?.get('tab');
	$: if (tabParam === 'members' || tabParam === 'leaderboard' || tabParam === 'hierarchy' || tabParam === 'pipeline') {
		if (activeTab !== tabParam) {
			activeTab = tabParam;
		}
	}

	function setTab(tab: CrewTabId): void {
		activeTab = tab;
		if (browser) {
			try {
				const url = new URL(window.location.href);
				url.searchParams.set('tab', tab);
				replaceState(url.toString(), {});
			} catch {
				// URL update failure ignored
			}
		}
	}

	function setStatusFilter(filter: StatusFilter): void {
		statusFilter = filter;
		if (activeTab !== 'members') {
			setTab('members');
		}
	}

	function setViewMode(mode: ViewMode): void {
		viewMode = mode;
		if (browser) {
			try {
				localStorage.setItem('magician:crew-view-mode', mode);
			} catch {
				// storage error ignored
			}
		}
	}

	function asString(value: unknown): string {
		if (typeof value === 'string') return value;
		if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') return String(value);
		return '';
	}

	function asRecord(value: unknown): Record<string, unknown> | null {
		return value != null && typeof value === 'object' && !Array.isArray(value) ? (value as Record<string, unknown>) : null;
	}

	function asFiniteNumber(value: unknown): number {
		const normalized = typeof value === 'number' ? value : Number.parseFloat(asString(value));
		return Number.isFinite(normalized) ? normalized : 0;
	}

	function readString(record: Record<string, unknown>, key: string): string | undefined {
		const value = record[key];
		return typeof value === 'string' && value.trim().length > 0 ? value : undefined;
	}

	function readNumber(record: Record<string, unknown>, key: string): number | undefined {
		const value = record[key];
		return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
	}

	function formatRelativeTime(timestamp: number | null | undefined, now = Date.now()): string {
		if (!timestamp || !Number.isFinite(timestamp)) return 'never';
		const delta = timestamp - now;
		const abs = Math.abs(delta);
		const minutes = Math.round(abs / 1000 / 60);
		const hours = Math.round(abs / 1000 / 60 / 60);
		const days = Math.round(abs / 1000 / 60 / 60 / 24);
		if (minutes < 1) return 'just now';
		if (minutes < 60) return `${minutes}m ${delta < 0 ? 'ago' : 'from now'}`;
		if (hours < 36) return `${hours}h ${delta < 0 ? 'ago' : 'from now'}`;
		return `${days}d ${delta < 0 ? 'ago' : 'from now'}`;
	}

	function safeAgentValue(value: unknown): string {
		return asString(value).trim() || 'n/a';
	}

	function safeRowKey(value: string): string {
		const normalized = asString(value).trim().toLowerCase();
		const safe = normalized
			.replace(/[^a-z0-9._-]/g, '-')
			.replace(/-+/g, '-')
			.replace(/^-+|-+$/g, '');
		return safe || `agent-${Math.max(0, normalized.length)}`;
	}

	function normalizeAgentId(agentId: unknown): string {
		return asString(agentId).trim() || 'unknown-agent';
	}

	function formatRoleLabel(agentId: string): string {
		const acronymWords = new Set(['ai', 'api', 'ceo', 'cfo', 'cmo', 'coo', 'cro', 'cto', 'gtm', 'qa', 'ui', 'ux']);
		return agentId
			.split(/[-_]+/)
			.map((word) => {
				const normalized = word.trim().toLowerCase();
				if (!normalized) return '';
				if (acronymWords.has(normalized)) return normalized.toUpperCase();
				return `${normalized.charAt(0).toUpperCase()}${normalized.slice(1)}`;
			})
			.filter(Boolean)
			.join(' ');
	}

	function statusLabelFromSummary(status: AgentSummary['status']): string {
		return status.replace(/_/g, ' ');
	}

	function statusColorFromSummary(status: AgentSummary['status']): BadgeColor {
		if (status === 'running' || status === 'triggered') return 'info';
		if (status === 'completed') return 'success';
		if (status === 'paused' || status === 'partial') return 'warning';
		if (status === 'error') return 'error';
		return 'default';
	}

	function connectionBadgeColor(status: ConnectionStatus): BadgeColor {
		if (status === 'connected') return 'success';
		if (status === 'connecting') return 'warning';
		return 'error';
	}

	function badgeClass(color: BadgeColor): string {
		return `crew-badge crew-badge-${color}`;
	}

	function normalizeDelegationTargets(targets: string[] | undefined): string[] {
		if (!Array.isArray(targets) || targets.length === 0) return [];
		const normalized = new Set<string>();
		for (const target of targets) {
			const value = asString(target).trim();
			if (value) normalized.add(value);
		}
		return Array.from(normalized);
	}

	function formatCapabilityPacks(packs: string[] | undefined): string {
		if (!packs || packs.length === 0) return 'none';
		return packs.join(', ');
	}

	function mapSystemAgent(agent: SystemAgentSummary): AgentRenderRecord {
		const agentId = normalizeAgentId(agent.agent_id);
		return {
			agentId,
			rowKey: safeRowKey(agentId),
			name: safeAgentValue(agent.name || agentId),
			roleLabel: formatRoleLabel(agentId),
			description: safeAgentValue(agent.description || 'Read-only internal pipeline role'),
			statusLabel: 'system',
			statusColor: 'default',
			availabilityLabel: 'enabled',
			availabilityColor: 'success',
			configuredDisabled: false,
			lastActive: 'n/a',
			kind: 'System',
			currentCycle: 'n/a',
			pendingApprovals: '0',
			capabilityPacks: 'none',
			delegatesTo: [],
			delegatedBy: [],
			isSystem: true,
			isReadOnly: true
		};
	}

	function mapCustomAgent(agent: AgentSummary): AgentRenderRecord {
		const agentId = normalizeAgentId(agent.agent_id);
		const rawUpdatedAt = asFiniteNumber(agent.updated_at);
		const pendingApprovals = Math.max(0, asFiniteNumber(agent.pending_approvals));
		const configuredDisabled = agent.configured_disabled === true;
		const isDisabled = agent.disabled === true || agent.status === 'disabled';
		return {
			agentId,
			rowKey: safeRowKey(agentId),
			name: safeAgentValue(agent.name || agentId),
			roleLabel: formatRoleLabel(agentId),
			description: safeAgentValue(agent.description || 'No description'),
			statusLabel: isDisabled ? 'disabled' : statusLabelFromSummary(agent.status || 'idle'),
			statusColor: isDisabled ? 'warning' : statusColorFromSummary(agent.status || 'idle'),
			availabilityLabel: isDisabled ? 'disabled' : 'enabled',
			availabilityColor: isDisabled ? 'warning' : 'success',
			configuredDisabled,
			lastActive: formatRelativeTime(rawUpdatedAt),
			kind: safeAgentValue(agent.kind || 'Personal'),
			currentCycle: safeAgentValue(agent.current_cycle_id || 'none'),
			pendingApprovals: String(pendingApprovals),
			capabilityPacks: formatCapabilityPacks(agent.tools),
			delegatesTo: normalizeDelegationTargets(agent.delegation_targets),
			delegatedBy: [],
			isSystem: false,
			isReadOnly: false
		};
	}

	function sortAgentsByName(left: AgentRenderRecord, right: AgentRenderRecord): number {
		return left.name.localeCompare(right.name) || left.agentId.localeCompare(right.agentId);
	}

	function agentDisplayName(agent: AgentRenderRecord): string {
		return agent.name && agent.name !== agent.agentId ? `${agent.name} (${agent.agentId})` : agent.agentId;
	}

	function formatAgentRefs(agentIds: string[], agentsById: Map<string, AgentRenderRecord>): string {
		if (agentIds.length === 0) return 'n/a';
		return agentIds
			.map((agentId) => {
				if (agentId === '*') return '*';
				const agent = agentsById.get(agentId);
				return agent ? agentDisplayName(agent) : `${agentId} (missing)`;
			})
			.join(', ');
	}

	function hierarchyPrefix(depth: number): string {
		if (depth <= 0) return 'root';
		return `${'>'.repeat(Math.min(depth, 8))} level ${depth}`;
	}

	function availabilityForHierarchy(agent: AgentRenderRecord, inheritedDisabledFrom: string | null): { text: string; color: BadgeColor } {
		if (agent.configuredDisabled) return { text: 'disabled', color: 'warning' };
		if (inheritedDisabledFrom) return { text: `disabled via ${inheritedDisabledFrom}`, color: 'warning' };
		if (agent.availabilityLabel === 'disabled') return { text: 'disabled', color: 'warning' };
		return { text: 'enabled', color: 'success' };
	}

	function buildDelegationGraph(agents: AgentRenderRecord[]): DelegationGraph {
		const agentsById = new Map<string, AgentRenderRecord>();
		for (const agent of agents) {
			if (!agent.isSystem) {
				agent.delegatedBy = [];
				agentsById.set(agent.agentId, agent);
			}
		}

		const childrenById = new Map<string, string[]>();
		const parentsById = new Map<string, string[]>();
		const missingTargetsByParent = new Map<string, string[]>();
		let edgeCount = 0;

		for (const agent of agentsById.values()) {
			for (const targetId of agent.delegatesTo) {
				if (targetId === '*') continue;
				if (!agentsById.has(targetId)) {
					const missing = missingTargetsByParent.get(agent.agentId) ?? [];
					missing.push(targetId);
					missingTargetsByParent.set(agent.agentId, missing);
					continue;
				}
				const children = childrenById.get(agent.agentId) ?? [];
				children.push(targetId);
				childrenById.set(agent.agentId, children);
				const parents = parentsById.get(targetId) ?? [];
				parents.push(agent.agentId);
				parentsById.set(targetId, parents);
				edgeCount += 1;
			}
		}

		for (const [agentId, parentIds] of parentsById) {
			const agent = agentsById.get(agentId);
			if (agent) agent.delegatedBy = Array.from(new Set(parentIds)).sort();
		}
		for (const [agentId, childIds] of childrenById) {
			const sorted = Array.from(new Set(childIds)).sort((left, right) => {
				const leftAgent = agentsById.get(left);
				const rightAgent = agentsById.get(right);
				if (!leftAgent || !rightAgent) return left.localeCompare(right);
				return sortAgentsByName(leftAgent, rightAgent);
			});
			childrenById.set(agentId, sorted);
		}

		const orderedAgents = Array.from(agentsById.values()).sort(sortAgentsByName);
		const roots = orderedAgents.filter((agent) => (parentsById.get(agent.agentId) ?? []).length === 0);
		const rows: HierarchyRow[] = [];
		const seenAgents = new Set<string>();
		let cycleCount = 0;

		function pushRow(
			agent: AgentRenderRecord,
			depth: number,
			parentId: string | null,
			path: string[],
			inheritedDisabledFrom: string | null,
			forcedNote?: string
		): void {
			const isCycle = path.includes(agent.agentId);
			if (isCycle) cycleCount += 1;
			const isShared = seenAgents.has(agent.agentId) && !isCycle;
			const availability = availabilityForHierarchy(agent, inheritedDisabledFrom);
			const missingTargets = missingTargetsByParent.get(agent.agentId) ?? [];
			const notes = [
				forcedNote,
				isCycle ? 'cycle detected; branch stopped' : '',
				isShared ? 'shared delegate; expanded above' : '',
				agent.delegatesTo.includes('*') ? 'wildcard delegation target' : '',
				missingTargets.length > 0 ? `missing target: ${missingTargets.join(', ')}` : ''
			].filter((value): value is string => Boolean(value));

			rows.push({
				rowId: `hierarchy-${safeRowKey(agent.agentId)}-${rows.length}`,
				depth,
				agentId: agent.agentId,
				member: `${hierarchyPrefix(depth)} ${agent.name}`,
				roleLabel: agent.roleLabel,
				parent: parentId ? formatAgentRefs([parentId], agentsById) : 'n/a',
				availabilityLabel: availability.text,
				availabilityColor: availability.color,
				statusLabel: agent.statusLabel,
				statusColor: agent.statusColor,
				delegatesTo: formatAgentRefs(agent.delegatesTo, agentsById),
				path: [...path, agent.agentId].join(' > '),
				note: notes.join('; ') || 'n/a'
			});

			if (isCycle || isShared) return;
			seenAgents.add(agent.agentId);
			const nextInheritedDisabledFrom =
				inheritedDisabledFrom ??
				(agent.configuredDisabled || agent.availabilityLabel === 'disabled' ? agent.name : null);
			for (const childId of childrenById.get(agent.agentId) ?? []) {
				const child = agentsById.get(childId);
				if (child) pushRow(child, depth + 1, agent.agentId, [...path, agent.agentId], nextInheritedDisabledFrom);
			}
		}

		for (const root of roots) {
			pushRow(root, 0, null, [], null);
		}
		for (const agent of orderedAgents) {
			if (!seenAgents.has(agent.agentId)) {
				pushRow(agent, 0, null, [], null, 'no root path; possible cycle or disconnected component');
			}
		}

		const missingTargetCount = Array.from(missingTargetsByParent.values()).reduce((total, targets) => total + targets.length, 0);
		return {
			rows,
			edgeCount,
			rootCount: roots.length,
			missingTargetCount,
			cycleCount
		};
	}

	function filterAgents(agents: AgentRenderRecord[], query: string): AgentRenderRecord[] {
		const normalized = query.trim().toLowerCase();
		if (!normalized) return agents;
		return agents.filter((agent) =>
			[
				agent.name,
				agent.agentId,
				agent.roleLabel,
				agent.description,
				agent.kind,
				agent.statusLabel,
				agent.availabilityLabel,
				agent.capabilityPacks,
				agent.delegatesTo.join(' '),
				agent.delegatedBy.join(' ')
			]
				.join(' ')
				.toLowerCase()
				.includes(normalized)
		);
	}

	function filterHierarchyRows(rows: HierarchyRow[], query: string): HierarchyRow[] {
		const normalized = query.trim().toLowerCase();
		if (!normalized) return rows;
		return rows.filter((row) =>
			[
				row.member,
				row.agentId,
				row.roleLabel,
				row.parent,
				row.availabilityLabel,
				row.statusLabel,
				row.delegatesTo,
				row.path,
				row.note
			]
				.join(' ')
				.toLowerCase()
				.includes(normalized)
		);
	}

	function readEnvelope(event: V2WebSocketEvent): AgentEventEnvelope | null {
		if (event.event_type !== 'AgentEvent') return null;
		const container = asRecord(event.data);
		if (!container) return null;
		const envelope = asRecord(container.event);
		if (!envelope) return null;

		const eventType = envelope.event_type;
		const agentId = envelope.agent_id;
		const timestamp = envelope.timestamp;
		if (typeof eventType !== 'string' || typeof agentId !== 'string') return null;

		return {
			event_type: eventType,
			agent_id: agentId,
			payload: envelope.payload ?? {},
			timestamp: typeof timestamp === 'number' && Number.isFinite(timestamp) ? timestamp : Date.now()
		};
	}

	function normalizeOutcome(outcome: string | undefined): string {
		return (outcome || '').trim().toLowerCase().replace(/[\s-]/g, '_');
	}

	function isFailureOutcome(outcome: string | undefined): boolean {
		return FAILURE_OUTCOMES.has(normalizeOutcome(outcome));
	}

	function failureCycleKey(
		agentId: string,
		cycleId: string | undefined,
		goalId: string | undefined,
		triggerSeq: number | undefined,
		timestamp: number,
		source: string
	): string {
		if (cycleId && cycleId.trim().length > 0) {
			return `${agentId}::${cycleId.trim()}`;
		}
		const safeGoalId = goalId && goalId.trim().length > 0 ? goalId.trim() : '__unknown_goal__';
		const safeSeq = typeof triggerSeq === 'number' ? String(triggerSeq) : '__na__';
		return `${agentId}::${safeGoalId}::${safeSeq}::${timestamp}::${source}`;
	}

	function failureFromEvent(event: V2WebSocketEvent): { agentId: string; cycleKey: string } | null {
		if (event.event_type === 'AgentCycleCompleted') {
			const payload = asRecord(event.data);
			if (!payload) return null;

			const agentId = readString(payload, 'agent_id');
			const outcome = readString(payload, 'outcome');
			if (!agentId || !isFailureOutcome(outcome)) return null;

			return {
				agentId,
				cycleKey: failureCycleKey(
					agentId,
					readString(payload, 'cycle_id'),
					readString(payload, 'goal_id'),
					readNumber(payload, 'trigger_seq'),
					readNumber(payload, 'timestamp') ?? Date.now(),
					'typed'
				)
			};
		}

		const envelope = readEnvelope(event);
		if (!envelope) return null;
		if (envelope.event_type !== 'agent.cycle.failed' && envelope.event_type !== 'agent.cycle.completed') return null;

		const payload = asRecord(envelope.payload) || {};
		const outcome = readString(payload, 'outcome');
		if (envelope.event_type === 'agent.cycle.completed' && !isFailureOutcome(outcome)) return null;

		return {
			agentId: envelope.agent_id,
			cycleKey: failureCycleKey(
				envelope.agent_id,
				readString(payload, 'cycle_id'),
				readString(payload, 'goal_id'),
				readNumber(payload, 'trigger_seq'),
				envelope.timestamp,
				envelope.event_type
			)
		};
	}

	function pruneFailureCycleIndex(now: number): void {
		const minTimestamp = now - FAILURE_DEDUP_TTL_MS;
		while (failureCycleIndex.length > 0) {
			const head = failureCycleIndex[0];
			const overCapacity = countedFailureCycles.size > FAILURE_DEDUP_MAX_ENTRIES;
			const expired = head.timestamp < minTimestamp;
			if (!overCapacity && !expired) break;

			failureCycleIndex.shift();
			if (countedFailureCycles.get(head.key) === head.timestamp) {
				countedFailureCycles.delete(head.key);
			}
		}
	}

	function shouldCountFailure(cycleKey: string, timestamp: number): boolean {
		if (countedFailureCycles.has(cycleKey)) return false;

		countedFailureCycles.set(cycleKey, timestamp);
		failureCycleIndex.push({ key: cycleKey, timestamp });
		pruneFailureCycleIndex(timestamp);
		return true;
	}

	function mergeFailureCounts(events: V2WebSocketEvent[]): void {
		if (events.length === 0) return;

		let changed = false;
		const nextCounts: FailureCounts = { ...failureCounts };
		for (const event of events) {
			const match = failureFromEvent(event);
			if (!match) continue;
			const eventTimestamp =
				event.event_type === 'AgentEvent'
					? (readEnvelope(event)?.timestamp ?? Date.now())
					: (readNumber(asRecord(event.data) || {}, 'timestamp') ?? Date.now());
			if (!shouldCountFailure(match.cycleKey, eventTimestamp)) continue;
			nextCounts[match.agentId] = (nextCounts[match.agentId] || 0) + 1;
			changed = true;
		}

		if (changed) failureCounts = nextCounts;
	}

	function resetFailureTracking(): void {
		failureCounts = {};
		countedFailureCycles.clear();
		failureCycleIndex.length = 0;
	}

	function totalFailureCount(agentId: string): number {
		const counted = failureCounts[agentId] || 0;
		if (counted > 0) return counted;
		const agent = $agentList.find((entry) => entry.agent_id === agentId);
		return isFailureOutcome(agent?.last_outcome) ? 1 : 0;
	}

	function findAgent(agentId: string): AgentSummary | undefined {
		return $agentList.find((entry) => entry.agent_id === agentId);
	}

	function canTrigger(record: AgentRenderRecord): boolean {
		const agent = findAgent(record.agentId);
		if (!agent) return false;
		return !(
			isAgentMutating(record.agentId) ||
			agent.status === 'running' ||
			agent.status === 'triggered' ||
			agent.status === 'paused' ||
			agent.status === 'disabled'
		);
	}

	function canPause(record: AgentRenderRecord): boolean {
		const agent = findAgent(record.agentId);
		if (!agent) return false;
		return !isAgentMutating(record.agentId) && (agent.status === 'running' || agent.status === 'triggered');
	}

	function canResume(record: AgentRenderRecord): boolean {
		const agent = findAgent(record.agentId);
		if (!agent) return false;
		return !isAgentMutating(record.agentId) && agent.status === 'paused';
	}

	function markLastUpdatedAt(): void {
		lastUpdatedAt = Date.now();
	}

	async function refreshLeaderboardHealth(): Promise<void> {
		const next = healthByAgent(await fetchCrewHealthCached());
		if (next) leaderboardHealthMap = next;
	}

	async function refreshLeaderboardFleetState(): Promise<void> {
		const requestScopeKey = currentCrewScopeKey;
		const next = await fetchFleetState();
		if (next && requestScopeKey === currentCrewScopeKey) leaderboardFleetState = next;
	}

	function resetCrewPageState(): void {
		listRefreshToken += 1;
		harnessRuntimeRequestToken += 1;
		interactionBusy = false;
		pageError = null;
		lastUpdatedAt = null;
		leaderboardHealthMap = null;
		leaderboardFleetState = null;
		harnessRuntime = null;
		harnessRuntimeLoading = true;
		harnessRuntimeMutating = false;
		harnessRuntimeError = null;
		expandedCustomRows = new Set();
		expandedHierarchyRows = new Set();
		expandedSystemRows = new Set();
		resetFailureTracking();
	}

	async function refreshHarnessRuntimeStatus(): Promise<void> {
		const token = ++harnessRuntimeRequestToken;
		harnessRuntimeLoading = true;
		harnessRuntimeError = null;
		try {
			const status = await loadHarnessRuntimeStatus();
			if (token !== harnessRuntimeRequestToken) return;
			harnessRuntime = status;
		} catch (error) {
			if (token !== harnessRuntimeRequestToken) return;
			harnessRuntimeError = error instanceof Error ? error.message : 'Failed to load harness runtime';
		} finally {
			if (token === harnessRuntimeRequestToken) harnessRuntimeLoading = false;
		}
	}

	async function toggleHarnessRuntime(): Promise<void> {
		if (!harnessRuntime || harnessRuntimeLoading || harnessRuntimeMutating) return;
		const nextEnabled = !harnessRuntime.enabled;
		harnessRuntimeMutating = true;
		harnessRuntimeError = null;
		let updated = false;
		try {
			const result = await setHarnessRuntimeEnabled(nextEnabled);
			harnessRuntime = result;
			updated = true;
			const warnings = result.warnings ?? [];
			if (warnings.length > 0) {
				const message = `Harness state saved, but cleanup was incomplete: ${warnings.join('; ')}`;
				harnessRuntimeError = message;
				showError(message);
			} else {
				showSuccess(`Harness agents turned ${result.enabled ? 'on' : 'off'}`);
			}
		} catch (error) {
			const message = error instanceof Error ? error.message : 'Failed to update harness runtime';
			harnessRuntimeError = message;
			showError(message);
		} finally {
			harnessRuntimeMutating = false;
		}
		if (updated) {
			try {
				await refreshAgents();
				markLastUpdatedAt();
			} catch {
				// Retried independently
			}
		}
	}

	async function refreshList(touchBusy = false): Promise<void> {
		const token = ++listRefreshToken;
		const requestScopeKey = currentCrewScopeKey;
		const priorBusy = interactionBusy;
		if (touchBusy && !interactionBusy) interactionBusy = true;
		pageError = null;
		try {
			await refreshAgents();
			if (token === listRefreshToken && requestScopeKey === currentCrewScopeKey) markLastUpdatedAt();
		} catch (error) {
			if (token === listRefreshToken && requestScopeKey === currentCrewScopeKey) {
				const message = error instanceof Error ? error.message : 'Failed to load crew';
				pageError = message;
				showError(message);
			}
		} finally {
			if (touchBusy && priorBusy === false && token === listRefreshToken && requestScopeKey === currentCrewScopeKey) {
				interactionBusy = false;
			}
		}
	}

	async function reloadDefinitionsFromDisk(): Promise<void> {
		if (reloadingDefinitions) return;
		reloadingDefinitions = true;
		pageError = null;
		try {
			const response = await timedFetch('/api/magician/v2/agents/refresh-definitions', {
				method: 'POST'
			});
			if (!response.ok) {
				throw new Error(`Refresh failed (${response.status})`);
			}
			const payload = (await response.json().catch(() => ({}))) as { definitions_reloaded?: number };
			const count = typeof payload.definitions_reloaded === 'number' ? payload.definitions_reloaded : null;
			await refreshList(false);
			showSuccess(count === null ? 'Reloaded agent definitions from disk' : `Reloaded ${count} agent definitions from disk`);
		} catch (error) {
			const message = error instanceof Error ? error.message : 'Failed to reload agent definitions';
			pageError = message;
			showError(message);
		} finally {
			reloadingDefinitions = false;
		}
	}

	async function onTrigger(record: AgentRenderRecord): Promise<void> {
		const agent = findAgent(record.agentId);
		if (!agent) return;
		pageError = null;
		try {
			await triggerAgent(agent.agent_id, { trigger: 'manual.crew' });
			showSuccess(`Triggered ${agent.name || agent.agent_id}`);
		} catch (error) {
			const message = error instanceof Error ? error.message : `Failed to trigger ${agent.agent_id}`;
			pageError = message;
			showError(message);
		}
	}

	async function onPause(record: AgentRenderRecord): Promise<void> {
		const agent = findAgent(record.agentId);
		if (!agent) return;
		pageError = null;
		try {
			await pauseAgent(agent.agent_id);
			showSuccess(`Paused ${agent.name || agent.agent_id}`);
		} catch (error) {
			const message = error instanceof Error ? error.message : `Failed to pause ${agent.agent_id}`;
			pageError = message;
			showError(message);
		}
	}

	async function onResume(record: AgentRenderRecord): Promise<void> {
		const agent = findAgent(record.agentId);
		if (!agent) return;
		pageError = null;
		try {
			await resumeAgent(agent.agent_id);
			showSuccess(`Resumed ${agent.name || agent.agent_id}`);
		} catch (error) {
			const message = error instanceof Error ? error.message : `Failed to resume ${agent.agent_id}`;
			pageError = message;
			showError(message);
		}
	}

	function openSummonFlow(): void {
		void goto('/crew/new');
	}

	function toggleSet(set: Set<string>, value: string): Set<string> {
		const next = new Set(set);
		if (next.has(value)) next.delete(value);
		else next.add(value);
		return next;
	}

	function toggleCustomRow(agentId: string): void {
		expandedCustomRows = toggleSet(expandedCustomRows, agentId);
	}

	function toggleHierarchyRow(rowId: string): void {
		expandedHierarchyRows = toggleSet(expandedHierarchyRows, rowId);
	}

	function toggleSystemRow(agentId: string): void {
		expandedSystemRows = toggleSet(expandedSystemRows, agentId);
	}

	onMount(() => {
		if (!browser) return;

		crewMounted = true;
		const currentScope = get(scopeIdentityStore);
		lastCrewScopeKey = `${currentScope.principal}:${currentScope.workspace}`;
		const unsubscribeEvents = v2Events.subscribe((events) => {
			mergeFailureCounts(events);
		});
		const unsubscribeConnection = v2Events.connectionStatus.subscribe((next) => {
			connectionStatus = next;
		});

		try {
			const savedMode = localStorage.getItem('magician:crew-view-mode');
			if (savedMode === 'grid' || savedMode === 'table') viewMode = savedMode;
		} catch {
			// storage error ignored
		}

		void refreshList(true);
		void refreshHarnessRuntimeStatus();
		void refreshLeaderboardHealth();
		leaderboardHealthTimer = setInterval(() => void refreshLeaderboardHealth(), 120_000);
		void refreshLeaderboardFleetState();
		leaderboardFleetStateTimer = setInterval(() => void refreshLeaderboardFleetState(), 30_000);
		v2Events.connectGlobal();

		return () => {
			crewMounted = false;
			if (leaderboardHealthTimer) clearInterval(leaderboardHealthTimer);
			leaderboardHealthTimer = null;
			if (leaderboardFleetStateTimer) clearInterval(leaderboardFleetStateTimer);
			leaderboardFleetStateTimer = null;
			unsubscribeEvents();
			unsubscribeConnection();
		};
	});

	$: if (browser && crewMounted && currentCrewScopeKey !== lastCrewScopeKey) {
		lastCrewScopeKey = currentCrewScopeKey;
		resetCrewPageState();
		void refreshList(true);
		void refreshHarnessRuntimeStatus();
		void refreshLeaderboardHealth();
		void refreshLeaderboardFleetState();
	}
</script>

<svelte:head>
	<title>Crew & Fleet — Operations</title>
</svelte:head>

<div class="crew-page">
	<!-- Top Command Header -->
	<header class="crew-masthead">
		<div class="crew-masthead__identity">
			<div class="crew-masthead__badge-row">
				<span class="crew-badge crew-badge-outline">Fleet Operations</span>
				<span class="crew-masthead__pulse">
					<span class="pulse-dot pulse-dot--{connectionStatus}"></span>
					{connectionStatus}
				</span>
				{#if lastUpdatedAt}
					<span class="crew-masthead__updated">Updated {formatRelativeTime(lastUpdatedAt)}</span>
				{/if}
			</div>
			<h1>Crew Operations</h1>
			<p>Govern autonomous agent execution, inspect organizational delegation, and review fleet performance.</p>
		</div>

		<div class="crew-masthead__actions">
			<button
				class="crew-button crew-button-secondary"
				type="button"
				title="Refresh fleet roster and harness runtime status"
				disabled={interactionBusy || $agentStoreState.isLoading}
				on:click={() => { void refreshList(true); void refreshHarnessRuntimeStatus(); }}
			>
				<Icon name="rotate-ccw" size={14} />
				<span>{interactionBusy || $agentStoreState.isLoading ? 'Refreshing...' : 'Refresh'}</span>
			</button>
			<button
				class="crew-button crew-button-secondary"
				type="button"
				title="Clear backend agent-definition cache and reload definitions from disk"
				disabled={reloadingDefinitions || interactionBusy}
				on:click={() => void reloadDefinitionsFromDisk()}
			>
				<Icon name="archive" size={14} />
				<span>{reloadingDefinitions ? 'Reloading...' : 'Reload from disk'}</span>
			</button>
			<button
				class="crew-button crew-button-primary"
				type="button"
				disabled={interactionBusy}
				on:click={openSummonFlow}
			>
				<Icon name="plus" size={15} />
				<span>Create Crew Member</span>
			</button>
		</div>
	</header>

	<!-- Autonomous Company Loop Command Banner -->
	<section
		class="company-loop-banner"
		class:company-loop-banner--active={harnessEnabled}
		class:company-loop-banner--error={Boolean(harnessRuntimeError || harnessRuntime?.config_error)}
		aria-label="Autonomous Company Loop Control"
	>
		<div class="company-loop-banner__main">
			<div class="company-loop-banner__icon-box" aria-hidden="true">
				<Icon name="sparkle" size={20} />
			</div>
			<div class="company-loop-banner__info">
				<div class="company-loop-banner__title-row">
					<div class="company-loop-banner__headline">
						<h2>Company Loop</h2>
						<span class={badgeClass(harnessStatusColor)}>{harnessStatusLabel}</span>
						{#if harnessEnabled}
							<span class="company-loop-pulse">
								<span class="pulse-dot pulse-dot--connected"></span>
								Autonomous Cadence Active
							</span>
						{/if}
					</div>
					<div class="company-loop-banner__meta-tags">
						<span class="meta-tag">
							Source: {harnessRuntime?.effective_source ?? 'config'}
						</span>
						{#if harnessRuntime?.effective_source === 'environment'}
							<span class="meta-tag meta-tag--env">Environment Override</span>
						{/if}
					</div>
				</div>
				<p class="company-loop-banner__desc">
					Autonomous company cadence engine — orchestrates scheduled routines, steward agent cycles, and the executive sequence (CMO → CRO → CPO → CTO → CEO).
				</p>
				{#if harnessRuntimeError || harnessRuntime?.config_error}
					<div class="company-loop-banner__error-banner" role="alert">
						<Icon name="alert" size={14} />
						<span>{harnessRuntimeError || harnessRuntime?.config_error}</span>
					</div>
				{/if}
			</div>
		</div>

		<div class="company-loop-banner__controls">
			<div class="company-loop-toggle-group">
				<span class="company-loop-toggle-label">
					{#if harnessRuntimeLoading || harnessRuntimeMutating}
						Updating...
					{:else if harnessEnabled}
						Cadence On
					{:else}
						Cadence Off
					{/if}
				</span>
				<button
					class="crew-switch"
					class:crew-switch-enabled={harnessEnabled}
					type="button"
					role="switch"
					aria-checked={harnessEnabled}
					aria-label={harnessEnabled ? 'Turn off autonomous company loop' : 'Turn on autonomous company loop'}
					title={harnessEnabled ? 'Turn off autonomous company loop' : 'Turn on autonomous company loop'}
					disabled={!harnessRuntime || harnessRuntimeLoading || harnessRuntimeMutating}
					on:click={toggleHarnessRuntime}
				>
					<span aria-hidden="true"></span>
				</button>
			</div>

			<a href="/harness" class="crew-button crew-button-secondary crew-button-sm">
				<Icon name="settings" size={14} />
				<span>Harness Console →</span>
			</a>
		</div>
	</section>

	<!-- KPI Command Ribbon -->
	<section class="crew-kpi-ribbon" aria-label="Fleet KPIs">
		<button
			type="button"
			class="crew-kpi-card"
			class:crew-kpi-card--active={activeTab === 'members' && statusFilter === 'all'}
			on:click={() => setStatusFilter('all')}
		>
			<span class="kpi-label">Total Fleet</span>
			<div class="kpi-value-row">
				<strong>{totalAgents}</strong>
				<span class="kpi-tag">{customRecords.length} custom</span>
			</div>
			<span class="kpi-subtext">{systemRecords.length} internal pipeline</span>
		</button>

		<button
			type="button"
			class="crew-kpi-card"
			class:crew-kpi-card--active={activeTab === 'members' && statusFilter === 'active'}
			on:click={() => setStatusFilter('active')}
		>
			<span class="kpi-label">Active Now</span>
			<div class="kpi-value-row">
				<strong class="text-emerald">{activeNow}</strong>
				{#if activeNow > 0}
					<span class="kpi-pulsing-badge">Running</span>
				{/if}
			</div>
			<span class="kpi-subtext">{activeNow > 0 ? 'Live execution in flight' : 'No active runs'}</span>
		</button>

		<button
			type="button"
			class="crew-kpi-card"
			class:crew-kpi-card--active={activeTab === 'members' && statusFilter === 'attention'}
			on:click={() => setStatusFilter('attention')}
		>
			<span class="kpi-label">Needs Attention</span>
			<div class="kpi-value-row">
				<strong class:text-amber={attentionCount > 0}>{attentionCount}</strong>
				{#if attentionCount > 0}
					<span class="kpi-warn-badge">Action required</span>
				{/if}
			</div>
			<span class="kpi-subtext">{knownFailures} counted failure{knownFailures === 1 ? '' : 's'}</span>
		</button>

		<button
			type="button"
			class="crew-kpi-card"
			class:crew-kpi-card--active={activeTab === 'members' && statusFilter === 'attention'}
			on:click={() => setStatusFilter('attention')}
		>
			<span class="kpi-label">Pending Approvals</span>
			<div class="kpi-value-row">
				<strong class:text-blue={$agentsWithPendingApprovals.length > 0}>{$agentsWithPendingApprovals.length}</strong>
				{#if $agentsWithPendingApprovals.length > 0}
					<span class="kpi-blue-badge">Awaiting decision</span>
				{/if}
			</div>
			<span class="kpi-subtext">{$agentsWithPendingApprovals.length > 0 ? 'Requires human review' : 'All clear'}</span>
		</button>
	</section>

	{#if routeError}
		<div class="crew-alert" role="alert">
			<Icon name="alert" size={16} />
			<span>{routeError}</span>
		</div>
	{/if}

	<!-- View Navigation Tabs -->
	<nav class="crew-tabs" aria-label="Crew Views">
		<button
			type="button"
			class="crew-tab"
			class:crew-tab--active={activeTab === 'members'}
			on:click={() => setTab('members')}
		>
			<Icon name="sparkle" size={15} />
			<span>Crew Members</span>
			<span class="tab-count">{customRecords.length}</span>
		</button>

		<button
			type="button"
			class="crew-tab"
			class:crew-tab--active={activeTab === 'leaderboard'}
			on:click={() => setTab('leaderboard')}
		>
			<Icon name="flag" size={15} />
			<span>Leaderboard & 7d</span>
			{#if leaderboardRows.length > 0}
				<span class="tab-count">{leaderboardRows.length}</span>
			{/if}
		</button>

		<button
			type="button"
			class="crew-tab"
			class:crew-tab--active={activeTab === 'hierarchy'}
			on:click={() => setTab('hierarchy')}
		>
			<Icon name="git-branch" size={15} />
			<span>Delegation Hierarchy</span>
			<span class="tab-count">{delegationGraph.edgeCount} edges</span>
		</button>

		<button
			type="button"
			class="crew-tab"
			class:crew-tab--active={activeTab === 'pipeline'}
			on:click={() => setTab('pipeline')}
		>
			<Icon name="settings" size={15} />
			<span>Pipeline Roles</span>
			<span class="tab-count">{systemRecords.length}</span>
		</button>
	</nav>

	<!-- TAB 1: CREW MEMBERS (PRIMARY ROSTER) -->
	{#if activeTab === 'members'}
		<section class="crew-roster-view" aria-label="Crew members roster">
			<!-- Toolbar: Search + Status Filter Chips + Layout Toggle -->
			<div class="crew-toolbar">
				<div class="crew-search-box">
					<Icon name="search" size={15} />
					<input
						type="search"
						bind:value={customFilter}
						placeholder="Search by name, role, ID, capability pack..."
						aria-label="Search crew members"
					/>
					{#if customFilter}
						<button class="search-clear" type="button" on:click={() => (customFilter = '')}>×</button>
					{/if}
				</div>

				<div class="crew-filters" role="group" aria-label="Filter agents by status">
					<button
						type="button"
						class="filter-chip"
						class:filter-chip--active={statusFilter === 'all'}
						on:click={() => setStatusFilter('all')}
					>
						All ({customRecords.length})
					</button>
					<button
						type="button"
						class="filter-chip"
						class:filter-chip--active={statusFilter === 'active'}
						on:click={() => setStatusFilter('active')}
					>
						<span class="filter-dot filter-dot--active"></span>
						Active ({activeNow})
					</button>
					<button
						type="button"
						class="filter-chip"
						class:filter-chip--active={statusFilter === 'attention'}
						on:click={() => setStatusFilter('attention')}
					>
						<span class="filter-dot filter-dot--attention"></span>
						Needs Attention ({attentionCount})
					</button>
					<button
						type="button"
						class="filter-chip"
						class:filter-chip--active={statusFilter === 'paused'}
						on:click={() => setStatusFilter('paused')}
					>
						Paused ({pausedCount})
					</button>
					<button
						type="button"
						class="filter-chip"
						class:filter-chip--active={statusFilter === 'disabled'}
						on:click={() => setStatusFilter('disabled')}
					>
						Disabled ({disabledCount})
					</button>
				</div>

				<div class="crew-view-switcher" role="group" aria-label="View format">
					<button
						type="button"
						class="view-toggle-btn"
						class:view-toggle-btn--active={viewMode === 'table'}
						title="Table list view"
						on:click={() => setViewMode('table')}
					>
						<Icon name="file-text" size={15} />
					</button>
					<button
						type="button"
						class="view-toggle-btn"
						class:view-toggle-btn--active={viewMode === 'grid'}
						title="Card grid view"
						on:click={() => setViewMode('grid')}
					>
						<Icon name="square" size={15} />
					</button>
				</div>
			</div>

			{#if displayedCustomRecords.length === 0}
				<div class="crew-empty">
					<Icon name="sparkle" size={32} />
					<h3>{customRecords.length === 0 ? 'No custom crew members yet' : 'No matching crew members'}</h3>
					<p>
						{customRecords.length === 0
							? 'Summon your first agent to begin autonomous task execution and delegation.'
							: 'Try clearing the search query or changing the status filter.'}
					</p>
					{#if customRecords.length === 0}
						<button class="crew-button crew-button-primary" type="button" on:click={openSummonFlow}>
							Create a crew member
						</button>
					{:else}
						<button class="crew-button crew-button-secondary" type="button" on:click={() => { customFilter = ''; statusFilter = 'all'; }}>
							Reset filters
						</button>
					{/if}
				</div>

			{:else if viewMode === 'table'}
				<!-- DENSE TABLE VIEW -->
				<div class="crew-table-container">
					<table class="crew-table">
						<colgroup>
							<col class="col-member" />
							<col class="col-status" />
							<col class="col-health" />
							<col class="col-telemetry" />
							<col class="col-cycle" />
							<col class="col-approvals" />
							<col class="col-actions" />
						</colgroup>
						<thead>
							<tr>
								<th>Crew member</th>
								<th>Status</th>
								<th>Health (7d)</th>
								<th>Spend / Calls</th>
								<th>Cycle</th>
								<th>Approvals / Alert</th>
								<th>Actions</th>
							</tr>
						</thead>
						<tbody>
							{#each pagedCustomRecords as agent (agent.agentId)}
								{@const rowStats = leaderboardRowById.get(agent.agentId)}
								<tr>
									<td data-label="Crew member">
										<div class="crew-member-cell">
											<button
												class="crew-expand"
												type="button"
												aria-label={`${expandedCustomRows.has(agent.agentId) ? 'Collapse' : 'Expand'} ${agent.name}`}
												aria-expanded={expandedCustomRows.has(agent.agentId)}
												on:click={() => toggleCustomRow(agent.agentId)}
											>
												<Icon name={expandedCustomRows.has(agent.agentId) ? 'chevron-up' : 'chevron-down'} size={14} />
											</button>
											<div class="crew-member-info">
												<div class="crew-member-title">
													<a href={`/crew/${encodeURIComponent(agent.agentId)}`} class="agent-title-link">
														{agent.name}
													</a>
													{#if rowStats?.isPrimary}
														<span class="agent-pill-primary">Primary</span>
													{:else if rowStats?.isEnvoy}
														<span class="agent-pill-envoy">Envoy</span>
													{/if}
													<span class="role-caption">{agent.roleLabel}</span>
												</div>
												<div class="crew-member-sub">
													<span class="crew-mono">{agent.agentId}</span>
													<span class="dot-sep">·</span>
													<span class="kind-tag">{agent.kind}</span>
													<span class="dot-sep">·</span>
													<a href={`/crew/${encodeURIComponent(agent.agentId)}`} class="sub-link">Open</a>
													<span class="dot-sep">·</span>
													<a href={`/crew/new?edit=${encodeURIComponent(agent.agentId)}`} class="sub-link">Edit</a>
												</div>
											</div>
										</div>
									</td>

									<td data-label="Status">
										<div class="status-cell">
											<span class={badgeClass(agent.statusColor)}>
												<span class="status-dot status-dot--{agent.statusColor}"></span>
												{agent.statusLabel}
											</span>
											{#if agent.availabilityLabel === 'disabled'}
												<span class="status-sub-disabled">disabled</span>
											{/if}
										</div>
									</td>

									<td data-label="Health (7d)">
										<div class="health-cell">
											{#if rowStats?.health != null}
												<strong class="health-score" data-band={healthBand(rowStats.health)}>{rowStats.health}</strong>
												{#if rowStats.healthDelta7d != null && Math.abs(rowStats.healthDelta7d) > 0.05}
													<small class="health-delta" data-delta={rowStats.healthDelta7d > 0 ? 'up' : 'down'}>
														{rowStats.healthDelta7d > 0 ? '+' : ''}{rowStats.healthDelta7d.toFixed(1)}
													</small>
												{/if}
											{:else}
												<span class="text-muted">-</span>
											{/if}
										</div>
									</td>

									<td data-label="Spend / Calls">
										<div class="telemetry-cell">
											<span class="spend-text">{rowStats?.spendUsd7d != null ? `$${rowStats.spendUsd7d.toFixed(2)}` : '$0.00'}</span>
											<span class="calls-sub">{rowStats?.calls7d != null ? `${rowStats.calls7d} calls` : '0 calls'}</span>
										</div>
									</td>

									<td data-label="Cycle">
										<span class="cycle-cell crew-mono" title={agent.currentCycle}>
											{agent.currentCycle === 'none' ? '-' : agent.currentCycle}
										</span>
									</td>

									<td data-label="Approvals / Alert">
										<div class="approvals-cell">
											{#if Number(agent.pendingApprovals) > 0}
												<span class="alert-pill alert-pill--warn">
													{agent.pendingApprovals} pending
												</span>
											{/if}
											{#if totalFailureCount(agent.agentId) > 0}
												<span class="alert-pill alert-pill--error">
													{totalFailureCount(agent.agentId)} failed
												</span>
											{/if}
											{#if Number(agent.pendingApprovals) === 0 && totalFailureCount(agent.agentId) === 0}
												<span class="text-muted">Clean</span>
											{/if}
										</div>
									</td>

									<td data-label="Actions">
										<div class="crew-action-row">
											<button
												class="crew-button crew-button-primary crew-button-sm"
												type="button"
												disabled={!canTrigger(agent)}
												on:click={() => onTrigger(agent)}
											>
												Trigger
											</button>
											<button
												class="crew-button crew-button-outline crew-button-sm"
												type="button"
												disabled={!canPause(agent)}
												on:click={() => onPause(agent)}
											>
												Pause
											</button>
											<button
												class="crew-button crew-button-outline crew-button-sm"
												type="button"
												disabled={!canResume(agent)}
												on:click={() => onResume(agent)}
											>
												Resume
											</button>
										</div>
									</td>
								</tr>

								{#if expandedCustomRows.has(agent.agentId)}
									<tr class="crew-details-row">
										<td colspan="7">
											<div class="crew-details-expanded">
												<dl class="crew-details-grid">
													<div><dt>Purpose</dt><dd>{agent.description}</dd></div>
													<div><dt>Delegated by</dt><dd>{formatAgentRefs(agent.delegatedBy, customRecordsById)}</dd></div>
													<div><dt>Delegates to</dt><dd>{formatAgentRefs(agent.delegatesTo, customRecordsById)}</dd></div>
													<div><dt>Capability Packs</dt><dd>{agent.capabilityPacks}</dd></div>
													<div><dt>Last active</dt><dd>{agent.lastActive}</dd></div>
												</dl>
												<div class="crew-effective-tools-slot">
													<EffectiveToolPolicyPanel agentId={agent.agentId} compact={true} />
												</div>
											</div>
										</td>
									</tr>
								{/if}
							{/each}
						</tbody>
					</table>
				</div>

			{:else}
				<!-- CARD / GRID VIEW -->
				<div class="crew-cards-grid">
					{#each pagedCustomRecords as agent (agent.agentId)}
						{@const rowStats = leaderboardRowById.get(agent.agentId)}
						<article class="agent-card">
							<header class="agent-card__header">
								<div class="agent-card__avatar">
									{agent.name.slice(0, 2).toUpperCase()}
									<span class="card-status-dot status-dot--{agent.statusColor}"></span>
								</div>
								<div class="agent-card__info">
									<div class="agent-card__title-row">
										<a href={`/crew/${encodeURIComponent(agent.agentId)}`} class="card-agent-name">
											{agent.name}
										</a>
										{#if rowStats?.isPrimary}
											<span class="agent-pill-primary">Primary</span>
										{:else if rowStats?.isEnvoy}
											<span class="agent-pill-envoy">Envoy</span>
										{/if}
									</div>
									<p class="card-role">{agent.roleLabel}</p>
									<p class="card-id crew-mono">{agent.agentId}</p>
								</div>
								<span class={badgeClass(agent.statusColor)}>{agent.statusLabel}</span>
							</header>

							<p class="agent-card__desc">{agent.description}</p>

							<!-- Telemetry strip -->
							<div class="agent-card__stats">
								<div class="card-stat">
									<span class="card-stat-label">Health</span>
									{#if rowStats?.health != null}
										<strong class="health-score" data-band={healthBand(rowStats.health)}>{rowStats.health}</strong>
									{:else}
										<span class="text-muted">-</span>
									{/if}
								</div>
								<div class="card-stat">
									<span class="card-stat-label">7d Spend</span>
									<strong>{rowStats?.spendUsd7d != null ? `$${rowStats.spendUsd7d.toFixed(2)}` : '$0.00'}</strong>
								</div>
								<div class="card-stat">
									<span class="card-stat-label">7d Calls</span>
									<strong>{rowStats?.calls7d != null ? rowStats.calls7d : '0'}</strong>
								</div>
								<div class="card-stat">
									<span class="card-stat-label">Cycle</span>
									<span class="crew-mono card-cycle-text">{agent.currentCycle === 'none' ? '-' : agent.currentCycle}</span>
								</div>
							</div>

							<!-- Alerts & Delegations row -->
							<div class="agent-card__meta">
								{#if Number(agent.pendingApprovals) > 0}
									<span class="alert-pill alert-pill--warn">
										{agent.pendingApprovals} pending approval
									</span>
								{/if}
								{#if totalFailureCount(agent.agentId) > 0}
									<span class="alert-pill alert-pill--error">
										{totalFailureCount(agent.agentId)} failed
									</span>
								{/if}
								{#if agent.delegatesTo.length > 0}
									<span class="delegates-tag">
										Delegates to: {agent.delegatesTo.join(', ')}
									</span>
								{/if}
							</div>

							<!-- Action footer -->
							<footer class="agent-card__footer">
								<div class="card-actions-left">
									<button
										class="crew-button crew-button-primary crew-button-sm"
										type="button"
										disabled={!canTrigger(agent)}
										on:click={() => onTrigger(agent)}
									>
										Trigger
									</button>
									<button
										class="crew-button crew-button-outline crew-button-sm"
										type="button"
										disabled={!canPause(agent)}
										on:click={() => onPause(agent)}
									>
										Pause
									</button>
									<button
										class="crew-button crew-button-outline crew-button-sm"
										type="button"
										disabled={!canResume(agent)}
										on:click={() => onResume(agent)}
									>
										Resume
									</button>
								</div>
								<div class="card-actions-right">
									<button
										class="icon-btn-toggle"
										type="button"
										title="Inspect tool policy"
										aria-expanded={expandedCustomRows.has(agent.agentId)}
										on:click={() => toggleCustomRow(agent.agentId)}
									>
										<Icon name={expandedCustomRows.has(agent.agentId) ? 'chevron-up' : 'chevron-down'} size={14} />
									</button>
									<a href={`/crew/${encodeURIComponent(agent.agentId)}`} class="crew-button crew-button-secondary crew-button-sm">
										Open →
									</a>
								</div>
							</footer>

							{#if expandedCustomRows.has(agent.agentId)}
								<div class="card-expanded-drawer">
									<div class="crew-effective-tools-slot">
										<EffectiveToolPolicyPanel agentId={agent.agentId} compact={true} />
									</div>
								</div>
							{/if}
						</article>
					{/each}
				</div>
			{/if}

			{#if displayedCustomRecords.length > 0}
				<div class="crew-pagination-bar">
					<div class="crew-pagination-bar__per-page">
						<label for="crew-page-size-select">Per page:</label>
						<select
							id="crew-page-size-select"
							bind:value={pageSize}
							aria-label="Items per page"
						>
							{#each PAGE_SIZE_OPTIONS as size}
								<option value={size}>{size}</option>
							{/each}
						</select>
					</div>
					<ServerPager
						{currentPage}
						{pageCount}
						{startItem}
						{endItem}
						totalItems={totalCustomItems}
						ariaLabel="Crew members pagination"
						on:pagechange={(event) => (currentPage = event.detail.page)}
					/>
				</div>
			{/if}
		</section>

	<!-- TAB 2: LEADERBOARD & TELEMETRY -->
	{:else if activeTab === 'leaderboard'}
		<section class="crew-tab-content" aria-label="Crew Leaderboard">
			<div class="tab-content-header">
				<div>
					<h2>Performance Leaderboard</h2>
					<p>Fleet members ranked by activity, call volume, cost, and overall health over the last 7 days.</p>
				</div>
			</div>
			<CrewLeaderboard
				rows={leaderboardRows}
				on:select={(event) => void goto(`/crew/${encodeURIComponent(event.detail)}`)}
			/>
		</section>

	<!-- TAB 3: DELEGATION HIERARCHY -->
	{:else if activeTab === 'hierarchy'}
		<section class="crew-tab-content" aria-label="Delegation hierarchy">
			<div class="tab-content-header">
				<div>
					<h2>Delegation Hierarchy</h2>
					<p>
						{delegationGraph.rootCount} root{delegationGraph.rootCount === 1 ? '' : 's'} ·
						{delegationGraph.edgeCount} edge{delegationGraph.edgeCount === 1 ? '' : 's'} ·
						{delegationGraph.missingTargetCount} missing target{delegationGraph.missingTargetCount === 1 ? '' : 's'} ·
						{delegationGraph.cycleCount} cycle{delegationGraph.cycleCount === 1 ? '' : 's'}
					</p>
				</div>
				<div class="hierarchy-search-box">
					<Icon name="search" size={14} />
					<input
						type="search"
						bind:value={hierarchyFilter}
						placeholder="Filter hierarchy or parent..."
						aria-label="Filter delegation hierarchy"
					/>
				</div>
			</div>

			{#if filteredHierarchyRows.length === 0}
				<div class="crew-empty">
					<Icon name="git-branch" size={32} />
					<h3>No hierarchy matches</h3>
					<p>No custom agent definitions matched the current filter.</p>
				</div>
			{:else}
				<div class="crew-table-container">
					<table class="crew-table">
						<colgroup>
							<col class="col-member" />
							<col class="col-small" />
							<col class="col-small" />
							<col class="col-parent" />
						</colgroup>
						<thead>
							<tr>
								<th>Hierarchy</th>
								<th>Enabled</th>
								<th>Status</th>
								<th>Parent</th>
							</tr>
						</thead>
						<tbody>
							{#each filteredHierarchyRows as row (row.rowId)}
								<tr>
									<td data-label="Hierarchy">
										<div class="crew-member-cell">
											<button
												class="crew-expand"
												type="button"
												aria-label={`${expandedHierarchyRows.has(row.rowId) ? 'Collapse' : 'Expand'} ${row.member}`}
												aria-expanded={expandedHierarchyRows.has(row.rowId)}
												on:click={() => toggleHierarchyRow(row.rowId)}
											>
												<Icon name={expandedHierarchyRows.has(row.rowId) ? 'chevron-up' : 'chevron-down'} size={14} />
											</button>
											<div class="crew-member-info">
												<div class="crew-member-title">
													<a href={`/crew/${encodeURIComponent(row.agentId)}`} class="agent-title-link">
														{row.member}
													</a>
													<span class="role-caption">{row.roleLabel}</span>
												</div>
												<div class="crew-member-sub">
													<span class="crew-mono">{row.agentId}</span>
													<span class="dot-sep">·</span>
													<a href={`/crew/${encodeURIComponent(row.agentId)}`} class="sub-link">Open</a>
												</div>
											</div>
										</div>
									</td>
									<td data-label="Enabled"><span class={badgeClass(row.availabilityColor)}>{row.availabilityLabel}</span></td>
									<td data-label="Status"><span class={badgeClass(row.statusColor)}>{row.statusLabel}</span></td>
									<td data-label="Parent">{row.parent}</td>
								</tr>
								{#if expandedHierarchyRows.has(row.rowId)}
									<tr class="crew-details-row">
										<td colspan="4">
											<dl class="crew-details-grid">
												<div><dt>Delegates to</dt><dd>{row.delegatesTo}</dd></div>
												<div><dt>Path</dt><dd>{row.path}</dd></div>
												<div><dt>Note</dt><dd>{row.note}</dd></div>
											</dl>
										</td>
									</tr>
								{/if}
							{/each}
						</tbody>
					</table>
				</div>
			{/if}
		</section>

	<!-- TAB 4: PIPELINE ROLES (SYSTEM) -->
	{:else if activeTab === 'pipeline'}
		<section class="crew-tab-content" aria-label="Internal pipeline agents">
			<div class="tab-content-header">
				<div>
					<h2>Internal Pipeline Agents</h2>
					<p>Read-only backend pipeline roles used for intent classification, planning, slot extraction, and clarification.</p>
				</div>
				<div class="hierarchy-search-box">
					<Icon name="search" size={14} />
					<input
						type="search"
						bind:value={systemFilter}
						placeholder="Filter pipeline roles..."
						aria-label="Filter pipeline roles"
					/>
				</div>
			</div>

			{#if filteredSystemRecords.length === 0}
				<div class="crew-empty">
					<Icon name="settings" size={32} />
					<h3>No internal pipeline agents</h3>
					<p>No internal pipeline descriptors matched the filter.</p>
				</div>
			{:else}
				<div class="crew-table-container">
					<table class="crew-table">
						<colgroup>
							<col class="col-member" />
							<col class="col-small" />
							<col class="col-small" />
							<col class="col-small" />
							<col class="col-small" />
						</colgroup>
						<thead>
							<tr>
								<th>Pipeline role</th>
								<th>Scope</th>
								<th>Enabled</th>
								<th>Status</th>
								<th>Kind</th>
							</tr>
						</thead>
						<tbody>
							{#each filteredSystemRecords as agent (agent.agentId)}
								<tr>
									<td data-label="Pipeline role">
										<div class="crew-member-cell">
											<button
												class="crew-expand"
												type="button"
												aria-label={`${expandedSystemRows.has(agent.agentId) ? 'Collapse' : 'Expand'} ${agent.name}`}
												aria-expanded={expandedSystemRows.has(agent.agentId)}
												on:click={() => toggleSystemRow(agent.agentId)}
											>
												<Icon name={expandedSystemRows.has(agent.agentId) ? 'chevron-up' : 'chevron-down'} size={14} />
											</button>
											<div class="crew-member-info">
												<div class="crew-member-title">
													<span>{agent.name}</span>
													<span class="role-caption">{agent.roleLabel}</span>
												</div>
												<span class="crew-mono">{agent.agentId}</span>
											</div>
										</div>
									</td>
									<td data-label="Scope">system</td>
									<td data-label="Enabled"><span class={badgeClass(agent.availabilityColor)}>{agent.availabilityLabel}</span></td>
									<td data-label="Status"><span class={badgeClass(agent.statusColor)}>{agent.statusLabel}</span></td>
									<td data-label="Kind">{agent.kind}</td>
								</tr>
								{#if expandedSystemRows.has(agent.agentId)}
									<tr class="crew-details-row">
										<td colspan="5">
											<dl class="crew-details-grid">
												<div><dt>Purpose</dt><dd>{agent.description}</dd></div>
											</dl>
										</td>
									</tr>
								{/if}
							{/each}
						</tbody>
					</table>
				</div>
			{/if}
		</section>
	{/if}
</div>

<style>
	.crew-page {
		box-sizing: border-box;
		color: var(--text-primary);
		display: flex;
		flex-direction: column;
		gap: 1.25rem;
		margin: 0 auto;
		max-width: var(--app-content-max, 1360px);
		padding: 1.5rem 1.5rem 5rem;
		width: 100%;
	}

	/* Top Masthead */
	.crew-masthead {
		align-items: flex-start;
		background:
			radial-gradient(ellipse at top right, color-mix(in srgb, var(--accent-primary) 12%, transparent), transparent 60%),
			linear-gradient(180deg, color-mix(in srgb, var(--bg-card) 96%, var(--bg-soft) 4%), var(--bg-card));
		border: 1px solid var(--border-soft);
		border-radius: 12px;
		display: flex;
		flex-wrap: wrap;
		gap: 1.25rem;
		justify-content: space-between;
		padding: 1.25rem 1.5rem;
	}

	.crew-masthead__identity {
		max-width: 48rem;
	}

	.crew-masthead__badge-row {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.6rem;
		margin-bottom: 0.4rem;
	}

	.crew-masthead__pulse {
		align-items: center;
		color: var(--text-secondary);
		display: inline-flex;
		font-size: 0.78rem;
		font-weight: 500;
		gap: 0.35rem;
		text-transform: capitalize;
	}

	.pulse-dot {
		border-radius: 50%;
		display: inline-block;
		height: 7px;
		width: 7px;
	}

	.pulse-dot--connected {
		background: var(--accent-success, #10b981);
		box-shadow: 0 0 6px var(--accent-success, #10b981);
	}

	.pulse-dot--connecting {
		background: var(--accent-warning, #f59e0b);
		box-shadow: 0 0 6px var(--accent-warning, #f59e0b);
	}

	.pulse-dot--disconnected {
		background: var(--accent-error, #ef4444);
	}

	.crew-masthead__updated {
		color: var(--text-muted);
		font-size: 0.76rem;
	}

	.crew-masthead h1 {
		font-family: var(--font-display, var(--font-primary));
		font-size: 1.85rem;
		font-weight: 700;
		letter-spacing: -0.02em;
		line-height: 1.15;
		margin: 0.15rem 0 0.4rem;
	}

	.crew-masthead p {
		color: var(--text-secondary);
		font-size: 0.92rem;
		line-height: 1.45;
		margin: 0;
	}

	.crew-masthead__actions {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.6rem;
	}

	/* KPI Ribbon */
	.crew-kpi-ribbon {
		display: grid;
		gap: 0.75rem;
		grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
		width: 100%;
	}

	.crew-kpi-card {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 10px;
		cursor: pointer;
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		padding: 0.85rem 1rem;
		text-align: left;
		transition: border-color 0.15s, background-color 0.15s, transform 0.1s;
	}

	.crew-kpi-card:hover {
		border-color: var(--accent-primary);
		transform: translateY(-1px);
	}

	.crew-kpi-card--active {
		border-color: var(--accent-primary);
		box-shadow: 0 0 0 1px var(--accent-primary);
	}

	.kpi-label {
		color: var(--text-secondary);
		font-size: 0.74rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}

	.kpi-value-row {
		align-items: baseline;
		display: flex;
		gap: 0.5rem;
	}

	.kpi-value-row strong {
		font-size: 1.65rem;
		font-weight: 700;
		line-height: 1.1;
	}

	.kpi-tag {
		background: var(--bg-soft);
		border-radius: 4px;
		color: var(--text-secondary);
		font-size: 0.72rem;
		padding: 0.15rem 0.4rem;
	}

	.kpi-subtext {
		color: var(--text-muted);
		font-size: 0.78rem;
	}

	.kpi-pulsing-badge {
		background: color-mix(in srgb, var(--accent-success, #10b981) 15%, transparent);
		border-radius: 9999px;
		color: var(--accent-success, #10b981);
		font-size: 0.7rem;
		font-weight: 600;
		padding: 0.15rem 0.5rem;
	}

	.kpi-warn-badge {
		background: color-mix(in srgb, var(--accent-warning, #f59e0b) 15%, transparent);
		border-radius: 9999px;
		color: var(--accent-warning, #f59e0b);
		font-size: 0.7rem;
		font-weight: 600;
		padding: 0.15rem 0.5rem;
	}

	.kpi-blue-badge {
		background: color-mix(in srgb, var(--accent-primary) 15%, transparent);
		border-radius: 9999px;
		color: var(--accent-primary);
		font-size: 0.7rem;
		font-weight: 600;
		padding: 0.15rem 0.5rem;
	}

	.text-emerald {
		color: var(--accent-success, #10b981);
	}

	.text-amber {
		color: var(--accent-warning, #f59e0b);
	}

	.text-blue {
		color: var(--accent-primary);
	}

	/* Autonomous Company Loop Banner */
	.company-loop-banner {
		align-items: center;
		background: linear-gradient(135deg, color-mix(in srgb, var(--accent-warning, #f59e0b) 6%, var(--bg-card)) 0%, var(--bg-card) 100%);
		border: 1px solid color-mix(in srgb, var(--accent-warning, #f59e0b) 25%, var(--border-soft));
		border-radius: 12px;
		display: flex;
		gap: 1.25rem;
		justify-content: space-between;
		padding: 1rem 1.35rem;
		position: relative;
		overflow: hidden;
		transition: border-color 0.2s ease, background 0.2s ease;
	}

	.company-loop-banner::before {
		background: var(--accent-warning, #f59e0b);
		content: '';
		inset: 0 auto 0 0;
		position: absolute;
		transition: background 0.2s ease;
		width: 4px;
	}

	.company-loop-banner--active {
		background: linear-gradient(135deg, color-mix(in srgb, var(--accent-success, #10b981) 12%, var(--bg-card)) 0%, var(--bg-card) 100%);
		border-color: color-mix(in srgb, var(--accent-success, #10b981) 40%, var(--border-soft));
	}

	.company-loop-banner--active::before {
		background: var(--accent-success, #10b981);
	}

	.company-loop-banner--error {
		border-color: color-mix(in srgb, var(--accent-error, #ef4444) 40%, var(--border-soft));
	}

	.company-loop-banner--error::before {
		background: var(--accent-error, #ef4444);
	}

	.company-loop-banner__main {
		align-items: flex-start;
		display: flex;
		flex: 1;
		gap: 1rem;
		min-width: 0;
	}

	.company-loop-banner__icon-box {
		align-items: center;
		background: color-mix(in srgb, var(--accent-primary) 12%, var(--bg-soft));
		border: 1px solid color-mix(in srgb, var(--accent-primary) 24%, var(--border-soft));
		border-radius: 10px;
		color: var(--accent-primary);
		display: inline-flex;
		flex-shrink: 0;
		height: 2.75rem;
		justify-content: center;
		margin-top: 0.1rem;
		width: 2.75rem;
	}

	.company-loop-banner--active .company-loop-banner__icon-box {
		background: color-mix(in srgb, var(--accent-success, #10b981) 16%, var(--bg-soft));
		border-color: color-mix(in srgb, var(--accent-success, #10b981) 32%, var(--border-soft));
		color: var(--accent-success, #10b981);
	}

	.company-loop-banner__info {
		display: flex;
		flex: 1;
		flex-direction: column;
		gap: 0.35rem;
		min-width: 0;
	}

	.company-loop-banner__title-row {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.6rem 0.85rem;
	}

	.company-loop-banner__headline {
		align-items: center;
		display: inline-flex;
		gap: 0.55rem;
	}

	.company-loop-banner__headline h2 {
		color: var(--text-primary);
		font-size: 1.125rem;
		font-weight: 700;
		letter-spacing: -0.01em;
		margin: 0;
	}

	.company-loop-pulse {
		align-items: center;
		background: color-mix(in srgb, var(--accent-success, #10b981) 12%, transparent);
		border-radius: 9999px;
		color: var(--accent-success, #10b981);
		display: inline-flex;
		font-size: 0.72rem;
		font-weight: 600;
		gap: 0.35rem;
		padding: 0.2rem 0.55rem;
	}

	.company-loop-banner__meta-tags {
		align-items: center;
		display: inline-flex;
		gap: 0.4rem;
	}

	.meta-tag {
		background: var(--bg-soft);
		border: 1px solid var(--border-soft);
		border-radius: 4px;
		color: var(--text-secondary);
		font-size: 0.72rem;
		padding: 0.15rem 0.45rem;
	}

	.meta-tag--env {
		border-color: color-mix(in srgb, var(--accent-warning, #f59e0b) 40%, var(--border-soft));
		color: var(--accent-warning, #f59e0b);
	}

	.company-loop-banner__desc {
		color: var(--text-secondary);
		font-size: 0.84rem;
		line-height: 1.4;
		margin: 0;
		max-width: 52rem;
	}

	.company-loop-banner__error-banner {
		align-items: center;
		background: color-mix(in srgb, var(--accent-error, #ef4444) 10%, transparent);
		border-radius: 6px;
		color: var(--accent-error, #ef4444);
		display: flex;
		font-size: 0.78rem;
		gap: 0.4rem;
		margin-top: 0.25rem;
		padding: 0.35rem 0.65rem;
	}

	.company-loop-banner__controls {
		align-items: center;
		display: flex;
		flex-shrink: 0;
		gap: 0.85rem;
	}

	.company-loop-toggle-group {
		align-items: center;
		background: var(--bg-soft);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		display: inline-flex;
		gap: 0.6rem;
		padding: 0.35rem 0.65rem;
	}

	.company-loop-toggle-label {
		color: var(--text-primary);
		font-size: 0.8125rem;
		font-weight: 600;
		white-space: nowrap;
	}

	/* Navigation Tabs */
	.crew-tabs {
		border-bottom: 1px solid var(--border-soft);
		display: flex;
		gap: 0.4rem;
		margin-top: 0.25rem;
		overflow-x: auto;
	}

	.crew-tab {
		align-items: center;
		background: transparent;
		border: none;
		border-bottom: 2px solid transparent;
		color: var(--text-secondary);
		cursor: pointer;
		display: inline-flex;
		font-size: 0.88rem;
		font-weight: 600;
		gap: 0.5rem;
		padding: 0.65rem 0.95rem;
		transition: color 0.15s, border-color 0.15s;
		white-space: nowrap;
	}

	.crew-tab:hover {
		color: var(--text-primary);
	}

	.crew-tab--active {
		border-bottom-color: var(--accent-primary);
		color: var(--text-primary);
	}

	.tab-count {
		background: var(--bg-soft);
		border-radius: 9999px;
		color: var(--text-secondary);
		font-size: 0.72rem;
		font-weight: 500;
		padding: 0.1rem 0.45rem;
	}

	.crew-tab--active .tab-count {
		background: color-mix(in srgb, var(--accent-primary) 15%, transparent);
		color: var(--accent-primary);
	}

	/* Tab Content Container */
	.crew-tab-content {
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}

	.tab-content-header {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 1rem;
		justify-content: space-between;
	}

	.tab-content-header h2 {
		font-size: 1.25rem;
		font-weight: 600;
		margin: 0 0 0.2rem;
	}

	.tab-content-header p {
		color: var(--text-secondary);
		font-size: 0.88rem;
		margin: 0;
	}

	/* Toolbar */
	.crew-toolbar {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.75rem;
		justify-content: space-between;
		margin-bottom: 0.25rem;
	}

	.crew-search-box {
		align-items: center;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		display: inline-flex;
		gap: 0.5rem;
		min-width: 280px;
		padding: 0.45rem 0.75rem;
	}

	.crew-search-box input {
		background: transparent;
		border: none;
		color: var(--text-primary);
		font-size: 0.86rem;
		outline: none;
		width: 100%;
	}

	.crew-search-box input::placeholder {
		color: var(--text-muted);
	}

	.search-clear {
		background: transparent;
		border: none;
		color: var(--text-muted);
		cursor: pointer;
		font-size: 1.1rem;
		line-height: 1;
		padding: 0;
	}

	.hierarchy-search-box {
		align-items: center;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		display: inline-flex;
		gap: 0.5rem;
		min-width: 240px;
		padding: 0.4rem 0.7rem;
	}

	.hierarchy-search-box input {
		background: transparent;
		border: none;
		color: var(--text-primary);
		font-size: 0.84rem;
		outline: none;
	}

	.crew-filters {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
	}

	.filter-chip {
		align-items: center;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 9999px;
		color: var(--text-secondary);
		cursor: pointer;
		display: inline-flex;
		font-size: 0.78rem;
		font-weight: 500;
		gap: 0.35rem;
		padding: 0.35rem 0.7rem;
		transition: all 0.15s;
	}

	.filter-chip:hover {
		border-color: var(--accent-primary);
		color: var(--text-primary);
	}

	.filter-chip--active {
		background: color-mix(in srgb, var(--accent-primary) 12%, transparent);
		border-color: var(--accent-primary);
		color: var(--text-primary);
		font-weight: 600;
	}

	.filter-dot {
		border-radius: 50%;
		display: inline-block;
		height: 6px;
		width: 6px;
	}

	.filter-dot--active {
		background: var(--accent-success, #10b981);
	}

	.filter-dot--attention {
		background: var(--accent-warning, #f59e0b);
	}

	.crew-view-switcher {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		display: inline-flex;
		padding: 2px;
	}

	.view-toggle-btn {
		align-items: center;
		background: transparent;
		border: none;
		border-radius: 6px;
		color: var(--text-secondary);
		cursor: pointer;
		display: inline-flex;
		justify-content: center;
		padding: 0.4rem 0.55rem;
		transition: all 0.15s;
	}

	.view-toggle-btn--active {
		background: var(--bg-soft);
		color: var(--text-primary);
	}

	/* Table Container */
	.crew-table-container {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 10px;
		overflow-x: auto;
		width: 100%;
	}

	.crew-table {
		border-collapse: collapse;
		font-size: 0.86rem;
		table-layout: auto;
		width: 100%;
	}

	.crew-table th,
	.crew-table td {
		border-bottom: 1px solid var(--border-soft);
		padding: 0.75rem 0.9rem;
		text-align: left;
		vertical-align: middle;
	}

	.crew-table th {
		background: color-mix(in srgb, var(--bg-card) 90%, var(--bg-soft) 10%);
		color: var(--text-secondary);
		font-size: 0.74rem;
		font-weight: 600;
		letter-spacing: 0.03em;
		text-transform: uppercase;
		white-space: nowrap;
	}

	.crew-table tbody tr:hover {
		background: color-mix(in srgb, var(--bg-soft) 50%, transparent);
	}

	.crew-table tbody tr:last-child td {
		border-bottom: none;
	}

	/* Member Cell */
	.crew-member-cell {
		align-items: flex-start;
		display: flex;
		gap: 0.6rem;
	}

	.crew-expand {
		align-items: center;
		background: transparent;
		border: 1px solid var(--border-soft);
		border-radius: 4px;
		color: var(--text-secondary);
		cursor: pointer;
		display: inline-flex;
		height: 22px;
		justify-content: center;
		margin-top: 2px;
		padding: 0;
		width: 22px;
	}

	.crew-expand:hover {
		border-color: var(--accent-primary);
		color: var(--text-primary);
	}

	.crew-member-info {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		min-width: 0;
	}

	.crew-member-title {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
	}

	.agent-title-link {
		color: var(--text-primary);
		font-weight: 600;
		text-decoration: none;
	}

	.agent-title-link:hover {
		color: var(--accent-primary);
		text-decoration: underline;
	}

	.role-caption {
		color: var(--text-secondary);
		font-size: 0.78rem;
	}

	.crew-member-sub {
		align-items: center;
		color: var(--text-muted);
		display: flex;
		flex-wrap: wrap;
		font-size: 0.74rem;
		gap: 0.35rem;
	}

	.dot-sep {
		opacity: 0.5;
	}

	.kind-tag {
		background: var(--bg-soft);
		border-radius: 3px;
		padding: 0.05rem 0.3rem;
	}

	.sub-link {
		color: var(--text-secondary);
		text-decoration: none;
	}

	.sub-link:hover {
		color: var(--accent-primary);
		text-decoration: underline;
	}

	.agent-pill-primary {
		background: color-mix(in srgb, var(--accent-primary) 15%, transparent);
		border-radius: 4px;
		color: var(--accent-primary);
		font-size: 0.68rem;
		font-weight: 600;
		padding: 0.08rem 0.35rem;
	}

	.agent-pill-envoy {
		background: color-mix(in srgb, var(--accent-warning, #f59e0b) 15%, transparent);
		border-radius: 4px;
		color: var(--accent-warning, #f59e0b);
		font-size: 0.68rem;
		font-weight: 600;
		padding: 0.08rem 0.35rem;
	}

	/* Status Cell */
	.status-cell {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
	}

	.status-dot {
		border-radius: 50%;
		display: inline-block;
		height: 6px;
		width: 6px;
	}

	.status-dot--info {
		background: var(--accent-primary);
	}

	.status-dot--success {
		background: var(--accent-success, #10b981);
	}

	.status-dot--warning {
		background: var(--accent-warning, #f59e0b);
	}

	.status-dot--error {
		background: var(--accent-error, #ef4444);
	}

	.status-dot--default {
		background: var(--text-muted);
	}

	.status-sub-disabled {
		color: var(--accent-warning, #f59e0b);
		font-size: 0.7rem;
	}

	/* Health Cell */
	.health-cell {
		align-items: baseline;
		display: inline-flex;
		gap: 0.35rem;
	}

	.health-score {
		font-size: 0.95rem;
		font-weight: 700;
	}

	.health-score[data-band='good'] {
		color: var(--accent-success, #10b981);
	}

	.health-score[data-band='fair'] {
		color: var(--accent-warning, #f59e0b);
	}

	.health-score[data-band='poor'] {
		color: var(--accent-error, #ef4444);
	}

	.health-delta {
		font-size: 0.72rem;
		font-weight: 600;
	}

	.health-delta[data-delta='up'] {
		color: var(--accent-success, #10b981);
	}

	.health-delta[data-delta='down'] {
		color: var(--accent-error, #ef4444);
	}

	/* Telemetry Cell */
	.telemetry-cell {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
	}

	.spend-text {
		font-weight: 600;
	}

	.calls-sub {
		color: var(--text-muted);
		font-size: 0.74rem;
	}

	.cycle-cell {
		color: var(--text-secondary);
		display: inline-block;
		max-width: 120px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	/* Approvals Cell */
	.approvals-cell {
		display: flex;
		flex-wrap: wrap;
		gap: 0.3rem;
	}

	.alert-pill {
		border-radius: 4px;
		font-size: 0.72rem;
		font-weight: 600;
		padding: 0.15rem 0.4rem;
	}

	.alert-pill--warn {
		background: color-mix(in srgb, var(--accent-warning, #f59e0b) 18%, transparent);
		color: var(--accent-warning, #f59e0b);
	}

	.alert-pill--error {
		background: color-mix(in srgb, var(--accent-error, #ef4444) 18%, transparent);
		color: var(--accent-error, #ef4444);
	}

	/* Actions */
	.crew-action-row {
		display: flex;
		gap: 0.4rem;
	}

	.crew-details-expanded {
		background: var(--bg-soft);
		border-radius: 8px;
		display: flex;
		flex-direction: column;
		gap: 0.85rem;
		margin: 0.35rem 0;
		padding: 1rem;
	}

	.crew-details-grid {
		display: grid;
		gap: 0.65rem 1.25rem;
		grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
		margin: 0;
	}

	.crew-details-grid dt {
		color: var(--text-secondary);
		font-size: 0.74rem;
		font-weight: 600;
		margin-bottom: 0.2rem;
		text-transform: uppercase;
	}

	.crew-details-grid dd {
		color: var(--text-primary);
		font-size: 0.86rem;
		line-height: 1.4;
		margin: 0;
		word-break: break-word;
	}

	/* CARD / GRID VIEW */
	.crew-cards-grid {
		display: grid;
		gap: 1rem;
		grid-template-columns: repeat(auto-fill, minmax(360px, 1fr));
		width: 100%;
	}

	.agent-card {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 12px;
		display: flex;
		flex-direction: column;
		gap: 0.85rem;
		padding: 1.15rem;
		transition: border-color 0.15s, box-shadow 0.15s;
	}

	.agent-card:hover {
		border-color: color-mix(in srgb, var(--accent-primary) 50%, var(--border-soft));
		box-shadow: 0 4px 16px rgba(0, 0, 0, 0.06);
	}

	.agent-card__header {
		align-items: flex-start;
		display: flex;
		gap: 0.75rem;
	}

	.agent-card__avatar {
		align-items: center;
		background: color-mix(in srgb, var(--accent-primary) 14%, var(--bg-soft));
		border-radius: 10px;
		color: var(--accent-primary);
		display: flex;
		font-family: var(--font-display, var(--font-primary));
		font-size: 0.95rem;
		font-weight: 700;
		height: 40px;
		justify-content: center;
		position: relative;
		width: 40px;
	}

	.card-status-dot {
		border: 2px solid var(--bg-card);
		border-radius: 50%;
		bottom: -2px;
		height: 9px;
		position: absolute;
		right: -2px;
		width: 9px;
	}

	.agent-card__info {
		flex: 1;
		min-width: 0;
	}

	.agent-card__title-row {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
	}

	.card-agent-name {
		color: var(--text-primary);
		font-size: 0.98rem;
		font-weight: 600;
		text-decoration: none;
	}

	.card-agent-name:hover {
		color: var(--accent-primary);
		text-decoration: underline;
	}

	.card-role {
		color: var(--text-secondary);
		font-size: 0.82rem;
		margin: 0.1rem 0;
	}

	.card-id {
		color: var(--text-muted);
		font-size: 0.74rem;
		margin: 0;
	}

	.agent-card__desc {
		color: var(--text-secondary);
		display: -webkit-box;
		font-size: 0.84rem;
		line-height: 1.45;
		margin: 0;
		overflow: hidden;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
	}

	.agent-card__stats {
		background: var(--bg-soft);
		border-radius: 8px;
		display: grid;
		gap: 0.5rem;
		grid-template-columns: repeat(4, 1fr);
		padding: 0.6rem 0.75rem;
	}

	.card-stat {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
	}

	.card-stat-label {
		color: var(--text-muted);
		font-size: 0.68rem;
		font-weight: 500;
		text-transform: uppercase;
	}

	.card-stat strong {
		font-size: 0.88rem;
	}

	.card-cycle-text {
		font-size: 0.76rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.agent-card__meta {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
	}

	.delegates-tag {
		color: var(--text-muted);
		font-size: 0.74rem;
	}

	.agent-card__footer {
		align-items: center;
		border-top: 1px solid var(--border-soft);
		display: flex;
		gap: 0.6rem;
		justify-content: space-between;
		padding-top: 0.75rem;
	}

	.card-actions-left {
		display: flex;
		gap: 0.4rem;
	}

	.card-actions-right {
		align-items: center;
		display: flex;
		gap: 0.4rem;
	}

	.icon-btn-toggle {
		align-items: center;
		background: var(--bg-soft);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-secondary);
		cursor: pointer;
		display: inline-flex;
		height: 28px;
		justify-content: center;
		padding: 0;
		width: 28px;
	}

	.icon-btn-toggle:hover {
		border-color: var(--accent-primary);
		color: var(--text-primary);
	}

	.card-expanded-drawer {
		border-top: 1px solid var(--border-soft);
		padding-top: 0.75rem;
	}

	/* Common Button & Badge primitives */
	.crew-button {
		align-items: center;
		border: 1px solid transparent;
		border-radius: 7px;
		cursor: pointer;
		display: inline-flex;
		font-family: inherit;
		font-size: 0.85rem;
		font-weight: 600;
		gap: 0.45rem;
		justify-content: center;
		line-height: 1;
		padding: 0.55rem 0.9rem;
		text-decoration: none;
		transition: all 0.15s;
		white-space: nowrap;
	}

	.crew-button:disabled {
		cursor: not-allowed;
		opacity: 0.55;
	}

	.crew-button-primary {
		background: var(--accent-primary);
		color: var(--accent-contrast, #ffffff);
	}

	.crew-button-primary:hover:not(:disabled) {
		filter: brightness(1.08);
	}

	.crew-button-secondary {
		background: var(--bg-card);
		border-color: var(--border-soft);
		color: var(--text-primary);
	}

	.crew-button-secondary:hover:not(:disabled) {
		background: var(--bg-soft);
		border-color: var(--accent-primary);
	}

	.crew-button-outline {
		background: transparent;
		border-color: var(--border-soft);
		color: var(--text-secondary);
	}

	.crew-button-outline:hover:not(:disabled) {
		background: var(--bg-soft);
		border-color: var(--text-secondary);
		color: var(--text-primary);
	}

	.crew-button-sm {
		font-size: 0.76rem;
		padding: 0.35rem 0.6rem;
	}

	.crew-badge {
		align-items: center;
		border-radius: 4px;
		display: inline-flex;
		font-size: 0.74rem;
		font-weight: 600;
		gap: 0.3rem;
		line-height: 1;
		padding: 0.25rem 0.45rem;
		white-space: nowrap;
	}

	.crew-badge-default {
		background: var(--bg-soft);
		color: var(--text-secondary);
	}

	.crew-badge-info {
		background: color-mix(in srgb, var(--accent-primary) 15%, transparent);
		color: var(--accent-primary);
	}

	.crew-badge-success {
		background: color-mix(in srgb, var(--accent-success, #10b981) 15%, transparent);
		color: var(--accent-success, #10b981);
	}

	.crew-badge-warning {
		background: color-mix(in srgb, var(--accent-warning, #f59e0b) 18%, transparent);
		color: var(--accent-warning, #f59e0b);
	}

	.crew-badge-error {
		background: color-mix(in srgb, var(--accent-error, #ef4444) 18%, transparent);
		color: var(--accent-error, #ef4444);
	}

	.crew-badge-outline {
		border: 1px solid var(--border-soft);
		color: var(--text-secondary);
	}

	/* Switch */
	.crew-switch {
		align-items: center;
		background: var(--bg-soft);
		border: 1px solid var(--border-soft);
		border-radius: 9999px;
		cursor: pointer;
		display: inline-flex;
		height: 22px;
		padding: 2px;
		position: relative;
		transition: background 0.15s, border-color 0.15s;
		width: 40px;
	}

	.crew-switch span {
		background: var(--text-secondary);
		border-radius: 50%;
		display: block;
		height: 16px;
		transform: translateX(0);
		transition: transform 0.15s, background 0.15s;
		width: 16px;
	}

	.crew-switch-enabled {
		background: var(--accent-success, #10b981);
		border-color: var(--accent-success, #10b981);
	}

	.crew-switch-enabled span {
		background: #ffffff;
		transform: translateX(18px);
	}

	/* Empty state */
	.crew-empty {
		align-items: center;
		background: var(--bg-card);
		border: 1px dashed var(--border-soft);
		border-radius: 12px;
		color: var(--text-secondary);
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
		justify-content: center;
		padding: 3rem 1.5rem;
		text-align: center;
	}

	.crew-empty h3 {
		color: var(--text-primary);
		font-size: 1.15rem;
		font-weight: 600;
		margin: 0;
	}

	.crew-empty p {
		font-size: 0.9rem;
		margin: 0;
		max-width: 28rem;
	}

	/* Alert */
	.crew-alert {
		align-items: center;
		background: color-mix(in srgb, var(--accent-error, #ef4444) 10%, transparent);
		border: 1px solid var(--accent-error, #ef4444);
		border-radius: 8px;
		color: var(--accent-error, #ef4444);
		display: flex;
		font-size: 0.88rem;
		font-weight: 500;
		gap: 0.6rem;
		padding: 0.75rem 1rem;
	}

	.crew-mono {
		font-family: var(--font-mono, monospace);
		font-size: 0.76rem;
	}

	.text-muted {
		color: var(--text-muted);
	}

	.crew-pagination-bar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		flex-wrap: wrap;
		gap: 0.75rem;
		margin-top: 1.25rem;
		padding-top: 0.75rem;
		border-top: 1px solid var(--border-subtle, rgba(255, 255, 255, 0.08));
	}

	.crew-pagination-bar__per-page {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		font-size: 0.8125rem;
		color: var(--text-muted, #94a3b8);
	}

	.crew-pagination-bar__per-page select {
		background: var(--surface-bg-subtle, rgba(255, 255, 255, 0.04));
		border: 1px solid var(--border-subtle, rgba(255, 255, 255, 0.12));
		color: var(--text-primary, #f1f5f9);
		border-radius: 4px;
		padding: 0.2rem 0.4rem;
		font-size: 0.8125rem;
		cursor: pointer;
	}

	.crew-pagination-bar__per-page select:focus {
		outline: none;
		border-color: var(--accent-primary, #38bdf8);
	}

	@media (max-width: 900px) {
		.company-loop-banner {
			flex-direction: column;
			align-items: stretch;
		}

		.company-loop-banner__controls {
			justify-content: space-between;
			width: 100%;
		}

		.crew-masthead {
			flex-direction: column;
		}

		.crew-kpi-ribbon {
			grid-template-columns: repeat(2, 1fr);
		}

		.crew-toolbar {
			flex-direction: column;
			align-items: stretch;
		}

		.crew-search-box {
			width: 100%;
			min-width: 0;
		}
	}

	@media (max-width: 600px) {
		.crew-page {
			padding: 1rem 0.75rem 4rem;
		}

		.crew-kpi-ribbon {
			grid-template-columns: 1fr;
		}
	}
</style>
