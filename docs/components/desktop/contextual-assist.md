# Contextual assist (desktop)

The left-Option contextual assist surface: a native macOS Accessibility
watcher (`desktop/src-tauri/src/contextual_assist.rs`) detects what the user
is working on, and a single left-Option tap opens a menu of actions for that
target. The menu itself is the `contextual-assist` Tauri window rendering the
unified-ui `/contextual-assist` route; the action catalog is Rust-owned and
served to the route via the `get_contextual_assist_action_catalog` command —
which first tries the backend's canonical `GET /contextual-writing/catalog`
(10-minute cache; failures are remembered for 30 seconds so a booting
backend cannot stall menu opens on the fetch timeout) and falls back to the
local copy whenever the backend is unreachable or serves a catalog missing
local states, so a stale backend can never hide desktop-native states. See
[contextual-writing](../magician/contextual-writing.md). Action requests
forward the target's `frame_url` alongside `url` for iframe-grounded browser
targets, and `state`/`targetTextKind` are validated server-side against
closed vocabularies.

## Insertion (closing the loop in place)

Writing actions with a writable target end in a preview card, not a dead
draft: **Tab** inserts the result where the user was (`insert_contextual_text`),
**R** reveals a one-sentence refine field that reruns the same action with
the prior visual context intact, **Esc** dismisses, and Copy is always
available. Actions without a writable target (screen fallback, page, files)
get Copy as the landing action.

Insertion is paste-based and focus-verified: the menu resigns keyboard focus
without hiding (an abort leaves the draft on screen), the frontmost
app/window is re-checked against the action's target — mismatch aborts with
nothing pasted — then the text is staged on the pasteboard, Cmd+V is
synthesized for the focused app (over a live selection this replaces the
selection; in a field it lands at the caret), and the user's previous
clipboard is restored after the target app consumed the paste. The focus
resign is guarded by an insert-in-progress flag so the window's own
focus-lost handler does not dismiss the menu mid-insert — the draft must
survive both the paste and any abort. The fallback-mirror catalog in Svelte
lacks the `requiresWritableTarget` flag, so the mirror path degrades to
Copy-only by design.

## Permissions honesty

Without macOS Accessibility trust the watcher detects nothing. The menu
surfaces a banner explaining that (with a button opening the exact
System Settings pane) instead of silently doing nothing
(`get_contextual_assist_permission_status`).

## Invocation

A single left-Option tap (no other modifiers, released within 650 ms) opens
the menu after a 475 ms disambiguation delay. A second Option tap within the
450 ms double-tap window cancels the pending action, suppresses either event
tap callback order through the delay window, and summons the HUD. This keeps
the single-tap listener from taking focus back from the HUD.

Between invocations a watcher polls the frontmost Accessibility target every
300 ms on a tokio worker. Each tick's synchronous body runs inside
`crate::with_autorelease_pool`: tokio threads never drain an Objective-C pool,
and `available_monitors()` autoreleases an `NSScreen.deviceDescription`
dictionary per screen per call, which otherwise leaks without bound (gigabytes
over days). Any periodic desktop task that touches windows or monitors
(the Orb's 1 s tick included) wraps its body the same way.

## Target resolution and the screen-context fallback

On hotkey invoke, targets resolve in priority order:

1. **Chrome extension probe** (extension browsers only, via magicutor
   `/contextual-assist/probe`): supplies selection/field state plus URL,
   frame URL, tab and window ids.
2. **Tauri webview self-context** (Magician's own windows push their
   selection/focus state).
3. **Browser/webview probes** (pasteboard Cmd+C probe and empty-field focus
   for known browser apps).
4. **Live AX detection**, falling back to the last stably-detected target.

When **none** of these finds a text target, the hotkey does not dead-end:
`screen_context_fallback_target` synthesizes an `empty-no-context` target
from the frontmost app and window (title + capture rect), so the menu offers
the observe/write/task/HUD actions grounded in a window capture — invoke
anywhere, with context derived from whatever is on screen.

One ordering rule matters: when the Chrome extension probe answers
*ineligible* (no selection, no editable field — not a secure field), the
hotkey jumps straight to the screen fallback. It must not fall into the
webview hotkey chain, whose pasteboard probe synthesizes a Cmd+C keystroke —
that would inject a spurious copy event into a page the extension just
declared has no target.

The fallback (and the whole menu) stays suppressed when it would be wrong:

- a focused **secure field** (the extension probe reports `secure_field`, and
  the fallback re-checks `AXSecureTextField` itself — a screenshot-backed
  menu must never visualize a password);
- an app in `contextual_assist.excluded_apps`;
- Magician's own process (its surfaces use the self-context path instead).

The always-on watcher only raises the
passive chip for text targets. The fallback applies to the explicit hotkey
path only.

## Screen-ask context join

The screen-ask chords (⇧⌥S screenshot, ⇧⌥A region) stamp captures with the
context they were taken in: at POST time the desktop reads the frontmost app
and window title (`frontmost_app_and_window_title`) and sends them as
`source_app` / `source_window_title` on the capture request. The backend
trims/bounds them and echoes them back; the HUD labels the staged chip
("screen capture — Safari") so a capture is self-describing instead of
anonymous pixels. The region path reads provenance *after* the picker window
hides (plus its 140 ms settle) so Magician's own picker is never the recorded
app — and the helper refuses this process entirely, so a chord fired while
the HUD itself holds focus produces an unlabelled capture rather than one
stamped with our own window. The clip (⇧⌥R) and watch (⇧⌥W) flows span time,
so they stay unlabelled.

## Known target kinds

The watcher recognizes text-shaped targets (selection, selection-in-field,
writable field draft/empty), the screen-context fallback above, and one
page-level target kind:

- **`page-context`** — from the browser extension's "Ask Magician about this
  page" right-click item (any page, no selection needed). The menu offers
  Summarize page (screenshot-backed via tab capture, URL as routing context),
  Task, and HUD. `target_text_kind` is `page_url`; session continuity follows
  the per-site key like every other contextual action.
- **`files`** — a Finder file/folder selection. The hotkey path (never the
  ambient poll — each probe spawns an osascript process) runs a bounded
  AppleScript probe (`tell application "Finder" to get POSIX path of
  (selection as alias list)`, 600 ms timeout, needs the Finder Automation
  TCC grant) and formats the POSIX paths as the action's text context
  (`target_text_kind: file_paths`), with a Finder window capture for visual
  grounding. The desktop never reads file contents — deeper access happens
  through the agent's governed file tools. On denial, timeout, or an empty
  selection, the hotkey falls through to the screen-context fallback, so
  Finder always gets a menu. The Cmd+C pasteboard probe never runs against
  Finder (it would destroy a file selection).

Dragged files dropped onto the HUD composer (see
[hud-overlay-window.md](hud-overlay-window.md)) are the complementary path
into chat for file content itself.
