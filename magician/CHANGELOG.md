# Changelog

All notable changes to the Magician project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---
## [Unreleased]

_Current development version: `0.7.89`._

- Gate destructive memory answers on reviewed evidence and record bounded, source-checked reference samples separately from production.

- Add text-only `chat-sarvam-adaptive` for Indic-language chat, with adaptive reasoning and locality safeguards.
- Route active GPT-6 Sol profiles to GPT-6.1 Sol, preserving saved aliases and updating live pricing.

_Current development version: `0.7.88`._

- Follow live processing locality for Decision Model routing and dispatch gated memory decisions with scoped receipts.

- Honor Decision Engine background queue allowances within memory owners’ existing total deadlines; read local Decision Model costs without rejecting zero-cost ledger rows.

- The meeting bot joins, listens, and speaks on Linux through PulseAudio (or PipeWire's Pulse layer): a virtual microphone null-sink, browser-output capture, and Xvfb when `DISPLAY` is unset. macOS stays on ScreenCaptureKit and BlackHole.
- Linux meeting audio puts the previous default sink back after creating the null-sinks, returns moved browser streams on leave, keeps the passive microphone off the virtual mic, and does not replace an installed PulseAudio daemon.
- Leaving one Linux meeting no longer unroutes another live attendee, and leave does not put the desktop microphone back on the virtual mic.

- Honor configured background decision budgets while reserving prose time; refine three memory packs to recover lifecycle, evidence and connection workflows without lowering confidence gates.
- Route memory classifications through Decision Engine v4 with scoped receipts, guarded text stages, operator-enabled gates and no gated LLM fallback; preserve execution lineage and refresh policy for sparse background jobs.

- App keys: a `*` tool gets no network when no app grant was resolved; a grant stored before per-key scopes still loads, and its unscoped key of a host-less tool stays unused until re-reviewed; tests cover two keys with different scopes and the full re-approval cycle through the lock and dispatch path.

- Notes: a file change outside the app refreshes the search index after the folder has been still for about a second.

- Notes: the library reads and writes the notes folder directly. Creating, saving, and deleting a note refreshes the on-disk search index. The notes server is not started.

- Bind interrupted transcript imports to their original metadata and reject conflicting candidate batches before publishing partial review queues.

- Prevent transcript retries from requeuing decided statements by changing their catalogue claim references.

- Expose the exact saved command for interrupted claim confirmations so owner review can resume after browser-state loss.

_Current development version: `0.7.87`._

- App jail security review: "any public host" must be requested by the app (`external_egress: any_public_host`) and survives only when every authority ceiling allows it; in-place runtime roots never expose home dot-directories (only recognised versioned runtimes, else the file's own directory), exclude `etc`/`var`, and refuse anything below another skill; the fingerprint covers package-local `node_modules`/`tests`/`fixtures` and a stat cache keyed by ctime and device; `PYTHONPYCACHEPREFIX` keeps stale `.pyc` out; boot sweeps leftover credential directories. Keys: declared tools reach declared ∩ granted hosts; undeclared tools' keys are limited to owner-picked hosts, or "any site" only by explicit opt-in; `destinations: ["*"]` marks an any-host skill; vault grants carry the exact domain scope (MagicVault `RequestedDomains`).

- MagicVault pinned to `5849709` (magicvault-core 0.1.6): `RequestedDomains` sets and Any for secret domain scoping; `Secret` batch expectations no longer redeem delegated grants.

- App jail: in-place skills (`app_in_place_skill_v1`). Any skill that is not one native file, one exact python3 script or a system tool runs in place in MagicRun's exec-roots jail, with roots derived from its package (less `config/`), the shared `node_modules` and its runtimes' install prefixes; data, home and other skills' directories are forbidden. The lock binds the exec-roots profile identity and a package fingerprint; a changed skill is refused until re-approved. `app_egress` accepts a `destinations` list; apps can grant several hosts or the explicit "any public host"; the broker receipt lists every host contacted. Keys can be delivered as an `MMX_CONFIG_DIR` config file in a private per-call directory.

- App jail: MagicRun pinned to 0.1.81 (declared exec roots; boot runs `sweep_stale_jail_members()` and logs the counts; a macOS teardown that cannot prove every jailed process dead fails the call as effect-uncertain).

- App jail: MagicRun pinned to 0.1.80 (the macOS descriptor listing uses an 8 KiB stack buffer).

- App jail: MagicRun pinned to 0.1.79 — a jailed skill no longer inherits Magician's open descriptors (every host descriptor above stdio is marked close-on-exec at launch, on macOS and Linux).

- App jail: pure-data skills — trusted system tools (`jq`) run in place, pinned by digest; read-only file inputs are passed as content and staged into the jail's private directory (`stage_input_file`); declared stream limits clamp to the app budget for every jailed skill. Linux runs require bubblewrap and the MagicRun jail helper (Dockerfile). MagicRun pinned to 0.1.78.

- App jail: API-key skills use an owner-ticked vault secret, injected only as an environment variable into a skill with a declared egress host, through the sealed credential adapter; ungranted required secrets refuse the call.

- App jail: a declared-host web skill reaches only its host through a per-call egress broker; reviewed Python 3 skills run under the host's pinned interpreter; skills without stdin lower correctly; MagicRun pinned to 0.1.77 (fixes jailed children killed by the process watchdog).

- Chat reference catalog ~57 ms instead of ~5 s: skill discovery caches each `SKILL.md` parse by size and mtime.
- A config reload reaches the LLM dispatch queue without a restart.
- Decision-loop caching: the stable prompt leads the conversation, stateless providers send only changed sections, snapshots show the screen without their envelope, OpenAI full sends keep one routing key and a tool load warms the cache (Opus 5.5 CUA runs: cost-equivalent tokens 513k → 249k).
- Jev never takes a read-only driver tool or repeats the step just taken; the snapshot after a desktop action uses that action's CuaDriver session.
- Pi runs Anthropic profiles (API-root base URL, adaptive thinking); harness usage reports cached and total tokens.
- Resumed runs number tool-result and synthetic ids globally, so results after a pause are no longer rejected as duplicates.

- Refuse changed transcript source context on retries and serialize concurrent imports before recording candidates.

- Bind Envoy dispatch to a durable attempt, exact text/recipient and complete claim capture; retain tracking across configuration changes.

- Connect controlled Envoy replies to Claims Review with exact payload capture, channel-scoped acceptance receipts, and separate human confirmation.

- Keep the shared-decision planner's task-local wrappers small to prevent debug worker stack overflow during native model dispatch.

- Keep stalled work unfinished, reduce duplicate planner context, fix Grok/Agy proposal handling and cache metering, and notify scoped provider failures through HITL.

- Extend shared Decision Engine decisions to direct chat and add a reloadable All Engines / Magician Only / Off policy.
- Preserve Decision Engine planner and final-reply instructions in Codex App Server turns and constrain Grok planner replies with its CLI JSON schema.
- Fix Agy planner arguments/replies, accept harmless Grok plan explanation metadata, and make empty harness failures visible in chat.
- Keep native provider dispatch on the heap to prevent debug worker stack overflow during Decision Engine planner fallback.
- Record planner calls with a valid bootstrap trace mode so cost and usage records are retained.
- Align autonomous catalog argument projection with dispatch policy so ordinary browser calls remain usable through the shared rail.

_Current development version: `0.7.79`._

- Route tool decisions through the shared Decision Engine across agentic harnesses, replace the surface-specific judges, and use Jev thresholds of 0.7.

- Authenticate reviewed app-page assets with scoped live sessions while preserving bearer and Cloudflare Access checks for other requests.

- Apps can read the owner's memory only as the owner grants it, per app and separately for interactive and background runs, through the new read-only `memory_data` binder.

- Apps can list and read the owner's tasks (`tasks_data`) and search and read notes (`notes_data`) through two new read-only, scope-bound host-read binders.

- Claude Code, Antigravity and Codex now qualify for VibeDev: Claude's built-in plugins are accepted, Agy 1.2's token path is read, and failed qualification probes log their reason once.
- Grok 1.0.40's launch flags are fixed; Grok stays blocked for coding because it always offers its MCP gateway tools, and the reason now says so.
- Saved agents with every-N-hours crons pass preflight again, and blocked app tools show their real blocker.

### 2026-09-25 — 0.7.73 — Apps macOS actions fence their target

- The Apps macOS owner fences each action on what it touches: an element action's permit digest covers the target element plus its ancestors' indexes and roles (a drag both elements), a key press the window and its child roles, computed from the observation's raw tree kept for its 30-second life. macOS retitling an untitled document after an edit no longer refuses an unrelated action, a menu target fails closed before a permit is issued, and the observation's menu bar never enters a fence.

### 2026-09-25 — 0.7.72 — CuaDriver 0.28.2

- The agent loop and the Apps macOS owner follow CuaDriver 0.28.2: desktop snapshots tell the model to address controls by `element_token` (or `snapshot_id` + `element_index`; a bare index is refused), the look/change verb lists drop `screenshot` and cover `get_desktop_state`, `get_browser_state`, `page`, `replay_trajectory` and the `browser_*` tools, and Apps observations are read from the structured `elements[]` (the 0.1.9 `[element_index N]` tree markers no longer exist) with each element's token kept for wire v2.

### 2026-09-25 — 0.7.71 — Voice follows the composer's engine

- A voice call thinks with its client's composer engine, and a changed server chat engine reaches every open composer (`chat.engine.updated`). Voice chat turns take the call's `chat_choice`; `install_chat_harness_snapshot` announces engine changes.

### 2026-09-25 — 0.7.70 — Runs keep the engine they launched with

- Each run pins its engine, harness model, and Pi profile at launch (`engine_pin.json`), so a Settings switch never moves a running, paused, or recovered run; children, tasks, and coding tasks launched by a chat or run inherit its engine unless one is named, and native sessions resume only under the same engine fingerprint.

### 2026-09-25 — 0.7.69 — Delete a workspace with its data

- `DELETE /workspaces/{id}?purge=true` accepts a workspace that still holds
  data and answers `202 scheduled_for_next_start`: the registry row goes at
  once (the ownership gate refuses the scope from that moment), database rows
  are retired, and the id is queued in `workspaces.json`'s `pending_purge`.
  `workspace_registry::drain_pending_purges` removes queued directories at
  server start — server path only, after the runtime lease is held, before any
  store discovers scopes. Not on the spot: a dozen per-scope caches hold
  handles with no eviction path, and the LLM trace journal recreates a deleted
  directory mid-sequence, which the next boot refuses to load.
- The default workspace is never purged; a queued id that is invalid or has
  been registered again is skipped, not obeyed.
- `POST /workspaces` refuses a slug still waiting to be purged with its own
  `409 workspace_pending_purge`, so a caller can tell it apart from
  `workspace_has_live_state`.
- Both Task Recipes harnesses purge earlier harness workspaces at start-up and
  a passed run purges its own, leaving at most one; the purge script now also
  matches a bare `recipes-eval`.

- Persist a bounded journal index so startup avoids replaying the full LLM trace history. Add automatic governed database compaction and publish maintenance status.
- Parallelize independent initialization, own Ollama warm-up in the background, and admit heavy initial backfills one at a time after HTTP readiness.

- **Pi as a replaceable runtime harness.** The pinned 0.87.1 RPC driver now
  serves chat and agentic execution, maps eligible Magician profiles and
  credentials into isolated Pi sessions, and exposes governed Magician tools
  through the bundled Pi Plane extension. Agentic Settings accepts a Pi
  profile; background operations can inherit Pi from the driving flow.
- **Review fixes.** Tighten harness grants and continuation cleanup,
  verification-code custody and redaction, and failures in the execution and
  channel-assist paths. The local Pi chat probe and agentic evaluation pass.

- **Pi coding engine pinned to 0.87.1 (was 0.83.0).** 0.83's catalogue
  lacked GPT-6 Sol/Luna/Astra, Opus 5.5, Fable 5.1 and Grok 4.7, so Pi ran
  them as clones of GPT-5.5 / Opus 4.8 / Grok 4.5 — the older model's context
  window, output cap, thinking map and cost. Recording the same RPC session on
  both versions showed one wire change Magician depends on: `message_update`
  moved its cumulative usage to a top-level `usage` (0.84 dropped
  `assistantMessageEvent.partial`), which the event parser now reads. A real
  0.87.1 session is checked in as a fixture and replayed through the
  production parser and turn collector. Deploy together: the exact-version
  gate refuses any other Pi, so `make setup-pi-coding-agent` must run with
  the new binary, not before it.
- **Grok 4.7 is a coding profile.** `coding-grok47` runs Pi on
  `grok47-responses-vision-toolsany-rhigh` (xAI Responses, vision, high
  reasoning, `XAI_API_KEY`). `coding-balanced` is
  relabelled "GPT-6 Sol coding · 64k output" so it no longer shares
  `coding-premium`'s label. magicllm's live xAI suite now also proves one
  streamed request carrying an image, a tool and high reasoning (the model
  reads the pixels and answers through the tool) and that a text answer
  streams token by token.
- **No agent pins a model; the Crew page pins one when you want it.** Every
  shipped agent's `llm_routing` is removed (templates and workspace copies,
  versions bumped): coder agents had pinned every operation — including each
  agentic decision — to Terra/Sol/GPT-5.5, and a pin outranks both the global
  routing and the harness engine, so on codex/claude_code those agents kept
  their side-calls on the OpenAI API while every other agent followed the
  harness. `/crew/[id]` → Overview gains a **Models** panel
  (`AgentModelPinsPanel`) that pins or clears each lane and the coding-engine
  model with one merge patch. Agent writes (PUT/PATCH) now refuse a pin that
  names a profile the runtime config does not define
  (`422 unknown_pinned_profile`); only names a write introduces are checked.

- **GPT-6 Sol replaces GPT-5.6 Terra wherever Terra was a routed profile.**
  Sol costs the same on input ($2.00 / $0.20 cached / $2.50 cache write per
  1M) and less on output ($10 vs $12; $15 vs $18 above 272k), and is the
  newer tier. Router seeds gain Sol twins of the six referenced Terra shapes
  (`gpt6sol-responses-toolsany`, `…-toolsany-rhigh`, `…-toolsnone-out16k`,
  `…-vision-toolsany-rhigh-out64k` — now `agentic_decision` — `…-vision-toolsany-rnone-out16k`,
  `…-vision-toolsany-rdefault`); every operation route, fallback, legacy
  alias, `default_profile`, the `coding-balanced` entry, the
  `chat-openai-adaptive-normal` tier, six coder/analyst agent templates and
  the brainstorm lane's bounded fallback now name Sol. The Terra profiles
  stay defined for manual pinning; nothing routes to them.

---

Older entries: `docs/archive/changelogs/magician.md`
