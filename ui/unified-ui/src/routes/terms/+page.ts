// Cloudflare Pages serves this marketing route from a physical HTML file.
// The deployed artifact intentionally has a real top-level 404, so relying on
// the SPA fallback would make a direct request to /terms fail.
export const prerender = true;
