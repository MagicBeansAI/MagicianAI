# Magicutor - Extension CDP Proxy

Magicutor is the local browser bridge used by Magician's browser inner loop. It
does not own agentic browser decisions anymore. Magician runs `agent-browser`;
Magicutor exposes a Chrome DevTools Protocol compatible proxy that forwards CDP
requests to the installed Chrome extension through a WebSocket bridge.

**Current versions:** Magicutor `0.1.92`; browser extension `0.2.2` (named **Magican**).

## Current Runtime

- `GET /health` reports the local proxy status and `extension_connected`; Edge
  advertises browser control only while that field is true.
- `GET /json/version`, `GET /json`, and `GET /json/list` expose CDP discovery.
- `GET /devtools/browser/{thread_id}` is the browser-level CDP WebSocket used by `agent-browser`.
- `GET /devtools/page/{tab_id}` is a page-level CDP shortcut for direct clients.
- `POST /cdp/threads/{thread_id}/bind-tab` binds an existing Chrome tab to an agent-browser thread for extension-initiated "Use this tab" runs.
- `DELETE /cdp/threads/{thread_id}` clears Magicutor's thread ownership state and closes dedicated proxy-created windows.
- `POST /contextual-assist/probe` asks the connected extension to inspect the
  active browser tab for selected text or a focused writable DOM field. This is
  a one-shot metadata probe for desktop Contextual Assist, not a continuous page
  capture buffer.
- The extension's right-click menu has these entries: **Magician** (on
  selection/editable targets, opens the native assist menu with the text) and
  **Ask Magician about this page** (on any page right-click, opens the native
  assist menu in `page-context` state with the tab URL/title — grounding comes
  from a tab capture on the desktop side). A third, selection-only entry,
  **Save selection to Magician Notes**, posts the selection to Magician's
  `/notes/capture-selection`.
- `GET /trace/drain/{thread_id}` drains per-thread network traces for Magician API mining.
- `GET /auth/drain/{thread_id}` one-shot drains bounded, memory-only raw request
  auth for transfer into Magician's encrypted scoped secret store. Durable
  traces remain redacted.
- `GET /ambient/page/drain/{thread_id}` drains per-thread page signals mirrored from foreground CDP automation commands/events for Magician ambient page understanding.
- `GET /bridge/native` is the historical bridge URL name. It is a WebSocket bridge, not Chrome native messaging.

The extension bridge is single-flight: one connection attempt, one reconnect
timer with bounded backoff/jitter, and one active socket generation. Magicutor
also assigns a server session generation, closes a superseded session, and only
lets the active generation clear the shared sender or pending requests. This
prevents stale close callbacks from producing reconnect fanout or disconnecting
a healthy replacement session. This lifecycle contract ships in Magicutor
v0.1.86 and extension v0.1.265. Magicutor v0.1.87 added the transient CDP auth
drain; v0.1.88 and extension v0.1.271 move Magician scope authority into the
workspace-bound bearer and enforce the configured Magician origin before
attaching it.

Removed runtime surfaces:

- Chrome native messaging host mode.
- Magicutor `/execute` browser-action API.
- Magicutor `/sessions` session manager API.
- Native Chromium/chromiumoxide executor.
- Rust-side browser action evaluators and generated-action registry.

## Architecture

```text
Magician execution API (3002)
        |
        | starts browser inner loop
        v
agent-browser CLI
        |
        | CDP: ws://127.0.0.1:3003/devtools/browser/{thread_id}
        v
Magicutor CDP proxy (3003)
        |
        | WebSocket bridge: /bridge/native
        v
Chrome extension background worker
        |
        | chrome.debugger / tabs / windows / downloads
        v
User Chrome profile
```

## Extension-Initiated Automation

The popup and side panel start work through Magician, not through Magicutor:

1. The extension creates a Magician execution with `POST /api/magician/v2/executions`.
2. If "Use this tab" is selected, the extension calls `POST /cdp/threads/magician-{execution_id}/bind-tab` before starting the execution.
3. The extension starts the execution with `POST /api/magician/v2/executions/{id}/start`.
4. The browser inner loop uses the same `magician-{execution_id}` thread id when `agent-browser` connects to Magicutor CDP.
5. Extension overlay, live preview, tab tracking, and stop/cleanup all use that same browser thread id.

Magician and Magicutor are expected to already be running. The extension does
not launch local binaries.

## Development

Run focused checks while editing:

```bash
cargo check -p magicutor
cargo check -p magician --bin magician
make test-magicutor-extension
```

Use `make check-all` before release-level validation.
