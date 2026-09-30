<script lang="ts">
	import { page } from '$app/stores';
	import { onMount } from 'svelte';

	import {
		isMarketingPath,
		isMobileOpenPath,
		MOBILE_APP_MEDIA_QUERY,
		shouldShowMobileAppGate
	} from './mobileAccess';

	let mobileViewport = false;

	onMount(() => {
		const query = window.matchMedia(MOBILE_APP_MEDIA_QUERY);
		const update = (): void => {
			mobileViewport = query.matches;
		};

		update();
		query.addEventListener('change', update);
		return () => query.removeEventListener('change', update);
	});

	$: blocked = shouldShowMobileAppGate($page.url.pathname, mobileViewport);
</script>

<div
	class="mobile-route-shell"
	class:is-root={isMarketingPath($page.url.pathname)}
	class:is-mobile-open={isMobileOpenPath($page.url.pathname)}
>
	<main class="mobile-app-gate" aria-labelledby="mobile-app-gate-title">
		<p id="mobile-app-gate-title">Install Magican Mobile App for better experience.</p>
		<a href="/">Back to home</a>
	</main>
	{#if !blocked}
		<div class="mobile-route-content">
			<slot />
		</div>
	{/if}
</div>

<style>
	.mobile-route-shell,
	.mobile-route-content {
		display: contents;
	}

	.mobile-app-gate {
		display: none;
		box-sizing: border-box;
		min-height: 100dvh;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 14px;
		padding: 32px 24px;
		background: var(--bg-base, #fff);
		color: var(--text-primary, #171717);
		text-align: center;
	}

	p {
		max-width: 30ch;
		margin: 0;
		font-family: var(--font-display, var(--font-primary, sans-serif));
		font-size: 22px;
		font-weight: 650;
		line-height: 1.35;
		letter-spacing: 0;
	}

	a {
		color: var(--accent-primary, #b9472f);
		font-family: var(--font-primary, sans-serif);
		font-size: 15px;
		font-weight: 600;
		text-underline-offset: 4px;
	}

	a:focus-visible {
		outline: 2px solid var(--accent-primary, #b9472f);
		outline-offset: 4px;
		border-radius: 2px;
	}

	/* Keyed on the route decision, not on the marketing class: the attention
	   page is not marketing — it talks to the backend and takes the app's
	   theme — but a phone is its expected client, because the alert whose link
	   opens it is delivered to the phone. Pathname is known during SSR, so this
	   still holds before hydration and with no JS at all. */
	@media (max-width: 1023px) and (pointer: coarse) {
		.mobile-route-shell:not(.is-mobile-open) .mobile-app-gate {
			display: flex;
		}

		.mobile-route-shell:not(.is-mobile-open) .mobile-route-content {
			display: none;
		}
	}
</style>
