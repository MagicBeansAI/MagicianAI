<script lang="ts">
	/**
	 * The pantry run: worktop, cupboard fronts, a sink and a kettle. Stretches to
	 * the width the plan gives it, so it always spans the wall it is drawn on.
	 * Origin is the counter's centre.
	 */
	export let width = 200;
	$: half = width / 2;
	$: doors = Math.max(2, Math.round(width / 52));
</script>

<g class="office-counter">
	<rect x={-half} y="-6" width={width} height="22" rx="2" fill="var(--office-counter)" />
	<rect x={-half - 2} y="-12" width={width + 4} height="8" rx="2" fill="var(--office-counter-top)" />
	{#each Array.from({ length: doors }, (_, i) => i) as i (i)}
		<rect
			x={-half + 4 + (i * (width - 8)) / doors}
			y="-1"
			width={(width - 8) / doors - 4}
			height="13"
			rx="1.5"
			fill="var(--office-ink-faint)"
		/>
	{/each}
	<!-- sink -->
	<rect x={-half + 14} y="-11" width="34" height="6" rx="2" fill="var(--office-metal)" />
	<rect x={-half + 28} y="-16" width="3" height="6" rx="1.5" fill="var(--office-metal-dark)" />
	<!-- kettle and a couple of mugs -->
	<rect x={half - 46} y="-19" width="14" height="9" rx="2" fill="var(--office-metal-dark)" />
	<circle cx={half - 24} cy="-14" r="3.6" fill="var(--office-paper)" />
	<circle cx={half - 15} cy="-14" r="3.6" fill="var(--office-paper)" />
</g>
