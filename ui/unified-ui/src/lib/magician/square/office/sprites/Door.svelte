<script lang="ts">
	/**
	 * A doorway onto the corridor: a threshold slab, a frame tick on each jamb,
	 * an open leaf, and the swing arc that makes a plan read as a plan rather
	 * than as a diagram with gaps in it.
	 *
	 * Origin is the centre of the opening, ON the room's corridor-facing edge.
	 * `band` says which side of the corridor the room is, so the leaf always
	 * swings INTO the room and never across the walking lane.
	 *
	 * The threshold is drawn in the CORRIDOR colour and pushed into the room,
	 * which is what actually sells the opening: the room's own floor is a
	 * lighter value, so the corridor visibly runs through the wall line.
	 */
	export let width = 48;
	/** 'north' rooms sit above the corridor, so their doors swing upward. */
	export let band: 'north' | 'south' = 'north';

	/** into-the-room direction */
	$: dir = band === 'north' ? -1 : 1;
	$: half = width / 2;
	$: leaf = width * 0.86;
</script>

<g class="office-door">
	<!-- threshold: the corridor floor running through the wall line -->
	<rect
		x={-half}
		y={dir === -1 ? -9 : -1}
		width={width}
		height="10"
		fill="var(--office-corridor)"
	/>
	<!-- jambs -->
	<rect x={-half - 3} y="-4" width="3.5" height="8" fill="var(--office-partition-edge)" />
	<rect x={half - 0.5} y="-4" width="3.5" height="8" fill="var(--office-partition-edge)" />
	<!-- swing arc -->
	<path
		d="M{half},0 A {leaf},{leaf} 0 0 {dir === -1 ? 0 : 1} {half - leaf},{dir * leaf}"
		fill="none"
		stroke="var(--office-ink-soft)"
		stroke-width="1.2"
		stroke-dasharray="4 4"
		opacity="0.5"
	/>
	<!-- the leaf, standing open against the jamb -->
	<rect
		x={-half}
		y={dir === -1 ? -leaf : 0}
		width="4.5"
		height={leaf}
		rx="1.5"
		fill="var(--office-desk-edge)"
	/>
</g>
