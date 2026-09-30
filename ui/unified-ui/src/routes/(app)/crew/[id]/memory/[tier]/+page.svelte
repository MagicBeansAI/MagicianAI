<script lang="ts">
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { onDestroy, onMount } from 'svelte';
	import { page } from '$app/stores';
	import NativeCrewRenderer from '$lib/magician/crew/NativeCrewRenderer.svelte';
	import type { CrewNativeComponent, CrewNativeInteractionEventDetail } from '$lib/magician/crew/nativeSurface';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { timedFetch } from '$lib/shared/fetch';

	type TierScope = 'agent' | 'agent_goal' | 'user';
	type MemoryTierRenderer = 'key_value' | 'scored_list' | 'append_log' | 'archive';

	interface TierDefinition {
		name: string;
		scope: TierScope;
		description: string;
		render: {
			format: string;
			template: string;
		};
	}

	interface V3MemoryTierRecord {
		tier_name: string;
		tier_scope?: TierScope;
		principal?: string;
		workspace?: string;
		agent_id?: string;
		goal_id?: string;
		fields: Record<string, unknown>;
		last_updated: string;
	}

	interface ConsolidationRule {
		name: string;
		source: string;
		target: string;
		trigger: unknown;
		transform: unknown;
	}

	interface AgentMemoryTierResponse {
		agent_id: string;
		tier: TierDefinition;
		goal_id?: string;
		available_goal_ids?: string[];
		shared: boolean;
		renderer: MemoryTierRenderer;
		data?: V3MemoryTierRecord;
		consolidation_rules: ConsolidationRule[];
		shared_with_agents: string[];
	}

	interface ApiErrorPayload {
		code?: string;
		error?: string;
		message?: string;
		details?: Record<string, unknown>;
	}

	interface ScoredRow {
		name: string;
		score: number;
		metadata: string;
		last_seen: string;
	}

	interface LogEntry {
		timestamp: string;
		message: string;
		source: string;
	}

	interface EnvironmentKnowledgeEntry {
		environment_key: string;
		kind: string;
		page_type: string;
		layout_notes: string;
		known_blockers: Record<string, string> | Array<{ key: string; value: string }>;
		successful_patterns: string;
		failure_modes: string;
		auth_required: string;
		last_used: string;
		use_count: string;
	}

	const LORE_TIER_SCHEMA_VERSION = 'presto.lore-tier-v1';

	let mounted = false;
	let routeKey = '';
	let isLoading = false;
	let error: string | null = null;
	let agentId = '';
	let tierName = '';
	let selectedGoalId = '';
	let goalOptions: string[] = [];
	let payload: AgentMemoryTierResponse | null = null;
	let lastRefreshAt: number | null = null;
	let latestTierRequestId = 0;
	let currentScopeKey = '';
	let lastScopeKey = '';
	let copiedNotice = '';
	let copyTimeout: ReturnType<typeof setTimeout> | null = null;

	// Environment knowledge layout notes expand/collapse state
	let envKnowledgeExpandedNotes: Set<string> = new Set();

	function clearScopeState(): void {
		latestTierRequestId += 1;
		isLoading = false;
		error = null;
		agentId = '';
		tierName = '';
		selectedGoalId = '';
		goalOptions = [];
		payload = null;
		lastRefreshAt = null;
		envKnowledgeExpandedNotes = new Set();
		copiedNotice = '';
		routeKey = '';
	}

	$: keyValueRows = payload?.data ? Object.entries(payload.data.fields || {}) : [];
	$: scoredRows = extractScoredRows(payload?.data);
	$: logRows = extractLogEntries(payload?.data);
	$: envKnowledgeRows = extractEnvironmentKnowledgeEntries(payload?.data);
	$: components = buildLoreTierSurface({
		agentId,
		tierName,
		selectedGoalId,
		goalOptions,
		payload,
		isLoading,
		error,
		copiedNotice,
		lastRefreshAt,
		keyValueRows,
		scoredRows,
		logRows,
		envKnowledgeRows,
		envKnowledgeExpandedNotes,
		schemaVersion: LORE_TIER_SCHEMA_VERSION
	});

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

	function readString(record: Record<string, unknown>, field: string): string | undefined {
		const value = record[field];
		return typeof value === 'string' && value.trim().length > 0 ? value : undefined;
	}

	function decodeRouteParam(raw: string): string {
		return (raw || '').trim();
	}

	function formatDateTime(value: string | undefined): string {
		if (!value) return '—';
		const parsed = Date.parse(value);
		if (!Number.isFinite(parsed)) return '—';
		return new Date(parsed).toLocaleString();
	}

	function formatRelativeTime(timestamp: number | null): string {
		if (!timestamp) return 'never';
		const diffMs = timestamp - Date.now();
		const diffMinutes = Math.round(diffMs / 60000);
		if (Math.abs(diffMinutes) < 1) return 'just now';
		if (Math.abs(diffMinutes) < 60) {
			return `${Math.abs(diffMinutes)}m ${diffMinutes < 0 ? 'ago' : 'from now'}`;
		}
		const diffHours = Math.round(diffMinutes / 60);
		if (Math.abs(diffHours) < 48) {
			return `${Math.abs(diffHours)}h ${diffHours < 0 ? 'ago' : 'from now'}`;
		}
		const diffDays = Math.round(diffHours / 24);
		return `${Math.abs(diffDays)}d ${diffDays < 0 ? 'ago' : 'from now'}`;
	}

	function formatIsoRelative(isoTimestamp?: string): string {
		if (!isoTimestamp) return 'never';
		const parsed = Date.parse(isoTimestamp);
		if (!Number.isFinite(parsed)) return 'unknown';
		return formatRelativeTime(parsed);
	}

	function formatJson(value: unknown): string {
		return JSON.stringify(value, null, 2);
	}

	function valuePreview(value: unknown): string {
		if (value === null || value === undefined) return 'null';
		if (typeof value === 'string') return value;
		if (typeof value === 'number' || typeof value === 'boolean') return String(value);
		if (Array.isArray(value)) return `${value.length} item${value.length === 1 ? '' : 's'}`;
		const record = asRecord(value);
		if (record) return `${Object.keys(record).length} field(s)`;
		return String(value);
	}

	function parseNumber(value: unknown): number {
		if (typeof value === 'number' && Number.isFinite(value)) return value;
		if (typeof value === 'string') {
			const parsed = Number.parseFloat(value);
			if (Number.isFinite(parsed)) return parsed;
		}
		return 0;
	}

	function normalizeGoalIds(value: unknown): string[] {
		const raw = Array.isArray(value) ? value : [];
		const ids: string[] = [];
		for (const item of raw) {
			if (typeof item !== 'string') continue;
			const normalized = item.trim();
			if (!normalized || ids.includes(normalized)) continue;
			ids.push(normalized);
		}
		return ids;
	}

	function extractScoredRows(data: V3MemoryTierRecord | undefined): ScoredRow[] {
		if (!data) return [];
		const rows: ScoredRow[] = [];
		for (const [field, value] of Object.entries(data.fields || {})) {
			if (!Array.isArray(value)) continue;
			for (let index = 0; index < value.length; index += 1) {
				const item = asRecord(value[index]);
				if (!item || !('score' in item)) continue;
				const name = readString(item, 'name') || readString(item, 'label') || readString(item, 'id') || `${field}-${index + 1}`;
				const score = parseNumber(item.score);
				const lastSeen = readString(item, 'last_seen') || readString(item, 'updated_at') || readString(item, 'timestamp') || '—';
				const metadata = { ...item };
				delete metadata.name;
				delete metadata.label;
				delete metadata.id;
				delete metadata.score;
				delete metadata.last_seen;
				delete metadata.updated_at;
				delete metadata.timestamp;
				rows.push({
					name,
					score,
					last_seen: lastSeen,
					metadata: Object.keys(metadata).length > 0 ? JSON.stringify(metadata) : '—'
				});
			}
		}
		rows.sort((left, right) => right.score - left.score);
		return rows;
	}

	function extractLogEntries(data: V3MemoryTierRecord | undefined): LogEntry[] {
		if (!data) return [];
		const rows: LogEntry[] = [];
		for (const [field, value] of Object.entries(data.fields || {})) {
			if (!Array.isArray(value)) continue;
			for (const item of value) {
				if (typeof item === 'string') {
					rows.push({ timestamp: '', message: item, source: field });
					continue;
				}
				const record = asRecord(item);
				if (!record) continue;
				const message = readString(record, 'message') || readString(record, 'summary') || readString(record, 'text') || valuePreview(record);
				const timestamp =
					readString(record, 'timestamp') ||
					readString(record, 'time') ||
					readString(record, 'at') ||
					readString(record, 'created_at') ||
					'';
				rows.push({
					timestamp,
					message,
					source: field
				});
			}
		}
		rows.sort((left, right) => {
			const leftTime = left.timestamp ? Date.parse(left.timestamp) : Number.NaN;
			const rightTime = right.timestamp ? Date.parse(right.timestamp) : Number.NaN;
			if (!Number.isFinite(leftTime) && !Number.isFinite(rightTime)) return 0;
			if (!Number.isFinite(leftTime)) return 1;
			if (!Number.isFinite(rightTime)) return -1;
			return rightTime - leftTime;
		});
		return rows;
	}

	function extractEnvironmentKnowledgeEntries(data: V3MemoryTierRecord | undefined): EnvironmentKnowledgeEntry[] {
		if (!data) return [];
		const rawEnvironments = Array.isArray(data.fields?.environments)
			? data.fields.environments
			: [];
		const entries: EnvironmentKnowledgeEntry[] = [];
		for (const raw of rawEnvironments) {
			const record = asRecord(raw);
			if (!record) continue;
			const envKey = asString(record.environment_key);
			if (!envKey) continue;
			entries.push({
				environment_key: envKey,
				kind: asString(record.kind) || 'unknown',
				page_type: asString(record.page_type),
				layout_notes: asString(record.layout_notes),
				known_blockers: (record.known_blockers as EnvironmentKnowledgeEntry['known_blockers']) || {},
				successful_patterns: asString(record.successful_patterns),
				failure_modes: asString(record.failure_modes),
				auth_required: asString(record.auth_required) || 'unknown',
				last_used: asString(record.last_used),
				use_count: asString(record.use_count) || '0'
			});
		}
		return entries;
	}

	/** Map environment kind to MUIJ Badge/Tag color */
	function envKindColor(kind: string): 'info' | 'success' | 'warning' | 'error' | 'default' {
		switch (kind) {
			case 'browser':
				return 'info';
			case 'http':
				return 'success';
			case 'bash':
				return 'warning';
			case 'tool':
				return 'error';
			case 'file':
			default:
				return 'default';
		}
	}

	/** Extract known_blockers as flat string array for display */
	function extractBlockerTags(blockers: EnvironmentKnowledgeEntry['known_blockers']): string[] {
		if (Array.isArray(blockers)) {
			return blockers
				.map((b) => {
					const rec = asRecord(b);
					if (rec) {
						const key = asString(rec.key);
						const value = asString(rec.value);
						return key && value ? `${key}: ${value}` : key || value;
					}
					return asString(b);
				})
				.filter((s) => s.length > 0);
		}
		const record = asRecord(blockers);
		if (record) {
			return Object.entries(record)
				.filter(([, v]) => typeof v === 'string' && v.trim().length > 0)
				.map(([k, v]) => `${k}: ${asString(v)}`);
		}
		return [];
	}

	/** Build structured MUIJ components for environment_knowledge tier entries */
	function buildEnvironmentKnowledgeDataComponent(
		entries: EnvironmentKnowledgeEntry[],
		expandedNotes: Set<string>
	): CrewNativeComponent {
		if (entries.length === 0) {
			return {
				id: 'presto-lore-data-envk-empty',
				component_type: 'EmptyState',
				props: {
					title: 'No environment entries',
					description: 'No learned environment data in this tier yet. Run tasks to build knowledge about sites, APIs, and tools.'
				}
			};
		}

		const entryCards: CrewNativeComponent[] = [];
		for (let i = 0; i < entries.length; i++) {
			const entry = entries[i];
			const cardId = `presto-lore-data-envk-entry:${i}`;
			const kindColor = envKindColor(entry.kind);
			const blockerTags = extractBlockerTags(entry.known_blockers);
			const isExpanded = expandedNotes.has(entry.environment_key);

			const cardChildren: CrewNativeComponent[] = [];

			// Kind badge + page_type label row
			const headerBadges: CrewNativeComponent[] = [
				{
					id: `${cardId}:kind-badge`,
					component_type: 'Badge',
					props: { text: entry.kind, color: kindColor }
				}
			];
			if (entry.page_type) {
				headerBadges.push({
					id: `${cardId}:page-type-badge`,
					component_type: 'Badge',
					props: { text: entry.page_type, color: 'default' }
				});
			}
			if (entry.auth_required === 'yes') {
				headerBadges.push({
					id: `${cardId}:auth-badge`,
					component_type: 'Badge',
					props: { text: 'auth required', color: 'warning' }
				});
			}
			cardChildren.push({
				id: `${cardId}:header-badges`,
				component_type: 'Stack',
				props: { direction: 'row', gap: '0.35rem', wrap: true },
				children: headerBadges
			});

			// Successful patterns (highest value content)
			if (entry.successful_patterns) {
				cardChildren.push({
					id: `${cardId}:patterns-section`,
					component_type: 'Card',
					props: {
						title: 'Successful patterns',
						subtitle: '',
						body: entry.successful_patterns
					}
				});
			}

			// Failure modes (highest value content)
			if (entry.failure_modes) {
				cardChildren.push({
					id: `${cardId}:failures-section`,
					component_type: 'Card',
					props: {
						title: 'Failure modes',
						subtitle: '',
						body: entry.failure_modes
					}
				});
			}

			// Known blockers as warning tags
			if (blockerTags.length > 0) {
				cardChildren.push({
					id: `${cardId}:blockers`,
					component_type: 'Stack',
					props: { direction: 'row', gap: '0.3rem', wrap: true },
					children: blockerTags.map((tag, tagIdx) => ({
						id: `${cardId}:blocker-tag:${tagIdx}`,
						component_type: 'Tag',
						props: { text: tag, color: 'warning' }
					}))
				});
			}

			// Metadata row: last_used, use_count
			cardChildren.push({
				id: `${cardId}:metadata`,
				component_type: 'DataList',
				props: {
					items: [
						{
							id: `${cardId}:meta-last-used`,
							key: 'Last used',
							value: formatIsoRelative(entry.last_used || undefined)
						},
						{
							id: `${cardId}:meta-use-count`,
							key: 'Uses',
							value: entry.use_count || '0'
						}
					]
				}
			});

			// Layout notes: collapsed by default, with toggle button
			if (entry.layout_notes) {
				const toggleChildren: CrewNativeComponent[] = [
					{
						id: `${cardId}:layout-toggle`,
						component_type: 'Button',
						label: isExpanded ? 'Hide layout notes' : 'Show layout notes',
						props: {
							interactive: true,
							variant: 'outline',
							size: 'sm'
						}
					}
				];
				if (isExpanded) {
					toggleChildren.push({
						id: `${cardId}:layout-content`,
						component_type: 'CodeBlock',
						label: 'Layout notes',
						props: {
							language: 'text',
							code: entry.layout_notes,
							showLineNumbers: false
						}
					});
				}
				cardChildren.push({
					id: `${cardId}:layout-section`,
					component_type: 'Stack',
					props: { direction: 'column', gap: '0.35rem' },
					children: toggleChildren
				});
			}

			entryCards.push({
				id: cardId,
				component_type: 'Card',
				props: {
					title: entry.environment_key,
					subtitle: '',
					body: ''
				},
				children: cardChildren
			});
		}

		return {
			id: 'presto-lore-data-envk-list',
			component_type: 'Stack',
			props: { direction: 'column', gap: '0.5rem' },
			children: entryCards
		};
	}

	function archiveText(data: V3MemoryTierRecord | undefined): string {
		if (!data) return 'No archived summary yet.';
		const stringFields = Object.values(data.fields || {}).filter((value) => typeof value === 'string');
		if (stringFields.length > 0) {
			return stringFields.join('\n\n');
		}
		return formatJson(data.fields || {});
	}

	async function readApiError(response: Response): Promise<{ message: string; payload: ApiErrorPayload | null }> {
		let message = `Request failed (${response.status})`;
		let payload: ApiErrorPayload | null = null;
		try {
			const text = await response.text();
			if (!text) return { message, payload };
			try {
				const parsed = JSON.parse(text) as unknown;
				payload = asRecord(parsed) as ApiErrorPayload;
				message = `Request failed (${response.status}): ${payload?.error || payload?.message || text}`;
			} catch {
				message = `Request failed (${response.status}): ${text}`;
			}
		} catch {
			// best effort
		}
		return { message, payload };
	}

	function updateGoalQuery(goalId: string): void {
		if (!browser) return;
		const currentGoalId = ($page.url.searchParams.get('goal_id') || '').trim();
		if (currentGoalId === goalId.trim()) return;
		const url = new URL(window.location.href);
		if (goalId) {
			url.searchParams.set('goal_id', goalId);
		} else {
			url.searchParams.delete('goal_id');
		}
		const search = url.searchParams.toString();
		const nextRoute = search ? `${url.pathname}?${search}` : url.pathname;
		void goto(nextRoute, { replaceState: true, noScroll: true, keepFocus: true });
	}

	async function fetchTierDetail(
		nextAgentId: string,
		nextTierName: string,
		goalId: string,
		goalOptionsHint: string[] = [],
		allowRetry = true
	): Promise<{ payload: AgentMemoryTierResponse; goalOptions: string[]; selectedGoalId: string }> {
		const normalizedGoalId = goalId.trim();
		const query = normalizedGoalId ? `?goal_id=${encodeURIComponent(normalizedGoalId)}` : '';
		const response = await timedFetch(`/api/magician/v2/agents/${encodeURIComponent(nextAgentId)}/memory/${encodeURIComponent(nextTierName)}${query}`);
		if (!response.ok) {
			const { message, payload: apiError } = await readApiError(response);
			if (allowRetry && (apiError?.code === 'memory_goal_id_required' || apiError?.code === 'invalid_goal_id')) {
				const details = asRecord(apiError.details);
				const availableGoalIds = normalizeGoalIds(details?.available_goal_ids);
				if (availableGoalIds.length > 0) {
					return fetchTierDetail(nextAgentId, nextTierName, availableGoalIds[0], availableGoalIds, false);
				}
			}
			throw new Error(message);
		}
		const nextPayload = (await response.json()) as AgentMemoryTierResponse;
		const payloadGoalId = (nextPayload.goal_id || '').trim();
		const resolvedGoalId = payloadGoalId || normalizedGoalId;
		const payloadGoalOptions = normalizeGoalIds(nextPayload.available_goal_ids);
		const fallbackGoalOptions = goalOptionsHint.length > 0 ? [...goalOptionsHint] : resolvedGoalId ? [resolvedGoalId] : [];
		let nextGoalOptions = payloadGoalOptions.length > 0 ? payloadGoalOptions : fallbackGoalOptions;
		if (resolvedGoalId && !nextGoalOptions.includes(resolvedGoalId)) {
			nextGoalOptions = [resolvedGoalId, ...nextGoalOptions];
		}
		return { payload: nextPayload, goalOptions: nextGoalOptions, selectedGoalId: resolvedGoalId };
	}

	async function hydrateRoute(): Promise<void> {
		const requestId = ++latestTierRequestId;
		isLoading = true;
		error = null;
		payload = null;
		envKnowledgeExpandedNotes = new Set();
		try {
			const nextAgentId = decodeRouteParam($page.params.id || '');
			const nextTierName = decodeRouteParam($page.params.tier || '');
			if (!nextAgentId || !nextTierName) {
				throw new Error('Missing agent/tier route parameters');
			}
			agentId = nextAgentId;
			tierName = nextTierName;
			const queryGoalId = ($page.url.searchParams.get('goal_id') || '').trim();

			const detail = await fetchTierDetail(nextAgentId, nextTierName, queryGoalId);
			if (requestId !== latestTierRequestId) return;
			goalOptions = detail.goalOptions;
			selectedGoalId = detail.selectedGoalId;
			updateGoalQuery(selectedGoalId);
			payload = {
				agent_id: detail.payload.agent_id,
				goal_id: detail.payload.goal_id,
				available_goal_ids: detail.payload.available_goal_ids || [],
				shared: detail.payload.shared,
				renderer: detail.payload.renderer,
				data: detail.payload.data,
				consolidation_rules: detail.payload.consolidation_rules || [],
				shared_with_agents: detail.payload.shared_with_agents || [],
				tier: detail.payload.tier
			};
			lastRefreshAt = Date.now();
		} catch (err) {
			if (requestId !== latestTierRequestId) return;
			error = err instanceof Error ? err.message : 'Failed to load memory tier';
		} finally {
			if (requestId === latestTierRequestId) {
				isLoading = false;
			}
		}
	}

	async function copyPayload(payloadText: string, label: string): Promise<void> {
		if (!browser || !payloadText) return;
		try {
			await navigator.clipboard.writeText(payloadText);
			copiedNotice = `${label} copied`;
		} catch {
			copiedNotice = 'Copy failed';
		}
		if (copyTimeout) {
			clearTimeout(copyTimeout);
		}
		copyTimeout = setTimeout(() => {
			if (copiedNotice === `${label} copied` || copiedNotice === 'Copy failed') {
				copiedNotice = '';
			}
		}, 1200);
	}

	function buildLoreTierSurface(input: {
		agentId: string;
		tierName: string;
		selectedGoalId: string;
		goalOptions: string[];
		payload: AgentMemoryTierResponse | null;
		isLoading: boolean;
		error: string | null;
		copiedNotice: string;
		lastRefreshAt: number | null;
		keyValueRows: Array<[string, unknown]>;
		scoredRows: ScoredRow[];
		logRows: LogEntry[];
		envKnowledgeRows: EnvironmentKnowledgeEntry[];
		envKnowledgeExpandedNotes: Set<string>;
		schemaVersion: string;
	}): CrewNativeComponent[] {
		const components: CrewNativeComponent[] = [
			{
				id: 'presto-lore-header',
				component_type: 'Card',
				props: {
					title: 'Memory Tier Viewer',
					subtitle: `Tier data renderer · ${input.schemaVersion}`,
					body: 'Type-aware memory rendering with sharing and consolidation context.'
				},
				children: [
					{
						id: 'presto-lore-meta',
						component_type: 'DataList',
						props: {
							items: [
								{ id: 'presto-lore-meta-agent', key: 'Agent', value: input.agentId || '—' },
								{ id: 'presto-lore-meta-tier', key: 'Tier', value: input.tierName || '—' },
								{ id: 'presto-lore-meta-renderer', key: 'Renderer', value: input.payload?.renderer || '—' },
								{ id: 'presto-lore-meta-refresh', key: 'Last refresh', value: formatRelativeTime(input.lastRefreshAt) }
							]
						}
					},
					{
						id: 'presto-lore-actions',
						component_type: 'Stack',
						props: {
							direction: 'row',
							gap: '0.5rem',
							wrap: true
						},
						children: [
							{ id: 'presto-lore-action-back', component_type: 'Button', label: 'Back to memory tab', props: { interactive: true, variant: 'outline', size: 'sm' } },
							{ id: 'presto-lore-action-copy', component_type: 'Button', label: 'Copy payload', props: { interactive: true, variant: 'secondary', size: 'sm', disabled: input.isLoading || !input.payload } },
							{ id: 'presto-lore-action-refresh', component_type: 'Button', label: input.isLoading ? 'Refreshing...' : 'Refresh', props: { interactive: true, variant: 'secondary', size: 'sm', disabled: input.isLoading } }
						]
					}
				]
			}
		];

		if (input.copiedNotice) {
			components.push({
				id: 'presto-lore-copy-notice',
				component_type: 'Alert',
				props: {
					type: input.copiedNotice === 'Copy failed' ? 'error' : 'success',
					message: input.copiedNotice,
					closable: false
				}
			});
		}

		if (input.error) {
			components.push({
				id: 'presto-lore-error',
				component_type: 'Alert',
				props: { type: 'error', message: input.error, closable: false }
			});
		}

		if (input.isLoading && !input.payload) {
			components.push({
				id: 'presto-lore-loading',
				component_type: 'EmptyState',
				props: {
					title: 'Loading tier data',
					description: 'Fetching memory tier definition and persisted data.'
				}
			});
			return components;
		}

		if (!input.payload) {
			components.push({
				id: 'presto-lore-empty',
				component_type: 'EmptyState',
				props: {
					title: 'No tier payload',
					description: 'Tier data was not returned for this route.'
				}
			});
			return components;
		}

		components.push({
			id: 'presto-lore-summary',
			component_type: 'Card',
			props: {
				title: input.payload.tier.name,
				subtitle: input.payload.tier.description || 'No description',
				body: input.payload.shared ? 'Shared tier' : 'Local tier'
			},
			children: [
				{
					id: 'presto-lore-summary-meta',
					component_type: 'DataList',
					props: {
						items: [
							{ id: 'presto-lore-summary-scope', key: 'Scope', value: input.payload.tier.scope },
							{ id: 'presto-lore-summary-format', key: 'Render format', value: input.payload.tier.render.format },
							{ id: 'presto-lore-summary-goal', key: 'Goal', value: input.payload.goal_id || 'n/a' },
							{ id: 'presto-lore-summary-updated', key: 'Last updated', value: formatDateTime(input.payload.data?.last_updated) }
						]
					}
				}
			]
		});

		if (input.goalOptions.length > 1) {
			components.push({
				id: 'presto-lore-goal-form',
				component_type: 'Form',
				label: 'Goal scope',
				props: {
					title: 'Goal scope',
					showSubmit: true,
					submitLabel: 'Load goal',
					disabled: input.isLoading,
					idBase: 'presto-lore-goal-form',
					fields: [
						{
							id: 'goal_id',
							label: 'Goal',
							type: 'select',
							required: true,
							value: input.selectedGoalId,
							options: input.goalOptions.map((goalId) => ({ value: goalId, label: goalId })),
							placeholder: 'goal'
						}
					]
				}
			});
		}

		let dataComponent: CrewNativeComponent;
		if (!input.payload.data) {
			dataComponent = {
				id: 'presto-lore-data-empty',
				component_type: 'EmptyState',
				props: {
					title: 'No persisted tier payload',
					description: 'No data has been persisted for this tier yet.'
				}
			};
		} else if (input.tierName === 'environment_knowledge') {
			// Specialized renderer for environment_knowledge tier
			dataComponent = buildEnvironmentKnowledgeDataComponent(
				input.envKnowledgeRows,
				input.envKnowledgeExpandedNotes
			);
		} else if (input.payload.renderer === 'key_value') {
			dataComponent = {
				id: 'presto-lore-data-key-value',
				component_type: 'Table',
				props: {
					columns: [
						{ key: 'key', label: 'Key' },
						{ key: 'value', label: 'Value' },
						{ key: 'preview', label: 'Preview' }
					],
					rows: input.keyValueRows.map(([key, value]) => ({
						key,
						value: formatJson(value),
						preview: valuePreview(value)
					}))
				}
			};
		} else if (input.payload.renderer === 'scored_list') {
			dataComponent = input.scoredRows.length === 0
				? {
						id: 'presto-lore-data-scored-empty',
						component_type: 'EmptyState',
						props: {
							title: 'No scored-list rows',
							description: 'No scored-list rows detected in this tier payload.'
						}
					}
				: {
						id: 'presto-lore-data-scored',
						component_type: 'Table',
						props: {
							columns: [
								{ key: 'name', label: 'Name' },
								{ key: 'score', label: 'Score', sortable: true },
								{ key: 'last_seen', label: 'Last seen' },
								{ key: 'metadata', label: 'Metadata' }
							],
							rows: input.scoredRows.map((row) => ({
								name: row.name,
								score: row.score.toFixed(3),
								last_seen: row.last_seen,
								metadata: row.metadata
							}))
						}
					};
		} else if (input.payload.renderer === 'append_log') {
			dataComponent = input.logRows.length === 0
				? {
						id: 'presto-lore-data-log-empty',
						component_type: 'EmptyState',
						props: {
							title: 'No append-log entries',
							description: 'No append-log entries detected in this tier payload.'
						}
					}
				: {
						id: 'presto-lore-data-log',
						component_type: 'Table',
						props: {
							columns: [
								{ key: 'source', label: 'Source' },
								{ key: 'timestamp', label: 'Timestamp' },
								{ key: 'message', label: 'Message' }
							],
							rows: input.logRows.map((row) => ({
								source: row.source,
								timestamp: formatDateTime(row.timestamp),
								message: row.message
							}))
						}
					};
		} else {
			dataComponent = {
				id: 'presto-lore-data-archive',
				component_type: 'CodeBlock',
				label: 'Archive',
				props: {
					language: 'text',
					code: archiveText(input.payload.data),
					showLineNumbers: false
				}
			};
		}

		components.push({
			id: 'presto-lore-content-grid',
			component_type: 'Grid',
			props: {
				autoFit: true,
				minColumnWidth: '320px',
				gap: '0.8rem'
			},
			children: [
				{
					id: 'presto-lore-data-card',
					component_type: 'Card',
					props: {
						title: input.tierName === 'environment_knowledge'
							? 'Environment Knowledge'
							: 'Tier data',
						subtitle: input.tierName === 'environment_knowledge'
							? `${input.envKnowledgeRows.length} environment${input.envKnowledgeRows.length !== 1 ? 's' : ''}`
							: `Renderer: ${input.payload.renderer}`,
						body: input.tierName === 'environment_knowledge'
							? 'Learned knowledge about websites, APIs, CLI tools, and environments.'
							: 'Rendered view of current memory tier payload.'
					},
					children: [dataComponent]
				},
				{
					id: 'presto-lore-context-card',
					component_type: 'Card',
					props: {
						title: 'Context',
						subtitle: 'Sharing and consolidation',
						body: 'Tier sharing scope and consolidation rules.'
					},
					children: [
						{
							id: 'presto-lore-sharing-table',
							component_type: 'Table',
							props: {
								columns: [
									{ key: 'field', label: 'Field' },
									{ key: 'value', label: 'Value' }
								],
								rows: [
									{ field: 'Shared', value: input.payload.shared ? 'yes' : 'no' },
									{ field: 'Shared with', value: input.payload.shared_with_agents.length > 0 ? input.payload.shared_with_agents.join(', ') : 'none' }
								]
							}
						},
						{
							id: 'presto-lore-consolidation-table',
							component_type: 'Table',
							props: {
								columns: [
									{ key: 'name', label: 'Rule' },
									{ key: 'source', label: 'Source' },
									{ key: 'target', label: 'Target' },
									{ key: 'trigger', label: 'Trigger' }
								],
								rows: input.payload.consolidation_rules.map((rule) => ({
									name: rule.name,
									source: rule.source,
									target: rule.target,
									trigger: formatJson(rule.trigger)
								}))
							}
						}
					]
				}
			]
		});

		return components;
	}

	async function handleSurfaceInteraction(event: CustomEvent<CrewNativeInteractionEventDetail>): Promise<void> {
		const detail = event?.detail;
		if (!detail) return;

		if (detail.interaction === 'action') {
			if (detail.componentId === 'presto-lore-action-back') {
				await goto(`/crew/${encodeURIComponent(agentId)}?tab=memory`);
				return;
			}
			if (detail.componentId === 'presto-lore-action-copy') {
				if (payload) {
					await copyPayload(formatJson(payload), 'Memory tier payload');
				}
				return;
			}
			if (detail.componentId === 'presto-lore-action-refresh') {
				await hydrateRoute();
				return;
			}

			// Environment knowledge layout notes toggle
			const layoutToggleMatch = /^presto-lore-data-envk-entry:(\d+):layout-toggle$/.exec(
				detail.componentId
			);
			if (layoutToggleMatch && layoutToggleMatch[1] !== undefined) {
				const entryIndex = parseInt(layoutToggleMatch[1], 10);
				const entry = envKnowledgeRows[entryIndex];
				if (entry) {
					const nextExpanded = new Set(envKnowledgeExpandedNotes);
					if (nextExpanded.has(entry.environment_key)) {
						nextExpanded.delete(entry.environment_key);
					} else {
						nextExpanded.add(entry.environment_key);
					}
					envKnowledgeExpandedNotes = nextExpanded;
				}
				return;
			}
		}

		if (detail.interaction === 'submit' && detail.componentId === 'presto-lore-goal-form') {
			const payloadDetail = asRecord(detail.detail);
			const values = payloadDetail ? asRecord(payloadDetail.values) : null;
			const nextGoalId = asString(values?.goal_id).trim();
			if (nextGoalId && nextGoalId !== selectedGoalId) {
				selectedGoalId = nextGoalId;
				updateGoalQuery(nextGoalId);
			}
		}
	}

	onMount(() => {
		mounted = true;
		lastScopeKey = currentScopeKey;
	});

	onDestroy(() => {
		if (copyTimeout) {
			clearTimeout(copyTimeout);
		}
		copyTimeout = null;
	});

	$: currentScopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: if (browser && mounted && currentScopeKey !== lastScopeKey) {
		lastScopeKey = currentScopeKey;
		clearScopeState();
	}
	$: if (browser && mounted) {
		const nextKey = `${currentScopeKey}::${$page.url.pathname}?${$page.url.searchParams.toString()}`;
		if (nextKey !== routeKey) {
			routeKey = nextKey;
			void hydrateRoute();
		}
	}
</script>

<svelte:head>
	<title>Memory · Magican</title>
</svelte:head>

<div class="agent-route presto-gaui-page">
	<NativeCrewRenderer {components} on:interaction={handleSurfaceInteraction} />
</div>
