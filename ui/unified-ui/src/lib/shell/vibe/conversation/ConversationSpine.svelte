<script lang="ts">
	/**
	 * The conversation spine — the cockpit's centre column. Renders the live
	 * `coding.*` event stream (from `codingSpineStore` → `spineModel`) as a
	 * threaded timeline of typed cards: run header, reasoning foldout, assistant
	 * message, tool action cards, compact diff summary cards (file count + ±
	 * stats + status — the ONE diff review home is the stage Diff tab, which the
	 * card links to), red→green test cards, errors. Replaces the
	 * monolith's 12-row / 180-char scrape. Windowed (paged "Load earlier") +
	 * stick-to-bottom. Cards render grouped into turn sections (spineModel
	 * `groupCards`): runs of ≥3 consecutive tool actions fold into a compact
	 * expandable action strip; while live, the latest action never folds.
	 */
	import { afterUpdate, onMount, onDestroy } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import ChatMarkdown from '$lib/magician/components/chat/ChatMarkdown.svelte';
	import SpineActionCard, {
		cardDurationMs,
		fmtDuration,
		fmtTime
	} from '$lib/shell/vibe/conversation/SpineActionCard.svelte';
	import {
		cardPreview,
		groupCards,
		snapWindowStart,
		type SpineCard
	} from '$lib/shell/vibe/conversation/spineModel';
	import type { VibeRow } from '$lib/stores/vibeHitlStore';
	import type { HitlRequest } from '$lib/hitl/types';
	import { createEventDispatcher } from 'svelte';

	export let cards: SpineCard[] = [];
	export let live = false;
	export let streamState: string = 'idle';
	/** The live run's event tail dropped and the store is reconnecting (parent
	 *  derives it: non-terminal recent run + a non-live stream state). Shows a
	 *  slim amber banner pinned over the spine; clears on resume. Previously the
	 *  only stream-health surfacing was the zero-card empty-state copy, so a
	 *  MID-run drop silently froze the timeline. */
	export let reconnecting = false;
	/** proposal_id → HITL row (stats + status source for the compact diff cards). */
	export let diffRowsByProposal: Map<string, VibeRow> = new Map();
	export let sessionId: string | null = null;
	/** True when the viewed task is terminal (completed/failed/cancelled). A
	 * finished run has no live build stream to connect to, so the empty state
	 * must NOT show the live "connecting…"/"error — retrying" copy (that copy is
	 * meaningless + alarming for historical runs and was the stuck-banner bug). */
	export let historical = false;
	/** Identity of the run being shown — when it changes, reset to a fresh
	 *  bottom-anchored view so a newly opened run loads straight to the latest event. */
	export let runKey: string = '';
	/** True while the build/plan stream has closed but the agent is still
	 *  synthesizing the final result — without a cue the spine "looks done". */
	export let synthesizing = false;
	/** FIX #6b: true when this is a TERMINAL coordinator run that engaged no coding
	 * pipeline (0 coding events) but is NOT aged out of the history window — i.e. a
	 * fresh delegate-only run with nothing to show, distinct from an old run whose
	 * stream was compacted away. The parent derives it (run terminal + 0 coding
	 * events + recent) so the empty state can give an accurate explanation instead
	 * of the misleading "may predate the build-history window" copy. */
	export let coordinatorTerminal = false;

	/** A follow-up turn was just dispatched onto THIS (threaded) run but its first coding
	 *  event hasn't landed yet. Shows a footer "starting" affordance below the existing turns,
	 *  independent of `historical`, so a composer follow-up reads as continuing rather than
	 *  silently doing nothing during the dispatch→first-event gap. */
	export let loadingTurn = false;

	// Self-advancing "starting" stepper. The pre-coding window (workspace/context
	// setup → routing to an engineer → engineer warmup) emits no `coding.*` events,
	// so the spine would otherwise sit on a frozen "Starting shortly…" spinner for
	// up to a minute and read as stuck. Advance through the REAL phases on an
	// elapsed timer (directional, not fabricated specifics) so the gap shows honest
	// motion. Resets per run; hidden the instant the first coding card lands.
	const STARTING_PHASES = [
		{ atMs: 0, title: 'Setting up your build…', sub: 'Preparing the workspace and gathering context.' },
		{ atMs: 18000, title: 'Routing to an engineer…', sub: 'Picking the right specialist for this change.' },
		{ atMs: 36000, title: 'Engineer starting up…', sub: 'Warming the coding session — the first step appears here shortly.' },
		{ atMs: 70000, title: 'Still working…', sub: 'Larger setups can take a little longer before the first step streams in.' }
	];
	let startingElapsedMs = 0;
	let startingAnchorKey = '';
	let startingTimer: ReturnType<typeof setInterval> | null = null;
	$: showStarting =
		cards.length === 0 && !historical && !!runKey && streamState !== 'error' && !synthesizing;
	// Reset the elapsed clock when a fresh starting-run begins.
	$: if (showStarting && runKey !== startingAnchorKey) {
		startingAnchorKey = runKey;
		startingElapsedMs = 0;
	}
	$: startingPhase = STARTING_PHASES.reduce(
		(acc, phase) => (startingElapsedMs >= phase.atMs ? phase : acc),
		STARTING_PHASES[0]
	);
	onMount(() => {
		startingTimer = setInterval(() => {
			if (showStarting) startingElapsedMs += 1000;
		}, 1000);
	});
	onDestroy(() => {
		if (startingTimer) clearInterval(startingTimer);
	});

	const dispatch = createEventDispatcher<{
		/** "Review in Diff tab →" on a diff card — the parent switches the stage
		 *  tab to Diff (the one diff review home). Apply/Reject live there, not here. */
		reviewDiff: { proposalId: string | null };
	}>();

	const WINDOW = 120;
	/** "Load earlier" page size — the window grows by this per click instead of
	 *  unwindowing ALL history at once (a 4000-card run used to land in the DOM
	 *  in one shot). */
	const PAGE = 150;
	let scroller: HTMLElement | null = null;
	let stick = true;
	let expanded = new Set<string>();
	let windowSize = WINDOW;
	/** Expanded action strips, keyed by the strip's DETERMINISTIC id (derived
	 *  from its first member card), so expansion survives re-derivation as new
	 *  cards stream in. */
	let expandedStrips = new Set<string>();

	// Window start snaps FORWARD past a bisected action run (`snapWindowStart`)
	// so the first visible strip always begins at a real run boundary — its
	// first-member-derived id can't churn as the window slides. hiddenCount /
	// "N remaining" derive from the SNAPPED start so the load-earlier counts
	// stay truthful. (Sole accepted churn edge: a run longer than the snap cap;
	// see `snapWindowStart`'s mega-run guard.)
	$: windowStart = snapWindowStart(cards, Math.max(0, cards.length - windowSize));
	$: visibleCards = cards.slice(windowStart);
	$: hiddenCount = windowStart;
	$: loadChunk = Math.min(PAGE, hiddenCount);

	// Turn grouping + action strips, derived over the WINDOWED slice only —
	// grouping the full history on every store publish would be wasted work;
	// we group exactly what's rendered. `live` gates the never-fold-the-last-
	// card rule so the currently-streaming action stays visible.
	$: groups = groupCards(visibleCards, { live });

	// A fresh run opens collapsed + stuck so it loads straight to the latest event.
	let lastRunKey = '';
	$: if (runKey !== lastRunKey) {
		lastRunKey = runKey;
		stick = true;
		windowSize = WINDOW;
		expandedStrips = new Set();
	}

	function loadEarlier(): void {
		windowSize += PAGE;
	}

	function toggleStrip(id: string): void {
		const next = new Set(expandedStrips);
		if (next.has(id)) {
			// Collapsing never touches stick.
			next.delete(id);
		} else {
			next.add(id);
			// EXPANDING a strip that isn't the last visible item means the user is
			// inspecting history — release stick-to-bottom so the newly inserted
			// cards don't yank the scroll to the bottom. The "Jump to latest" pill
			// re-engages it.
			const lastGroup = groups[groups.length - 1];
			const lastItem = lastGroup?.items[lastGroup.items.length - 1];
			const isLastVisibleItem = lastItem?.kind === 'action-strip' && lastItem.id === id;
			if (stick && !isLastVisibleItem) stick = false;
		}
		expandedStrips = next;
	}

	// Stick-to-bottom: pin to the bottom synchronously AFTER each DOM patch (before
	// the browser paints) while the user is at/near the bottom. Doing it in
	// `afterUpdate` (vs the old `await tick()` then scroll) means a loaded run appears
	// already at the latest event — no visible load-at-top-then-scroll jump — and live
	// updates stay pinned. `onScroll` flips `stick` off the instant the user scrolls up.
	afterUpdate(() => {
		if (stick && scroller) {
			scroller.scrollTop = scroller.scrollHeight;
		}
	});

	function onScroll(): void {
		if (!scroller) return;
		const distance = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;
		stick = distance < 120;
	}

	function jumpToLatest(): void {
		stick = true;
		scroller?.scrollTo({ top: scroller.scrollHeight, behavior: 'smooth' });
	}

	function toggle(id: string): void {
		const next = new Set(expanded);
		if (next.has(id)) next.delete(id);
		else next.add(id);
		expanded = next;
	}

	// Run-header prompts whose 2-line clamp actually hides text — only those get a
	// "Show more" toggle. Membership is sticky (add-only): once a clamped element is
	// measured as overflowing it stays toggleable even after expanding (where it's no
	// longer clamped, so a re-measure would read as not-overflowing).
	let promptOverflow = new Set<string>();
	function probeClamp(node: HTMLElement, id: string) {
		const measure = (key: string) => {
			if (node.scrollHeight > node.clientHeight + 1 && !promptOverflow.has(key)) {
				const next = new Set(promptOverflow);
				next.add(key);
				promptOverflow = next;
			}
		};
		measure(id);
		return {
			update(nextId: string) {
				measure(nextId);
			}
		};
	}

	function diffStat(request: HitlRequest, kind: 'additions' | 'deletions'): number {
		return (request.schema.files ?? []).reduce((sum, file) => sum + file[kind], 0);
	}

	/** Proposal ids with a LIVE pending row (not historical hydration) — those
	 *  compact cards get the attention treatment so they're findable, and the
	 *  "Review in Diff tab →" affordance is their primary action. Reactive on
	 *  the map prop so cards update the moment a proposal resolves. */
	$: livePendingProposals = new Set(
		Array.from(diffRowsByProposal.entries())
			.filter(([, row]) => row.request.raw?.historical !== true)
			.map(([id]) => id)
	);

	/** Label for a HISTORICAL (already-resolved) proposal diff — shown instead of
	 *  Apply/Reject when the run was hydrated from the durable proposal store. */
	function historicalDiffStatus(status: unknown): string {
		const s = String(status ?? '').toLowerCase();
		if (s.includes('reject')) return '✕ Rejected';
		if (s.includes('partial')) return '◐ Partially applied';
		if (s.includes('appl')) return '✓ Applied';
		return '• Resolved';
	}

	// fmtTime / cardDurationMs / fmtDuration are imported from SpineActionCard's
	// module context (single source for the meta-footer formatting); tool-action
	// icon + status-glyph rendering lives entirely inside SpineActionCard.
	// Result glyphs (✕/◐/✓/• in historicalDiffStatus) render as TEXT deliberately —
	// typographic, inherit font/color; interactive/semantic icons use <Icon>.
</script>

<div
	class="spine"
	role="log"
	aria-live="polite"
	aria-relevant="additions"
	aria-label="Coding conversation"
	bind:this={scroller}
	on:scroll={onScroll}
>
	{#if reconnecting}
		<div class="spine__streamhealth" role="status" aria-live="polite">
			<span class="spine__streamhealth-dot" aria-hidden="true"></span>
			Stream reconnecting… the build keeps running; live updates resume automatically.
		</div>
	{/if}

	{#if hiddenCount > 0}
		<button type="button" class="spine__earlier" on:click={loadEarlier}>
			Load {loadChunk} earlier event{loadChunk === 1 ? '' : 's'}{hiddenCount - loadChunk > 0
				? ` · ${hiddenCount - loadChunk} remaining`
				: ''}
		</button>
	{/if}

	{#each groups as group (group.id)}
		{#if group.label}
			<div class="spine__turn"><span class="spine__turn-label">{group.label}</span></div>
		{/if}
		{#each group.items as item (item.kind === 'card' ? item.card.id : item.id)}
		{#if item.kind === 'action-strip'}
			<div class="strip">
				<button
					type="button"
					class="strip__head"
					aria-expanded={expandedStrips.has(item.id)}
					on:click={() => toggleStrip(item.id)}
				>
					<span class="strip__chev" class:open={expandedStrips.has(item.id)} aria-hidden="true">▸</span>
					<span class="strip__count">{item.count} actions</span>
					{#if item.summary}
						<span class="strip__sep" aria-hidden="true">·</span>
						<span class="strip__summary">{item.summary}</span>
					{/if}
				</button>
				{#if expandedStrips.has(item.id)}
					<div class="strip__cards">
						{#each item.cards as card (card.id)}
							<SpineActionCard
								{card}
								expanded={expanded.has(card.id)}
								on:toggle={(e) => toggle(e.detail.id)}
							/>
						{/each}
					</div>
				{/if}
			</div>
		{:else if item.card.kind === 'action' || item.card.kind === 'test'}
			<SpineActionCard
				card={item.card}
				expanded={expanded.has(item.card.id)}
				on:toggle={(e) => toggle(e.detail.id)}
			/>
		{:else}
		{@const card = item.card}
		{@const preview = cardPreview(card)}
		<article
			class="card card--{card.kind} is-{card.status}"
			class:card--attention={card.kind === 'diff' &&
				!!card.proposalId &&
				livePendingProposals.has(card.proposalId)}
			aria-label={`${card.kind}: ${card.title}`}
		>
			{#if card.kind === 'run_header'}
				<header class="card__head">
					<span class="card__dot" aria-hidden="true"></span>
					<span class="card__title">{card.title}</span>
					{#if card.profileLabel}<span class="card__tag">{card.profileLabel}</span>{/if}
				</header>
				{#if card.promptFull || card.promptPreview}
					{@const full = card.promptFull ?? card.promptPreview ?? ''}
					{@const isOpen = expanded.has(card.id)}
					<div
						class="card__prompt"
						class:card__prompt--clamped={!isOpen}
						class:card__prompt--full={isOpen}
						use:probeClamp={card.id}
					>{full}</div>
					{#if isOpen || promptOverflow.has(card.id)}
						<button
							type="button"
							class="card__more"
							aria-expanded={isOpen}
							on:click={() => toggle(card.id)}
						>{isOpen ? 'Show less' : 'Show more'}</button>
					{/if}
				{/if}
				{#if card.repoPath}<p class="card__sub">{card.repoPath}</p>{/if}

			{:else if card.kind === 'reasoning'}
				<button type="button" class="card__foldhead" aria-expanded={expanded.has(card.id)} on:click={() => toggle(card.id)}>
					<span class="card__chev" class:open={expanded.has(card.id)} aria-hidden="true">▸</span>
					<span class="card__title card__title--muted">Thinking</span>
					{#if !expanded.has(card.id) && preview}
						<span class="card__preview">{preview}</span>
					{/if}
				</button>
				{#if expanded.has(card.id) && card.detail}
					<pre class="card__reasoning">{card.detail}</pre>
				{/if}

			{:else if card.kind === 'message'}
				<div class="card__message">
					<ChatMarkdown content={card.detail ?? ''} {sessionId} streaming={card.status === 'running'} />
				</div>

			{:else if card.kind === 'diff'}
				<!-- Compact summary ONLY — the full diff + Apply/Reject live in the
				     stage Diff tab (one diff home). This card states the facts and
				     routes there in one click. -->
				{@const row = card.proposalId ? diffRowsByProposal.get(card.proposalId) : undefined}
				{@const livePending = !!card.proposalId && livePendingProposals.has(card.proposalId)}
				<div class="card__diff">
					<header class="card__head">
						<span class="card__title">{card.title}</span>
						<span class="card__sandbox" title="Changes are staged in a sandbox until you apply them">Sandboxed · 0 to repo</span>
					</header>
					<div class="card__diff-meta">
						{#if row}
							{@const files = row.request.schema.files ?? []}
							<span>{files.length} file{files.length === 1 ? '' : 's'}</span>
							<span>+{diffStat(row.request, 'additions')} / -{diffStat(row.request, 'deletions')}</span>
						{:else}
							<span>{card.fileCount ?? '?'} file{card.fileCount === 1 ? '' : 's'}</span>
						{/if}
						{#if livePending}
							<span class="card__diff-pending">Pending review</span>
						{/if}
					</div>
					{#if row && !livePending}
						<div class="card__diff-actions">
							<span class="card__diff-status">{historicalDiffStatus(row.request.raw?.status)}</span>
						</div>
					{:else}
						<div class="card__diff-actions">
							<button
								type="button"
								class="vbtn vbtn--primary"
								aria-label="Review these changes in the Diff tab"
								on:click={() => dispatch('reviewDiff', { proposalId: card.proposalId ?? null })}
							>Review in Diff tab →</button>
						</div>
					{/if}
				</div>

			{:else if card.kind === 'error'}
				<header class="card__head">
					<span class="card__icon" aria-hidden="true"><Icon name="x" size={14} /></span>
					<span class="card__title">{card.title}</span>
				</header>
				{#if card.detail}<pre class="card__error-detail">{card.detail}</pre>{/if}

			{:else if card.kind === 'plan'}
				<header class="card__head">
					<span class="card__icon" aria-hidden="true"><Icon name="file-text" size={14} /></span>
					<span class="card__title">{card.title}</span>
				</header>
				{#if card.detail}
					<div class="card__message"><ChatMarkdown content={card.detail} {sessionId} /></div>
				{/if}

			{:else if card.kind === 'completed'}
				<header class="card__head">
					<span class="card__icon" aria-hidden="true">{card.status === 'waiting' ? '⏸' : '✓'}</span>
					<span class="card__title">{card.title}</span>
				</header>
				{#if card.detail}
					<div class="card__message"><ChatMarkdown content={card.detail} {sessionId} /></div>
				{/if}
			{/if}

			{#if expanded.has(card.id) || card.kind !== 'reasoning'}
				<div class="card__meta">
					<span class="card__meta-time">{fmtTime(card.startTs ?? card.ts)}</span>
					{#if cardDurationMs(card) > 0}
						<span class="card__meta-sep" aria-hidden="true">·</span>
						<span class="card__meta-dur">took {fmtDuration(cardDurationMs(card))}</span>
					{/if}
				</div>
			{/if}
		</article>
		{/if}
		{/each}
	{/each}

	{#if live && cards.length > 0}
		<div class="spine__live" aria-hidden="true"><span class="spine__pulse"></span> live</div>
	{/if}

	{#if synthesizing}
		<div class="spine__synthesizing" aria-live="polite">
			<span class="spine__synthesizing-spinner" aria-hidden="true"></span>
			<span>Synthesizing the result — finishing up, not done yet…</span>
		</div>
	{/if}

	{#if !stick && cards.length > 0}
		<button type="button" class="spine__jump" on:click={jumpToLatest} aria-label="Jump to latest activity">
			↓ Jump to latest
		</button>
	{/if}

	{#if cards.length === 0}
		{#if !historical && runKey && streamState !== 'error' && !synthesizing}
			<!-- Active run, first event not in yet: ease the dispatch→first-event gap
			     with a friendly "starting" affordance instead of the misleading
			     "send a request" empty copy (the request was already sent). -->
			<div class="spine__starting" aria-live="polite">
				<span class="spine__starting-spinner" aria-hidden="true"></span>
				<span class="spine__starting-title">{startingPhase.title}</span>
				<span class="spine__starting-sub">{startingPhase.sub}</span>
			</div>
		{:else}
			<div class="spine__empty">
				{#if coordinatorTerminal}
					This run finished without engaging the build pipeline — the coordinator
					didn't hand the work to an engineer, so there are no coding steps to
					show. See the coordinator activity above, or re-run it as a Build.
				{:else if historical}
					This run has finished — there's no live build stream. No activity was
					recorded for it here (it may predate the build-history window).
				{:else if streamState === 'error'}
					The build stream hit an error — retrying.
				{:else}
					No build activity yet. Send a request below and watch it build here.
				{/if}
			</div>
		{/if}
	{/if}

	{#if cards.length > 0 && loadingTurn && streamState !== 'error'}
		<!-- Threaded follow-up: a new turn was dispatched but its first coding event hasn't
		     landed yet — a footer "starting" affordance (independent of `historical`) so a
		     composer follow-up reads as continuing rather than doing nothing during the gap. -->
		<div class="spine__starting spine__starting--footer" aria-live="polite">
			<span class="spine__starting-spinner" aria-hidden="true"></span>
			<span class="spine__starting-title">Starting the next step…</span>
			<span class="spine__starting-sub">Preparing the workspace — the next step appears here shortly.</span>
		</div>
	{/if}
</div>

<style>
	.spine {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		overflow-x: hidden;
		display: flex;
		flex-direction: column;
		gap: var(--space-sm, 0.6rem);
		padding: var(--space-md, 1rem) var(--space-md, 1rem) var(--space-lg, 1.5rem);
		scroll-behavior: smooth;
	}

	/* Slim amber stream-health banner — pinned over the spine while the live
	   tail reconnects; disappears the moment the stream resumes. */
	.spine__streamhealth {
		position: sticky;
		top: 0;
		z-index: 3;
		align-self: stretch;
		display: flex;
		align-items: center;
		gap: 0.45rem;
		border: 1px solid color-mix(in srgb, var(--vibe-warning) 45%, var(--vibe-border));
		border-radius: var(--radius-sm, 10px);
		background: color-mix(in srgb, var(--vibe-warning) 12%, var(--vibe-surface));
		color: var(--vibe-text);
		font-size: 0.76rem;
		font-weight: 600;
		padding: 0.3rem 0.65rem;
	}
	.spine__streamhealth-dot {
		flex-shrink: 0;
		width: 0.5rem;
		height: 0.5rem;
		border-radius: 999px;
		background: var(--vibe-warning);
		animation: spine-pulse 1.4s ease-in-out infinite;
	}

	.spine__earlier {
		align-self: center;
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-full, 999px);
		background: var(--vibe-surface);
		color: var(--vibe-text-muted);
		font: inherit;
		font-size: 0.76rem;
		font-weight: 600;
		padding: 0.3rem 0.85rem;
		cursor: pointer;
	}
	.spine__earlier:hover {
		color: var(--vibe-accent);
		border-color: var(--vibe-accent);
	}

	/* Subtle turn-section divider: hairline + small-caps label. */
	.spine__turn {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		margin: 0.35rem 0 -0.15rem;
		color: var(--vibe-text-muted);
	}
	.spine__turn::before,
	.spine__turn::after {
		content: '';
		flex: 1;
		height: 1px;
		background: var(--vibe-border);
		opacity: 0.7;
	}
	.spine__turn-label {
		font-size: var(--text-2xs);
		font-weight: 700;
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}

	/* Action strip: one compact row for a folded run of tool actions, expanding
	   in place to the full member cards. */
	.strip {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
	}
	.strip__head {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		width: 100%;
		border: 1px dashed var(--vibe-border);
		border-radius: var(--radius-md, 18px);
		background: color-mix(in srgb, var(--vibe-page-surface) 45%, var(--vibe-surface));
		color: inherit;
		font: inherit;
		text-align: left;
		cursor: pointer;
		padding: 0.4rem 0.7rem;
	}
	.strip__head:hover {
		border-color: var(--vibe-accent);
	}
	.strip__chev {
		color: var(--vibe-text-muted);
		transition: transform 0.15s var(--ease-settle, ease);
	}
	.strip__chev.open {
		transform: rotate(90deg);
	}
	.strip__count {
		font-size: 0.78rem;
		font-weight: 700;
		color: var(--vibe-text-muted);
		white-space: nowrap;
	}
	.strip__sep {
		color: var(--vibe-text-muted);
	}
	.strip__summary {
		flex: 1;
		min-width: 0;
		font-family: var(--font-mono, monospace);
		font-size: 0.74rem;
		color: var(--vibe-text-muted);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.strip__cards {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
		margin-left: 0.55rem;
		padding-left: 0.9rem;
		border-left: 2px solid color-mix(in srgb, var(--vibe-border) 70%, transparent);
	}

	.card {
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-md, 18px);
		background: var(--vibe-surface);
		padding: 0.8rem 0.9rem;
		animation: card-in 0.24s var(--ease-settle, cubic-bezier(0.16, 1, 0.3, 1));
	}
	/* (action/test card styling lives in SpineActionCard.) */
	.card--reasoning {
		padding: 0.5rem 0.7rem;
		background: color-mix(in srgb, var(--vibe-page-surface) 60%, var(--vibe-surface));
	}
	.card--message,
	.card--completed {
		border-color: color-mix(in srgb, var(--vibe-accent) 22%, var(--vibe-border));
	}
	.card--run_header {
		border-color: color-mix(in srgb, var(--vibe-accent) 34%, var(--vibe-border));
		background: color-mix(in srgb, var(--vibe-accent) 5%, var(--vibe-surface));
	}
	.card--error,
	.card.is-failed {
		border-color: color-mix(in srgb, var(--vibe-error) 45%, var(--vibe-border));
		background: color-mix(in srgb, var(--vibe-error) 6%, var(--vibe-surface));
	}
	/* LIVE pending proposal: subtle statusTone-attention treatment so the
	   compact card is findable without shouting (review happens in the Diff tab). */
	.card--attention {
		border-color: color-mix(in srgb, var(--status-attention) 45%, var(--vibe-border));
		background: color-mix(in srgb, var(--status-attention) 5%, var(--vibe-surface));
	}

	.card__head {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
	}
	.card__dot {
		width: 0.55rem;
		height: 0.55rem;
		border-radius: 999px;
		background: var(--vibe-accent);
	}
	.card__title {
		font-family: var(--font-display, inherit);
		font-weight: 600;
		font-size: 0.9rem;
		color: var(--vibe-text);
	}
	.card__title--muted {
		color: var(--vibe-text-muted);
		font-weight: 600;
	}
	.card__tag {
		font-size: var(--text-2xs);
		font-weight: 600;
		color: var(--vibe-text-muted);
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-full, 999px);
		padding: 0.05rem 0.5rem;
	}
	.card__prompt {
		margin: 0.4rem 0 0;
		font-family: inherit;
		font-size: 0.88rem;
		line-height: 1.45;
		color: var(--vibe-text);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}
	/* Collapsed: clamp the prompt to two lines. */
	.card__prompt--clamped {
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		overflow: hidden;
	}
	/* Expanded: full prompt in a scrollable, lightly inset box. */
	.card__prompt--full {
		max-height: 18rem;
		overflow: auto;
		margin-top: 0.45rem;
		padding: 0.5rem 0.65rem;
		background: color-mix(in srgb, var(--vibe-page-surface) 55%, var(--vibe-surface));
		border-radius: var(--radius-sm, 10px);
	}
	.card__more {
		margin: 0.3rem 0 0;
		border: 0;
		background: transparent;
		padding: 0;
		font-size: 0.76rem;
		font-weight: 600;
		color: var(--vibe-accent, #c2502a);
		cursor: pointer;
	}
	.card__more:hover {
		text-decoration: underline;
	}
	.card__sub {
		margin: 0.3rem 0 0;
		font-family: var(--font-mono, monospace);
		font-size: 0.74rem;
		color: var(--vibe-text-muted);
		overflow-wrap: anywhere;
	}

	.card__foldhead {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		width: 100%;
		border: 0;
		background: transparent;
		color: inherit;
		font: inherit;
		text-align: left;
		cursor: pointer;
		padding: 0;
	}
	.card__icon {
		display: inline-grid;
		place-items: center;
		width: 1.25rem;
		font-size: 0.85rem;
		color: var(--vibe-text-muted);
	}
	.card__chev {
		margin-left: auto;
		color: var(--vibe-text-muted);
		transition: transform 0.15s var(--ease-settle, ease);
	}
	.card__chev.open {
		transform: rotate(90deg);
	}
	/* Collapsed one-line content preview (command / path / first line of thinking).
	   `flex:1; min-width:0` lets it absorb free space + ellipsize, so the chevron's
	   `margin-left:auto` stays pinned right. */
	.card__preview {
		flex: 1;
		min-width: 0;
		font-family: var(--font-mono, monospace);
		font-size: 0.74rem;
		line-height: 1.3;
		color: var(--vibe-text-muted);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		text-align: left;
	}

	/* Expanded-card footer: when it happened + how long it took. */
	.card__meta {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		margin-top: 0.4rem;
		font-size: var(--text-2xs);
		color: var(--vibe-text-muted);
		opacity: 0.85;
	}
	.card__meta-time {
		font-variant-numeric: tabular-nums;
	}
	.card__meta-dur {
		font-variant-numeric: tabular-nums;
	}

	.card__reasoning,
	.card__error-detail {
		margin: 0.5rem 0 0;
		padding: 0.5rem 0.65rem;
		background: color-mix(in srgb, var(--vibe-page-surface) 55%, var(--vibe-surface));
		border-radius: var(--radius-sm, 10px);
		font-family: var(--font-mono, monospace);
		font-size: 0.76rem;
		line-height: 1.5;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		color: var(--vibe-text-muted);
		max-height: 22rem;
		overflow: auto;
	}
	.card__error-detail {
		color: var(--vibe-error);
	}

	.card__message :global(p:first-child) {
		margin-top: 0;
	}
	.card__message :global(p:last-child) {
		margin-bottom: 0;
	}

	.card__diff {
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
	}
	.card__sandbox {
		margin-left: auto;
		font-size: var(--text-2xs);
		font-weight: 600;
		color: var(--vibe-success);
		border: 1px solid color-mix(in srgb, var(--vibe-success) 40%, transparent);
		border-radius: var(--radius-full, 999px);
		padding: 0.05rem 0.5rem;
	}
	.card__diff-meta {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		font-family: var(--font-mono, monospace);
		font-size: 0.74rem;
		color: var(--vibe-text-muted);
	}
	.card__diff-pending {
		font-weight: 700;
		color: var(--status-attention);
	}
	.card__diff-actions {
		display: flex;
		justify-content: flex-end;
		gap: 0.45rem;
	}

	.card__diff-status {
		font-size: var(--text-xs);
		font-weight: 600;
		color: var(--vibe-text-muted);
		padding: 0.2rem 0.5rem;
		border: 1px solid var(--vibe-border);
		border-radius: 0.4rem;
		white-space: nowrap;
	}

	.vbtn {
		min-height: 2rem;
		border: 1px solid var(--vibe-border-strong);
		border-radius: var(--radius-sm, 10px);
		background: var(--vibe-surface);
		color: var(--vibe-text);
		font: inherit;
		font-size: 0.78rem;
		font-weight: 600;
		padding: 0.35rem 0.7rem;
		cursor: pointer;
	}
	.vbtn--primary {
		border-color: color-mix(in srgb, var(--vibe-accent) 72%, transparent);
		background: var(--vibe-accent);
		color: var(--button-primary-color, #fff);
	}
	.vbtn:disabled {
		opacity: 0.55;
		cursor: not-allowed;
	}

	.spine__live {
		align-self: center;
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		font-size: var(--text-2xs);
		font-weight: 600;
		color: var(--vibe-text-muted);
	}
	.spine__pulse {
		width: 0.5rem;
		height: 0.5rem;
		border-radius: 999px;
		background: var(--vibe-success);
		animation: spine-pulse 1.4s ease-in-out infinite;
	}

	.spine__jump {
		position: sticky;
		bottom: 0.4rem;
		align-self: center;
		z-index: 2;
		border: 1px solid color-mix(in srgb, var(--vibe-accent) 50%, var(--vibe-border));
		border-radius: var(--radius-full, 999px);
		background: var(--vibe-accent);
		color: var(--button-primary-color, #fff);
		font: inherit;
		font-size: 0.74rem;
		font-weight: 600;
		padding: 0.3rem 0.85rem;
		box-shadow: var(--shadow-md, 0 6px 16px rgba(0, 0, 0, 0.18));
		cursor: pointer;
	}

	.spine__empty {
		margin: auto;
		max-width: 26rem;
		text-align: center;
		color: var(--vibe-text-muted);
		font-size: 0.9rem;
		line-height: 1.5;
		padding: 2rem 1rem;
	}

	.spine__starting {
		margin: auto;
		max-width: 26rem;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.6rem;
		text-align: center;
		padding: 2rem 1rem;
	}
	.spine__starting-spinner {
		width: 1.5rem;
		height: 1.5rem;
		border-radius: 999px;
		border: 2.5px solid color-mix(in srgb, var(--vibe-accent) 28%, transparent);
		border-top-color: var(--vibe-accent);
		animation: spine-spin 0.8s linear infinite;
	}
	.spine__starting-title {
		font-size: 0.92rem;
		font-weight: 600;
		color: var(--vibe-text, inherit);
	}
	.spine__starting-sub {
		font-size: 0.8rem;
		line-height: 1.45;
		color: var(--vibe-text-muted);
	}

	.spine__synthesizing {
		align-self: center;
		display: inline-flex;
		align-items: center;
		gap: 0.45rem;
		font-size: 0.78rem;
		font-weight: 600;
		color: var(--vibe-text-muted);
		padding: 0.3rem 0 0.1rem;
	}
	.spine__synthesizing-spinner {
		width: 0.8rem;
		height: 0.8rem;
		border-radius: 999px;
		border: 2px solid color-mix(in srgb, var(--vibe-accent) 30%, transparent);
		border-top-color: var(--vibe-accent);
		animation: spine-spin 0.8s linear infinite;
	}

	@keyframes card-in {
		from {
			opacity: 0;
			transform: translateY(6px);
		}
		to {
			opacity: 1;
			transform: translateY(0);
		}
	}
	@keyframes spine-spin {
		to {
			transform: rotate(360deg);
		}
	}
	@keyframes spine-pulse {
		0%,
		100% {
			opacity: 0.35;
			transform: scale(0.85);
		}
		50% {
			opacity: 1;
			transform: scale(1.1);
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.card,
		.spine__starting-spinner,
		.spine__synthesizing-spinner,
		.spine__streamhealth-dot,
		.spine__pulse {
			animation: none;
		}
		.spine {
			scroll-behavior: auto;
		}
	}
</style>
