/**
 * Settings embedded by a native host — the desktop, iOS or Android app opens
 * `/settings?embed=1&host=<host>` in a WebView.
 *
 * That URL is served by its own route ({@link EMBEDDED_SETTINGS_ROUTE}, via
 * the `reroute` hook in `src/hooks.ts`) instead of the `(app)` group, so the
 * page loads the root layout and Settings only. The `(app)` layout imports
 * the whole app shell — top bar, rail, palette, Attention, voice and media
 * monitors, chat — which a phone fetched module by module over the tunnel
 * before Settings could paint (~490 requests, ~8.5 s on 2026-10-25). The URL
 * the apps open does not change.
 */
export const EMBEDDED_SETTINGS_ROUTE = '/embed/settings';

export function isEmbeddedSettingsUrl(url: URL): boolean {
	return url.searchParams.get('embed') === '1' && /^\/settings\/?$/.test(url.pathname);
}

/** The route that serves `url`, when it is embedded Settings; else undefined. */
export function embeddedSettingsReroute(url: URL): string | undefined {
	return isEmbeddedSettingsUrl(url) ? EMBEDDED_SETTINGS_ROUTE : undefined;
}
