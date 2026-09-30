<script lang="ts">
	export let text: string;
	export let title = '';
	export let height = 92;
	export let showGuides = true;
	export let showComparison = false;
	export let muted = false;
</script>

<article
	class:sample-muted={muted}
	class="cursive-sample"
	style={`--sample-height: ${height}px;`}
>
	<div class="sample-head">
		<div>
			<p class="sample-title">{title || text}</p>
		</div>
		<code>{text}</code>
	</div>

	<div class="sample-sheet" aria-label={`Font cursive preview for ${text}`}>
		{#if showGuides}
			<div class="guide guide-top" aria-hidden="true"></div>
			<div class="guide guide-mid" aria-hidden="true"></div>
			<div class="guide guide-base" aria-hidden="true"></div>
			<div class="guide guide-desc" aria-hidden="true"></div>
		{/if}
		<div class="sample-text">{text}</div>
	</div>

	{#if showComparison}
		<div class="font-comparison" aria-label={`System font comparison for ${text}`}>
			<span>fallback</span>
			<strong>{text}</strong>
		</div>
	{/if}
</article>

<style>
	@font-face {
		font-family: 'Tutor Playwrite US Trad';
		src: url('/fonts/tutor/PlaywriteUSTrad.ttf') format('truetype');
		font-weight: 100 400;
		font-style: normal;
		font-display: swap;
	}

	.cursive-sample {
		display: flex;
		min-width: 0;
		flex-direction: column;
		gap: 8px;
		border: 1px solid var(--border-soft, rgba(15, 23, 42, 0.12));
		border-radius: 8px;
		background: var(--surface-elevated, rgba(255, 255, 255, 0.86));
		padding: 10px;
		box-shadow: 0 10px 30px rgba(15, 23, 42, 0.06);
	}

	.sample-muted {
		background: var(--surface, rgba(248, 250, 252, 0.82));
		box-shadow: none;
	}

	.sample-head,
	.font-comparison {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 10px;
		min-width: 0;
		font-family: var(--font-primary, system-ui, sans-serif);
	}

	.sample-title {
		margin: 0;
		color: var(--text-primary, #111827);
		font-size: 12px;
		font-weight: 750;
		line-height: 1.2;
	}

	code {
		max-width: 48%;
		overflow: hidden;
		color: var(--text-muted, #64748b);
		font-family: var(--font-mono, ui-monospace, monospace);
		font-size: 11px;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.sample-sheet {
		position: relative;
		display: flex;
		min-height: var(--sample-height);
		align-items: center;
		overflow: visible;
		border-radius: 6px;
		background:
			linear-gradient(180deg, rgba(14, 165, 233, 0.045), transparent 48%),
			rgba(255, 255, 255, 0.62);
		padding: 8px 10px;
	}

	.guide {
		position: absolute;
		right: 10px;
		left: 10px;
		height: 1px;
		background: rgba(100, 116, 139, 0.24);
	}

	.guide-top {
		top: 24%;
	}

	.guide-mid {
		top: 43%;
		border-top: 1px dashed rgba(100, 116, 139, 0.2);
		background: transparent;
	}

	.guide-base {
		top: 63%;
		background: rgba(15, 23, 42, 0.34);
	}

	.guide-desc {
		top: 77%;
		border-top: 1px dashed rgba(100, 116, 139, 0.2);
		background: transparent;
	}

	.sample-text,
	.font-comparison strong {
		min-width: 0;
		overflow: visible;
		color: #0f172a;
		font-family: 'Tutor Playwrite US Trad', 'Brush Script MT', 'Brush Script', cursive;
		font-size: clamp(34px, 5vw, 58px);
		font-weight: 400;
		letter-spacing: 0;
		line-height: 1.08;
		white-space: normal;
		overflow-wrap: anywhere;
	}

	.font-comparison {
		align-items: baseline;
		border-top: 1px solid rgba(100, 116, 139, 0.16);
		padding-top: 7px;
		color: var(--text-muted, #64748b);
		font-size: 11px;
	}

	.font-comparison strong {
		max-width: 68%;
		font-size: 24px;
		text-align: right;
	}
</style>
