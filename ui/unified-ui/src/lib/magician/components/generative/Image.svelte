<script lang="ts">
	/**
	 * Image Component — GD-F02-A
	 *
	 * Responsive image with error handling and lazy loading.
	 */

	export let src: unknown = '';
	export let alt: unknown = '';
	export let fit: unknown = 'contain';
	export let width: unknown = undefined;
	export let height: unknown = undefined;

	function toString(value: unknown): string {
		return typeof value === 'string' ? value : '';
	}

	function toFit(value: unknown): 'contain' | 'cover' | 'fill' | 'none' {
		if (value === 'contain' || value === 'cover' || value === 'fill' || value === 'none') {
			return value;
		}
		return 'contain';
	}

	function toDimension(value: unknown): string | undefined {
		if (typeof value === 'number' && Number.isFinite(value)) {
			return `${value}px`;
		}
		if (typeof value === 'string') {
			return value;
		}
		return undefined;
	}

	$: safeSrc = toString(src);
	$: safeAlt = toString(alt);
	$: safeFit = toFit(fit);
	$: safeWidth = toDimension(width);
	$: safeHeight = toDimension(height);

	let hasError = false;

	function handleError(): void {
		hasError = true;
	}
</script>

{#if hasError || !safeSrc}
	<div
		class="muij-image-placeholder"
		style:width={safeWidth || '100%'}
		style:height={safeHeight || '200px'}
		role="img"
		aria-label={safeAlt || 'Image unavailable'}
	>
		<span class="muij-image-placeholder-icon" aria-hidden="true">🖼️</span>
		<span class="muij-image-placeholder-text">{safeAlt || 'Image unavailable'}</span>
	</div>
{:else}
	<img
		class="muij-image"
		src={safeSrc}
		alt={safeAlt}
		style:object-fit={safeFit}
		style:width={safeWidth}
		style:height={safeHeight}
		loading="lazy"
		on:error={handleError}
	/>
{/if}

<style>
	.muij-image {
		max-width: 100%;
		height: auto;
		display: block;
		border-radius: var(--radius-sm);
	}

	.muij-image-placeholder {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: var(--space-xs);
		background: var(--bg-soft);
		border: 1px dashed var(--border-soft);
		border-radius: var(--radius-sm);
		color: var(--text-muted);
		font-family: var(--font-primary);
	}

	.muij-image-placeholder-icon {
		font-size: 2rem;
	}

	.muij-image-placeholder-text {
		font-size: 0.75rem;
		text-align: center;
		padding: 0 var(--space-sm);
	}
</style>
