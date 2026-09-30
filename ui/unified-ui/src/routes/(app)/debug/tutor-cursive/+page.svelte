<script lang="ts">
	import CursiveFontCandidatePreview from '$lib/magician/tutor/CursiveFontCandidatePreview.svelte';

	let customText = 'wr we r red river write';

	const selectedStyles = [
		{
			label: 'Playwrite USA Traditional',
			fontFamily: "'Tutor Playwrite US Trad', 'Brush Script MT', 'Brush Script', cursive",
			status: 'bundled' as const,
			note: 'Selected primary tutor cursive font for live cursive_text.'
		},
		{
			label: 'macOS Brush Script',
			fontFamily: "'Brush Script MT', 'Brush Script', cursive",
			status: 'installed' as const,
			note: 'Selected secondary fallback when the bundled tutor font is unavailable.'
		}
	];

	const reviewSamples = ['r', 'wr', 'we', 'red', 'river', 'write', 'where', 'work'];
	const previewRevision = 'selected-font-stack';
</script>

<svelte:head>
	<title>Tutor Cursive Preview · Debug</title>
</svelte:head>

<main class="cursive-debug-page">
	<header class="page-head">
		<div>
			<p class="eyebrow">Debug / Tutor</p>
			<h1>Tutor cursive font preview</h1>
			<p class="lede">
				Live tutor <code>cursive_text</code> now uses Playwrite USA Traditional first, with
				macOS Brush Script as the fallback. This page keeps only that selected stack visible.
			</p>
		</div>
		<a class="back-link" href="/debug">Debug</a>
	</header>

	<section class="control-band" aria-label="Cursive preview controls">
		<label class="text-control">
			<span>Custom sample</span>
			<input bind:value={customText} autocomplete="off" spellcheck="false" data-revision={previewRevision} />
		</label>
	</section>

	<section class="preview-section">
		<div class="section-head">
			<h2>Selected tutor stack</h2>
			<p>Primary first; fallback second.</p>
		</div>
		<div class="font-candidate-grid">
			{#each selectedStyles as style}
				<CursiveFontCandidatePreview
					text={customText}
					label={style.label}
					fontFamily={style.fontFamily}
					status={style.status}
					note={style.note}
				/>
			{/each}
		</div>
	</section>

	<section class="preview-section">
		<div class="section-head">
			<h2>Focused review samples</h2>
			<p>Short cases that previously exposed the bad lowercase <code>r</code>.</p>
		</div>
		<div class="sample-matrix">
			{#each reviewSamples as sample}
				<div class="sample-row">
					<div class="sample-label">{sample}</div>
					{#each selectedStyles as style}
						<CursiveFontCandidatePreview
							text={sample}
							label={style.label}
							fontFamily={style.fontFamily}
							status={style.status}
							note=""
						/>
					{/each}
				</div>
			{/each}
		</div>
	</section>
</main>

<style>
	@font-face {
		font-family: 'Tutor Playwrite US Trad';
		src: url('/fonts/tutor/PlaywriteUSTrad.ttf') format('truetype');
		font-weight: 100 400;
		font-style: normal;
		font-display: swap;
	}

	.cursive-debug-page {
		width: min(1320px, calc(100vw - 32px));
		margin: 0 auto;
		padding: 24px 0 44px;
		color: var(--text-primary, #111827);
	}

	.page-head {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 18px;
		margin-bottom: 18px;
	}

	.eyebrow {
		margin: 0 0 5px;
		color: #0f766e;
		font-family: var(--font-primary, system-ui, sans-serif);
		font-size: 11px;
		font-weight: 800;
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}

	h1,
	h2,
	p {
		margin: 0;
	}

	h1 {
		font-family: var(--font-display, var(--font-primary, system-ui, sans-serif));
		font-size: clamp(28px, 4vw, 42px);
		letter-spacing: 0;
		line-height: 1.04;
	}

	h2 {
		font-family: var(--font-display, var(--font-primary, system-ui, sans-serif));
		font-size: 20px;
		letter-spacing: 0;
		line-height: 1.15;
	}

	code {
		font-family: var(--font-mono, ui-monospace, monospace);
	}

	.lede {
		max-width: 760px;
		margin-top: 9px;
		color: var(--text-secondary, #475569);
		font-family: var(--font-primary, system-ui, sans-serif);
		font-size: 14px;
		line-height: 1.5;
	}

	.back-link {
		flex: 0 0 auto;
		border: 1px solid var(--border-soft, rgba(15, 23, 42, 0.14));
		border-radius: 8px;
		background: var(--surface-elevated, #fff);
		color: var(--text-primary, #111827);
		padding: 8px 12px;
		font-family: var(--font-primary, system-ui, sans-serif);
		font-size: 13px;
		font-weight: 750;
		text-decoration: none;
	}

	.control-band {
		display: grid;
		grid-template-columns: minmax(260px, 1fr);
		gap: 12px;
		border: 1px solid var(--border-soft, rgba(15, 23, 42, 0.12));
		border-radius: 8px;
		background: var(--surface-elevated, rgba(255, 255, 255, 0.88));
		padding: 12px;
	}

	.text-control {
		display: flex;
		min-width: 0;
		flex-direction: column;
		gap: 7px;
		color: var(--text-secondary, #475569);
		font-family: var(--font-primary, system-ui, sans-serif);
		font-size: 12px;
		font-weight: 700;
	}

	.text-control input {
		width: 100%;
		box-sizing: border-box;
		border: 1px solid rgba(100, 116, 139, 0.24);
		border-radius: 8px;
		background: rgba(255, 255, 255, 0.9);
		color: var(--text-primary, #111827);
		font: 650 14px var(--font-primary, system-ui, sans-serif);
		padding: 9px 10px;
	}

	.preview-section {
		margin-top: 24px;
	}

	.section-head {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 14px;
		margin-bottom: 10px;
	}

	.section-head p {
		color: var(--text-secondary, #475569);
		font-family: var(--font-primary, system-ui, sans-serif);
		font-size: 13px;
		line-height: 1.4;
		text-align: right;
	}

	.font-candidate-grid,
	.sample-matrix {
		display: grid;
		gap: 12px;
	}

	.font-candidate-grid {
		grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
	}

	.sample-row {
		display: grid;
		grid-template-columns: 70px repeat(2, minmax(220px, 1fr));
		align-items: stretch;
		gap: 10px;
	}

	.sample-label {
		display: flex;
		align-items: center;
		justify-content: center;
		border: 1px solid var(--border-soft, rgba(15, 23, 42, 0.12));
		border-radius: 8px;
		background: rgba(255, 255, 255, 0.74);
		color: var(--text-primary, #111827);
		font: 800 18px var(--font-mono, ui-monospace, monospace);
	}

	@media (max-width: 760px) {
		.cursive-debug-page {
			width: min(100vw - 20px, 720px);
			padding-top: 16px;
		}

		.page-head,
		.section-head {
			align-items: flex-start;
			flex-direction: column;
		}

		.section-head p {
			text-align: left;
		}

		.font-candidate-grid,
		.sample-row {
			grid-template-columns: 1fr;
		}

		.sample-label {
			min-height: 44px;
		}
	}
</style>
