# Changelog - Magdroid Companion

All notable changes to the Android automation companion are documented here.

This file also serves the Apache 2.0 §4(b) obligation to state significant
changes made to the vendored work.

---

## [Unreleased]

### 0.6.16 (versionCode 43) — 2026-10-30

- A structured reply whose rows carry numbers (an artifact's `size`) loads again: row values are read as text instead of failing the whole message with "String literal for key 'size' should be quoted".
- The composer's `/engine` and `/run-engine` now pick an engine, then its model and thinking level, showing the current choice, and `/voice-mode` switches between Live, Dictate and Hands-free.
- Chat pages older messages and shows sessions being deleted, Today cards get an **⚡ Act** menu, Goals lists closed workspaces separately, and Today publishes pulse slices independently.
- Chat loads older messages a page at a time as you scroll to the top, history rows show **Deleting…** until the server confirms, and the history drawer re-sorts itself as chats start and finish.
- Today's Morning Brief deck replaces **⚡ Do it** / **⚡ Open** with **⚡ Act**, a menu of the card's own actions: follow-up channel actions, Open in the provider, Start follow-up task… with an optional instruction, Snooze and Details. Broadsheet cards get the same **⚡ Act** menu, the message sheet's **⚡ Do it** becomes the same **Start follow-up task…** prompt, deck cards show a **Details ›** link, and **Ask Presto** now reads **Ask <your assistant>**.
- Goals: a workspace whose goals are all done or dropped is listed only under Closed, and each list says so when it is empty.
- Today publishes pulse slices independently, omits full-universe card diagnostics, and distinguishes unavailable values from zero.

### 0.6.15 (versionCode 42) — 2026-10-28

- Today restores Reading Room timestamps and source labels, distinguishes loaded cards from totals, and fixes deck paging and progressive loading.

- Live voice keeps mute-button colours stable on first draw; Live and dictation show measured waveforms with smooth composer resizing, without refreshing the whole chat.
- The live-call panel matches iOS: an aurora orb that moves with your voice, a call timer, rolling captions with speaker names, and a red X to end the call. Choose **Open mic** or **Hold to talk** in a two-part control. The status line and composer strip show the current mode, and a muted mic shows a crossed-out mic icon instead of a speaker.
- Settings → Service accounts connects Google Workspace, Telegram and WhatsApp from the phone (code, pairing code, QR or Google sign-in); rows missing a tool or keys read **Unavailable**.

- The God modes menu offers **Auto-approve this chat**: God mode then asks only before deletes, root and code a command does not show.
- Long commands in a once-only God-mode approval scroll, keeping Deny and **Allow once** visible.
- An MCP sign-in ask offers **Not now**, which cancels the sign-in and closes the ask everywhere.
- Settings → Service accounts shows each account's status with **Reconnect**, and multi-endpoint services sign in per endpoint.
- Deleting a task that is still finishing shows "This task is still finishing. Try again in a few seconds." instead of a raw error.

### 0.6.14 (versionCode 41) — 2026-10-26

- Chat and VibeDev composer model chips show the selected thinking level beside the model, as on web.
- A send an agent made that was held for review shows in Attention with its preview, recipients and expiry, and three choices: **Send**, **Discard**, or **Always allow** that agent to send to that recipient. It is never answered from the notification; older servers show the plain confirmation as before.

### 0.6.13 (versionCode 40) — 2026-10-25

- Notices you can only dismiss (a service-health notice, an expired notice) no longer appear in the chat or VibeDev composer; Attention still lists them.
- Another chat's God-mode approval shows in the composer dock with its origin instead of as this chat's question, as on web and iOS; VibeDev still lists God approvals first.
- Magican settings has a Refresh button on the right of its top bar; it reloads the page, or retries after a failed load.
- Magican settings (the embedded web Settings) shows its page instead of a blank screen: the WebView now fills its space (MATCH_PARENT), where the default wrap-content height made every viewport unit on the page read 0.
- Settings → Help & about and Connection → Services show the Magician core version for the backend (not the API crate's), plus the Magician API, Magicutor and Desktop versions when the machine reports them.
- The app's own version row reads **Magican Android** with its build, e.g. `0.6.13 (40)`.
- The app reports its version to the paired machine on every request, so web Settings → Devices shows which build each phone runs.

### 0.6.12 (versionCode 39) — 2026-10-25

- Settings keeps only this phone's settings — Voice & audio, Notifications & permissions, Keyboard & privacy, Connection, Help & about — with search. **Magican settings** at the top opens the shared web Settings inside the app (sign in once; the app remembers it), which now holds Budgets, Harness credentials, Your preferences, storage and speaking voices. Searching for one of those leads to Magican settings.
- Launch: the system splash shows only the background and the launch screen fades in the star with the name, so phones whose launcher enlarges the splash icon (HiOS) no longer show an oversized, clipped star that then shrinks.
- A question from another chat in the same named thread is no longer offered as this chat's; it shows which chat it came from. One model call shows as one Steps row, not a chat-side and a typed copy.

---

Older entries: `docs/archive/changelogs/magdroid.md`
