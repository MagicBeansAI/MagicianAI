<script lang="ts">
	import { MANIFESTO_DATE, MANIFESTO_EXCERPT, MANIFESTO_TITLE } from './manifesto';
	import ManifestoBlocks from './ManifestoBlocks.svelte';
</script>

<section class="mx" aria-labelledby="manifesto-excerpt-title">
	<p class="mx-date">{MANIFESTO_DATE}</p>
	<h2 id="manifesto-excerpt-title">{MANIFESTO_TITLE}</h2>
	<ManifestoBlocks blocks={MANIFESTO_EXCERPT} />
	<p class="mx-more">
		<a href="/manifesto">Continue reading the manifesto →</a>
	</p>
</section>

<style>
	.mx {
		max-width: 40rem;
		margin: 0 auto;
		padding: clamp(3rem, 8vh, 5.5rem) clamp(1rem, 4vw, 2.5rem);
		font-family: var(--lp-font);
		color: var(--text-primary);
	}

	.mx-date {
		margin: 0 0 0.7rem;
		color: var(--landing-subtitle, var(--text-secondary));
		font-size: 0.78rem;
		letter-spacing: 0.04em;
	}

	.mx h2 {
		margin: 0 0 1.7rem;
		font-size: clamp(2rem, 4.8vw, 3.15rem);
		font-weight: 400;
		letter-spacing: -0.03em;
		line-height: 1.12;
		text-wrap: balance;
	}

	.mx h2::after {
		content: '';
		display: block;
		width: 2.25rem;
		height: 1px;
		margin-top: 1.15rem;
		background: currentColor;
		opacity: 0.28;
	}

	.mx-more {
		margin: 1.85rem 0 0;
	}

	.mx-more a {
		color: var(--lp-primary, var(--accent-primary));
		text-decoration: none;
	}

	.mx-more a:hover {
		text-decoration: underline;
	}
</style>
