// /screen-region-picker is a transparent Tauri-only capture surface.
// It depends on pointer events and Tauri invoke, so it must render purely
// client-side.
export const ssr = false;
export const prerender = false;
