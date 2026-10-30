# Skillshub Changelog

All notable changes to the installed Magician skill bundle are documented here.

## Unreleased

- agent-browser moves to 0.41.0-Magician.0 on upstream `v0.41.0`. Upstream fixes include: a project config can no longer set the browser executable, args or plugins; origin-checked inspect sockets; PID-checked signals; deny-on-error policy loading; restored storage no longer exposes tokens to pages; and MCP limits that tool arguments cannot override. After `connect`, commands stay on the attached browser, closing a tab no longer ends the session, and a command whose reply was lost fails as `outcome_unknown` instead of being re-sent. `click` now fails on hidden, disabled or off-viewport elements, and an ambiguous stale ref fails instead of picking another element. The Magician patch was rebased: the launch-time download setup is dropped (upstream now handles it), and the pid-last daemon cleanup moved into upstream's `remove_daemon_files`.
- agent-browser 0.38.1-Magician.0 patch: `tab new <url>` right after `connect` navigates the session's only page when it is still the untouched `about:blank` (checked live) instead of opening a second tab beside it, so a task's tab group no longer keeps an empty tab. Once that page has been used, `tab new` opens a real new tab as before. Rebuild with `make rebuild-agent-browser`, install with `make setup-agent-browser`.
- Deep research skills 0.2.4 allow 35-minute runs, retain partial findings, stream/resume Claude turns, and collect unfinished OpenAI runs by `response_id` using `gpt-6.1-sol`.

- Sheets `append` caps `params` and `json` at 4 KB, the runtime's per-argument limit; larger limits made the whole Sheets pack fail validation.

- AgentMail recreates moved webhook registrations and adopts their new signing secret because provider updates cannot change webhook URLs.

- Telegram bot passes `/claim <code>` through to the backend, which records the chat as the owner identity when the code is a live one the owner minted.
- Kapso and AgentMail bots derive their webhook URLs from `MAGICIAN_WEBHOOK_HOST` and register them on start; Kapso adopts the registration's signing secret, keeps it unbuffered and retires other machines' hooks for its number.
- Telegram bot passes `/god` and `/godoff` through to the backend, which toggles per-chat god mode for the verified owner only.
- 0.2.3: Add Shopify Global Catalog with automatic installation-owned public profiles, Zomato MCP setup and Swiggy Scenes with approved paid/free checkout from account-bound cart quotes.

- AgentMail bot moves its existing webhook registration to a changed `AGENTMAIL_WEBHOOK_URL` instead of keeping the old host.
- Gmail bot reuses one Pub/Sub topic per profile instead of leaking a new `gws-gmail-watch-*` topic on every start.
- Gmail bot shows needs sign-in when Google rejects its credentials twice in a row, instead of restarting in a loop.
- Zepto, Swiggy, Zomato and Metabase declare their own sign-in callback modes; Zepto uses localhost, and Swiggy/Zomato use numeric loopback.

- Browser `secure_prompt_fill` guidance covers typed values filling without a second **Use once** and reading `not_filled` vs `uncertain` receipts.
- WhatsApp bot "Link with phone number" now works, with one pairing code per login.
- Shared CLI dependencies are linked into each pack and scope; all installers verify activation paths, invalidate failed/stale readiness records, and check browser core discovery without disturbing sessions.
- Gmail helpers match gws's threading/quoting behavior; Sheets append retains the selected range through the native API. Polymarket uses query-based search with pagination, and scope readiness catches missing media companions even when both link trees lack them.
- `whatsapp-mcp` uses pinned wu-cli 0.3.1 native schemas with governed stdio, file/recipient policy and shared bot IPC; the small `whatsapp` CLI companion adds native polls and retains write/admin commands with approvals.
- `whatsapp` `0.3.1`: CLI actions preserve wu's package imports when the governed launcher is relocated; native polls retain 2–12 distinct options, and sends time out after 300 s. CLI and MCP reuse the bot's IPC connection.
- Gmail bot: re-checks `gws auth status` before asking to sign in again, names both accounts on a mismatch, and a watcher that keeps crashing exits 70 with its reason shown in Bot Control; bot SDK children run in their own process group.

- `goal-workspace-startup` `0.4.0`: carries every working rule itself for whichever agent runs it (Magican, or a goal's owning agent; `workspace-steward` retired): notes written with `files`, the whole proposal in every approval `hint`, named sources read in full. One folder per main goal (sub-goals flat inside, names unique per workspace, moves and renames with `files`, dropped goals to `Archive/Goals/`). The README's `members:` frontmatter lists who may see the workspace; setup proposes it for approval and the skill explains what Magican enforces.
- `goal-workspace-startup` `0.1.0`: new procedure skill, the startup template for [goal workspaces](../docs/features/goal-workspaces.md). It covers the setup interview, goal-note frontmatter, metric and state tasks, the state dashboard, monitors, later changes and visibility. It reads program docs and notes through the file sandbox's read-only roots and never writes them.
- `0.2.2`: signed AgentMail delivery/reply routing, sent-header correlation, conditional calendar writes and confirmed Work hold/release references.
- `anatomy` `0.1.0`: vendor the MIT-licensed Anatomy procedure skill for isometric explainer figures, with provenance recorded alongside it.
- `video-generation-via-veo` `0.2.2`: remove unsupported controls, enforce valid count/resolution combinations, disable automatic paid retries and compose silent output through media editing.
- `image-generation-via-openai` `0.2.0`: GPT Image 2.5 Flare for generation and Sunburst for edits, selectable pinned snapshots, and `xhigh`/`max` quality. Replaces GPT Image 2.
- `image-generation-via-openai` `0.1.0`: GPT Image 2 generation and masked editing with scoped credentials, workspace artifacts and cost receipts; granted to Muse.
- Metabase MCP `0.1.0`: per-workspace endpoints, with scope setup repaired on template install or update.

### Fixed (0.2.1)

- `deep-research-with-claude` defaults to Claude Sonnet 5.5 and uses adaptive thinking with a new `effort` input on models that require it.
- Installing from a custom source root now works.
- Scoped `agent-browser` finds its bundled skills again.
- Scope CLIs survive cleaning, moving the checkout or copying the data root: pinned native executables are stored in the data root and linked relatively.
- Setup links every existing scope; new `link-scopes` and `check-scopes` targets repair and check them.
- Runtime verification checks each scope's CLIs and names the target that rebuilds a missing one.
- `make clean-runtime` keeps bot configs, secrets and workdirs.

### Changed

- **Security:** API-key skills declare the HTTPS hosts they contact (`app_egress`), so in an app an owner-approved key reaches only those hosts. Covers AgentMail, Claude and OpenAI research/search, Google image/video, Kapso, Imgflip, Reddit, Telegram, TinyFish, MiniMax and YouTube search; Metabase uses the operator's configured host.
- Tracked Telegram and Kapso sends require provider receipts and tracked Envoy replies over WhatsApp wait for server acknowledgement; Telegram, Kapso, WhatsApp, AgentMail and Envoy waits time out, keeping uncertain outcomes unknown without resending.
- **Security:** Gmail and AgentMail sender attribution uses one strictly parsed mailbox, refusing ambiguous headers and display-name substitution; AgentMail webhooks must be verified before inbound senders are trusted.
- Envoy sends and replies are bound to the exact recipient and body (exact MIME body for Gmail), use one AgentMail POST per tracked send, and pending receipts are retried while bots run without repeating a send.
- `macos-ui-automation` publishes per-tool read-only hints and revives its driver session when it ends.
- `macos-ui-automation` 0.5.0 and `screen-observation` 0.1.2 match CuaDriver 0.28.2: the daemon starts before every call, calls share one session, stale-snapshot errors ask the caller to re-snapshot, large captures go to files, and documented commands and arguments match the driver. The `mac-operator` template addresses elements by token.

### Fixed

- `@magician/bot-telegram` 0.2.3: a rejected secure-link button no longer drops the whole alert; the card is resent without it.
- `@magician/bot-telegram`: the unit tests run again.
- `@magician/bot-sdk`: bots now open their realtime feed, so critical alerts are delivered.

### Changed

- `browser` 0.5.4: `webmcp`, `pushstate` and `vitals` are actions; command descriptions match the driver's flags.
- **Security:** `@magician/bot-sdk` remembers an open secret ask across restarts, so a password typed in a channel is still refused; the hold ends when superseded or on `/stop`.
- **Security:** `@magician/bot-sdk` ties each delivery claim to a per-process connection, so an orphaned bot cannot send a duplicate credential card or finalize another's delivery.
- `@magician/bot-sdk` 0.3.6, `@magician/bot-telegram` 0.2.2, `@magician/bot-kapso` 0.2.3: critical-request alerts reach the owner's verified channel (see `bots/CHANGELOG.md`).
- **Security:** `browser` 0.5.3: `secure_prompt_fill` can deliver a password or one-time code the run already holds, only to its exact origin and only once; other commands refuse the placeholder, and the next screenshot after a secret fill is withheld.
- **Security:** `@magician/bot-sdk`: channels never ask for a secret; sensitive asks direct the owner to the app and channel replies are refused.
- `macos-ui-automation` 0.4.0: window state returns a compact accessibility tree by default (about 4x fewer tokens), with query, role, line-limit and full-tree options.
- Account setup is declared in skill metadata, so Desktop derives fields, pairing and OAuth for Google Workspace, Telegram, WhatsApp, Swiggy and Zepto without provider-specific code.
- `whatsapp` 0.2.4 publishes a private QR image and connected marker for Desktop setup and no longer logs the raw QR payload; the Presto Gmail OAuth profile is seeded.
- `browser` 0.5.1: `read` returns a content document envelope, so a genuinely opened page counts as claim-eligible evidence.

### Fixed

- `setup-deps` rebuilds only native addons the pinned Node can no longer load after a Node upgrade, fixing the WhatsApp bot's database crashes.
- `media-fetch` tests no longer depend on a locally installed ffmpeg link.
- `work-modules`: its missing adapter is now committed, so the pack works on fresh clones.
- `csvkit` is installed by setup, so its actions work on fresh installs.

---

Older entries: `docs/archive/changelogs/skillshub.md`
