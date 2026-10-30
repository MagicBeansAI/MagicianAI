# Magicutor Changelog

## Unreleased

- Extension 0.2.17: Magician names each task's tab group (`?label=`): the conversation's title for a chat run, else the task's title. The extension's own task-title lookup is removed. It never worked: it called a `/v3` URL that the extension's Magician request helper refuses, so groups showed the agent's name. The agent name remains only as a fallback, and never replaces a name from Magician.
- Magicutor 0.1.96, extension 0.2.16: a browser task's tabs open in a tab group named `✱ <task>` in your own window, as background tabs that never take your view. The task falls back to a group in a window of its own (opened without focus) when its page does not keep rendering in the background, checked at start and every 30 s, or when no normal window is open; it falls back to a plain window where tab groups are unavailable. Task tabs get focus emulation so a background page keeps drawing and its load events arrive. The group name ends in ⏸ while the task waits for you and ✓ when it is done; handed-off tabs leave the group. `bridge.automation_surface` (`auto`, `in_window`, `own_window`) sets the starting point, and a CDP URL's `?label=` names the group. The extension's description reads "Your browser tasks, done by Magican" (extension 0.2.17).
- Fixed (magicutor 0.1.95, extension 0.2.15): opening a page with a cross-site iframe (Reddit, X, Substack, Gmail) through the extension no longer times out with "Operation timed out" after the page has loaded. The page's own load events sometimes went out under the iframe's CDP session, so agent-browser never saw them. The extension now forwards each event's child session id, the proxy labels the page's events with the page's session, and iframe response bodies are fetched from the iframe's session.
- Fixed (extension 0.2.15): a browser task keeps receiving page events after Chrome restarts the extension's service worker or the extension is reloaded. The lazy re-attach reset every CDP domain without the client knowing, so every later `open` timed out. The tab's setup commands (domain enables, lifecycle events, iframe auto-attach, bindings, emulation) are now kept in `chrome.storage.local`, cleared on browser startup, and replayed on that re-attach.
- Fixed (extension 0.2.15): one stuck storage call in the task-tab ownership queue can no longer block every tab action (`list_tabs`, `create_window`, `create_tab`, `close_session_tabs`) until the worker dies. Each storage call has an 8 s deadline, readers stop waiting for queued changes after 4 s, and the error names the step that hung. That ownership record moved to `chrome.storage.local` (cleared on browser startup), so a task's tabs are still closed by its cleanup after an extension reload or update.
- Magicutor 0.1.95: bridge timeout and error log lines name the action, CDP method and tab (and elapsed time for errors).
- Fixed: secure phone/OTP fills no longer fail with `transport_uncertain` or drop the agent's browser session.
- Extension 0.2.14: "Ask Magican about this page" also opens on local `file://` pages such as static reports; before, the item showed there but did nothing. It is hidden on pages it cannot open.
- Magicutor 0.1.94, extension 0.2.13: a thread's CDP session sees only the tabs it created, the bound owner tab and popups opened from them; a user tab in the same window stays private, an empty owned set is refused and `Browser.close` never reaches the owner's Chrome. `DELETE /cdp/threads/{id}` closes only task-created tabs (`?keep_tabs=true` hands them to the owner) and returns an error, keeping state for a retry, until cleanup is confirmed. The extension remembers task-created tabs across worker restarts, and a popup that opens just after cleanup is closed instead of adopted.
- Extension 0.2.13: the popup and side panel are organised as Control, Live, Status and Settings tabs, with a bottom-docked chat composer and a live activity feed; menus read "Magican".
- New browsers must be accepted once (in the extension, or remotely from Magician) before they get work, and acceptance survives restarts; when an attended run could use several browsers, the owner picks one from a card in each browser or a Magician decision card (“Always use this browser” sets the default).
- Magicutor 0.1.93, extension 0.2.12: up to four browsers or profiles (`bridge.max_connections`) can connect at once. Each run stays in the browser it started in, a fifth browser is refused with `bridge_full`, a browser's reconnect (proven by a per-install secret key) replaces only its own quiet connection, and the panel shows “Connected as Comet (2 of 4)” with an editable browser name.
- Extension 0.2.11: WhatsApp calls show the caller with Join, Listen, Ignore and Cancel on any tab; Observe can hide the bar, and Cancel hangs up even after Join or Listen.

### Fixed - a thread's dedicated window is checked before it is reused (Magicutor v0.1.92)

- Fixed: chat browser runs failed when a thread's dedicated Chrome window had been closed; a closed window is now detected and replaced.

_Shipped on another branch as v0.1.89 and renumbered on merge (2026-09-21)._

### Fixed - the automation panel and aurora end with the CDP session (Magicutor v0.1.91; extension v0.2.2)

- Fixed: the page's automation panel and border stayed after a run ended until the user pressed Stop. The proxy now tells the extension when a session ends, and the extension shows the real outcome on every tab the run touched.

### Changed - the debugger release is logged at info (Magicutor v0.1.90)

- Debugger detach on session end is logged at info.

### Changed - the extension is named "Magican" (extension v0.2.1)

- The extension is renamed "Magican".

### Fixed - the debugger is released when a CDP session ends (Magicutor v0.1.89; extension v0.2.1)

- Fixed: Chrome's "is being debugged" bar stayed after a run ended; the debugger is now detached from every tab the session attached.

---

Older entries: `docs/archive/changelogs/magicutor.md`
