<script lang="ts">
	// Skeleton placeholder — a sweeping shimmer block used while content
	// loads. Replaces spinners in lists / cards / detail panes so the
	// page composes in place rather than going blank then snapping in.
	//
	// Usage:
	//   <Skeleton width="60%" height="0.95rem" />
	//   <Skeleton variant="card" />          // pre-tuned card placeholder
	//   <Skeleton variant="line" lines={3} /> // 3 stacked text lines
	//   <Skeleton variant="circle" size="32px" />

	export let width: string = '100%';
	export let height: string = '0.95rem';
	export let radius: string | undefined = undefined;
	export let variant: 'line' | 'card' | 'circle' | 'pill' = 'line';
	export let lines: number = 1;
	export let size: string = '32px';
	export let className: string = '';

	$: resolvedRadius =
		radius ??
		(variant === 'circle'
			? '9999px'
			: variant === 'pill'
				? '9999px'
				: 'var(--radius-sm, 6px)');
</script>

{#if variant === 'card'}
	<div class={`skeleton-card ${className}`}>
		<div class="skel skel-line" style="width: 65%; height: 0.95rem;"></div>
		<div class="skel skel-line" style="width: 100%; height: 0.78rem; margin-top: 0.5rem;"></div>
		<div class="skel skel-line" style="width: 90%; height: 0.78rem; margin-top: 0.3rem;"></div>
		<div class="skel skel-line" style="width: 40%; height: 0.78rem; margin-top: 0.3rem;"></div>
	</div>
{:else if variant === 'circle'}
	<div
		class={`skel ${className}`}
		style:width={size}
		style:height={size}
		style:border-radius={resolvedRadius}
	></div>
{:else if lines > 1}
	<div class={`skeleton-stack ${className}`}>
		{#each Array(lines) as _, i}
			<div
				class="skel skel-line"
				style:width={i === lines - 1 ? '60%' : width}
				style:height={height}
				style:border-radius={resolvedRadius}
			></div>
		{/each}
	</div>
{:else}
	<div
		class={`skel ${className}`}
		style:width={width}
		style:height={height}
		style:border-radius={resolvedRadius}
	></div>
{/if}

<style>
	.skel {
		display: block;
		background:
			linear-gradient(
				90deg,
				var(--bg-soft, rgba(0, 0, 0, 0.06)) 0%,
				var(--bg-warm, rgba(0, 0, 0, 0.10)) 50%,
				var(--bg-soft, rgba(0, 0, 0, 0.06)) 100%
			);
		background-size: 200% 100%;
		animation: skeleton-shimmer 1400ms linear infinite;
	}

	.skel-line {
		flex-shrink: 0;
	}

	.skeleton-stack {
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
	}

	.skeleton-card {
		padding: 0.95rem 1rem;
		border-radius: var(--radius-md, 12px);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		background: var(--bg-card, transparent);
	}

	@keyframes skeleton-shimmer {
		from { background-position: 200% 0; }
		to { background-position: -200% 0; }
	}

	/* Reduced motion — render as a flat panel without the shimmer. */
	@media (prefers-reduced-motion: reduce) {
		.skel {
			animation: none;
			background: var(--bg-soft, rgba(0, 0, 0, 0.08));
		}
	}
</style>
