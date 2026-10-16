<script lang="ts">
	/**
	 * Potted greenery. `tall` swaps the small desk plant for a floor plant —
	 * same pot language, more foliage, so a room reads as furnished rather than
	 * decorated. Origin is the pot's base centre.
	 */
	export let tall = false;
</script>

{#if tall}
	<g class="office-plant">
		<circle cx="0" cy="-30" r="13" fill="var(--office-plant)" />
		<circle cx="-9" cy="-21" r="9.5" fill="var(--office-plant-dark)" />
		<circle cx="9" cy="-22" r="9" fill="var(--office-plant-dark)" />
		<circle cx="-3" cy="-39" r="8" fill="var(--office-plant)" />
		<circle cx="6" cy="-34" r="7" fill="var(--office-plant)" />
		<rect x="-2" y="-24" width="4" height="14" fill="var(--office-plant-dark)" />
		<path d="M-11,-10 L11,-10 L8,6 L-8,6 Z" fill="var(--office-pot)" />
		<rect x="-12" y="-12" width="24" height="4" rx="1.5" fill="var(--office-pot-dark)" />
	</g>
{:else}
	<g class="office-plant">
		<circle cx="0" cy="-19" r="9.5" fill="var(--office-plant)" />
		<circle cx="-7" cy="-13" r="7" fill="var(--office-plant-dark)" />
		<circle cx="7" cy="-13" r="6.5" fill="var(--office-plant-dark)" />
		<path d="M-8,-6 L8,-6 L6,6 L-6,6 Z" fill="var(--office-pot)" />
		<rect x="-9" y="-8" width="18" height="3.5" rx="1.2" fill="var(--office-pot-dark)" />
	</g>
{/if}
