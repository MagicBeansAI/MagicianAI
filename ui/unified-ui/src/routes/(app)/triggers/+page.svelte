<script lang="ts">
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { onMount } from 'svelte';
	import { page } from '$app/stores';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { timedFetch } from '$lib/shared/fetch';

	interface AgentOption {
		agent_id: string;
		name: string;
	}

	interface TriggerMonitorEntry {
		agent_id: string;
		agent_name?: string;
		goal_id: string;
		trigger_seq: number;
		trigger_kind: string;
		schedule?: string;
		timezone?: string;
		event_pattern?: string;
		event_filter?: Record<string, string>;
		next_fire_at?: string;
		last_triggered_at?: string;
	}

	interface TriggerMonitorResponse {
		triggers: TriggerMonitorEntry[];
		total_count: number;
	}

	let mounted = false;
	let routeKey = '';
	let isLoading = false;
	let error: string | null = null;
	let agents: AgentOption[] = [];
	let selectedAgentId = '';
	let filterAgentId = '';
	let triggers: TriggerMonitorEntry[] = [];
	let lastRefreshAt: number | null = null;
	let latestTriggerRequestId = 0;
	let currentScopeKey = '';
	let lastScopeKey = '';
	let expandedTriggerRows = new Set<string>();

	$: selectedAgentLabel =
		selectedAgentId ? agents.find((agent) => agent.agent_id === selectedAgentId)?.name || selectedAgentId : 'All agents';
	$: summaryItems = [
		{ id: 'triggers-summary-agents', key: 'Agents', value: String(agents.length) },
		{ id: 'triggers-summary-triggers', key: 'Triggers', value: String(triggers.length) },
		{ id: 'triggers-summary-filter', key: 'Filter', value: selectedAgentLabel },
		{ id: 'triggers-summary-refresh', key: 'Last refresh', value: formatRelativeTime(lastRefreshAt) }
	];

	function asRecord(value: unknown): Record<string, unknown> | null {
		return typeof value === 'object' && value !== null && !Array.isArray(value)
			? (value as Record<string, unknown>)
			: null;
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
				const rootMessage = root ? readString(root, 'error') || readString(root, 'message') : undefined;
				message = `Request failed (${response.status}): ${rootMessage || text}`;
			} catch {
				message = `Request failed (${response.status}): ${text}`;
			}
		} catch {
			// best effort
		}
		return message;
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

	function triggerDescription(trigger: TriggerMonitorEntry): string {
		if (trigger.trigger_kind === 'cron') {
			const tz = trigger.timezone ? ` (${trigger.timezone})` : '';
			return `${trigger.schedule || 'n/a'}${tz}`;
		}
		if (trigger.trigger_kind === 'event') {
			return trigger.event_pattern || 'event subscription';
		}
		return 'idle trigger';
	}

	function eventFilterText(trigger: TriggerMonitorEntry): string {
		const filter = trigger.event_filter || {};
		const entries = Object.entries(filter);
		if (entries.length === 0) return '—';
		return entries.map(([key, value]) => `${key}=${value}`).join(', ');
	}

	function triggerRowKey(trigger: TriggerMonitorEntry): string {
		return `${trigger.agent_id}:${trigger.goal_id}:${trigger.trigger_seq}`;
	}

	function triggerDisplayName(trigger: TriggerMonitorEntry): string {
		return trigger.agent_name || trigger.agent_id;
	}

	function clearScopeState(): void {
		latestTriggerRequestId += 1;
		isLoading = false;
		error = null;
		agents = [];
		selectedAgentId = '';
		filterAgentId = '';
		triggers = [];
		lastRefreshAt = null;
		routeKey = '';
		expandedTriggerRows = new Set();
	}

	function statusClass(trigger: TriggerMonitorEntry): 'running' | 'pending' | 'idle' {
		if (trigger.trigger_kind === 'cron' && trigger.next_fire_at) return 'pending';
		return 'idle';
	}

	function statusColor(status: 'running' | 'pending' | 'idle'): 'info' | 'warning' | 'default' {
		if (status === 'running') return 'info';
		if (status === 'pending') return 'warning';
		return 'default';
	}

	function parseAgentOptionFromRecord(record: Record<string, unknown> | null): AgentOption | null {
		const definition = record ? asRecord(record.definition) : null;
		if (!definition) return null;
		const agentId = readString(definition, 'agent_id');
		if (!agentId) return null;
		return {
			agent_id: agentId,
			name: readString(definition, 'name') || agentId
		};
	}

	function sortAgentOptions(options: AgentOption[]): AgentOption[] {
		return [...options].sort((left, right) => left.name.localeCompare(right.name));
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

	function setAgentQuery(agentId: string): void {
		if (!browser) return;
		const currentAgentId = ($page.url.searchParams.get('agent_id') || '').trim();
		if (currentAgentId === agentId.trim()) return;
		const url = new URL(window.location.href);
		if (agentId) {
			url.searchParams.set('agent_id', agentId);
		} else {
			url.searchParams.delete('agent_id');
		}
		const search = url.searchParams.toString();
		const nextRoute = search ? `${url.pathname}?${search}` : url.pathname;
		void goto(nextRoute, { replaceState: true, noScroll: true, keepFocus: true });
	}

	async function loadAgents(): Promise<AgentOption[]> {
		const response = await timedFetch('/api/magician/v2/agents?limit=200');
		if (!response.ok) {
			throw new Error(await readApiError(response));
		}
		return extractAgentOptions((await response.json()) as unknown);
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

	async function loadTriggers(agentId: string): Promise<TriggerMonitorEntry[]> {
		const query = agentId ? `?agent_id=${encodeURIComponent(agentId)}` : '';
		const response = await timedFetch(`/api/magician/v2/triggers${query}`);
		if (!response.ok) {
			throw new Error(await readApiError(response));
		}
		const payload = (await response.json()) as TriggerMonitorResponse;
		return Array.isArray(payload.triggers) ? payload.triggers : [];
	}

	async function hydrateRoute(): Promise<void> {
		const requestId = ++latestTriggerRequestId;
		isLoading = true;
		error = null;
		try {
			const nextAgents = await loadAgents();
			if (requestId !== latestTriggerRequestId) return;
			const queryAgentId = ($page.url.searchParams.get('agent_id') || '').trim();
			let hydratedAgents = nextAgents;
			if (queryAgentId && !hydratedAgents.some((agent) => agent.agent_id === queryAgentId)) {
				const deepLinkedAgent = await loadAgentById(queryAgentId);
				if (requestId !== latestTriggerRequestId) return;
				if (deepLinkedAgent && !hydratedAgents.some((agent) => agent.agent_id === deepLinkedAgent.agent_id)) {
					hydratedAgents = sortAgentOptions([...hydratedAgents, deepLinkedAgent]);
				}
			}
			agents = hydratedAgents;
			let nextSelectedAgentId = selectedAgentId;
			if (queryAgentId && hydratedAgents.some((agent) => agent.agent_id === queryAgentId)) {
				nextSelectedAgentId = queryAgentId;
			} else if (queryAgentId && !hydratedAgents.some((agent) => agent.agent_id === queryAgentId)) {
				nextSelectedAgentId = '';
			} else if (nextSelectedAgentId && !hydratedAgents.some((agent) => agent.agent_id === nextSelectedAgentId)) {
				nextSelectedAgentId = '';
			}
			if (queryAgentId && queryAgentId !== nextSelectedAgentId) {
				setAgentQuery(nextSelectedAgentId);
			}
			selectedAgentId = nextSelectedAgentId;
			filterAgentId = nextSelectedAgentId;
			const nextTriggers = await loadTriggers(nextSelectedAgentId);
			if (requestId !== latestTriggerRequestId || selectedAgentId !== nextSelectedAgentId) {
				return;
			}
			triggers = nextTriggers;
			lastRefreshAt = Date.now();
		} catch (err) {
			if (requestId !== latestTriggerRequestId) return;
			error = err instanceof Error ? err.message : 'Failed to load trigger monitor';
			triggers = [];
		} finally {
			if (requestId === latestTriggerRequestId) {
				isLoading = false;
			}
		}
	}

	function badgeClass(status: 'running' | 'pending' | 'idle'): string {
		return `triggers-badge triggers-badge-${statusColor(status)}`;
	}

	function toggleTriggerRow(rowKey: string): void {
		const next = new Set(expandedTriggerRows);
		if (next.has(rowKey)) next.delete(rowKey);
		else next.add(rowKey);
		expandedTriggerRows = next;
	}

	function handleFilterSubmit(): void {
		const nextAgentId = filterAgentId.trim();
		if (nextAgentId === selectedAgentId) return;
		selectedAgentId = nextAgentId;
		setAgentQuery(selectedAgentId);
		triggers = [];
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
	<title>Triggers - Magican</title>
</svelte:head>

<div class="triggers-page">
	<section class="triggers-hero">
		<div class="triggers-copy">
			<p class="triggers-overline">Triggers</p>
			<h1>Trigger monitor</h1>
			<p>Monitor cron, event, and idle trigger registrations with next-fire visibility.</p>
		</div>
		<div class="triggers-actions">
			<button type="button" class="triggers-button triggers-button-primary" disabled={isLoading} on:click={() => hydrateRoute()}>
				{isLoading ? 'Refreshing...' : 'Refresh'}
			</button>
		</div>
	</section>

	{#if error}
		<div class="triggers-alert" role="alert">{error}</div>
	{/if}

	<section class="triggers-overview" aria-labelledby="triggers-summary-title">
		<div>
			<p class="triggers-overline">Status</p>
			<h2 id="triggers-summary-title">Summons summary</h2>
			<p>Current trigger inventory and active agent filter.</p>
		</div>
		<dl class="triggers-summary-grid">
			{#each summaryItems as item}
				<div class="triggers-summary-item">
					<dt>{item.key}</dt>
					<dd>{item.value}</dd>
				</div>
			{/each}
		</dl>
	</section>

	<section class="triggers-filter-panel" aria-labelledby="triggers-filter-title">
		<div>
			<h2 id="triggers-filter-title">Agent filter</h2>
			<p>Limit the inventory to one registered agent or show all agents.</p>
		</div>
		<form class="triggers-filter-form" on:submit|preventDefault={handleFilterSubmit}>
			<label class="triggers-field" for="triggers-agent-filter">
				<span>Agent</span>
				<select id="triggers-agent-filter" bind:value={filterAgentId} disabled={isLoading}>
					<option value="">All agents</option>
					{#each agents as agent}
						<option value={agent.agent_id}>{agent.name}</option>
					{/each}
				</select>
			</label>
			<button type="submit" class="triggers-button triggers-button-secondary" disabled={isLoading}>
				Apply
			</button>
		</form>
	</section>

	<section class="triggers-inventory" aria-labelledby="triggers-inventory-title">
		<div class="triggers-section-header">
			<div>
				<h2 id="triggers-inventory-title">Trigger inventory</h2>
				<p>{triggers.length} trigger{triggers.length === 1 ? '' : 's'} matched the current filter.</p>
			</div>
			<span class="triggers-badge triggers-badge-default">{isLoading ? 'Loading' : 'Current'}</span>
		</div>

		{#if isLoading && triggers.length === 0}
			<div class="triggers-empty">
				<h3>Loading triggers</h3>
				<p>Fetching scheduler registrations and next-fire metadata.</p>
			</div>
		{:else if triggers.length === 0}
			<div class="triggers-empty">
				<h3>No trigger registrations</h3>
				<p>No active scheduler registrations matched the current filter.</p>
			</div>
		{:else}
			<table class="triggers-table">
				<colgroup>
					<col class="col-agent" />
					<col class="col-goal" />
					<col class="col-kind" />
					<col class="col-definition" />
					<col class="col-time" />
					<col class="col-time" />
					<col class="col-state" />
				</colgroup>
				<thead>
					<tr>
						<th>Agent</th>
						<th>Goal</th>
						<th>Kind</th>
						<th>Definition</th>
						<th>Next fire</th>
						<th>Last triggered</th>
						<th>State</th>
					</tr>
				</thead>
				<tbody>
					{#each triggers as trigger (triggerRowKey(trigger))}
						{@const rowKey = triggerRowKey(trigger)}
						<tr>
							<td data-label="Agent">
								<div class="triggers-agent-cell">
									<button
										class="triggers-expand"
										type="button"
										aria-label={`${expandedTriggerRows.has(rowKey) ? 'Collapse' : 'Expand'} ${triggerDisplayName(trigger)}`}
										aria-expanded={expandedTriggerRows.has(rowKey)}
										on:click={() => toggleTriggerRow(rowKey)}
									>
										<Icon name={expandedTriggerRows.has(rowKey) ? 'chevron-up' : 'chevron-down'} size={14} />
									</button>
									<div>
										<strong>{triggerDisplayName(trigger)}</strong>
										<p class="triggers-mono">{trigger.agent_id}</p>
									</div>
								</div>
							</td>
							<td data-label="Goal">{trigger.goal_id}</td>
							<td data-label="Kind"><span class="triggers-badge triggers-badge-default">{trigger.trigger_kind}</span></td>
							<td data-label="Definition">{triggerDescription(trigger)}</td>
							<td data-label="Next fire">{formatDateTime(trigger.next_fire_at)}</td>
							<td data-label="Last triggered">{formatDateTime(trigger.last_triggered_at)}</td>
							<td data-label="State"><span class={badgeClass(statusClass(trigger))}>{statusClass(trigger)}</span></td>
						</tr>
						{#if expandedTriggerRows.has(rowKey)}
							<tr class="triggers-details-row">
								<td colspan="7">
									<dl class="triggers-detail-grid">
										<div>
											<dt>Sequence</dt>
											<dd>{trigger.trigger_seq}</dd>
										</div>
										<div>
											<dt>Event filter</dt>
											<dd>{eventFilterText(trigger)}</dd>
										</div>
										<div>
											<dt>Schedule</dt>
											<dd>{trigger.schedule || 'n/a'}</dd>
										</div>
										<div>
											<dt>Timezone</dt>
											<dd>{trigger.timezone || 'n/a'}</dd>
										</div>
										<div>
											<dt>Event pattern</dt>
											<dd>{trigger.event_pattern || 'n/a'}</dd>
										</div>
									</dl>
								</td>
							</tr>
						{/if}
					{/each}
				</tbody>
			</table>
		{/if}
	</section>
</div>

<style>
	.triggers-page {
		box-sizing: border-box;
		color: var(--text-primary);
		display: flex;
		flex-direction: column;
		gap: 1.25rem;
		margin: 0 auto;
		max-width: var(--app-content-max, 1320px);
		padding: 1.35rem 1.45rem 5rem;
		width: 100%;
	}

	.triggers-hero,
	.triggers-overview,
	.triggers-filter-panel,
	.triggers-inventory,
	.triggers-empty {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
	}

	.triggers-hero,
	.triggers-overview,
	.triggers-filter-panel,
	.triggers-inventory {
		padding: 1rem;
	}

	.triggers-hero {
		align-items: flex-start;
		display: flex;
		gap: 1rem;
		justify-content: space-between;
		background:
			radial-gradient(circle at top right, color-mix(in srgb, var(--accent-primary) 13%, transparent), transparent 42%),
			linear-gradient(180deg, color-mix(in srgb, var(--bg-card) 94%, var(--bg-soft) 6%), var(--bg-card));
	}

	.triggers-copy {
		max-width: 64rem;
	}

	.triggers-overline,
	.triggers-copy h1,
	.triggers-overview h2,
	.triggers-filter-panel h2,
	.triggers-inventory h2,
	.triggers-empty h3,
	.triggers-copy p,
	.triggers-overview p,
	.triggers-filter-panel p,
	.triggers-inventory p,
	.triggers-empty p {
		letter-spacing: 0;
		margin: 0;
	}

	.triggers-copy h1 {
		font-family: var(--font-display, var(--font-primary));
		font-size: 2.35rem;
		font-weight: 700;
		line-height: 1.1;
		margin: 0.25rem 0 0.55rem;
	}

	.triggers-overline {
		color: var(--text-secondary);
		font-size: 0.75rem;
		font-weight: 700;
		text-transform: uppercase;
	}

	.triggers-copy p,
	.triggers-overview p,
	.triggers-filter-panel p,
	.triggers-inventory p,
	.triggers-empty p {
		color: var(--text-secondary);
		font-size: 0.92rem;
		line-height: 1.5;
	}

	.triggers-overview h2,
	.triggers-filter-panel h2,
	.triggers-inventory h2,
	.triggers-empty h3 {
		color: var(--text-primary);
		font-size: 1.05rem;
		font-weight: 600;
		line-height: 1.25;
	}

	.triggers-actions,
	.triggers-section-header {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.75rem;
		justify-content: space-between;
	}

	.triggers-actions {
		justify-content: flex-end;
	}

	.triggers-overview {
		display: grid;
		gap: 1rem;
		grid-template-columns: minmax(12rem, 0.72fr) minmax(0, 2.28fr);
		background: color-mix(in srgb, var(--bg-card) 92%, var(--bg-soft) 8%);
	}

	.triggers-summary-grid,
	.triggers-detail-grid {
		display: grid;
		gap: 0.75rem;
		margin: 0;
	}

	.triggers-summary-grid {
		grid-template-columns: repeat(auto-fit, minmax(9rem, 1fr));
	}

	.triggers-summary-item,
	.triggers-detail-grid > div {
		border-left: 2px solid color-mix(in srgb, var(--accent-primary) 42%, var(--border-soft));
		min-width: 0;
		padding-left: 0.75rem;
	}

	.triggers-summary-grid dt,
	.triggers-detail-grid dt {
		color: var(--text-secondary);
		font-size: 0.74rem;
		font-weight: 600;
		line-height: 1.3;
		margin: 0;
	}

	.triggers-summary-grid dd,
	.triggers-detail-grid dd {
		color: var(--text-primary);
		font-size: 0.9rem;
		font-weight: 600;
		line-height: 1.35;
		margin: 0.15rem 0 0;
		overflow-wrap: anywhere;
	}

	.triggers-filter-panel {
		align-items: end;
		display: grid;
		gap: 1rem;
		grid-template-columns: minmax(0, 1fr) minmax(20rem, 0.75fr);
	}

	.triggers-filter-form {
		align-items: end;
		display: grid;
		gap: 0.75rem;
		grid-template-columns: minmax(0, 1fr) auto;
	}

	.triggers-field {
		color: var(--text-primary);
		display: flex;
		flex-direction: column;
		font-size: 0.82rem;
		font-weight: 600;
		gap: 0.4rem;
		min-width: 0;
	}

	.triggers-field select {
		background: color-mix(in srgb, var(--bg-card) 92%, var(--bg-soft) 8%);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-primary);
		font: inherit;
		font-size: 0.9rem;
		line-height: 1.3;
		min-height: 2.4rem;
		padding: 0.55rem 0.65rem;
		width: 100%;
	}

	.triggers-field select:focus {
		border-color: color-mix(in srgb, var(--accent-primary) 50%, var(--border-soft));
		box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent-primary) 18%, transparent);
		outline: none;
	}

	.triggers-button {
		align-items: center;
		border: 1px solid transparent;
		border-radius: 6px;
		cursor: pointer;
		display: inline-flex;
		font: inherit;
		font-size: 0.86rem;
		font-weight: 600;
		justify-content: center;
		line-height: 1.2;
		min-height: 2.25rem;
		padding: 0.55rem 0.8rem;
		white-space: nowrap;
	}

	.triggers-button-primary {
		background: var(--accent-primary);
		border-color: var(--accent-primary);
		color: var(--accent-on-primary, #fff);
	}

	.triggers-button-secondary {
		background: transparent;
		border-color: var(--border-soft);
		color: var(--text-primary);
	}

	.triggers-button:disabled,
	.triggers-field select:disabled {
		cursor: not-allowed;
		opacity: 0.58;
	}

	.triggers-button:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary) 54%, transparent);
		outline-offset: 2px;
	}

	.triggers-badge {
		align-items: center;
		border: 1px solid var(--border-soft);
		border-radius: 999px;
		color: var(--text-secondary);
		display: inline-flex;
		font-size: 0.76rem;
		font-weight: 700;
		line-height: 1.2;
		max-width: 100%;
		min-height: 1.65rem;
		padding: 0.28rem 0.55rem;
		white-space: normal;
		overflow-wrap: anywhere;
	}

	.triggers-badge-info {
		background: color-mix(in srgb, var(--accent-primary) 12%, transparent);
		border-color: color-mix(in srgb, var(--accent-primary) 28%, var(--border-soft));
		color: color-mix(in srgb, var(--accent-primary) 75%, var(--text-primary));
	}

	.triggers-badge-warning {
		background: color-mix(in srgb, var(--warning, #b7791f) 14%, transparent);
		border-color: color-mix(in srgb, var(--warning, #b7791f) 34%, var(--border-soft));
		color: color-mix(in srgb, var(--warning, #b7791f) 82%, var(--text-primary));
	}

	.triggers-badge-default {
		background: color-mix(in srgb, var(--bg-card) 88%, var(--bg-soft) 12%);
		border-color: var(--border-soft);
		color: var(--text-secondary);
	}

	.triggers-alert {
		background: color-mix(in srgb, var(--danger, #c2410c) 10%, var(--bg-card));
		border: 1px solid color-mix(in srgb, var(--danger, #c2410c) 30%, var(--border-soft));
		border-radius: 8px;
		color: var(--text-primary);
		font-size: 0.88rem;
		line-height: 1.45;
		padding: 0.75rem 0.85rem;
	}

	.triggers-inventory {
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}

	.triggers-empty {
		padding: 0.85rem;
	}

	.triggers-mono {
		color: var(--text-secondary);
		font-family: var(--font-mono);
		font-size: 0.78rem;
		line-height: 1.4;
		margin: 0.15rem 0 0;
		overflow-wrap: anywhere;
	}

	.triggers-detail-grid {
		grid-template-columns: repeat(auto-fit, minmax(12rem, 1fr));
	}

	.triggers-table {
		border-collapse: separate;
		border-spacing: 0;
		table-layout: fixed;
		width: 100%;
	}

	.triggers-table th,
	.triggers-table td {
		border-bottom: 1px solid var(--border-soft);
		padding: 0.7rem 0.65rem;
		text-align: left;
		vertical-align: top;
	}

	.triggers-table th {
		color: var(--text-secondary);
		font-size: 0.74rem;
		font-weight: 700;
		line-height: 1.25;
	}

	.triggers-table td {
		color: var(--text-primary);
		font-size: 0.86rem;
		line-height: 1.35;
		overflow-wrap: break-word;
	}

	.col-agent { width: 23%; }
	.col-goal { width: 17%; }
	.col-kind { width: 10%; }
	.col-definition { width: 19%; }
	.col-time { width: 13%; }
	.col-state { width: 8%; }

	.triggers-agent-cell {
		align-items: flex-start;
		display: flex;
		gap: 0.55rem;
		min-width: 0;
	}

	.triggers-agent-cell strong {
		color: var(--text-primary);
		display: block;
		font-size: 0.9rem;
		font-weight: 700;
		line-height: 1.25;
		overflow-wrap: anywhere;
	}

	.triggers-expand {
		align-items: center;
		background: transparent;
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-primary);
		cursor: pointer;
		display: inline-flex;
		height: 1.65rem;
		justify-content: center;
		line-height: 1;
		margin-top: 0.1rem;
		width: 1.65rem;
	}

	.triggers-details-row td {
		background: color-mix(in srgb, var(--bg-card) 90%, var(--bg-soft) 10%);
	}

	.triggers-empty {
		background: color-mix(in srgb, var(--bg-card) 90%, var(--bg-soft) 10%);
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
	}

	@media (max-width: 900px) {
		.triggers-copy h1 {
			font-size: 1.9rem;
		}

		.triggers-hero,
		.triggers-section-header {
			flex-direction: column;
		}

		.triggers-actions {
			justify-content: flex-start;
		}

		.triggers-overview,
		.triggers-filter-panel,
		.triggers-filter-form {
			grid-template-columns: minmax(0, 1fr);
		}
	}

	@media (max-width: 640px) {
		.triggers-page {
			padding: 0.85rem 0.75rem 4.5rem;
		}

		.triggers-table,
		.triggers-table thead,
		.triggers-table tbody,
		.triggers-table tr,
		.triggers-table th,
		.triggers-table td {
			display: block;
			width: 100%;
		}

		.triggers-table colgroup,
		.triggers-table thead {
			display: none;
		}

		.triggers-table tr {
			border: 1px solid var(--border-soft);
			border-radius: 8px;
			margin-bottom: 0.75rem;
			padding: 0.65rem;
		}

		.triggers-table td {
			border-bottom: 0;
			display: grid;
			gap: 0.5rem;
			grid-template-columns: minmax(7rem, 0.42fr) minmax(0, 1fr);
			padding: 0.4rem 0;
		}

		.triggers-table td::before {
			color: var(--text-secondary);
			content: attr(data-label);
			font-size: 0.74rem;
			font-weight: 700;
		}

		.triggers-table td:first-child {
			grid-template-columns: minmax(0, 1fr);
		}

		.triggers-table td:first-child::before,
		.triggers-details-row td::before {
			display: none;
		}

		.triggers-details-row td {
			display: block;
		}
	}
</style>
