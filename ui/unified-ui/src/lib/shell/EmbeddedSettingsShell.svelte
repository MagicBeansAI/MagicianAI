<script lang="ts">
	// The shell for Settings embedded in a native app's WebView: the page,
	// the toasts and confirmation dialogs Settings raises, Esc for its
	// overlays, and a live-session check. Nothing else — see
	// `embeddedSettings.ts` for why this is not the `(app)` layout.
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { onMount } from 'svelte';
	import ToastNotifications from '$lib/shared/components/ToastNotifications.svelte';
	import ConfirmationModalHost from '$lib/magician/components/ConfirmationModalHost.svelte';
	import { mountEscapeHandler } from '$lib/shell/overlayCoordinator';
	import { refreshScopeSession } from '$lib/stores/scopeIdentityStore';

	onMount(() => {
		// The load-time gate already sent a phone with no session to /login;
		// this catches a session that is present but no longer live.
		void refreshScopeSession()
			.then((session) => {
				if (!session) {
					void goto(`/login?redirectTo=${encodeURIComponent($page.url.pathname + $page.url.search)}`);
				}
			})
			.catch(() => {});

		// The pane scrolls inside the window at the window's real height. An
		// Android WebView hosted at WRAP_CONTENT height (Magdroid before
		// 0.6.13) reports every viewport unit — vh, dvh, svh — as 0, which
		// collapsed the pane to a blank screen; innerHeight stays correct.
		const root = document.documentElement;
		const sizePane = () => root.style.setProperty('--embedded-viewport-h', `${window.innerHeight}px`);
		sizePane();
		window.addEventListener('resize', sizePane);
		const releaseEscape = mountEscapeHandler();
		return () => {
			window.removeEventListener('resize', sizePane);
			root.style.removeProperty('--embedded-viewport-h');
			releaseEscape();
		};
	});
</script>

<div class="embedded-settings-shell">
	<main class="embedded-settings-pane">
		<slot />
	</main>
</div>

<ToastNotifications />
<ConfirmationModalHost />

<style>
	.embedded-settings-shell {
		background: var(--bg-base, #ffffff);
	}

	/* Settings scrolls inside the pane so its sticky category nav has a
	   scrolling ancestor to stick to. */
	.embedded-settings-pane {
		height: 100dvh;
		height: var(--embedded-viewport-h, 100dvh);
		overflow-y: auto;
	}
</style>
