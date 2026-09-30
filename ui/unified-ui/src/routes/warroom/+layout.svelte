<script lang="ts">
	/**
	 * War-room layout — full-bleed, no chrome. Lives outside the `(app)`
	 * group so it inherits zero topbar / sidebar surface from the regular
	 * UI shell.
	 *
	 * Theme: war-room intentionally **does not force a theme**. The page
	 * styling reads from theme CSS variables (`--bg-base`, `--text-primary`,
	 * `--accent-primary`, …) so whichever theme the operator has on stays
	 * applied — Jarvis dark, longhand, mixtape, anything. Earlier versions
	 * pinned to `jarvis` on mount; we restored that decision to the
	 * operator.
	 *
	 * SCROLL LOCK — idempotent by construction (fixed 2026-07-27).
	 *
	 * This used to save the previous inline `overflow` on mount and restore it
	 * on destroy. That is not safe against a double mount: if the layout
	 * mounted while `overflow` was ALREADY `hidden` — a second deck instance,
	 * or an HMR remount whose `onMount` runs before the outgoing instance's
	 * `onDestroy` — it captured `'hidden'` as the "previous" value and then
	 * faithfully restored `'hidden'` on the way out. Document scroll stayed
	 * locked for every page visited afterwards, and that reads to a user as
	 * content having disappeared: the landing page's footer, for one, sits
	 * below the fold and simply becomes unreachable.
	 *
	 * Adding and removing a class carries no captured state, so the failure
	 * cannot recur however many times this mounts, or in whatever order.
	 */
	import { onDestroy, onMount } from 'svelte';
	import { browser } from '$app/environment';
	import { installScopedApiFetch } from '$lib/stores/scopeIdentityStore';

	// AUTHENTICATED FETCH — this route is OUTSIDE the `(app)` group, and that group's
	// layout is the only other place `installScopedApiFetch` runs. Without it
	// `window.fetch` is unpatched here, so every store that relies on
	// the auto-injected workspace-bound bearer fails silently on this
	// surface: the Today pulse, task list, attention queue and chat session all
	// come back empty, which the deck then reports honestly as dashes, an
	// offline channel and (via the health probe) a FAULT. The deck is not
	// broken in that state — it is correctly reporting that it can see nothing.
	if (browser) {
		installScopedApiFetch();
	}

	const LOCK_CLASS = 'warroom-scroll-lock';

	onMount(() => {
		if (!browser) return;
		document.documentElement.classList.add(LOCK_CLASS);
	});

	onDestroy(() => {
		if (!browser) return;
		document.documentElement.classList.remove(LOCK_CLASS);
	});
</script>

<slot />

<style>
	/* Global: the lock must reach `html`/`body`, which are outside this
	   component's style scope. Because no inline style is ever written,
	   removing the class fully restores the document's own overflow and
	   nothing can be left stranded. */
	:global(html.warroom-scroll-lock),
	:global(html.warroom-scroll-lock body) {
		overflow: hidden;
	}
</style>
