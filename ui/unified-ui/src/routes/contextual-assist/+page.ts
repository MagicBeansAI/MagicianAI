// Contextual Assist is the WebView content for the host-native Tauri overlay.
// It depends on browser globals and optional Tauri invoke calls, so it should
// not SSR.
export const ssr = false;
export const prerender = false;
