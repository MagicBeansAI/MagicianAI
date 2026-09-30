<script lang="ts">
	/**
	 * The boardroom table: a long rounded slab ringed with chairs, a laptop and
	 * a speakerphone. Chair count scales with the table's length so the ring
	 * stays evenly spaced whatever width the plan hands over.
	 *
	 * Origin is the table centre.
	 */
	import Chair from './Chair.svelte';

	export let width = 236;
	export let height = 122;

	$: perSide = Math.max(2, Math.round((width - 60) / 62));
	$: sideXs = Array.from(
		{ length: perSide },
		(_, i) => -width / 2 + 34 + (i * (width - 68)) / Math.max(1, perSide - 1)
	);
</script>

<g class="office-meeting-table">
	{#each sideXs as x, i (`top-${i}`)}
		<g transform="translate({x}, {-height / 2 - 12})"><Chair angle={180} /></g>
	{/each}
	{#each sideXs as x, i (`bottom-${i}`)}
		<g transform="translate({x}, {height / 2 + 12})"><Chair /></g>
	{/each}
	<g transform="translate({-width / 2 - 14}, 0)"><Chair angle={90} /></g>
	<g transform="translate({width / 2 + 14}, 0)"><Chair angle={-90} /></g>

	<rect
		x={-width / 2}
		y={-height / 2}
		width={width}
		height={height}
		rx={Math.min(34, height / 2)}
		fill="var(--office-desk)"
	/>
	<rect
		x={-width / 2 + 8}
		y={-height / 2 + 8}
		width={width - 16}
		height={height - 16}
		rx={Math.min(28, height / 2 - 8)}
		fill="#fff"
		opacity="0.12"
	/>
	<!-- speakerphone, a laptop and a stack of paper -->
	<circle cx="0" cy="0" r="9" fill="var(--office-screen)" />
	<circle cx="0" cy="0" r="4" fill="var(--office-screen-glow)" opacity="0.55" />
	<rect x={-width / 2 + 34} y="-12" width="26" height="18" rx="2" fill="var(--office-screen)" />
	<rect x={width / 2 - 62} y="-8" width="22" height="15" rx="1.5" fill="var(--office-paper)" />
</g>
