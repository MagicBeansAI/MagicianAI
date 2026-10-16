import { redirect } from '@sveltejs/kit';
import { browser } from '$app/environment';
import { isOffDeviceSurface } from '$lib/shell/mobileAccess';

const LOCAL_HOSTNAMES = new Set(['localhost', '127.0.0.1', 'tauri.localhost']);

// The session gate lives in the root layout (`src/routes/+layout.ts`) so it
// covers every non-public route, not only this group. This layer keeps the
// app shell's own rule: it is served on local hosts (and the Tauri origin)
// only, and never when the runtime is missing.
//
// Authenticated Work, Attention and the explicit Work specialist allowlist can
// open on a paired phone. Other app pages retain their local-host boundary.
// Authentication still runs in the root layout before these pages render.
export const load = async ({ url }: { url: URL }) => {
	if (browser) {
		const runtimeMissing = Boolean((window as any).__MAGICIAN_MISSING__);
		const remoteHost = !LOCAL_HOSTNAMES.has(window.location.hostname);
		if (runtimeMissing || (remoteHost && !isOffDeviceSurface(url.pathname))) {
			throw redirect(302, '/');
		}
	}
};
