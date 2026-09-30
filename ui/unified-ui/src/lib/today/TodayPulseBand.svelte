<script lang="ts">
	/**
	 * Today's Pulse — the seven-chip analytics band that sits between the
	 * Today metrics strip and the What-Changed digest. Pure presentation:
	 * every number comes from `todayPulseStore`'s snapshot, and every chip is
	 * a real link to the surface that owns its metric.
	 *
	 * Lifecycle: the band owns the store's refcounted start/stop, scoped to
	 * its own mount — the page never has to know the pulse store exists.
	 * Ordering note for the task-count chip: child `onMount` runs before the
	 * parent page's, but the page's `onMount` (which starts taskStore) fires
	 * in the same mount flush, while the pulse's task counts are only
	 * computed when its first analytics POSTs settle — long after — so
	 * `computeTaskCounts` reads a live task list; and even if taskStore were
	 * somehow not started, the store's documented contract is zero counts,
	 * never a crash.
	 *
	 * States (the band is decorative — it must never add noise to Today):
	 * - unavailable, or a loaded snapshot that is all-zero → render NOTHING
	 *   (no empty shell, so hiding can't shift the page layout).
	 * - loading with no snapshot yet → the card shell with skeleton chips.
	 * - a chip whose backing slice is in `staleSlices` dims to 60% with a
	 *   "Refreshing…" title; its numbers are the last-good carry-forward.
	 */
	import { onMount, tick } from 'svelte';
	import { browser } from '$app/environment';

	import Icon from '$lib/shared/icons/Icon.svelte';
	import type { IconName } from '$lib/shared/icons/paths';
	import Skeleton from '$lib/shared/components/Skeleton.svelte';
	import Sparkline from '$lib/magician/components/generative/Sparkline.svelte';
	import { settleIn } from '$lib/shared/motion';
	import { todayPulseStore, type PulseSliceName } from '$lib/stores/todayPulseStore';
	import { formatDelta, pulseIsEmpty, type PulseSnapshot } from '$lib/today/pulseQueries';
	import {
		countTone,
		deltaAria,
		formatSpend,
		spendTone,
		withYday,
		type PulseTone
	} from '$lib/today/pulseFormat';

	interface PulseChip {
		id: string;
		label: string;
		value: string;
		/** Empty string → no delta line rendered. */
		delta: string;
		tone: PulseTone;
		href: string;
		icon: IconName;
		/** Latest fetch for this chip's slice failed — dim + "Refreshing…". */
		stale: boolean;
		/** Spend chip only: today's 24 hourly buckets. */
		sparkline?: number[];
		ariaLabel: string;
	}

	/** Skeleton placeholder count matches the full chip set. */
	const SKELETON_CHIP_COUNT = 7;

	onMount(() => {
		todayPulseStore.start();
		return () => todayPulseStore.stop();
	});

	// formatSpend / spendTone / countTone / withYday / deltaAria live in
	// `$lib/today/pulseFormat` — shared with the /llm "Today vs yesterday"
	// section so both surfaces format and tint deltas identically. Tones
	// take the RAW today/yesterday numbers (never the formatted string).

	function buildChips(
		snapshot: PulseSnapshot,
		staleSlices: ReadonlyArray<PulseSliceName>
	): PulseChip[] {
		const stale = new Set(staleSlices);
		const chips: PulseChip[] = [];
		const llm = snapshot.llm;

		const spendValue = formatSpend(llm.spendToday);
		const spendYday = formatSpend(llm.spendYesterday);
		chips.push({
			id: 'llm-spend',
			label: 'LLM spend',
			value: spendValue,
			delta: `vs ${spendYday} yday`,
			tone: spendTone(llm.spendToday, llm.spendYesterday),
			href: '/llm#today',
			icon: 'zap',
			stale: stale.has('llm'),
			sparkline: llm.hourlySpend,
			ariaLabel: `LLM spend: ${spendValue} today, versus ${spendYday} yesterday`
		});

		// Calls stay NEUTRAL: fewer calls isn't "good" the way lower spend is
		// (a quiet day, a cached day, a broken day all look identical) — the
		// delta informs, it doesn't judge.
		const callsDelta = formatDelta(llm.callsToday, llm.callsYesterday, 'count');
		chips.push({
			id: 'llm-calls',
			label: 'LLM calls',
			value: llm.callsToday.toLocaleString(),
			delta: withYday(callsDelta),
			tone: 'neutral',
			href: '/llm#today',
			icon: 'message',
			stale: stale.has('llm'),
			ariaLabel: `LLM calls: ${llm.callsToday.toLocaleString()} today${deltaAria(callsDelta)}`
		});

		if (llm.topProvider) {
			const share = Math.round(llm.topProvider.sharePct);
			chips.push({
				id: 'top-model',
				label: 'Top model',
				value: llm.topProvider.model,
				delta: `${llm.topProvider.name} · ${share}% calls`,
				tone: 'neutral',
				href: '/llm#today',
				icon: 'sparkle',
				stale: stale.has('llm'),
				ariaLabel: `Top model: ${llm.topProvider.model} on ${llm.topProvider.name}, ${share} percent of today's calls`
			});
		}

		const tasksDone = snapshot.tasks.completedToday;
		const tasksDelta = formatDelta(tasksDone, snapshot.tasks.completedYesterday, 'count');
		chips.push({
			id: 'tasks-done',
			label: 'Tasks done',
			value: tasksDone.toLocaleString(),
			delta: withYday(tasksDelta),
			tone: countTone(tasksDone, snapshot.tasks.completedYesterday),
			href: '/tasks?filter=completed',
			icon: 'check',
			// Task counts come from taskStore, not a fetched slice — never stale.
			stale: false,
			ariaLabel: `Tasks done: ${tasksDone.toLocaleString()} today${deltaAria(tasksDelta)}`
		});

		chips.push({
			id: 'coding-runs',
			label: 'Coding runs',
			value: snapshot.codingRunsToday.toLocaleString(),
			delta: '',
			tone: 'neutral',
			href: '/vibe',
			icon: 'git-branch',
			stale: stale.has('coding'),
			ariaLabel: `Coding runs: ${snapshot.codingRunsToday.toLocaleString()} today`
		});

		chips.push({
			id: 'memories',
			label: 'Memories',
			value: snapshot.memoriesToday.toLocaleString(),
			delta: '',
			tone: 'neutral',
			href: '/memory',
			icon: 'archive',
			stale: stale.has('memory'),
			ariaLabel: `Memories learned: ${snapshot.memoriesToday.toLocaleString()} today`
		});

		if (snapshot.evals.casesToday > 0) {
			const cases = snapshot.evals.casesToday;
			const passPct = Math.round((snapshot.evals.passesToday * 100) / cases);
			chips.push({
				id: 'evals',
				label: 'Evals',
				value: cases.toLocaleString(),
				delta: `${passPct}% pass`,
				tone: 'neutral',
				href: '/memory',
				icon: 'eye',
				stale: stale.has('memory'),
				ariaLabel: `Evals: ${cases.toLocaleString()} cases today, ${passPct} percent passing`
			});
		}

		return chips;
	}

	$: pulseState = $todayPulseStore;
	// Hide-on-empty contract: unreachable analytics OR an all-zero day →
	// nothing at all (no dead chrome on the calmest page).
	$: pulseHidden =
		pulseState.unavailable ||
		(pulseState.snapshot !== null && pulseIsEmpty(pulseState.snapshot));
	$: pulseSkeleton = pulseState.loading && pulseState.snapshot === null;
	// Stale slices are baked into the chip objects (not looked up in the
	// template through a helper fn) so legacy-mode reactivity re-derives the
	// grid whenever either the snapshot or staleSlices changes.
	$: chips = pulseState.snapshot
		? buildChips(pulseState.snapshot, pulseState.staleSlices)
		: [];

	// Entrance gate, mirroring the page's `todayRowsIntroReady` pattern
	// (the flag itself is page-local, so the band mirrors rather than
	// imports): stay silent until the first loaded paint — tick() resolves
	// after the flush that mounts the chips — so the initial page load never
	// animates, while a later re-entry (band reappearing once an empty day
	// gains activity) settles in. Re-armed when the snapshot resets to null
	// (scope switch), matching the page treating a new scope as initial load.
	let pulseIntroReady = false;
	$: if (browser && pulseState.snapshot !== null && !pulseIntroReady) {
		void tick().then(() => {
			pulseIntroReady = true;
		});
	}
	$: if (pulseState.snapshot === null && pulseIntroReady) {
		pulseIntroReady = false;
	}
</script>

{#if !pulseHidden && (pulseSkeleton || chips.length > 0)}
	<section
		class="pulse-band"
		aria-label="Today's pulse"
		in:settleIn={pulseIntroReady ? {} : { duration: 0 }}
	>
		<div class="pulse-band__header">
			<h2>Today's pulse</h2>
			<a class="pulse-band__link" href="/llm#today">
				LLM details
				<Icon name="arrow-right" size={12} />
			</a>
		</div>

		{#if pulseSkeleton}
			<div class="pulse-band__grid" aria-hidden="true">
				{#each Array(SKELETON_CHIP_COUNT) as _, i (i)}
					<Skeleton height="4.4rem" radius="var(--radius-sm)" />
				{/each}
			</div>
		{:else}
			<div class="pulse-band__grid">
				{#each chips as chip (chip.id)}
					<a
						class="pulse-chip"
						class:pulse-chip--stale={chip.stale}
						class:pulse-chip--has-spark={Boolean(chip.sparkline)}
						href={chip.href}
						aria-label={chip.ariaLabel}
						title={chip.stale ? 'Refreshing…' : undefined}
					>
						<span class="pulse-chip__top">
							<Icon name={chip.icon} size={13} />
							<span>{chip.label}</span>
						</span>
						<span class="pulse-chip__value">{chip.value}</span>
						{#if chip.sparkline}
							<span class="pulse-chip__spark" aria-hidden="true">
								<Sparkline data={chip.sparkline} height={24} strokeWidth={1.5} />
							</span>
						{/if}
						{#if chip.delta}
							<span
								class="pulse-chip__delta"
								class:pulse-chip__delta--good={chip.tone === 'good'}
								class:pulse-chip__delta--bad={chip.tone === 'bad'}
							>
								{chip.delta}
							</span>
						{/if}
					</a>
				{/each}
			</div>
		{/if}
	</section>
{/if}

<style>
	/* Card shell — same idiom as the Today briefings/activity sections. */
	.pulse-band {
		display: grid;
		gap: 0.65rem;
		padding: clamp(0.85rem, 1.6vw, 1rem);
		border-radius: var(--radius-md);
		background:
			linear-gradient(
				180deg,
				color-mix(in srgb, var(--bg-card) 92%, transparent),
				color-mix(in srgb, var(--bg-soft) 46%, transparent)
			),
			var(--bg-card);
		border: 1px solid color-mix(in srgb, var(--border-soft) 70%, transparent);
		box-shadow: var(--shadow-md);
	}

	.pulse-band__header {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
		gap: 1rem;
		min-width: 0;
	}

	.pulse-band__header h2 {
		margin: 0;
		color: var(--text-primary);
		font-size: var(--text-md);
		line-height: 1.2;
	}

	.pulse-band__link {
		display: inline-flex;
		align-items: center;
		gap: 0.3rem;
		flex: 0 0 auto;
		border-radius: var(--radius-sm);
		color: var(--text-secondary);
		font-size: var(--text-xs);
		text-decoration: none;
	}

	.pulse-band__link:hover,
	.pulse-band__link:focus-visible {
		color: var(--text-primary);
		text-decoration: underline;
		text-decoration-thickness: 1px;
		text-underline-offset: 0.18em;
	}

	.pulse-band__link:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary) 60%, transparent);
		outline-offset: 2px;
	}

	.pulse-band__grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(130px, 1fr));
		grid-auto-flow: dense;
		gap: 0.5rem;
		width: 100%;
	}

	@media (min-width: 720px) {
		.pulse-chip--has-spark {
			grid-column: span 2;
		}
	}

	/* Whole chip = the link (house grammar: row body is the action). */
	.pulse-chip {
		display: grid;
		align-content: start;
		gap: 0.3rem;
		min-width: 0;
		padding: 0.6rem 0.75rem;
		border: 1px solid color-mix(in srgb, var(--border-soft) 70%, transparent);
		border-radius: var(--radius-sm);
		background: color-mix(in srgb, var(--bg-card) 82%, transparent);
		color: var(--text-primary);
		text-decoration: none;
		transition:
			border-color 140ms ease,
			background 140ms ease,
			opacity 140ms ease;
	}

	.pulse-chip:hover {
		border-color: color-mix(in srgb, var(--accent-primary) 38%, var(--border-soft));
		background: color-mix(in srgb, var(--accent-primary) 6%, var(--bg-card));
	}

	.pulse-chip:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary) 60%, transparent);
		outline-offset: 2px;
	}

	/* Slice mid-refresh after a failed fetch: numbers are last-good. */
	.pulse-chip--stale {
		opacity: 0.6;
	}

	.pulse-chip__top {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		min-width: 0;
		color: var(--text-muted);
		font-size: var(--text-2xs);
	}

	.pulse-chip__top > span {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.pulse-chip__top :global(svg) {
		flex: 0 0 auto;
	}

	.pulse-chip__value {
		color: var(--text-primary);
		font-size: var(--text-lg);
		font-variant-numeric: tabular-nums;
		line-height: 1.15;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.pulse-chip__spark {
		display: block;
		min-width: 0;
	}

	/* The sparkline is a decorative accent here: its min/max readout is
	   chart-panel furniture (and sits below the --text-2xs floor), so the
	   chip suppresses it. */
	.pulse-chip__spark :global(.muij-sparkline-range) {
		display: none;
	}

	.pulse-chip__delta {
		color: var(--text-muted);
		font-size: var(--text-2xs);
		font-variant-numeric: tabular-nums;
	}

	.pulse-chip__delta--good {
		color: color-mix(in srgb, var(--color-success) 72%, var(--text-muted));
	}

	.pulse-chip__delta--bad {
		color: color-mix(in srgb, var(--color-error) 72%, var(--text-muted));
	}
</style>
