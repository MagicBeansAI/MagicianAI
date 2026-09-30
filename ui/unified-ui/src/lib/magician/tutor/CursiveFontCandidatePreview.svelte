<script lang="ts">
	export let text: string;
	export let fontFamily: string;
	export let label: string;
	export let note = '';
	export let status: 'candidate' | 'bundled' | 'installed' | 'reference' | 'reject' = 'candidate';
</script>

<article class="font-candidate font-candidate-{status}">
	<div class="candidate-meta">
		<div>
			<h3>{label}</h3>
			{#if note}
				<p>{note}</p>
			{/if}
		</div>
		<span>{status}</span>
	</div>
	<div class="candidate-sample" style={`font-family: ${fontFamily};`}>
		{text}
	</div>
	<div class="candidate-baseline" aria-hidden="true"></div>
</article>

<style>
	.font-candidate {
		position: relative;
		display: flex;
		min-width: 0;
		flex-direction: column;
		gap: 10px;
		border: 1px solid var(--border-soft, rgba(15, 23, 42, 0.12));
		border-radius: 8px;
		background: rgba(255, 255, 255, 0.86);
		padding: 12px;
		box-shadow: 0 10px 30px rgba(15, 23, 42, 0.06);
	}

	.font-candidate-reject {
		background: rgba(254, 242, 242, 0.75);
	}

	.font-candidate-reference {
		background: rgba(240, 253, 250, 0.82);
	}

	.font-candidate-bundled {
		background: rgba(239, 246, 255, 0.82);
	}

	.candidate-meta {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 12px;
	}

	h3,
	p {
		margin: 0;
	}

	h3 {
		color: var(--text-primary, #111827);
		font-family: var(--font-primary, system-ui, sans-serif);
		font-size: 13px;
		font-weight: 800;
		line-height: 1.2;
	}

	p {
		margin-top: 3px;
		color: var(--text-secondary, #475569);
		font-family: var(--font-primary, system-ui, sans-serif);
		font-size: 11px;
		line-height: 1.35;
	}

	.candidate-meta span {
		flex: 0 0 auto;
		border-radius: 999px;
		background: rgba(15, 23, 42, 0.08);
		color: var(--text-secondary, #475569);
		padding: 3px 7px;
		font-family: var(--font-mono, ui-monospace, monospace);
		font-size: 10px;
		font-weight: 800;
		text-transform: uppercase;
	}

	.font-candidate-reject .candidate-meta span {
		background: rgba(220, 38, 38, 0.12);
		color: #991b1b;
	}

	.font-candidate-reference .candidate-meta span {
		background: rgba(15, 118, 110, 0.12);
		color: #0f766e;
	}

	.font-candidate-bundled .candidate-meta span {
		background: rgba(37, 99, 235, 0.12);
		color: #1d4ed8;
	}

	.candidate-sample {
		position: relative;
		z-index: 1;
		min-height: 112px;
		overflow: visible;
		color: #0f172a;
		font-size: clamp(34px, 5vw, 58px);
		font-weight: 400;
		letter-spacing: 0;
		line-height: 1.16;
		white-space: normal;
		overflow-wrap: anywhere;
	}

	.candidate-baseline {
		position: absolute;
		right: 12px;
		bottom: 36px;
		left: 12px;
		height: 1px;
		background: rgba(15, 23, 42, 0.18);
	}
</style>
