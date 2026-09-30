import { isMarketingPath } from '$lib/shell/mobileAccess';

/**
 * The routes a visitor may open without a session. Everything else is a
 * Magician surface and is gated at the root layout, so a new page is
 * protected unless it is deliberately listed here.
 *
 * - the marketing pages (`/`, `/manifesto`, `/privacy`, `/terms`)
 * - `/login` itself
 * - the theme specimen sheet under `/dev`, a static page with no backend calls
 */
const PUBLIC_EXACT = new Set(['/login', '/dev/theme-gallery']);

function normalize(pathname: string): string {
	const trimmed = pathname.replace(/\/+$/, '');
	return trimmed === '' ? '/' : trimmed;
}

export function isPublicRoute(pathname: string): boolean {
	const path = normalize(pathname);
	return isMarketingPath(path) || PUBLIC_EXACT.has(path);
}
