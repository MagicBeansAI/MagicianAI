import { redirect } from '@sveltejs/kit';
import { browser } from '$app/environment';
import { isPublicRoute } from '$lib/shared/publicRoutes';
import { hasHydratedScopeBearer } from '$lib/stores/scopeIdentityStore';

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
	if (!browser || isPublicRoute(url.pathname)) return;
	if (!(await hasHydratedScopeBearer())) {
		throw redirect(307, `/login?redirectTo=${encodeURIComponent(url.pathname + url.search)}`);
	}
};
