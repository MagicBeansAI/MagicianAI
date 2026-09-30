# Skillshub Changelog

All notable changes to the installed Magician skill bundle are documented here.

## Unreleased

### Changed

- API-key skills declare the HTTPS hosts their code contacts (`app_egress`), so in an app an owner-ticked key reaches only those hosts: `agentmail-read` / `agentmail-send` 0.2.1 (`api.agentmail.to`), `deep-research-with-claude` 0.2.1 and `websearch-via-claude` 0.2.3 (`api.anthropic.com`), `deep-research-with-openai` 0.2.1 and `websearch-via-openai` 0.2.3 (`api.openai.com`), `image-generation` 0.2.1 and `video-generation-via-veo` 0.2.1 (`generativelanguage.googleapis.com`), `kapso-whatsapp-read` / `kapso-whatsapp-send` 0.2.2 (`api.kapso.ai`), `meme-generation-via-imgflip` 0.2.2 (`api.imgflip.com`), `reddit-search` 0.3.2 (`oauth.reddit.com`, `www.reddit.com`), `telegram` 0.2.2 (`api.telegram.org`) and `web-via-tinyfish` 0.1.1 (`api.search.tinyfish.ai`, `api.fetch.tinyfish.ai`). `metabase` declares none: its host is the operator's `METABASE_BASE_URL`.

- MiniMax skills (web search, image, music, video, image analysis) 0.2.2 declare `app_egress: api.minimax.io`, so in an app their key and network reach only MiniMax's API host.

- `youtube-search` 0.3.1 declares `app_egress: www.youtube.com`, the only host its `yt-dlp` search contacts, so in an app it no longer needs hosts granted or "any public host".

- Require Telegram receipts for the intended chat and bound tracked Telegram sends and AgentMail response-body reads without resending.

- Keep unreceipted or timed-out Kapso replies unknown in Claims Review and stop further chunks without resending.

- Release stalled WhatsApp acknowledgement waits and time out Envoy receipt requests without resending messages.

- Bind Gmail and AgentMail sender attribution to one parsed mailbox, refusing ambiguous headers and display-name address substitution.

- Require WhatsApp server acknowledgement for tracked Envoy replies and verified AgentMail webhooks before trusting inbound senders.

- Keep tracked Envoy sends to one AgentMail POST and prevent Kapso from substituting unrecorded template words.

- `macos-ui-automation`: the controller publishes the driver's `readOnlyHint` per tool and revives its shared CuaDriver session when the driver ends it.

- Retain and retry pending Envoy receipts while bots are running, with bounded recovery and no repeated sends.

- Harden Envoy dispatch retries and recipient binding, including exact-body Gmail replies.

- `macos-ui-automation` 0.5.0 and `screen-observation` 0.1.2 match the pinned
  CuaDriver 0.28.2, after an audit of every call against the driver's schemas.
  - The controller starts the daemon before every call (0.28 has no daemon-less
    mode, so `check_permissions` / `get_config` / `set_config` failed on a cold
    host) and adds a shared `session` label (`magician`, or
    `MAGICIAN_CUA_SESSION`) to the 37 tools that take one. Each call is its own
    transport, so without a label every call ran in a fresh session and the
    Tutor/Copilot cursor settings were gone by the next action.
  - The "No cached AX state" refresh-and-retry is gone: a refresh mints a new
    `snapshot_id`, so resending the old address always failed closed after an AX
    walk of up to 20 s. `snapshot_id_required`, `stale_element_token` and
    `No cached AX state` now tell the caller to re-snapshot and re-address.
  - `get_desktop_state` (~1.8 MB of inline base64 on a Retina display) and
    `zoom` captures go to files like window snapshots and the reply names
    `screenshot_file`. Four identical copies of `call_cua` (a merge artifact)
    are one again.
  - The skill: `--double` (not a flag) is `double_click`; cursor motion keys
    are snake_case (camelCase is silently ignored); `type_text` / `set_value` /
    the osascript combo carry `snapshot_id`; a key or text that does not land in
    the background is resent with `"delivery_mode":"foreground"` before
    `bring_to_front` or osascript; the grant path is `cua-driver permissions
    grant` and the check `permissions status`; `invoke_menu`, `verify_state`,
    `get_desktop_state` and the `clipboard_write` + `cmd+v` paste path are
    documented; `scroll` / `drag` / `press_key` list their real arguments.
    `screenshot`, `set_recording` and `set_agent_cursor_style` do not exist in
    0.28, so captures use a capture-only `get_window_state`, recording is
    `start_recording` / `stop_recording`, and the cursor look is
    `set_agent_cursor_theme`.
  - `screen-observation`: the one-window capture is a capture-only
    `get_window_state`; the tree-only read uses `include_screenshot:false`
    (the deprecated `capture_mode:"ax"` still grabbed a screenshot).
  - The `mac-operator` template (and its live copy) addresses elements by
    `element_token`; the desktop grounding eval quotes its JSON arguments
    (unquoted, the shell brace-expanded them) and captures once.

### Fixed

- `@magician/bot-telegram` 0.2.3 (secure HITL P5): a link origin Telegram
  refuses no longer loses the whole alert. The secure-request link is attached
  as an inline keyboard button; Telegram validates that URL and rejects a
  non-public one (`Bad Request: inline keyboard button URL '…' is invalid:
  Wrong HTTP URL`), which threw and failed the delivery — so the owner was told
  nothing at all because a decoration could not be drawn. The card's own text
  already carries the same link, so that one refusal now retries without the
  button and logs why; `chat not found`, `bot was blocked`, `429` and transport
  errors still throw, so the runtime keeps reporting the provider's real answer
  (`isRejectedButtonUrl`, covered in `test/callback-data.test.mjs`).
- `@magician/bot-telegram`: the bot's unit tests could never run. They import
  `../dist/callback-data.js`, but the bundler emitted only `dist/index.js` from
  `src/index.ts`, which exports nothing — so the whole test file failed to
  resolve. The build now also emits the pure-helpers module, and both tests pass.

- `@magician/bot-sdk` (secure HITL P5): a bot never opened its realtime feed,
  so no channel alert was ever claimed. Two causes, both silent. The client
  offered only the bearer subprotocol, while the runtime negotiates
  `magician-events-v2` and echoes the protocol it selects — with no overlap it
  selects none, omits `Sec-WebSocket-Protocol` from the `101`, and a strict
  client must reject the handshake. And `ChannelRuntime.start()` opened the
  feed after awaiting the adapter, which for a long-polling adapter never
  returns (Telegraf's `launch()` awaits its polling loop), leaving that line
  unreachable. The feed now opens before the adapter, offering both protocols.
  A live Telegram bot reaches `provider_accepted` with a provider message id.

### Changed

- `browser` 0.5.4: `webmcp`, `pushstate`, and `vitals` are actions. Snapshot, screenshot, tab, record, auth login, and network descriptions name the flags the driver has.
- `@magician/bot-sdk` (secure HITL, deep-review round 2): the refusal that
  keeps a secret out of a channel transcript no longer lives only in one
  process's memory. On every inbound message the runtime re-reads the session's
  own last escalation row and re-arms the hold from it, so a restart or a
  redeploy can no longer forget an open credential ask and accept the owner's
  password as an ordinary message. A RECOVERED hold is armed only while the
  ask's own collection window is still open and expires with it — never with the
  just-delivered fallback, which would have restarted a fresh fifteen minutes
  from the same stale row on every message. The hold is also LIFTED when it should be:
  an ordinary question delivered afterwards supersedes it, and `/stop` clears
  it — an agentic resolution produces no `escalation_resolved` row, so the hold
  used to survive until the ask's own window ran out (up to a day) and refuse
  every message in between.
- `@magician/bot-sdk` (secure HITL, deep-review round 1): a connection
  generation now carries a per-process token, because the counter alone was
  not an identity — every process of a channel started at the same number, so
  an orphaned bot beside its replacement claimed under the identical
  generation and the owner could get the same credential card twice. A report
  echoes the generation its claim was made with, and the backend accepts a
  report only from the connection that claimed: an orphan can no longer take a
  delivery terminal for a send its replacement is still making.
- `@magician/bot-sdk` 0.3.6, `@magician/bot-telegram` 0.2.2,
  `@magician/bot-kapso` 0.2.3 (secure HITL P5): critical-request alerts.
  `ChannelRuntime` opens the realtime feed at start and reconnects it with
  backoff (counting connection generations); a `CriticalRequestAlert` for the
  runtime's channel type is claimed over the authenticated API — the feed
  carries no address; the claim returns the owner's address for that one
  delivery and binds it to this bot and socket — the value-free card is sent
  (`adapter.sendCriticalAlert`, else `sendText`) and the provider's answer
  reported; `CriticalRequestRetired` edits (Telegram) or annotates (Kapso)
  a sent card. Telegram sends an inline **Open secure request** button;
  Kapso sends a CTA-URL message inside the 24-hour window and the fallback
  template outside it, failing honestly without one. Details in
  `skillshub/bots/CHANGELOG.md`.
- `browser` 0.5.3 (secure HITL P4): `secure_prompt_fill` also delivers
  material the run already holds — a `fields[].value` set exactly to the
  placeholder the runtime gave for a `need_user_input` password or `otp`
  answer. The fill is the only sink for such a placeholder in the browser
  lane: `fill`/`type`/`batch` commands and CLI arguments refuse it. A code is
  reserved for the exact origin, spent as the fill starts and never filled
  twice (`material_unavailable` means ask for a fresh one); the **Use once**
  confirmation is already given when the code was collected for that origin.
  After a fill delivered a secret the next `screenshot` is withheld until the
  page moves on.
- `@magician/bot-sdk` (secure HITL P3): a channel never solicits a secret.
  `ChannelRuntime` relays a notice instead of the question for an escalation
  whose `input_schema.sensitive` is set, whose `input_type` is `password` or
  `otp`, or whose `request_type` is a built-in secure browser ask — it names
  what is needed and says to enter it in the Magician app or web UI, never
  here — and while the ask is open a reply on the channel is refused, not
  forwarded. `EscalationContent` gained `input_type` / `input_schema`;
  `sensitiveAskNotice`, `sensitiveAskExpiry`, `SENSITIVE_REPLY_REFUSAL` are
  exported. Details in `bots/CHANGELOG.md`.
- `macos-ui-automation` 0.4.0: `get_window_state` returns a compact AX tree by
  default (`tree_view: "labelled"`): unlabeled containers and the action suffix
  every node carries are dropped, static text that repeats its parent's title
  is folded, indent is one space per level. A WorkFlowy window went from 734
  lines / ~17k tokens to ~330 lines / ~4k — one tool-result page instead of
  four `read_result` decisions per look, which is where a magician-engine CUA
  run spent a third of its steps (and why models started dumping the JSON to
  /tmp and grepping it). Element indices are the driver's own. View args on the
  same call, consumed by the controller and never sent to the driver:
  `"query":"inbox"` (matches plus children, ~70 tokens), `"roles":[...]`,
  `"max_lines":N`, `"filter":"full"` for the raw tree. Tested in
  `tests/test_tree_compaction.py`.
- Account setup is now declared by `metadata.magician.setup`. Google Workspace,
  Telegram, WhatsApp, Swiggy MCP, and Zepto MCP skills bind to shared setup
  definitions, so Desktop derives fields, pairing, and governed OAuth behavior
  without provider-name branches.
- `whatsapp` 0.2.4 now publishes a private QR PNG and a separate
  connected marker for supervisor-owned Desktop setup, removes both when they
  become stale, and no longer writes the raw QR payload to logs. The bot config
  seed also includes the fixed `gmail-presto` OAuth profile so installed Presto
  Google Workspace skills can use the same managed account flow.
- `browser` 0.5.1: a `read` returns a content document envelope (`document.text`, bounded `excerpt`, `fetch_status`, `evidence_role`, `claim_eligible`), so a genuinely opened page is claim-eligible evidence under the same rules as `content_read` and a degraded shell is `discovery_only`.

### Fixed

- `setup-deps` rebuilds native addons when the pinned Node's ABI changes.
  `.node/` was swapped from Node 22 to 24 on 2026-08-26, but the deps stamp
  keys on the lockfile (with `.node/bin/node` order-only), so nothing
  recompiled and `better-sqlite3` — loaded unbundled by the `whatsapp` bot
  through `@ibrahimwithi/wu-cli` — kept its ABI-127 binary; every database
  call in the bot died with `ERR_DLOPEN_FAILED` / `NODE_MODULE_VERSION 127
  vs 137`. A new `node_modules/.node-abi` stamp, redone whenever the runtime
  or the installed tree changes, runs
  `scripts/native_addons_needing_rebuild.mjs` — `process.dlopen` of every
  installed `*.node`, the exact check `require` performs — and
  `npm rebuild`s only the packages the runtime cannot load (N-API addons
  such as `fsevents`/`sharp` load across majors and are never touched; a
  blanket `npm rebuild` would re-download the git, agentmail and gws binaries
  on every bump). `setup-node` now prints the follow-up when a tree exists.
- `media-fetch`: `test_ffmpeg_location_prefers_pack_local_bin` asserted that
  `_ffmpeg_location()` ends in `media-fetch/bin`, which only holds where
  `media-fetch/bin/ffmpeg` exists. That file is a symlink to the host's ffmpeg,
  written by `make -C skillshub setup-media-fetch` and gitignored, and
  `make test-content-retrieval-eval-harness` has no prerequisites — so the test
  measured whether the machine had run setup, not whether the resolver prefers
  the pack. It failed on a second machine. The resolver logic is now driven
  against a temporary pack dir with `shutil.which` patched, covering the
  preference, the PATH fallback and the `None` case; a `skipUnless` test still
  checks the real symlink where it is installed. This suite already said it
  "deliberately never depends on" a real binary — now it doesn't. Verified with
  the symlinks deleted: 130 pass either way.
- `work-modules`: the pack's adapter, `bin/work-modules`, had never been
  committed. `.gitignore` ignores `skillshub/*/bin/` wholesale and re-admits
  each adapter source by name; this one was missed when the pack was added to
  the classification manifest on 2026-09-02, so it existed only on the machine
  that wrote it and every action was dead on any other clone. Allowlisted and
  committed.

  Both are the same defect in different clothes: **a test must never assert on
  a gitignored artifact without ensuring it exists.** Drive the logic
  hermetically, gate the assertion on the artifact's presence, or make the
  target depend on the setup that creates it.

- `csvkit`: the pack declared 13 console scripts (`in2csv`, `csvstat`,
  `csvsql`, `sql2csv`, …) in `requires.bins` and built 26 actions on them,
  but had no setup path of any kind — no Make target, no entry in
  `requirements.txt`, no symlink. The binaries resolved only from whatever
  Python happened to be on the host PATH; on the machine where this was
  found, an ad-hoc `/Library/Frameworks/Python.framework/Versions/3.11/`.
  The pack therefore worked by accident there and every action was dead on a
  fresh install. It was the only CLI pack in skillshub in that state.
  `csvkit` is now owned in `ADAPTER_PACKAGES`, so `setup-python` installs it
  into `.venv` and the dispatcher's existing `skillshub/.venv/bin` PATH
  prefix resolves all thirteen with no per-binary wiring. Verified with the
  host Python hidden from PATH.


### Added

- `media-fetch` `0.1.0`: `metadata`/`transcript`/`audio`/`video` actions over
  1,747 yt-dlp extractors (not YouTube-only), keyless, backed by `yt-dlp`
  plus pack-local `ffmpeg`/`ffprobe` symlinks for `video`'s merge step.
  Measured failure modes drove the design, and every action requires a
  positive artifact signal because of them:
  - yt-dlp exits 0 in every failure case this pack cares about: an empty
    extraction, a captionless source (writes nothing), `-f "bv*+ba"`
    without ffmpeg (leaves two unmerged streams on disk), and a
    `--max-filesize` abort. None of these can be detected from the exit
    code alone.
  - Raw `json3` caption payloads are far larger than the text they carry:
    measured on a 14-hour video, 10,275,361 bytes of `json3` (word-level
    timing metadata) flatten to 470,326 characters of actual text — a 21x
    overhead. `transcript` flattens to timestamped ~30s paragraphs on disk
    instead of returning the raw payload.
  - The obvious workaround for the merge requirement is closed: requesting
    pre-muxed format `18` to sidestep `ffmpeg` entirely now returns
    `HTTP Error 403: Forbidden` from YouTube, so `video` has no escape
    hatch and must merge for real.
  - `audio`/`video` verified live: a 252,182-byte `.webm` (native, no
    re-encode) and a 475,957-byte `.mp4` that `ffprobe` confirms carries
    both a video and an audio stream (a real merge, not two files renamed).

---

Older entries: `docs/archive/changelogs/skillshub.md`
