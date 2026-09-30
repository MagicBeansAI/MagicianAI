<script context="module" lang="ts">
	export type WireKind = 'all' | 'event' | 'insight' | 'activity';

	export interface WireItem {
		id: string;
		kind: 'event' | 'insight' | 'activity';
		title: string;
		summary: string;
		timestamp: number;
		badge: string;
		severity?: 'info' | 'warn' | 'error' | 'success';
		href?: string;
	}
</script>

<script lang="ts">
	import { browser } from '$app/environment';
	import { onDestroy, onMount } from 'svelte';
	import { fade, slide } from 'svelte/transition';
	import { goto } from '$app/navigation';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { timedFetch } from '$lib/shared/fetch';
	import { taxonomyFor } from '$lib/realtime/event-taxonomy';
	import type { FeedItem } from '$lib/feed/types';
	import type { AgentUpdate } from '$lib/types/agentUpdate';

	// Props
	export let maxDisplay = 5;
	export let autoCycleIntervalMs = 4500;

	// State
	let isExpanded = false;
	let activeFilter: WireKind = 'all';
	let tickerIndex = 0;
	let tickerTimer: ReturnType<typeof setInterval> | null = null;
	export let eventCount24h = 0;
	let streamAbortController: AbortController | null = null;
	let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
	let mounted = false;

	// Starts empty: the wire shows only real feed items, agent updates and
	// stream events — never placeholder copy dressed up as system activity.
	export let initialItems: WireItem[] = [];
	let allItems: WireItem[] = [...initialItems].sort((a, b) => b.timestamp - a.timestamp);

	// Filtered view of latest items
	$: filteredItems = allItems
		.filter((item) => activeFilter === 'all' || item.kind === activeFilter)
		.slice(0, maxDisplay);

	// Cycle current ticker item in collapsed view
	$: currentTickerItem = filteredItems.length > 0
		? filteredItems[tickerIndex % filteredItems.length]
		: null;

	// Count per category (from all recent items)
	$: counts = {
		all: allItems.slice(0, maxDisplay).length,
		event: allItems.filter((i) => i.kind === 'event').length,
		insight: allItems.filter((i) => i.kind === 'insight').length,
		activity: allItems.filter((i) => i.kind === 'activity').length
	};

	function formatTimeAgo(ts: number): string {
		const diffSec = Math.max(0, Math.floor((Date.now() - ts) / 1000));
		if (diffSec < 45) return 'just now';
		const diffMin = Math.floor(diffSec / 60);
		if (diffMin < 60) return `${diffMin}m ago`;
		const diffHr = Math.floor(diffMin / 60);
		if (diffHr < 24) return `${diffHr}h ago`;
		return `${Math.floor(diffHr / 24)}d ago`;
	}

	function formatClockTime(ts: number): string {
		try {
			return new Date(ts).toLocaleTimeString('en-US', {
				hour: 'numeric',
				minute: '2-digit',
				second: '2-digit',
				hour12: true
			});
		} catch {
			return '';
		}
	}

	function asRecord(v: unknown): Record<string, unknown> | null {
		return v !== null && typeof v === 'object' && !Array.isArray(v)
			? (v as Record<string, unknown>)
			: null;
	}

	function stringOrNull(v: unknown): string | null {
		return typeof v === 'string' && v.trim().length > 0 ? v.trim() : null;
	}

	function titleCaseWord(w: string): string {
		if (!w) return '';
		const lower = w.toLowerCase();
		const acronyms = new Set(['llm', 'hitl', 'ui', 'api', 'id', 'cli', 'sse', 'db']);
		if (acronyms.has(lower)) return lower.toUpperCase();
		return lower.charAt(0).toUpperCase() + lower.slice(1);
	}

	function humanizeEventType(rawType: string): string {
		const cleaned = rawType.replace(/^event\./i, '').trim();
		if (!cleaned) return 'System event';
		const parts = cleaned.split('.');
		if (parts.length > 1) {
			const head = parts[0].replace(/_/g, ' ').split(/\s+/).map(titleCaseWord).join(' ');
			const tail = parts.slice(1).join(' ').replace(/_/g, ' ').split(/\s+/).map(titleCaseWord).join(' ');
			return `${head}: ${tail}`;
		}
		return cleaned.replace(/_/g, ' ').split(/\s+/).map(titleCaseWord).join(' ');
	}

	function parseTimestampCandidate(v: unknown): number | null {
		if (typeof v === 'number' && Number.isFinite(v) && v > 0) return v;
		if (typeof v === 'string') {
			const parsed = Date.parse(v);
			if (Number.isFinite(parsed) && parsed > 0) return parsed;
		}
		return null;
	}

	function formatCompact24hCount(n: number): string {
		if (n <= 0) return '0/24H';
		if (n >= 1_000_000) {
			const m = n / 1_000_000;
			return `${m >= 10 ? Math.round(m) : m.toFixed(1).replace(/\.0$/, '')}M/24H`;
		}
		if (n >= 1_000) {
			const k = n / 1_000;
			return `${k >= 10 ? Math.round(k) : k.toFixed(1).replace(/\.0$/, '')}K/24H`;
		}
		return `${n}/24H`;
	}

	function addItem(item: WireItem): void {
		// Deduplicate and insert at top
		const existingIdx = allItems.findIndex((i) => i.id === item.id);
		if (existingIdx >= 0) {
			allItems[existingIdx] = item;
		} else {
			allItems = [item, ...allItems].slice(0, 50);
		}
		// Sort newest first
		allItems.sort((a, b) => b.timestamp - a.timestamp);
		if (!isExpanded) {
			tickerIndex = 0; // Snap to newest item on incoming live dispatch
		}
	}

	function normalizeStreamEvent(raw: Record<string, unknown>): WireItem {
		const rawData = asRecord(raw.data);
		const rawPayload = asRecord(raw.payload);
		const innerEvent = asRecord(rawData?.event);
		const innerPayload = asRecord(innerEvent?.payload) ?? asRecord(rawData?.payload) ?? rawPayload;

		const outerType = stringOrNull(raw.event_type) ?? 'event';
		const innerType =
			stringOrNull(innerEvent?.event_type) ??
			(rawData && typeof rawData.event_type === 'string' && rawData.event_type !== 'AgentEvent'
				? rawData.event_type
				: null);

		const effectiveType = outerType === 'AgentEvent' && innerType ? innerType : outerType;

		// Extract entities
		const rawAgentId =
			stringOrNull(innerEvent?.agent_id) ??
			stringOrNull(rawData?.agent_id) ??
			stringOrNull(innerPayload?.agent_id) ??
			stringOrNull(raw.agent_id);
		const agentLabel = rawAgentId
			? rawAgentId.split(/[-_]+/).map(titleCaseWord).join(' ')
			: null;

		const taskId =
			stringOrNull(raw.task_id) ??
			stringOrNull(rawData?.task_id) ??
			stringOrNull(innerEvent?.task_id) ??
			stringOrNull(innerPayload?.task_id);

		const executionId =
			stringOrNull(raw.execution_id) ??
			stringOrNull(rawData?.execution_id) ??
			stringOrNull(innerEvent?.execution_id) ??
			stringOrNull(innerPayload?.execution_id);

		const stepId =
			stringOrNull(raw.step_id) ??
			stringOrNull(rawData?.step_id) ??
			stringOrNull(innerPayload?.step_id);

		const toolName =
			stringOrNull(innerPayload?.tool_name) ??
			stringOrNull(innerPayload?.tool) ??
			stringOrNull(rawData?.tool_name);

		const errorMsg =
			stringOrNull(innerPayload?.error) ??
			stringOrNull(rawData?.error) ??
			stringOrNull(raw.error);

		const outcome =
			stringOrNull(innerPayload?.outcome) ??
			stringOrNull(rawData?.outcome);

		const directMsg =
			stringOrNull(innerPayload?.message) ??
			stringOrNull(rawData?.message) ??
			stringOrNull(raw.message);

		const directSummary =
			stringOrNull(innerPayload?.summary) ??
			stringOrNull(rawData?.summary) ??
			stringOrNull(innerPayload?.description) ??
			stringOrNull(rawData?.description) ??
			stringOrNull(innerPayload?.reason) ??
			stringOrNull(rawData?.reason) ??
			stringOrNull(innerPayload?.focus_area);

		const directTitle =
			stringOrNull(innerPayload?.title) ??
			stringOrNull(rawData?.title);

		// Determine taxonomy and base severity
		const taxonomy = taxonomyFor(effectiveType !== 'AgentEvent' ? effectiveType : 'agent');
		let severity: 'info' | 'warn' | 'error' | 'success' =
			errorMsg || outcome === 'failed' || outcome === 'failure' || taxonomy.severity === 'error'
				? 'error'
				: outcome === 'warn' || taxonomy.severity === 'warn'
					? 'warn'
					: outcome === 'success' || outcome === 'done' || outcome === 'completed'
						? 'success'
						: 'info';

		let title = '';
		let summary = '';
		let badge: string = String(taxonomy.category || 'event');

		const lowerType = effectiveType.toLowerCase();

		// Specific event type handlers
		if (lowerType === 'agent.cycle.completed' || lowerType === 'agentcyclecompleted') {
			title = agentLabel ? `${agentLabel}: Cycle completed` : 'Agent cycle completed';
			summary = outcome
				? `Outcome: ${outcome}${directSummary ? ` · ${directSummary}` : ''}`
				: directSummary || directMsg || 'Autonomous cycle completed successfully';
			severity = outcome === 'failed' || outcome === 'failure' ? 'error' : 'success';
			badge = 'cycle';
		} else if (lowerType === 'agent.cycle.started' || lowerType === 'agentcyclestarted') {
			title = agentLabel ? `${agentLabel}: Started cycle` : 'Agent cycle started';
			summary = directSummary || directMsg || (taskId ? `Working on task ${taskId}` : 'Autonomous goal execution started');
			badge = 'cycle';
		} else if (lowerType === 'agent.cycle.failed') {
			title = agentLabel ? `${agentLabel}: Cycle failed` : 'Agent cycle failed';
			summary = errorMsg || directSummary || 'Cycle execution terminated with error';
			severity = 'error';
			badge = 'cycle';
		} else if (lowerType === 'agent.ui.delta') {
			title = agentLabel ? `${agentLabel}: UI updated` : 'Agent interface update';
			summary = directSummary || directMsg || 'Synchronized live agent state';
			badge = 'interface';
		} else if (lowerType.startsWith('tool.') || toolName) {
			const action = toolName ? `Tool: ${toolName}` : 'Tool execution';
			title = agentLabel ? `${agentLabel} · ${action}` : action;
			summary = directSummary || directMsg || (stepId ? `Executed in step ${stepId}` : 'Tool invocation completed');
			badge = 'tool';
		} else if (lowerType === 'execution.started' || lowerType === 'executionstarted') {
			title = 'Execution started';
			summary = taskId ? `Task ${taskId} is now running` : 'Task execution pipeline initiated';
			badge = 'exec';
		} else if (lowerType === 'execution.completed' || lowerType === 'executioncompleted') {
			title = 'Execution completed';
			summary = taskId ? `Task ${taskId} finished successfully` : 'Task execution finished';
			severity = 'success';
			badge = 'exec';
		} else if (lowerType === 'execution.failed' || lowerType === 'executionfailed') {
			title = 'Execution failed';
			summary = errorMsg || (taskId ? `Task ${taskId} failed` : 'Execution stopped with error');
			severity = 'error';
			badge = 'exec';
		} else if (lowerType === 'execution.runtime_handoff') {
			title = 'Runtime handoff';
			summary = agentLabel ? `${agentLabel} assumed runtime execution` : 'Orchestrator transferred execution';
			badge = 'runtime';
		} else if (lowerType === 'execution.responsibility_changed') {
			title = 'Responsibility changed';
			summary = agentLabel ? `Active owner: ${agentLabel}` : 'Execution ownership updated';
			badge = 'runtime';
		} else if (lowerType === 'step.started' || lowerType === 'executionstepstarted') {
			title = 'Step started';
			summary = stepId ? `Executing ${stepId}` : (taskId ? `Step for task ${taskId}` : 'Execution step running');
			badge = 'step';
		} else if (lowerType === 'step.completed' || lowerType === 'executionstepcompleted') {
			title = 'Step completed';
			summary = stepId ? `Completed ${stepId}` : (taskId ? `Step for task ${taskId}` : 'Step completed successfully');
			severity = 'success';
			badge = 'step';
		} else if (lowerType === 'step.failed') {
			title = 'Step failed';
			summary = errorMsg || (stepId ? `${stepId} failed` : 'Execution step encountered error');
			severity = 'error';
			badge = 'step';
		} else if (lowerType === 'task.status.completed' || lowerType === 'taskstatuscompleted') {
			title = 'Task completed';
			summary = taskId ? `Task ${taskId} completed` : 'Task completed successfully';
			severity = 'success';
			badge = 'task';
		} else if (lowerType === 'task.created' || lowerType === 'taskcreated') {
			title = 'Task created';
			summary = taskId ? `Task ${taskId} queued` : 'New task registered in workspace';
			badge = 'task';
		} else if (outerType === 'AgentEvent') {
			// Outer type is AgentEvent, but did not match a specific subtype
			title = agentLabel ? `${agentLabel}: Activity update` : 'Agent activity';
			summary = directSummary || directMsg || (taskId ? `Activity on task ${taskId}` : 'Autonomous action dispatched');
			badge = 'agent';
		} else {
			// Clean human-readable fallback for any other event
			const baseTitle = directTitle || humanizeEventType(effectiveType);
			title = agentLabel && !baseTitle.toLowerCase().includes(agentLabel.toLowerCase())
				? `${agentLabel}: ${baseTitle}`
				: baseTitle;
			summary = directSummary || directMsg || (stepId ? `Step: ${stepId}` : taskId ? `Task ${taskId}` : 'System telemetry dispatch');
		}

		// Deep link
		let href = '/events';
		if (executionId) href = `/events?execution_id=${encodeURIComponent(executionId)}`;
		else if (taskId) href = `/events?task_id=${encodeURIComponent(taskId)}`;

		// Timestamp resolution
		const ts =
			parseTimestampCandidate(raw.timestamp_ms) ??
			parseTimestampCandidate(raw.timestamp) ??
			parseTimestampCandidate(rawData?.timestamp_ms) ??
			parseTimestampCandidate(rawData?.timestamp) ??
			parseTimestampCandidate(innerEvent?.timestamp) ??
			parseTimestampCandidate(innerEvent?.timestamp_ms) ??
			parseTimestampCandidate(innerPayload?.timestamp_ms) ??
			parseTimestampCandidate(innerPayload?.timestamp) ??
			Date.now();

		return {
			id: `event-${effectiveType}-${ts}-${Math.random().toString(36).slice(2, 6)}`,
			kind: 'event',
			title,
			summary,
			timestamp: ts,
			badge,
			severity,
			href
		};
	}

	function normalizeFeedItem(item: FeedItem): WireItem {
		const isInsight =
			item.item_type === 'learning_insight' ||
			item.item_type === 'learning_candidate' ||
			item.item_type === 'agent_learning';

		const kind: WireItem['kind'] = isInsight ? 'insight' : 'activity';
		const href = item.task_id
			? `/tasks?selected=${encodeURIComponent(item.task_id)}`
			: `/feed?selected_item=${encodeURIComponent(item.id)}`;

		return {
			id: `feed-${item.id}`,
			kind,
			title: item.title?.trim() || (isInsight ? 'Distilled Memory' : 'Fleet Activity'),
			summary: item.summary?.trim() || (item.task_id ? `Task ${item.task_id}` : 'Feed insight recorded'),
			timestamp: item.updated_at || item.created_at || Date.now(),
			badge: item.item_type.replace(/_/g, ' '),
			severity: item.status === 'failed' ? 'error' : item.status === 'done' ? 'success' : 'info',
			href
		};
	}

	function normalizeAgentUpdate(u: AgentUpdate): WireItem {
		const title = `${u.agent_id ? titleCaseWord(u.agent_id) : 'Fleet Agent'}: ${u.kind.replace(/_/g, ' ')}`;
		let summary = '';
		if ('error' in u && typeof u.error === 'string') summary = u.error;
		else if ('focus_area' in u && typeof u.focus_area === 'string') summary = u.focus_area;
		else if ('reason' in u && typeof u.reason === 'string') summary = u.reason;
		else summary = 'Autonomous agent cycle logged';
		const href = u.thread_id ? `/t/${encodeURIComponent(u.thread_id)}` : '/feed';

		return {
			id: `agent-update-${u.id}`,
			kind: 'activity',
			title,
			summary,
			timestamp: u.ts || Date.now(),
			badge: u.kind.replace(/_/g, ' '),
			severity: 'outcome' in u && u.outcome === 'failed' ? 'error' : 'info',
			href
		};
	}

	async function load24hEventCount(): Promise<void> {
		if (!browser) return;
		try {
			const floorMs = Date.now() - 24 * 60 * 60 * 1000;
			const res = await timedFetch('/api/magician/v2/analytics/query', {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({
					sql: `SELECT COUNT(*) AS total_24h FROM events WHERE epoch_ms(timestamp) >= ${floorMs}`
				}),
				timeoutMs: 4000
			});
			if (res.ok) {
				const data = (await res.json()) as { rows?: unknown[][] };
				if (Array.isArray(data.rows) && data.rows.length > 0 && typeof data.rows[0][0] === 'number') {
					eventCount24h = data.rows[0][0];
					return;
				}
			}
		} catch {
			// Fallback handled below
		}
		// Leave the count as-is when the query fails; the stream still
		// increments it and a made-up floor would misstate activity.
	}

	async function loadInitialFeedAndUpdates(): Promise<void> {
		if (!browser) return;
		try {
			const [feedRes, updatesRes] = await Promise.allSettled([
				timedFetch('/api/magician/v2/feed?limit=15', { timeoutMs: 3500 }),
				timedFetch('/api/magician/v2/agents/updates', { timeoutMs: 3500 })
			]);

			if (feedRes.status === 'fulfilled' && feedRes.value.ok) {
				const data = (await feedRes.value.json()) as { items?: FeedItem[] };
				if (Array.isArray(data.items)) {
					for (const item of data.items) {
						addItem(normalizeFeedItem(item));
					}
				}
			}

			if (updatesRes.status === 'fulfilled' && updatesRes.value.ok) {
				const data = (await updatesRes.value.json()) as { events?: AgentUpdate[] };
				if (Array.isArray(data.events)) {
					for (const u of data.events) {
						addItem(normalizeAgentUpdate(u));
					}
				}
			}
		} catch {
			// Non-blocking for paint
		}
	}

	function disconnectEventStream(): void {
		if (streamAbortController) {
			streamAbortController.abort();
			streamAbortController = null;
		}
	}

	async function connectEventStream(): Promise<void> {
		if (!browser) return;
		disconnectEventStream();
		const controller = new AbortController();
		streamAbortController = controller;

		try {
			// Limit to 20 for instant backfill burst
			const res = await timedFetch('/api/magician/v3/events?limit=20', {
				signal: controller.signal,
				timeoutMs: 60000
			});

			if (!res.ok || !res.body) return;

			const reader = res.body.getReader();
			const decoder = new TextDecoder('utf-8');
			let buffer = '';

			while (true) {
				const { done, value } = await reader.read();
				if (controller.signal.aborted || done) break;

				buffer += decoder.decode(value, { stream: true });
				let newlineIndex: number;
				while ((newlineIndex = buffer.indexOf('\n')) !== -1) {
					const line = buffer.slice(0, newlineIndex);
					buffer = buffer.slice(newlineIndex + 1);
					if (!line.trim()) continue;
					try {
						const parsed = JSON.parse(line) as Record<string, unknown>;
						if (String(parsed.event_type ?? '').startsWith('__events_')) continue;
						addItem(normalizeStreamEvent(parsed));
						eventCount24h += 1;
					} catch {
						// Drop unparseable chunk
					}
				}
			}
		} catch (err: unknown) {
			if (!controller.signal.aborted && mounted) {
				reconnectTimer = setTimeout(() => {
					if (mounted) void connectEventStream();
				}, 6000);
			}
		}
	}

	function startTicker(): void {
		if (tickerTimer) clearInterval(tickerTimer);
		tickerTimer = setInterval(() => {
			if (!isExpanded && filteredItems.length > 0) {
				tickerIndex = (tickerIndex + 1) % filteredItems.length;
			}
		}, autoCycleIntervalMs);
	}

	function toggleExpanded(): void {
		isExpanded = !isExpanded;
	}

	function setFilter(filter: WireKind): void {
		activeFilter = filter;
		tickerIndex = 0;
	}

	function navigateToItem(href?: string): void {
		if (href) {
			void goto(href);
		}
	}

	onMount(() => {
		if (!browser) return;
		mounted = true;
		void load24hEventCount();
		void loadInitialFeedAndUpdates();
		void connectEventStream();
		startTicker();
	});

	onDestroy(() => {
		mounted = false;
		if (tickerTimer) clearInterval(tickerTimer);
		if (reconnectTimer) clearTimeout(reconnectTimer);
		disconnectEventStream();
	});
</script>

<section class="realtime-wire" aria-label="Realtime mini feed and system dispatches">
	<!-- Collapsed Single-Row Ticker Bar -->
	<!-- svelte-ignore a11y_click_events_have_key_events -->
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div
		class="wire-bar"
		class:is-expanded={isExpanded}
		on:click={toggleExpanded}
	>
		<!-- Left: Blinking Green Dot -->
		<div class="wire-bar__meta">
			<span class="live-dot" title="Live stream active" aria-label="Live stream active"></span>
		</div>

		<!-- Center: Scrolling Ticker Content -->
		<div class="wire-bar__ticker" aria-live="polite">
			{#if currentTickerItem}
				{#key currentTickerItem.id}
					<div class="ticker-item" in:fade={{ duration: 240 }}>
						<span class="kind-tag kind-tag--{currentTickerItem.kind}">
							{currentTickerItem.kind.toUpperCase()}
						</span>
						<strong class="ticker-title">{currentTickerItem.title}</strong>
						{#if currentTickerItem.summary}
							<span class="ticker-sep">·</span>
							<span class="ticker-summary">{currentTickerItem.summary}</span>
						{/if}
						<span class="ticker-time">({formatTimeAgo(currentTickerItem.timestamp)})</span>
					</div>
				{/key}
			{:else}
				<div class="ticker-item ticker-item--empty">
					<span>Awaiting incoming transmissions…</span>
				</div>
			{/if}
		</div>

		<!-- Right: 24H Event Count + Expand Toggle -->
		<div class="wire-bar__actions">
			<span class="count-chip" title="Total system events in the last 24 hours ({eventCount24h.toLocaleString()} events)">
				{formatCompact24hCount(eventCount24h)}
			</span>
			<button
				type="button"
				class="wire-toggle-btn"
				aria-expanded={isExpanded}
				aria-label={isExpanded ? 'Collapse realtime wire' : 'Expand realtime wire to view latest 5 events'}
				on:click|stopPropagation={toggleExpanded}
			>
				<span class="toggle-label">{isExpanded ? 'Hide' : 'Latest 5'}</span>
				<Icon name={isExpanded ? 'chevron-up' : 'chevron-down'} size={14} />
			</button>
		</div>
	</div>

	<!-- Expanded Drawer Panel with Filter & Latest 5 Items -->
	{#if isExpanded}
		<div class="wire-drawer" transition:slide={{ duration: 250 }}>
			<div class="wire-drawer__header">
				<!-- Category Filter Tabs -->
				<div class="filter-tabs" role="tablist" aria-label="Filter realtime stream">
					<button
						type="button"
						role="tab"
						class="filter-tab"
						class:active={activeFilter === 'all'}
						aria-selected={activeFilter === 'all'}
						on:click={() => setFilter('all')}
					>
						All <span class="tab-count">{counts.all}</span>
					</button>
					<button
						type="button"
						role="tab"
						class="filter-tab"
						class:active={activeFilter === 'event'}
						aria-selected={activeFilter === 'event'}
						on:click={() => setFilter('event')}
					>
						Events <span class="tab-count">{counts.event}</span>
					</button>
					<button
						type="button"
						role="tab"
						class="filter-tab"
						class:active={activeFilter === 'insight'}
						aria-selected={activeFilter === 'insight'}
						on:click={() => setFilter('insight')}
					>
						Insights <span class="tab-count">{counts.insight}</span>
					</button>
					<button
						type="button"
						role="tab"
						class="filter-tab"
						class:active={activeFilter === 'activity'}
						aria-selected={activeFilter === 'activity'}
						on:click={() => setFilter('activity')}
					>
						Activity <span class="tab-count">{counts.activity}</span>
					</button>
				</div>

				<!-- Deep Links & Close Action -->
				<div class="drawer-links">
					<a href="/events" class="drawer-link" title="Open full event stream">
						<span>/events stream</span>
						<Icon name="arrow-up-right" size={11} />
					</a>
					<a href="/feed" class="drawer-link" title="Open full feed">
						<span>/feed</span>
						<Icon name="arrow-up-right" size={11} />
					</a>
				</div>
			</div>

			<!-- List of Latest 5 Dispatches -->
			<div class="wire-drawer__list" role="list">
				{#each filteredItems as item (item.id)}
					<button
						type="button"
						class="wire-card wire-card--{item.kind}"
						class:is-clickable={!!item.href}
						on:click={() => navigateToItem(item.href)}
					>
						<div class="wire-card__header">
							<div class="wire-card__kind-row">
								<span class="kind-tag kind-tag--{item.kind}">
									{item.kind.toUpperCase()}
								</span>
								{#if item.badge}
									<span class="item-badge">{item.badge}</span>
								{/if}
								{#if item.severity === 'error'}
									<span class="status-indicator status-indicator--error">error</span>
								{:else if item.severity === 'success'}
									<span class="status-indicator status-indicator--success">completed</span>
								{/if}
							</div>
							<div class="wire-card__time" title={formatClockTime(item.timestamp)}>
								<Icon name="clock" size={11} />
								<span>{formatTimeAgo(item.timestamp)}</span>
							</div>
						</div>

						<div class="wire-card__body">
							<strong class="wire-card__title">{item.title}</strong>
							{#if item.summary}
								<p class="wire-card__summary">{item.summary}</p>
							{/if}
						</div>
					</button>
				{/each}

				{#if filteredItems.length === 0}
					<div class="wire-empty">
						<span>No recent {activeFilter === 'all' ? 'dispatches' : activeFilter} recorded.</span>
					</div>
				{/if}
			</div>
		</div>
	{/if}
</section>

<style>
	.realtime-wire {
		width: 100%;
		box-sizing: border-box;
		margin: 0;
		background: color-mix(in srgb, var(--bg-card, #ffffff) 92%, transparent);
		border: 1px solid color-mix(in srgb, var(--border-soft, #e2e8f0) 85%, transparent);
		border-radius: var(--radius-sm, 6px);
		box-shadow: 0 1px 3px rgba(0, 0, 0, 0.03);
		overflow: hidden;
		transition: border-color 0.15s ease, box-shadow 0.15s ease;
	}

	.realtime-wire:hover {
		border-color: color-mix(in srgb, var(--border-soft, #e2e8f0) 100%, transparent);
		box-shadow: 0 2px 6px rgba(0, 0, 0, 0.05);
	}

	/* --- Collapsed Bar --- */
	.wire-bar {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		padding: 0.4rem 0.85rem;
		cursor: pointer;
		min-height: 2.25rem;
		user-select: none;
	}

	.wire-bar.is-expanded {
		border-bottom: 1px solid color-mix(in srgb, var(--border-soft, #e2e8f0) 80%, transparent);
		background: color-mix(in srgb, var(--bg-card, #ffffff) 96%, transparent);
	}

	.wire-bar__meta {
		display: flex;
		align-items: center;
		flex-shrink: 0;
	}

	@keyframes pulse-dot {
		0%, 100% {
			transform: scale(1);
			opacity: 1;
		}
		50% {
			transform: scale(1.35);
			opacity: 0.5;
		}
	}

	.live-dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background-color: #22c55e;
		box-shadow: 0 0 6px #22c55e;
		animation: pulse-dot 2s infinite ease-in-out;
		flex-shrink: 0;
	}

	/* --- Ticker Center --- */
	.wire-bar__ticker {
		flex: 1;
		min-width: 0;
		height: 1.4rem;
		overflow: hidden;
		display: flex;
		align-items: center;
		position: relative;
	}

	.ticker-item {
		display: inline-flex;
		align-items: center;
		gap: 0.45rem;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		font-size: 0.8rem;
		width: 100%;
	}

	.ticker-title {
		font-weight: 600;
		color: var(--text-primary, #0f172a);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.ticker-sep {
		color: var(--text-muted, #94a3b8);
	}

	.ticker-summary {
		color: var(--text-secondary, #475569);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: 0.78rem;
	}

	.ticker-time {
		font-family: var(--font-mono, monospace);
		font-size: 0.72rem;
		color: var(--text-muted, #94a3b8);
		flex-shrink: 0;
	}

	.ticker-item--empty {
		color: var(--text-muted, #94a3b8);
		font-style: italic;
		font-size: 0.78rem;
	}

	/* --- Actions & 24h Count Aligned Right --- */
	.wire-bar__actions {
		display: flex;
		align-items: center;
		gap: 0.55rem;
		flex-shrink: 0;
		margin-left: auto;
	}

	.count-chip {
		font-family: var(--font-mono, monospace);
		font-size: 0.7rem;
		font-weight: 700;
		color: var(--text-muted, #64748b);
		background: color-mix(in srgb, var(--bg-soft, #f8fafc) 80%, transparent);
		padding: 0.12rem 0.45rem;
		border-radius: 4px;
		border: 1px solid color-mix(in srgb, var(--border-soft, #e2e8f0) 65%, transparent);
		white-space: nowrap;
		letter-spacing: 0.05em;
	}

	.wire-toggle-btn {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		font-family: var(--font-mono, monospace);
		font-size: 0.72rem;
		font-weight: 600;
		color: var(--text-muted, #64748b);
		background: transparent;
		border: 1px solid color-mix(in srgb, var(--border-soft, #e2e8f0) 70%, transparent);
		padding: 0.2rem 0.5rem;
		border-radius: 4px;
		cursor: pointer;
		transition: background 0.15s ease, color 0.15s ease;
	}

	.wire-toggle-btn:hover {
		background: color-mix(in srgb, var(--bg-soft, #f1f5f9) 90%, transparent);
		color: var(--text-primary, #0f172a);
	}

	/* --- Kind Tags --- */
	.kind-tag {
		font-family: var(--font-mono, monospace);
		font-size: 0.65rem;
		font-weight: 700;
		padding: 0.1rem 0.35rem;
		border-radius: 3px;
		letter-spacing: 0.04em;
		flex-shrink: 0;
	}

	.kind-tag--event {
		background: color-mix(in srgb, #0ea5e9 14%, transparent);
		color: #0284c7;
		border: 1px solid color-mix(in srgb, #0ea5e9 25%, transparent);
	}

	:global([data-theme^="dark"]) .kind-tag--event {
		color: #38bdf8;
	}

	.kind-tag--insight {
		background: color-mix(in srgb, #f59e0b 14%, transparent);
		color: #d97706;
		border: 1px solid color-mix(in srgb, #f59e0b 25%, transparent);
	}

	:global([data-theme^="dark"]) .kind-tag--insight {
		color: #fbbf24;
	}

	.kind-tag--activity {
		background: color-mix(in srgb, #10b981 14%, transparent);
		color: #059669;
		border: 1px solid color-mix(in srgb, #10b981 25%, transparent);
	}

	:global([data-theme^="dark"]) .kind-tag--activity {
		color: #34d399;
	}

	/* --- Expanded Drawer --- */
	.wire-drawer {
		padding: 0.75rem 0.9rem 0.9rem;
		background: color-mix(in srgb, var(--bg-card, #ffffff) 98%, transparent);
	}

	.wire-drawer__header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 1rem;
		margin-bottom: 0.75rem;
		flex-wrap: wrap;
	}

	.filter-tabs {
		display: flex;
		gap: 0.35rem;
		align-items: center;
	}

	.filter-tab {
		font-family: var(--font-mono, monospace);
		font-size: 0.72rem;
		font-weight: 500;
		padding: 0.2rem 0.55rem;
		border-radius: 4px;
		background: transparent;
		border: 1px solid color-mix(in srgb, var(--border-soft, #e2e8f0) 80%, transparent);
		color: var(--text-muted, #64748b);
		cursor: pointer;
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		transition: all 0.15s ease;
	}

	.filter-tab:hover {
		color: var(--text-primary, #0f172a);
		border-color: var(--border-soft, #cbd5e1);
	}

	.filter-tab.active {
		background: var(--text-primary, #0f172a);
		color: var(--bg-base, #ffffff);
		border-color: var(--text-primary, #0f172a);
		font-weight: 600;
	}

	:global([data-theme^="dark"]) .filter-tab.active {
		background: #f8fafc;
		color: #0f172a;
		border-color: #f8fafc;
	}

	.tab-count {
		font-size: 0.65rem;
		opacity: 0.8;
	}

	.drawer-links {
		display: flex;
		align-items: center;
		gap: 0.75rem;
	}

	.drawer-link {
		font-family: var(--font-mono, monospace);
		font-size: 0.72rem;
		color: var(--accent-primary, #b45309);
		text-decoration: none;
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		transition: opacity 0.15s ease;
	}

	.drawer-link:hover {
		text-decoration: underline;
		opacity: 0.85;
	}

	/* --- Cards List --- */
	.wire-drawer__list {
		display: flex;
		flex-direction: column;
		gap: 0.45rem;
	}

	.wire-card {
		width: 100%;
		box-sizing: border-box;
		font: inherit;
		padding: 0.6rem 0.75rem;
		border-radius: var(--radius-sm, 5px);
		background: color-mix(in srgb, var(--bg-card, #ffffff) 65%, transparent);
		border: 1px solid color-mix(in srgb, var(--border-soft, #e2e8f0) 65%, transparent);
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		transition: transform 0.1s ease, border-color 0.15s ease, background 0.15s ease;
		text-align: left;
	}

	.wire-card.is-clickable {
		cursor: pointer;
	}

	.wire-card.is-clickable:hover {
		border-color: color-mix(in srgb, var(--border-soft, #cbd5e1) 90%, transparent);
		background: color-mix(in srgb, var(--bg-soft, #f8fafc) 90%, transparent);
		transform: translateY(-1px);
	}

	.wire-card__header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 0.5rem;
	}

	.wire-card__kind-row {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		flex-wrap: wrap;
	}

	.item-badge {
		font-family: var(--font-mono, monospace);
		font-size: 0.65rem;
		color: var(--text-muted, #64748b);
		background: color-mix(in srgb, var(--bg-soft, #f1f5f9) 90%, transparent);
		padding: 0.05rem 0.35rem;
		border-radius: 3px;
	}

	.status-indicator {
		font-family: var(--font-mono, monospace);
		font-size: 0.62rem;
		font-weight: 600;
		padding: 0.05rem 0.35rem;
		border-radius: 3px;
		text-transform: uppercase;
	}

	.status-indicator--error {
		background: color-mix(in srgb, #ef4444 15%, transparent);
		color: #ef4444;
	}

	.status-indicator--success {
		background: color-mix(in srgb, #10b981 15%, transparent);
		color: #10b981;
	}

	.wire-card__time {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		font-family: var(--font-mono, monospace);
		font-size: 0.68rem;
		color: var(--text-muted, #94a3b8);
		flex-shrink: 0;
	}

	.wire-card__body {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
	}

	.wire-card__title {
		font-size: 0.82rem;
		font-weight: 600;
		color: var(--text-primary, #0f172a);
		line-height: 1.35;
	}

	.wire-card__summary {
		margin: 0;
		font-size: 0.76rem;
		color: var(--text-secondary, #475569);
		line-height: 1.4;
		overflow: hidden;
		text-overflow: ellipsis;
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
	}

	.wire-empty {
		padding: 1.25rem;
		text-align: center;
		color: var(--text-muted, #94a3b8);
		font-size: 0.8rem;
		font-style: italic;
	}

	@media (max-width: 640px) {
		.wire-bar__meta .sep,
		.count-chip {
			display: none;
		}
		.toggle-label {
			display: none;
		}
	}
</style>
