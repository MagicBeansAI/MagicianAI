# Changelog

## [Unreleased]

### 2026-11-02 — 0.4.16 (build 227)

- A failed tool call in a task's deep panel shows why it failed, two lines with Show more when cut.
- Chat task cards show when the task was created and last updated ("Created 14:28 · Updated 2 min. ago", with the date when it is not today), and expanded Steps show when they started and last updated. Touch and hold the line to see and copy the exact times.
- Chat links are visible (accent colour and underline) and work: one router handles every chat link. `file://` and task paths open the delivered file in the in-app viewer (or on the host), relative `/api/…` links are resolved against the server and fetched with the session token, app routes navigate in the app, and anything else shows "Can't open link" instead of doing nothing. Structured text, callout and summary blocks render Markdown, and mentioned paths become links.
- Chat links to a conversation or thread (`/chat?session=…`, `/t/<thread>[/chat][?session=…]`) open that conversation in the app, or the thread's latest one. `/notes`, `/vibe` and thread task links open their native screens instead of the web view.
- Tapping a thread in History opens its latest conversation and keeps History open on Sessions, showing only that thread's sessions (Personal and Automated). A **#thread ×** label replaces the Personal/Automated switch; × shows sessions from all threads again. **New Session** there starts the conversation in that thread.
- Renamed threads keep working: a `/t/<slug>` link, current or old, is resolved to its thread before opening. Thread chips show the thread's name. New Thread requires a name, lets the server choose the id, and opens the new thread.

### 2026-10-31 — 0.4.15 (build 226)

- A partly typed command (`/eng`, `/budget defa`) opens that command on Send instead of reaching the model; a message that only starts with a command name (`/budget please help`) is still sent, and the send options menu is hidden while the composer holds a command.
- Opening a conversation shows a spinner until its transcript loads, and **Try again** if it fails, instead of the welcome screen.
- Team routines dispatch each harness crew member's current focus areas, read when the sequence starts, instead of five retired built-in routines.

### 2026-10-30 — 0.4.14 (build 225)

- A new or cleared chat opens on the welcome screen (suggestions and recent chats) instead of a stored "I am Sam" greeting, which kept the transcript from ever being empty.
- Composer `/engine` and `/run-engine` go engine → model → thinking level (the model dropdown's catalog; Settings → Agentic flows for runs) and show the current choice; new `/voice-mode` picks Live, Dictate or Hands-free.
- Chat shows **Compacting conversation · k/N**, pages long histories and marks sessions being deleted; Today cards share an **⚡ Act** menu; Goals splits Open and Closed; Today publishes pulse slices independently.
- Chat's context step shows **Compacting conversation · k/N** and its result, and marks a failed compaction as failed.
- Chat loads earlier messages as you scroll to the top of a long conversation, and History shows **Deleting…** on a session until the delete finishes, then keeps times and order fresh while chats run.
- Reading Room deck and Broadsheet cards share one **⚡ Act** menu with the card's own actions (reply, reminders, open in the app, a follow-up task that asks for your instruction first, snooze, Worth actions); deck cards also get a **Details ›** link, and **Ask Presto** now names your agent.
- Goals lists only open goals, with an **Open | Closed** switch; open goals have **Mark done** and closed goals **Reopen**. The All work summary counts only open goals. A workspace whose goals are all closed is listed only under Closed.
- Today publishes pulse slices independently, omits full-universe card diagnostics, and distinguishes unavailable values from zero.
- With notifications denied, the status message points to **Settings → Notifications** (the old Permissions section is gone).

### 2026-10-28 — 0.4.13 (build 224)

- Today restores Reading Room timestamps and source labels, distinguishes loaded cards from totals, and fixes deck paging and progressive loading.

- Live voice recovers stopped capture; Live and dictation show measured waveforms with smooth composer resizing, and either side of the mode control opens modes and Settings.
- In a Live call the mic mode is a clear **Open mic | Hold to talk** switch with the active mode highlighted; Open mic shows a round mute button, Hold to talk a wide hold button, and the status line and composer strip name the mode.
- The God modes menu offers **Auto-approve this chat**: God mode then asks only before deletes, root and code a command does not show.
- An MCP sign-in in Attention offers **Not now**, which cancels the sign-in and closes the ask everywhere.
- Chat artifacts with long output paths open again.
- Settings → Service accounts shows each account's status with **Reconnect**, and multi-endpoint services sign in per endpoint.
- Settings → Service accounts connects Google Workspace, Telegram and WhatsApp from the phone (code, pairing code, QR or Google sign-in); rows missing a tool or keys read **Unavailable**.
- Deleting a task that is still finishing shows "This task is still finishing. Try again in a few seconds." instead of a raw error.

### 2026-10-26 — 0.4.12 (build 223)

- Chat and VibeDev composer model chips show the selected thinking level beside the model, as on web.
- A send an agent made that was held for review (an email or message to someone outside) now shows its preview in Attention with **Send**, **Discard** and **Always allow <agent> to send to <recipient>**, plus the recipient and when the review expires; the composer ask dock opens it there instead of offering Approve/Reject.

### 2026-10-25 — 0.4.11 (build 222)

- Notices you can only dismiss (a service-health notice, an expired notice) no longer appear in the chat or VibeDev composer; Attention still lists them.
- Magican settings has a Refresh button on the right of its navigation bar; it reloads the page, or retries after a failed load (the same action as Retry).
- Settings → Help & about shows the Magician core version for Magician backend, plus Magician API, Magicutor and Desktop versions when the paired machine reports them; the Connection service rows show the same versions. The app's own row now reads "Magican iOS".
- The app sends its version with each request to the paired machine, so web Settings → Devices can show which build each iPhone runs.

### 2026-10-25 — 0.4.10 (build 221)

- The composer ask dock no longer shows another chat's question as this chat's own when both sit in the shared General thread, so typed text answers only this chat's question; one model call no longer shows as two step rows.
- Settings keeps only this iPhone's settings — Voice & audio, Notifications, Keyboard, Connection, Help & about — with search. **Magican settings** at the top opens the shared web Settings inside the app (sign in once; the app remembers it), which now holds Budgets, Harness credentials, Your preferences, notes from preference checks, storage and speaking voices. Searching for one of those leads to Magican settings.

---

Older entries: `docs/archive/changelogs/magios.md`
