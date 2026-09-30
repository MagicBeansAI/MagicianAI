<script lang="ts">
	/**
	 * A routed task or a landed delivery, drawn as a small paper token.
	 * Travels with the same CSS transform transition as a walker — no frame loop.
	 */
	export let kind: 'task' | 'parcel' = 'task';
	export let title = '';
</script>

<g class="office-token" class:is-parcel={kind === 'parcel'}>
	<ellipse cx="0" cy="10" rx="11" ry="3.2" fill="#000" opacity="0.16" />
	{#if kind === 'parcel'}
		<rect x="-9" y="-8" width="18" height="14" rx="1.6" fill="var(--office-wood, #c4a574)" />
		<rect x="-9" y="-2" width="18" height="3" fill="var(--office-ink, #2a2230)" opacity="0.18" />
		<rect x="-1.2" y="-8" width="2.4" height="14" fill="var(--office-ink, #2a2230)" opacity="0.18" />
	{:else}
		<rect x="-11" y="-8" width="22" height="16" rx="2" fill="var(--office-paper, #f4efe3)" />
		<rect x="-11" y="-8" width="22" height="4" fill="var(--office-ink, #2a2230)" opacity="0.12" />
		<path d="M-11,-4 L0,4 L11,-4" fill="none" stroke="var(--office-ink, #2a2230)" stroke-width="1.1" opacity="0.35" />
		<rect x="-7" y="2" width="10" height="1.4" fill="var(--office-ink, #2a2230)" opacity="0.22" />
	{/if}
	<title>{title}</title>
</g>
