# Skillshub Changelog

All notable changes to the installed Magician skill bundle are documented here.

## Unreleased

- Gmail bot 0.2.4: the business account looped on 'Authenticate' because Pub/Sub refused the watch's cached access token while Gmail accepted the credentials, and a new sign-in does not replace that cache. When Gmail accepts the credentials the bot sets the token cache aside once and retries; a refusal that persists is reported as a failing watch, not a sign-in.
- Gmail bot 0.2.4: a restart no longer leaves a Pub/Sub subscription behind (136 had piled up on four topics). The bot saves the subscription its first watch creates, reuses it with `--subscription`, and renews the Gmail watch itself before each run and daily.
- frontend-slides (new, MIT, instruction-only) adapts zarazhangrui/frontend-slides at a pinned revision: discover a style through three previews, then build a single-file, fixed 16:9 HTML deck. Upstream presets, template, animation and stage CSS are kept unmodified and digest-checked (`UPSTREAM.json`); installers, deployment and the PDF exporter are left out. Muse (`creative-mind`) and `presentation-maker` can activate it.
- browser 0.5.5 tells the agent that a `cdp` session opens its own tab group with one empty `about:blank` tab, that its first call should `open` the target URL (which navigates that tab), and that the owner's other tabs are invisible to it but their sign-ins apply. Agents started with `tab list`, which only created the empty tab, and one run stopped as blocked because "the connected browser exposes only one tab, `about:blank`".
- office-powerpoint 0.3.0 exports a deck to PDF: `view` mode `pdf` runs the tracked `bin/officecli-exporter-pdf`, an OfficeCLI exporter plugin found on the skill's PATH. It prints OfficeCLI's own HTML render with headless Chrome, Chromium or Edge, one page per slide at the slide's size with selectable text, and reports `unsupported_feature` when no browser is installed. OfficeCLI has no PDF exporter of its own outside Windows, so agents fell back to shell, where `officecli` is not on PATH. The skill guide now says so, renders one screenshot per slide (a page range is one scrolled capture), and points slide inspection at `get`/`query` rather than hand-parsing the zip.
- metabase-mcp 0.2.2 names its server `${METABASE_BASE_URL}`, read from each workspace's skill `config/.env` when the skill loads, so its `SKILL.md` is linked into every workspace like any other skill. Per-scope rendering (`render.yaml`, `skill_render.py`) is retired; `link_runtime` relinks a rendered manifest whose content is unchanged and leaves an edited one.
- Tool skills start under the governed runtime again. Python skills declare `python3` and get a generated `bin/python3` exec shim into the shared venv: the old symlink started the bare base interpreter, so htmltotext, openai-image, veo31 and nanobanana2 lost their packages and the MiniMax music/video scripts ran on macOS's Python 3.9 and failed to import. find-details-by-username 0.2.1 declares `maigret`, macos-ui-automation 0.5.1 `cua-driver`.
- web-via-tinyfish 0.1.2, telegram-self 0.2.2, higgsfield 0.2.1 and agentmail-read 0.2.2 start through tracked `bin/*-adapter` entrypoints that exec their CLI in place: the runtime runs an entrypoint from a private copy, which their launchers' sibling imports (and tinyfish's working-folder-relative `command_prefix`) could not survive.
- dugite 0.2.1 starts through `bin/git-adapter`, which sets git's exec and template paths (https remotes and `init` templates work) and signs commits made without a repository identity as `Magician <magician@localhost>` from the tracked `bin/dugite.gitconfig`, git's lowest-precedence layer, instead of a hostname guess.
- The live canaries of websearch-via-claude 0.2.4 and deep-research-with-openai 0.2.5 ask a question that needs a search (the latest stable Rust release): for "What is the Rust programming language?" both models answered from memory and cited no sources.
- structured-web-data 0.2.2 and pdftotext 0.2.1 declare `input_file` as a read-only `workspace_path`. It was a plain string, so the content reader's brokered file was refused ("broker-owned path requires a workspace_path parameter") on every structured-data read. Reader skills with a wrong `input_file` type are now refused when they load.
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
