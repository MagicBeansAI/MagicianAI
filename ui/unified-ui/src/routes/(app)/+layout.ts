import { redirect } from '@sveltejs/kit';
import { browser } from '$app/environment';
import { isOffDeviceSurface } from '$lib/shell/mobileAccess';

const LOCAL_HOSTNAMES = new Set(['localhost', '127.0.0.1', 'tauri.localhost']);

// The session gate lives in the root layout (`src/routes/+layout.ts`) so it
// covers every non-public route, not only this group. This layer keeps the
// app shell's own rule: it is served on local hosts (and the Tauri origin)
// only, and never when the runtime is missing.
//
// The exception is an off-device surface. The app shell is the operator's own
// console and stays local-only, but a critical-request alert is delivered to a
// chat channel the owner reads on another device, and its link has to resolve
// there. Without this, the attention page loaded from the tunnel hostname for
// one frame and then bounced to `/`. Everything outside
// `isOffDeviceSurface` still refuses any hostname but this machine's.
export const load = async ({ url }: { url: URL }) => {
	if (browser) {
		const runtimeMissing = Boolean((window as any).__MAGICIAN_MISSING__);
		const remoteHost = !LOCAL_HOSTNAMES.has(window.location.hostname);
		if (runtimeMissing || (remoteHost && !isOffDeviceSurface(url.pathname))) {
			throw redirect(302, '/');
		}
	}
};
