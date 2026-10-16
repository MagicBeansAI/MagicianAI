<!--
  TimeLoom — the collapsible "⏪ Time Loom" replay panel for one Live Thinking
  Map (plan Phase 9: play/pause/speed, category jumps, then/now comparison,
  branch restore).

  Data flow (server-authoritative, mirrors the detail page's philosophy):
    · On first open it loads the FULL event log (`events(id, 0)`); while open
      it appends the tail (`events(id, lastSeq)`) whenever the live map's
      revision moves, so the timeline keeps up with the brainstorm.
    · The scrubber runs over EVENT INDEX (0‥n−1). The far-right position is
      "now" (the live map — replaying the last event reproduces it), modeled
      as `historyIndex = null` so the host page knows when to leave history
      mode. Every seek emits a `history` event (`HistoryView | null`).
    · Seeks are DEBOUNCED (~250 ms) and replays are cached per sequence — the
      event log is append-only, so a replay at a given sequence is immutable
      and the cache never invalidates for this map.
    · Playback advances one event per tick; the tick shortens with the chosen
      speed (0.5× / 1× / 2× / 4×). Reaching the end returns to "now".
    · "Restore as branch" (confirm-gated) calls `restore()` at the scrubbed
      sequence and emits `restored` with the new map id — the host page owns
      navigation.

  The panel renders NOTHING but a slim header row while closed (no layout
  jank); closing it also returns to "now" so a collapsed panel can never
  leave the page stuck read-only.
-->
<script lang="ts">
	import { createEventDispatcher, onDestroy } from 'svelte';
	import {
		events as fetchEvents,
		replay as fetchReplay,
		restore as restoreBranch
	} from '$lib/thinkingMaps/api';
	import {
		EVENT_CATEGORIES,
		categorizeEvent,
		diffMaps,
		jumpTargetIndex,
		type EventCategory,
		type HistoryView,
		type MapDiff
	} from '$lib/thinkingMaps/timeLoom';
	import type { MapEvent, ThinkingMap } from '$lib/types/thinkingMap';

	export let mapId: string;
	/** The LIVE map (the page's latest poll) — used for the diff + restore title. */
	export let map: ThinkingMap;

	const dispatch = createEventDispatcher<{
		history: HistoryView | null;
		restored: { mapId: string };
	}>();

	const CATEGORY_LABELS: Record<EventCategory, string> = {
		utterance: '🗣 utterance',
		correction: '✏️ correction',
		decision: '◆ decision',
		clarification: '❓ clarification',
		promotion: '↗ promotion'
	};

	/** Theme token per category (set as a CSS custom property inline — dynamic
	 *  class names would be pruned by Svelte's scoped-CSS analysis). */
	const CATEGORY_TOKENS: Record<EventCategory, string> = {
		utterance: 'var(--accent-primary)',
		correction: 'var(--color-warning, #b8860b)',
		decision: 'var(--color-success, #22a06b)',
		clarification: 'var(--color-info, #4d9de0)',
		promotion: 'var(--accent-secondary, var(--accent-primary))'
	};

	// ── Panel + event log ────────────────────────────────────────────────────────
	let open = false;
	let timeline: MapEvent[] = [];
	let eventsLoading = false;
	let eventsError: string | null = null;
	let loadedOnce = false;
	let lastTailRevision = -1;

	async function loadEvents(initial: boolean): Promise<void> {
		if (eventsLoading) return;
		eventsLoading = true;
		if (initial) eventsError = null;
		try {
			const after = timeline.length > 0 ? timeline[timeline.length - 1].sequence : 0;
			const tail = await fetchEvents(mapId, after);
			if (tail.length > 0) timeline = [...timeline, ...tail];
			loadedOnce = true;
			eventsError = null;
		} catch (err) {
			if (initial) eventsError = err instanceof Error ? err.message : String(err);
		} finally {
			eventsLoading = false;
		}
	}

	function toggleOpen(): void {
		open = !open;
		if (open) {
			lastTailRevision = map?.revision ?? -1;
			if (!loadedOnce) void loadEvents(true);
			else void loadEvents(false);
		} else {
			// Closing the loom always returns to the present — a collapsed panel
			// must never leave the page silently stuck in read-only history.
			backToNow();
		}
	}

	// While open, chase the live map: new envelopes append to the tail.
	$: if (open && loadedOnce && map && map.revision !== lastTailRevision) {
		lastTailRevision = map.revision;
		void loadEvents(false);
	}

	$: maxIndex = timeline.length - 1;

	// ── Scrub position ("now" = null) ────────────────────────────────────────────
	let historyIndex: number | null = null;
	let historyMap: ThinkingMap | null = null;
	let replayError: string | null = null;
	let desiredSeq: number | null = null;
	let debounceTimer: ReturnType<typeof setTimeout> | undefined;
	const replayCache = new Map<number, ThinkingMap>();

	function emitHistory(): void {
		if (historyIndex === null) {
			dispatch('history', null);
			return;
		}
		const ev = timeline[historyIndex];
		dispatch('history', {
			seq: ev.sequence,
			revision: ev.resulting_revision,
			map: historyMap
		});
	}

	function scrubTo(rawIndex: number | null): void {
		let index = rawIndex;
		if (index !== null && timeline.length === 0) index = null;
		if (index !== null) {
			index = Math.max(0, Math.min(maxIndex, Math.round(index)));
			// The far-right position IS the present: replaying the last event
			// reproduces the live map, so treat it as "now" and leave history.
			if (index === maxIndex) index = null;
		}
		historyIndex = index;
		replayError = null;
		clearTimeout(debounceTimer);
		if (index === null) {
			desiredSeq = null;
			historyMap = null;
			confirmRestoreOpen = false;
			emitHistory();
			return;
		}
		const ev = timeline[index];
		desiredSeq = ev.sequence;
		const cached = replayCache.get(ev.sequence);
		historyMap = cached ?? null;
		emitHistory();
		if (!cached) {
			debounceTimer = setTimeout(() => void loadReplay(ev.sequence), 250);
		}
	}

	async function loadReplay(seq: number): Promise<void> {
		try {
			const replayed = await fetchReplay(mapId, seq);
			replayCache.set(seq, replayed);
			if (desiredSeq === seq) {
				historyMap = replayed;
				emitHistory();
			}
		} catch (err) {
			if (desiredSeq === seq) {
				replayError = err instanceof Error ? err.message : String(err);
			}
		}
	}

	/** Host page hook (the banner's "Back to now" calls this via bind:this). */
	export function backToNow(): void {
		setPlaying(false);
		scrubTo(null);
	}

	$: positionLabel =
		historyIndex === null
			? `now · rev ${map?.revision ?? '—'}`
			: `seq ${timeline[historyIndex].sequence} · rev ${timeline[historyIndex].resulting_revision}`;

	// ── Playback ─────────────────────────────────────────────────────────────────
	let playing = false;
	let speed = 1;
	let playTimer: ReturnType<typeof setInterval> | undefined;
	const SPEEDS = [0.5, 1, 2, 4];
	const BASE_TICK_MS = 800;

	function restartPlayTimer(): void {
		clearInterval(playTimer);
		if (playing) playTimer = setInterval(tick, BASE_TICK_MS / speed);
	}

	function setPlaying(next: boolean): void {
		playing = next && timeline.length > 0;
		restartPlayTimer();
	}

	function setSpeed(next: number): void {
		speed = next;
		if (playing) restartPlayTimer();
	}

	function tick(): void {
		// One event per tick. From "now", play restarts from the beginning
		// (replay the whole map); reaching the end returns to the present.
		const next = historyIndex === null ? 0 : historyIndex + 1;
		if (next >= maxIndex) {
			setPlaying(false);
			scrubTo(null);
			return;
		}
		scrubTo(next);
	}

	// ── Category jumps + markers ─────────────────────────────────────────────────
	let selectedCategory: EventCategory = 'utterance';

	function jump(direction: 1 | -1): void {
		const target = jumpTargetIndex(timeline, historyIndex, selectedCategory, direction);
		if (target !== null) scrubTo(target);
	}

	/** Timeline ticks: every categorized event, colored by its FIRST category. */
	$: markers = timeline
		.map((ev, index) => ({ index, categories: categorizeEvent(ev), seq: ev.sequence }))
		.filter((m) => m.categories.length > 0);

	function markerLeft(index: number): string {
		return maxIndex <= 0 ? '0%' : `${(index / maxIndex) * 100}%`;
	}

	// ── Then/now comparison ──────────────────────────────────────────────────────
	let compareOn = false;
	let diff: MapDiff | null = null;
	$: diff = compareOn && historyMap && map ? diffMaps(historyMap, map) : null;

	// ── Branch restore ───────────────────────────────────────────────────────────
	let confirmRestoreOpen = false;
	let restoreBusy = false;
	let restoreError: string | null = null;

	function uuid(): string {
		if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
			return crypto.randomUUID();
		}
		return `k-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
	}

	async function doRestore(): Promise<void> {
		if (historyIndex === null || restoreBusy) return;
		const ev = timeline[historyIndex];
		restoreBusy = true;
		restoreError = null;
		try {
			const branch = await restoreBranch(mapId, {
				at_sequence: ev.sequence,
				new_map_id: uuid(),
				new_title: `${map?.title ?? 'Thinking map'} · branch @${ev.sequence}`
			});
			confirmRestoreOpen = false;
			dispatch('restored', { mapId: branch.map_id });
		} catch (err) {
			restoreError = err instanceof Error ? err.message : String(err);
		} finally {
			restoreBusy = false;
		}
	}

	onDestroy(() => {
		clearInterval(playTimer);
		clearTimeout(debounceTimer);
	});
</script>

<section class="tl" class:tl--open={open} aria-label="Time Loom replay">
	<button class="tl-head" aria-expanded={open} on:click={toggleOpen}>
		<span class="tl-head__title">⏪ Time Loom</span>
		{#if loadedOnce}
			<span class="tl-head__meta"
				>{timeline.length} event{timeline.length === 1 ? '' : 's'}</span
			>
		{/if}
		{#if historyIndex !== null}
			<span class="tl-head__viewing">viewing history</span>
		{/if}
		<span class="tl-head__chev" aria-hidden="true">{open ? '▾' : '▸'}</span>
	</button>

	{#if open}
		<div class="tl-body">
			{#if eventsLoading && !loadedOnce}
				<div class="tl-empty">Loading history…</div>
			{:else if eventsError}
				<div class="tl-error" role="alert">
					<span>Couldn't load the event log: {eventsError}</span>
					<button class="tl-mini" on:click={() => void loadEvents(true)}>Retry</button>
				</div>
			{:else if timeline.length === 0}
				<div class="tl-empty">No history yet — every applied change will appear here.</div>
			{:else}
				<!-- Transport -->
				<div class="tl-transport">
					<button
						class="tl-play"
						title={playing ? 'Pause' : 'Play (one event per tick)'}
						aria-label={playing ? 'Pause replay' : 'Play replay'}
						on:click={() => setPlaying(!playing)}>{playing ? '⏸' : '▶'}</button
					>
					<div class="tl-speeds" role="group" aria-label="Playback speed">
						{#each SPEEDS as s (s)}
							<button
								class="tl-speed"
								class:tl-speed--active={speed === s}
								aria-pressed={speed === s}
								on:click={() => setSpeed(s)}>{s}×</button
							>
						{/each}
					</div>
					<span class="tl-pos" aria-live="polite">{positionLabel}</span>
					{#if historyIndex !== null}
						<button class="tl-mini tl-mini--accent" on:click={backToNow}>Back to now</button>
					{/if}
				</div>

				<!-- Timeline: scrubber + category markers -->
				<div class="tl-track">
					<input
						class="tl-range"
						type="range"
						min="0"
						max={Math.max(maxIndex, 0)}
						step="1"
						value={historyIndex ?? maxIndex}
						aria-label="Scrub through the map's history"
						aria-valuetext={positionLabel}
						on:input={(e) => scrubTo(Number(e.currentTarget.value))}
					/>
					<div class="tl-markers" aria-hidden="true">
						{#each markers as m (m.index)}
							<button
								class="tl-marker"
								style={`left: ${markerLeft(m.index)}; --tl-cat-color: ${CATEGORY_TOKENS[m.categories[0]]}`}
								title={`seq ${m.seq} · ${m.categories.join(', ')}`}
								tabindex="-1"
								on:click={() => scrubTo(m.index)}
							></button>
						{/each}
					</div>
				</div>

				<!-- Category jumps -->
				<div class="tl-cats">
					<button
						class="tl-mini"
						title={`Jump back to the previous ${selectedCategory}`}
						on:click={() => jump(-1)}>◀</button
					>
					{#each EVENT_CATEGORIES as c (c)}
						<button
							class="tl-cat"
							class:tl-cat--active={selectedCategory === c}
							style={`--tl-cat-color: ${CATEGORY_TOKENS[c]}`}
							aria-pressed={selectedCategory === c}
							on:click={() => (selectedCategory = c)}>{CATEGORY_LABELS[c]}</button
						>
					{/each}
					<button
						class="tl-mini"
						title={`Jump forward to the next ${selectedCategory}`}
						on:click={() => jump(1)}>▶</button
					>
				</div>

				{#if replayError}
					<div class="tl-error" role="alert">
						<span>Couldn't replay this point: {replayError}</span>
					</div>
				{/if}

				<!-- History-mode actions: then/now comparison + branch restore -->
				{#if historyIndex !== null}
					<div class="tl-actions">
						<button
							class="tl-mini"
							class:tl-mini--accent={compareOn}
							aria-pressed={compareOn}
							on:click={() => (compareOn = !compareOn)}>⇄ Compare with now</button
						>
						{#if compareOn}
							{#if diff}
								<span class="tl-diff" role="status">
									{#if diff.added.length === 0 && diff.removed.length === 0 && diff.changed.length === 0}
										No changes since this point.
									{:else}
										<span class="tl-diff__added">+{diff.added.length} added since</span>
										<span class="tl-diff__removed">−{diff.removed.length} removed</span>
										<span class="tl-diff__changed">{diff.changed.length} changed</span>
									{/if}
								</span>
							{:else}
								<span class="tl-diff">Loading replay…</span>
							{/if}
						{/if}
						<span class="tl-actions__spacer"></span>
						{#if !confirmRestoreOpen}
							<button
								class="tl-mini"
								title="Fork the map at this point into a new branch"
								on:click={() => (confirmRestoreOpen = true)}>⎇ Restore as branch</button
							>
						{/if}
					</div>
					{#if confirmRestoreOpen}
						<div class="tl-confirm" role="alertdialog" aria-label="Confirm branch restore">
							<span
								>Fork this map at seq {timeline[historyIndex].sequence} into a new branch? The
								current map stays untouched.</span
							>
							<div class="tl-confirm__actions">
								<button
									class="tl-mini tl-mini--accent"
									disabled={restoreBusy}
									on:click={() => void doRestore()}
									>{restoreBusy ? 'Creating…' : 'Create branch'}</button
								>
								<button
									class="tl-mini"
									disabled={restoreBusy}
									on:click={() => (confirmRestoreOpen = false)}>Cancel</button
								>
							</div>
						</div>
					{/if}
					{#if restoreError}
						<div class="tl-error" role="alert">
							<span>Couldn't create the branch: {restoreError}</span>
						</div>
					{/if}
				{/if}
			{/if}
		</div>
	{/if}
</section>

<style>
	.tl {
		flex-shrink: 0;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-card);
		box-shadow: var(--shadow-sm);
		overflow: hidden;
	}

	.tl-head {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		width: 100%;
		padding: 0.5rem 0.75rem;
		border: none;
		background: transparent;
		color: var(--text-primary);
		font-size: 0.8rem;
		font-weight: 700;
		cursor: pointer;
		text-align: left;
	}

	.tl-head:hover {
		background: color-mix(in srgb, var(--accent-primary) 6%, transparent);
	}

	.tl-head__title {
		flex-shrink: 0;
	}

	.tl-head__meta {
		color: var(--text-muted);
		font-size: 0.72rem;
		font-weight: 500;
	}

	.tl-head__viewing {
		padding: 0.05rem 0.5rem;
		border-radius: var(--radius-full, 999px);
		background: color-mix(in srgb, var(--accent-primary) 14%, transparent);
		color: var(--accent-primary);
		font-size: 0.68rem;
		font-weight: 700;
	}

	.tl-head__chev {
		margin-left: auto;
		color: var(--text-muted);
		font-size: 0.75rem;
	}

	.tl-body {
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
		padding: 0.35rem 0.75rem 0.7rem;
		border-top: 1px solid var(--border-soft);
	}

	.tl-empty {
		padding: 0.4rem 0;
		color: var(--text-secondary);
		font-size: 0.78rem;
	}

	.tl-error {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.6rem;
		padding: 0.4rem 0.6rem;
		border-radius: var(--radius-sm, 6px);
		background: color-mix(in srgb, var(--color-error, #c0392b) 10%, transparent);
		color: var(--color-error, #c0392b);
		font-size: 0.76rem;
	}

	.tl-transport {
		display: flex;
		align-items: center;
		gap: 0.55rem;
		flex-wrap: wrap;
	}

	.tl-play {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 1.9rem;
		height: 1.9rem;
		border: 1px solid color-mix(in srgb, var(--accent-primary) 45%, transparent);
		border-radius: var(--radius-full, 999px);
		background: color-mix(in srgb, var(--accent-primary) 10%, transparent);
		color: var(--accent-primary);
		font-size: 0.8rem;
		cursor: pointer;
		transition: background var(--transition-fast, 0.15s ease);
	}

	.tl-play:hover {
		background: color-mix(in srgb, var(--accent-primary) 18%, transparent);
	}

	.tl-speeds {
		display: inline-flex;
		gap: 2px;
		padding: 2px;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-full, 999px);
	}

	.tl-speed {
		padding: 0.1rem 0.45rem;
		border: none;
		border-radius: var(--radius-full, 999px);
		background: transparent;
		color: var(--text-secondary);
		font-size: 0.68rem;
		font-weight: 600;
		cursor: pointer;
	}

	.tl-speed--active {
		background: color-mix(in srgb, var(--accent-primary) 16%, transparent);
		color: var(--accent-primary);
	}

	.tl-pos {
		color: var(--text-secondary);
		font-family: var(--font-mono, monospace);
		font-size: 0.72rem;
		white-space: nowrap;
	}

	.tl-mini {
		padding: 0.18rem 0.6rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-full, 999px);
		background: transparent;
		color: var(--text-secondary);
		font-size: 0.72rem;
		font-weight: 600;
		cursor: pointer;
		transition:
			color var(--transition-fast, 0.15s ease),
			background var(--transition-fast, 0.15s ease);
	}

	.tl-mini:hover:not(:disabled) {
		color: var(--text-primary);
		background: color-mix(in srgb, var(--text-primary) 6%, transparent);
	}

	.tl-mini:disabled {
		opacity: 0.55;
		cursor: default;
	}

	.tl-mini--accent {
		border-color: color-mix(in srgb, var(--accent-primary) 45%, transparent);
		background: color-mix(in srgb, var(--accent-primary) 10%, transparent);
		color: var(--accent-primary);
	}

	.tl-mini--accent:hover:not(:disabled) {
		color: var(--accent-primary);
		background: color-mix(in srgb, var(--accent-primary) 18%, transparent);
	}

	.tl-track {
		position: relative;
		padding-bottom: 0.55rem;
	}

	.tl-range {
		width: 100%;
		accent-color: var(--accent-primary);
	}

	.tl-markers {
		position: absolute;
		left: 0;
		right: 0;
		bottom: 0;
		height: 0.45rem;
	}

	.tl-marker {
		position: absolute;
		bottom: 0;
		width: 3px;
		height: 0.45rem;
		padding: 0;
		border: none;
		border-radius: 1px;
		background: var(--tl-cat-color, var(--text-muted));
		cursor: pointer;
		transform: translateX(-50%);
	}

	.tl-cats {
		display: flex;
		align-items: center;
		gap: 0.3rem;
		flex-wrap: wrap;
	}

	.tl-cat {
		padding: 0.14rem 0.5rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-full, 999px);
		background: transparent;
		color: var(--text-secondary);
		font-size: 0.68rem;
		font-weight: 600;
		cursor: pointer;
	}

	.tl-cat--active {
		border-color: color-mix(in srgb, var(--tl-cat-color, var(--accent-primary)) 55%, transparent);
		background: color-mix(in srgb, var(--tl-cat-color, var(--accent-primary)) 12%, transparent);
		color: var(--text-primary);
	}

	.tl-actions {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
	}

	.tl-actions__spacer {
		flex: 1;
	}

	.tl-diff {
		display: inline-flex;
		gap: 0.6rem;
		color: var(--text-secondary);
		font-size: 0.72rem;
	}

	.tl-diff__added {
		color: var(--color-success, #22a06b);
		font-weight: 600;
	}

	.tl-diff__removed {
		color: var(--color-error, #c0392b);
		font-weight: 600;
	}

	.tl-diff__changed {
		color: var(--color-warning, #b8860b);
		font-weight: 600;
	}

	.tl-confirm {
		display: flex;
		flex-direction: column;
		gap: 0.45rem;
		padding: 0.55rem 0.65rem;
		border: 1px solid color-mix(in srgb, var(--color-warning, #b8860b) 45%, transparent);
		border-radius: var(--radius-sm, 6px);
		background: color-mix(in srgb, var(--color-warning, #b8860b) 10%, transparent);
		color: var(--text-primary);
		font-size: 0.76rem;
	}

	.tl-confirm__actions {
		display: flex;
		gap: 0.4rem;
	}
</style>
