<script lang="ts">
	// The fork — where the film ends and the visitor chooses whether to keep
	// going, and down which road.
	//
	// It sits AFTER the whole current experience (film → trust → creator →
	// ask), so nobody meets it before they have been offered the one thing the
	// page wants. No road is in the DOM until a tab is pressed: a visitor who
	// does not opt in pays nothing for this section beyond the fork card
	// itself. And each road ends by handing the ask back, so scrolling past the
	// CTA can never strand someone who had already decided.
	//
	// Road C ("A day with it") joined on 2026-08-05, when the day-in-the-life
	// left the main scroll. It changes nothing structural: the tablist, the
	// roving tabindex, the single-mount rule and the returned ask are the same
	// contract they were for two.
	//
	// Selection is in-memory for the visit. There is no localStorage and no URL
	// parameter: a returning visitor meets the same closed fork, which is the
	// behaviour the default ("neither expanded until chosen") describes.
	import { tick } from 'svelte';
	import { motionEnabled } from '$lib/motion';
	import RoadModal from './RoadModal.svelte';
	import PathLifecycle from './PathLifecycle.svelte';
	import PathPromise from './PathPromise.svelte';
	import {
		PATHS,
		arrowTarget,
		otherPath,
		pathById,
		pathIndex,
		requestedPath,
		type PathChoice,
		type PathId
	} from './pathFork';

	let chosen: PathChoice = null;

	// Someone elsewhere on the page asked for a road — the film's closing
	// marker, or the link under the ask. Honour it and clear the request, so
	// the same link works a second time.
	$: if ($requestedPath) {
		const want = $requestedPath;
		requestedPath.set(null);
		void choose(want);
	}
	let panelEl: HTMLElement | null = null;
	let tabEls: HTMLButtonElement[] = [];
	/** Roving tabindex: with nothing chosen the first tab carries the stop. */
	let focusIndex = 0;

	$: still = !$motionEnabled;
	$: current = pathById(chosen);
	$: other = chosen ? otherPath(chosen) : null;

	// `null` CLOSES the road. The fork used to have no closed-with-a-choice
	// state — a road was open or the page had never been asked — because
	// inline there was nowhere to close TO. A modal has somewhere.
	async function choose(id: PathId | null): Promise<void> {
		const changed = chosen !== id;
		chosen = id;
		if (id) focusIndex = pathIndex(id);
		if (!changed) return;
		await tick();
		// Landing and focus are the MODAL's job now: it scrolls its own
		// scroller to the road's head and takes the focus ring, and on close
		// it hands focus back to whatever opened it.
	}

	function onTabKey(event: KeyboardEvent, index: number): void {
		const next = arrowTarget(index, event.key, PATHS.length);
		if (next < 0) return;
		event.preventDefault();
		focusIndex = next;
		tabEls[next]?.focus();
	}

	// The safeguard: every road ends within one press of the real ask, so the
	// page's one conversion point is never below the visitor's last scroll.
	function toTheAsk(): void {
		document
			.getElementById('landing-cta')
			?.scrollIntoView({ behavior: still ? 'auto' : 'smooth', block: 'center' });
		(document.getElementById('landing-task-description') as HTMLInputElement | null)?.focus({
			preventScroll: true
		});
	}
</script>

<div class="pf-root">
	<section class="pf" id="the-fork" aria-labelledby="pf-h">
		<p class="pf-kicker"><span class="pf-rule" aria-hidden="true"></span>the road splits<span class="pf-rule" aria-hidden="true"></span></p>
		<h2 id="pf-h">Three more ways to look at it.</h2>
		<p class="pf-sub">Pick one and keep scrolling. The others stay open the whole way down.</p>

		<!-- `aria-controls` is set only once a panel exists to control: the
		     fork's resting state is genuinely closed, not a tablist pointing at
		     an id that is not in the document. -->
		<div class="pf-tabs" role="tablist" aria-label="Three more ways to look at it">
			{#each PATHS as path, i}
				<button
					bind:this={tabEls[i]}
					type="button"
					role="tab"
					id="pf-tab-{path.id}"
					class="pf-tab"
					class:on={chosen === path.id}
					aria-selected={chosen === path.id}
					aria-controls={chosen ? 'pf-panel' : undefined}
					tabindex={focusIndex === i ? 0 : -1}
					aria-label="{path.label} — {path.spine}"
					on:click={() => choose(path.id)}
					on:keydown={(e) => onTabKey(e, i)}
				>
					<span class="pf-tab-mark" aria-hidden="true">{path.mark}</span>
					<span class="pf-tab-label">{path.label}</span>
					<!-- The road drawn as a rail: three stops on one line, the
					     protagonist thread's motif at card size. aria-hidden
					     because the tab's own label already says the spine. -->
					<span class="pf-tab-rail" aria-hidden="true">
						{#each path.acts as name}
							<span class="pf-tab-stop"><i></i>{name}</span>
						{/each}
					</span>
					<span class="pf-tab-blurb">{path.blurb}</span>
					<span class="pf-tab-go" aria-hidden="true">{chosen === path.id ? 'reading ↓' : 'take this road ↓'}</span>
				</button>
			{/each}
		</div>
	</section>

	<!-- THE ROAD OPENS AS A MODAL. Inline, a pinned scrubbed track fought the
	     page's own scroll and this fork's sticky switcher, and a visitor who
	     scrolled deep into a road had to climb back out of it. In its own
	     full-viewport scroller it has neither problem — and closing puts them
	     back exactly where they were, which is why the roads no longer have
	     to end by handing the ask back to avoid stranding anyone. -->
	<RoadModal
		open={!!chosen && !!current}
		label={current ? `${current.label} — ${current.spine}` : ''}
		onClose={() => choose(null)}
	>
		<div class="pf-panel" id="pf-panel" tabindex="-1" bind:this={panelEl}>
			{#if current}
			<!-- The head travels with the road: which one you are on, and the
			     one press it takes to be on the other. -->
			<!-- ALL THREE ROADS, always. This showed the current one and a
			     single `other` — a ROTATION, which made sense when the roads
			     were inline and there were two of them: you took the one you
			     were on or the one offered. With three, a rotation hides a
			     road behind a road, and a visitor who wants the day from
			     inside the promises has to guess that pressing once will
			     eventually get there. In a modal there is room to just show
			     them. -->
			<div class="pf-switch" class:still>
				{#each PATHS as road (road.id)}
					{#if road.id === chosen}
						<span class="pf-switch-here" aria-current="true">
							<span class="pf-sr">Reading:</span><b aria-hidden="true">{road.mark}</b>
							{road.label}
						</span>
					{:else}
						<button
							type="button"
							class="pf-switch-go"
							aria-label="Switch to {road.label} — {road.spine}"
							on:click={() => choose(road.id)}
						>
							<b aria-hidden="true">{road.mark}</b>
							{road.label}
						</button>
					{/if}
				{/each}
			</div>

			{#if chosen === 'promise'}
				<PathPromise {still} />
			{:else if chosen === 'lifecycle'}
				<PathLifecycle {still} />
			{/if}

			<section class="pf-close" aria-label="Back to the ask">
				<h3>{current.close.line}</h3>
				<p>{current.close.sub}</p>
				<button type="button" class="pf-ask" on:click={toTheAsk}>
					Give it something <span aria-hidden="true">↑</span>
				</button>
				{#if other}
					<button type="button" class="pf-close-alt" on:click={() => choose(other.id)}>
						or take the other road — {other.spine}
					</button>
				{/if}
			</section>
			{/if}
		</div>
	</RoadModal>
</div>

<style>
	.pf-root {
		--pf-ink: var(--text-primary, #2d3436);
		--pf-dim: color-mix(in srgb, var(--pf-ink) 68%, transparent);
		--pf-card: color-mix(in srgb, var(--bg-elevated, var(--landing-bg)) 86%, transparent);
		--pf-card-border: var(--border-default, color-mix(in srgb, var(--pf-ink) 14%, transparent));
		--pf-line: var(--border-soft, color-mix(in srgb, var(--pf-ink) 13%, transparent));
		--pf-shadow: var(--shadow-lg, var(--landing-task-card-shadow));
		/* The fork's own chrome runs on the theme pair, undivided — it belongs
		   to neither road, so it takes no act's accent. */
		--a: var(--accent-primary, #9e59ff);
		--a2: var(--accent-secondary, var(--a));
		--a-ink: color-mix(in srgb, var(--a) 60%, var(--pf-ink));
		--ah: color-mix(in srgb, var(--a) 14%, transparent);
		--ahs: color-mix(in srgb, var(--a) 32%, transparent);
		position: relative;
		background: var(--landing-bg);
		font-family: var(--lp-font);
		color: var(--pf-ink);
	}

	/* ── the fork ───────────────────────────────────────────────── */
	.pf {
		max-width: 64rem;
		margin: 0 auto;
		padding: clamp(1rem, 3vh, 2rem) clamp(1rem, 4vw, 2.5rem) clamp(3rem, 8vh, 5rem);
		text-align: center;
	}
	.pf-kicker {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 0.9em;
		margin: 0 0 0.9rem;
		font-family: var(--lp-mono);
		font-size: 0.66rem;
		font-weight: 500;
		letter-spacing: 0.34em;
		text-transform: uppercase;
		color: var(--a-ink);
	}
	.pf-rule {
		width: 2.6em;
		height: 1px;
		background: var(--a);
		opacity: 0.55;
	}
	.pf h2 {
		margin: 0 0 0.6rem;
		font-size: clamp(1.7rem, 4vw, 2.6rem);
		font-weight: 420;
		letter-spacing: -0.02em;
		background: var(--landing-title-gradient, linear-gradient(100deg, var(--pf-ink), var(--a)));
		-webkit-background-clip: text;
		background-clip: text;
		color: transparent;
	}
	.pf-sub {
		margin: 0 auto clamp(1.6rem, 4vh, 2.4rem);
		max-width: 34rem;
		font-size: clamp(0.92rem, 1.7vw, 1.05rem);
		line-height: 1.55;
		color: var(--pf-dim);
		text-wrap: balance;
	}

	/* Three cards at 17rem need 53rem plus gaps, which the 64rem shell has;
	   below that auto-fit drops to two and then to one on its own, so the
	   third road costs no breakpoint of its own. */
	.pf-tabs {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(17rem, 1fr));
		gap: 1rem;
	}
	/* Each road is one card-sized target: the mark, the name, the spine it
	   keeps, and what it is going to spend your scrolling on. */
	.pf-tab {
		display: grid;
		gap: 0.4rem;
		text-align: left;
		font: inherit;
		color: inherit;
		cursor: pointer;
		padding: 1.1rem 1.2rem 1rem;
		border: 1px solid var(--pf-card-border);
		border-top: 3px solid color-mix(in srgb, var(--a) 55%, transparent);
		border-radius: var(--radius-lg, 16px);
		background: var(--pf-card);
		box-shadow: 0 18px 44px -24px var(--ahs);
		transition:
			transform 650ms cubic-bezier(0.22, 1, 0.36, 1),
			box-shadow 650ms ease,
			border-color 650ms ease;
	}
	/* Each road wears its own temperature — mark, rail, shadow and rule.
	   Roads that differ only in words are a list; roads that differ in
	   temperature are a fork. The three sit on the theme pair rather than on
	   three invented hues: primary, secondary, and the blend between them,
	   which is the same three-step the film's own phase table walks. */
	.pf-tabs > .pf-tab:nth-child(2) {
		--a: var(--accent-secondary, var(--accent-primary));
		--a-ink: color-mix(in srgb, var(--a) 60%, var(--pf-ink));
		--ah: color-mix(in srgb, var(--a) 14%, transparent);
		--ahs: color-mix(in srgb, var(--a) 32%, transparent);
		border-top-color: color-mix(in srgb, var(--a) 55%, transparent);
	}
	.pf-tabs > .pf-tab:nth-child(3) {
		--a: color-mix(in srgb, var(--accent-primary) 52%, var(--accent-secondary));
		--a-ink: color-mix(in srgb, var(--a) 60%, var(--pf-ink));
		--ah: color-mix(in srgb, var(--a) 14%, transparent);
		--ahs: color-mix(in srgb, var(--a) 32%, transparent);
		border-top-color: color-mix(in srgb, var(--a) 55%, transparent);
	}
	.pf-tab:hover {
		transform: translateY(-3px);
		box-shadow: 0 26px 60px -26px var(--ahs);
	}
	.pf-tab:focus-visible {
		outline: 2px solid var(--a);
		outline-offset: 3px;
	}
	.pf-tab.on {
		border-top-width: 3px;
		border-top-color: var(--a);
		box-shadow:
			0 0 0 1px var(--ahs),
			0 26px 60px -26px var(--ahs);
	}
	.pf-tab-mark {
		font-family: var(--lp-mono);
		font-size: 0.68rem;
		letter-spacing: 0.3em;
		color: var(--a-ink);
	}
	.pf-tab-label {
		font-size: clamp(1.2rem, 2.4vw, 1.5rem);
		font-weight: 460;
		letter-spacing: -0.015em;
	}
	.pf-tab-rail {
		position: relative;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.4rem;
		margin: 0.35rem 0 0.15rem;
		font-family: var(--lp-mono);
		font-size: 0.66rem;
		letter-spacing: 0.02em;
		color: var(--a-ink);
	}
	/* The line the stops sit on, drawn behind them and stopping short of
	   both ends so the first and last stop terminate it rather than float
	   on it. */
	.pf-tab-rail::before {
		content: '';
		position: absolute;
		left: 3px;
		right: 3px;
		top: 3px;
		height: 1px;
		background: linear-gradient(
			90deg,
			var(--a),
			color-mix(in srgb, var(--a) 40%, transparent)
		);
		opacity: 0.55;
	}
	.pf-tab-stop {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.4rem;
		flex: 1;
		min-width: 0;
	}
	/* The last stop lands ON the end of the line rather than short of it —
	   a rail that overshoots its terminus reads as unfinished. */
	.pf-tab-stop:last-child {
		flex: none;
		align-items: flex-end;
		text-align: right;
	}
	.pf-tab-stop i {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--a);
		box-shadow: 0 0 0 3px var(--ah);
	}
	.pf-tab.on .pf-tab-stop i {
		box-shadow: 0 0 0 4px var(--ah);
	}
	.pf-tab-blurb {
		font-size: 0.9rem;
		line-height: 1.5;
		color: var(--pf-dim);
	}
	.pf-tab-go {
		margin-top: 0.35rem;
		font-family: var(--lp-mono);
		font-size: 0.66rem;
		letter-spacing: 0.16em;
		text-transform: uppercase;
		color: var(--a-ink);
	}

	/* ── the chosen road ────────────────────────────────────────── */
	.pf-panel {
		outline: none;
	}
	.pf-panel:focus-visible {
		outline: 2px solid var(--a);
		outline-offset: -4px;
	}

	.pf-switch {
		position: sticky;
		top: 0;
		z-index: 6;
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
		max-width: 64rem;
		/* clears the fixed reel/theme cluster in the top-right corner */
		margin: 0 auto;
		padding: 0.6rem clamp(5.5rem, 12vw, 7rem) 0.6rem clamp(1rem, 4vw, 2.5rem);
		font-family: var(--lp-mono);
		font-size: 0.68rem;
		letter-spacing: 0.06em;
	}
	/* The bar has to sit over live copy without a seam: solid where the pills
	   are, then a long dissolve into the paper the beats are drawn on. */
	.pf-switch::before {
		content: '';
		position: absolute;
		inset: 0 0 -1.4rem;
		z-index: -1;
		background: linear-gradient(180deg, var(--landing-bg) 58%, transparent);
	}
	.pf-switch-here,
	.pf-switch-go {
		display: inline-flex;
		align-items: center;
		gap: 0.5em;
		padding: 0.32rem 0.7rem;
		border-radius: 999px;
		border: 1px solid transparent;
		font: inherit;
	}
	.pf-switch-here {
		background: var(--ah);
		border-color: color-mix(in srgb, var(--a) 26%, transparent);
		color: var(--a-ink);
	}
	.pf-switch-go {
		background: color-mix(in srgb, var(--bg-elevated, var(--landing-bg)) 80%, transparent);
		border-color: var(--pf-line);
		color: var(--pf-dim);
		cursor: pointer;
		transition:
			color 650ms ease,
			border-color 650ms ease;
	}
	.pf-switch-go:hover {
		color: var(--a-ink);
		border-color: color-mix(in srgb, var(--a) 32%, transparent);
	}
	.pf-switch-go:focus-visible {
		outline: 2px solid var(--a);
		outline-offset: 2px;
	}
	.pf-switch-here b,
	.pf-switch-go b {
		font-weight: 700;
		opacity: 0.7;
	}
	/* "Reading:" is the one word a screen reader needs and a sighted visitor
	   already gets from the pill being filled. */
	.pf-sr {
		position: absolute;
		width: 1px;
		height: 1px;
		margin: -1px;
		padding: 0;
		overflow: hidden;
		clip-path: inset(50%);
		white-space: nowrap;
	}
	.pf-switch.still {
		position: static;
	}

	/* ── act accents ────────────────────────────────────────────
	   One themed phase per act, taken from the film's own phase table so a
	   road's colour temperature is the product's, not a new palette: the
	   day's aurora runs primary → secondary and back exactly as it does on
	   the orb. :global because the acts are declared by the road components
	   this panel hosts. */
	.pf-panel :global(.pa-knows) {
		--a: var(--accent-secondary, #9e59ff);
		--a2: color-mix(in srgb, var(--accent-secondary) 58%, var(--accent-primary));
	}
	.pf-panel :global(.pa-acts),
	.pf-panel :global(.pa-started) {
		--a: var(--accent-primary, #9e59ff);
		--a2: var(--accent-secondary, var(--a));
	}
	.pf-panel :global(.pa-belongs) {
		--a: color-mix(in srgb, var(--accent-primary) 48%, var(--accent-secondary));
		--a2: var(--accent-primary);
	}
	.pf-panel :global(.pa-moving) {
		--a: color-mix(in srgb, var(--accent-primary) 58%, var(--accent-secondary));
		--a2: color-mix(in srgb, var(--accent-secondary) 62%, var(--accent-primary));
	}
	.pf-panel :global(.pa-closed) {
		--a: var(--accent-secondary, #9e59ff);
		--a2: var(--accent-primary, var(--a));
	}
	.pf-panel :global(.pa-act) {
		--a-ink: color-mix(in srgb, var(--a) 60%, var(--pf-ink));
		--ah: color-mix(in srgb, var(--a) 13%, transparent);
		--ahs: color-mix(in srgb, var(--a) 30%, transparent);
	}

	/* ── the road's last frame: the ask, handed back ────────────── */
	.pf-close {
		max-width: 40rem;
		margin: 0 auto;
		padding: clamp(2.5rem, 8vh, 5rem) clamp(1rem, 4vw, 2rem) clamp(3rem, 9vh, 5rem);
		text-align: center;
	}
	.pf-close h3 {
		margin: 0 0 0.6rem;
		font-size: clamp(1.7rem, 4vw, 2.5rem);
		font-weight: 420;
		letter-spacing: -0.02em;
		background: var(--landing-title-gradient, linear-gradient(100deg, var(--pf-ink), var(--a)));
		-webkit-background-clip: text;
		background-clip: text;
		color: transparent;
	}
	.pf-close p {
		margin: 0 0 1.5rem;
		font-size: clamp(0.92rem, 1.7vw, 1.05rem);
		line-height: 1.55;
		color: var(--pf-dim);
		text-wrap: balance;
	}
	.pf-ask {
		font: inherit;
		font-weight: 560;
		font-size: 1rem;
		cursor: pointer;
		padding: 0.72rem 1.5rem;
		border: none;
		border-radius: 999px;
		/* Aurora, not a two-brand sweep: the accent shifts a fraction toward
		   its partner rather than crossing the whole way, which on warm/cool
		   theme pairs is the difference between a glow and a mud line. */
		background: linear-gradient(
			100deg,
			var(--a),
			color-mix(in srgb, var(--a) 74%, var(--a2))
		);
		color: var(--text-on-accent, #fff);
		box-shadow: 0 16px 40px -16px var(--ahs);
		transition:
			transform 650ms cubic-bezier(0.22, 1, 0.36, 1),
			box-shadow 650ms ease;
	}
	.pf-ask:hover {
		transform: translateY(-2px);
		box-shadow: 0 22px 52px -18px var(--ahs);
	}
	.pf-ask:focus-visible {
		outline: 2px solid var(--a);
		outline-offset: 3px;
	}
	.pf-close-alt {
		display: block;
		margin: 1.1rem auto 0;
		padding: 0.3rem 0.2rem;
		font-family: var(--lp-mono);
		font-size: 0.7rem;
		letter-spacing: 0.04em;
		border: none;
		background: none;
		color: var(--pf-dim);
		cursor: pointer;
		text-decoration: underline;
		text-underline-offset: 4px;
		text-decoration-color: var(--pf-line);
	}
	.pf-close-alt:hover {
		color: var(--a-ink);
	}
	.pf-close-alt:focus-visible {
		outline: 2px solid var(--a);
		outline-offset: 2px;
		border-radius: 6px;
	}

	@media (prefers-reduced-motion: reduce) {
		.pf-tab,
		.pf-ask {
			transition: none;
		}
		.pf-tab:hover,
		.pf-ask:hover {
			transform: none;
		}
	}

	@media (max-width: 640px) {
		/* The two pills stack under the fixed corner chrome rather than
		   fighting it for the line — so the bar costs one thumb of height,
		   not two. */
		.pf-switch {
			font-size: 0.63rem;
			padding: 0.45rem 5.4rem 0.5rem 1rem;
			gap: 0.3rem;
		}
		.pf-tab {
			padding: 0.95rem 1rem;
		}
	}
</style>
