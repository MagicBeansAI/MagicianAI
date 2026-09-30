<script lang="ts">
	// Road C — "A day with it".
	//
	// This is now a thin mount, and the road IS the day: `DayTrack` is the
	// original scroll-scrubbed day extracted whole from commit 7a4087161 —
	// eleven stations from 7:30 to 22:30, the aurora thread running through
	// them, the depth-of-field camera, the five-phase voice orb, the
	// lock-screen wake, the Pythagoras overlay and the closing receipt loop.
	//
	// It replaces a static document-beat summary that stood here from
	// 2026-08-05. That summary carried the day's COPY faithfully and none of
	// the thing itself, which is exactly what this road is for: the film
	// makes the argument, and the day is the proof you scroll through.
	//
	// `still` is accepted but not forwarded, and that is deliberate.
	// DayTrack reads `motionEnabled` itself and collapses to its own stacked
	// document under reduced motion — the same eleven stations rendered as
	// readable prose — so handing it a second, independent stillness flag
	// would give one question two sources of truth. The prop stays because
	// PathFork calls all three roads with one signature.
	import DayTrack from './DayTrack.svelte';

	export let still = false;
	$: void still;
</script>

<div class="pd-road">
	<DayTrack />
</div>

<style>
	/* The track sizes itself in svh and pins its own stage, so the road is
	   nothing but a containing block — there is deliberately no height,
	   overflow or transform here that could fight it. */
	.pd-road {
		position: relative;
	}
</style>
