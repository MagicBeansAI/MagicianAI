<script lang="ts">
	// The divider between a road's acts — the film's road signage ("One day
	// with it:") reused, so an act change reads as the same kind of event it
	// does inside the movie: a big name on the paper, then the road resumes.
	export let ordinal: string;
	export let name: string;
	export let line: string;
	export let still = false;
	/**
	 * Optional. The road's act, restated as one of TrustReveal's three
	 * promises ("Knows you" / "Acts for you" / "Belongs to you") — added
	 * 2026-08-17 so PathLifecycle's Started/Moving/Closed arc can carry the
	 * promise it is proof of on its own sign, without a visitor needing to
	 * hold both framings in their head at once. Unset by default: every
	 * existing call site (PathPromise's own three acts, which already ARE
	 * the promises) is unaffected.
	 */
	export let tag = '';
</script>

<!-- A plain div, not a <header>: a <header> outside a sectioning element
     computes as a `banner` landmark, and a road would then plant three more
     banners on a page that already has one. The h3 carries the outline. -->
<div class="pas" class:still data-beat data-station>
	<span class="pas-num">{ordinal}</span>
	<h3>{name}</h3>
	{#if tag}
		<span class="pas-tag">{tag}</span>
	{/if}
	<p>{line}</p>
</div>

<style>
	.pas {
		--local: 1;
		max-width: 46rem;
		margin: 0 auto;
		padding: clamp(3rem, 10vh, 6.5rem) clamp(1rem, 4vw, 2rem) 0;
		text-align: center;
		opacity: clamp(0, calc((var(--local) - 0.05) * 5), 1);
		transform: translateY(calc((1 - clamp(0, calc((var(--local) - 0.05) * 5), 1)) * 14px));
	}
	.pas-num {
		font-family: var(--lp-mono);
		font-size: 0.66rem;
		letter-spacing: 0.4em;
		text-transform: uppercase;
		color: var(--pf-dim);
	}
	.pas h3 {
		margin: 0.75rem 0 0.55rem;
		font-family: var(--lp-font);
		font-size: clamp(2.3rem, 6.6vw, 4rem);
		font-weight: 440;
		letter-spacing: -0.03em;
		line-height: 1;
		background: linear-gradient(96deg, var(--pf-ink), var(--a) 62%, var(--a2));
		-webkit-background-clip: text;
		background-clip: text;
		color: transparent;
	}
	.pas-tag {
		display: inline-block;
		margin: 0 0 0.9rem;
		padding: 0.22rem 0.7rem;
		border: 1px solid var(--pf-line, currentColor);
		border-radius: 999px;
		font-family: var(--lp-mono);
		font-size: 0.66rem;
		font-weight: 650;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--a, var(--pf-dim));
	}
	.pas p {
		margin: 0;
		font-family: var(--lp-font);
		font-size: clamp(0.92rem, 1.7vw, 1.06rem);
		line-height: 1.5;
		color: var(--pf-dim);
		text-wrap: balance;
	}
	.pas.still {
		opacity: 1;
		transform: none;
		padding-top: clamp(2rem, 6vh, 3.4rem);
	}
</style>
