/** The only viewer path a computer-mode browser sign-in may hand out. */
export const BROWSER_SIGNIN_VIEWER_PATH = '/api/magician/v2/auth/browser-signin/view';
const TICKET = /^bsi_[a-f0-9]{64}$/;

/**
 * Viewer URL for a computer-mode sign-in result (`{mode: "computer", ticket, viewer_path}`),
 * with the ticket in the fragment so it never reaches a server log. Throws on anything else.
 */
export function browserSignInViewerUrl(data: { viewer_path?: unknown; ticket?: unknown }, base: string | URL): URL {
	if (data.viewer_path !== BROWSER_SIGNIN_VIEWER_PATH || typeof data.ticket !== 'string' || !TICKET.test(data.ticket)) {
		throw new Error('Invalid sign-in response.');
	}
	const url = new URL(data.viewer_path, base);
	url.hash = data.ticket;
	return url;
}

export function isBrowserSignInTicket(value: unknown): value is string {
	return typeof value === 'string' && TICKET.test(value);
}
