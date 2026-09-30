<script lang="ts">
	/**
	 * A desk, seen from slightly above: a wood top face, a front edge, legs, a
	 * monitor and a bit of clutter.
	 *
	 * Origin is the slab centre. The monitor is pushed to one SIDE rather than
	 * centred, because a centred monitor covers the face of the person seated
	 * behind it and the face is the thing worth seeing. `monitorSide` comes from
	 * the seat (id-derived), so the sides alternate across a room without
	 * looking alternated.
	 */
	import type { Vibe } from '../../engine/types';
	import { STATUS_COLOUR } from './palette';

	export let monitorSide: -1 | 1 = 1;
	export let vibe: Vibe = 'idle';
	/** Draw the status chip on the desk edge. Off when the name plate below the
	 * desk is showing the same thing at a larger, crisper size. */
	export let showStatusChip = true;
</script>

<g class="office-desk">
	<!-- legs first, so the slab sits on them -->
	<rect x="-32" y="10" width="5" height="6" fill="var(--office-desk-leg)" />
	<rect x="27" y="10" width="5" height="6" fill="var(--office-desk-leg)" />

	<!-- monitor, standing on the far edge of the top face -->
	<rect x={monitorSide * 20 - 2} y="-16" width="4" height="5" fill="var(--office-metal-dark)" />
	<rect
		x={monitorSide * 20 - 13}
		y="-28"
		width="26"
		height="14"
		rx="2"
		fill="var(--office-screen)"
	/>
	<rect
		x={monitorSide * 20 - 11}
		y="-26"
		width="22"
		height="4"
		rx="1"
		fill="var(--office-screen-glow)"
		opacity="0.35"
	/>

	<!-- top face + front edge -->
	<rect x="-36" y="-12" width="72" height="18" rx="3" fill="var(--office-desk)" />
	<rect x="-36" y="-12" width="72" height="4" rx="2" fill="#fff" opacity="0.14" />
	<rect x="-36" y="5" width="72" height="6" rx="1.5" fill="var(--office-desk-edge)" />

	<!-- clutter on the free side: paper and a mug -->
	<rect
		x={-monitorSide * 27 - 6}
		y="-7"
		width="13"
		height="9"
		rx="1"
		fill="var(--office-paper)"
		opacity="0.92"
	/>
	<circle cx={-monitorSide * 13} cy="-2" r="3.4" fill="var(--office-paper)" />
	<circle cx={-monitorSide * 13} cy="-2" r="1.8" fill="var(--office-desk-edge)" opacity="0.6" />

	{#if showStatusChip}
		<rect x="-33" y="6" width="18" height="4" rx="1.4" fill={STATUS_COLOUR[vibe]} />
	{/if}
</g>
