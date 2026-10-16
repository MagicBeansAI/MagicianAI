# Desktop custom-surface origin policy

The desktop app hosts the same unified-ui bundle in its system WebView, so the
web scripted-surface instantiation carries over with two added constraints,
both constants in `desktop/src-tauri/src/app_surface_origin_policy.rs`
(mirrored in the web host's `appScriptedSurface.ts`):

1. **The surface frame never loads from the host's own origins.** The Tauri
   origin (`tauri://localhost`, `https?://tauri.localhost`) and the dev-server
   origins (`http://localhost:5173`, `http://localhost:3002`, plus their
   `127.0.0.1` forms) are refused by name (case-insensitively); every other
   non-relative source is refused by the relative-path requirement. Only
   kernel-issued, digest-keyed relative API paths are admitted frame sources.
2. **The sandbox attribute is never relaxed.** The only admitted value is the
   kernel constant `allow-scripts`; `allow-same-origin` and every
   navigation/download/forms/modals token is refused. A cross-origin sandboxed
   frame cannot reach Tauri IPC.

Both rules are pure functions with in-tree tests
(`surface_frame_source_is_admitted`, `surface_sandbox_is_admitted`);
`admits_webview_navigation` is their wired form. The desktop shell embeds the
unified-ui bundle instead of building a second WebView host. Every
desktop-managed webview that renders unified-ui routes carries the guard: the
Attention window (`tray.rs` `open_attention_window_at_url`), the notch
Attention panel (`orb_window.rs`), and the Settings and Logs windows
(`tray.rs`). Each `on_navigation` handler consults `admits_webview_navigation`
for every navigation; WKWebView reports subframe loads through the same
callback, so a surface **frame** is seen too, and later `navigate` calls pass
the same delegate:

- every non-surface URL is admitted unchanged (the guard is inert unless a
  scripted custom-surface asset route is requested; there is no feature gate);
- a URL addressing the digest-keyed `custom-surface-v1/assets/` route is
  admitted only when `surface_frame_source_is_admitted` admits it. The
  callback receives absolute URLs, so every candidate is refused, and a
  scripted surface frame never loads inside a desktop-managed webview;
- the same admission requires `surface_sandbox_is_admitted` to hold for the
  kernel sandbox constant. A navigation carries no sandbox attribute, so this
  is the desktop form of the never-widen rule: if the constant is ever
  widened, the guard denies every custom-surface navigation.

General app-surface routes (including every `/apps/...` page) open in the
system browser on desktop, where the web host enforces the same constants
(`appScriptedSurface.ts`). Any future native surface window must satisfy these
rules through the same function.

## Additional invariants

- Subframe visibility is platform-scoped: macOS/Linux WebViews report
  subframe loads to `on_navigation`; Windows WebView2 sees top-level
  navigations only and relies on the web host's sandbox/CSP.
- Settings and Logs need the guard because their SPA can navigate to
  `/apps`. Tauri supplies a typed `Url`; the handlers pass `as_str()`.
- The kernel mints `entry_url` with the live session reference as the first
  asset-path segment (a served frame's opaque origin carries no headers).
  That form adds no percent-encoding, escapes, query or origin, so the policy
  admits it; an in-tree test pins it.
- The relative-path requirement refuses any source starting with `//`, which
  would resolve to a cross-origin absolute URL. Pinned by
  `the_surface_frame_never_loads_from_the_host_or_dev_origins` and
  `webview_navigation_denies_every_absolute_surface_asset_source`.
- The TS mirror `surfaceFrameSourceIsAdmitted` (`appScriptedSurface.ts`)
  carries the same rules and tests. No automated guard keeps the two in
  parity; change them together.
