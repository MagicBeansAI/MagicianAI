<script lang="ts">
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { onMount } from 'svelte';
	import { page } from '$app/stores';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import ServerPager from '$lib/shared/components/ServerPager.svelte';
	import { timedFetch } from '$lib/shared/fetch';
	import Icon from '$lib/shared/icons/Icon.svelte';

	interface AgentOption {
		agent_id: string;
		name: string;
		roleLabel: string;
	}

	interface TriggerEvent {
		trigger_type: string;
		goal_id: string;
		trigger_seq: number;
		timestamp: string;
	}

	interface V3EpisodeRecordView {
		episode_id: string;
		agent_id: string;
		goal_id?: string;
		goal_key?: string;
		trigger_seq: number;
		trigger?: TriggerEvent;
		trigger_type?: string;
		trigger_timestamp?: string;
		started_at: string;
		completed_at: string;
		outcome?: unknown;
		outcome_kind?: string;
		outcome_summary?: string;
		actions_taken: unknown[];
		observations: string[];
		memory_updates: unknown[];
	}

	interface MemoryUpdateEntry {
		tier_name: string;
		scope?: string;
		entity_count?: number;
		insight_count?: number;
		summary?: string;
		operation?: string;
	}

	interface EpisodesResponse {
		episodes: V3EpisodeRecordView[];
		total_count: number;
	}

	const EPISODES_PAGE_LIMIT_DEFAULT = 25;
	const EPISODES_PAGE_LIMIT_OPTIONS = [25, 50, 100, 250] as const;

	let mounted = false;
	let routeKey = '';
	let isLoadingAgents = false;
	let isLoadingEpisodes = false;
	let error: string | null = null;
	let agents: AgentOption[] = [];
	let selectedAgentId = '';
	let episodes: V3EpisodeRecordView[] = [];
	let totalEpisodeCount = 0;
	let episodeOffset = 0;
	let episodePageLimit = EPISODES_PAGE_LIMIT_DEFAULT;
	let selectedEpisodeId = '';
	let detailPanelOpen = false;
	let lastRefreshAt: number | null = null;
	let latestEpisodesRequestId = 0;
	let currentScopeKey = '';
	let lastScopeKey = '';

	$: totalPages = Math.max(1, Math.ceil(totalEpisodeCount / episodePageLimit));
	$: currentPage = totalEpisodeCount === 0 ? 1 : Math.floor(episodeOffset / episodePageLimit) + 1;
	$: pageStart = totalEpisodeCount === 0 ? 0 : episodeOffset + 1;
	$: pageEnd = totalEpisodeCount === 0 ? 0 : Math.min(totalEpisodeCount, episodeOffset + episodes.length);
	$: selectedEpisode = episodes.find((episode) => episode.episode_id === selectedEpisodeId) || null;
	$: if (!selectedEpisode && detailPanelOpen) {
		detailPanelOpen = false;
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

	function readString(record: Record<string, unknown>, field: string): string | undefined {
		const value = record[field];
		return typeof value === 'string' && value.trim().length > 0 ? value : undefined;
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
				const rootMessage = root ? readString(root, 'error') || readString(root, 'message') : undefined;
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

	function parseDateMs(value: string | undefined): number | null {
		if (!value) return null;
		const parsed = Date.parse(value);
		return Number.isFinite(parsed) ? parsed : null;
	}

	function formatDateTime(value: string | undefined): string {
		const parsed = parseDateMs(value);
		if (!parsed) return '—';
		return new Date(parsed).toLocaleString();
	}

	function formatDurationMs(startedAt: string | undefined, completedAt: string | undefined): string {
		const start = parseDateMs(startedAt);
		const end = parseDateMs(completedAt);
		if (!start || !end || end < start) return '—';
		const durationMs = end - start;
		if (durationMs < 1000) return `${durationMs}ms`;
		const seconds = Math.round(durationMs / 1000);
		if (seconds < 60) return `${seconds}s`;
		const minutes = Math.floor(seconds / 60);
		const remainder = seconds % 60;
		return `${minutes}m ${remainder}s`;
	}

	function episodeGoalId(episode: V3EpisodeRecordView): string {
		return episode.goal_key || episode.goal_id || 'unknown';
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

	function compactRoleLabel(value: string | undefined): string {
		if (!value) return '';
		const normalized = value.replace(/\s+/g, ' ').trim();
		if (!normalized) return '';
		const sentenceMatch = normalized.match(/^(.+?)[.!?](?:\s|$)/);
		const firstSentence = (sentenceMatch?.[1] || normalized).trim();
		return firstSentence.length > 72 ? `${firstSentence.slice(0, 69).trim()}...` : firstSentence;
	}

	function kindRoleLabel(kind: string | undefined): string {
		const normalized = kind?.trim().toLowerCase();
		if (normalized === 'personal') return 'Personal Agent';
		if (normalized === 'worker') return 'Worker Agent';
		if (normalized === 'system') return 'System Agent';
		return '';
	}

	function deriveAgentRoleLabel(definition: Record<string, unknown>, agentId: string): string {
		const formattedAgentId = formatRoleLabel(agentId);
		const name = readString(definition, 'name') || '';
		const kindLabel = kindRoleLabel(readString(definition, 'kind'));
		return (
			compactRoleLabel(readString(definition, 'role')) ||
			compactRoleLabel(readString(definition, 'role_label')) ||
			compactRoleLabel(readString(definition, 'job_title')) ||
			(formattedAgentId.toLowerCase() !== name.trim().toLowerCase() ? formattedAgentId : '') ||
			kindLabel ||
			formattedAgentId
		);
	}

	function agentOptionLabel(agent: AgentOption): string {
		const roleLabel = agent.roleLabel.trim();
		if (!roleLabel || roleLabel.toLowerCase() === agent.name.trim().toLowerCase()) return agent.name;
		return `${agent.name} - ${roleLabel}`;
	}

	function episodeTriggerType(episode: V3EpisodeRecordView): string {
		return episode.trigger_type || episode.trigger?.trigger_type || 'unknown';
	}

	function episodeTriggerTimestamp(episode: V3EpisodeRecordView): string | undefined {
		return episode.trigger_timestamp || episode.trigger?.timestamp;
	}

	function outcomeVariant(outcome: unknown): string {
		if (typeof outcome === 'string') return outcome;
		const record = asRecord(outcome);
		if (!record) return 'unknown';
		const keys = Object.keys(record);
		return keys.length === 1 ? keys[0] : 'unknown';
	}

	function outcomeSummary(outcome: unknown): string {
		if (typeof outcome === 'string') return outcome;
		const record = asRecord(outcome);
		if (!record) return 'No summary';
		const key = Object.keys(record)[0];
		if (!key) return 'No summary';
		const payload = asRecord(record[key]);
		if (!payload) return key;
		return (
			readString(payload, 'summary') ||
			readString(payload, 'error') ||
			readString(payload, 'reason') ||
			readString(payload, 'remaining') ||
			key
		);
	}

	function episodeOutcomeVariant(episode: V3EpisodeRecordView): string {
		if (typeof episode.outcome_kind === 'string' && episode.outcome_kind.trim()) {
			return episode.outcome_kind;
		}
		return outcomeVariant(episode.outcome);
	}

	function episodeOutcomeSummary(episode: V3EpisodeRecordView): string {
		if (typeof episode.outcome_summary === 'string' && episode.outcome_summary.trim()) {
			return episode.outcome_summary;
		}
		return outcomeSummary(episode.outcome);
	}

	function statusTone(variant: string): 'success' | 'warning' | 'error' | 'default' {
		const normalized = variant.toLowerCase();
		if (normalized.includes('achieved') || normalized.includes('success')) return 'success';
		if (normalized.includes('paused') || normalized.includes('partial')) return 'warning';
		if (normalized.includes('failed') || normalized.includes('error') || normalized.includes('budget') || normalized.includes('circuit')) {
			return 'error';
		}
		return 'default';
	}

	function parseMemoryUpdates(rawUpdates: unknown[]): MemoryUpdateEntry[] {
		if (!Array.isArray(rawUpdates) || rawUpdates.length === 0) return [];
		const entries: MemoryUpdateEntry[] = [];
		for (const update of rawUpdates) {
			const rec = asRecord(update);
			if (!rec) {
				if (typeof update === 'string') {
					entries.push({ tier_name: update });
				}
				continue;
			}
			const tierName = readString(rec, 'tier_name') || readString(rec, 'tier') || readString(rec, 'name') || 'unknown';
			const scope = readString(rec, 'scope');
			const entityCount = typeof rec.entity_count === 'number' ? rec.entity_count : undefined;
			const insightCount = typeof rec.insight_count === 'number' ? rec.insight_count : undefined;
			const summary = readString(rec, 'summary') || readString(rec, 'description') || readString(rec, 'content');
			const operation = readString(rec, 'operation') || readString(rec, 'type') || readString(rec, 'action');
			entries.push({ tier_name: tierName, scope, entity_count: entityCount, insight_count: insightCount, summary, operation });
		}
		return entries;
	}

	function clearScopeState(): void {
		latestEpisodesRequestId += 1;
		isLoadingAgents = false;
		isLoadingEpisodes = false;
		error = null;
		agents = [];
		selectedAgentId = '';
		episodes = [];
		totalEpisodeCount = 0;
		episodeOffset = 0;
		episodePageLimit = EPISODES_PAGE_LIMIT_DEFAULT;
		selectedEpisodeId = '';
		detailPanelOpen = false;
		lastRefreshAt = null;
		routeKey = '';
	}

	function formatRelativeTime(timestamp: number | null): string {
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

	function parseAgentOptionFromRecord(record: Record<string, unknown> | null): AgentOption | null {
		const definition = record ? asRecord(record.definition) : null;
		if (!definition) return null;
		const agentId = readString(definition, 'agent_id');
		if (!agentId) return null;
		return {
			agent_id: agentId,
			name: readString(definition, 'name') || agentId,
			roleLabel: deriveAgentRoleLabel(definition, agentId)
		};
	}

	function sortAgentOptions(options: AgentOption[]): AgentOption[] {
		return [...options].sort((left, right) => left.name.localeCompare(right.name));
	}

	function parseEpisodePageLimit(value: string | null): number {
		if (!value) return EPISODES_PAGE_LIMIT_DEFAULT;
		const parsed = Number.parseInt(value, 10);
		return EPISODES_PAGE_LIMIT_OPTIONS.includes(parsed as (typeof EPISODES_PAGE_LIMIT_OPTIONS)[number])
			? parsed
			: EPISODES_PAGE_LIMIT_DEFAULT;
	}

	function parseOffset(value: string | null): number {
		if (!value) return 0;
		const parsed = Number.parseInt(value, 10);
		if (!Number.isFinite(parsed) || parsed <= 0) return 0;
		return parsed;
	}

	function alignOffsetToLimit(offset: number, limit: number): number {
		const normalizedLimit = Math.max(1, Math.trunc(limit));
		const normalizedOffset = Math.max(0, Math.trunc(offset));
		return Math.floor(normalizedOffset / normalizedLimit) * normalizedLimit;
	}

	function lastPageOffset(totalCount: number, limit: number): number {
		if (totalCount <= 0) return 0;
		return Math.floor((totalCount - 1) / limit) * limit;
	}

	function setRouteQuery(agentId: string, offset: number, limit = episodePageLimit): void {
		if (!browser) return;
		const normalizedAgentId = agentId.trim();
		const normalizedLimit = parseEpisodePageLimit(String(limit));
		const normalizedOffset = alignOffsetToLimit(offset, normalizedLimit);
		const currentAgentId = ($page.url.searchParams.get('agent_id') || '').trim();
		const currentOffset = parseOffset($page.url.searchParams.get('offset'));
		const currentLimit = parseEpisodePageLimit($page.url.searchParams.get('limit'));
		if (
			currentAgentId === normalizedAgentId &&
			currentOffset === normalizedOffset &&
			currentLimit === normalizedLimit
		) {
			return;
		}
		const url = new URL(window.location.href);
		if (normalizedAgentId) {
			url.searchParams.set('agent_id', normalizedAgentId);
		} else {
			url.searchParams.delete('agent_id');
		}
		if (normalizedOffset > 0) {
			url.searchParams.set('offset', String(normalizedOffset));
		} else {
			url.searchParams.delete('offset');
		}
		if (normalizedLimit !== EPISODES_PAGE_LIMIT_DEFAULT) {
			url.searchParams.set('limit', String(normalizedLimit));
		} else {
			url.searchParams.delete('limit');
		}
		const search = url.searchParams.toString();
		const nextRoute = search ? `${url.pathname}?${search}` : url.pathname;
		void goto(nextRoute, { replaceState: true, noScroll: true, keepFocus: true });
	}

	function extractAgentOptions(payload: unknown): AgentOption[] {
		const root = asRecord(payload);
		const rawAgents = Array.isArray(root?.agents) ? root.agents : [];
		const options: AgentOption[] = [];
		for (const entry of rawAgents) {
			const option = parseAgentOptionFromRecord(asRecord(entry));
			if (!option) continue;
			options.push(option);
		}
		return sortAgentOptions(options);
	}

	async function loadAgents(): Promise<AgentOption[]> {
		const response = await timedFetch('/api/magician/v2/agents?limit=200');
		if (!response.ok) {
			throw new Error(await readApiError(response));
		}
		const payload = (await response.json()) as unknown;
		return extractAgentOptions(payload);
	}

	async function loadAgentById(agentId: string): Promise<AgentOption | null> {
		const normalizedAgentId = agentId.trim();
		if (!normalizedAgentId) return null;
		const response = await timedFetch(`/api/magician/v2/agents/${encodeURIComponent(normalizedAgentId)}`);
		if (response.status === 404) {
			return null;
		}
		if (!response.ok) {
			throw new Error(await readApiError(response));
		}
		const payload = (await response.json()) as unknown;
		return parseAgentOptionFromRecord(asRecord(payload));
	}

	async function loadEpisodes(agentId: string, offset: number, limit: number): Promise<EpisodesResponse> {
		if (!agentId) {
			return { episodes: [], total_count: 0 };
		}
		const normalizedLimit = parseEpisodePageLimit(String(limit));
		const normalizedOffset = alignOffsetToLimit(offset, normalizedLimit);
		const params = new URLSearchParams({
			limit: String(normalizedLimit),
			offset: String(normalizedOffset)
		});
		const response = await timedFetch(`/api/magician/v2/agents/${encodeURIComponent(agentId)}/episodes?${params.toString()}`);
		if (!response.ok) {
			throw new Error(await readApiError(response));
		}
		const payload = (await response.json()) as EpisodesResponse;
		const nextEpisodes = Array.isArray(payload.episodes) ? payload.episodes : [];
		const totalCount = Number.isFinite(payload.total_count) ? payload.total_count : nextEpisodes.length;
		return {
			episodes: nextEpisodes,
			total_count: Math.max(nextEpisodes.length, totalCount)
		};
	}

	async function hydrateRoute(): Promise<void> {
		const requestId = ++latestEpisodesRequestId;
		isLoadingAgents = true;
		isLoadingEpisodes = true;
		error = null;
		try {
			const nextAgents = await loadAgents();
			if (requestId !== latestEpisodesRequestId) return;
			const queryAgentId = ($page.url.searchParams.get('agent_id') || '').trim();
			const queryLimit = parseEpisodePageLimit($page.url.searchParams.get('limit'));
			const queryOffset = alignOffsetToLimit(parseOffset($page.url.searchParams.get('offset')), queryLimit);
			let hydratedAgents = nextAgents;
			if (queryAgentId && !hydratedAgents.some((agent) => agent.agent_id === queryAgentId)) {
				const deepLinkedAgent = await loadAgentById(queryAgentId);
				if (requestId !== latestEpisodesRequestId) return;
				if (deepLinkedAgent && !hydratedAgents.some((agent) => agent.agent_id === deepLinkedAgent.agent_id)) {
					hydratedAgents = sortAgentOptions([...hydratedAgents, deepLinkedAgent]);
				}
			}
			agents = hydratedAgents;
			let nextSelectedAgentId = selectedAgentId;
			if (queryAgentId && hydratedAgents.some((agent) => agent.agent_id === queryAgentId)) {
				nextSelectedAgentId = queryAgentId;
			} else if (!nextSelectedAgentId || !hydratedAgents.some((agent) => agent.agent_id === nextSelectedAgentId)) {
				nextSelectedAgentId = hydratedAgents[0]?.agent_id || '';
			}
			let nextOffset = queryOffset;
			if (!nextSelectedAgentId || (queryAgentId && queryAgentId !== nextSelectedAgentId)) {
				nextOffset = 0;
			}
			if (queryAgentId !== nextSelectedAgentId || queryOffset !== nextOffset) {
				setRouteQuery(nextSelectedAgentId, nextOffset, queryLimit);
			}
			selectedAgentId = nextSelectedAgentId;
			episodePageLimit = queryLimit;
			episodeOffset = nextOffset;
			const response = await loadEpisodes(nextSelectedAgentId, nextOffset, queryLimit);
			if (
				requestId !== latestEpisodesRequestId ||
				selectedAgentId !== nextSelectedAgentId ||
				episodeOffset !== nextOffset ||
				episodePageLimit !== queryLimit
			) {
				return;
			}
			if (response.total_count > 0 && nextOffset >= response.total_count) {
				const lastOffset = lastPageOffset(response.total_count, queryLimit);
				if (lastOffset !== nextOffset) {
					episodeOffset = lastOffset;
					setRouteQuery(nextSelectedAgentId, lastOffset, queryLimit);
					return;
				}
			}
			episodes = response.episodes;
			totalEpisodeCount = response.total_count;
			if (!episodes.some((episode) => episode.episode_id === selectedEpisodeId)) {
				selectedEpisodeId = '';
				detailPanelOpen = false;
			}
			lastRefreshAt = Date.now();
		} catch (err) {
			if (requestId !== latestEpisodesRequestId) return;
			error = err instanceof Error ? err.message : 'Failed to load episodes';
			episodes = [];
			totalEpisodeCount = 0;
			selectedEpisodeId = '';
			detailPanelOpen = false;
		} finally {
			if (requestId === latestEpisodesRequestId) {
				isLoadingAgents = false;
				isLoadingEpisodes = false;
			}
		}
	}

	function goToEpisodePage(pageNumber: number): void {
		if (isLoadingEpisodes) return;
		const safePage = Math.min(totalPages, Math.max(1, Math.floor(pageNumber)));
		const nextOffset = (safePage - 1) * episodePageLimit;
		if (nextOffset === episodeOffset) return;
		episodeOffset = nextOffset;
		setRouteQuery(selectedAgentId, nextOffset, episodePageLimit);
		episodes = [];
		selectedEpisodeId = '';
		detailPanelOpen = false;
	}

	function onPageLimitChange(event: Event): void {
		const target = event.currentTarget;
		if (!(target instanceof HTMLSelectElement)) return;
		const nextLimit = parseEpisodePageLimit(target.value);
		const nextOffset = alignOffsetToLimit(episodeOffset, nextLimit);
		if (nextLimit === episodePageLimit && nextOffset === episodeOffset) return;
		episodePageLimit = nextLimit;
		episodeOffset = nextOffset;
		setRouteQuery(selectedAgentId, nextOffset, nextLimit);
		episodes = [];
		selectedEpisodeId = '';
		detailPanelOpen = false;
	}

	function memoryUpdatesForEpisode(episode: V3EpisodeRecordView | null): MemoryUpdateEntry[] {
		return episode ? parseMemoryUpdates(episode.memory_updates || []) : [];
	}

	async function openCreateAgent(): Promise<void> {
		await goto('/crew/new');
	}

	function selectEpisode(episodeId: string): void {
		selectedEpisodeId = episodeId;
		detailPanelOpen = true;
	}

	function closeDetailPanel(): void {
		detailPanelOpen = false;
	}

	function handleWindowKeydown(event: KeyboardEvent): void {
		if (event.key === 'Escape' && detailPanelOpen) {
			closeDetailPanel();
		}
	}

	function handleAgentFilterSubmit(event: SubmitEvent): void {
		const form = event.currentTarget;
		if (!(form instanceof HTMLFormElement)) return;
		const values = new FormData(form);
		const nextAgentId = asString(values.get('agent_id')).trim();
		if (!nextAgentId || nextAgentId === selectedAgentId) return;
		selectedAgentId = nextAgentId;
		episodeOffset = 0;
		setRouteQuery(nextAgentId, 0);
		episodes = [];
		totalEpisodeCount = 0;
		selectedEpisodeId = '';
		detailPanelOpen = false;
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
	<title>History · Magican</title>
</svelte:head>

<svelte:window on:keydown={handleWindowKeydown} />

<div class="agent-route history-route">
	<header class="history-header">
		<div class="history-title">
			<h1>History</h1>
			<p>Inspect trigger, cycle, and outcome history for each agent.</p>
		</div>

		<div class="history-metrics" aria-label="History summary">
			<div class="history-metric">
				<span>Agents</span>
				<strong>{agents.length}</strong>
			</div>
			<div class="history-metric">
				<span>Episodes</span>
				<strong>{totalEpisodeCount === 0 ? '0' : `${pageStart}-${pageEnd}/${totalEpisodeCount}`}</strong>
			</div>
			<div class="history-metric">
				<span>Page</span>
				<strong>{currentPage}/{totalPages}</strong>
			</div>
			<div class="history-metric">
				<span>Last refresh</span>
				<strong>{formatRelativeTime(lastRefreshAt)}</strong>
			</div>
		</div>

		<div class="history-controls">
			<form class="history-agent-form" on:submit|preventDefault={handleAgentFilterSubmit}>
				<label>
					<span>Agent</span>
					<select
						name="agent_id"
						value={selectedAgentId}
						disabled={isLoadingAgents || agents.length === 0}
						required
					>
						{#if agents.length === 0}
							<option value="">Select agent</option>
						{:else}
							{#each agents as agent (agent.agent_id)}
								<option value={agent.agent_id}>{agentOptionLabel(agent)}</option>
							{/each}
						{/if}
					</select>
				</label>
				<button class="history-button history-button--primary" type="submit" disabled={isLoadingAgents || agents.length === 0}>
					Apply
				</button>
				<button
					class="history-icon-button history-refresh-button"
					type="button"
					title={isLoadingAgents || isLoadingEpisodes ? 'Refreshing history' : 'Refresh history'}
					aria-label={isLoadingAgents || isLoadingEpisodes ? 'Refreshing history' : 'Refresh history'}
					disabled={isLoadingAgents || isLoadingEpisodes}
					on:click={() => hydrateRoute()}
				>
					<Icon name="rotate-ccw" size={15} />
				</button>
			</form>
		</div>
	</header>

	{#if error}
		<div class="history-alert" role="alert">{error}</div>
	{/if}

	{#if isLoadingAgents && agents.length === 0}
		<section class="history-empty">
			<h2>Loading agents</h2>
			<p>Fetching available agents for episode inspection.</p>
		</section>
	{:else if agents.length === 0}
		<section class="history-empty">
			<h2>No agents available</h2>
			<p>Create an agent first to browse execution episodes.</p>
			<button class="history-button history-button--primary" type="button" on:click={openCreateAgent}>Create agent</button>
		</section>
	{:else}
		<section class="history-panel history-panel--list" aria-labelledby="history-episode-list-title">
			<header class="history-panel-header">
				<div>
					<h2 id="history-episode-list-title">Episode list</h2>
					<p>
						{#if totalEpisodeCount === 0}
							0 episodes
						{:else}
							{pageStart}-{pageEnd} of {totalEpisodeCount}
						{/if}
					</p>
				</div>
				{#if isLoadingEpisodes}
					<span class="history-loading-chip">Refreshing</span>
				{/if}
			</header>

			{#if totalEpisodeCount > 0}
				<div class="history-list-toolbar" aria-label="Episode pagination">
					<label class="history-page-size">
						<span>Rows</span>
						<select
							value={episodePageLimit}
							disabled={isLoadingEpisodes}
							on:change={onPageLimitChange}
						>
							{#each EPISODES_PAGE_LIMIT_OPTIONS as option (option)}
								<option value={option}>{option}</option>
							{/each}
						</select>
					</label>
					<ServerPager
						currentPage={currentPage}
						pageCount={totalPages}
						startItem={pageStart}
						endItem={pageEnd}
						totalItems={totalEpisodeCount}
						loading={isLoadingEpisodes}
						ariaLabel="Episode pagination"
						on:pagechange={(event) => goToEpisodePage(event.detail.page)}
					/>
				</div>
			{/if}

			{#if isLoadingEpisodes && episodes.length === 0}
				<div class="history-empty history-empty--inline">
					<h3>Loading episodes</h3>
					<p>Fetching persisted execution episodes.</p>
				</div>
			{:else if episodes.length === 0}
				<div class="history-empty history-empty--inline">
					<h3>No episodes</h3>
					<p>No episodes persisted for this agent yet.</p>
				</div>
			{:else}
				<div class="history-episode-list">
					{#each episodes as episode (episode.episode_id)}
						{@const variant = episodeOutcomeVariant(episode)}
						<button
							class:selected={selectedEpisode?.episode_id === episode.episode_id && detailPanelOpen}
							class="history-episode-card"
							type="button"
							aria-label={`Open details for ${episodeGoalId(episode)}`}
							on:click={() => selectEpisode(episode.episode_id)}
						>
							<div class="history-episode-card__main">
								<span class={`history-badge history-badge--${statusTone(variant)}`}>{variant}</span>
								<div class="history-episode-card__copy">
									<strong>{episodeGoalId(episode)}</strong>
									<span class="history-episode-summary">{episodeOutcomeSummary(episode)}</span>
								</div>
								<span class="history-episode-card__action">
									Details <Icon name="chevron-right" size={13} />
								</span>
							</div>
							<div class="history-episode-facts" aria-label="Episode summary">
								<span><strong>Trigger</strong>{episodeTriggerType(episode)} · seq {episode.trigger_seq}</span>
								<span><strong>Started</strong>{formatDateTime(episode.started_at)}</span>
								<span><strong>Duration</strong>{formatDurationMs(episode.started_at, episode.completed_at)}</span>
								<span>
									<strong>Activity</strong>{episode.actions_taken?.length || 0} actions · {episode.observations?.length || 0} obs · {episode.memory_updates?.length || 0} memory
								</span>
							</div>
						</button>
					{/each}
				</div>
				{#if totalEpisodeCount > 0}
					<div class="history-list-toolbar history-list-toolbar--bottom" aria-label="Episode pagination">
						<ServerPager
							currentPage={currentPage}
							pageCount={totalPages}
							startItem={pageStart}
							endItem={pageEnd}
							totalItems={totalEpisodeCount}
							loading={isLoadingEpisodes}
							ariaLabel="Episode pagination"
							on:pagechange={(event) => goToEpisodePage(event.detail.page)}
						/>
					</div>
				{/if}
			{/if}
		</section>
	{/if}
</div>

{#if selectedEpisode && detailPanelOpen}
	{@const memoryUpdates = memoryUpdatesForEpisode(selectedEpisode)}
	<button
		class="history-detail-backdrop"
		type="button"
		aria-label="Close episode details"
		on:click={closeDetailPanel}
	></button>
	<div class="history-detail-drawer" role="dialog" aria-modal="true" aria-labelledby="history-detail-title">
		<header class="history-detail-drawer__header">
			<div class="history-detail-drawer__title">
				<span class={`history-badge history-badge--${statusTone(episodeOutcomeVariant(selectedEpisode))}`}>
					{episodeOutcomeVariant(selectedEpisode)}
				</span>
				<h2 id="history-detail-title">{episodeGoalId(selectedEpisode)}</h2>
				<p>{selectedEpisode.episode_id}</p>
			</div>
			<button
				class="history-icon-button"
				type="button"
				title="Close"
				aria-label="Close episode details"
				on:click={closeDetailPanel}
			>
				<Icon name="x" size={16} />
			</button>
		</header>

		<div class="history-detail-drawer__body">
			<section class="history-detail-section" aria-labelledby="history-detail-summary-title">
				<header class="history-detail-section__header">
					<h3 id="history-detail-summary-title">Cycle summary</h3>
					<p>{formatDurationMs(selectedEpisode.started_at, selectedEpisode.completed_at)}</p>
				</header>
				<dl class="history-datalist">
					<div>
						<dt>Triggered</dt>
						<dd>{episodeTriggerType(selectedEpisode)} · seq {selectedEpisode.trigger_seq}</dd>
					</div>
					<div>
						<dt>Trigger time</dt>
						<dd>{formatDateTime(episodeTriggerTimestamp(selectedEpisode))}</dd>
					</div>
					<div>
						<dt>Cycle started</dt>
						<dd>{formatDateTime(selectedEpisode.started_at)}</dd>
					</div>
					<div>
						<dt>Cycle completed</dt>
						<dd>{formatDateTime(selectedEpisode.completed_at)}</dd>
					</div>
					<div class="history-datalist__wide">
						<dt>Outcome</dt>
						<dd>{episodeOutcomeSummary(selectedEpisode)}</dd>
					</div>
				</dl>
			</section>

			<div class="history-counts">
				<div>
					<span>Actions</span>
					<strong>{selectedEpisode.actions_taken?.length || 0}</strong>
				</div>
				<div>
					<span>Observations</span>
					<strong>{selectedEpisode.observations?.length || 0}</strong>
				</div>
				<div>
					<span>Memory updates</span>
					<strong>{selectedEpisode.memory_updates?.length || 0}</strong>
				</div>
			</div>

			<section class="history-detail-section history-memory" aria-labelledby="history-memory-title">
				<header class="history-detail-section__header">
					<h3 id="history-memory-title">Memory updates</h3>
					<p>{selectedEpisode.memory_updates?.length || 0} update(s)</p>
				</header>

				{#if memoryUpdates.length === 0}
					<div class="history-empty history-empty--inline">
						<h4>No memory updates</h4>
						<p>This episode did not produce any memory updates.</p>
					</div>
				{:else}
					<div class="history-memory-list">
						{#each memoryUpdates as update, index (`${update.tier_name}-${index}`)}
							<article class="history-memory-card">
								<div class="history-memory-tags">
									<span class="history-chip history-chip--info">{update.tier_name}</span>
									{#if update.scope}
										<span class="history-chip">{update.scope}</span>
									{/if}
									{#if update.operation}
										<span class:history-chip--success={update.operation === 'write' || update.operation === 'upsert'} class="history-chip">{update.operation}</span>
									{/if}
								</div>
								{#if typeof update.entity_count === 'number' || typeof update.insight_count === 'number'}
									<dl class="history-memory-counts">
										{#if typeof update.entity_count === 'number'}
											<div>
												<dt>Entities</dt>
												<dd>{update.entity_count}</dd>
											</div>
										{/if}
										{#if typeof update.insight_count === 'number'}
											<div>
												<dt>Insights</dt>
												<dd>{update.insight_count}</dd>
											</div>
										{/if}
									</dl>
								{/if}
								{#if update.summary}
									<p>{update.summary}</p>
								{/if}
							</article>
						{/each}
					</div>
				{/if}
			</section>
		</div>
	</div>
{/if}

<style>
	.history-route,
	.history-detail-drawer {
		--history-surface: color-mix(
			in srgb,
			var(--theme-color-surface, var(--bg-card, #fff)) 88%,
			transparent
		);
		--history-surface-raised: color-mix(
			in srgb,
			var(--bg-card, #fff) 86%,
			var(--accent-primary, #c2502a) 4%
		);
		--history-surface-soft: color-mix(
			in srgb,
			var(--bg-card, #fff) 78%,
			var(--accent-primary, #c2502a) 6%
		);
		--history-surface-info: color-mix(
			in srgb,
			var(--bg-card, #fff) 78%,
			var(--accent-secondary, #4ecdc4) 7%
		);
		--history-border: color-mix(
			in srgb,
			var(--accent-primary, #c2502a) 16%,
			var(--border-soft, #e5e7eb)
		);
		--history-border-strong: color-mix(
			in srgb,
			var(--accent-primary, #c2502a) 28%,
			var(--border-soft, #e5e7eb)
		);
		--history-shadow: 0 12px 32px color-mix(in srgb, var(--text-primary, #111827) 10%, transparent);
	}

	.history-route {
		width: min(100%, var(--app-content-max, 1320px));
		margin: 0 auto;
		padding: 0 1rem 1.5rem;
		color: var(--text-primary, #111827);
	}

	.history-header,
	.history-panel,
	.history-empty,
	.history-alert {
		border: 1px solid var(--history-border);
		border-radius: 8px;
		background:
			linear-gradient(
				180deg,
				color-mix(in srgb, var(--accent-primary, #c2502a) 4%, transparent),
				transparent 72%
			),
			var(--history-surface);
		box-shadow: var(--shadow-sm, var(--history-shadow));
	}

	.history-header {
		display: grid;
		gap: 1rem;
		margin-top: 0.5rem;
		padding: 1rem;
	}

	.history-title h1,
	.history-panel h2,
	.history-detail-drawer h2,
	.history-detail-section h3,
	.history-memory h3,
	.history-empty h2,
	.history-empty h3,
	.history-empty h4 {
		margin: 0;
	}

	.history-title h1 {
		font-size: 1.35rem;
		line-height: 1.15;
	}

	.history-title p,
	.history-panel-header p,
	.history-detail-drawer p,
	.history-detail-section__header p,
	.history-empty p,
	.history-memory header p {
		margin: 0.25rem 0 0;
		color: var(--text-secondary, #6b7280);
		font-size: 0.84rem;
		line-height: 1.4;
	}

	.history-metrics,
	.history-counts {
		display: grid;
		grid-template-columns: repeat(4, minmax(0, 1fr));
		gap: 0.65rem;
	}

	.history-metric,
	.history-counts > div {
		--history-metric-accent: var(--accent-primary, #c2502a);
		display: grid;
		gap: 0.2rem;
		min-width: 0;
		position: relative;
		overflow: hidden;
		border: 1px solid color-mix(in srgb, var(--history-metric-accent) 24%, var(--border-soft, #e5e7eb));
		border-radius: 8px;
		background:
			linear-gradient(
				90deg,
				color-mix(in srgb, var(--history-metric-accent) 9%, transparent),
				transparent 72%
			),
			color-mix(in srgb, var(--history-surface-raised) 92%, transparent);
		padding: 0.7rem;
	}

	.history-metric:nth-child(2),
	.history-counts > div:nth-child(2) {
		--history-metric-accent: var(--accent-secondary, #4ecdc4);
	}

	.history-metric:nth-child(3),
	.history-counts > div:nth-child(3) {
		--history-metric-accent: var(--color-success, #00bb7f);
	}

	.history-metric:nth-child(4) {
		--history-metric-accent: var(--color-info, #3b82f6);
	}

	.history-metric span,
	.history-counts span,
	.history-datalist dt,
	.history-memory-counts dt {
		color: var(--text-muted, #7f8c8d);
		font-size: 0.68rem;
		font-weight: 800;
		text-transform: uppercase;
	}

	.history-metric strong,
	.history-counts strong {
		min-width: 0;
		overflow-wrap: anywhere;
		font-size: 1rem;
	}

	.history-controls,
	.history-agent-form {
		display: flex;
		flex-wrap: wrap;
		gap: 0.65rem;
		align-items: end;
	}

	.history-agent-form label {
		display: grid;
		gap: 0.3rem;
		min-width: min(320px, 100%);
		color: var(--text-secondary, #6b7280);
		font-size: 0.74rem;
		font-weight: 800;
	}

	.history-agent-form select {
		min-height: 2rem;
		border: 1px solid var(--input-border, var(--history-border));
		border-radius: 6px;
		background: var(--input-bg, var(--history-surface-raised));
		color: var(--text-primary, #111827);
		font: inherit;
		padding: 0.42rem 0.55rem;
	}

	.history-button {
		min-height: 2rem;
		border: 1px solid var(--history-border);
		border-radius: 6px;
		background: var(--history-surface-raised);
		color: var(--text-secondary, #5f6668);
		cursor: pointer;
		font: inherit;
		font-size: 0.78rem;
		font-weight: 820;
		padding: 0.45rem 0.75rem;
	}

	.history-button:hover:not(:disabled) {
		border-color: var(--history-border-strong);
		background: color-mix(in srgb, var(--history-surface-raised) 86%, var(--accent-primary, #c2502a) 8%);
		color: var(--text-primary, #111827);
	}

	.history-button--primary,
	.history-button--secondary {
		border-color: var(--accent-primary, #c2502a);
		background: var(--accent-primary, #c2502a);
		color: var(--button-primary-color, #fff);
	}

	.history-button--secondary {
		background: color-mix(in srgb, var(--accent-primary, #c2502a) 10%, var(--history-surface-raised));
		color: var(--accent-primary, #c2502a);
	}

	.history-button:disabled {
		cursor: not-allowed;
		opacity: 0.55;
	}

	.history-list-toolbar {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: 0.65rem;
		margin-bottom: 0.75rem;
		border: 1px solid var(--history-border);
		border-radius: 8px;
		background:
			linear-gradient(
				90deg,
				color-mix(in srgb, var(--accent-primary, #c2502a) 6%, transparent),
				transparent 70%
			),
			var(--history-surface-raised);
		padding: 0.6rem 0.65rem;
	}

	.history-list-toolbar--bottom {
		justify-content: flex-end;
		margin: 0.75rem 0 0;
	}

	.history-page-size {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		color: var(--text-secondary, #6b7280);
		font-size: 0.74rem;
		font-weight: 820;
	}

	.history-page-size span {
		color: var(--text-muted, #7f8c8d);
		font-size: 0.68rem;
		text-transform: uppercase;
	}

	.history-page-size select {
		min-height: 1.85rem;
		border: 1px solid var(--input-border, var(--history-border));
		border-radius: 6px;
		background: var(--input-bg, var(--history-surface-soft));
		color: var(--text-primary, #111827);
		font: inherit;
		padding: 0.3rem 1.7rem 0.3rem 0.5rem;
	}

	.history-alert {
		margin-top: 0.85rem;
		border-color: color-mix(in srgb, var(--color-error, #e85d5d) 40%, var(--border-soft, #e5e7eb));
		background: var(--color-error-soft, rgba(255, 107, 107, 0.12));
		color: var(--color-error, #b42318);
		padding: 0.75rem 0.85rem;
		font-size: 0.84rem;
	}

	.history-panel {
		min-width: 0;
		padding: 0.85rem;
	}

	.history-panel--list {
		margin-top: 0.85rem;
	}

	.history-panel-header {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		gap: 0.75rem;
		margin-bottom: 0.75rem;
	}

	.history-panel h2 {
		font-size: 0.98rem;
		line-height: 1.2;
	}

	.history-loading-chip,
	.history-badge,
	.history-chip {
		display: inline-flex;
		width: fit-content;
		max-width: 100%;
		align-items: center;
		border-radius: 999px;
		border: 1px solid var(--history-border);
		background: var(--history-surface-soft);
		color: var(--text-secondary, #5f6668);
		font-size: 0.68rem;
		font-weight: 800;
		line-height: 1;
		padding: 0.24rem 0.48rem;
		white-space: nowrap;
	}

	.history-badge--success,
	.history-chip--success {
		border-color: color-mix(in srgb, var(--color-success, #00bb7f) 34%, var(--border-soft, #e5e7eb));
		background: var(--color-success-soft, rgba(0, 187, 127, 0.12));
		color: var(--color-success, #047857);
	}

	.history-badge--warning {
		border-color: color-mix(in srgb, var(--color-warning, #f59e0b) 36%, var(--border-soft, #e5e7eb));
		background: var(--color-warning-soft, rgba(245, 158, 11, 0.14));
		color: var(--color-warning, #92400e);
	}

	.history-badge--error {
		border-color: color-mix(in srgb, var(--color-error, #e85d5d) 36%, var(--border-soft, #e5e7eb));
		background: var(--color-error-soft, rgba(255, 107, 107, 0.12));
		color: var(--color-error, #b42318);
	}

	.history-chip--info {
		border-color: color-mix(in srgb, var(--accent-secondary, #4ecdc4) 38%, var(--border-soft, #e5e7eb));
		background: var(--accent-secondary-soft, rgba(78, 205, 196, 0.12));
		color: var(--accent-secondary, #05716c);
	}

	.history-episode-list,
	.history-memory-list {
		display: grid;
		gap: 0.65rem;
	}

	.history-episode-card {
		display: grid;
		gap: 0.65rem;
		width: 100%;
		min-width: 0;
		border: 1px solid var(--history-border);
		border-radius: 8px;
		background: var(--history-surface-raised);
		color: inherit;
		cursor: pointer;
		padding: 0.75rem;
		text-align: left;
		transition:
			border-color 0.15s ease,
			box-shadow 0.15s ease,
			background 0.15s ease;
	}

	.history-episode-card:hover {
		border-color: var(--history-border-strong);
		background: color-mix(in srgb, var(--history-surface-raised) 86%, var(--accent-primary, #c2502a) 7%);
	}

	.history-episode-card.selected {
		border-color: var(--accent-primary, #c2502a);
		box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent-primary, #c2502a) 18%, transparent);
	}

	.history-episode-card__main {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		align-items: start;
		gap: 0.7rem;
		min-width: 0;
	}

	.history-episode-card__copy {
		display: grid;
		gap: 0.18rem;
		min-width: 0;
	}

	.history-episode-card__copy strong {
		overflow-wrap: anywhere;
		font-size: 0.92rem;
		line-height: 1.25;
	}

	.history-episode-card__action {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		color: var(--accent-primary, #c2502a);
		font-size: 0.72rem;
		font-weight: 820;
		white-space: nowrap;
	}

	.history-episode-summary {
		color: var(--text-secondary, #6b7280);
		font-size: 0.8rem;
		line-height: 1.35;
	}

	.history-episode-facts {
		display: grid;
		grid-template-columns: 1fr 1.35fr 0.75fr 1.45fr;
		gap: 0.5rem;
		min-width: 0;
		color: var(--text-secondary, #6b7280);
		font-size: 0.75rem;
		line-height: 1.35;
	}

	.history-episode-facts > span {
		display: grid;
		gap: 0.12rem;
		min-width: 0;
		overflow-wrap: anywhere;
	}

	.history-episode-facts strong {
		color: var(--text-muted, #7f8c8d);
		font-size: 0.64rem;
		font-weight: 820;
		line-height: 1;
		text-transform: uppercase;
	}

	.history-datalist,
	.history-memory-counts {
		display: grid;
		gap: 0.55rem;
		margin: 0;
	}

	.history-datalist {
		grid-template-columns: repeat(2, minmax(0, 1fr));
	}

	.history-datalist__wide {
		grid-column: 1 / -1;
	}

	.history-datalist > div,
	.history-memory-counts > div {
		display: grid;
		gap: 0.2rem;
		min-width: 0;
		border: 1px solid var(--history-border);
		border-radius: 8px;
		background: var(--history-surface-soft);
		padding: 0.58rem 0.65rem;
	}

	.history-datalist dd,
	.history-memory-counts dd {
		margin: 0;
		overflow-wrap: anywhere;
		color: var(--text-primary, #111827);
		font-size: 0.83rem;
		line-height: 1.35;
	}

	.history-counts {
		grid-template-columns: repeat(3, minmax(0, 1fr));
		margin-top: 0.85rem;
	}

	.history-detail-backdrop {
		position: fixed;
		inset: 0;
		z-index: 940;
		border: 0;
		background: color-mix(in srgb, var(--text-primary, #111827) 30%, transparent);
		cursor: default;
		padding: 0;
	}

	.history-detail-drawer {
		position: fixed;
		top: 0;
		right: 0;
		bottom: 0;
		z-index: 950;
		display: flex;
		width: min(560px, calc(100vw - 1rem));
		min-width: 0;
		flex-direction: column;
		border-left: 1px solid var(--history-border-strong);
		background:
			linear-gradient(
				180deg,
				color-mix(in srgb, var(--accent-primary, #c2502a) 5%, transparent),
				transparent 58%
			),
			var(--history-surface);
		box-shadow: -20px 0 48px color-mix(in srgb, var(--text-primary, #111827) 18%, transparent);
		color: var(--text-primary, #111827);
		animation: history-drawer-enter 0.18s ease-out;
	}

	.history-detail-drawer__header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 0.9rem;
		border-bottom: 1px solid var(--history-border);
		background:
			linear-gradient(
				90deg,
				color-mix(in srgb, var(--accent-primary, #c2502a) 8%, transparent),
				transparent 72%
			),
			var(--history-surface-raised);
		padding: 1rem;
	}

	.history-detail-drawer__title {
		display: grid;
		gap: 0.45rem;
		min-width: 0;
	}

	.history-detail-drawer__title h2 {
		overflow-wrap: anywhere;
		font-size: 1.05rem;
		line-height: 1.2;
	}

	.history-detail-drawer__title p {
		overflow-wrap: anywhere;
		font-family: var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
		font-size: 0.72rem;
	}

	.history-detail-drawer__body {
		display: grid;
		gap: 0.85rem;
		min-height: 0;
		overflow-y: auto;
		background: color-mix(in srgb, var(--history-surface) 72%, transparent);
		padding: 0.9rem 1rem 1.2rem;
	}

	.history-detail-section {
		display: grid;
		gap: 0.7rem;
		min-width: 0;
		border: 1px solid var(--history-border);
		border-radius: 8px;
		background: var(--history-surface-raised);
		padding: 0.8rem;
	}

	.history-detail-section__header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 0.75rem;
	}

	.history-detail-section__header h3 {
		font-size: 0.9rem;
		line-height: 1.2;
	}

	.history-icon-button {
		display: inline-flex;
		width: 2rem;
		height: 2rem;
		flex: 0 0 auto;
		align-items: center;
		justify-content: center;
		border: 1px solid var(--history-border);
		border-radius: 6px;
		background: var(--history-surface-soft);
		color: var(--text-secondary, #5f6668);
		cursor: pointer;
	}

	.history-icon-button:hover {
		border-color: var(--accent-primary, #c2502a);
		color: var(--accent-primary, #c2502a);
	}

	.history-refresh-button {
		align-self: end;
		width: 2rem;
		height: 2rem;
	}

	@keyframes history-drawer-enter {
		from {
			transform: translateX(100%);
		}
		to {
			transform: translateX(0);
		}
	}

	.history-memory {
		display: grid;
		gap: 0.65rem;
		margin-top: 0.95rem;
	}

	.history-memory header {
		display: flex;
		justify-content: space-between;
		gap: 0.75rem;
	}

	.history-memory-card {
		display: grid;
		gap: 0.55rem;
		border: 1px solid var(--history-border);
		border-radius: 8px;
		background:
			linear-gradient(
				90deg,
				color-mix(in srgb, var(--accent-secondary, #4ecdc4) 6%, transparent),
				transparent 76%
			),
			var(--history-surface-info);
		padding: 0.7rem;
	}

	.history-memory-tags {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
		min-width: 0;
	}

	.history-memory-counts {
		grid-template-columns: repeat(2, minmax(0, 1fr));
	}

	.history-memory-card p {
		margin: 0;
		color: var(--text-secondary, #6b7280);
		font-size: 0.82rem;
		line-height: 1.42;
		overflow-wrap: anywhere;
	}

	.history-empty {
		display: grid;
		gap: 0.45rem;
		margin-top: 0.85rem;
		padding: 1rem;
	}

	.history-empty--inline {
		margin-top: 0;
		border-color: var(--history-border);
		border-style: dashed;
		background:
			linear-gradient(
				135deg,
				color-mix(in srgb, var(--accent-secondary, #4ecdc4) 7%, transparent),
				transparent 58%
			),
			var(--history-surface-soft);
		box-shadow: none;
	}

	@media (max-width: 980px) {
		.history-datalist,
		.history-episode-facts {
			grid-template-columns: 1fr;
		}

		.history-metrics,
		.history-counts {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
	}

	@media (max-width: 640px) {
		.history-route {
			padding-inline: 0.65rem;
		}

		.history-detail-drawer {
			width: calc(100vw - 0.65rem);
		}

		.history-detail-drawer__header,
		.history-detail-drawer__body {
			padding-inline: 0.75rem;
		}

		.history-episode-card__main {
			grid-template-columns: minmax(0, 1fr) auto;
		}

		.history-episode-card__main .history-badge {
			grid-column: 1 / -1;
		}

		.history-metrics,
		.history-counts,
		.history-memory-counts {
			grid-template-columns: 1fr;
		}

		.history-agent-form,
		.history-agent-form label,
		.history-agent-form .history-button,
		.history-refresh-button,
		.history-page-size,
		.history-page-size select {
			width: 100%;
		}
	}
</style>
