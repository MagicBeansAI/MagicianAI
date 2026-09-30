# Magicutor Changelog

## Unreleased

### Fixed - a thread's dedicated window is checked before it is reused (Magicutor v0.1.92)

- `ensure_thread_window` returned a thread's cached Chrome window id for the
  life of the process, but the window itself is closed by the run that used
  it (`agent-browser close`) or by the user. A shared thread id
  (`magician-chat-<thread>`, the window every run rooted on a chat thread
  attaches to) then came back to a window that no longer existed, and every
  `Target.createTarget` on the thread answered "no tab attached for this
  session" — the chat browser flow failed on every engine ~70 s in. The
  cached window is now judged by the extension's `list_tabs` (Chrome keeps no
  empty windows: no tab in it means it is gone), a gone window is forgotten
  along with its owned tabs and a fresh dedicated window is created; an
  unanswered check keeps the cached id, since recreating would not fix a
  bridge that is down. Tests:
  `a_cached_window_is_live_only_while_the_extension_still_lists_a_tab_in_it`,
  `forgetting_a_gone_window_drops_only_that_threads_mapping`.

_Merged from origin/meetable_bot on 2026-09-21, where it shipped as v0.1.89; renumbered because this branch's v0.1.89–v0.1.91 (the debugger release and the automation UI ending with the session) already carried that number._

### Fixed - the automation panel and aurora end with the CDP session (Magicutor v0.1.91; extension v0.2.2)

- Runs 12–16 (2026-09-21): the CDP session closed and the debugger
  detached at run end, but the page's status panel (Stop button) and
  aurora border stayed until the user pressed Stop — whose
  `stop_automation` also cancels an execution that had already finished.
  Cause: the only path that told the page a run had ended was the
  one-minute `cleanupStaleExecutions` alarm, and it addressed the
  terminal `overlay_status` to the session's registered tabs — already
  cleared by `detach_debugger`'s `cleanupSession()` — falling back to the
  tab id tracked at window creation, which the run had navigated away
  from or replaced (`Target.createTarget`). The message went to a stale
  tab; the execution was then untracked, so nothing retried.
- Proxy: `CdpSession::stopped` sends `session_ended {sessionId}` before
  the per-tab `detach_debugger`s, in one ordered task
  (`spawn_session_end`), while the session still knows its tabs. Logged
  at info with the extension's result.
- Extension: new bridge action `session_ended` → `endAutomationSession`:
  asks Magician for the execution's status (2 s); a terminal status
  reaches every overlay tab as the real ending (success/error gathering
  animation), an unreachable Magician gets `overlay_panel_hide` +
  `overlay_hide`, and a paused / waiting-children execution keeps its
  panel. Then the execution is untracked and the session cleared.
  `executionOverlayTabs` addresses the session's tabs, the tracked tab,
  and every tab in the execution's dedicated window; the cleanup alarm
  uses it too.

### Changed - the debugger release is logged at info (Magicutor v0.1.90)

- `[cdp-proxy] session stopped thread=… detaching debugger from N tab(s)`
  and `detached debugger tab=N` moved from `debug` to `info`. After run
  12 (2026-09-21) the question "was the debugger released when the run
  ended?" could not be answered from the service log, which runs at
  info: the agent-browser daemon was gone and the extension's own map
  said detached, but neither proves `chrome.debugger.detach` ran. One
  line per session end plus one per tab — low volume, high signal.

### Changed - the extension is named "Magican" (extension v0.2.1)

- `manifest.json` name "Magican Browser Extension" → "Magican"; popup and
  side-panel titles follow; version 0.2.0 → 0.2.1.

### Fixed - the debugger is released when a CDP session ends (Magicutor v0.1.89; extension v0.2.1)

- `CdpSession::stopped` sends `detach_debugger` for every top-level tab
  the session attached, and an explicit `Target.detachFromTarget`
  releases a top-level tab no other session still references. Attach
  had been proxied on the way in and nothing proxied a detach on the way
  out, so a tab the agent drove kept `chrome.debugger` attached — Chrome's
  "is being debugged" bar — after the run ended and the Magician log said
  the session was closed.

---

Older entries: `docs/archive/changelogs/magicutor.md`
