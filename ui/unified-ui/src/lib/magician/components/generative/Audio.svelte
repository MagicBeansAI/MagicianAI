<script lang="ts">
	/**
	 * Audio Component — GD-F02-C
	 *
	 * Native HTML5 audio player.
	 */

	export let src: unknown = '';
	export let controls: unknown = true;
	export let autoplay: unknown = false;
	export let loop: unknown = false;

	function toString(value: unknown): string {
		return typeof value === 'string' ? value : '';
	}

	function toBoolean(value: unknown): boolean {
		return value === true || value === 'true';
	}

	$: safeSrc = toString(src);
	$: safeControls = toBoolean(controls);
	$: safeAutoplay = toBoolean(autoplay);
	$: safeLoop = toBoolean(loop);

	let hasError = false;

	function handleError(): void {
		hasError = true;
	}
</script>

<div class="muij-audio-wrapper">
	{#if hasError || !safeSrc}
		<div class="muij-audio-error" role="img" aria-label="Audio unavailable">
			<span class="muij-audio-error-icon" aria-hidden="true">🎵</span>
			<span class="muij-audio-error-text">Audio unavailable</span>
		</div>
	{:else}
		<audio
			class="muij-audio"
			src={safeSrc}
			controls={safeControls}
			autoplay={safeAutoplay}
			loop={safeLoop}
			on:error={handleError}
		>
			Your browser does not support the audio element.
		</audio>
	{/if}
</div>

<style>
	.muij-audio-wrapper {
		width: 100%;
	}

	.muij-audio {
		width: 100%;
		max-width: 400px;
	}

	.muij-audio-error {
		display: flex;
		align-items: center;
		gap: var(--space-xs);
		padding: var(--space-sm);
		background: var(--bg-soft);
		border: 1px dashed var(--border-soft);
		border-radius: var(--radius-sm);
		color: var(--text-muted);
		font-family: var(--font-primary);
		font-size: 0.75rem;
	}

	.muij-audio-error-icon {
		font-size: 1.25rem;
	}
</style>
