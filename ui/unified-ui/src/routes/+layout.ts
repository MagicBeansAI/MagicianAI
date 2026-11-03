import { redirect } from '@sveltejs/kit';
import { browser } from '$app/environment';
import { isPublicRoute } from '$lib/shared/publicRoutes';
import { hasHydratedScopeBearer } from '$lib/stores/scopeIdentityStore';
import { canonicalWorkHref } from '$lib/work/routes';
import { threadIdForKey } from '$lib/threads/threadLinks';

/**
 * Root auth gate. Every route is a Magician surface unless `isPublicRoute`
 * says otherwise, so a page outside the `(app)` group (the war room, the HUD,
 * the overlays, dev tools) is gated the same way the app shell is; before
 * this, only `(app)` carried the gate and `/warroom` opened signed-out.
 *
 * Load-time rather than mount-time so no unauthenticated shell flashes
 * before `/login` paints. Hydration-aware: a Tauri webview holding a native
 * session is not misread as signed-out. Presence only; a stale token passes
 * here and is caught by the mount-time session refresh or the 401 re-gate.
 * On the server (SSR and prerender) this is a no-op.
 */
export const load = async ({ url }: { url: URL }) => {
	if (!browser) return;
	// The root also loads for unmatched SPA entries. Resolve retired Work
	// bookmarks before the error page, using the same adapter as stored links.
	// A load function may not read `url.hash` (SvelteKit throws in dev, which
	// left every page on "Internal Error"). The hash is only carried over, so
	// map path + query and re-attach the browser's hash on redirect.
	const entered = url.pathname + url.search;
	// `/t/<key>/tasks` names a thread by slug; Work filters by the id.
	const canonical = canonicalWorkHref(entered, threadIdForKey);
	if (canonical !== entered) throw redirect(307, canonical + location.hash);
	if (isPublicRoute(url.pathname)) return;
	// Dev-only visual check for the draw overlay. The desktop webview is already
	// signed in; a headless browser is not. The fixture paints sample marks and
	// does not read a session.
	if (
		import.meta.env.DEV &&
		url.pathname === '/draw-overlay' &&
		(url.searchParams.get('fixture') === 'tutor' || url.searchParams.get('fixture') === 'working')
	) {
		return;
	}
	if (!(await hasHydratedScopeBearer())) {
		throw redirect(307, `/login?redirectTo=${encodeURIComponent(url.pathname + url.search)}`);
	}
};
