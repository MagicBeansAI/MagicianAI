<script lang="ts">
	// A NUMBER THAT ARRIVES BY COUNTING.
	//
	// A meter that is simply printed is a screenshot of a meter. The roads are
	// full of readouts a real run would produce — what it cost, how many steps
	// it took, how much of the context was cached — and every one of them was
	// sitting at its final value before the visitor had finished reading the
	// heading above it.
	//
	// NO JAVASCRIPT RUNS HERE. The count rides `--local`, the station's own
	// dwell progress, which RoadTrack publishes on every `[data-station]` — so
	// it scrubs with the scroll, settles when the camera settles, and counts
	// back down if you scroll away. A registered `<integer>` property is what
	// makes that possible: `counter()` can only render a counter, and
	// `counter-reset` can only take an integer, so the easing has to be rounded
	// by the engine before it ever reaches the box tree.
	//
	// TWO RULES ON THE NUMBERS, both measured rather than assumed:
	//
	//   · `from` AND `to` MUST HAVE THE SAME DIGIT COUNT. There is no zero-pad
	//     in CSS counters, so a count from 0 would change width as it climbed
	//     and the row would jitter. Starting a cost part-way is also simply
	//     what a cost meter does — it is never at zero once work has begun.
	//   · NEITHER MAY REACH 1,000,000. A registered property's computed value
	//     is serialised with six significant digits, so 1832625 substitutes
	//     into `counter-reset` as `1.83262e+06`, which is not an integer — the
	//     declaration is dropped and the counter renders 0 forever. The cost
	//     row below the fold was printing `$0.00` for exactly this reason.
	//     Split the value across the prefix instead: count 100000 → 832625
	//     behind a `$0.01` head rather than 1000000 → 1832625 behind `$0.0`.

	/** Where the count starts. Same digit count as `to` — see above. */
	export let from: number;
	/** The real value. This is what the row is actually claiming. */
	export let to: number;
	/** Printed before the digits, e.g. a currency mark or a fixed decimal head. */
	export let pre = '';
	/** Printed after the digits, e.g. a unit. */
	export let post = '';
	/** Where in the station's local the count begins, and how long it runs. */
	export let at = 0.3;
	export let span = 0.4;
	/** Stacked-document / reduced-motion: print the final value, full stop. */
	export let still = false;
</script>

{#if still}
	<span class="pc-plain">{pre}{to}{post}</span>
{:else}
	<!-- The digits live in a pseudo-element, so the accessible name comes from
	     the real text beside them rather than from a value mid-count. -->
	<span
		class="pc"
		style="--from:{from};--to:{to};--at:{at};--span:{span};--pre:'{pre}';--post:'{post}'"
		aria-hidden="true"
	></span>
	<span class="pc-sr">{pre}{to}{post}</span>
{/if}

<style>
	@property --pcn {
		syntax: '<integer>';
		inherits: false;
		initial-value: 0;
	}
	.pc {
		--e: clamp(0, calc((var(--local, 1) - var(--at)) / var(--span)), 1);
		--pcn: calc(var(--from) + (var(--to) - var(--from)) * var(--e));
		counter-reset: pcn var(--pcn);
		/* Tabular figures, or the row's right edge walks while it counts. */
		font-variant-numeric: tabular-nums;
	}
	.pc::after {
		content: var(--pre, '') counter(pcn) var(--post, '');
	}
	.pc-plain {
		font-variant-numeric: tabular-nums;
	}
	/* Available to assistive tech, invisible and unmeasured otherwise. */
	.pc-sr {
		position: absolute;
		width: 1px;
		height: 1px;
		margin: -1px;
		padding: 0;
		overflow: hidden;
		clip-path: inset(50%);
		white-space: nowrap;
		border: 0;
	}
	/* A browser without registered properties would render `counter(pcn)` as
	   0 forever, which is worse than a static number. Reduced motion takes the
	   same exit the rest of the landing does. */
	@media (prefers-reduced-motion: reduce) {
		.pc {
			--e: 1;
		}
	}
</style>
