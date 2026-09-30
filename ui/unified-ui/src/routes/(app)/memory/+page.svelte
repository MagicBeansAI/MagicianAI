<script lang="ts">
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { onMount } from 'svelte';
	import { page } from '$app/stores';
	import MemoryObservabilityDashboard from '$lib/magician/memory/MemoryObservabilityDashboard.svelte';
	import MemoryEntryCard from '$lib/memory/MemoryEntryCard.svelte';
	import type { MemoryEntry, MemoryScopeDraft } from '$lib/memory/memoryEntry';
	import {
		clampPreferencePageIndex,
		fetchPreferenceEntriesPage,
		MEMORY_EFFECT_REVIEW_ENDPOINT,
		memoryEffectAdvanceLabel,
		memoryEffectReviewCanAdvance,
		memoryEffectReviewShowsBanner,
		memoryTabFromQuery,
		preferenceConfirmPath,
		preferenceKeepConflictPath,
		preferencePagerView,
		preferenceScopePath,
		type MemoryEffectReview,
		type MemoryTabKey
	} from '$lib/memory/preferenceEntries';
	import TasteProposalsPanel from '$lib/taste/TasteProposalsPanel.svelte';
	import ServerPager from '$lib/shared/components/ServerPager.svelte';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { timedFetch } from '$lib/shared/fetch';

	interface AgentOption {
		agent_id: string;
		name: string;
	}

	interface MemoryTierSummary {
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

	interface AgentMemorySnapshot {
		agent_id: string;
		agent_name: string;
		tiers: MemoryTierSummary[];
		total_count: number;
	}

	interface PendingTask {
		task_id: string;
		title: string;
		status: string;
		agent_id: string;
	}

	interface SynthesizeResult {
		task_id: string;
		title: string;
		agent_id: string;
		episode_created: boolean;
		consolidation_targets: string[];
		consolidation_error?: string;
	}

	interface UserKnowledgePromotion {
		key: string;
		confidence: number;
		source_type: string;
		target_tier: string;
		rationale: string;
		value: Record<string, unknown>;
		delete_path: string;
	}

	interface UserKnowledgeSkip {
		source_id: string;
		reason: string;
		delete_path: string;
	}

	interface UserKnowledgePage<T> {
		items?: T[];
		total_count?: number;
		limit?: number;
		offset?: number;
	}

	interface MemorySearchMatch {
		kind: string;
		scope: string;
		tier: string;
		semantic_memory_type: string;
		temperature_tier: string;
		key: string;
		value: string;
		score: number;
		score_backend: string;
		confidence?: number | null;
		last_updated?: string;
		goal_id?: string | null;
		source_path?: string | null;
	}

	interface MemorySearchResponse {
		status: string;
		reason?: string;
		query?: string;
		count?: number;
		backend?: string;
		fallback_reason?: string;
		matches?: MemorySearchMatch[];
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

	type TabKey = MemoryTabKey;

	interface TabDef {
		key: TabKey;
		label: string;
	}

	const TABS: TabDef[] = [
		{ key: 'overview', label: 'Overview' },
		{ key: 'user-memory', label: 'User Memory' },
		{ key: 'observability', label: 'Observability' }
	];

	const USER_MEMORY_PROMOTIONS_PAGE_SIZE = 12;
	const USER_MEMORY_SKIPS_PAGE_SIZE = 20;

	let mounted = false;
	let routeKey = '';
	let isLoading = false;
	let error: string | null = null;
	let agents: AgentOption[] = [];
	let agentSnapshots: AgentMemorySnapshot[] = [];
	let selectedAgentId = '';
	let lastRefreshAt: number | null = null;
	let latestRequestId = 0;
	let currentScopeKey = '';
	let lastScopeKey = '';

	// Tab gating: each section's data loads only when its tab first becomes
	// active. This replaces the prior "load everything on mount" behaviour
	// that fanned out to one request per agent (up to 200) plus ~20 SQL
	// queries for the observability dashboard on every hydration.
	let activeTab: TabKey = 'overview';
	let loadedTabs: Set<TabKey> = new Set();
	let agentMemoryLoading = false;
	// Tracks which agents we've actually fetched memory for. Without this
	// the subtitle can't tell `tiers: []` (placeholder before fetch) apart
	// from `tiers: []` (fetched, agent genuinely has no tiers).
	let loadedAgentMemoryIds: Set<string> = new Set();

	// Memory synthesis state
	let pendingTasks: PendingTask[] = [];
	let alreadySynthesized: PendingTask[] = [];
	let selectedTaskIds: string[] = [];
	let synthesizingTaskIds: Set<string> = new Set();
	let synthesizeResults: SynthesizeResult[] = [];
	let synthesizeError: string | null = null;

	// User knowledge state
	let userKnowledgePromotions: UserKnowledgePromotion[] = [];
	let userKnowledgeSkips: UserKnowledgeSkip[] = [];
	let userKnowledgePromotionsTotal = 0;
	let userKnowledgeSkipsTotal = 0;
	let userKnowledgeLoading = false;
	let userKnowledgeError: string | null = null;
	let userKnowledgePromotionsPage = 0;
	let userKnowledgeSkipsPage = 0;
	let preferenceEntries: MemoryEntry[] = [];
	let preferenceError: string | null = null;
	let preferenceTotal = 0;
	let preferencePage = 0;
	let confirmingKey: string | null = null;
	let savingScopeKey: string | null = null;
	let effectReview: MemoryEffectReview | null = null;
	let effectReviewBusy = false;
	let effectReviewError: string | null = null;
	let memoryQuery = '';
	let memoryTierFilter = '';
	let memorySearchBusy = false;
	let memorySearchError: string | null = null;
	let memorySearchTookMs: number | null = null;
	let memorySearchResult: MemorySearchResponse | null = null;

	// Environment knowledge state
	let envKnowledgeEntries: EnvironmentKnowledgeEntry[] = [];
	let envKnowledgeLoading = false;
	let envKnowledgeError: string | null = null;
	let envKnowledgeExpandedNotes: Set<string> = new Set();

	$: selectedSnapshot = agentSnapshots.find((s) => s.agent_id === selectedAgentId) || null;
	$: totalPending = pendingTasks.length;
	$: totalSynthesized = alreadySynthesized.length;
	$: inflightCount = synthesizingTaskIds.size;
	$: availableToSelect = pendingTasks.filter((task) => !synthesizingTaskIds.has(task.task_id));
	$: promotionsPage = clampPageIndex(
		userKnowledgePromotionsPage,
		userKnowledgePromotionsTotal,
		USER_MEMORY_PROMOTIONS_PAGE_SIZE
	);
	$: promotionStart = promotionsPage * USER_MEMORY_PROMOTIONS_PAGE_SIZE;
	$: visiblePromotions = userKnowledgePromotions;
	$: promotionsPageCount = totalPages(
		userKnowledgePromotionsTotal,
		USER_MEMORY_PROMOTIONS_PAGE_SIZE
	);
	$: promotionsStartItem =
		userKnowledgePromotionsTotal === 0 ? 0 : promotionsPage * USER_MEMORY_PROMOTIONS_PAGE_SIZE + 1;
	$: promotionsEndItem = Math.min(
		userKnowledgePromotionsTotal,
		promotionStart + visiblePromotions.length
	);
	$: skipsPage = clampPageIndex(
		userKnowledgeSkipsPage,
		userKnowledgeSkipsTotal,
		USER_MEMORY_SKIPS_PAGE_SIZE
	);
	$: skipStart = skipsPage * USER_MEMORY_SKIPS_PAGE_SIZE;
	$: visibleSkips = userKnowledgeSkips;
	$: skipsPageCount = totalPages(userKnowledgeSkipsTotal, USER_MEMORY_SKIPS_PAGE_SIZE);
	$: skipsStartItem =
		userKnowledgeSkipsTotal === 0 ? 0 : skipsPage * USER_MEMORY_SKIPS_PAGE_SIZE + 1;
	$: skipsEndItem = Math.min(userKnowledgeSkipsTotal, skipStart + visibleSkips.length);
	$: preferencePager = preferencePagerView(
		preferencePage,
		preferenceTotal,
		preferenceEntries.length
	);
	$: preferencePageCount = preferencePager.pageCount;
	$: preferenceStartItem = preferencePager.startItem;
	$: preferenceEndItem = preferencePager.endItem;
	$: hasEnvironmentKnowledgeTier =
		activeTab === 'overview' &&
		Boolean(selectedSnapshot?.tiers.some((tier) => tier.tier_name === 'environment_knowledge'));

	function asRecord(value: unknown): Record<string, unknown> | null {
		return typeof value === 'object' && value !== null && !Array.isArray(value)
			? (value as Record<string, unknown>)
			: null;
	}

	function asString(value: unknown): string {
		if (typeof value === 'string') return value;
		if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint')
			return String(value);
		return '';
	}

	function readString(record: Record<string, unknown>, field: string): string | undefined {
		const value = record[field];
		return typeof value === 'string' && value.trim().length > 0 ? value : undefined;
	}

	function clearScopeState(): void {
		latestRequestId += 1;
		isLoading = false;
		error = null;
		agents = [];
		agentSnapshots = [];
		selectedAgentId = '';
		lastRefreshAt = null;
		pendingTasks = [];
		alreadySynthesized = [];
		selectedTaskIds = [];
		synthesizingTaskIds = new Set();
		synthesizeResults = [];
		synthesizeError = null;
		userKnowledgePromotions = [];
		userKnowledgeSkips = [];
		userKnowledgePromotionsTotal = 0;
		userKnowledgeSkipsTotal = 0;
		userKnowledgeLoading = false;
		userKnowledgeError = null;
		userKnowledgePromotionsPage = 0;
		userKnowledgeSkipsPage = 0;
		preferenceEntries = [];
		preferenceError = null;
		preferenceTotal = 0;
		preferencePage = 0;
		confirmingKey = null;
		savingScopeKey = null;
		effectReview = null;
		effectReviewBusy = false;
		effectReviewError = null;
		envKnowledgeEntries = [];
		envKnowledgeLoading = false;
		envKnowledgeError = null;
		envKnowledgeExpandedNotes = new Set();
		agentMemoryLoading = false;
		loadedTabs = new Set();
		loadedAgentMemoryIds = new Set();
		routeKey = '';
	}

	async function readApiError(response: Response): Promise<string> {
		let message = `Request failed (${response.status})`;
		try {
			const text = await response.text();
			if (!text) return message;
			try {
				const parsed = JSON.parse(text) as unknown;
				const root = asRecord(parsed);
				const nested = root ? asRecord(root.details) : null;
				const rootMessage = root
					? readString(root, 'error') || readString(root, 'message')
					: undefined;
				const nestedMessage = nested ? readString(nested, 'reason') : undefined;
				message = `Request failed (${response.status}): ${nestedMessage || rootMessage || text}`;
			} catch {
				message = `Request failed (${response.status}): ${text}`;
			}
		} catch {
			// best effort
		}
		return message;
	}

	function formatRelativeTime(timestamp: number | null): string {
		if (!timestamp) return 'never';
		const diffMs = timestamp - Date.now();
		const diffMinutes = Math.round(diffMs / 60000);
		if (Math.abs(diffMinutes) < 1) return 'just now';
		if (Math.abs(diffMinutes) < 60)
			return `${Math.abs(diffMinutes)}m ${diffMinutes < 0 ? 'ago' : 'from now'}`;
		const diffHours = Math.round(diffMinutes / 60);
		if (Math.abs(diffHours) < 48)
			return `${Math.abs(diffHours)}h ${diffHours < 0 ? 'ago' : 'from now'}`;
		const diffDays = Math.round(diffHours / 24);
		return `${Math.abs(diffDays)}d ${diffDays < 0 ? 'ago' : 'from now'}`;
	}

	function formatIsoRelative(isoTimestamp?: string): string {
		if (!isoTimestamp) return 'never';
		const parsed = Date.parse(isoTimestamp);
		if (!Number.isFinite(parsed)) return 'unknown';
		return formatRelativeTime(parsed);
	}

	function totalPages(total: number, pageSize: number): number {
		return Math.max(1, Math.ceil(Math.max(0, total) / pageSize));
	}

	function clampPageIndex(page: number, total: number, pageSize: number): number {
		return Math.max(0, Math.min(page, totalPages(total, pageSize) - 1));
	}

	function scopeColor(scope: string): string {
		switch (scope) {
			case 'agent':
				return 'info';
			case 'agent_goal':
				return 'warning';
			case 'user':
				return 'success';
			default:
				return 'default';
		}
	}

	/** Map environment kind to the native badge color variants. */
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

	function extractAgentOptions(payload: unknown): AgentOption[] {
		const root = asRecord(payload);
		const rawAgents = Array.isArray(root?.agents) ? root.agents : [];
		const options: AgentOption[] = [];
		for (const entry of rawAgents) {
			const record = asRecord(entry);
			const definition = record ? asRecord(record.definition) : null;
			if (!definition) continue;
			const agentId = readString(definition, 'agent_id');
			if (!agentId) continue;
			options.push({
				agent_id: agentId,
				name: readString(definition, 'name') || agentId
			});
		}
		return options.sort((a, b) => a.name.localeCompare(b.name));
	}

	async function loadAgents(): Promise<AgentOption[]> {
		const response = await timedFetch('/api/magician/v2/agents?limit=200');
		if (!response.ok) throw new Error(await readApiError(response));
		return extractAgentOptions(await response.json());
	}

	async function loadMemoryForAgent(agentId: string): Promise<AgentMemorySnapshot | null> {
		const response = await timedFetch(
			`/api/magician/v2/agents/${encodeURIComponent(agentId)}/memory`
		);
		if (!response.ok) {
			if (response.status === 404) return null;
			return null; // silently skip agents whose memory endpoint fails
		}
		const payload = (await response.json()) as {
			agent_id: string;
			tiers: MemoryTierSummary[];
			total_count: number;
		};
		return {
			agent_id: payload.agent_id || agentId,
			agent_name: agentId,
			tiers: Array.isArray(payload.tiers) ? payload.tiers : [],
			total_count: typeof payload.total_count === 'number' ? payload.total_count : 0
		};
	}

	/** Load environment_knowledge tier data for a given agent */
	async function loadEnvironmentKnowledge(agentId: string): Promise<void> {
		envKnowledgeEntries = [];
		envKnowledgeError = null;
		envKnowledgeExpandedNotes = new Set();

		// Check if the selected agent has the environment_knowledge tier with data
		const snapshot = agentSnapshots.find((s) => s.agent_id === agentId);
		const envTier = snapshot?.tiers.find(
			(t) => t.tier_name === 'environment_knowledge' && t.has_data
		);
		if (!envTier) return;

		envKnowledgeLoading = true;
		try {
			const response = await timedFetch(
				`/api/magician/v2/agents/${encodeURIComponent(agentId)}/memory/${encodeURIComponent('environment_knowledge')}`
			);
			if (!response.ok) {
				envKnowledgeError = await readApiError(response);
				return;
			}
			const payload = (await response.json()) as {
				data?: { fields: Record<string, unknown> };
			};
			const fields = payload?.data?.fields;
			if (!fields) return;

			// The tier schema stores entries in an "environments" collection
			const rawEnvironments = Array.isArray(fields.environments)
				? fields.environments
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
			envKnowledgeEntries = entries;
		} catch (err) {
			envKnowledgeError = err instanceof Error ? err.message : 'Failed to load environment knowledge';
		} finally {
			envKnowledgeLoading = false;
		}
	}

	async function loadUserKnowledge(): Promise<void> {
		userKnowledgePromotions = [];
		userKnowledgeSkips = [];
		userKnowledgeError = null;
		userKnowledgeLoading = true;
		try {
			const params = new URLSearchParams({
				promotions_limit: String(USER_MEMORY_PROMOTIONS_PAGE_SIZE),
				promotions_offset: String(userKnowledgePromotionsPage * USER_MEMORY_PROMOTIONS_PAGE_SIZE),
				skips_limit: String(USER_MEMORY_SKIPS_PAGE_SIZE),
				skips_offset: String(userKnowledgeSkipsPage * USER_MEMORY_SKIPS_PAGE_SIZE)
			});
			const response = await timedFetch(`/api/magician/v2/memory/user-knowledge?${params}`);
			if (!response.ok) {
				userKnowledgeError = await readApiError(response);
				return;
			}
			const payload = (await response.json()) as {
				promotions?: UserKnowledgePage<UserKnowledgePromotion>;
				skips?: UserKnowledgePage<UserKnowledgeSkip>;
			};
			const promotions = payload.promotions;
			const skips = payload.skips;
			userKnowledgePromotions = Array.isArray(promotions?.items) ? promotions.items : [];
			userKnowledgeSkips = Array.isArray(skips?.items) ? skips.items : [];
			userKnowledgePromotionsTotal =
				typeof promotions?.total_count === 'number' ? promotions.total_count : 0;
			userKnowledgeSkipsTotal = typeof skips?.total_count === 'number' ? skips.total_count : 0;

			const nextPromotionsPage = clampPageIndex(
				userKnowledgePromotionsPage,
				userKnowledgePromotionsTotal,
				USER_MEMORY_PROMOTIONS_PAGE_SIZE
			);
			const nextSkipsPage = clampPageIndex(
				userKnowledgeSkipsPage,
				userKnowledgeSkipsTotal,
				USER_MEMORY_SKIPS_PAGE_SIZE
			);
			if (
				nextPromotionsPage !== userKnowledgePromotionsPage ||
				nextSkipsPage !== userKnowledgeSkipsPage
			) {
				userKnowledgePromotionsPage = nextPromotionsPage;
				userKnowledgeSkipsPage = nextSkipsPage;
				await loadUserKnowledge();
			}
		} catch (err) {
			userKnowledgeError = err instanceof Error ? err.message : 'Failed to load user knowledge';
		} finally {
			userKnowledgeLoading = false;
		}
	}

	async function loadPreferenceEntries(): Promise<void> {
		preferenceError = null;
		try {
			const result = await fetchPreferenceEntriesPage(preferencePage, {
				fetchImpl: timedFetch,
				readError: readApiError
			});
			preferenceEntries = result.entries;
			preferenceTotal = result.total;
			preferencePage = result.page;
		} catch (err) {
			preferenceError = err instanceof Error ? err.message : 'Failed to load memory entries';
			preferenceEntries = [];
			preferenceTotal = 0;
		}
	}

	async function loadEffectReview(): Promise<void> {
		effectReviewError = null;
		try {
			const response = await timedFetch(MEMORY_EFFECT_REVIEW_ENDPOINT);
			if (!response.ok) {
				effectReview = null;
				return;
			}
			const payload = (await response.json()) as MemoryEffectReview;
			effectReview = payload;
		} catch {
			effectReview = null;
		}
	}

	async function decideEffectReview(decision: 'advance' | 'stay'): Promise<void> {
		if (effectReviewBusy) return;
		effectReviewBusy = true;
		effectReviewError = null;
		try {
			const response = await timedFetch(MEMORY_EFFECT_REVIEW_ENDPOINT, {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ decision })
			});
			if (!response.ok) {
				effectReviewError = await readApiError(response);
				return;
			}
			effectReview = (await response.json()) as MemoryEffectReview;
		} catch (err) {
			effectReviewError = err instanceof Error ? err.message : 'Failed to apply memory-effect review';
		} finally {
			effectReviewBusy = false;
		}
	}

	async function confirmPreference(entry: MemoryEntry): Promise<void> {
		confirmingKey = `${entry.tier}/${entry.key}`;
		try {
			const response = await timedFetch(preferenceConfirmPath(entry.tier, entry.key), {
				method: 'POST'
			});
			if (!response.ok) {
				preferenceError = await readApiError(response);
				return;
			}
			await loadPreferenceEntries();
		} catch (err) {
			preferenceError = err instanceof Error ? err.message : 'Failed to confirm memory';
		} finally {
			confirmingKey = null;
		}
	}

	async function keepPreferenceConflict(entry: MemoryEntry): Promise<void> {
		try {
			const response = await timedFetch(preferenceKeepConflictPath(entry.tier, entry.key), {
				method: 'POST'
			});
			if (!response.ok) {
				preferenceError = await readApiError(response);
			}
		} catch (err) {
			preferenceError = err instanceof Error ? err.message : 'Failed to keep memory';
		}
	}

	async function savePreferenceScope(
		entry: MemoryEntry,
		scope: MemoryScopeDraft | null
	): Promise<void> {
		savingScopeKey = `${entry.tier}/${entry.key}`;
		try {
			const response = await timedFetch(preferenceScopePath(entry.tier, entry.key), {
				method: 'PATCH',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ scope })
			});
			if (!response.ok) {
				preferenceError = await readApiError(response);
				return;
			}
			await loadPreferenceEntries();
		} catch (err) {
			preferenceError = err instanceof Error ? err.message : 'Failed to save memory scope';
		} finally {
			savingScopeKey = null;
		}
	}

	async function setPreferencePage(page: number): Promise<void> {
		preferencePage = clampPreferencePageIndex(page, preferenceTotal);
		await loadPreferenceEntries();
	}

	async function deleteUserKnowledgeEntry(keyPath: string): Promise<void> {
		try {
			const response = await timedFetch(
				`/api/magician/v2/memory/user-knowledge/${encodeURIComponent(keyPath)}`,
				{ method: 'DELETE' }
			);
			if (!response.ok) {
				userKnowledgeError = await readApiError(response);
				return;
			}
			// Reload after deletion
			await loadUserKnowledge();
		} catch (err) {
			userKnowledgeError = err instanceof Error ? err.message : 'Failed to delete entry';
		}
	}

	async function loadPendingTasks(): Promise<void> {
		try {
			const response = await timedFetch('/api/magician/v2/memory/pending-tasks');
			if (!response.ok) return;
			const data = (await response.json()) as {
				pending: PendingTask[];
				already_synthesized: PendingTask[];
				synthesizing?: string[];
			};
			pendingTasks = Array.isArray(data.pending) ? data.pending : [];
			alreadySynthesized = Array.isArray(data.already_synthesized)
				? data.already_synthesized
				: [];
			// Merge server-side in-flight tasks into local tracking set
			if (Array.isArray(data.synthesizing) && data.synthesizing.length > 0) {
				synthesizingTaskIds = new Set([
					...synthesizingTaskIds,
					...data.synthesizing
				]);
			}
			// Clear stale selections
			const validIds = new Set(pendingTasks.map((t) => t.task_id));
			selectedTaskIds = selectedTaskIds.filter((id) => validIds.has(id));
		} catch {
			// best effort — don't break the page
		}
	}

	async function synthesizeSelectedTasks(): Promise<void> {
		// Filter out tasks already being synthesized
		const batchIds = selectedTaskIds.filter((id) => !synthesizingTaskIds.has(id));
		if (batchIds.length === 0) return;

		// Mark these tasks as in-flight and clear selection
		synthesizingTaskIds = new Set([...synthesizingTaskIds, ...batchIds]);
		selectedTaskIds = selectedTaskIds.filter((id) => !batchIds.includes(id));

		try {
			const response = await timedFetch('/api/magician/v2/memory/synthesize', {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ task_ids: batchIds })
			});
			if (!response.ok) {
				synthesizeError = await readApiError(response);
				return;
			}
			const data = (await response.json()) as {
				results: SynthesizeResult[];
				batch_consolidation: unknown;
			};
			const batchResults = Array.isArray(data.results) ? data.results : [];
			synthesizeResults = [...synthesizeResults, ...batchResults];
			synthesizeError = null;
			// Refresh pending tasks list and memory snapshots (hydrateRoute calls loadPendingTasks internally)
			await hydrateRoute();
		} catch (err) {
			synthesizeError = err instanceof Error ? err.message : 'Synthesis failed';
		} finally {
			// Remove completed batch from in-flight set
			const remaining = new Set(synthesizingTaskIds);
			for (const id of batchIds) remaining.delete(id);
			synthesizingTaskIds = remaining;
		}
	}

	async function searchMemory(): Promise<void> {
		const query = memoryQuery.trim();
		if (!query || !selectedAgentId || memorySearchBusy) return;
		memorySearchBusy = true;
		memorySearchError = null;
		const started = performance.now();
		try {
			const response = await timedFetch('/api/magician/v2/memory/search', {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				timeoutMs: 60_000,
				body: JSON.stringify({
					query,
					agent_id: selectedAgentId,
					tier: memoryTierFilter.trim() || undefined,
					limit: 20
				})
			});
			const data = (await response.json()) as MemorySearchResponse;
			memorySearchTookMs = Math.round(performance.now() - started);
			if (!response.ok || data.status === 'error') {
				memorySearchResult = null;
				memorySearchError = data.reason || `Memory search failed (${response.status})`;
				return;
			}
			memorySearchResult = data;
		} catch (err) {
			memorySearchTookMs = Math.round(performance.now() - started);
			memorySearchResult = null;
			memorySearchError = err instanceof Error ? err.message : 'Memory search failed';
		} finally {
			memorySearchBusy = false;
		}
	}

	function clearMemorySearch(): void {
		memoryQuery = '';
		memorySearchResult = null;
		memorySearchError = null;
		memorySearchTookMs = null;
	}

	function memoryTabHref(tab: TabKey): string {
		const url = new URL($page.url);
		if (tab === 'overview') {
			url.searchParams.delete('tab');
		} else {
			url.searchParams.set('tab', tab);
		}
		return `${url.pathname}${url.search}${url.hash}`;
	}

	async function hydrateRoute(): Promise<void> {
		const requestId = ++latestRequestId;
		isLoading = true;
		error = null;
		// A fresh hydrate (manual refresh or scope/route change) treats all
		// tabs as stale so re-opening them re-fetches instead of showing
		// data from a different scope.
		loadedTabs = new Set();
		activeTab = memoryTabFromQuery($page.url.searchParams.get('tab'));

		try {
			void loadEffectReview();
			const nextAgents = await loadAgents();
			if (requestId !== latestRequestId) return;
			agents = nextAgents;

			// Seed lightweight snapshots (one per agent) so the agents tab
			// can list them without firing per-agent memory requests. Tier
			// data fills in lazily when the user picks an agent.
			agentSnapshots = nextAgents.map((agent) => ({
				agent_id: agent.agent_id,
				agent_name: agent.name,
				tiers: [],
				total_count: 0
			}));

			// Honour ?agent_id= from the URL; otherwise just take the first.
			// No "first with data" preference here — that required walking
			// every agent's memory, which is what we're trying to avoid.
			const queryAgentId = ($page.url.searchParams.get('agent_id') || '').trim();
			if (queryAgentId && agentSnapshots.some((s) => s.agent_id === queryAgentId)) {
				selectedAgentId = queryAgentId;
			} else if (!selectedAgentId || !agentSnapshots.some((s) => s.agent_id === selectedAgentId)) {
				selectedAgentId = agentSnapshots[0]?.agent_id || '';
			}
			lastRefreshAt = Date.now();
			await ensureTabLoaded(activeTab);
		} catch (err) {
			if (requestId !== latestRequestId) return;
			error = err instanceof Error ? err.message : 'Failed to load memory data';
		} finally {
			if (requestId === latestRequestId) {
				isLoading = false;
			}
		}
	}

	async function ensureTabLoaded(tab: TabKey): Promise<void> {
		if (loadedTabs.has(tab)) return;
		// Mark as loaded BEFORE awaiting so rapid tab switches don't queue
		// duplicate loads. Failures inside the per-tab loader set their
		// own error fields; we don't roll back the marker.
		loadedTabs = new Set([...loadedTabs, tab]);
		switch (tab) {
			case 'overview':
				// Overview now hosts both Synthesize and Agents inline, so
				// it triggers both loaders in parallel. Selected-agent
				// memory still loads on demand only; other agents stay
				// placeholders until the user picks them.
				await Promise.all([
					loadPendingTasks(),
					selectedAgentId ? loadMemoryForSelectedAgent(selectedAgentId) : Promise.resolve()
				]);
				return;
			case 'user-memory':
				await Promise.all([loadUserKnowledge(), loadPreferenceEntries()]);
				return;
			case 'observability':
				// The dashboard's heavy SQL queries fire when its component
				// mounts; the {#if} guard in markup handles that. Nothing
				// to do here.
				return;
		}
	}

	async function setActiveTab(tab: TabKey): Promise<void> {
		if (tab !== activeTab) {
			activeTab = tab;
			await goto(memoryTabHref(tab), {
				replaceState: true,
				keepFocus: true,
				noScroll: true
			});
		}
		await ensureTabLoaded(tab);
	}

	async function loadMemoryForSelectedAgent(agentId: string): Promise<void> {
		agentMemoryLoading = true;
		try {
			const snap = await loadMemoryForAgent(agentId);
			// Mark as loaded even if `snap` is null (404 / endpoint error
			// for this agent) so the UI shows the final state instead of
			// the "tiers load on select" placeholder forever.
			loadedAgentMemoryIds = new Set([...loadedAgentMemoryIds, agentId]);
			if (!snap) return;
			// Splice the loaded snapshot into agentSnapshots so the rest of
			// the surface (which reads from agentSnapshots) sees the new
			// tiers. Other agents keep their empty placeholders until the
			// user picks them.
			const nameMap = new Map(agents.map((a) => [a.agent_id, a.name]));
			snap.agent_name = nameMap.get(agentId) || agentId;
			const next = agentSnapshots.map((s) => (s.agent_id === agentId ? snap : s));
			if (!next.some((s) => s.agent_id === agentId)) next.push(snap);
			agentSnapshots = next;
			await loadEnvironmentKnowledge(agentId);
		} finally {
			agentMemoryLoading = false;
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

	function agentTierSummary(snapshot: AgentMemorySnapshot): string {
		const tiersWithData = snapshot.tiers.filter((tier) => tier.has_data).length;
		if (snapshot.tiers.length > 0) {
			return `${snapshot.tiers.length} tier${snapshot.tiers.length !== 1 ? 's' : ''} · ${tiersWithData} with data`;
		}
		if (loadedAgentMemoryIds.has(snapshot.agent_id)) return 'no memory tiers';
		return 'tiers load on select';
	}

	function promotionTitle(promo: UserKnowledgePromotion): string {
		return typeof promo.value?.name === 'string' ? promo.value.name : promo.key;
	}

	function promotionBody(promo: UserKnowledgePromotion): string {
		return typeof promo.value?.description === 'string' ? promo.value.description : promo.rationale;
	}

	function agentDisplayName(agentId: string): string {
		return agents.find((agent) => agent.agent_id === agentId)?.name || agentId;
	}

	function toggleTaskSelection(taskId: string, checked: boolean): void {
		if (checked) {
			selectedTaskIds = selectedTaskIds.includes(taskId)
				? selectedTaskIds
				: [...selectedTaskIds, taskId];
			return;
		}
		selectedTaskIds = selectedTaskIds.filter((id) => id !== taskId);
	}

	function handleTaskCheckboxChange(taskId: string, event: Event): void {
		const input = event.currentTarget as HTMLInputElement | null;
		toggleTaskSelection(taskId, Boolean(input?.checked));
	}

	function selectAllPendingTasks(): void {
		selectedTaskIds = availableToSelect.map((task) => task.task_id);
	}

	async function selectAgent(agentId: string): Promise<void> {
		selectedAgentId = agentId;
		await loadMemoryForSelectedAgent(agentId);
	}

	async function openAgentDetail(agentId: string): Promise<void> {
		await goto(`/crew/${encodeURIComponent(agentId)}?tab=memory`);
	}

	async function openTier(agentId: string, tierName: string): Promise<void> {
		await goto(`/crew/${encodeURIComponent(agentId)}/memory/${encodeURIComponent(tierName)}`);
	}

	async function setPromotionsPage(page: number): Promise<void> {
		userKnowledgePromotionsPage = clampPageIndex(
			page,
			userKnowledgePromotionsTotal,
			USER_MEMORY_PROMOTIONS_PAGE_SIZE
		);
		await loadUserKnowledge();
	}

	async function setSkipsPage(page: number): Promise<void> {
		userKnowledgeSkipsPage = clampPageIndex(
			page,
			userKnowledgeSkipsTotal,
			USER_MEMORY_SKIPS_PAGE_SIZE
		);
		await loadUserKnowledge();
	}

	async function deletePromotion(promo: UserKnowledgePromotion): Promise<void> {
		if (!promo.delete_path) return;
		if (userKnowledgePromotions.length === 1 && userKnowledgePromotionsPage > 0) {
			userKnowledgePromotionsPage -= 1;
		}
		await deleteUserKnowledgeEntry(promo.delete_path);
	}

	function toggleEnvironmentNotes(entry: EnvironmentKnowledgeEntry): void {
		const nextExpanded = new Set(envKnowledgeExpandedNotes);
		if (nextExpanded.has(entry.environment_key)) {
			nextExpanded.delete(entry.environment_key);
		} else {
			nextExpanded.add(entry.environment_key);
		}
		envKnowledgeExpandedNotes = nextExpanded;
	}

	onMount(() => {
		mounted = true;
		lastScopeKey = currentScopeKey;
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

<div class="agent-route memory-page presto-gaui-page">
	<section class="memory-hero panel">
		<div class="hero-copy">
			<p class="eyebrow">Memory overview</p>
			<h1>Memory</h1>
			<p class="hero-description">
				Browse memory tiers across all agents. Select an agent to inspect stored knowledge.
			</p>
		</div>
		<div class="hero-side">
			<dl class="metric-list">
				<div>
					<dt>Agents</dt>
					<dd>{agentSnapshots.length}</dd>
				</div>
				<div>
					<dt>Last refresh</dt>
					<dd>{formatRelativeTime(lastRefreshAt)}</dd>
				</div>
			</dl>
			<button class="btn btn--secondary" type="button" disabled={isLoading} on:click={() => void hydrateRoute()}>
				{isLoading ? 'Refreshing...' : 'Refresh'}
			</button>
		</div>
	</section>

	{#if memoryEffectReviewShowsBanner(effectReview) && effectReview}
		<section class="alert alert--info effect-review-banner" role="status">
			<p>
				<strong>Memory-effect rollout ({effectReview.effective_mode})</strong>
				— {effectReview.reason}
			</p>
			<p>{effectReview.next_step}</p>
			{#if effectReview.pending_hitl}
				<p>The same choice is waiting on the Attention bar.</p>
			{/if}
			{#if effectReviewError}
				<p class="effect-review-error">{effectReviewError}</p>
			{/if}
			{#if memoryEffectReviewCanAdvance(effectReview.advice)}
				<div class="button-row">
					<button
						class="btn btn--primary"
						type="button"
						disabled={effectReviewBusy}
						on:click={() => void decideEffectReview('advance')}
					>
						{memoryEffectAdvanceLabel(effectReview.advice)}
					</button>
					<button
						class="btn btn--outline"
						type="button"
						disabled={effectReviewBusy}
						on:click={() => void decideEffectReview('stay')}
					>
						Stay in {effectReview.effective_mode}
					</button>
				</div>
			{/if}
		</section>
	{/if}

	<section class="panel memory-search-panel">
		<div class="section-head">
			<div>
				<h2>Search memory</h2>
				<p>
					{#if selectedSnapshot}
						Same recall as search_memory for {selectedSnapshot.agent_name}. A lookup here is not counted as a use.
					{:else}
						Select an agent, then search the memory that agent would recall.
					{/if}
				</p>
			</div>
		</div>
		<form class="memory-search-row" on:submit|preventDefault={() => void searchMemory()}>
			<label class="memory-search-field">
				<span class="visually-hidden">Search memory</span>
				<input
					type="search"
					bind:value={memoryQuery}
					placeholder="Search memory"
					autocomplete="off"
					enterkeyhint="search"
					disabled={!selectedAgentId || memorySearchBusy}
				/>
				{#if memoryQuery}
					<button class="memory-search-clear" type="button" aria-label="Clear search" on:click={clearMemorySearch}>
						×
					</button>
				{/if}
			</label>
			<label class="memory-tier-field">
				<span class="visually-hidden">Tier</span>
				<input
					type="text"
					bind:value={memoryTierFilter}
					placeholder="Tier, or episodes"
					autocomplete="off"
					disabled={!selectedAgentId || memorySearchBusy}
				/>
			</label>
			<button class="btn btn--primary" type="submit" disabled={!selectedAgentId || memorySearchBusy || !memoryQuery.trim()}>
				{memorySearchBusy ? 'Searching…' : 'Search'}
			</button>
		</form>
		{#if memorySearchError}
			<p class="alert alert--error" role="alert">{memorySearchError}</p>
		{/if}
		{#if memorySearchResult}
			<p class="memory-search-status">
				{memorySearchResult.count ?? memorySearchResult.matches?.length ?? 0} result{(memorySearchResult.count ?? 0) === 1 ? '' : 's'}
				{#if memorySearchResult.backend} · {memorySearchResult.backend}{/if}
				{#if memorySearchTookMs != null} · {memorySearchTookMs} ms{/if}
				{#if memorySearchResult.fallback_reason}
					· {memorySearchResult.fallback_reason}
				{/if}
			</p>
			{#if (memorySearchResult.matches?.length ?? 0) === 0}
				<div class="empty-state">
					<h3>No matching memory</h3>
					<p>Nothing in this agent's memory answered that search.</p>
				</div>
			{:else}
				<ol class="memory-search-hits">
					{#each memorySearchResult.matches ?? [] as match, index (`${match.scope}:${match.tier}:${match.key}:${index}`)}
						<li>
							<div class="memory-hit-meta">
								<span class={`badge badge--${scopeColor(match.scope)}`}>{match.scope}</span>
								<span class="badge">{match.tier}</span>
								<span class="badge">{match.semantic_memory_type}</span>
								{#if match.temperature_tier}
									<span class="badge">{match.temperature_tier}</span>
								{/if}
								<span class="memory-hit-score">
									{match.score.toFixed(2)} · {match.score_backend}
								</span>
							</div>
							<p class="memory-hit-key">
								{match.key}
								{#if match.goal_id}<span> · {match.goal_id}</span>{/if}
							</p>
							<p class="memory-hit-value">{match.value}</p>
							{#if match.last_updated || match.source_path}
								<p class="memory-hit-source">
									{#if match.last_updated}{formatIsoRelative(match.last_updated)}{/if}
									{#if match.source_path}<span>{match.source_path}</span>{/if}
								</p>
							{/if}
						</li>
					{/each}
				</ol>
			{/if}
		{/if}
	</section>

	<nav class="memory-tab-strip" aria-label="Memory sections">
		{#each TABS as tab (tab.key)}
			<button
				type="button"
				class="memory-tab-button"
				class:active={activeTab === tab.key}
				aria-current={activeTab === tab.key ? 'page' : undefined}
				on:click={() => void setActiveTab(tab.key)}
			>
				{tab.label}
			</button>
		{/each}
	</nav>

	{#if error}
		<div class="alert alert--error" role="alert">{error}</div>
	{/if}

	{#if activeTab === 'overview'}
		<!-- Taste proposals sit above synthesis: they are the only thing here
		     waiting on a decision, and a queue below the fold is a queue that
		     stops being read. -->
		<section class="panel">
			<TasteProposalsPanel />
		</section>

		<section class="panel synthesis-panel">
			<div class="section-head">
				<div>
					<h2>Synthesize Memory</h2>
					<p>{totalPending} pending · {totalSynthesized} synthesized</p>
				</div>
				<div class="button-row">
					<button
						class="btn btn--primary"
						type="button"
						disabled={selectedTaskIds.length === 0}
						on:click={() => void synthesizeSelectedTasks()}
					>
						{inflightCount > 0
							? `Synthesize ${selectedTaskIds.length > 0 ? selectedTaskIds.length : ''} more`
							: `Synthesize ${selectedTaskIds.length > 0 ? selectedTaskIds.length : ''} task${selectedTaskIds.length !== 1 ? 's' : ''}`}
					</button>
					<button
						class="btn btn--outline"
						type="button"
						disabled={availableToSelect.length === 0}
						on:click={selectAllPendingTasks}
					>
						Select all
					</button>
				</div>
			</div>
			<p class="section-description">
				Extract episodic memory from completed tasks into entities, insights, and other tiers.
			</p>

			{#if totalPending > 0}
				<div class="task-select-list" aria-label="Tasks to synthesize">
					<div class="list-caption">
						{#if inflightCount > 0}
							{totalPending} pending · {inflightCount} in progress
						{:else}
							{totalPending} pending
						{/if}
					</div>
					{#each pendingTasks as task (task.task_id)}
						<label class="task-option" class:disabled={synthesizingTaskIds.has(task.task_id)}>
							<input
								class="task-checkbox"
								type="checkbox"
								disabled={synthesizingTaskIds.has(task.task_id)}
								checked={selectedTaskIds.includes(task.task_id)}
								on:change={(event) => handleTaskCheckboxChange(task.task_id, event)}
							/>
							<span class="task-option-copy">
								<strong>{task.title}</strong>
								<small>
									{#if synthesizingTaskIds.has(task.task_id)}
										Synthesizing...
									{:else}
										{task.status}
									{/if}
								</small>
							</span>
							<span class="task-agent-name" title={task.agent_id}>{agentDisplayName(task.agent_id)}</span>
						</label>
					{/each}
				</div>
			{:else}
				<div class="empty-state">
					<h3>All caught up</h3>
					<p>
						{totalSynthesized > 0
							? `All ${totalSynthesized} completed task${totalSynthesized !== 1 ? 's have' : ' has'} been synthesized into memory.`
							: 'No completed tasks found to synthesize.'}
					</p>
				</div>
			{/if}

			{#if synthesizeError}
				<div class="alert alert--error" role="alert">{synthesizeError}</div>
			{/if}

			{#if synthesizeResults.length > 0}
				<div class="table-wrap">
					<table>
						<thead>
							<tr>
								<th>Task</th>
								<th>Episode</th>
								<th>Tiers updated</th>
								<th>Error</th>
							</tr>
						</thead>
						<tbody>
							{#each synthesizeResults as result, index (`${result.task_id}:${index}`)}
								<tr>
									<td>{result.title}</td>
									<td>{result.episode_created ? 'Created' : 'Existed'}</td>
									<td>{result.consolidation_targets.length > 0 ? result.consolidation_targets.join(', ') : 'none'}</td>
									<td>{result.consolidation_error || ''}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			{/if}
		</section>

		{#if isLoading && agentSnapshots.length === 0}
			<div class="empty-state panel">
				<h3>Loading memory data</h3>
				<p>Fetching agent registry.</p>
			</div>
		{:else if agentSnapshots.length === 0}
			<div class="empty-state panel">
				<h3>No memory data</h3>
				<p>No agents have memory tiers configured. Create an agent with autonomous config to enable memory.</p>
				<button class="btn btn--primary" type="button" on:click={() => void goto('/crew/new')}>Create agent</button>
			</div>
		{:else}
			<section class="memory-grid">
				<aside class="panel agent-panel">
					<div class="section-head compact">
						<div>
							<h2>Agents</h2>
							<p>{agentSnapshots.length} agent{agentSnapshots.length !== 1 ? 's' : ''}</p>
						</div>
					</div>
					<div class="agent-list">
						{#each agentSnapshots as snapshot (snapshot.agent_id)}
							<button
								type="button"
								class="agent-row"
								class:selected={snapshot.agent_id === selectedAgentId}
								on:click={() => void selectAgent(snapshot.agent_id)}
							>
								<span class="agent-name">{snapshot.agent_name}</span>
								<span class="agent-meta">{snapshot.agent_id} · {agentTierSummary(snapshot)}</span>
							</button>
						{/each}
					</div>
				</aside>

				<section class="panel detail-panel">
					<div class="section-head">
						<div>
							<h2>{selectedSnapshot ? `${selectedSnapshot.agent_name} - Memory` : 'Memory detail'}</h2>
							<p>
								{#if agentMemoryLoading}
									loading...
								{:else if selectedSnapshot}
									{selectedSnapshot.tiers.length} tier{selectedSnapshot.tiers.length !== 1 ? 's' : ''}
								{:else}
									none selected
								{/if}
							</p>
						</div>
						{#if selectedSnapshot}
							<button class="btn btn--secondary" type="button" on:click={() => void openAgentDetail(selectedSnapshot.agent_id)}>
								Open in Crew
							</button>
						{/if}
					</div>

					{#if agentMemoryLoading}
						<p class="section-description">Fetching memory tiers for the selected agent.</p>
					{/if}

					{#if selectedSnapshot}
						{#if selectedSnapshot.tiers.length === 0}
							<div class="empty-state">
								<h3>No memory tiers</h3>
								<p>This agent does not have memory tiers configured.</p>
							</div>
						{:else}
							<div class="table-wrap">
								<table>
									<thead>
										<tr>
											<th>Tier</th>
											<th>Scope</th>
											<th>Data</th>
											<th>Renderer</th>
											<th>Shared</th>
											<th>Updated</th>
										</tr>
									</thead>
									<tbody>
										{#each selectedSnapshot.tiers as tier (tier.tier_name)}
											<tr>
												<td>{tier.tier_name}</td>
												<td><span class={`badge badge--${scopeColor(tier.scope)}`}>{tier.scope}</span></td>
												<td>{tier.has_data ? 'Yes' : 'No'}</td>
												<td>{tier.renderer || tier.render_format || '-'}</td>
												<td>{tier.shared ? 'Yes' : 'No'}</td>
												<td>{formatIsoRelative(tier.last_updated)}</td>
											</tr>
										{/each}
									</tbody>
								</table>
							</div>
							<div class="button-row tier-actions">
								{#each selectedSnapshot.tiers.filter((tier) => tier.has_data) as tier (tier.tier_name)}
									<button class="btn btn--outline" type="button" on:click={() => void openTier(selectedSnapshot!.agent_id, tier.tier_name)}>
										Open {tier.tier_name}
									</button>
								{/each}
							</div>
						{/if}
					{:else}
						<div class="empty-state">
							<h3>No agent selected</h3>
							<p>Select an agent to browse its memory tiers.</p>
						</div>
					{/if}
				</section>
			</section>
		{/if}

		{#if hasEnvironmentKnowledgeTier}
			<section class="panel env-panel">
				<div class="section-head">
					<div>
						<h2>Environment Knowledge</h2>
						<p>
							{#if envKnowledgeEntries.length > 0}
								{envKnowledgeEntries.length} environment{envKnowledgeEntries.length !== 1 ? 's' : ''} learned
							{:else}
								Learned knowledge about sites, APIs, and tools
							{/if}
						</p>
					</div>
				</div>
				<p class="section-description">
					Persistent memory about websites, HTTP APIs, CLI tools, and file system patterns discovered during execution.
				</p>

				{#if envKnowledgeLoading}
					<div class="empty-state">
						<h3>Loading environment knowledge</h3>
						<p>Fetching learned environment data.</p>
					</div>
				{/if}
				{#if envKnowledgeError}
					<div class="alert alert--error" role="alert">{envKnowledgeError}</div>
				{/if}
				{#if !envKnowledgeLoading && envKnowledgeEntries.length === 0}
					<div class="empty-state">
						<h3>No environment knowledge</h3>
						<p>No learned environment data yet. Run tasks to build knowledge about sites, APIs, and tools.</p>
					</div>
				{:else if envKnowledgeEntries.length > 0}
					<div class="env-grid">
						{#each envKnowledgeEntries as entry (entry.environment_key)}
							{@const blockers = extractBlockerTags(entry.known_blockers)}
							{@const notesExpanded = envKnowledgeExpandedNotes.has(entry.environment_key)}
							<article class="sub-card env-card">
								<header>
									<h3>{entry.environment_key}</h3>
									<div class="badge-row">
										<span class={`badge badge--${envKindColor(entry.kind)}`}>{entry.kind}</span>
										{#if entry.page_type}<span class="badge">{entry.page_type}</span>{/if}
										{#if entry.auth_required === 'yes'}<span class="badge badge--warning">auth required</span>{/if}
									</div>
								</header>
								{#if entry.successful_patterns}
									<section class="note-block">
										<h4>Successful patterns</h4>
										<p>{entry.successful_patterns}</p>
									</section>
								{/if}
								{#if entry.failure_modes}
									<section class="note-block">
										<h4>Failure modes</h4>
										<p>{entry.failure_modes}</p>
									</section>
								{/if}
								{#if blockers.length > 0}
									<div class="tag-row">
										{#each blockers as blocker (blocker)}
											<span class="tag tag--warning">{blocker}</span>
										{/each}
									</div>
								{/if}
								<dl class="inline-facts">
									<div><dt>Last used</dt><dd>{formatIsoRelative(entry.last_used || undefined)}</dd></div>
									<div><dt>Uses</dt><dd>{entry.use_count || '0'}</dd></div>
								</dl>
								{#if entry.layout_notes}
									<button class="btn btn--outline" type="button" on:click={() => toggleEnvironmentNotes(entry)}>
										{notesExpanded ? 'Hide layout notes' : 'Show layout notes'}
									</button>
									{#if notesExpanded}
										<pre class="code-block">{entry.layout_notes}</pre>
									{/if}
								{/if}
							</article>
						{/each}
					</div>
				{/if}
			</section>
		{/if}
	{:else if activeTab === 'user-memory'}
		<section class="panel user-memory-panel">
			<div class="section-head">
				<div>
					<h2>User Memory</h2>
					<p>
						{#if userKnowledgePromotionsTotal > 0 || userKnowledgeSkipsTotal > 0}
							{userKnowledgePromotionsTotal} promoted · {userKnowledgeSkipsTotal} skipped
						{:else}
							Cross-agent promoted knowledge
						{/if}
					</p>
				</div>
			</div>
			<p class="section-description">
				Insights promoted from agent memory to user-level persistent storage. These persist across all agents and sessions.
			</p>

			<section class="memory-section">
				<div class="subsection-head">
					<h3>Preferences</h3>
					<span>{preferenceTotal} entries</span>
				</div>
				<p class="section-description">
					Confirm an inferred preference to make it stated. Scope decides what it can attach to;
					without topics or entities it stays inert. Untrusted observations cannot become rules.
				</p>
				{#if preferenceError}
					<div class="alert alert--error" role="alert">{preferenceError}</div>
				{/if}
				{#if preferencePageCount > 1}
					<ServerPager
						currentPage={preferencePage + 1}
						pageCount={preferencePageCount}
						startItem={preferenceStartItem}
						endItem={preferenceEndItem}
						totalItems={preferenceTotal}
						ariaLabel="User memory entries pagination"
						on:pagechange={(event) => void setPreferencePage(event.detail.page - 1)}
					/>
				{/if}
				{#each preferenceEntries as entry (entry.tier + ':' + entry.key)}
					<MemoryEntryCard
						{entry}
						confirming={confirmingKey === `${entry.tier}/${entry.key}`}
						savingScope={savingScopeKey === `${entry.tier}/${entry.key}`}
						onConfirm={confirmPreference}
						onKeepConflict={keepPreferenceConflict}
						onSaveScope={savePreferenceScope}
					/>
				{/each}
			</section>

			{#if userKnowledgeLoading}
				<div class="empty-state">
					<h3>Loading user memory</h3>
					<p>Fetching promoted knowledge.</p>
				</div>
			{:else if userKnowledgeError}
				<div class="alert alert--error" role="alert">{userKnowledgeError}</div>
			{:else if userKnowledgePromotionsTotal === 0 && userKnowledgeSkipsTotal === 0}
				<div class="empty-state">
					<h3>No user memory</h3>
					<p>No insights have been promoted to user-level memory yet. This happens automatically as agents accumulate high-confidence insights.</p>
				</div>
			{:else}
				{#if userKnowledgePromotionsTotal > 0}
					<section class="memory-section">
						<div class="subsection-head">
							<h3>Promoted</h3>
							<span>{userKnowledgePromotionsTotal} total</span>
						</div>
						{#if promotionsPageCount > 1}
							<ServerPager
								currentPage={promotionsPage + 1}
								pageCount={promotionsPageCount}
								startItem={promotionsStartItem}
								endItem={promotionsEndItem}
								totalItems={userKnowledgePromotionsTotal}
								loading={userKnowledgeLoading}
								ariaLabel="Promoted user memory pagination"
								on:pagechange={(event) => void setPromotionsPage(event.detail.page - 1)}
							/>
						{/if}
						<div class="table-wrap">
							<table class="memory-table">
								<thead>
									<tr>
										<th>Memory</th>
										<th>Tier</th>
										<th>Confidence</th>
										<th>Source</th>
										<th class="memory-table__actions">Action</th>
									</tr>
								</thead>
								<tbody>
									{#each visiblePromotions as promo, index (`${promo.delete_path}:${promotionStart + index}`)}
										<tr>
											<td class="memory-table__main">
												<span class="memory-table__title">{promotionTitle(promo)}</span>
												<span class="memory-table__body">{promotionBody(promo)}</span>
											</td>
											<td><span class="badge badge--success">{promo.target_tier}</span></td>
											<td><span class={`badge badge--${promo.confidence >= 0.8 ? 'success' : 'warning'}`}>{Math.round(promo.confidence * 100)}%</span></td>
											<td><span class="badge badge--info">{promo.source_type}</span></td>
											<td class="memory-table__actions">
												<button class="btn btn--outline btn--small" type="button" on:click={() => void deletePromotion(promo)}>Remove</button>
											</td>
										</tr>
									{/each}
								</tbody>
							</table>
						</div>
						{#if promotionsPageCount > 1}
							<ServerPager
								currentPage={promotionsPage + 1}
								pageCount={promotionsPageCount}
								startItem={promotionsStartItem}
								endItem={promotionsEndItem}
								totalItems={userKnowledgePromotionsTotal}
								loading={userKnowledgeLoading}
								ariaLabel="Promoted user memory pagination"
								on:pagechange={(event) => void setPromotionsPage(event.detail.page - 1)}
							/>
						{/if}
					</section>
				{/if}

				{#if userKnowledgeSkipsTotal > 0}
					<section class="memory-section">
						<div class="subsection-head">
							<h3>Skipped</h3>
							<span>{userKnowledgeSkipsTotal} total</span>
						</div>
						{#if skipsPageCount > 1}
							<ServerPager
								currentPage={skipsPage + 1}
								pageCount={skipsPageCount}
								startItem={skipsStartItem}
								endItem={skipsEndItem}
								totalItems={userKnowledgeSkipsTotal}
								loading={userKnowledgeLoading}
								ariaLabel="Skipped user memory pagination"
								on:pagechange={(event) => void setSkipsPage(event.detail.page - 1)}
							/>
						{/if}
						<div class="table-wrap">
							<table>
								<thead>
									<tr><th>Source</th><th>Reason</th></tr>
								</thead>
								<tbody>
									{#each visibleSkips as skip (`${skip.source_id}:${skip.reason}`)}
										<tr><td>{skip.source_id}</td><td>{skip.reason}</td></tr>
									{/each}
								</tbody>
							</table>
						</div>
						{#if skipsPageCount > 1}
							<ServerPager
								currentPage={skipsPage + 1}
								pageCount={skipsPageCount}
								startItem={skipsStartItem}
								endItem={skipsEndItem}
								totalItems={userKnowledgeSkipsTotal}
								loading={userKnowledgeLoading}
								ariaLabel="Skipped user memory pagination"
								on:pagechange={(event) => void setSkipsPage(event.detail.page - 1)}
							/>
						{/if}
					</section>
				{/if}
			{/if}
		</section>
	{:else if activeTab === 'observability'}
		<MemoryObservabilityDashboard />
	{/if}
</div>

<style>
	.memory-page {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		box-sizing: border-box;
		width: 100%;
		max-width: var(--app-content-max, 1320px);
		margin: 0 auto;
		padding: 1.35rem 1.45rem 5rem;
		color: var(--text-primary, #2d3436);
	}

	.panel,
	.sub-card {
		box-sizing: border-box;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		background: var(--bg-card, #fff);
		box-shadow: var(--shadow-sm, 0 1px 3px rgba(0, 0, 0, 0.08));
	}

	.panel {
		border-radius: 8px;
		padding: 1rem;
	}

	.sub-card {
		border-radius: 6px;
		padding: 0.85rem;
	}

	.memory-hero {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		gap: 1rem;
		align-items: start;
		width: 100%;
	}

	.eyebrow,
	.section-head p,
	.section-description,
	.agent-meta,
	.list-caption,
	.task-option small,
	.subsection-head span,
	.inline-facts dt {
		color: var(--text-secondary, #5f6668);
	}

	.eyebrow {
		margin: 0 0 0.25rem;
		font-size: var(--text-2xs, 0.72rem);
		font-weight: 700;
		text-transform: uppercase;
	}

	h1,
	h2,
	h3,
	h4,
	p,
	dl {
		margin: 0;
	}

	h1 {
		font: 750 var(--text-xl, 1.35rem) var(--font-display, inherit);
	}

	h2 {
		font: 700 var(--text-lg, 1.1rem) var(--font-display, inherit);
	}

	h3 {
		font: 700 var(--text-md, 0.95rem) var(--font-display, inherit);
	}

	h4 {
		font: 700 var(--text-sm, 0.85rem) var(--font-display, inherit);
	}

	.hero-description,
	.section-description {
		margin-top: 0.35rem;
		line-height: var(--leading-normal, 1.5);
	}

	.hero-side,
	.button-row,
	.badge-row,
	.tag-row {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
		align-items: center;
	}

	.hero-side {
		justify-content: flex-end;
	}

	.metric-list,
	.inline-facts {
		display: flex;
		gap: 0.75rem;
	}

	.metric-list div,
	.inline-facts div {
		display: grid;
		gap: 0.1rem;
	}

	.metric-list dt,
	.inline-facts dt {
		font-size: var(--text-2xs, 0.72rem);
		font-weight: 700;
		text-transform: uppercase;
	}

	.metric-list dd,
	.inline-facts dd {
		font-size: var(--text-sm, 0.85rem);
		font-weight: 700;
	}

	.memory-search-row {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
		align-items: center;
		margin-top: 0.75rem;
	}

	.memory-search-field,
	.memory-tier-field {
		position: relative;
		display: flex;
		align-items: center;
		min-width: 0;
	}

	.memory-search-field {
		flex: 1 1 16rem;
	}

	.memory-tier-field {
		flex: 0 1 12rem;
	}

	.memory-search-field input,
	.memory-tier-field input {
		box-sizing: border-box;
		width: 100%;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		border-radius: 8px;
		background: var(--bg-soft, #f6f1e8);
		color: var(--text-primary, #2d3436);
		font: 500 var(--text-sm, 0.9rem) var(--font-primary, inherit);
		padding: 0.55rem 0.75rem;
	}

	.memory-search-field input {
		padding-right: 2rem;
	}

	.memory-search-clear {
		position: absolute;
		right: 0.35rem;
		border: 0;
		background: transparent;
		color: var(--text-secondary, #5f6668);
		font-size: 1.1rem;
		line-height: 1;
		cursor: pointer;
	}

	.memory-search-status,
	.memory-hit-source,
	.memory-hit-score {
		color: var(--text-secondary, #5f6668);
		font-size: var(--text-xs, 0.78rem);
	}

	.memory-search-status {
		margin-top: 0.75rem;
	}

	.memory-search-hits {
		display: grid;
		gap: 0.75rem;
		margin: 0.75rem 0 0;
		padding: 0;
		list-style: none;
	}

	.memory-search-hits li {
		display: grid;
		gap: 0.35rem;
		border-top: 1px solid var(--border-subtle, rgba(0, 0, 0, 0.08));
		padding-top: 0.75rem;
	}

	.memory-hit-meta {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
		align-items: center;
	}

	.memory-hit-key {
		font-weight: 700;
	}

	.memory-hit-key span,
	.memory-hit-source span {
		font-weight: 500;
		color: var(--text-secondary, #5f6668);
	}

	.memory-hit-value {
		max-height: 8rem;
		overflow: auto;
		white-space: pre-wrap;
		line-height: var(--leading-normal, 1.5);
	}

	.memory-hit-source {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
	}

	.visually-hidden {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
	}

	.memory-tab-strip {
		display: flex;
		flex-wrap: wrap;
		gap: 0.25rem;
		box-sizing: border-box;
		width: 100%;
		border-bottom: 1px solid var(--border-subtle, rgba(0, 0, 0, 0.08));
		padding-bottom: 0.25rem;
	}

	.memory-tab-button,
	.btn {
		appearance: none;
		border-radius: 6px;
		cursor: pointer;
		font: 700 var(--text-xs, 0.78rem) var(--font-primary, inherit);
		transition: background 120ms ease, border-color 120ms ease, color 120ms ease;
	}

	.memory-tab-button {
		background: transparent;
		border: 1px solid transparent;
		color: var(--text-secondary, #5f6668);
		padding: 0.4rem 0.85rem;
	}

	.memory-tab-button:hover {
		background: var(--surface-hover, rgba(0, 0, 0, 0.04));
		color: var(--text-primary, #2d3436);
	}

	.memory-tab-button.active {
		background: var(--bg-elevated, #fff);
		border-color: var(--border-soft, rgba(0, 0, 0, 0.1));
		color: var(--text-primary, #2d3436);
	}

	.btn {
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		background: var(--bg-card, #fff);
		color: var(--text-primary, #2d3436);
		padding: 0.45rem 0.7rem;
	}

	.btn:hover:not(:disabled) {
		border-color: var(--accent-primary, #ff6b6b);
	}

	.btn:disabled {
		cursor: not-allowed;
		opacity: 0.55;
	}

	.btn--primary {
		background: var(--accent-primary, #ff6b6b);
		border-color: var(--accent-primary, #ff6b6b);
		color: var(--button-primary-color, #fff);
	}

	.btn--secondary {
		background: color-mix(in srgb, var(--accent-primary, #ff6b6b) 12%, var(--bg-card, #fff));
	}

	.btn--outline {
		background: transparent;
	}

	.btn--small {
		padding: 0.3rem 0.55rem;
	}

	.section-head,
	.subsection-head {
		display: flex;
		justify-content: space-between;
		gap: 1rem;
		align-items: flex-start;
	}

	.section-head.compact {
		margin-bottom: 0.5rem;
	}

	.synthesis-panel,
	.user-memory-panel,
	.env-panel {
		display: grid;
		gap: 0.85rem;
		width: 100%;
	}

	.task-select-list,
	.agent-list,
	.env-grid {
		display: grid;
		gap: 0.55rem;
	}

	.task-option {
		display: grid;
		grid-template-columns: 1rem minmax(0, 1fr) minmax(8rem, auto);
		gap: 0.75rem;
		align-items: center;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		border-radius: 6px;
		padding: 0.6rem;
		background: color-mix(in srgb, var(--bg-card, #fff) 88%, var(--bg-soft, #f6f1e8));
	}

	.task-option:hover {
		border-color: color-mix(in srgb, var(--accent-primary, #ff6b6b) 42%, var(--border-soft, rgba(0, 0, 0, 0.08)));
	}

	.task-option.disabled {
		opacity: 0.7;
	}

	.task-checkbox {
		appearance: none;
		display: grid;
		place-content: center;
		width: 1rem;
		height: 1rem;
		margin: 0;
		border: 1.5px solid var(--border-default, color-mix(in srgb, currentColor 32%, transparent));
		border-radius: 5px;
		background:
			linear-gradient(
				180deg,
				color-mix(in srgb, var(--bg-card, #fff) 96%, transparent),
				color-mix(in srgb, var(--bg-soft, #f8fafc) 92%, transparent)
			);
		color: var(--text-on-accent, #fff);
		box-shadow:
			inset 0 1px 0 color-mix(in srgb, #fff 40%, transparent),
			0 1px 2px color-mix(in srgb, #000 10%, transparent);
		cursor: pointer;
		transition:
			background 120ms ease,
			border-color 120ms ease,
			box-shadow 120ms ease,
			transform 120ms ease;
	}

	.task-checkbox::before {
		content: '';
		width: 0.32rem;
		height: 0.56rem;
		border-right: 2px solid currentColor;
		border-bottom: 2px solid currentColor;
		transform: rotate(42deg) scale(0);
		transform-origin: center;
		transition: transform 120ms ease;
	}

	.task-checkbox:hover:not(:disabled) {
		border-color: var(--accent-primary, currentColor);
		box-shadow:
			0 0 0 3px color-mix(in srgb, var(--accent-primary, currentColor) 14%, transparent),
			inset 0 1px 0 color-mix(in srgb, #fff 36%, transparent);
	}

	.task-checkbox:checked {
		background: linear-gradient(
			135deg,
			var(--accent-primary, #ff6b6b),
			color-mix(in srgb, var(--accent-primary, #ff6b6b) 72%, var(--accent-secondary, #4d9de0))
		);
		border-color: var(--accent-primary, #ff6b6b);
		box-shadow:
			0 0 0 3px color-mix(in srgb, var(--accent-primary, #ff6b6b) 18%, transparent),
			0 2px 8px color-mix(in srgb, var(--accent-primary, #ff6b6b) 24%, transparent);
	}

	.task-checkbox:checked::before {
		transform: rotate(42deg) scale(1);
	}

	.task-checkbox:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 55%, transparent);
		outline-offset: 2px;
	}

	.task-checkbox:disabled {
		cursor: not-allowed;
		opacity: 0.55;
	}

	.task-option-copy {
		display: grid;
		gap: 0.15rem;
		min-width: 0;
	}

	.task-option-copy strong,
	.task-option-copy small,
	.task-agent-name {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.task-agent-name {
		justify-self: end;
		max-width: 16rem;
		border-radius: 999px;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		background: color-mix(in srgb, var(--accent-primary, #ff6b6b) 9%, var(--bg-card, #fff));
		color: var(--text-secondary, #5f6668);
		font-size: var(--text-2xs, 0.72rem);
		font-weight: 700;
		line-height: 1;
		padding: 0.3rem 0.5rem;
	}

	.memory-grid {
		display: grid;
		box-sizing: border-box;
		width: 100%;
		grid-template-columns: minmax(260px, 0.35fr) minmax(0, 1fr);
		gap: 1rem;
		align-items: start;
	}

	.agent-panel {
		position: sticky;
		top: 0.75rem;
		max-height: clamp(420px, 80vh, 900px);
		display: flex;
		flex-direction: column;
		min-width: 0;
	}

	.agent-list {
		overflow-y: auto;
		padding-right: 0.25rem;
	}

	.agent-row {
		display: grid;
		gap: 0.2rem;
		width: 100%;
		border: 1px solid transparent;
		border-radius: 6px;
		background: var(--bg-card, #fff);
		color: inherit;
		cursor: pointer;
		padding: 0.55rem 0.65rem;
		text-align: left;
	}

	.agent-row:hover,
	.agent-row.selected {
		border-color: var(--accent-primary, #ff6b6b);
		background: color-mix(in srgb, var(--accent-primary, #ff6b6b) 10%, var(--bg-card, #fff));
	}

	.agent-name {
		font-size: var(--text-sm, 0.85rem);
		font-weight: 700;
	}

	.agent-meta {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: var(--text-2xs, 0.72rem);
	}

	.detail-panel {
		display: grid;
		gap: 0.85rem;
		min-width: 0;
	}

	.table-wrap {
		overflow-x: auto;
	}

	table {
		width: 100%;
		border-collapse: collapse;
		font-size: var(--text-xs, 0.78rem);
	}

	.memory-table {
		min-width: 820px;
	}

	th,
	td {
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		padding: 0.55rem 0.45rem;
		text-align: left;
		vertical-align: top;
	}

	th {
		color: var(--text-secondary, #5f6668);
		font-size: var(--text-2xs, 0.72rem);
		text-transform: uppercase;
	}

	.memory-table__main {
		min-width: 20rem;
		width: 52%;
	}

	.memory-table__title,
	.memory-table__body {
		display: block;
		overflow-wrap: anywhere;
	}

	.memory-table__title {
		margin-bottom: 0.2rem;
		color: var(--text-primary, #2d3436);
		font-weight: 700;
	}

	.memory-table__body {
		color: var(--text-secondary, #5f6668);
		line-height: var(--leading-normal, 1.5);
	}

	.memory-table__actions {
		text-align: right;
		white-space: nowrap;
	}

	.badge,
	.tag {
		display: inline-flex;
		align-items: center;
		border-radius: 999px;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		background: var(--bg-soft, #f6f1e8);
		color: var(--text-primary, #2d3436);
		font-size: var(--text-2xs, 0.72rem);
		font-weight: 700;
		line-height: 1;
		padding: 0.25rem 0.45rem;
	}

	.badge--info {
		background: var(--color-info-soft, rgba(77, 157, 224, 0.14));
		color: var(--color-info, #4d9de0);
	}

	.badge--warning,
	.tag--warning {
		background: var(--color-warning-soft, rgba(255, 230, 109, 0.22));
		color: color-mix(in srgb, var(--color-warning, #ffe66d) 55%, var(--text-primary, #2d3436));
	}

	.badge--success {
		background: var(--color-success-soft, rgba(0, 187, 127, 0.14));
		color: var(--color-success, #00bb7f);
	}

	.badge--error {
		background: var(--color-error-soft, rgba(255, 107, 107, 0.14));
		color: var(--color-error, #ff6b6b);
	}

	.empty-state {
		display: grid;
		gap: 0.35rem;
		border: 1px dashed var(--border-soft, rgba(0, 0, 0, 0.1));
		border-radius: 6px;
		padding: 1rem;
		color: var(--text-secondary, #5f6668);
	}

	.alert {
		border-radius: 6px;
		padding: 0.75rem;
		font-size: var(--text-sm, 0.85rem);
	}

	.alert--error {
		border: 1px solid var(--color-error, #ff6b6b);
		background: var(--color-error-soft, rgba(255, 107, 107, 0.14));
		color: var(--text-primary, #2d3436);
	}

	.alert--info {
		border: 1px solid var(--color-info, #4c8dff);
		background: var(--color-info-soft, rgba(76, 141, 255, 0.12));
		color: var(--text-primary, #2d3436);
	}

	.effect-review-banner {
		display: grid;
		gap: 0.4rem;
	}

	.effect-review-error {
		color: var(--color-error, #ff6b6b);
	}

	.env-grid {
		grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
	}

	.env-card,
	.memory-section {
		display: grid;
		gap: 0.7rem;
	}

	.env-card header {
		display: flex;
		justify-content: space-between;
		gap: 0.75rem;
		align-items: flex-start;
	}

	.note-block {
		display: grid;
		gap: 0.25rem;
		border-radius: 6px;
		background: var(--bg-soft, #f6f1e8);
		padding: 0.65rem;
	}

	.note-block p {
		line-height: var(--leading-normal, 1.5);
		color: var(--text-secondary, #5f6668);
	}

	.code-block {
		overflow-x: auto;
		border-radius: 6px;
		background: var(--bg-soft, #f6f1e8);
		padding: 0.75rem;
		font: var(--text-xs, 0.78rem) var(--font-mono, monospace);
		white-space: pre-wrap;
	}

	@media (max-width: 900px) {
		.memory-page {
			padding: 0.75rem 0.75rem 5rem;
		}

		.memory-hero,
		.memory-grid,
		.memory-search-row {
			grid-template-columns: 1fr;
		}

		.memory-search-field,
		.memory-tier-field,
		.memory-search-row .btn {
			flex: 1 1 100%;
		}

		.hero-side {
			justify-content: flex-start;
		}

		.agent-panel {
			position: static;
			max-height: none;
		}

		.agent-list {
			max-height: 360px;
		}

		.task-option {
			grid-template-columns: 1rem minmax(0, 1fr);
		}

		.task-agent-name {
			grid-column: 2;
			justify-self: start;
		}
	}
</style>
