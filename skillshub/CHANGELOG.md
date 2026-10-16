# Skillshub Changelog

All notable changes to the installed Magician skill bundle are documented here.

## Unreleased

- `0.2.2`: signed AgentMail delivery/reply routing, sent-header correlation, conditional calendar writes and confirmed Work hold/release references.
- `anatomy` `0.1.0`: vendor the MIT-licensed Anatomy procedure skill for isometric explainer figures, with provenance recorded alongside it.
- `video-generation-via-veo` `0.2.2`: remove unsupported controls, enforce valid count/resolution combinations, disable automatic paid retries and compose silent output through media editing.
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
