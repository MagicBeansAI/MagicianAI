<script lang="ts">
	import { sanitizeCssNonNegativeLength } from './cssUtil';

	export let maxHeight: string = '300px';
	export let scrollbar: 'auto' | 'thin' | 'hidden' = 'auto';
	/** R333: Allow parent to set a descriptive aria-label for the scroll region. */
	export let ariaLabel: string = 'Scrollable content';

	$: safeMaxHeight = sanitizeCssNonNegativeLength(maxHeight, '300px');
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex (keyboard-scrollable overflow region) -->
<div
	class="muij-scroll-area"
	class:muij-scroll-thin={scrollbar === 'thin'}
	class:muij-scroll-hidden={scrollbar === 'hidden'}
	style="max-height: {safeMaxHeight};"
	tabindex="0"
	role="region"
	aria-label={ariaLabel || 'Scrollable content'}
>
	<slot />
</div>

<style>
	.muij-scroll-area {
		overflow-y: auto;
		overflow-wrap: anywhere;
	}

	.muij-scroll-thin {
		scrollbar-width: thin;
		scrollbar-color: var(--border-soft) transparent;
	}

	.muij-scroll-thin::-webkit-scrollbar {
		width: 6px;
	}

	.muij-scroll-thin::-webkit-scrollbar-track {
		background: transparent;
	}

	.muij-scroll-thin::-webkit-scrollbar-thumb {
		background: var(--border-soft);
		border-radius: 3px;
	}

	.muij-scroll-hidden {
		scrollbar-width: none;
	}

	.muij-scroll-hidden::-webkit-scrollbar {
		display: none;
	}
</style>
