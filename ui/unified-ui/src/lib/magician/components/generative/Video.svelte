<script lang="ts">
	/**
	 * Video Component — GD-F02-B
	 *
	 * Native HTML5 video player with responsive container.
	 */

	export let src: unknown = '';
	export let poster: unknown = undefined;
	export let controls: unknown = true;
	export let autoplay: unknown = false;
	export let loop: unknown = false;
	export let muted: unknown = false;

	function toString(value: unknown): string {
		return typeof value === 'string' ? value : '';
	}

	function toBoolean(value: unknown): boolean {
		return value === true || value === 'true';
	}

	$: safeSrc = toString(src);
	$: safePoster = toString(poster) || undefined;
	$: safeControls = toBoolean(controls);
	$: safeAutoplay = toBoolean(autoplay);
	$: safeLoop = toBoolean(loop);
	$: safeMuted = toBoolean(muted);

	let hasError = false;

	function handleError(): void {
		hasError = true;
	}
</script>

{#if hasError || !safeSrc}
	<div class="muij-video-error" role="img" aria-label="Video unavailable">
		<span class="muij-video-error-icon" aria-hidden="true">🎬</span>
		<span class="muij-video-error-text">Video unavailable</span>
	</div>
{:else}
	<video
		class="muij-video"
		src={safeSrc}
		poster={safePoster}
		controls={safeControls}
		autoplay={safeAutoplay}
		loop={safeLoop}
		muted={safeMuted}
		playsinline
		on:error={handleError}
	>
		Your browser does not support the video element.
	</video>
{/if}

<style>
	.muij-video {
		max-width: 100%;
		height: auto;
		display: block;
		border-radius: var(--radius-sm);
		background: #000;
	}

	.muij-video-error {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: var(--space-xs);
		width: 100%;
		height: 200px;
		background: var(--bg-soft);
		border: 1px dashed var(--border-soft);
		border-radius: var(--radius-sm);
		color: var(--text-muted);
		font-family: var(--font-primary);
	}

	.muij-video-error-icon {
		font-size: 2rem;
	}

	.muij-video-error-text {
		font-size: 0.75rem;
	}
</style>
