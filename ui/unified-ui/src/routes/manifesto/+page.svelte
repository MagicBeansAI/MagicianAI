<script lang="ts">
	import {
		MANIFESTO_BODY,
		MANIFESTO_DATE,
		MANIFESTO_SIGN,
		MANIFESTO_TAGLINE,
		MANIFESTO_TITLE
	} from '$lib/landing/manifesto';
	import DocShell from '$lib/landing/DocShell.svelte';
	import ManifestoBlocks from '$lib/landing/ManifestoBlocks.svelte';
</script>

<svelte:head>
	<title>Personal Intelligence is your asset — Magican</title>
</svelte:head>

<DocShell title={MANIFESTO_TITLE} date={MANIFESTO_DATE}>
	<ManifestoBlocks blocks={MANIFESTO_BODY} />
	<p class="sign">{MANIFESTO_SIGN}</p>
	<p class="tagline">{MANIFESTO_TAGLINE}</p>
</DocShell>

<style>
	.sign {
		margin: 2.6rem 0 0.35rem;
		font-size: 1.08rem;
		font-weight: 500;
		letter-spacing: -0.015em;
	}

	.tagline {
		margin: 0;
		color: var(--landing-subtitle);
		font-size: 1rem;
		font-style: italic;
		line-height: 1.5;
	}
</style>
