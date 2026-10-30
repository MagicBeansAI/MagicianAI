# Changelog

## [Unreleased]

- A new or cleared chat opens on the welcome screen (suggestions and recent chats) instead of a stored "I am Sam" greeting, which kept the transcript from ever being empty.
- Composer `/engine` and `/run-engine` go engine → model → thinking level (the model dropdown's catalog; Settings → Agentic flows for runs) and show the current choice; new `/voice-mode` picks Live, Dictate or Hands-free.

### 2026-10-30 — 0.4.14 (build 225)

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
