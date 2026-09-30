<script lang="ts">
	export let size: 'sm' | 'md' | 'lg' = 'md';
	export let label = '';
	export let centered = false;

	$: safeSize = size === 'sm' || size === 'lg' ? size : 'md';
</script>

<span class={['native-spinner-wrap', centered ? 'native-spinner-wrap--centered' : ''].filter(Boolean).join(' ')}>
	<span class="native-spinner native-spinner--{safeSize}" aria-hidden="true"></span>
	{#if label}
		<span class="native-spinner__label">{label}</span>
	{/if}
</span>

<style>
	.native-spinner-wrap {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
		color: var(--text-secondary);
		font-family: var(--font-primary);
		font-size: 0.8125rem;
	}

	.native-spinner-wrap--centered {
		width: 100%;
		justify-content: center;
	}

	.native-spinner {
		display: inline-block;
		border-radius: 999px;
		border: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 22%, transparent);
		border-top-color: var(--accent-primary, currentColor);
		animation: native-spinner-rotate 800ms linear infinite;
	}

	.native-spinner--sm {
		width: 0.875rem;
		height: 0.875rem;
	}

	.native-spinner--md {
		width: 1.125rem;
		height: 1.125rem;
	}

	.native-spinner--lg {
		width: 1.5rem;
		height: 1.5rem;
	}

	@keyframes native-spinner-rotate {
		to {
			transform: rotate(360deg);
		}
	}
</style>
