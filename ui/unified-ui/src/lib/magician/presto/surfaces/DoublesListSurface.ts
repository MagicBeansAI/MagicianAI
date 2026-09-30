import type { MuijComponent } from '$lib/stores/muijStore';
import type { AgentSummary, SystemAgentSummary } from '$lib/stores/agentStore';

export interface DoublesListSurfaceInput {
	systemAgents: SystemAgentSummary[];
	customAgents: AgentSummary[];
	isLoading: boolean;
	pageError: string | null;
	interactionBusy: boolean;
	lastUpdatedAt: number | null;
	schemaVersion?: string;
	now?: number;
}

export interface ParsedDoublesAction {
	type: 'refresh' | 'summon';
}

type BadgeColor = 'default' | 'success' | 'warning' | 'error' | 'info';

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

interface DelegationGraph {
	rows: Array<Record<string, unknown>>;
	edgeCount: number;
	rootCount: number;
	missingTargetCount: number;
	cycleCount: number;
}

type AgentSection = 'system' | 'custom';

const PRESTO_DOUBLES_LIST_SCHEMA_VERSION = 'presto.doubles-list-v1';
const ROUTE_PREFIX = 'presto-doubles';
const CONTROL_REFRESH_ID = `${ROUTE_PREFIX}-action-refresh`;
const CONTROL_SUMMON_ID = `${ROUTE_PREFIX}-action-summon`;
const EMPTY_CUSTOM_ID = `${ROUTE_PREFIX}-custom-empty`;
const EMPTY_SYSTEM_ID = `${ROUTE_PREFIX}-system-empty`;

function asString(value: unknown): string {
	if (typeof value === 'string') return value;
	if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') {
		return String(value);
	}
	return '';
}

function asBoolean(value: unknown): boolean {
	return value === true;
}

function asRecord(value: unknown): Record<string, unknown> {
	return value != null && typeof value === 'object' && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

function asFiniteNumber(value: unknown): number {
	const normalized = typeof value === 'number' ? value : Number.parseFloat(asString(value));
	return Number.isFinite(normalized) ? normalized : 0;
}

function formatRelativeTime(timestamp: number, now: number): string {
	if (!timestamp || !Number.isFinite(timestamp)) return 'never';
	const delta = now - timestamp;
	const abs = Math.abs(delta);
	const minutes = Math.round(abs / 1000 / 60);
	const hours = Math.round(abs / 1000 / 60 / 60);
	const days = Math.round(abs / 1000 / 60 / 60 / 24);
	if (minutes < 1) return 'just now';
	if (minutes < 60) return `${minutes}m ${delta >= 0 ? 'ago' : 'from now'}`;
	if (hours < 36) return `${hours}h ${delta >= 0 ? 'ago' : 'from now'}`;
	return `${days}d ${delta >= 0 ? 'ago' : 'from now'}`;
}

function safeAgentValue(value: unknown): string {
	return asString(value).trim() || '—';
}

function safeRowKey(value: string): string {
	const normalized = asString(value).trim().toLowerCase();
	const safe = normalized
		.replace(/[^a-z0-9._-]/g, '-')
		.replace(/-+/g, '-')
		.replace(/^-+|-+$/g, '');
	return safe || `agent-${Math.max(0, normalized.length)}`;
}

function statusLabelFromSummary(status: AgentSummary['status']): string {
	switch (status) {
		case 'running':
			return 'running';
		case 'triggered':
			return 'triggered';
		case 'paused':
			return 'paused';
		case 'completed':
			return 'completed';
		case 'error':
			return 'error';
		case 'disabled':
			return 'disabled';
		case 'idle':
		default:
			return 'idle';
	}
}

function statusColorFromSummary(status: AgentSummary['status']): BadgeColor {
	switch (status) {
		case 'running':
		case 'triggered':
			return 'info';
		case 'completed':
			return 'success';
		case 'paused':
			return 'warning';
		case 'error':
			return 'error';
		case 'disabled':
			return 'default';
		case 'idle':
		default:
			return 'default';
	}
}

function asBadge(text: string, color: BadgeColor): Record<string, string> {
	return {
		kind: 'badge',
		text,
		color
	};
}

function normalizeAgentId(agentId: unknown): string {
	return asString(agentId).trim() || 'unknown-agent';
}

function sectionGridId(section: AgentSection): string {
	return `${ROUTE_PREFIX}-${section}-grid`;
}

function sectionCardId(section: AgentSection): string {
	return `${ROUTE_PREFIX}-${section}-section`;
}

function sectionTitle(section: AgentSection): string {
	return section === 'system' ? 'Internal pipeline agents' : 'Custom crew';
}

function rowId(section: AgentSection, rowKey: string, index: number): string {
	const suffix = rowKey || `agent-${index}`;
	return `${ROUTE_PREFIX}-${section}-row-${suffix}-${index}`;
}

function asDoublesLink(href: string, label: string): Record<string, string> {
	const safeHref = asString(href).trim();
	const safeLabel = asString(label).trim() || safeHref;
	return {
		kind: 'link',
		href: safeHref,
		label: safeLabel
	};
}

function asSummaryCell(title: string, meta: string, links: Array<Record<string, string>>): Record<string, unknown> {
	return {
		kind: 'summary',
		title,
		meta,
		links
	};
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
	if (!packs || packs.length === 0) return '—';
	return packs.join(', ');
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

function agentDisplayName(agent: AgentRenderRecord): string {
	return agent.name && agent.name !== agent.agentId ? `${agent.name} (${agent.agentId})` : agent.agentId;
}

function formatAgentRefs(agentIds: string[], agentsById: Map<string, AgentRenderRecord>): string {
	if (agentIds.length === 0) return '—';
	return agentIds
		.map((agentId) => {
			if (agentId === '*') return '*';
			const agent = agentsById.get(agentId);
			return agent ? agentDisplayName(agent) : `${agentId} (missing)`;
		})
		.join(', ');
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
		capabilityPacks: '—',
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
		lastActive: formatRelativeTime(rawUpdatedAt, Date.now()),
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
	const rows: Array<Record<string, unknown>> = [];
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
			level: String(depth),
			member: `${hierarchyPrefix(depth)} ${agent.name}`,
			memberSummary: asSummaryCell(`${hierarchyPrefix(depth)} ${agent.name}`, agent.roleLabel, [
				asDoublesLink(`/crew/${encodeURIComponent(agent.agentId)}`, 'Open')
			]),
			roleLabel: agent.roleLabel,
			agentId: agent.agentId,
			parent: parentId ? formatAgentRefs([parentId], agentsById) : '—',
			availability: asBadge(availability.text, availability.color),
			status: asBadge(agent.statusLabel, agent.statusColor),
			delegatesTo: formatAgentRefs(agent.delegatesTo, agentsById),
			delegatedBy: formatAgentRefs(agent.delegatedBy, agentsById),
			path: [...path, agent.agentId].join(' > '),
			note: notes.join('; ') || '—'
		});

		if (isCycle || isShared) return;
		seenAgents.add(agent.agentId);
		const nextInheritedDisabledFrom = inheritedDisabledFrom
			?? (agent.configuredDisabled || agent.availabilityLabel === 'disabled' ? agent.name : null);
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

function buildTopMetrics(agents: AgentRenderRecord[]): MuijComponent[] {
	const customCount = agents.filter((agent) => !agent.isSystem).length;
	const systemCount = agents.length - customCount;
	const disabledCount = agents.filter((agent) => !agent.isSystem && agent.availabilityLabel === 'disabled').length;
	const activeNow = agents.filter(
		(agent) => !agent.isSystem && (agent.statusLabel === 'running' || agent.statusLabel === 'triggered')
	).length;
	const withPendingApproval = agents.filter(
		(agent) => !agent.isSystem && asFiniteNumber(agent.pendingApprovals) > 0
	).length;

	return [
		{
			id: 'presto-doubles-metric-total',
			component_type: 'MetricCard',
			label: 'Agent count',
			props: {
				value: String(agents.length),
				label: 'known definitions'
			}
		},
		{
			id: 'presto-doubles-metric-system',
			component_type: 'MetricCard',
			label: 'Pipeline',
			props: {
				value: String(systemCount),
				label: 'internal roles'
			}
		},
		{
			id: 'presto-doubles-metric-custom',
			component_type: 'MetricCard',
			label: 'Custom',
			props: {
				value: String(customCount),
				label: 'editable definitions'
			}
		},
		{
			id: 'presto-doubles-metric-disabled',
			component_type: 'MetricCard',
			label: 'Disabled',
			props: {
				value: String(disabledCount),
				label: 'not schedulable or triggerable'
			}
		},
		{
			id: 'presto-doubles-metric-active',
			component_type: 'MetricCard',
			label: 'Active',
			props: {
				value: String(activeNow),
				label: 'running or triggered'
			}
		},
		{
			id: 'presto-doubles-metric-pending',
			component_type: 'MetricCard',
			label: 'Pending',
			props: {
				value: String(withPendingApproval),
				label: 'approval requests'
			}
		}
	];
}

function buildSectionRows(section: AgentSection, agents: AgentRenderRecord[]): Array<Record<string, unknown>> {
	const agentsById = new Map(agents.map((agent) => [agent.agentId, agent]));
	return agents.map((agent, index) => {
		const row = rowId(section, agent.rowKey, index);
		const baseRow: Record<string, unknown> = {
			rowId: row,
			agentId: agent.agentId,
			name: agent.name,
			roleLabel: agent.roleLabel,
			description: agent.description,
			scope: section,
			availability: asBadge(agent.availabilityLabel, agent.availabilityColor),
			status: asBadge(agent.statusLabel, agent.statusColor),
			kind: agent.kind,
			delegatedBy: formatAgentRefs(agent.delegatedBy, agentsById),
			delegatesTo: formatAgentRefs(agent.delegatesTo, agentsById),
			capabilities: agent.capabilityPacks,
			cycle: agent.currentCycle,
			approvals: agent.pendingApprovals
		};
		if (section === 'custom') {
			baseRow.memberSummary = asSummaryCell(agent.name, agent.roleLabel, [
				asDoublesLink(`/crew/${encodeURIComponent(agent.agentId)}`, 'Open'),
				asDoublesLink(`/crew/new?edit=${encodeURIComponent(agent.agentId)}`, 'Edit')
			]);
				baseRow.delegationSummary = [
					`By: ${formatAgentRefs(agent.delegatedBy, agentsById)}`,
					`To: ${formatAgentRefs(agent.delegatesTo, agentsById)}`
				].join(' · ');
				baseRow.runtimeSummary = `Capabilities: ${agent.capabilityPacks}`;
		}
		if (section === 'system') {
			baseRow.pipelineSummary = asSummaryCell(agent.name, agent.agentId, []);
		}
		return baseRow;
	});
}

function buildSection(section: AgentSection, sectionAgents: AgentRenderRecord[]): MuijComponent {
	const rows = buildSectionRows(section, sectionAgents);
	const hasNoAgents = rows.length === 0;
	const sectionId = sectionCardId(section);
	const sectionEmptyId = section === 'system' ? EMPTY_SYSTEM_ID : EMPTY_CUSTOM_ID;

	return {
		id: sectionId,
		component_type: 'Card',
		label: sectionTitle(section),
		props: {
			title: sectionTitle(section),
			subtitle: `${sectionAgents.length} ${sectionAgents.length === 1 ? 'entry' : 'entries'}`,
			body: section === 'system'
				? 'Read-only backend pipeline roles used for intent classification, planning, slot extraction, and clarification. These are not scoped runtime crew members.'
				: 'Editable custom definitions controlled by users.'
		},
		children: hasNoAgents
			? [
				{
					id: sectionEmptyId,
					component_type: 'EmptyState',
					label: section === 'system' ? 'No internal pipeline agents' : 'No custom crew members yet',
					props: {
						icon: '∅',
						title: section === 'system' ? 'No internal pipeline agents' : 'No custom crew members yet',
						description:
							section === 'system'
								? 'No internal pipeline descriptors were returned by the registry.'
								: 'Create one to begin accruing autonomous agent behavior.',
						...(section === 'custom' ? { actionLabel: 'Create a crew member' } : {})
					}
				}
			]
			: [
				{
					id: sectionGridId(section),
					component_type: 'EntityGrid',
					props: {
						columns: section === 'custom'
								? [
									{ key: 'memberSummary', label: 'Crew member', sortable: false, width: '38%' },
									{ key: 'scope', label: 'Scope', sortable: true, width: '9%' },
									{ key: 'availability', label: 'Enabled', sortable: false, width: '11%' },
									{ key: 'status', label: 'Status', sortable: false, width: '9%' },
									{ key: 'kind', label: 'Kind', sortable: true, width: '12%' },
									{ key: 'cycle', label: 'Cycle', sortable: true, width: '12%' },
									{ key: 'approvals', label: 'Approvals', sortable: true, width: '9%' }
								]
							: [
								{ key: 'pipelineSummary', label: 'Pipeline role', sortable: false, width: '48%' },
								{ key: 'scope', label: 'Scope', sortable: true, width: '14%' },
								{ key: 'availability', label: 'Enabled', sortable: false, width: '14%' },
								{ key: 'status', label: 'Status', sortable: false, width: '12%' },
								{ key: 'kind', label: 'Kind', sortable: true, width: '12%' }
							],
						rows,
						pageSize: 10,
						...(section === 'custom'
							? {
								filterKeys: ['name', 'roleLabel'],
								filterPlaceholder: 'Filter by crew member',
								filterLabel: 'Filter custom crew by crew member',
								stackedFields: [
									{ key: 'description', label: 'Role' },
									{ key: 'delegationSummary', label: 'Delegation' },
									{ key: 'runtimeSummary', label: 'Runtime' }
								],
								stackedValueMaxLines: 0,
								stackedRowsExpandable: true,
								wrapTable: true
							}
							: {
								stackedFields: [
									{ key: 'description', label: 'Purpose' }
								],
								stackedValueMaxLines: 0,
								stackedRowsExpandable: true,
								wrapTable: true
							})
					}
				}
			]
	};
}

function buildDelegationHierarchySection(graph: DelegationGraph): MuijComponent {
	const hasRows = graph.rows.length > 0;
	const diagnostics = [
		`${graph.rootCount} ${graph.rootCount === 1 ? 'root' : 'roots'}`,
		`${graph.edgeCount} ${graph.edgeCount === 1 ? 'delegation edge' : 'delegation edges'}`,
		graph.missingTargetCount > 0 ? `${graph.missingTargetCount} missing ${graph.missingTargetCount === 1 ? 'target' : 'targets'}` : '',
		graph.cycleCount > 0 ? `${graph.cycleCount} ${graph.cycleCount === 1 ? 'cycle' : 'cycles'}` : ''
	].filter(Boolean);

	return {
		id: `${ROUTE_PREFIX}-delegation-hierarchy`,
		component_type: 'Card',
		label: 'Delegation hierarchy',
		props: {
			title: 'Delegation Hierarchy',
			subtitle: diagnostics.join(' · ') || 'No delegation graph yet',
			body: 'Derived from delegation_targets in agent definitions. Disabled ancestors are shown on descendants so inactive branches remain visible.'
		},
		children: hasRows
			? [
				{
					id: `${ROUTE_PREFIX}-delegation-hierarchy-grid`,
					component_type: 'EntityGrid',
					props: {
						columns: [
							{ key: 'memberSummary', label: 'Hierarchy', sortable: false, width: '52%' },
							{ key: 'availability', label: 'Enabled', sortable: false, width: '16%' },
							{ key: 'status', label: 'Status', sortable: false, width: '12%' },
							{ key: 'parent', label: 'Parent', sortable: false, width: '20%' }
						],
						rows: graph.rows,
						pageSize: 25,
						filterKeys: ['member', 'roleLabel', 'parent'],
						filterPlaceholder: 'Filter by hierarchy or parent',
						filterLabel: 'Filter delegation hierarchy by hierarchy or parent',
						stackedFields: [
							{ key: 'delegatesTo', label: 'Delegates to' },
							{ key: 'path', label: 'Path' },
							{ key: 'note', label: 'Note' }
						],
						stackedValueMaxLines: 0,
						stackedRowsExpandable: true,
						wrapTable: true
					}
				}
			]
			: [
				{
					id: `${ROUTE_PREFIX}-delegation-hierarchy-empty`,
					component_type: 'EmptyState',
					label: 'No delegation hierarchy',
					props: {
						icon: '∅',
						title: 'No custom crew hierarchy',
						description: 'No custom agent definitions were returned, so no delegation graph can be derived.'
					}
				}
			]
	};
}

export function buildDoublesListSurface(input: DoublesListSurfaceInput): MuijComponent[] {
	const safeNow = input.now ?? Date.now();
	const safeSystems = input.systemAgents.map(mapSystemAgent);
	const safeCustom = input.customAgents.map(mapCustomAgent);
	const delegationGraph = buildDelegationGraph(safeCustom);
	const allRows = [...safeSystems, ...safeCustom];
	const schemaTag = asString(input.schemaVersion || PRESTO_DOUBLES_LIST_SCHEMA_VERSION).trim();
	const isBusy = asBoolean(input.interactionBusy);
	const isLoading = asBoolean(input.isLoading);
	const freshnessLabel = formatRelativeTime(input.lastUpdatedAt ?? safeNow, safeNow);

	const components: MuijComponent[] = [
		{
			id: `${ROUTE_PREFIX}-header`,
			component_type: 'Card',
			label: 'Crew',
			props: {
				title: 'Crew',
				body: isLoading ? 'Loading crew directory…' : 'Open a crew member to inspect status, memory, and execution.'
			},
			children: [
				{
					id: `${ROUTE_PREFIX}-meta`,
					component_type: 'Stack',
					props: {
						direction: 'row',
						gap: '0.5rem',
						wrap: true
					},
					children: [
						{
							id: 'presto-doubles-meta-total',
							component_type: 'Tag',
							props: {
								text: `Total entries ${String(allRows.length)}`,
								color: 'info'
							}
						},
						{
							id: 'presto-doubles-meta-custom',
							component_type: 'Tag',
							props: {
								text: `Custom ${String(safeCustom.length)}`,
								color: 'success'
							}
						},
						{
							id: 'presto-doubles-meta-system',
							component_type: 'Tag',
							props: {
								text: `Pipeline ${String(safeSystems.length)}`,
								color: 'default'
							}
						},
						{
							id: 'presto-doubles-meta-updated',
							component_type: 'Tag',
							props: {
								text: `Updated ${freshnessLabel}`,
								color: 'default'
							}
						}
					]
				}
			]
		},
		{
			id: `${ROUTE_PREFIX}-metrics`,
			component_type: 'Grid',
			label: 'Crew metrics',
			props: {
				autoFit: true,
				minColumnWidth: '180px',
				gap: '0.7rem',
				className: 'presto-doubles-metrics-grid'
			},
			children: buildTopMetrics(allRows)
		},
		{
			id: `${ROUTE_PREFIX}-controls`,
			component_type: 'Card',
			label: 'Crew Controls',
			props: {
				title: 'Crew Controls',
				subtitle: 'Manage and open crew members from this operator surface',
				body: isBusy ? 'Applying requested action…' : 'Use controls to navigate and refresh.'
			},
			children: [
				{
					id: 'presto-doubles-action-row',
					component_type: 'Stack',
					props: {
						direction: 'row',
						gap: '0.6rem',
						wrap: true
					},
					children: [
						{
							id: CONTROL_REFRESH_ID,
							component_type: 'Button',
							label: isBusy ? 'Refreshing…' : 'Refresh',
							props: {
								interactive: true,
								variant: 'secondary',
								size: 'sm',
								disabled: isBusy
							}
						},
						{
							id: CONTROL_SUMMON_ID,
							component_type: 'Button',
							label: 'Create Crew Member',
							props: {
								interactive: true,
								variant: 'primary',
								size: 'sm',
								disabled: isBusy
							}
						}
					]
				}
			]
		}
	];

	if (input.pageError) {
		components.push({
			id: `${ROUTE_PREFIX}-error`,
			component_type: 'Alert',
			label: 'Crew fetch warning',
			props: {
				type: 'error',
				message: input.pageError,
				closable: false
			}
		});
	}

	components.push(buildSection('custom', safeCustom));
	components.push(buildDelegationHierarchySection(delegationGraph));
	components.push(buildSection('system', safeSystems));

	return components;
}

export function parseDoublesAction(value: unknown): ParsedDoublesAction | null {
	const detail = asRecord(value);
	const componentId = asString(detail.componentId).trim();

	if (componentId === CONTROL_REFRESH_ID) {
		return { type: 'refresh' };
	}
	if (componentId === CONTROL_SUMMON_ID || componentId === EMPTY_CUSTOM_ID) {
		return { type: 'summon' };
	}

	return null;
}
