<!--
	THE PROMISES ROAD, mounted over the landing backdrop.

	This used to be a switcher: "The three promises" and "A day in your life"
	as two tabs, one mounted at a time, the day reachable only by choosing it
	instead of this. Owner call replaced that with a SEQUENCE — the backdrop
	holds while the hero plays and then while all three promise acts play over
	it, and only once those are finished does the page leave the backdrop for
	the day, on plain ground, and then the montage. A toggle offered them as
	alternatives; the page argues them in order.

	What survives is the CSS token wrapper. `.ps-panel` reproduces the act
	colour rules PathPromise actually uses (`.pa-knows` / `.pa-acts` /
	`.pa-belongs`), copied verbatim from PathFork.svelte — PathFork sets them
	via `.pf-panel :global(...)`, which carries PathFork's own Svelte scoping
	hash, so reusing the class names alone does not inherit the colours.
	That is the whole reason this component still exists rather than
	PathPromise being mounted directly.
-->
<script lang="ts">
	import { motionEnabled } from '$lib/motion';
	import PathPromise from './PathPromise.svelte';

	$: reduced = !$motionEnabled;
	/** Passed straight through — see RoadTrack.progress. */
	export let progress: number | null = null;
</script>

<div class="ps-root">
	<!-- The intro is gone. It printed "Knows you · Acts for you · Belongs to
	     you" as a kicker and then explained the three promises in prose —
	     both of which LandingChrome now does live, titling each act as its
	     own stations arrive. Saying it twice, once statically above the road
	     and once again over it, made the static copy read as a caption for
	     something the visitor had not reached yet. -->


	<div class="ps-panel">
		<PathPromise {progress} still={reduced} />
	</div>

</div>

<style>
	.ps-root {
		position: relative;
	}


	/* ── the borrowed PathFork tokens, verbatim — see the header comment ── */
	.ps-panel {
		--pf-ink: var(--text-primary, #2d3436);
		--pf-dim: color-mix(in srgb, var(--pf-ink) 68%, transparent);
		--pf-card: color-mix(in srgb, var(--bg-elevated, var(--landing-bg)) 86%, transparent);
		--pf-card-border: var(--border-default, color-mix(in srgb, var(--pf-ink) 14%, transparent));
		--pf-line: var(--border-soft, color-mix(in srgb, var(--pf-ink) 13%, transparent));
		--pf-shadow: var(--shadow-lg, var(--landing-task-card-shadow));
		--a: var(--accent-primary, #9e59ff);
		--a2: var(--accent-secondary, var(--a));
		--a-ink: color-mix(in srgb, var(--a) 60%, var(--pf-ink));
		--ah: color-mix(in srgb, var(--a) 14%, transparent);
		--ahs: color-mix(in srgb, var(--a) 32%, transparent);
		background: var(--landing-bg);
		color: var(--pf-ink);
	}
	.ps-panel :global(.pa-knows) {
		--a: var(--accent-secondary, #9e59ff);
		--a2: color-mix(in srgb, var(--accent-secondary) 58%, var(--accent-primary));
	}
	.ps-panel :global(.pa-acts) {
		--a: var(--accent-primary, #9e59ff);
		--a2: var(--accent-secondary, var(--a));
	}
	.ps-panel :global(.pa-belongs) {
		--a: color-mix(in srgb, var(--accent-primary) 48%, var(--accent-secondary));
		--a2: var(--accent-primary);
	}
	.ps-panel :global(.pa-act) {
		--a-ink: color-mix(in srgb, var(--a) 60%, var(--pf-ink));
		--ah: color-mix(in srgb, var(--a) 13%, transparent);
		--ahs: color-mix(in srgb, var(--a) 30%, transparent);
	}
</style>
