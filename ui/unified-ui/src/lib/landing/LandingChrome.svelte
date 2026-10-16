<script lang="ts">
	import { createEventDispatcher, onMount } from 'svelte';
	import { pushState } from '$app/navigation';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { settleIn, settleOut } from '$lib/shared/motion';
	import { EXPLAIN_URL } from './explain';
	import GitHubLink from './GitHubLink.svelte';
	export let actionLabel = 'Request setup';
	export let actionHref = '#landing-cta';
	export let dark = false;
	const dispatch = createEventDispatcher<{ appearance: void }>();

	let header: HTMLElement;
	let heroPassed = false;

	onMount(() => {
		const hero = document.getElementById('landing-hero');
		if (!hero) {
			heroPassed = true;
			return;
		}
		let observer: IntersectionObserver;
		function observeHero(): void {
			const headerBottom = header.getBoundingClientRect().bottom;
			heroPassed = hero!.getBoundingClientRect().bottom <= headerBottom;
			observer?.disconnect();
			observer = new IntersectionObserver(([entry]) => {
				heroPassed = entry.boundingClientRect.bottom <= headerBottom;
			}, { rootMargin: `-${headerBottom}px 0px 0px 0px` });
			observer.observe(hero!);
		}
		observeHero();
		const resize = new ResizeObserver(observeHero);
		resize.observe(header);
		return () => {
			observer.disconnect();
			resize.disconnect();
		};
	});

	function jumpToTop(event: MouseEvent): void {
		if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
		const hero = document.getElementById('landing-hero');
		if (!hero) return;
		event.preventDefault();
		if (location.hash !== '#landing-hero') pushState('#landing-hero', {});
		// Skip the document's smooth scrolling and the intervening scroll tracks.
		window.scrollTo({ top: 0, behavior: 'instant' });
		hero.focus({ preventScroll: true });
	}
</script>

<header class="lc" bind:this={header}>
	<nav class="lc-inner" aria-label="Main navigation">
		<a class="lc-brand" href="#landing-hero" aria-label="Magican home" on:click={jumpToTop}>
			<img src={dark ? '/brand/star-dark.png' : '/brand/star-light.png'} width="36" height="36" alt="" />
			<span>magican</span>
		</a>
		<div class="lc-links">
			<a class="lc-explain" href={EXPLAIN_URL} aria-label="How it works" title="How it works">
				<span class="lc-explain-icon"><Icon name="info" size={20} /></span>
				<span class="lc-explain-label">How it works</span>
			</a>
			<a class="lc-manifesto" href="/manifesto">Manifesto</a>
			<GitHubLink compact />
			<button
				type="button"
				class="lc-appearance"
				on:click={() => dispatch('appearance')}
				aria-label={dark ? 'Switch landing to day' : 'Switch landing to night'}
				aria-pressed={dark}
				title={dark ? 'Switch to Day' : 'Switch to Night'}
			>
				<Icon name={dark ? 'sun' : 'moon'} size={18} />
			</button>
			{#if heroPassed}
				<a
					class="lc-action"
					href={actionHref}
					in:settleIn={{ y: -6, duration: 280 }}
					out:settleOut={{ x: 0, y: -4, duration: 180 }}
				>{actionLabel} <span aria-hidden="true">↗</span></a>
			{/if}
		</div>
	</nav>
</header>

<style>
	.lc {
		position: sticky;
		top: 0;
		z-index: 10;
		background: color-mix(in srgb, var(--landing-bg) 94%, transparent);
		backdrop-filter: blur(12px);
		font-family: var(--lp-font);
		color: var(--text-primary);
		border-bottom: 1px solid var(--border-soft);
	}

	.lc-inner {
		max-width: 74rem;
		margin: 0 auto;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 1rem;
		min-height: calc(var(--landing-nav-height) - 1px);
		padding: .65rem clamp(1.15rem, 4vw, 3rem);
	}

	.lc-brand {
		display: inline-flex;
		align-items: center;
		gap: .5rem;
		flex-shrink: 0;
		text-decoration: none;
		color: var(--text-primary);
		font-size: 1.5rem;
		font-weight: 650;
		letter-spacing: -.04em;
	}

	.lc-brand img {
		object-fit: contain;
	}

	.lc-links {
		display: flex;
		align-items: center;
		flex-wrap: nowrap;
		gap: clamp(.65rem, 2vw, 1.5rem);
	}

	.lc-links a {
		display: inline-flex;
		align-items: center;
		min-height: 2.75rem;
		font-size: .85rem;
		text-decoration: none;
		color: var(--text-secondary);
	}

	.lc-explain:hover,
	.lc-manifesto:hover {
		color: var(--text-primary);
	}

	.lc-explain-icon {
		display: none;
	}

	.lc-manifesto {
		text-decoration: none;
	}

	.lc-appearance {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex-shrink: 0;
		width: 2.75rem;
		height: 2.75rem;
		padding: 0;
		border: 0;
		border-radius: 50%;
		background: transparent;
		color: var(--text-faint);
		cursor: pointer;
		transition: background-color 160ms ease, color 160ms ease;
	}

	.lc-appearance:hover {
		background: var(--bg-soft);
		color: var(--text-primary);
	}

	.lc-links .lc-action {
		white-space: nowrap;
		flex-shrink: 0;
		padding: .65rem 1rem;
		gap: .65rem;
		border-radius: 999px;
		background: var(--button-primary-bg);
		color: var(--text-on-accent);
		font-weight: 550;
	}

	@media (max-width: 700px) {
		.lc-links {
			gap: .6rem;
		}
		.lc-brand {
			font-size: 1.25rem;
			gap: .25rem;
		}
		.lc-brand img {
			width: 30px;
			height: 30px;
		}
	}

	@media (max-width: 540px) {
		.lc-links .lc-explain {
			justify-content: center;
			width: 2.75rem;
			flex-shrink: 0;
		}
		.lc-explain-icon { display: inline-flex; }
		.lc-explain-label { display: none; }
		.lc:has(.lc-action) .lc-brand span,
		.lc-action span[aria-hidden='true'] {
			display: none;
		}
		.lc-manifesto {
			display: none !important;
		}
		.lc-links .lc-action {
			padding-inline: .8rem;
			font-size: .76rem;
		}
		.lc-inner {
			gap: .5rem;
		}
	}

	@media (max-width: 360px) {
		.lc-inner { padding-inline: .75rem; }
		.lc-links .lc-action { padding-inline: .6rem; }
		.lc-action span[aria-hidden='true'] {
			display: none;
		}
		.lc-brand img {
			width: 23px;
			height: 23px;
		}
		.lc-brand {
			font-size: 1.1rem;
		}
		.lc-links {
			gap: .15rem;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.lc-appearance {
			transition: none;
		}
	}
</style>
