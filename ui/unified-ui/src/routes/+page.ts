// Marketing artifacts contain crawlable HTML. Personal-app builds retain their
// SPA fallback and existing backend-aware landing behavior.
export const prerender = import.meta.env.VITE_MARKETING_BUILD === 'true';
