# HUD overlay window

The quick-summon HUD (double-tap Left Option, or the configured chord) is a
Tauri window labelled `overlay`, built in
`desktop/src-tauri/src/overlay.rs`. It hosts the `/hud` route from unified-ui.
The window shape and the composer-side layout (docked `ContextPill`, collapsed
command-bar state) move together: the panel assumes this window shape, and the
window width assumes the composer's desktop breakpoint.

## Shape

A small **centred panel**, not a full-monitor overlay.

| property | value | why |
| --- | --- | --- |
| `transparent` | `true` | rounded corners + a real macOS drop shadow; an opaque `decorations(false)` window has hard square corners |
| `decorations` | `false` | no title bar |
| `always_on_top` | `true` | it is a launcher |
| NSWindow level | `3` (`NSFloatingWindowLevel`) | above app windows, **below** the menu bar (24) and dock (20), so system affordances stay clickable |
| panel width | `820px` fixed | must clear the unified-ui phone breakpoint — see below |
| panel height | **follows content** | |
| position | centred on the cursor's monitor, every summon | not remembered |

The window paints nothing itself; the **panel** inside `/hud` carries the theme
(`--bg-base`, a `--border-soft` hairline, rounded corners, a shadow). No
vibrancy: `NSVisualEffectView` blurs the whole screen behind the window.

## A transparent stage around a themed panel

The window is a **stage**: larger than the panel, fully transparent, giving
popups somewhere to render. The themed panel (`.hud`) is centred inside it by
CSS.

```
OVERLAY_HUD_WIDTH                1000.0   also the webview viewport width
OVERLAY_HUD_HEIGHT                760.0
OVERLAY_HUD_MAX_SCREEN_FRACTION     0.9   clamp on a short screen
```

The stage leaves room for the theme dropdown, session more-menu, profile picker
and mention list, each taller than a collapsed composer.

**Why not shrink-wrap the window to the panel:** a window cannot paint outside
itself, so a shrink-wrapped window clips every popup, and growing it on demand
fails because a popup must be visible before it can be measured. Giving the
stage room up front removes the problem. The transparent stage still captures
clicks over its area, which is what makes click-to-dismiss work.

### Width must clear the phone breakpoint

unified-ui's composer collapses to a phone layout at `max-width: 767px`
(`Do|Plan` hidden, 40px touch targets, icon-only profile chip). **The window
width is the webview width**, so a narrow window silently renders the mobile
composer. `/hud` therefore re-asserts the desktop composer layout
unconditionally, overriding that media query; the stage width is defence in
depth, not the mechanism.

## Centring

`center_hud_window` picks the monitor under the cursor via `cursor_monitor`,
which does manual rect containment rather than `monitor_from_point` (the latter
intermittently returns `None` on multi-display setups). The stage is centred on
that monitor and CSS centres the panel in the stage. `fill_screen` remains for
the **draw overlay**, which does cover a monitor.

## Dismiss

- Clicking **outside the window** blurs it; `WindowEvent::Focused(false)` hides
  it.
- Clicking the **transparent stage** hits `handleStageClick`, which dismisses
  unless the click landed inside `.hud` (or an open drawer or modal). This is a
  plain `closest()` ancestor check; popups are descendants of `.hud`, so no
  popup registry is needed.
- ESC is handled in the route's keydown listener, **not** as a global shortcut:
  a global ESC deadlocks on macOS because the dismiss handler runs on a Tauri
  thread and unregisters the shortcut whose callback is still running.

## Mascot anchoring

`glide_mascot_to` takes **screen** coordinates. Viewport coordinates are
window-relative, so the route adds the window's `outerPosition()` before
converting to AppKit's bottom-left origin.

## Lifecycle (first-summon latency)

The window is **lazy at boot**: constructing HUD/contextual-assist WebViews
during startup serializes WebKit/AppKit work onto the main thread and delays
boot essentials (host gateway, shortcuts).

To keep the cold-WebView cost off the first summon, the HUD is **pre-warmed ~5s
after boot** (`OVERLAY_PREWARM_DELAY` in `initialize_overlay`): a hidden WebView
is created at idle. The show path still self-ensures the window, so a summon
during the delay, or after macOS purges the parked WebView under memory
pressure, takes the cold path and stays correct. Cost: one resident parked
WebKit process (~100–150 MB). `general.hud_prewarm = false` in the desktop
config restores fully lazy first use. Only the HUD is pre-warmed; the
contextual-assist, draw and notify overlays stay lazy.

## The summon gesture needs Accessibility

Double-tapping Left ⌥ is a `CGEventTapOptions::Default` CGEventTap — an *active*
tap — so macOS requires the **Accessibility** grant (not Input Monitoring).
Creating a tap is not a permission request; without the grant it just fails to
construct. `voice_gesture.rs` therefore:

- calls `AXIsProcessTrustedWithOptions` once per process when the gesture is
  configured but untrusted (the only call that makes macOS show the dialog);
- surfaces it: the tray title reads `Quick Automate… (Double Left ⌥ — needs
  Accessibility)` and a `voice-gesture-status` event tells the webview;
- re-arms when trust flips, via a bounded watcher (2s poll, 10-minute cap,
  single-flight), since `sync_voice_gestures` only runs at startup and on
  voice-config change.

`overlay_gesture_label` is the pure label; `overlay_gesture_menu_label` adds the
permission suffix and is what the tray calls. Keep the trust probe out of the
pure one, or its test depends on the host's Accessibility grant.

**A bundle-identifier change orphans these grants.** TCC keys on the bundle ID
(currently `ai.magicbeans.magican.desktop`), so a rename makes the app new to
macOS and strands Accessibility, Microphone, Automation and keychain items under
the old ID — an upgrade then looks like "everything forgot me". Clear stale
grants with `tccutil reset <Service> <old.bundle.id>`, never bare
`tccutil reset <Service>` (which wipes every app).

## File drag-and-drop into the composer

The HUD window is built with `disable_drag_drop_handler()`; Tauri's default
native drag-drop interception would stop HTML5 `drop` from seeing
`dataTransfer.files` in WKWebView. Files dropped on the composer hit
`FloatingComposer`'s handlers, which dispatch `attachFiles`; ChatPanel routes
them through the same `uploadFilesAsAttachments` path as the file picker. The
drop zone is gated on `supportsAttachments` and read-only/disabled states. The
transparent stage is also a drop zone (so a near-miss uploads instead of
navigating the webview to the file); composer drops `stopPropagation` so nothing
double-uploads; drops on the transcript are swallowed. Other overlay windows
keep the native handler.

## Attach what you were looking at

The composer dock's camera button (`hud_attach_screen_context` in
`screen_ask.rs`) hides the HUD for ~200 ms, captures the window that was
frontmost before the summon through the ⇧⌥A region-capture staging lane
(provenance labelled with that app; the helper refuses our own windows), and
returns with the shot staged as a seed chip. When the panel is already bound to
a chat session, the capture stages into that session (a rebind would clear
anything staged) and the transcript state is preserved. Nothing is captured
until the tap, and an unsent capture is discarded.

## Rebuilding

Changes here need a desktop rebuild (`make build-desktop-tray`); changes
confined to `ui/unified-ui` need only `make build-ui`.

In dev the window loads `http://localhost:5173/hud` (`OVERLAY_HUD_DEV_URL`); a
packaged app loads the bundled `/hud` (`OVERLAY_HUD_APP_PATH`).
`MAGICIAN_DESKTOP_BUNDLED_OVERLAYS=1` in a debug build serves the built
`desktop/dist` through the custom protocol instead (as Settings/Logs do), so
latency checks skip Vite's on-demand transform.
