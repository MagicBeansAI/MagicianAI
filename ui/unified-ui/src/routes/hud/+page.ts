// /hud is a client-only Tauri / browser surface. It depends on
// `installScopedApiFetch`, `document` event listeners, and the
// chat store — none of which make sense to render server-side.
// Disabling SSR avoids any "this throws on the server" failures
// (e.g. browser-only globals reached transitively through imports)
// and skips the SSR work entirely; the page hydrates as a pure CSR
// SPA route, which is what the HUD wants anyway.
export const ssr = false;
export const prerender = false;
