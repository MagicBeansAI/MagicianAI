<script lang="ts">
	import { browser } from '$app/environment';
	import { beforeNavigate, goto } from '$app/navigation';
	import { onMount } from 'svelte';
	import { hudWindow, isHudWindow, connectHudWindow, hudModeHref } from './hudWindow';

	// Only the compact launcher is route-bound. Expanded windows host the real app.
	const nativeHud = browser && isHudWindow();

	function recoveryHref(url: URL | undefined): string | null {
		if (!nativeHud || !$hudWindow.ready || $hudWindow.expanded || !url || url.origin !== window.location.origin) return null;
		const path = url.pathname.replace(/\/+$/, '');
		const threadChat = /^\/t\/([^/]+)(?:\/chat)?$/.exec(path);
		if (!['/chat', '/vibe'].includes(path) && (!threadChat || threadChat[1] === 'new')) return null;
		try { return hudModeHref(url, false); }
		catch { return null; }
	}

	beforeNavigate(({ to, cancel }) => {
		const href = recoveryHref(to?.url);
		if (!href) return;
		cancel();
		void goto(href, { replaceState: true });
	});

	onMount(() => {
		let disposed = false;
		let disconnect: (() => void) | undefined;
		void connectHudWindow().then((stop) => { if (disposed) stop(); else disconnect = stop; })
			.catch((error) => console.warn('[desktop] Could not read window mode', error));
		return () => { disposed = true; disconnect?.(); };
	});
</script>
