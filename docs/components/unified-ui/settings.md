# Settings page

`/settings` (`src/routes/(app)/settings/+page.svelte`) renders every settings
card on one page, grouped under eight categories. The categories, the cards in
each and their search keywords live in `$lib/settings/settingsIndex.ts`:

Shared panels reset when account scope changes; outward-send grants also drop
pending responses from the previous workspace so they cannot touch current rows.

| Category (`#hash`) | Cards |
|---|---|
| Account & workspaces (`#account`) | Sign-in and workspace (`#workspace`), [Workspaces](settings-workspaces.md), Service accounts |
| Agents & safety (`#agents`) | Agent powers (God mode), Rules (trust policy), Your preferences (counts of rules, goals and tastes, linking to Memory → Preferences, where they are edited), [Outward actions](../magician/outward-actions.md#optional-context-controls) (`#outward-actions`, immediate save/apply), [Spending](settings-budgets.md) (`#budgets`: model spend and action limits), Harness credentials, [Terminal grants](plane-terminal-grants.md) |
| Models & engines (`#models`) | Engines (Chat and Agentic flows), [Local models](settings-local-generation.md) with the Processing switch (`GET`/`PUT /settings/privacy`, on device or cloud), Backend config with the [model routing](model-routing-panel.md) link |
| Voice & calls (`#voice`) | Voice and audio profiles, Voice preferences, Text-to-speech voices, Live call voices, WhatsApp, Google Meet |
| Notifications (`#notifications`) | Browser notifications, [Critical alerts](settings-critical-delivery.md) |
| Devices & connection (`#devices`) | Self-hosted connection, Connected devices (each row shows the app version the device last reported, `app_version`) |
| Data & storage (`#data`) | Workspace storage, Storage governance, Notes folder, API mining |
| About (`#about`) | Versions (`#versions`, `lib/settings/VersionsPanel.svelte`): web app (`__APP_VERSION__`), Magician backend, Magician API, Magicutor and Desktop from `GET /health` (`versions` map, falling back to `version` / `api_version`), with Refresh |

A new card goes into the page inside its category's `<section>`, wrapped in
`<div class="settings-item" data-settings-card="<id>">`, and into the index
with a title and keywords; `settingsIndex.test.ts` checks the ids are unique.

**Navigation.** `$lib/settings/SettingsNav.svelte` is a sticky left panel
(heading, search, categories with card counts). Clicking a category scrolls
to it and sets `#<category>`; the highlighted category is the last one whose
top has passed a third of the viewport (the last one at the bottom of the
page). Category, card and in-card ids all deep-link; a deep link stays aligned
while the cards above it load, until the user scrolls.

**Search.** Typing filters the page: a card stays when every query word is in
its title, keywords or rendered text. Non-matching cards and empty categories
are hidden with CSS, so panels stay mounted (no refetch, unsaved edits kept).
Counts follow the matches; Enter scrolls to and highlights the first match,
Esc clears, `/` focuses the field.

**Narrow screens** (below 900px): the panel becomes a sticky top bar — title
and search on one row, categories as a horizontally scrolling chip row that
keeps the active chip in view. Cards fit 375px without horizontal scrolling;
panels with grid roots use `minmax(0, 1fr)` columns so a wide child (a tab
row, a long option) cannot stretch the card.

**Embedded** (`/settings?embed=1`): for the desktop, iOS and Android apps to
show web Settings inside their own Settings. The `reroute` hook
(`src/hooks.ts`, `lib/shell/embeddedSettings.ts`) serves that URL from
`routes/embed/settings` — the same page in `EmbeddedSettingsShell` (toasts,
confirmations, Esc, a live-session check) — so it never loads the `(app)`
layout's shell: no top bar, rail, Attention, voice/media monitors or
shortcuts. It is client-rendered (`ssr = false`), its pane takes the window's
real height (`--embedded-viewport-h`; an Android WebView at wrap-content
height reports viewport units as 0), and its panels load through
`LazyPanel`: the opened category first, the rest straight after
(`panelLoading.ts`). Dev responses through the tunnel are compressed
(`vite-tunnel-compression.ts`). On a phone over the tunnel this took the page
from ~8.5 s to ~1.6 s (all panels ~2.5 s), 2026-10-25.

**In the iOS and Android apps** (`&host=ios|android`): the apps' own Settings
keep only device settings and open this page in a persistent WebView from a
"Magican settings" row. A paired phone's device token cannot open a web
session (`/auth/session` needs a login identity), so the page uses the normal
web sign-in once (`/login?redirectTo=…` keeps `embed`/`host`) and the app's web
storage keeps it. The apps keep `/settings` and `/login` in the WebView and
open other links in the browser.

**In the desktop Settings window** (`&host=desktop&device=Mac|Windows|Linux`,
only inside Tauri): the page shows the window's **This Mac | Magican** tabs;
This Mac calls `plugin:magician-settings|open_desktop_settings` to go back.
Links to other app pages open through
`plugin:magician-attention|open_application_path` (the browser, or the
Attention window for approvals) instead of replacing Settings.

Capture-all disables the outward sending selectors without discarding their choices; browser submission review remains independent.

**When a send is captured** (shown only when the server's settings carry
`review_captured_sends`, and sent back only then): *Ask me to review* pauses
the agent with a Send / Discard / Always allow ask on Attention and the
phones; *Just record it* keeps the record-only behaviour. It stays editable
under capture-all, which still records without asking. **Always allowed**
lists `GET …/outward-actions/grants` (agent → recipient, capability, date)
with Revoke (`DELETE …/grants/{id}`); a 404 hides the list on older servers.
The review ask itself renders in the shared HITL prompt
(`HitlPromptFields`, `escalation_trigger: "outward_review"`) and answers
`{type:"choice", selected_id, other_value:null}`; see
the design.

Service accounts sits below Workspaces in a collapsible card, closed by default
(embedded Settings too); a matching search expands it. A responsive grid shows
one card per endpoint with sign-in status, detail and Connect/Reconnect; sign-in
prompts keep Finish sign-in and Not now in one row. Scoped
[browser sign-in](../magician/browser-signin.md) keeps credentials on the
computer without granting a setup token; statuses refresh on window focus and
after a launch. `unavailable` rows (missing binary or environment) show a grey
chip and their detail, with no Connect. Managed-bot rows (`kind: managed_bot`:
Google Workspace per profile, Telegram, WhatsApp) open an inline connect panel
(`ManagedBotConnect.svelte`) driven by `bot/start|input|state|cancel`
(plan): method
choice, phone first for code and pairing-code logins, then the pending step
(code, a two-step password that is never kept, WhatsApp pairing code with Copy,
QR, or Google's computer-mode viewer with an **Open on computer** fallback),
polling `state` every 2 s while signing in. A connected Google row with
`project_access: true` checks its OAuth client's Cloud project roles
(`bot/project-access`, `GoogleProjectAccess.svelte`) each time the panel opens:
a quiet "Cloud project access OK", or the missing roles with **Grant access**
(when the computer's gcloud account can grant) or copyable commands and a Cloud
console link; **Check again** sends `fresh: true`.
