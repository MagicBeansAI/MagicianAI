<script lang="ts">
	// One beat of a road — the film's station grammar, in document flow.
	//
	// The film's stations are: mono kicker with a rule, one display headline
	// whose words arrive a beat apart, one product-shaped mock, one sparse
	// closing caption. A beat is that same four-part shape, driven by its own
	// `--local` (beatReveal.ts) instead of the pinned scrubber. Everything a
	// road puts in the slot can opt into the stagger by wearing `.pb-in` and
	// setting `--i` — the same contract MovieTrack's rows use.
	/** Still accepted, no longer rendered — the kicker that showed them is
	 *  gone (see the markup). Kept because all nine call sites in
	 *  PathPromise, and PathLifecycle's own, pass them, and because the act
	 *  name is worth keeping in the markup's data even when it is not shown.
	 *  `void` marks them as deliberately unread rather than forgotten, the
	 *  same way PathDay marks `still`. */
	export let num: string;
	export let act: string;
	$: void num;
	$: void act;
	export let title: string;
	export let caption: string;
	/** Reduced motion: the beat renders finished and never listens. */
	export let still = false;

	$: words = title.split(' ');
</script>

<!-- Unnamed on purpose: an aria-label here would turn all nine beats into
     `region` landmarks and bury the two that matter (the fork's tablist and
     its panel). The heading outline — act h3, beat h4 — is the navigation. -->
<section class="pb" class:still data-beat data-station>
	<!-- No kicker. Each card used to open with its index and the act's name
	     again — "01 · KNOWS YOU" — directly under a fixed heading already
	     naming that act in gold. The number counted stations nobody was
	     counting, and the act name was the third place the same two words
	     appeared on one screen. -->

	<h4 class="pb-title">
		{#each words as w, wi}<span class="pb-w" style="--wi:{wi}">{w}{wi < words.length - 1 ? ' ' : ''}</span>{/each}
	</h4>

	<div class="pb-stage">
		<slot />
	</div>

	<p class="pb-caption">{caption}</p>
</section>

<style>
	.pb {
		--local: 1;
		position: relative;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: clamp(0.8rem, 1.8vh, 1.3rem);
		text-align: center;
		padding: clamp(3.5rem, 11vh, 8rem) clamp(1rem, 4vw, 2rem);
		min-height: clamp(0px, 62svh, 100vh);
		justify-content: center;
	}
	/* Each beat carries its act's bloom on the paper, the way every station
	   in the film plants one themed glow in the world it stands in. */
	.pb::before {
		content: '';
		position: absolute;
		inset: 6% -6% 10%;
		z-index: 0;
		pointer-events: none;
		background: radial-gradient(ellipse 60% 46% at 50% 42%, var(--ah), transparent 72%);
		opacity: clamp(0, calc(var(--local) * 1.6), 1);
		transition: opacity 650ms ease;
	}
	.pb > * {
		position: relative;
		z-index: 1;
	}


	.pb-title {
		margin: 0;
		max-width: 22ch;
		font-family: var(--lp-font);
		font-weight: 420;
		font-size: clamp(1.7rem, 3.9vw, 2.9rem);
		letter-spacing: -0.025em;
		line-height: 1.05;
		color: var(--pf-ink);
		text-wrap: balance;
	}
	.pb-w {
		display: inline-block;
		white-space: pre;
		opacity: clamp(0, calc((var(--local) - 0.04 - var(--wi) * 0.03) * 9), 1);
		transform: translateY(
			calc((1 - clamp(0, calc((var(--local) - 0.04 - var(--wi) * 0.03) * 9), 1)) * 0.34em)
		);
	}

	.pb-stage {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.8rem;
		width: 100%;
		margin-top: clamp(0.4rem, 1.4vh, 1rem);
	}

	/* The slot's own arrival contract, stated once: anything a road hands in
	   wears .pb-in and declares --i for its place in the queue. */
	.pb-stage :global(.pb-in) {
		opacity: clamp(0, calc((var(--local) - 0.16 - var(--i, 0) * 0.055) * 7), 1);
		transform: translateY(
			calc((1 - clamp(0, calc((var(--local) - 0.16 - var(--i, 0) * 0.055) * 7), 1)) * 10px)
		);
	}

	/* ── the mock vocabulary, stated once for both roads ──────────────
	   The film's stations all share one panel recipe (paper glass, a 1px
	   border, a shadow that bleeds the station's aurora onto the ground).
	   Both roads draw product-shaped mock UI, so that recipe and the small
	   parts that go inside it live here rather than twice. */
	/* ── THE MOCK ASSEMBLES ITSELF ────────────────────────────────────
	   The beats arrived fully built: the card was simply THERE, whole, the
	   moment its station came into focus, which makes a road of twelve
	   finished screenshots. The film's own beats build — tabs land one at a
	   time, counters climb, windows fly — and these should too.

	   All of it rides `--local`, which RoadTrack publishes per station from
	   the camera's own proximity, so nothing here needs a timer, an observer
	   or a second engine. Scrolling back takes it apart again.

	   The stagger comes from `:nth-child`, not from markup: the mocks are
	   slotted content owned by the roads, and a reveal that required every
	   row to declare an index would be a reveal nobody remembered to use. */
	.pb-stage :global(.pb-row),
	.pb-stage :global(.pb-chip) {
		--ri: 0;
		opacity: clamp(0, calc((var(--local, 1) - 0.12 - var(--ri) * 0.055) * 7), 1);
		transform: translateY(calc((1 - clamp(0, calc((var(--local, 1) - 0.12 - var(--ri) * 0.055) * 7), 1)) * 7px));
	}
	.pb-stage :global(.pb-row:nth-child(2)),
	.pb-stage :global(.pb-chip:nth-child(2)) {
		--ri: 1;
	}
	.pb-stage :global(.pb-row:nth-child(3)),
	.pb-stage :global(.pb-chip:nth-child(3)) {
		--ri: 2;
	}
	.pb-stage :global(.pb-row:nth-child(4)),
	.pb-stage :global(.pb-chip:nth-child(4)) {
		--ri: 3;
	}
	.pb-stage :global(.pb-row:nth-child(5)),
	.pb-stage :global(.pb-chip:nth-child(5)) {
		--ri: 4;
	}
	.pb-stage :global(.pb-row:nth-child(6)),
	.pb-stage :global(.pb-chip:nth-child(6)) {
		--ri: 5;
	}
	.pb-stage :global(.pb-row:nth-child(n + 7)),
	.pb-stage :global(.pb-chip:nth-child(n + 7)) {
		--ri: 6;
	}
	/* A FIGURE SETTLES rather than simply appearing. It arrives a shade
	   large and softly out of focus and resolves — which is what a number
	   finishing its count looks like, and it works for any figure without
	   this file having to know what the value is or how to count to it. */
	.pb-stage :global(.pb-figure) {
		--fs: clamp(0, calc((var(--local, 1) - 0.3) * 5), 1);
		display: inline-block;
		filter: blur(calc((1 - var(--fs)) * 3px));
		transform: scale(calc(1 + (1 - var(--fs)) * 0.16));
		opacity: clamp(0, calc(var(--fs) * 1.6), 1);
	}
	/* A status dot lands last and lands hard — it is the thing the row is
	   asserting, so it should arrive after the row that carries it. */
	.pb-stage :global(.pb-dot) {
		--ds: clamp(0, calc((var(--local, 1) - 0.34) * 6), 1);
		transform: scale(calc(0.2 + var(--ds) * 0.8));
		opacity: var(--ds);
	}
	/* A WELL FILLS TOP-DOWN. The wells hold a live transcript and a prompt as
	   the model receives it — both things that arrive line by line in reality
	   and were printed whole here. A soft mask sweeping down reads as exactly
	   that, and needs no per-line markup, which matters because the copy is
	   written with <br> between speakers rather than an element per line. */
	.pb-stage :global(.pb-well) {
		--we: clamp(0, calc((var(--local, 1) - 0.22) * 3), 1);
		-webkit-mask-image: linear-gradient(
			to bottom,
			#000 0 calc(var(--we) * 132% - 20%),
			transparent calc(var(--we) * 132%)
		);
		mask-image: linear-gradient(
			to bottom,
			#000 0 calc(var(--we) * 132% - 20%),
			transparent calc(var(--we) * 132%)
		);
	}
	/* A BUTTON IS OFFERED LAST — it is the only thing on the card you could
	   act on, so it should not be sitting there before the card has made its
	   case. */
	.pb-stage :global(.pb-btn) {
		--be: clamp(0, calc((var(--local, 1) - 0.46) * 6), 1);
		opacity: var(--be);
		transform: scale(calc(0.94 + var(--be) * 0.06));
	}
	/* The head's right-hand annotation, and the footnote, bracket the card:
	   the first qualifies the heading, the last qualifies everything. Both
	   arrive after the rows they are about. */
	.pb-stage :global(.pb-head-r) {
		opacity: clamp(0, calc((var(--local, 1) - 0.2) * 6), 1);
	}
	.pb-stage :global(.pb-foot) {
		--fe: clamp(0, calc((var(--local, 1) - 0.52) * 5), 1);
		opacity: var(--fe);
		transform: translateY(calc((1 - var(--fe)) * 5px));
	}
	/* The card itself rises into place under all of it. */
	.pb-stage :global(.pb-card) {
		--cs: clamp(0, calc(var(--local, 1) * 3), 1);
		opacity: var(--cs);
		transform: translateY(calc((1 - var(--cs)) * 16px));
	}
	/* Reduced motion, and the stacked document, get everything finished —
	   `--local` defaults to 1 everywhere these formulas read it, and the
	   overrides below make that explicit for the two that are not opacity. */
	.pb.still :global(.pb-row),
	.pb.still :global(.pb-chip),
	.pb.still :global(.pb-figure),
	.pb.still :global(.pb-dot),
	.pb.still :global(.pb-btn),
	.pb.still :global(.pb-head-r),
	.pb.still :global(.pb-foot),
	.pb.still :global(.pb-card) {
		opacity: 1;
		transform: none;
		filter: none;
	}
	.pb.still :global(.pb-well) {
		-webkit-mask-image: none;
		mask-image: none;
	}

	.pb-stage :global(.pb-card) {
		width: min(94vw, 37rem);
		box-sizing: border-box;
		text-align: left;
		border: 1px solid var(--pf-card-border);
		border-radius: 18px;
		background:
			linear-gradient(180deg, rgba(255, 255, 255, 0.05), transparent 40%),
			var(--pf-card);
		box-shadow:
			inset 0 1px 0 rgba(255, 255, 255, 0.06),
			var(--pf-shadow),
			0 24px 80px -30px var(--ahs);
		padding: 0.95rem 1.1rem;
		font-family: var(--lp-font);
		color: var(--pf-ink);
	}
	.pb-stage :global(.pb-card-wide) {
		width: min(96vw, 46rem);
	}
	/* Two mocks that argue with each other — the film's split panels. */
	.pb-stage :global(.pb-pair) {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(15rem, 1fr));
		gap: 0.8rem;
		width: min(96vw, 46rem);
	}
	/* Two cards side by side stretch to the taller one. Left as blocks that
	   leaves a pocket of dead paper under the shorter card's last row, so the
	   card becomes a column and its footnote takes the slack — the two
	   footnotes then sit on one line, which is what makes the pair read as one
	   exhibit rather than two leftovers. */
	.pb-stage :global(.pb-pair .pb-card) {
		display: flex;
		flex-direction: column;
		width: auto;
	}
	.pb-stage :global(.pb-pair .pb-foot) {
		margin-top: auto;
	}

	.pb-stage :global(.pb-head) {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		font-family: var(--lp-mono);
		font-size: 0.64rem;
		letter-spacing: 0.16em;
		text-transform: uppercase;
		color: var(--a-ink);
		padding-bottom: 0.6rem;
	}
	/* The right side of a mock's header is a status, not a title: it stays in
	   the case it was written in so "3m 12s" reads as a duration rather than
	   as a second heading shouting over the first. */
	.pb-stage :global(.pb-head-r) {
		margin-left: auto;
		color: var(--pf-dim);
		letter-spacing: 0.04em;
		text-transform: none;
	}
	/* The one number a mock is actually about. */
	.pb-stage :global(.pb-figure) {
		font-family: var(--lp-mono);
		font-size: 1.05rem;
		font-weight: 600;
		color: var(--a-ink);
		letter-spacing: -0.01em;
	}
	.pb-stage :global(.pb-row) {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		padding: 0.42rem 0;
		border-top: 1px solid var(--pf-line);
		font-size: 0.88rem;
		line-height: 1.35;
	}
	.pb-stage :global(.pb-row-r) {
		margin-left: auto;
		text-align: right;
		flex: none;
	}
	.pb-stage :global(.pb-dot) {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: var(--a);
		flex: none;
	}
	.pb-stage :global(.pb-dot-off) {
		background: color-mix(in srgb, var(--pf-ink) 22%, transparent);
	}
	.pb-stage :global(.pb-mono) {
		font-family: var(--lp-mono);
		font-size: 0.84em;
	}
	.pb-stage :global(.pb-dim) {
		color: var(--pf-dim);
	}
	.pb-stage :global(.pb-acc) {
		color: var(--a-ink);
	}
	.pb-stage :global(.pb-strike) {
		text-decoration: line-through;
		color: var(--pf-dim);
		opacity: 0.75;
	}
	.pb-stage :global(.pb-chip) {
		display: inline-block;
		font-family: var(--lp-mono);
		font-size: 0.64rem;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		padding: 0.16rem 0.5rem;
		border-radius: 999px;
		background: var(--ah);
		color: var(--a-ink);
		white-space: nowrap;
	}
	.pb-stage :global(.pb-chip-quiet) {
		background: transparent;
		border: 1px solid var(--pf-line);
		color: var(--pf-dim);
	}
	.pb-stage :global(.pb-chips) {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
		padding-top: 0.6rem;
	}
	.pb-stage :global(.pb-foot) {
		margin: 0.7rem 0 0;
		padding-top: 0.5rem;
		border-top: 1px dashed var(--pf-line);
		font-family: var(--lp-mono);
		font-size: 0.66rem;
		letter-spacing: 0.02em;
		color: var(--pf-dim);
	}
	.pb-stage :global(.pb-foot-acc) {
		color: var(--a-ink);
	}
	/* A field inside a mock: the sunken surface the film uses for inputs,
	   transcripts and anything the runtime is quoting back at you. */
	.pb-stage :global(.pb-well) {
		border: 1px solid var(--pf-line);
		background: var(--bg-soft, var(--landing-input-surface));
		border-radius: 10px;
		padding: 0.6rem 0.7rem;
		font-family: var(--lp-mono);
		font-size: 0.76rem;
		line-height: 1.6;
		color: var(--pf-dim);
	}
	.pb-stage :global(.pb-btn) {
		font-family: var(--lp-mono);
		font-size: 0.7rem;
		letter-spacing: 0.04em;
		padding: 0.34rem 0.7rem;
		border-radius: 8px;
		border: 1px solid var(--pf-line);
		background: transparent;
		color: var(--pf-dim);
	}
	.pb-stage :global(.pb-btn-go) {
		border-color: transparent;
		background: var(--a);
		color: var(--text-on-accent, #fff);
	}

	.pb-caption {
		margin: 0;
		max-width: 34rem;
		font-family: var(--lp-font);
		font-size: clamp(0.88rem, 1.5vw, 1rem);
		line-height: 1.5;
		color: var(--pf-dim);
		text-wrap: balance;
		opacity: clamp(0, calc((var(--local) - 0.55) * 6), 1);
		transform: translateY(calc((1 - clamp(0, calc((var(--local) - 0.55) * 6), 1)) * 8px));
	}

	/* Reduced motion: no scroll dependence anywhere. Every beat is a finished
	   block of a readable document, exactly as the film's stations are. */
	.pb.still {
		min-height: 0;
		padding-block: clamp(2rem, 5vh, 3rem);
	}
	.pb.still::before {
		opacity: 1;
	}
	.pb.still .pb-w,
	.pb.still .pb-caption,
	.pb.still .pb-stage :global(.pb-in) {
		opacity: 1;
		transform: none;
	}

	@media (max-width: 640px) {
		.pb {
			min-height: clamp(0px, 56svh, 100vh);
			padding-inline: 1rem;
		}
		.pb-title {
			max-width: 18ch;
		}
	}
</style>
