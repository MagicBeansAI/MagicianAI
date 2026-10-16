# Contextual assist (desktop)

The left-Option contextual assist surface. A native macOS Accessibility
watcher (`desktop/src-tauri/src/contextual_assist.rs`) detects what the user is
working on, and a single left-Option tap opens a menu of actions for that
target. The menu is the `contextual-assist` Tauri window rendering the
unified-ui `/contextual-assist` route. The action catalog is Rust-owned and
served by `get_contextual_assist_action_catalog`, which first tries the
backend's `GET /contextual-writing/catalog` (10-minute cache; failures are
remembered for 30 seconds so a booting backend cannot stall menu opens) and
falls back to the local copy when the backend is unreachable or its catalog
lacks local states. See [contextual-writing](../magician/contextual-writing.md).
Action requests forward the target's `frame_url` alongside `url` for
iframe-grounded browser targets, and `state`/`targetTextKind` are validated
server-side against closed vocabularies.

## Insertion (closing the loop in place)

Writing actions with a writable target end in a preview card: **Tab** inserts
the result where the user was (`insert_contextual_text`), **R** opens a
one-sentence refine field that reruns the action with the same visual context,
**Esc** dismisses, and Copy is always available. Actions without a writable
target (screen fallback, page, files) land on Copy.

Insertion is paste-based and focus-verified. The menu resigns focus without
hiding, the frontmost app/window is rechecked against the target (a mismatch
aborts with nothing pasted), the text is staged on the pasteboard, Cmd+V is
synthesized (replacing a live selection or landing at the caret), and the
previous clipboard is restored after the paste. An insert-in-progress flag
stops the window's focus-lost handler from dismissing the draft mid-insert.
The Svelte fallback-mirror catalog lacks `requiresWritableTarget`, so that
path is Copy-only by design.

## Permissions honesty

Without macOS Accessibility trust the watcher detects nothing. The menu shows
a banner saying so, with a button to the exact System Settings pane
(`get_contextual_assist_permission_status`).

## Invocation

A single left-Option tap (no other modifiers, released within 650 ms) opens
the menu after a 475 ms disambiguation delay. A second tap within the 450 ms
double-tap window cancels the pending action and summons the HUD, so the
single-tap listener cannot take focus back from it.

Between invocations the watcher polls the frontmost Accessibility target every
300 ms on a tokio worker. Each tick runs inside `crate::with_autorelease_pool`:
tokio threads never drain an Objective-C pool, and `available_monitors()`
autoreleases an `NSScreen.deviceDescription` dictionary per screen per call,
which otherwise leaks without bound. Any periodic desktop task that touches
windows or monitors (including the Orb's 1 s tick) must do the same.

## Target resolution and the screen-context fallback

On hotkey invoke, targets resolve in priority order:

1. **Chrome extension probe** (extension browsers only, via magicutor
   `/contextual-assist/probe`): selection/field state plus URL, frame URL, tab
   and window ids.
2. **Tauri webview self-context** (Magician's own windows push their
   selection/focus state).
3. **Browser/webview probes** (pasteboard Cmd+C probe and empty-field focus
   for known browser apps).
4. **Live AX detection**, falling back to the last stably detected target.

When none finds a text target, `screen_context_fallback_target` synthesizes an
`empty-no-context` target from the frontmost app and window (title + capture
rect), so the menu offers observe/write/task/HUD actions grounded in a window
capture.

When the extension probe answers *ineligible* (no selection, no editable
field, not a secure field), the hotkey jumps straight to the screen fallback.
It must not fall into the webview chain, whose pasteboard probe would inject a
spurious Cmd+C into the page.

The fallback, and the whole menu, stays suppressed for:

- a focused **secure field** (the extension reports `secure_field`, and the
  fallback rechecks `AXSecureTextField`; a screenshot-backed menu must never
  show a password);
- an app in `contextual_assist.excluded_apps`;
- Magician's own process (its surfaces use the self-context path).

The always-on watcher raises the passive chip only for text targets; the
fallback applies to the explicit hotkey only.

## Screen-ask context join

The screen-ask chords (⇧⌥S screenshot, ⇧⌥A region) send the frontmost app and
window title (`frontmost_app_and_window_title`) as `source_app` /
`source_window_title` on the capture request. The backend bounds and echoes
them, and the HUD labels the staged chip ("screen capture — Safari"). The
region path reads provenance *after* the picker hides (plus its 140 ms
settle), and the helper refuses this process, so a chord fired while the HUD
has focus produces an unlabelled capture. Clip (⇧⌥R) and watch (⇧⌥W) span
time and stay unlabelled.

## Known target kinds

Besides text targets (selection, selection-in-field, writable field
draft/empty) and the screen fallback:

- **`page-context`** — from the browser extension's "Ask Magician about this
  page" right-click item. The menu offers Summarize page (tab capture, URL as
  routing context), Task and HUD. `target_text_kind` is `page_url`; session
  continuity follows the per-site key.
- **`files`** — a Finder selection. On the hotkey path only (never the
  ambient poll, since each probe spawns osascript), a bounded AppleScript
  probe (`tell application "Finder" to get POSIX path of
  (selection as alias list)`, 600 ms timeout, needs the Finder Automation TCC
  grant) formats the POSIX paths as text context
  (`target_text_kind: file_paths`), with a Finder window capture. The desktop
  never reads file contents; deeper access goes through the agent's governed
  file tools. On denial, timeout or empty selection the hotkey falls through
  to the screen fallback. The Cmd+C probe never runs against Finder (it would
  destroy a file selection).

Files dropped onto the HUD composer (see
[hud-overlay-window.md](hud-overlay-window.md)) are the complementary path for
file content.
