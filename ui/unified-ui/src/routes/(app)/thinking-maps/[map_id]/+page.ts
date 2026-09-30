// The map detail view is fully client-driven (getMap() in onMount + a D3 canvas
// that needs the browser), so skip SSR for this route.
export const ssr = false;
