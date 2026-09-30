# Desktop custom-surface origin policy (plan 1.6)

The desktop app hosts the same unified-ui bundle in its system WebView,
so the web scripted-surface instantiation carries over with two added
constraints, both enforced as constants in
`desktop/src-tauri/src/app_surface_origin_policy.rs` (mirrored in the web
host's `appScriptedSurface.ts`):

1. **The surface frame never loads from the host's own origins.** The
   Tauri origin (`tauri://localhost`, `https?://tauri.localhost`) and the
   dev-server origins (`http://localhost:5173`, `http://localhost:3002`,
   plus their `127.0.0.1` forms) are host-page sources, never asset
   sources; an absolute URL is refused against those origins by name
   (case-insensitively) before the relative-path requirement applies, and
   every other non-relative source is refused by that requirement. Only
   kernel-issued, digest-keyed relative API paths are admitted frame
   sources.
2. **The sandbox attribute is never relaxed.** The single admitted value
   is the kernel constant `allow-scripts`; `allow-same-origin` and every
   navigation/download/forms/modals token is refused. A cross-origin
   sandboxed frame cannot reach Tauri IPC — and stays that way.

Both rules are pure functions with in-tree tests
(`surface_frame_source_is_admitted`, `surface_sandbox_is_admitted`);
`admits_webview_navigation` is their wired form. The desktop shell
embeds the unified-ui bundle rather than building a second WebView host,
and the one desktop-managed webview that renders unified-ui routes at
the shared UI origin is the bounded Attention window (`tray.rs`
`open_attention_window_at_url`). Its `on_navigation` handler consults
`admits_webview_navigation` for every navigation action — WKWebView
reports subframe loads through the same policy callback, so a scripted
surface **frame** is seen, not only top-level navigations, and later
`navigate` calls pass through the same delegate:

- every non-surface URL is admitted unchanged (the guard is inert unless
  a scripted custom-surface asset route is requested — there is no
  feature gate to configure, and none is needed);
- a URL addressing the digest-keyed `custom-surface-v1/assets/` route is
  admitted only when `surface_frame_source_is_admitted` admits the
  source — and since the callback receives absolute URLs while the only
  admitted sources are kernel-issued relative API paths, every absolute
  candidate is refused: the Tauri origin and the dev-server origins by
  name, any other origin by the relative-path requirement. A scripted
  surface frame therefore never loads inside a desktop-managed webview;
- the same admission requires `surface_sandbox_is_admitted` to still
  hold for the kernel sandbox constant. A navigation action carries no
  sandbox attribute to inspect, so this is the load-bearing desktop form
  of the never-widen rule: fail-closed drift protection — if the kernel
  constant is ever widened, the guard denies every custom-surface
  navigation instead of admitting a relaxed posture.

General app-surface routes (including every `/apps/...` surface page)
open in the system browser on desktop, where the web host enforces the
same kernel constants (`appScriptedSurface.ts`). Any future native
surface window must satisfy these rules through the same function.

## Additional invariants

- Subframe visibility is platform-scoped: macOS/Linux WebViews report subframe
  loads to `on_navigation`; Windows WebView2 sees top-level navigations only and
  relies on the web host's sandbox/CSP.
- The Settings and Logs windows carry the same navigation guard (inert unless a
  scripted asset route is requested — their SPA can navigate to `/apps`). Tauri
  supplies a typed `Url`; they pass `as_str()` to the policy.
- The kernel mints `entry_url` with the live session reference as the first
  asset-path segment (the asset route's credential — a served frame's opaque
  origin carries no headers, and relative subresources keep the containing
  path). That form adds no percent-encoding, escapes, query or origin, so the
  policy admits it; an in-tree test pins it.
- The relative-path requirement refuses any source starting with `//`: a
  protocol-relative source resolves against the host page's scheme to a
  cross-origin absolute URL. Pinned by
  `the_surface_frame_never_loads_from_the_host_or_dev_origins` and
  `webview_navigation_denies_every_absolute_surface_asset_source`.
- The TS mirror `surfaceFrameSourceIsAdmitted` (`appScriptedSurface.ts`) carries
  the same rules and tests. There is no automated parity guard between the two
  policies — change them together.
