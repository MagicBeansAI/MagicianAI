<script lang="ts">
	import site from './publicSite.json';

	export let path: string;
	type Page = { title: string; description: string };
	const pages: Record<string, Page> = site.pages;
	const indexable = import.meta.env.VITE_MARKETING_BUILD === 'true';
	$: entry = pages[path];
	$: canonical = site.origin + path;
	$: structured = JSON.stringify({
		'@context': 'https://schema.org',
		'@graph': [
			{ '@type': 'WebSite', '@id': `${site.origin}/#website`, url: `${site.origin}/`, name: site.name, inLanguage: 'en', sameAs: [site.source] },
			{ '@type': 'WebPage', '@id': `${canonical}#webpage`, url: canonical, name: entry?.title, description: entry?.description, inLanguage: 'en', isPartOf: { '@id': `${site.origin}/#website` } }
		]
	}).replace(/</g, '\\u003c');
</script>

<svelte:head>
	<title>{entry?.title ?? site.name}</title>
	<meta name="robots" content={entry && indexable ? 'index,follow,max-image-preview:large,max-snippet:-1,max-video-preview:-1' : 'noindex,nofollow'} />
	{#if entry}
		<meta name="description" content={entry.description} />
		<link rel="canonical" href={canonical} />
		<meta property="og:type" content="website" />
		<meta property="og:site_name" content={site.name} />
		<meta property="og:locale" content="en_US" />
		<meta property="og:title" content={entry.title} />
		<meta property="og:description" content={entry.description} />
		<meta property="og:url" content={canonical} />
		<meta property="og:image" content={site.origin + site.image.path} />
		<meta property="og:image:secure_url" content={site.origin + site.image.path} />
		<meta property="og:image:type" content="image/png" />
		<meta property="og:image:width" content={String(site.image.width)} />
		<meta property="og:image:height" content={String(site.image.height)} />
		<meta property="og:image:alt" content={site.image.alt} />
		<meta name="twitter:card" content="summary_large_image" />
		<meta name="twitter:title" content={entry.title} />
		<meta name="twitter:description" content={entry.description} />
		<meta name="twitter:image" content={site.origin + site.image.path} />
		<meta name="twitter:image:alt" content={site.image.alt} />
		{@html `<script type="application/ld+json">${structured}</script>`}
	{/if}
</svelte:head>
