<script lang="ts">
	/**
	 * A loose office chair — the ones around the meeting table and the pantry
	 * table. Seated crew carry their own chair inside Person.svelte, because
	 * there the chair has to be drawn behind the figure rather than beside it.
	 *
	 * Origin is the seat centre. `angle` points the backrest away from whatever
	 * the chair is pulled up to.
	 */
	export let angle = 0;
	export let scale = 1;
</script>

<g transform="rotate({angle}) scale({scale})" class="office-chair">
	<ellipse cx="0" cy="0" rx="11" ry="10" fill="var(--office-chair)" />
	<rect x="-10" y="-14" width="20" height="8" rx="3.5" fill="var(--office-chair-dark)" />
	<ellipse cx="0" cy="1" rx="7.5" ry="6.5" fill="var(--office-chair-dark)" opacity="0.45" />
</g>
