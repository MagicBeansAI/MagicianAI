<script lang="ts">
	import { PRODUCT_NAME } from '$lib/presentationIdentity';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { onDestroy, onMount, tick } from 'svelte';
	import { get } from 'svelte/store';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { createFeedStore } from '$lib/stores/feedStore';
	import { showError, showInfo, showSuccess } from '$lib/shared/stores/notifications';
	import {
		clearScope,
		seedScope,
		updatesForScope
	} from '$lib/stores/agentUpdateStore';
	import type { FeedItem } from '$lib/feed/types';
	import {
		evidenceHrefIsExternal,
		learningCandidateId,
		learningEvidenceHref,
		readMetadataString,
		titleCase
	} from '$lib/feed/learningCards';
	import type { AgentUpdate, AgentUpdateKindTag } from '$lib/types/agentUpdate';
	import AgentUpdateCard from '$lib/magician/components/agent-updates/AgentUpdateCard.svelte';
	import LearningCandidateFeedCard from '$lib/magician/components/feed/LearningCandidateFeedCard.svelte';
	import LearningInsightFeedCard from '$lib/magician/components/feed/LearningInsightFeedCard.svelte';
	import { timedFetch } from '$lib/shared/fetch';

	interface AgentUpdatesResponse {
		count: number;
		events: AgentUpdate[];
	}

	let principal = '';
	let workspace = '';
	let loading = false;
	let error: string | null = null;
	let events: AgentUpdate[] = [];
	let activeTab: 'insights' | 'review' | 'activity' = 'insights';
	let insightActionKey: string | null = null;
	let appliedSelectedItemId: string | null = null;
	let selectedItemFocusInFlight: string | null = null;
	let selectedItemFocusAttemptKey: string | null = null;
	let highlightedItemId: string | null = null;
	let highlightTimeout: ReturnType<typeof setTimeout> | null = null;
	let feedCardRefs = new Map<string, HTMLElement>();
	const learningFeed = createFeedStore({ limit: 200 });

	// Filters (Phase 2)
	let kindFilter: AgentUpdateKindTag | '' = '';
	let agentFilter = '';
	let threadFilter = '';
	let hideResolved = true;

	/** Groups of consecutive same-kind+agent events after filtering. */
	interface EventGroup {
		key: string;
		events: AgentUpdate[];
	}

	/** Expanded group keys — when a collapsed group is clicked open. */
	let expandedGroups = new Set<string>();

	$: availableKinds = uniqueSorted(events.map((e) => e.kind));
	$: availableAgents = uniqueSorted(events.map((e) => e.agent_id));
	$: availableThreads = uniqueSorted(
		events.map((e) => e.thread_id).filter((v): v is string => Boolean(v))
	);
	$: resolvedApprovalIds = collectResolvedApprovalIds(events);
	$: filteredEvents = applyFilters(
		events,
		kindFilter,
		agentFilter,
		threadFilter,
		hideResolved,
		resolvedApprovalIds
	);
	$: groupedEvents = groupConsecutive(filteredEvents);
	$: mutedCount = hideResolved ? countResolvedRequests(events, resolvedApprovalIds) : 0;
	$: learningInsights = $learningFeed.items.filter((item) => item.item_type === 'learning_insight');
	$: reviewLearnings = $learningFeed.items.filter((item) => item.item_type === 'learning_candidate');
	$: insightMetrics = buildInsightMetrics(learningInsights);
	$: digestGroups = buildDigestGroups(learningInsights);
	$: selectedItemId = (($page.url.searchParams.get('selected_item') || '').trim() || null);
	$: selectedItemFocusKey = selectedItemId
		? `${selectedItemId}:${$learningFeed.items.map((item) => item.id).join('|')}`
		: null;
	$: if (
		selectedItemId &&
		selectedItemId !== appliedSelectedItemId &&
		selectedItemId !== selectedItemFocusInFlight &&
		selectedItemFocusKey !== selectedItemFocusAttemptKey &&
		$learningFeed.items.length > 0
	) {
		selectedItemFocusAttemptKey = selectedItemFocusKey;
		void focusSelectedItemWhenReady(selectedItemId);
	}
	$: if (!selectedItemId && appliedSelectedItemId) {
		appliedSelectedItemId = null;
		selectedItemFocusAttemptKey = null;
	}

	function uniqueSorted(values: string[]): string[] {
		return Array.from(new Set(values)).sort();
	}

	/** Approval IDs that have either an `approval_resolved` or
	 * `approval_expired` follow-up; those requests can be muted. */
	function collectResolvedApprovalIds(list: AgentUpdate[]): Set<string> {
		const ids = new Set<string>();
		for (const event of list) {
			if (
				(event.kind === 'approval_resolved' || event.kind === 'approval_expired') &&
				typeof event.approval_id === 'string'
			) {
				ids.add(event.approval_id);
			}
		}
		return ids;
	}

	function countResolvedRequests(
		list: AgentUpdate[],
		resolved: Set<string>
	): number {
		let n = 0;
		for (const event of list) {
			if (event.kind === 'approval_requested' && resolved.has(event.approval_id)) {
				n += 1;
			}
		}
		return n;
	}

	function applyFilters(
		list: AgentUpdate[],
		kind: AgentUpdateKindTag | '',
		agent: string,
		thread: string,
		mute: boolean,
		resolved: Set<string>
	): AgentUpdate[] {
		return list.filter((event) => {
			if (kind && event.kind !== kind) return false;
			if (agent && event.agent_id !== agent) return false;
			if (thread && event.thread_id !== thread) return false;
			if (
				mute &&
				event.kind === 'approval_requested' &&
				resolved.has(event.approval_id)
			) {
				return false;
			}
			return true;
		});
	}

	/** Collapse consecutive events that share the same `kind + agent_id` into
	 * one group. Preserves chronological order within a group. Single events
	 * render as ungrouped cards; groups of 2+ render as an expanding stack. */
	function groupConsecutive(list: AgentUpdate[]): EventGroup[] {
		const groups: EventGroup[] = [];
		for (const event of list) {
			const key = `${event.kind}::${event.agent_id}`;
			const last = groups[groups.length - 1];
			if (last && last.key === key) {
				last.events.push(event);
			} else {
				groups.push({ key, events: [event] });
			}
		}
		return groups;
	}

	function toggleGroup(groupId: string): void {
		const next = new Set(expandedGroups);
		if (next.has(groupId)) next.delete(groupId);
		else next.add(groupId);
		expandedGroups = next;
	}

	function clearFilters(): void {
		kindFilter = '';
		agentFilter = '';
		threadFilter = '';
	}

	let currentFeed = updatesForScope(principal, workspace);
	let currentFeedUnsub: (() => void) | null = null;

	function resubscribeFeed(p: string, w: string): void {
		currentFeedUnsub?.();
		currentFeed = updatesForScope(p, w);
		currentFeedUnsub = currentFeed.subscribe((value) => {
			events = value;
		});
	}

	async function loadInitial(p: string, w: string): Promise<void> {
		if (!p || !w) {
			error = 'No scope selected.';
			return;
		}
		loading = true;
		error = null;
		try {
			const params = new URLSearchParams({
				limit: '200'
			});
			const res = await timedFetch(`/api/magician/v2/updates?${params.toString()}`);
			if (!res.ok) {
				throw new Error(`HTTP ${res.status}`);
			}
			const body = (await res.json()) as AgentUpdatesResponse;
			seedScope(p, w, body.events ?? []);
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		} finally {
			loading = false;
		}
	}

	async function handleScopeChange(nextPrincipal: string, nextWorkspace: string): Promise<void> {
		if (nextPrincipal === principal && nextWorkspace === workspace) return;
		// Reset the previous scope's window; a seed on the new scope repopulates.
		if (principal && workspace) {
			clearScope(principal, workspace);
		}
		principal = nextPrincipal;
		workspace = nextWorkspace;
		resubscribeFeed(principal, workspace);
		await loadInitial(principal, workspace);
	}

	const scopeUnsub = scopeIdentityStore.subscribe((state) => {
		if (!state.isResolved) return;
		void handleScopeChange(state.principal, state.workspace);
	});

	onMount(() => {
		learningFeed.start();
		const initial = get(scopeIdentityStore);
		if (initial.isResolved) {
			void handleScopeChange(initial.principal, initial.workspace);
		}
	});

	onDestroy(() => {
		scopeUnsub();
		currentFeedUnsub?.();
		learningFeed.stop();
		if (highlightTimeout) clearTimeout(highlightTimeout);
	});

	function formatTs(ts: number): string {
		try {
			const delta = Date.now() - ts;
			if (delta < 60_000) return 'just now';
			if (delta < 3_600_000) return `${Math.round(delta / 60_000)}m ago`;
			if (delta < 86_400_000) return `${Math.round(delta / 3_600_000)}h ago`;
			return new Date(ts).toLocaleString();
		} catch {
			return '';
		}
	}

	interface InsightMetric {
		label: string;
		value: string;
	}

	interface DigestGroup {
		key: string;
		label: string;
		count: number;
		latestTitle: string;
	}

	function buildInsightMetrics(items: FeedItem[]): InsightMetric[] {
		const needsAction = items.filter((item) => item.status === 'needs_action').length;
		const failed = items.filter((item) => item.status === 'failed').length;
		const withEvidence = items.filter((item) => {
			const metadata = typeof item.metadata === 'object' && item.metadata && !Array.isArray(item.metadata)
				? (item.metadata as Record<string, unknown>)
				: {};
			return Array.isArray(metadata.evidence_refs) && metadata.evidence_refs.length > 0;
		}).length;
		return [
			{ label: 'Insights', value: String(items.length) },
			{ label: 'Actionable', value: String(needsAction) },
			{ label: 'Warnings', value: String(failed) },
			{ label: 'With evidence', value: String(withEvidence) }
		];
	}

	function buildDigestGroups(items: FeedItem[]): DigestGroup[] {
		const groups = new Map<string, DigestGroup>();
		for (const item of items) {
			const kind = readMetadataString(item, 'insight_kind') || readMetadataString(item, 'source_type') || 'insight';
			const key = kind.toLowerCase();
			const existing = groups.get(key);
			if (!existing) {
				groups.set(key, {
					key,
					label: titleCase(kind),
					count: 1,
					latestTitle: item.title
				});
			} else {
				existing.count += 1;
			}
		}
		return Array.from(groups.values())
			.filter((group) => group.count > 1)
			.sort((left, right) => right.count - left.count)
			.slice(0, 6);
	}

	function registerFeedCard(node: HTMLElement, itemId: string) {
		feedCardRefs.set(itemId, node);
		return {
			destroy() {
				feedCardRefs.delete(itemId);
			}
		};
	}

	function itemMatchesSelectedId(item: FeedItem, selectedId: string): boolean {
		if (item.id === selectedId) return true;
		if (learningCandidateId(item) === selectedId) return true;
		return [
			'insight_id',
			'candidate_id',
			'source_id',
			'event_id',
			'run_id',
			'backlog_id',
			'suite_id',
			'related_candidate_id'
		].some((key) => readMetadataString(item, key) === selectedId);
	}

	async function focusSelectedItemWhenReady(itemId: string): Promise<void> {
		selectedItemFocusInFlight = itemId;
		try {
			if (await focusSelectedItem(itemId)) {
				appliedSelectedItemId = itemId;
			}
		} finally {
			if (selectedItemFocusInFlight === itemId) {
				selectedItemFocusInFlight = null;
			}
		}
	}

	async function focusSelectedItem(itemId: string): Promise<boolean> {
		const item = $learningFeed.items.find((candidate) => itemMatchesSelectedId(candidate, itemId));
		if (!item) return false;
		const resolvedItemId = item?.id || itemId;
		if (item?.item_type === 'learning_candidate') {
			activeTab = 'review';
		} else if (item?.item_type === 'learning_insight') {
			activeTab = 'insights';
		}
		await tick();
		const node = feedCardRefs.get(resolvedItemId);
		if (!node) return false;
		node.scrollIntoView({ block: 'center', behavior: 'smooth' });
		highlightedItemId = resolvedItemId;
		if (highlightTimeout) clearTimeout(highlightTimeout);
		highlightTimeout = setTimeout(() => {
			highlightedItemId = null;
			highlightTimeout = null;
		}, 2200);
		return true;
	}

	async function openLearningEvidence(item: FeedItem): Promise<void> {
		const href = learningEvidenceHref(item);
		if (evidenceHrefIsExternal(href)) {
			window.open(href, '_blank', 'noreferrer');
			return;
		}
		await goto(href, { replaceState: false, noScroll: true });
	}

	async function openCandidate(item: FeedItem): Promise<void> {
		const candidateId = learningCandidateId(item);
		const suffix = candidateId ? `?candidate=${encodeURIComponent(candidateId)}` : '';
		await goto(`/memory${suffix}`, { replaceState: false, noScroll: true });
	}

	async function archiveInsight(item: FeedItem): Promise<void> {
		const actionKey = `${item.id}:archive`;
		insightActionKey = actionKey;
		try {
			await learningFeed.archiveLearningInsight(item.id, 'Archived from the learning feed.');
			showInfo('Insight archived.');
		} catch (err) {
			showError(err instanceof Error ? err.message : 'Failed to archive insight');
		} finally {
			if (insightActionKey === actionKey) insightActionKey = null;
		}
	}

	async function saveInsight(item: FeedItem): Promise<void> {
		const actionKey = `${item.id}:memory`;
		insightActionKey = actionKey;
		try {
			await learningFeed.saveLearningInsightToMemory(item.id, item.summary || item.title);
			showSuccess('Insight queued for memory review.');
		} catch (err) {
			showError(err instanceof Error ? err.message : 'Failed to save insight');
		} finally {
			if (insightActionKey === actionKey) insightActionKey = null;
		}
	}

	async function createInsightTask(item: FeedItem): Promise<void> {
		const actionKey = `${item.id}:task`;
		insightActionKey = actionKey;
		try {
			await learningFeed.createLearningInsightFollowUp(item.id);
			showSuccess('Follow-up task created.');
		} catch (err) {
			showError(err instanceof Error ? err.message : 'Failed to create task');
		} finally {
			if (insightActionKey === actionKey) insightActionKey = null;
		}
	}

	async function archiveCandidate(item: FeedItem): Promise<void> {
		const candidateId = learningCandidateId(item);
		if (!candidateId) {
			showError('This learning card is missing its candidate id.');
			return;
		}
		const actionKey = `${candidateId}:archive`;
		insightActionKey = actionKey;
		try {
			await learningFeed.archiveLearningCandidate(candidateId, 'Archived from the learning feed.');
			showInfo('Learning archived.');
		} catch (err) {
			showError(err instanceof Error ? err.message : 'Failed to archive learning');
		} finally {
			if (insightActionKey === actionKey) insightActionKey = null;
		}
	}

	async function confirmCandidate(item: FeedItem): Promise<void> {
		const candidateId = learningCandidateId(item);
		if (!candidateId) {
			showError('This learning card is missing its candidate id.');
			return;
		}
		const actionKey = `${candidateId}:confirm`;
		insightActionKey = actionKey;
		try {
			await learningFeed.confirmLearningCandidate(candidateId);
			showSuccess('Learning filed to memory.');
		} catch (err) {
			showError(err instanceof Error ? err.message : 'Failed to file learning');
		} finally {
			if (insightActionKey === actionKey) insightActionKey = null;
		}
	}
</script>

<svelte:head>
	<title>Feed · {PRODUCT_NAME}</title>
</svelte:head>

<div class="feed-root presto-gaui-page">
	<header class="feed-header">
		<h1>Agent feed</h1>
		<p class="feed-subtitle">
			{#if principal && workspace}
				<code>{principal}</code> · <code>{workspace}</code>
			{:else}
				No scope selected
			{/if}
			{#if events.length > 0}
				· {filteredEvents.length}{filteredEvents.length !== events.length ? ` of ${events.length}` : ''} event{events.length === 1 ? '' : 's'}
			{/if}
		</p>
	</header>

	<div class="feed-tabs" role="tablist" aria-label="Feed sections">
		<button
			type="button"
			class:active={activeTab === 'insights'}
			on:click={() => (activeTab = 'insights')}
		>
			Insights <span>{learningInsights.length}</span>
		</button>
		<button
			type="button"
			class:active={activeTab === 'review'}
			on:click={() => (activeTab = 'review')}
		>
			Review <span>{reviewLearnings.length}</span>
		</button>
		<button
			type="button"
			class:active={activeTab === 'activity'}
			on:click={() => (activeTab = 'activity')}
		>
			Activity <span>{events.length}</span>
		</button>
	</div>

	{#if activeTab === 'insights'}
		{#if $learningFeed.isLoading && learningInsights.length === 0}
			<div class="feed-state">Loading insights…</div>
		{:else if learningInsights.length === 0}
			<div class="feed-state">No learning insights yet.</div>
		{:else}
			<section class="learning-insight-overview" aria-label="Learning insight overview">
				<div class="learning-insight-metrics">
					{#each insightMetrics as metric}
						<div class="learning-insight-metric">
							<strong>{metric.value}</strong>
							<span>{metric.label}</span>
						</div>
					{/each}
				</div>
				{#if digestGroups.length > 0}
					<div class="learning-digest">
						<strong>Digest</strong>
						<div>
							{#each digestGroups as group}
								<span title={group.latestTitle}>{group.count} {group.label}</span>
							{/each}
						</div>
					</div>
				{/if}
			</section>
			<div class="learning-feed-grid">
				{#each learningInsights as item (item.id)}
					<div use:registerFeedCard={item.id}>
						<LearningInsightFeedCard
							{item}
							actionKey={insightActionKey}
							highlighted={highlightedItemId === item.id}
							on:openEvidence={() => void openLearningEvidence(item)}
							on:save={() => void saveInsight(item)}
							on:createTask={() => void createInsightTask(item)}
							on:archive={() => void archiveInsight(item)}
						/>
					</div>
				{/each}
			</div>
		{/if}
	{:else if activeTab === 'review'}
		{#if $learningFeed.isLoading && reviewLearnings.length === 0}
			<div class="feed-state">Loading review queue…</div>
		{:else if reviewLearnings.length === 0}
			<div class="feed-state">No memory learnings need review.</div>
		{:else}
			<div class="learning-feed-grid">
				{#each reviewLearnings as item (item.id)}
					<div use:registerFeedCard={item.id}>
						<LearningCandidateFeedCard
							{item}
							actionKey={insightActionKey}
							showEdit={false}
							highlighted={highlightedItemId === item.id}
							on:open={() => void openCandidate(item)}
							on:openEvidence={() => void openLearningEvidence(item)}
							on:confirm={() => void confirmCandidate(item)}
							on:archive={() => void archiveCandidate(item)}
						/>
					</div>
				{/each}
			</div>
		{/if}
	{:else}

	{#if events.length > 0}
		<div class="feed-filters">
			<label class="feed-filter">
				<span class="feed-filter-label">Kind</span>
				<select bind:value={kindFilter}>
					<option value="">All</option>
					{#each availableKinds as kind}
						<option value={kind}>{kind}</option>
					{/each}
				</select>
			</label>
			<label class="feed-filter">
				<span class="feed-filter-label">Agent</span>
				<select bind:value={agentFilter}>
					<option value="">All</option>
					{#each availableAgents as agent}
						<option value={agent}>{agent}</option>
					{/each}
				</select>
			</label>
			{#if availableThreads.length > 0}
				<label class="feed-filter">
					<span class="feed-filter-label">Thread</span>
					<select bind:value={threadFilter}>
						<option value="">All</option>
						{#each availableThreads as thread}
							<option value={thread}>{thread}</option>
						{/each}
					</select>
				</label>
			{/if}
			<label class="feed-filter feed-filter-toggle">
				<input type="checkbox" bind:checked={hideResolved} />
				<span>Hide resolved</span>
				{#if mutedCount > 0}
					<span class="feed-muted-count">({mutedCount})</span>
				{/if}
			</label>
			{#if kindFilter || agentFilter || threadFilter}
				<button type="button" class="feed-filter-clear" on:click={clearFilters}>
					Clear
				</button>
			{/if}
		</div>
	{/if}

	{#if loading}
		<div class="feed-state">Loading…</div>
	{:else if error}
		<div class="feed-state feed-error">Error: {error}</div>
	{:else if events.length === 0}
		<div class="feed-state">
			No events yet. Run a cycle to see live updates appear here.
		</div>
	{:else if filteredEvents.length === 0}
		<div class="feed-state">
			No events match the current filters.
		</div>
	{:else}
		<ol class="feed-list">
			{#each groupedEvents as group (group.events[0].id)}
				{@const latest = group.events[0]}
				{@const groupId = latest.id}
				{@const collapsed = group.events.length > 1 && !expandedGroups.has(groupId)}
				<li class="feed-item">
					<div class="feed-meta">
						<span class="feed-kind">{latest.kind}</span>
						<span class="feed-agent">{latest.agent_id}</span>
						{#if latest.cycle_id}
							<span class="feed-cycle">cycle {latest.cycle_id}</span>
						{/if}
						<span class="feed-time">{formatTs(latest.ts)}</span>
						{#if group.events.length > 1}
							<button
								type="button"
								class="feed-group-toggle"
								on:click={() => toggleGroup(groupId)}
							>
								{collapsed ? `+${group.events.length - 1} more` : 'Collapse'}
							</button>
						{/if}
					</div>
					{#if collapsed}
						<AgentUpdateCard event={latest} />
					{:else}
						{#each group.events as event (event.id)}
							<AgentUpdateCard {event} />
						{/each}
					{/if}
				</li>
			{/each}
		</ol>
	{/if}
	{/if}
</div>

<style>
	/* Width + centering come from the shared `.presto-gaui-page` class
	   (see `(app)/+layout.svelte` — capped at 1320px and centered with
	   `margin: 0 auto`), so /feed lives on the same column as /history,
	   /events, /tasks, and the rest of the (app) shell. We only declare
	   the flex/gap needed for /feed's internal layout here. */
	.feed-root {
		display: flex;
		flex-direction: column;
		gap: var(--space-lg, 24px);
	}

	.feed-header h1 {
		margin: 0 0 4px 0;
		font-family: var(--font-primary);
		font-size: 1.5rem;
	}

	.feed-subtitle {
		color: var(--text-secondary);
		font-size: 0.9rem;
		margin: 0;
	}

	.feed-subtitle code {
		background: var(--bg-soft);
		padding: 2px 6px;
		border-radius: 4px;
		font-family: var(--font-mono);
	}

	.feed-tabs {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
		padding: 4px;
		border: 1px solid color-mix(in srgb, var(--border-soft) 70%, transparent);
		border-radius: var(--radius-lg, 12px);
		background: color-mix(in srgb, var(--bg-soft) 64%, transparent);
		width: fit-content;
		max-width: 100%;
	}

	.feed-tabs button {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		border: 0;
		border-radius: var(--radius-md, 10px);
		background: transparent;
		color: var(--text-secondary);
		padding: 8px 12px;
		font: inherit;
		cursor: pointer;
	}

	.feed-tabs button.active {
		background: var(--bg-card);
		color: var(--text-primary);
		box-shadow: 0 8px 22px rgba(15, 23, 42, 0.08);
	}

	.feed-tabs span {
		min-width: 1.4rem;
		border-radius: var(--radius-full, 999px);
		background: color-mix(in srgb, var(--accent-primary) 14%, transparent);
		color: var(--text-primary);
		font-family: var(--font-mono);
		font-size: 0.72rem;
		padding: 2px 6px;
		text-align: center;
	}

	.learning-feed-grid {
		/* One card per row instead of an auto-fit grid. The grid layout
		   crammed multiple cards into narrow 280px columns, which made
		   long titles, agent IDs, and source paths overflow horizontally.
		   A single-column flow gives each item the full pane width so
		   the card's own text wrapping handles overflow naturally. */
		display: flex;
		flex-direction: column;
		gap: var(--space-sm, 10px);
	}
	.learning-feed-grid > * {
		min-width: 0;
	}

	.learning-insight-overview {
		display: grid;
		gap: 0.75rem;
		margin-bottom: var(--space-md, 16px);
	}

	.learning-insight-metrics {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
		gap: 0.75rem;
	}

	.learning-insight-metric,
	.learning-digest {
		border: 1px solid color-mix(in srgb, var(--border-soft) 70%, transparent);
		border-radius: var(--radius-lg, 12px);
		background: color-mix(in srgb, var(--bg-card) 78%, transparent);
		padding: 0.85rem 0.95rem;
		min-width: 0;
	}

	.learning-insight-metric {
		display: grid;
		gap: 0.2rem;
	}

	.learning-insight-metric strong {
		color: var(--text-primary);
		font-size: 1.35rem;
		line-height: 1;
	}

	.learning-insight-metric span,
	.learning-digest {
		color: var(--text-secondary);
		font-size: 0.84rem;
	}

	.learning-digest {
		display: flex;
		flex-wrap: wrap;
		gap: 0.7rem 1rem;
		align-items: center;
	}

	.learning-digest strong {
		color: var(--text-primary);
	}

	.learning-digest div {
		display: flex;
		flex-wrap: wrap;
		gap: 0.45rem;
	}

	.learning-digest span {
		border: 1px solid color-mix(in srgb, var(--border-soft) 70%, transparent);
		border-radius: 999px;
		padding: 0.22rem 0.55rem;
		background: color-mix(in srgb, var(--bg-soft) 65%, transparent);
		color: var(--text-secondary);
		overflow-wrap: anywhere;
	}

	.feed-state {
		color: var(--text-secondary);
		padding: var(--space-md, 16px);
		border: 1px dashed var(--border-soft);
		border-radius: var(--radius-lg, 8px);
		text-align: center;
	}

	.feed-filters {
		display: flex;
		gap: var(--space-md, 16px);
		align-items: flex-end;
		flex-wrap: wrap;
	}

	.feed-filter {
		display: flex;
		flex-direction: column;
		gap: 4px;
		font-size: 0.8rem;
	}

	.feed-filter-label {
		color: var(--text-secondary);
		font-family: var(--font-mono);
		font-size: 0.7rem;
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.feed-filter select {
		padding: 6px 8px;
		background: var(--bg-card);
		color: var(--text-primary);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 4px);
		font-family: var(--font-primary);
		font-size: 0.85rem;
		min-width: 140px;
	}

	.feed-filter-clear {
		padding: 6px 12px;
		background: transparent;
		color: var(--text-secondary);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 4px);
		cursor: pointer;
		font-size: 0.8rem;
		align-self: flex-end;
	}

	.feed-filter-clear:hover {
		color: var(--text-primary);
		border-color: var(--text-primary);
	}

	.feed-filter-toggle {
		flex-direction: row;
		align-items: center;
		gap: 6px;
		padding: 6px 0;
		cursor: pointer;
	}

	.feed-filter-toggle input[type='checkbox'] {
		cursor: pointer;
	}

	.feed-muted-count {
		color: var(--text-muted, #888);
		font-family: var(--font-mono);
		font-size: 0.7rem;
	}

	.feed-group-toggle {
		background: transparent;
		border: 1px solid var(--border-soft);
		color: var(--text-secondary);
		padding: 1px 8px;
		border-radius: var(--radius-full, 9999px);
		cursor: pointer;
		font-family: var(--font-mono);
		font-size: 0.7rem;
	}

	.feed-group-toggle:hover {
		color: var(--text-primary);
		border-color: var(--text-primary);
	}

	.feed-item > :global(.muij-card + .muij-card) {
		margin-top: var(--space-xs, 4px);
	}

	.feed-error {
		color: var(--color-error, #ef4444);
		border-color: var(--color-error, #ef4444);
	}

	.feed-list {
		list-style: none;
		padding: 0;
		margin: 0;
		display: flex;
		flex-direction: column;
		gap: var(--space-md, 16px);
	}

	.feed-item {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}

	.feed-meta {
		display: flex;
		gap: 8px;
		align-items: center;
		flex-wrap: wrap;
		font-size: 0.75rem;
		font-family: var(--font-mono);
		color: var(--text-muted, #888);
	}

	.feed-kind {
		color: var(--color-info, #3b82f6);
	}

	.feed-cycle,
	.feed-agent,
	.feed-time {
		color: var(--text-muted, #888);
	}
</style>
