# Changelog - Unified UI

## [Unreleased]

### 2026-11-02 — 0.1.73

- The God mode settings panel says god mode now reaches coding runs started from the chat.
- Stop works on a turn the server no longer runs (one a restart cut off). The card's Stop cancelled only the turn's run, which had already ended, so nothing changed; when the run has nothing left to cancel and only the page's guess keeps the turn live (no send, no running task, no running turn on the server), Stop now records the turn as stopped (a turn the server already settled keeps its state; the server's own running state still wins).

### 2026-11-02 — 0.1.72

- A failed tool call in the task panel's timeline shows why it failed, clamped to two lines with Show more.
- Tapping a thread in the history rail or drawer still opens its chat, and now keeps the panel open (phones too) on Sessions filtered to that thread, `#general` included, until × clears it or another thread is tapped; New Session there creates in that thread.
- The public site's title, descriptions, `llms.txt` and share image (`og-image.png`) say **More You — Life assistant for work, play, and everything else** instead of "Personal AI under your control", "side quests" and "Your personal superintelligence".
- Chat task cards show **Created <time> · Updated <age>** and the Steps card shows **Started <time> · Updated <age>** (e.g. "Created Oct 31, 14:28 · Updated 2m ago"); hover for the exact times. The age keeps moving after the work settles.
- New thread asks for the name first (Enter submits, Esc cancels, Create is disabled while the name is empty) and opens `/t/<slug>`. `/t/new` shows the same prompt instead of creating a thread called `new`. Thread URLs follow the name, old links redirect in place, and `#` tags show thread names instead of ids.
- Global shortcuts no longer fire while you type in the name dialog, and Work's thread filter accepts a thread's slug.
- A finished task's card and the task panel show the main deliverable first (marked, with Download), then other delivered files, then the agent's summary labelled **Summary** instead of its internal file name. Continuation outputs are hidden. Links to `/api/magician/v3/tasks/{id}/outputs/…` open through the authenticated output fetch for any task.

### 2026-10-31 — 0.1.71

- Team routines' **Start team sequence** runs one stage per focus area of each harness agent, read from the live definitions, instead of five built-in CXO routines retired in October; with no focus areas configured it says so and stays disabled.
- Team routines dispatch the stages shown when the sequence started, even if the roster changes mid-run; two focus areas that name the same goal are one stage; a focus area with no description is kept; goal ids follow the server's 64-character slug limit; the stepper says when routines are loading or could not load.
- A `/` command draft can no longer go out through **Stop & send** or **Run in parallel** (the send options are hidden while the picker is open), and a `/engine` model pick that fails leaves the engine unchanged.

### 2026-10-30 — 0.1.70

- Composer `/` commands: `/engine` and `/run-engine` show the current engine and model, then list the chosen engine's models (the composer dropdown's catalog, the Agentic flows loop model, or the CLI's models) and then the model's thinking level; new `/voice-mode` switches Live, Dictate and Hands-free.
- Attention sorts asks into the lane of what they need, Today cards get an **⚡ Act** menu, chat shows deletion progress and pages history, Goals separates closed workspaces, and Today/analytics read bounded windows with unavailable spend shown apart from zero.
- A live turn's expanded step list keeps a long step beside its dot, wrapped to two lines with an ellipsis, instead of moving it to the line below.
- Attention lists service outages under **Failed** and plan approvals, diff reviews and action confirmations under **Approvals**, using the server's lane for each ask; chat's context step shows **Compacting conversation · k/N** and marks a failed compaction as failed.
- Today's Morning Brief deck and Broadsheet cards: ⚡ **Act** opens a menu of the card's own actions (reply and other channel actions, open in the provider, start a follow-up task after an optional instruction, snooze, details) instead of starting a task at once; cards gain a "Details ›" link, and Ask Presto is shown as "Ask <your agent's name>".
- Chat shows deletion progress, opens recent messages directly, loads activity near the viewport, and refreshes history timestamps and ordering live.
- Goals: a workspace whose goals are all done or dropped is listed only under Closed, and each list says so when it is empty.
- Today avoids full-universe diagnostics on card reads; analytics prepare bounded windows and show unavailable spend separately from zero.
- `/llm` loads faster: time windows are sent as literal bounds the server can prune on, today's widgets load in their own batch instead of waiting for the 7- and 30-day queries, and the rest of a tab goes out as one request.

### 2026-10-28 — 0.1.69

- Today restores Reading Room timestamps and source labels, distinguishes loaded cards from totals, and fixes deck paging and progressive loading.

- Dictation shows a Listening waveform and timer; live voice shows measured microphone and speaker waveforms in inline and floating controls.
- 0.1.68: Settings → Notifications → Owner identities lists owner identities per channel (masked) and mints a one-time code to claim a WhatsApp number or Telegram chat with `/claim`; Critical alerts links to it when a channel has no owner.
- Live voice call: the mic mode is a segmented **Open mic | Hold to talk** control above the main mic control (round mute button with a level ring in Open mic, the hold button in Hold to talk), and the status line names the mode ("Open mic · Listening", "Hold to talk · Ready"); the desktop call pill uses the same control and words.
- Settings → Service accounts checks a connected Google account's Cloud project roles and offers **Grant access** (or the commands) and **Check again**.
- God mode auto-approve is on by default (Settings → Agent powers) with a per-chat **Auto** switch by the orb; it still asks before deletes, root and unshown code.
- Sign-in buttons on the Magician computer itself show one action that opens the sign-in in that computer’s browser; elsewhere they offer “Connect on this device” and “Open on computer”.
- An MCP sign-in ask offers **Not now**, which cancels the sign-in and closes the ask everywhere.
- Chat artifacts whose id the server shortened (long output paths) open again via `meta.artifact_locators`.
- Settings → Service accounts shows a status chip per account, **Reconnect** for connected ones, and one row per endpoint of a multi-endpoint service.
- Settings → Service accounts connects Google Workspace, Telegram and WhatsApp in place (code, pairing code, QR or Google sign-in); rows missing a tool or keys read **Unavailable**.
- Deleting a task that is still finishing shows "This task is still finishing. Try again in a few seconds." instead of a raw error.

### 2026-10-25 — 0.1.67

- **Spend ledger** (`/budget`) opens on your sign-in instead of a 401; the resource-authority key is needed only to freeze, fund, revoke or close periods.
- Settings → **Budgets** is now **Spending**: **Model spend** controls, editable **Action limits** (`/budgets/limits`, applied on restart) and a link to the spend ledger.
- A captured outward send held for review (`escalation_trigger: "outward_review"`) shows its preview and then **Send** (primary), **Discard** and **Always allow <agent> to send to <recipient>** (the server's label), answering as a choice (`selected_id`: `send` / `discard` / `always_allow_recipient`) like tool authorization; a muted "Expires in … · <date>" line shows when the review has a deadline. Attention rows never offer it as a one-click Approve / Deny.
- Settings → Outward actions: **When a send is captured** — *Ask me to review* (pause and ask on Attention and the phones) or *Just record it* — right under capture-all, shown only on servers that report `review_captured_sends`; under capture-all it notes that sends are recorded without asking.
- Settings → Outward actions: an **Always allowed** list of agent → recipient permissions with **Revoke**, and an empty state explaining that Always allow on a captured send adds one; hidden on servers without it.
- Settings → Outward actions: **Capture all sending categories** sits in an accented box of its own (tinted, accent edge), deepening and showing "On · nothing is sent" while it is on, since it overrides every sending choice below it.
- Settings opens behind a skeleton of its own shape (search, category chips, a heading and cards) instead of a blank page and "Starting your workspace…": a static copy in `app.html` before any script runs, the same markup while the server is still starting, and each section shows a card skeleton until its panel arrives.
- Settings → Devices names the ESP32 voice terminal's build "Voice terminal firmware <version>" and, until it reports one, asks to update the firmware rather than "the app".
- Embedded Settings in the iOS and Android apps drops its own bold "Settings" heading (the app's top bar already names the screen); the search field takes the row. Web and desktop keep it.
- Embedded Settings (the phone and desktop apps' "Magican settings") opens in about 1.6 s over the tunnel instead of ~8.5 s: it is served without the app shell (same URL, rerouted to its own route), client-rendered, loads the opened category's panels first and the rest straight after, and the dev server compresses responses that come through the Cloudflare tunnel.
- Settings embedded in the iOS and Android apps loads again over the machine's UI address: `/settings` is allowed off-device (it was sent to `/` the moment a signed-in phone opened it, showing an error page), and the embedded pane takes its height from the window, because an Android WebView hosted at wrap-content height reports every `vh`/`dvh` unit as 0 and the page was blank.
- Settings → **About → Versions** (last category): the web app's version and, from `/health`, the Magician backend, Magician API, Magicutor and Desktop versions with their status; missing ones show "—" with offline/unknown, and **Refresh** re-reads them.
- Settings → Connected devices: each device shows the app version it last reported ("Magican iOS 0.4.11 (222)", "Magican Android …", "Magican Desktop …"), or "Version not reported yet — update the app".
- The top bar's web app version next to the brand stays visible at every width, in a legible secondary colour.
- Settings → Notifications → Browser notifications: when the server has no notification (VAPID) keys, **Set up browser notifications** creates them on the server and the panel moves straight to Turn on.
- Settings → Local models: a **Processing** switch (On device / Cloud) for `privacy.processing.mode`, with a list of the operations it moves and a confirmation before sending work to the cloud; it applies at once. The Outward actions card uses the Settings theme tokens.
- Settings controls share one scale: action buttons, dropdowns and inputs are 32px rounded squares with 13px text (16px fields on touch screens, so phones don't zoom), dropdowns stop at 28rem, and compact pickers, tabs and chips keep their own sizes. Chat and Agentic flows sit in matching boxes with their controls at the top and buttons in one row; the decision routing panel, critical alerts and connected devices are tightened, and the trust-policy viewer is shorter. Card headers with buttons keep them beside the title and run the description the full card width underneath. Every card's small label above its title has the same line over it.
- Settings → Engines: the run engine row is now **Agentic flows** (engine + model), which background runs and the work they start use. It can be **Same as Chat**. With Magician, its model and thinking level are picked in place (the `agentic_decision` mapping in Model routing) instead of a disabled "CLI default" box. The Decision Engine setting and its model routing share one box, after Chat and Agentic flows.
- Settings → Connected devices: **Connect iPhone**, **Connect Android** and **Connect Desktop Edge** open their connection choice and QR code right under the buttons, sliding open above Android screen control, instead of below it off-screen.
- Settings → Agents & safety now holds **Preference checks** (moved out of Engines), as its own card under Your preferences, in the Settings type scale and theme colours: what is in force, how firmly to check, and notes from recent checks in soft inner boxes.
- Landing page on iPhone: scrolling down no longer reloads the page just as "Love what you do? … Go. Do what you love." begins. The day walkthrough wrote each scene's progress (`--local`) at four decimals, and Safari kept style data for every value — about 250 MB per pass — until iOS reloaded the tab. Progress is now written in hundredths (`cssProgress` in `scrub.ts`), which holds memory flat across repeated passes.
- The notch Attention mini panel: smaller category tabs and filter field, thin accent-coloured scrollbars (horizontal under the tabs, vertical for the cards) instead of the grey bar that covered the tabs, and a subtle edge in the theme's own colour.
- Settings is grouped into seven categories with a sticky left panel that follows your scroll, and a search that filters cards as you type (`/` to focus, Enter to jump, Esc to clear). On phones the categories become a swipeable chip bar under a sticky search, and every card fits a 375px screen.
- Settings → Agents & safety shows **Your preferences**: how many rules, goals and tastes you have, with a link to edit them in Memory → Preferences. The phone apps reach it through their embedded Settings.
- Expanded chat Steps show each model call's latency, tokens, cache and cost, and each context phase's timings, on a muted second line.
- A page waiting on the service ("Starting your workspace…") checks again as soon as it is shown or focused, so a desktop notch panel suspended while hidden no longer stays on that screen after the service is ready.
- Notices you can only dismiss (a service-health notice, an expired notice) no longer appear in the chat or VibeDev composer; the Attention page and the desktop notch still list them.
- The composer dock no longer treats another chat's question as this chat's: chats sharing the default thread each keep their own questions, so typed text cannot answer another chat's question. A question its feed row names as another chat's (in the row's input schema or ask scope), and in the default thread another chat's task questions, stay elsewhere too.
- `/settings?embed=1` shows Settings without the app shell, for the desktop and mobile apps to embed. Inside the desktop Settings window it shows the window's This Mac / Magican tabs, and links to other pages open outside the window.
- Memory → Preferences: **Review what I've learned (N)** lists only the preferences Magician inferred, newest first, 25 at a time with a source filter, each with where and when it was learned. **Confirm** makes one yours (it can then make an outward act wait); **Dismiss** retires it so checks stop weighing it.
- Decision models: a **Decision model health** card at the top lists each operation's decided, unresolved, timed-out and errored decisions, p50/p95 latency against the caller budget and the last cause, coloured healthy / degraded (over 10% or p95 over budget) / failing (over 50%). It loads on page open and Refresh, with a 1 h–7 day window.

### 2026-10-24 — 0.1.66

- Chat, saved responses and task Steps show context preparation with its timings, and Decision Engine calls with tokens, cache and cost.
- Memory search can pick the agent to search as (Personal Assistant by default), ignores stale results after a switch, and flags incomplete results.
- Settings → Budgets: the per-run limit (or separate Magician and harness limits), the daily spend limit with its warning %, today's spend, remaining and reset, per-class spend, and Add to today.
- Models tab per chat: calls, tokens, cache and cost per model including delegated work, refreshed on an interval you pick (default 1 min) with a countdown, only while the tab is visible; the usage chip moved into the thread bar with a by-model breakdown. Models, Files and thread Settings follow the selected theme.
- Files shows sizes and a total and can delete a chat copy after confirmation; task originals stay under Work.
- The composer dock shows every open question wherever you are: this chat's first, others labelled with the chat or task they came from and answered in place; typed chat text answers only this chat's question.
- Task reports render an HTML answer whose closing fence was cut off by a length limit as the page, not as source.
- The composer ask dock shows every open question with its own state and falls back to Work's plan review; VibeDev's dock adds project-chat questions and workspace God approvals. An answer counts only once the server accepts it, a closed approval never starts a run, and switching credentials clears pending asks.
- Dictation and spoken replies are billed to the chat that started them; speech reads findings first and ends with "Some findings remain unverified." Contextual Assist follows the app theme and opens on its current target. Run controls refresh after a refused action.

### 2026-10-23 — 0.1.65

- Chat Files tab (`/chat/files`, `/t/{name}/files`): a chat's uploads and the artifacts of its tasks, filterable, searchable, openable and downloadable.
- One thread/session bar across Chat, Tasks, Settings and Files, with status badges and the engine and model the next message will use.
- Usage totals chip in the chat header and task drawer: calls, tokens, cache and cost including delegated work, marked "≥" when some calls were unpriced.
- Unsent drafts survive a reload per chat; engine and model picks are kept per panel and inherited by new chats; a provider outage raises its own notification card.
- Cancelled tasks no longer show "writing the final answer"; the goal deadline chip opens the date picker; the Super God orb is labelled while active; a voice call locks in its model picks.

---

Older entries: `docs/archive/changelogs/unified-ui.md`
