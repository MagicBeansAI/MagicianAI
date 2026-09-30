export const MOBILE_APP_MEDIA_QUERY = '(max-width: 1023px) and (pointer: coarse)';

// Public documents anyone may read without an account: the landing page and
// the long-form pages hanging off it. They pin their own theme and never ask
// the backend for preferences, and they stay readable on a phone.
const MARKETING_PATHS = new Set(['/', '/manifesto', '/privacy', '/terms']);

export function isMarketingPath(pathname: string): boolean {
	return MARKETING_PATHS.has(pathname);
}

function normalizePath(pathname: string): string {
	// A link that arrives in a chat message may carry a trailing slash, and no
	// gate should depend on which.
	const trimmed = pathname.replace(/\/+$/, '');
	return trimmed === '' ? '/' : trimmed;
}

// Surfaces the owner may open from somewhere that is not this machine.
//
// The app shell is otherwise local-only — see `routes/(app)/+layout.ts`, which
// sends any other hostname back to `/` — because it is the operator's own
// console. The attention page is the deliberate exception: the critical-request
// alert that links it is delivered OFF this device, to a chat channel the owner
// reads on their phone, so the link has to resolve from wherever that message
// was read or the whole delivery path ends at a page that refuses to load.
// Everything else stays local-only.
const OFF_DEVICE_SURFACES = new Set(['/attention']);

export function isOffDeviceSurface(pathname: string): boolean {
	return OFF_DEVICE_SURFACES.has(normalizePath(pathname));
}

// Routes a phone is an expected client for, app installed or not: the
// off-device surfaces, plus the login page they bounce through. An
// unauthenticated phone opening the alert link is sent to `/login?redirectTo=…`
// by the root session gate, so gating the login form on mobile makes the
// attention exemption unreachable — the page shows for a frame, the gate
// redirects, and the owner lands back on "install the app".
//
// These are not marketing paths: they talk to the backend and follow the app's
// theme, so they must not take the marketing shell class.
export function isMobileAllowedPath(pathname: string): boolean {
	const path = normalizePath(pathname);
	return isOffDeviceSurface(path) || path === '/login';
}

// The one question both halves of the gate must ask. The gate is enforced
// twice — a CSS media query, so it holds before hydration and without JS, and
// the `{#if}` that keeps gated content out of the DOM — and they have to agree:
// keying the CSS off the marketing class alone is what kept showing "install
// the app" over a route the script had already decided to allow.
export function isMobileOpenPath(pathname: string): boolean {
	return isMarketingPath(pathname) || isMobileAllowedPath(pathname);
}

export function shouldShowMobileAppGate(pathname: string, mobileViewport: boolean): boolean {
	return mobileViewport && !isMobileOpenPath(pathname);
}
