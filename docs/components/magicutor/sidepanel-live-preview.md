# Side Panel Live Preview

## "Do It" automation creates an internal task

The side-panel "Do It" (`start_automation`) path creates its V3 task as
`Internal`: `createExecution` (`extension_messages.js`) POSTs
`{ title, internal: true }`, so the spawned task lands in `internal_tasks/`
(visible on `/tasks?type=internal`, hidden from the regular `/tasks` list). It's
user-initiated but an execution side-effect that shouldn't clutter the task
list; `created_by` is preserved (not `__system__`), and with no `chat_session_id`
it's never auto-swept. The `start` flow and progress overlay are otherwise
unchanged.

The live-preview allow-list (`content-script.js` `isAllowedLivePreviewPage()`)
matches pathname prefixes `/tasks` and `/internal-tasks`, so
`/tasks?type=internal` is allowed. `/internal-tasks` is retained as a redirect.

## Summary

The `magicutor` side panel has Control, Settings, and Live tabs. Control remains
window-local automation. Live is a global monitor for active automation sessions
across browser windows.

## Behavior

### Control Tab

Window-local: owning `windowId`, draft state keyed by `windowId`, and the
existing automation controls, status, and activity views.

### Live Tab

Lists tracked active automation executions, resolves a previewable tab from
session tab tracking, starts capture on that tab, and streams JPEG frames over
an extension `runtime.connect` port (`live_preview`). The selected tab can
belong to another window than the one hosting the side panel.

## Implementation Notes

### Background Capture Path

The background service worker owns live preview state in
`magicutor/extension/live_preview_background.js`.

Responsibilities: enumerate preview sources from active executions plus session
tabs; choose a capture mode; fan out JPEG frames to connected side-panel ports;
clean up when capture stops or viewers disconnect.

Mode selection in `startTabCapture`:

- Debugger-backed tabs poll `Page.captureScreenshot` (`format: jpeg`) at
  `CDP_PREVIEW_FPS` (4). No user gesture is required, and background tabs still
  render.
- Other tabs use `chrome.tabCapture.getMediaStreamId` plus an offscreen document
  (`offscreen.html`).
- If tabCapture fails, fall back to CDP screenshots (attaching the debugger if
  needed). If that also fails, the worker posts `capture_pending`.

There is no `Page.startScreencast` path.

### Side Panel UI

The monitor UI lives in `live_preview_panel.js`, `sidepanel.html`, and
`control_panel.css`. The `Live` tab renders frames into an `<img>` as
`data:image/jpeg;base64,...`.

## Current Limitations

- Preview source selection is session-driven rather than action-driven; it
  chooses the best available session tab, not a guaranteed "currently
  interacted" DOM target.
- This is a JPEG still-frame stream (CDP polling or tabCapture), not a
  WebRTC/video pipeline.
