<script lang="ts">
	/** Lounge sofa, seen from above-front. Origin is the centre of the seat. */
	export let width = 148;
</script>

<g class="office-sofa">
	<rect x={-width / 2} y="-24" width={width} height="18" rx="6" fill="var(--office-sofa-dark)" />
	<rect x={-width / 2} y="-14" width={width} height="26" rx="6" fill="var(--office-sofa)" />
	<rect x={-width / 2} y="-18" width="16" height="30" rx="6" fill="var(--office-sofa-dark)" />
	<rect x={width / 2 - 16} y="-18" width="16" height="30" rx="6" fill="var(--office-sofa-dark)" />
	<rect x={-width / 2 + 20} y="-11" width={width / 2 - 24} height="18" rx="4" fill="#fff" opacity="0.13" />
	<rect x="4" y="-11" width={width / 2 - 24} height="18" rx="4" fill="#fff" opacity="0.13" />
</g>
