// Embedded Settings renders in the WebView only. Server-rendering it in dev
// sent ~1 MB of inlined CSS ahead of every script over the phone's tunnel;
// the root layout's session gate is browser-only anyway.
export const ssr = false;
