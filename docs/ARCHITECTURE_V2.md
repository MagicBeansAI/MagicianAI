# Runtime Architecture (No Magictunnel)

## Overview

Magician owns Decision Engine participation through `decision.mode` (All Engines,
Magician Only, Off), exposed by the owner-only `PUT /api/magician/v2/plane/decision-mode`
route and applied before managed chat/run socket discovery; model and action
selection remain inside the Decision Engine. See [the shared rail](components/magician/structured-decision.md#opt-out-and-failure-visibility).

### Scripted app page credentials

The binary composition root registers the app API's shared scripted-session
verifier at the bearer gate. Canonical asset GETs may present the live session
in their path because sandboxed frames cannot send login headers. Verification
produces a request-local file-serving proof, never general API authority; the
asset kernel checks installation and package/surface/grant bindings before
serving. Cloudflare Access remains enforced. See
[custom surfaces](components/magician/custom-surfaces-v1.md#browser-asset-authentication)
and `make test-app-scripted-surfaces` for the boundary and regression lane.

### One-time browser credential delivery (2026-09-16)

The built-in browser action `secure_prompt_fill` uses Magician's authenticated
HITL UI, a private zeroizing in-process response channel, and MagicVault's
published `magicvault-effect`/`magicvault-protocol` adapter at revision
`a22c744d565b459dfeaae6d2b15223dc8af583ea`. It does not connect to the standalone
MCP service or enroll credentials. Public responses and request history retain
only metadata/status. Magicutor's reversible scope alias preserves existing
tab ownership when its session IDs contain underscores. The MagicRun source
and pinned version remain unchanged.

See [one-time browser credentials](components/magician/jit-browser-credentials.md)

Secure HITL credentials (P1 of the plan): every `UserRequest` carries a
server-owned sensitivity spec classified once at acceptance; sensitive answers
enter service custody before history, events, or the ordinary response and are
taken by reference in-process; the web and plane responders route by that spec,
and an agentic resume records the collection-time sensitivity beside each
resolved input so rendering and vaulting never re-derive it from a name. P3
publishes that spec on every announcement (`hitl.requested`
`input_schema.sensitive`, for the request service and for an agentic pause
decided at pause time), vaults a flagged answer before it is retained (one-time
custody for a code, a bounded ephemeral entry for a password; the run keeps a
`[REF:…]` placeholder), holds a chat answer for the turn and seeds pack
sub-runs with placeholders so the model never sees the value, adds the typed
`otp` ask, and has every client (web, iOS, Android) mask by the published
spec while bot channels relay a notice instead of the question. P4 lowers a
reference into a value only inside an approved sink (an HTTP header or body,
a process's stdin, the typed browser fill, a declared pack credential input)
and refuses it everywhere else; a one-time code is reserved with a claim
naming its operation, destination and challenge immediately before dispatch
and consumed as the submission starts, never replayed after an ambiguous
outcome; typed challenges (an HTTP `401` with `WWW-Authenticate` from the
request's host, a governed program's declared login prompt) bind the next
secure ask and the material it becomes to their destination; every delivered
value scrubs every later observation of the run. P5 adds the delivery
coordinator (`hitl_delivery`): a critical request — one with the spec or a
deadline — is projected onto the owner's verified private destinations
(registered push, and the Kapso/Telegram bots enabled in
`hitl.critical_delivery`, addresses from `envoy.owner_identities`) as a
value-free card with the exact request's link; bots claim a delivery over
the authenticated API rather than receiving addresses on the feed; one
record per destination tracks queued, claimed, accepted and every failure
state; rechecking retries, quiet hours, staged fallback and one dedup with
the origin relay live in that one place, and a restart RE-DRIVES the rows a
previous run left live (checked against each request first) instead of closing
them — no task survives a process, but the owner's unanswered question does. P6 adds the verification-code
resolver (`verification_codes`): a pending `otp` ask is the challenge; the
sources the owner granted the `verification_codes` purpose (an Observe email
account, the local Messages store, an AgentMail inbox, a permitted Android
phone) are watched inside a bounded window anchored on the server's
challenge start; a code extracted deterministically from a message whose
receive time, sender domain and provider authentication match answers the
ask through its own path — first response wins, custody at accept, the model
sees status; the Android companion answers the ask itself over its paired
credential and returns status only. See
`docs/components/magician/hitl-attention.md`,
`docs/components/magician/critical-request-delivery.md`,
`docs/components/magician/verification-code-retrieval.md` and
`docs/archive/components/magician/secure-hitl-coverage-matrix.md`.
for target binding, confirmation, cancellation, logging constraints, supported
transports, validation, and the coordinated runtime/Magicutor/UI/skill rollout.

### Extracted library ownership (2026-09-06)

MagicRun owns `tool-runtime-core`; MagicVault owns `magicvault-core` and
`magicvault-primitives`. Root workspace dependencies and `Cargo.lock` pin their
exact Git revisions. `magician`, `magician-api`, `magician-bin`, and
`magician-mcp-client` inherit the same runtime pin. `magician` consumes custody
through its existing `magician_v2::secrets` facade, while `magician-core`
re-exports neutral JSON/durable primitives. No library depends back on Magician.

OS-keychain identities, app-data/signing keys, scope path rules, bootstrap,
brokers, action/result traversal, runtime audit projection, and execution owners
remain product-owned. `SecretScopeLayout` supplies the existing path mapping;
the shared resolver retains one canonical store per scope. `AuditReceipt` keeps
the product's typed receipt outside core without converting it to arbitrary JSON.
App attestations read actual upstream source bytes, not sibling paths or frozen
digests. Source-bound approvals retain their normal re-review requirements.

The 2026-09-09 dependency update pins MagicRun `tool-runtime-core` `0.1.74` at
`af348ab566cbf495f59d155a328bf2cac6afa09d`. Non-jailed macOS batch launches use
native `posix_spawn` (macOS 10.15+); jailed, PTY and non-macOS backends retain
their previous behavior. The public coordinator API and dependency requirements
are unchanged, but the runtime source fingerprint changes, including the new
macOS backend in the existing batch-source attestation. Source-bound approvals
must follow their normal re-review path; this update does not bypass admission.
The 2026-09-21 re-pin to `0c5b9c4559395fcfc3dc1406771d9200c99d1c63`
also makes the production library compile on Windows by keeping its Unix-only
declared-artifact collector out of non-Unix builds. Artifact authority remains
fail-closed on Windows. The manual MagicRun qualification workflow now includes
a Windows production compile lane.
MagicVault core `0.1.3` and primitives `0.1.1` remain at the existing `05acd8f`
pin because their source trees are unchanged in the newer standalone releases.
The `[patch]` sections that linked the sibling worktrees `../MagicVault-secure-hitl` and `../MagicRun-secure-hitl` are gone as of 2026-09-23, replaced by literal revisions as the pre-merge rule required: a path patch makes the workspace unbuildable anywhere those folders do not exist, including CI, another machine and this repository's own root worktree. Both branches landed on their repository's `main` first — MagicVault `659da930` (core `0.1.5`, primitives `0.1.2`: the P2 one-time custody work plus P4's bound-destination receipt) and MagicRun `ea7631df` (`tool-runtime-core` `0.1.75`, declared login prompts) — and every sibling dependency now pins one revision per repository. The primitives carry upstream's
0.1.2 additions (Windows filesystem/ACL and local-stream helpers; on unix the
durable-write path is unchanged), reviewed at the literal re-pin.
Removing the patch exposed two things it had been collapsing. `magicvault-effect`
and `magicvault-protocol` had resolved at `150e3511`, twelve commits behind
MagicVault's `main`, because the patch only ever overrode core and primitives;
they now pin the same revision as the rest (effect `0.7.1`, protocol `0.7.0`).
And `tool-runtime-core` resolved twice — once at magician's pin and once through
`magicvault-effect`, which depends on it by version from MagicRun's default
branch — so landing MagicRun on `main` first is what lets both resolve `0.1.75`;
`Cargo.lock` pins branchless git sources too, so the older commit persists until
re-resolved explicitly. On 2026-09-25 both moved together to MagicRun
`b25d989` (`0.1.76`, the opt-in brokered-egress jail), then to `70ceac9`
(`0.1.77`, jail interpreter mode and the macOS watchdog fix), then on 2026-09-28
to `532bdad` (`0.1.78`, staged jail inputs, the Linux in-jail helper and task
ceiling), then to `167bb11` (`0.1.79`, no host descriptor leaks into a jail) and `d27efd2`
(`0.1.80`, a smaller macOS descriptor-listing stack buffer), then to `c65fbba`
(`0.1.81`, declared exec roots for running installed programs in place, the
stale jail sweep Magician runs at boot, and the fail-closed macOS
`JailTeardownIncomplete`); MagicVault stayed at `a22c744` through those because it uses none
of the changed jail modules. On 2026-09-28 MagicVault moved to `5849709`
(magicvault-core `0.1.6`, MagicVault PR #1 + PR #2): secret domain scoping takes
a host set or all sites (`*`), and MagicVault's own lock now selects the same
MagicRun `0.1.81`. Magician's app credential route now asks MagicVault for
exactly the hosts a call may reach (`RequestedDomains`, `issue_grant_scoped`),
and the boot jail sweep first removes leftover private app-credential
directories (`magician-app-credentials-<pid>-<uuid>`) from a previous process.
This dependency update is Windows cross-compile qualified in Magician.
Portable agent storage remains available on Windows with canonical containment
checks. The Apps package stager and app-bound file/table owner still depend on
Unix descriptor primitives; Windows does not advertise those capabilities and
returns `UnsupportedPlatform` at their public boundaries.

At the initial extraction checkpoint, the repositories were private. Dependency
resolution used Git's existing credential helper; public distribution and full
runtime/coverage qualification were still pending. That checkpoint restarted no
service, migrated no vault, and enabled no new model-facing operation. See the
Phase 1 execution ledger.

Phase 2 adds separate `magicvault-protocol`, `magicvault-service`, `magicvault`
(CLI/daemon) and `magicvault-mcp` crates in the public MagicVault repository.
Magician does not link or call them. It advances with shared core `0.1.3`, with
additive `provisioned_metadata` / `ProvisionedSecretMetadata` (sorted field names
without cloning plaintext) and opt-in `try_audit_event_durably`. Only standalone
uses the durable audit method; existing Magician audit methods remain append-only
without new filesystem barriers. Existing core methods, formats,
keys and host paths stay unchanged. Exact Git revisions provide reproducible
updates, not a frozen core fork. See the Phase 2 plan/ledger.
Standalone enrollment/metadata consent is not Magician effect authority.
Browser and HTTP/process delivery continued in MagicVault after this Magician
checkpoint; Magician still does not link those surfaces. The later
targeted migration tests
record the owner-authorized limited execution and SDK/fixture corrections at
MagicVault `5b99781e2fc4e4e1138aaacfe78627f839199837`, not a full suite pass.
The retained product injection tests explicitly import the shared iterative JSON
disposal helper; this test-only extraction fix does not change runtime traversal.
The second deep review
advances both MagicVault dependencies to `05acd8f4fce2529e4efeea6cf00f9a567f8bd854`
for the durable journal method and standalone startup refusal of unsafe journals.
This does not change Magician's legacy journal recovery/permissions policy.

The follow-up static review
also updates shared primitives to `0.1.1` and the host's async durable writer:
exclusive staging receives its requested permissions before the first byte,
then data/permissions are synced before rename. Bare relative filenames sync
`.` as their parent. Host blocking admission still precedes rename; no new
runtime owner or IPC hop is added. Standalone fixes to approval reservation,
enrollment expiry, setup durability and MCP client teardown stay upstream.
Checked-in Cargo configuration fetches the extracted crates through the normal
Git CLI. Both repositories are public, so a fresh clone resolves them with no
credential at all; nothing is embedded in manifests and no one-off environment
override is required.

### Runtime services

The runtime is now a **local-first three-service stack**:
- `magician` (API + orchestration, default `3002`)
- `magicutor` (browser automation, default `3003`)
- `magic-supervisor` (process control, default `8081`)

Desktop-managed native Magician listens on the LAN by default so mobile
enrollment can offer a server-derived private Same Wi-Fi route alongside the
configured `connect.magican.ai` remote route. Containers stay remote-only unless
given an explicit host-reachable local origin. Pairing clients choose a route
mode; they never provide the destination address.

**Current shape (2026-09-01).** Magician is a replaceable-runtime personal
agent: `execution.harness_engine` and `chat.harness_engine` can swap the
*mouth* (Magician LLM or a roster harness) while *hands* stay on the plane
MCP door (`/plane/mcp`, ChatScoped vs Plane `plt_` grants). Each run pins its engine at launch, and work it or a chat launches inherits that engine unless one is named. A voice call thinks with its client's composer engine (`chat_choice` on `session.start`), and a changed server chat engine reaches every open composer as `chat.engine.updated`. Apps are a
governed platform (`magician-app-contract` public 8-ops, `magician-apps`
surface/OS-jail/custom surfaces). Background ops — scheduled tasks,
monitors, wakes, and harness-enabled CEO/CTO/CMO/CRO agents — enter the
same flat loop as interactive work. Live scoped state lives under
`MAGICIAN_ROOT_DIR` (default `~/MagicianNotes`); `magician_data_v3` is the
git-backed seed. The C4 canvas is generated from
[`docs/architecture/architecture.yaml`](architecture/architecture.yaml).

Host-native desktop surfaces are intentionally outside that container/runtime
stack. The Tauri desktop app owns the local host gateway (default `3017`) and
launches host-session processes such as the Swift macOS presence mascot. The
mascot registers as a normal media surface, sends mascot-originated text
through the chat ledger, and is controlled from the Tauri menu through a
loopback-only Swift control listener (default `3027`). This keeps native screen,
pointer, and future desktop-automation permissions on the host side while the
runtime stack stays URL-addressed.

For a remote engine, the desktop becomes **Magician Edge**: canonical chat,
task, memory, and workspace data remain on the selected Linux engine while the
desktop establishes an outbound authenticated session for machine-local
capabilities. `runtime-core::edge` owns the versioned hello, lease/heartbeat,
capability-generation, execution-grant, invoke/result, and cancellation wire
types shared by the service and Tauri. Every invocation is fenced by the Edge
session generation assigned by the server and a short-lived
workspace/execution/device/capability-generation grant with explicit request
and response bounds. `EdgeSessionRegistry` owns exact-generation replacement,
leases, capability updates, bounded in-flight calls, result correlation, and
disconnect/revocation failure. The public service never routes inbound traffic
to the loopback host gateway; `3017` remains local-only. Browser/CDP, CUA,
screen, iMessage, and explicitly granted files cross the outbound Edge session
as typed capabilities while Android, iOS, ESP32, and the desktop UI address the
remote engine directly. `GET /api/magician/v2/edge/bridge` now admits only a
durably enrolled `desktop` credential, requires the shared hello as its first
frame, and binds it to that credential's exact principal, workspace, and device
id. Credential rotation and unpairing revoke the live Edge generation. The
Tauri connector enrolls through the authenticated Settings page, stores its
desktop-only credential in the OS credential store, reconnects with bounded
backoff, and follows selected-engine and login changes. Its local dispatcher
advertises `host.cua` from the installed driver's real tool list,
`browser.cdp` while loopback Magicutor reports a connected extension, and read-only
`host.imessage/query` only on a macOS host with a Messages database. Capability
health changes rotate the advertised generation and cancel older local work.
The scoped `POST /api/magician/v2/edge/devices/{device_id}/invoke` door mints
limits from that live manifest; callers cannot supply their own grant,
generation, endpoint, or byte ceiling.

CUA setup and provider availability are independent of Mac automation. Backend
startup probes a local desktop driver or the relay's `cua_available` field for
`requires.cua`; AppleScript and other Mac skills retain `requires.host_gateway`.
Windows executable discovery and Linux desktop-session checks are shared in
`runtime-core`. See [CUA setup](components/scripts/cua-setup.md) for installation,
read-only checks, and the remaining native Windows deployment limitation.

The Tauri tray also hosts the **Jarvis HUD overlay** — a Tauri WebView window
loading the unified-ui `/hud` route. The window is summoned by a **double-tap of
Left Option** (`quick_overlay_gesture`, handled by the native CGEventTap in
`voice_gesture.rs`); the legacy `Cmd+Z` global-shortcut chord — which clobbered
the system-wide Undo — is retired and now an optional, empty-by-default override
(`quick_overlay_shortcut`). The window opens on the cursor's monitor and toggles
open/close on each trigger. Click-outside or `WindowEvent::Focused(false)` hides
it. The HUD content (chat surface, 3D
crawl animation, composer focus state machine, glass backdrop, ContextPill +
theme switcher row) is entirely Svelte — see
`docs/archive/plans/2026-05-22-tauri-unified-ui-consolidation.md` Phase 2 for the
state machine and layout. The Tauri side only handles window placement, OS
shortcut registration, and the mascot anchoring command set
(`glide_mascot_to`, `dock_mascot`).

Desktop Settings is the host-native editor for local operator surfaces:
desktop TOML and, when this Mac owns a local engine, the active runtime dotenv
file. Provider/runtime secrets are edited in the Settings Environment
tab, redacted by default, and written atomically to `.env.development` in
debug/dev builds, `.env` in release builds, or the file named by
`MAGICIAN_DESKTOP_ENV_FILE`. Voice mode, recording STT, reply TTS, auto-speak,
and meeting/observe STT are owned by Magician's scoped media preferences and
edited in Web Settings. Notes capture and workspace storage are edited there too.
Reply TTS can also use Grok and Gemini 3.8 Flash or Flash-Lite. Dictation can also transcribe with Grok Voice Transcribe 2.0. Live calls can use Grok Voice. The
desktop process keeps a headless realtime subscription so browser/composer
changes refresh the native tray menu even when the Settings window is closed.
The tray exposes one mode-aware voice action and only shows live-call controls
while Call mode or an already-active live session requires them.

Tool discovery, registry loading, and ranked lexical matching run in-process via `tool-runtime-core`.
There is no standalone `magictunnel` service in the active architecture.

### Licensing

Every workspace crate is dual-licensed `MIT OR Apache-2.0` (root
`LICENSE-MIT` / `LICENSE-APACHE`; the `license` field in each `Cargo.toml`
says the same). Third-party trees keep their own license files — `magdroid/`
(Apache-2.0 + NOTICE), bundled fonts and 3D assets under `ui/unified-ui/static/`
and `magios/Magios/Fonts/`, and `magesp/components/` — as listed in the
README's License section.

## Current V3 Storage Model

The active runtime now uses a two-tier V3 storage model:
- **System templates and process-level scheduler infra** live under `magician_data_v3/system/...` (git-backed seed)
- **Live mutable state** lives under `$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/...` (default `~/MagicianNotes`)
- raw storage-base roots with top-level `scopes/` or `system/` are not live owners; the active scoped root is always an explicit runtime root, not the repo seed tree by itself
- **A scope exists exactly while its directory exists.** That is the authority every pass which walks `scopes/` already uses, and background discovery must agree with it. Scope lists read out of a database (the mail, resurfacing and attention-learning stores each keep per-scope rows) say what a scope once did, not that it still exists — re-initializing a scope from such a row materializes its directory again, and a deleted workspace then comes back on the next boot and looks live to everything downstream. `AttentionHistoricalBootstrapWorker::discover_scopes` filters its DB-sourced union against the directory for this reason; the default scope is the one exception, admitted without a directory because a fresh install has not created it yet. Resources follow scopes: each live scope opens its own DuckDB databases and every DuckDB instance spawns a scheduler pool sized to the core count, so stale scopes cost threads and memory, not just disk.
- **Deleting a workspace retires what keyed off it.** `DELETE /auth/workspaces/{id}` removes the registry row only after `workspace_has_files` confirms the directory holds no files — data is never deleted for you — and then retires the scope's rows from the attention learning store (`delete_scope`, which also covers the `attention_delivery_*` tables keyed through a decision id) and the resurfacing store (`retire_scope`). Retirement is best effort and logged: the workspace is already deleted by that point, and a store that is unavailable must not turn a completed deletion into an error. A row that survives is inert rather than load-bearing, because discovery filters on the directory. `?purge=true` deletes the data as well: the row and the retired rows go at once, and the directory is removed at the next server start, before any boot sweep can see it (see `docs/components/magician/auth.md`). Creating a slug still waiting to be purged answers `409 workspace_pending_purge`. Settings → Workspaces is the UI for all of it.
- **Retiring a scope is a seven-place operation**, and the scope directory is the smallest of them — the wake queue at the runtime root is what actually resurrects a deleted workspace, and it is global rather than per scope. Two enumerations rebuild scopes from database rows and are filtered against the directory: boot-time historical bootstrap discovery, and the actionability training worker on an hourly tick after a ten-minute quiet period — so verifying a deletion at boot alone proves nothing. Scopes named by a constant (`DEFAULT_SCOPE_*`, `SYSTEM_PRINCIPAL`/`SYSTEM_WORKSPACE`, `QUARANTINE_*`) are infrastructure however empty they look, and `~/MagicianNotes/system/` is the system root rather than a scope. Full procedure: retiring a scope. A scope is created implicitly by anything that writes to it, so a constant naming a scope that no pair reserves manufactures a whole tenant: magicllm's unscoped-call fallback pointed at `system`/`default` — a hybrid of the system principal and the default user workspace, reserved by neither — and boot enumeration then promoted it to ~17 subsystem directories with a duplicate copy of every seed app package and a permanent slot in each per-scope sweep, all to hold 8 KB of real rows. The fallback is now the reserved system scope; `magicllm::trace::RESERVED_SYSTEM_*` mirrors `SYSTEM_PRINCIPAL`/`SYSTEM_WORKSPACE` because magicllm sits below magician and cannot import them, and a test in `transport_log` pins the two together. A directory filter does not defend against this: the writer keeps minting fresh rows, so the scope is legitimately recreated every boot until the constant itself is corrected.

Applied concretely:
- capability packs, tools, bots, trust policies, and DuckDB schemas seed from system templates
- live capability code, bot config, auth/config state, task/execution state, memory, progress, feed, UI-thread, analytics, and publication state resolve from the active scope
- the skillshub npm workspace is pinned to Node `22.19.0` via `skillshub/.nvmrc` (moved out of repo root so the pin stays scoped to the JS subtree); `skillshub/node_modules/` hoists shared deps and immutable CLI packages such as Kapso for rg, dugite, esbuild, and the bot daemons, while workspace-specific credentials, policies, work directories, and CLI homes stay under the active scope (browser is intentionally NOT a workspace member — it owns its own node_modules for the patched-binary overlay)
- bots are now bundled per-bot via `skillshub/bots/build_bundle.mjs` (esbuild) into self-contained `dist/index.js` files; scope installs receive the bundle only, with no per-scope `node_modules/` and no per-scope npm install. Per-bot account env files land at `<scope>/bots/<bot>/.env.<account>` (gmail multi-account) or `<scope>/bots/<bot>/.env.development` (single-instance bots), materialized by `skillshub/scripts/setup_bot_envs.py` from the canonical `bot_configs.yaml`
- the remaining `magician_data_v3/system/wake_up_queue.json` file is global scheduler infrastructure over scoped work, not workspace-owned user state
- the old repo-root `capabilities/`, `bots/`, `.config/`, and `.magician_data/` layouts are not part of the live runtime contract anymore
- runtime components resolve the local-profile root through the shared Artifact V2 storage-root helper; explicit `MAGICIAN_ROOT_DIR` / `MAGICIAN_STORAGE_PATH` wins, live default remains `~/MagicianNotes`, `magician_data_v3` is the git-backed seed, and cargo test binaries without an override use OS temp storage so prompt projections and execution artifacts do not leak into the repo tree
- durable placement is independent of compute placement. Typed capabilities in `magician-storage` plus [`storage-catalog.yaml`](components/magician/storage-catalog.yaml) are the contract. `WorkspaceFileProvider` is a local compatibility adapter, not a remote-storage boundary. Copying a scope directory is not a supported migration. Default startup stays `local_embedded`. New implicit-path bypasses fail `make check-typed-storage-boundaries`

## Runtime config layout

The runtime config is **two files**: `magician-config.yaml` and a sibling
`llm-router.yaml` holding `llm.router.profiles` and `operation_mapping`. Those
tables were 4,000 of the config's 6,200 lines; the config is now 2,156.

The loader splices the tables in as *text* before parsing, because fifteen
profiles alias `*local_generation_model` whose anchor lives in `runtime:` of the
main config, and YAML anchors do not cross documents. Settings → On-device
generation (`GET`/`PUT /api/magician/v2/settings/local-generation`) rewrites
that one `selected:` line surgically, warns when the RAM-tier rule is
violated, still allows the switch, and reloads Ollama. A missing sibling is
fatal: there are no built-in profiles, and `profiles` carries
`#[serde(default)]`, so an absent file would otherwise parse into an empty
router and fail much later.

Consequences for anything that reads or ships the config: read it through
`load_magician_config_from_path`, `shipped_repo_config_yaml()`, or
`scripts/magician_config_text.py` — never with a plain file read, which parses
cleanly into an empty router. Anything that seeds or packages a runtime root
must write both files. Full contract:
[router tables](components/magician/router-tables-file.md).

## Components

### `magician-core`
Layer-1 extraction target for breaking the `magician` monolith: pure-logic modules
with no dependencies on the rest of `magician_v2` move here first. The `magician`
crate re-exports each moved module from its old `magician_v2` path, so consumer
call sites are unchanged. Currently owns `json_traversal` (stack-safe traversal,
metrics, canonical serialization, and blake3 hashing for external JSON payloads),
the attention-routing vocabulary and its rusqlite observability store
(`attention_funnel`, `attention_funnel_store`), plus the zero-dependency
leaves `history`, `config_extras`, `hitl`, and `gws_cli`, `local_resource_governor`
(hard `admit_agent_loop` default cap 50; `0` observe-only; RSS probe-injected;
retrieval HOL gauges injected), and the slot-graph kernel (`slot_graph` types + enrichment,
`confidence`)
`durable_io` (the temp-and-rename durable-write primitives plus transient-I/O
retry shared by every filesystem-backed store, extracted from
`artifact_v2::io`, which re-exports them), and `prompts` (prompt constants,
types, storage, and the `PromptManager`; re-exported from `magician_v2::prompts`).
See [components/magician-core](components/magician-core/README.md).

### `magician-api` / `magician-bin`
The HTTP/WS surface (167k lines) is now the `magician-api` satellite crate
depending on the `magician` lib, and the server binary is the `magician-bin`
package (bin name still `magician`). Live-runtime `main()` resolves the typed storage bootstrap
(`--storage-bootstrap` / `MAGICIAN_STORAGE_BOOTSTRAP_CONFIG`) then loads
`magician-config.yaml` and resolves `runtime.scale` (env
`MAGICIAN_SCALE_PROFILE`) before constructing main, execution, and Lance
Tokio runtimes. After the workspace root is resolved it installs one
`StorageRuntime` process-wide before CLI/`--reindex` returns. The exclusive
default scope lease is acquired only on the long-running server path. It
serves `GET /health/storage`. It also installs `DeviceTransport::LocalLoopback` for
machine-bound CDP/Ollama dials. Durable-artifact REST and realtime WS resolve
`magician_storage::ScopeId` through `api_scope::LocalPermissive`. Provider-free
app authoring still runs before that load.
Missing bootstrap keeps the current local `workspace_storage` profile.
Both API scopes (`/api/magician/v2`, `/v3`) carry the same three-layer wrap:
`magician_v2::cors::api_cors_middleware` innermost (grants the wildcard on
every routed response and answers every `OPTIONS` before routing — a nested
`web::scope` never falls back to a catch-all route, so a preflight into one
would 404), Cloudflare Access verification, then the bearer auth gate
outermost (`auth::middleware::authenticate_request`, whose 401 carries the
same CORS constants because it short-circuits above the CORS layer).

### `magician-storage`
Neutral identifiers, errors, capability traits, bootstrap profile
validation, local filesystem adapters (`LocalStorage::open`) with
Task 3A per-object sidecar versions and CAS, and `StorageRuntime` /
`ScopeLeaseManager` (Task 4). No
AWS/Postgres/domain-model dependency. Domain repositories stay with owner
crates. Decision Gate 1 selects PostgreSQL as the first remote transactional
backend; local embedded stays SQLite. The disposable spike
`magician-storage-gate1` is not a production adapter. See
[components/magician-storage](components/magician-storage/README.md)
and the
[Gate 1 ADR](components/magician-storage/adr-2026-08-31-remote-transactional-backend.md).
Dormant S3 object/dataset adapters live in `magician-storage-s3` and are
not selected by default startup. Dormant SQLite/Postgres repository
foundations and SQL leases live in `magician-storage-state`. The dormant
owner-closure and migration coordinator lives in
`magician-storage-migration` and is not selected by default startup.
`magician-bin` constructs `StorageRuntime::open_local`, installs that runtime
process-wide (`StorageRuntime::current()` / `process_storage::workspace()`),
and must not depend on
the S3, state, or migration crates. Live workspace writes classify through
`typed_io` onto cataloged owner kits. DuckDB `COPY TO` parquet is
republished with `publish_written_parquet`; live SQLite/DuckDB opens
kit-owned paths. `magician-api` and `magician-comms` use
that handle. Task 21
(`scripts/check_typed_storage_boundaries.py`) ratchets runtime-root I/O,
Parquet globs, object SDKs, and ambient backend construction. Reaching
`remote_ready` does not delete the local source; cleanup needs `remote_active`,
the rollback-retention window, a restore drill, and a separate operator
decision. Catalog inventory is 89 owners / 74 Tier 1 / 10 Tier 2 / 5 device-local,
all `remote_ready` with `local_active` authority. Closed owner packets
cover Tasks 9–16B (delivery receipts through desktop engine roots).
See
[owner-closure-reference.md](components/magician-storage/owner-closure-reference.md).
Prerequisite "api-drain" moved every
lib-side dependency out of api first (vibedev store, task lanes, HITL
metrics, observe config IO, monitor support, event-scope visibility, task
ownership, task-run factory, screen capture, scope, today cache, canonical
attention, memory API, MCP OAuth broker) — all re-exported from their old
api paths. See components/magician-api and components/magician-bin.

### `magician_v2::vibedev::projects`
The scoped VibeDev `projects.json` substrate (records, store, locks, durable
read/mutate, pointer semantics, episode resolver), extracted from
`api::vibedev_api` so lib modules stop importing from `api` — the
prerequisite for extracting the api surface into its own satellite crate.
`api::vibedev_api` re-exports it. Since plan 3.4 the substrate lives in the
`vibedev/` module tree; the pre-3.4 flat path (`magician_v2::vibedev_projects`)
was a shim removed in Phase 5 (2026-08-28).
The same drain (tranche 2) moved `observe_connectors`, `monitor_support`,
`agents::task_ownership`, and event-scope visibility into `realtime_events`
— all lib-side, all re-exported from their old api paths. Tranche 2b added `media_rails::screen_capture` and `task_run_factory`; tranche 3
moved the remaining grouped imports (observe config IO, monitor task creation,
vibedev project selection). The drain is complete.

### `magician-comms`
The comms data plane (channel-assist, 36k lines) as a satellite crate over the
magician lib. Its observe-config substrate and pinned LLM dispatch seam live
lib-side (`observe_connectors`, `channel_types`, `llm_dispatch_seam`) so the
lib keeps zero dependencies on it. See components/magician-comms.

### LLM routing, embeddings, and processing locality

All generation and embedding provider calls cross the `magicllm` routing
boundary. Embedding operations use the explicit `embed_documents` and
`embed_query` operation bindings and call the provider's typed embedding API;
the vector-index crate retains a direct-provider fallback only for standalone
use when no configured router has been installed. The dedicated embedding
listener is not a generation profile, is excluded from generation prewarming
and `MAGICIAN_OLLAMA_BASE_URL` rewrites, and remains governed by the runtime
embedding admission and residency policy.

`privacy.processing.mode` is the operator-owned locality switch. In `local`
mode, channel-assist and other governed processing operations must resolve to
their reviewed Ollama profiles. In `cloud` mode, the same operation bindings
select their explicit remote arms, local preparation is disabled, and app
remote-processing posture is derived from that one setting. The server exposes
the bounded settings view and update route through `magician-api`; `magician-bin`
installs the configured router and applies the mode during startup and reload.
There is no caller-supplied provider override at the comms or app boundary. The
three shipped/development config surfaces currently select `cloud` and are kept
byte-identical; the schema default remains `local` when the privacy section is
absent.

### `magician-apps`
The app-platform runtime surface (~22k lines: surface hosting/hydration/
workers, entity lifecycle periphery, migration, sandbox, custom-surface
scripted host) as a satellite crate. The workflows/registry core stays
lib-side pending the execution↔apps cycle inversion. The supported-public
wire is `magician-app-contract` (live 1.4.0, eight operations). Internal
apps (Town Square) sit on the same primitives. Background ticks are
designed on the task-cron spine; the timer is not yet wired. See
[components/magician-apps](components/magician-apps/README.md) and
[the public contract](contracts/app-platform/v1/README.md).

### Replaceable runtime (plane + chat mouth)
`execution.harness_engine` (loop decide) and `chat.harness_engine` (chat
mouth) are orthogonal. Default for both is `magician`. Roster names
(`pi`, `claude_code`, `codex`, `codex_app_server`, `grok`, `agy`) spawn isolated
CLI children; the grant never rides argv. Hands are always the plane:
`POST/GET/DELETE /api/magician/v2/plane/mcp` (`DELETE` ends a
streamable-HTTP session, never the grant). Chat mints **ChatScoped** `plt_`
grants so the conversation keeps its lane surface; run and terminal mints
stamp Plane. Meeting, App Copilot, Tutor, public envoy, and
disclosure-guarded turns stay on Magician's LLM. Live/Realtime
`delegate_to_chat` follows `chat.harness_engine` (Live is the mouth). See
[plane.md](components/magician/plane.md) and
[chat-mode.md](components/magician/chat-mode.md).
The binary installs the saved run snapshot at boot, including
`execution.pi_profile`. A Pi-driven agentic turn resolves that name from the
live LLM router and passes its provider/model settings to the pinned Pi 0.87.1
process. Config reload and the Settings run-engine write refresh the snapshot;
an empty choice uses Pi's own model settings.
Web chat Settings and the composer share a browser-local engine, model, and
profile choice. Each chat request carries that choice, so another device can
use a different one without changing the server's `chat.harness_engine` default.

### Background ops
Scheduled/recurring tasks, monitors, and wake consumers share the
task-cron / sealed accepted-launch spine. Harness-enabled personal agents
(`kind: personal` + `harness` defaults) compile `focus_areas[]` into
scoped scheduler goals; the dogfood topology is CEO → CTO with CMO/CRO
seeded as ordinary personal agents. See
[harness company loop](components/magician/harness-company-loop-phase2.md)
and [features](features/README.md).

### `magician-surfaces`
Near-free surface modules (~31k: thinking_map, progress_channels, evals,
counterparties) as a satellite crate; their lib-consumed vocabulary lives in
the thinking_map_models / tutor_map_context / progress_channel_seam /
counterparty_types seams. See components/magician-surfaces.

### `magician-media`
The media plane (~47k: voice orchestration, streaming STT, fluid audio,
runtime config, providers, meeting engines, plus scheduling/run_state) as a
satellite crate; the cross-module vocabulary lives in
magician_v2::media_seam. Personal realtime voice stays usable when an
owner-session credential is present but the selected profile is not in the
processing-trust catalog (browser DirectPeerToPeer and undeclared native
backend-proxied routes drop the fence rather than abort the call). See
components/magician-media.

### `magician-learning`
The learning plane (~21k: outcome_learning, data_room, bots, execution_panel
engines) as a satellite crate; the panel's serde state vocabulary stays
lib-side in `magician_v2::execution_panel::types`. See
components/magician-learning.

### `magician-chunking`
The logical-chunking domain adapters (~7.5k: hierarchical/consolidation,
memory, shadow eval) as a satellite crate; the runner, registry and release
readiness stay lib-side, and builtins register into the global registry at
boot. See components/magician-chunking.

The 2026-08-25 `meetable_bot` integration retained these six satellite
boundaries. Qualification covered every workspace/all-target build with zero
warnings and the complete non-live repository test gate, including 14,385 Rust
tests, doctests, web/desktop/native suites, deterministic evaluation harnesses,
and iOS/Xcode.

The 2026-09-15 compiler cleanup preserves runtime behavior: router override
inspection is available only in test/fixture builds, pack retries reuse the
borrowed capability name, and the runtime environment catalog closure needs no
mutable binding. The focused offline `magician-api` library check, including
`magician`, completed with zero warnings. Full `make check-all` stopped during
external-library setup because GitHub DNS resolution was unavailable.

### `magician-decision`
Vendor-neutral structured-decision plane: Choice/Score/Noul question IR,
versioned packs under `data/magician_v2/decision_packs/`, and model
adapters (TypeSafe Jev first). Typed decisions stay off the chat-provider
seams entirely — see
[structured-decision.md](components/magician/structured-decision.md).
The host's `decision:` block configures the client in `magician_v2::decision_host`.
The agentic decide phase uses one `tool_action_judge` operation for every authorized
tool and selected harness. Decision Engine owns candidate construction, model fit,
per-model thresholds, continuation limits and escalation to the selected planner.
Surface-specific judges and their operation configs have been removed.

### `decision-engine-contract` and `decision-engine`
`decision-engine-contract` owns typed decisions, shared action requests/verdicts
and the Unix-socket client. Contract version 2 removes the old surface endpoint;
update the host and engine together. `decision-engine` reads only
`$MAGICIAN_ROOT_DIR/decision-engine.yaml` and serves `/health`, `/v1/operations`,
`/v1/decide`, and `/v1/action` on `$MAGICIAN_ROOT_DIR/run/decision-engine.sock`.
Magician uses this process and the supervisor manages its lifecycle. Configured
shared-rail transport/contract failures cannot silently bypass action selection.

### `tool-runtime-core`
Shared crate for:
- capability/registry loading from the active scoped capability roots materialized from V3 templates
- hierarchical visibility + enablement checks, including the shared `core_utility` category that remains visible across agent-filtered catalogs unless explicitly excluded
- lexical matching and category-ranking primitives

### `magician`
Owns:
- `/api/magician/v2/*`
- `/api/magician/v2/realtime/ws`
- planning/orchestration + tool selection
- operator-owned Settings endpoints for trust policy and live `magician-config.yaml` reload

Chat responses retain their canonical semantic `ChatMessageContent` and attach an
optional, server-derived `StructuredResponseV1` presentation sidecar for every eligible
non-user message. The sidecar is bounded and validated before persistence and transport;
clients render it only after full validation and otherwise fall back to canonical content.
The signing-disabled iOS Simulator target compiles this native validation path as of
Magician `0.6.1143` / Magios `0.1.177`; live behavior validation remains operational work.

It uses local capability files through `LocalToolServices` backed by `tool-runtime-core`.
Shipped skill / bot definitions live in `skillshub/` (the source-of-truth tree) and
install into `$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/skills/...` (default
`~/MagicianNotes`). The former
system-shared skill tier is retired; credentials and sessions remain scope/profile
owned rather than determining an install layer.
Bots, scope auth, and per-task workdirs are first-class scope siblings:
`$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/{bots,auth,workdirs,skills}/`.
The `<scope>/capabilities/` umbrella is gone — pack dispatch resolves names through the
in-memory `CapabilityRegistry` (embedded compiled defs plus governed `SKILL.md` tool
packages; legacy schemas are external compatibility only),
no disk-yaml lookup at dispatch time, no hot-reload service.
The old `macos_file_dialog` composite pack and legacy native-file-dialog browser
action surface are intentionally removed; browser file handling goes through the
browser pack's CDP/file-input paths, while host-native desktop automation
belongs in the macOS host bridge.
Browser-facing V2 consumers still exist, but they now attach explicit scope on fetch and
WebSocket requests so they remain compatible with the scoped V3 backend contract. That now
includes the browser extension's direct Magician execution/control/status requests as well;
the current temporary fallback is `anonymous/default` until active principal/workspace lookup
is exposed directly to the extension transport.
Mutable GAUI agent layout snapshots are runtime state under scoped agent-runtime roots; they
must not be persisted back into `system/agent_templates/...`.
Harness-enabled personal agents are now the active autonomous-operations control architecture on
that same runtime path. Harness remains a capability on `kind: personal`: `harness` defaults plus
`autonomous_config.focus_areas[]` compile into stable scoped scheduler goals, harness read/action
/evaluation tools run through the normal capability registry, structural changes remain
proposal-backed and approval-gated, and the default dogfood topology now uses scoped CEO -> CTO
harness ownership instead of a separate hardcoded meta-agent control loop.
The scoped default seed also includes CMO and CRO harness agents for marketing and revenue. These are normal personal agents, not system agents, and their delegation targets are filtered through the same runtime worker boundary as every other user-visible delegation surface.
Agent definitions can be temporarily disabled with `disabled: true`. The runtime expands that flag through reachable `delegation_targets`, blocks manual and scheduled triggers for the disabled hierarchy, prunes scheduler entries, excludes disabled delegates from planner/runtime catalogs, and reports disabled status through the agent read model. This is a rollback/rollout control for OPC subtrees, not a replacement for deleting or redesigning the seeded topology.

### `magicutor`
Owns browser and automation execution. `magician` delegates browser-heavy operations to `magicutor`.
Magicutor no longer exposes a native macOS file-dialog action; OS-level desktop
dialogs are outside the browser action model and should be handled through the
host bridge/native automation path when needed.

### `magic-supervisor`
Owns lifecycle management (start/stop/restart/health) for `magician` and `magicutor`.
Manual restart protection is rolling-window based: the supervisor limits repeated
restart attempts inside the configured time window instead of permanently
blocking a long-running supervisor after a fixed lifetime count.

### Unified UI (`ui/unified-ui`)
User-facing app routes:
- `/`
- the app shell routes under `ui/unified-ui/src/routes/(app)/` (for example
  `/home`, `/chat`, `/tasks`, `/channels`, `/skills`, `/settings`)

Historical admin routes under `/magictunnel` are removed.
Veil SOTA fixtures under `ui/unified-ui/static/tests/sota-tests/` are static,
page-owned test applications: each fixture initializes its visible canvas, SVG,
DOM-overlay, or sandboxed iframe state on load, and reset/status controls must
remain wired to the page ids used by that fixture script. The agent/browser
harness should observe and interact with page-visible state; it must not depend
on a first pointer event to reveal target visuals.
Crew/operator surfaces now also act as the thin human control plane for harness-enabled personal
agents: the global config-backed on/off switch, config editing, focus-area health, scoped
subordinate status, structural proposal review, and owner-briefing queue/history all live on the
normal UI path rather than in a parallel admin tool.

## Guided Setup

`magician-setup` is the installer people meet first. It asks about
**capabilities** — outcomes a person recognises — and derives the components,
because nobody wants a browser executor; they want the agent to read their tabs.
The graph and resolver come from `magician-components`, so the wizard and the
runtime answer from one declaration.

It draws an inline Ratatui viewport rather than taking the alternate screen: a
build streams for minutes through the same terminal, and manual steps ask
someone to grant a permission and come back, so the transcript has to survive.
Three modes — rich, plain, and non-interactive — read the same model, and it
falls back rather than prompting when there is no TTY, since a wizard that
blocks in CI is a hung build.

Selection starts from what already works. The plan orders dependencies before
dependants, picks one provider per requirement and says which, and lists
everything left off with its reason. Details:
`docs/components/magician-setup/README.md`.

Magican Desktop consumes the same graph from the selected engine through
`GET /api/magician/v2/components/catalog` and plans a chosen capability set
through `POST /api/magician/v2/components/plan`. Probes therefore describe the
machine that will run Magician, including remote Linux engines, rather than the
machine displaying the wizard. The same flow also reads the existing
Skillshub catalog, including typed authentication modes and secret-reference
names, and is available after onboarding from Desktop Settings. Completion is
blocked until selected requirements are observed or an explicitly manual probe
has been confirmed; credentials are never returned by catalog APIs. Automatic
component actions are setup-token-gated, serialized server-side jobs whose
target/script comes only from the compiled graph and whose result is decided by
a fresh probe. Scoped Skillshub install/remove and write-only runtime/skill env
updates use the selected engine's API, with Desktop storing its setup token per
exact engine origin in the OS credential store.

## Component Registry

`magician-components` answers, for every surface that asks: given what is
actually running and what the operator asked for, which features work, and for
the ones that do not, what exactly is missing?

It keeps **declared** intent apart from **observed** reality, because they
disagree in ways that matter. A component can be wanted and down, which is a
fault worth surfacing, or present without ever having been declared, which is
what every install predating the registry looks like. Observation is ground
truth for whether a feature works now; declaration only decides whether an
absence is a problem or a choice.

Three node kinds. **Components** are installable or configurable things,
including API keys; a `core` component is not declinable. **Requirements** are
named join points several components satisfy ("a model", "a browser driver"),
named once so two features cannot drift about what counts as one. **Features**
are outcomes a person recognises. A feature needs all of its needs; a
requirement takes any one provider. A component whose own dependency is unusable
is blocked however healthy its probe looked.

The crate performs no I/O — callers supply the declared and observed maps — so
the resolver is a pure function testable with no stack running. Declared state's
home is `operator-config.yaml` in the data root, beside the identity layer.

This is what makes per-component installer gates safe. They were added and
reverted once without it (`cb32bb06ce`), because a gate whose absence nothing
downstream understands yields a broken install rather than a smaller one.
Details: `docs/components/magician-components/README.md`.

## Operator Identity Layer

Every operator-specific value — owner name, principal, the agent's own
inbox/WhatsApp JID, the Cloudflare zone, the public-ingress mode — lives in
one **untracked identity layer**: the data root's `.env` (0600) plus the
`operator-config.yaml` instance. `make setup-identity`
(`scripts/setup-identity.sh`) is the single writer. Nothing in the tracked
tree carries a code default for these values: an unset identity makes the
dependent feature decline to start (the AgentMail ingestor skips a keyed
account with no configured inbox; the tunnel/Access scripts die without
`MAGICIAN_TUNNEL_ZONE`) instead of silently using someone else's identity.
Crate `authors` fields carry the org, not an individual. Prompts address the
owner through the `{owner_name}` template variable (rendered from
`MAGICIAN_OWNER_NAME`, neutral fallback "the owner") — the public-envoy
instruction's v1.2.0 bump is the reference example — and seed agent
templates ship generic personas/fixture names; personal detail lives only in
the live scoped copies under the data root. Test fixtures, doc comments, and
docs use example identities only — `agent@example.com` for the agent inbox,
`connect.<zone>` / `connect.example.com` for operator hosts — never a live address or
hostname, so the mirror's publish gate finds nothing to scrub. Keys are documented in
`magician_data_v3/.env.example` under "OPERATOR IDENTITY"; design in
`docs/plans/2026-09-01-github-go-live.md` §4.

## Runtime Flow

### Task Execution (Agentic)

> **Flat loop (v0.6.689).** The nested per-tool "inner loop" referenced in the
> steps below is **removed**. Execution is a single flat per-action loop: the
> outer LLM sees a flat catalog of `<pack>__<action>` leaves and dispatches each
> leaf **LLM-lessly** via the per-tool primitive dispatchers. The former
> `execution/inner_loop/` module is `execution/primitive_dispatch/`, and pack
> `type: inner_loop` is now `type: primitive` (legacy accepted via serde alias).
> Where steps below say "browser inner loop" / "inner-loop pack" / "inner-loop
> traces," read "the flat loop's per-primitive dispatch." Canonical:
> [`docs/components/magician/execution/FLAT_LOOP.md`](components/magician/execution/FLAT_LOOP.md).

> **Skill invocation evidence and failure routing (v0.6.847).** The shared
> `execution::compiled_dispatch` recorder now captures best-effort Skill
> Evolution evidence for compiled skill/tool calls, primitive CLI-template
> tools, browser primitives, direct native file/http/shell actions, and chat
> runtime tools when scoped runtime context is available. Records live under
> `learning/skill_invocations/dt=YYYY-MM-DD/<invocation_id>.json` and append
> compact `skill_invocation_*` learning events. The evidence model captures
> skill/action identity, source (`compiled_pack`, `compiled_provider`,
> `primitive_cli_template`, `browser_tool`, `native_tool`, `runtime_tool`,
> etc.), agent/task/execution/chat provenance, redacted input-shape fingerprints,
> structural result summaries, duration, and normalized failure classes. It
> deliberately avoids raw argument values and large result bodies. Repeated
> failures cluster under `learning/skill_invocation_failure_clusters/` by source,
> skill/action, failure class, and input fingerprint; high-confidence clusters
> route one review-gated Skill Evolution backlog item through the existing
> `LearningCapabilityEvolutionBridge`.

> **Approval-gated file edits (v0.6.769).** Coding-oriented compiled tools
> (`write_file`, `edit_file`, `apply_patch`) stage proposed writes under the
> scope's `workdirs/home` tree as `FileEditTransaction` records and return a
> `diff_approval` pause instead of mutating disk. The agentic executor converts
> that tool result into `AgenticWaitingForUser` plus canonical
> `HitlRequested { source: "diff_approval" }`, and `/hitl/{transaction}/respond`
> applies or rejects the transaction before resuming the stored pause state.
> Apply uses the snapshot/hash-guarded file-edit path; reject is a transaction
> status transition with no disk mutation.
> As of v0.6.772, the same `diff_approval` source can also resolve Pi-style
> `CodeChangeProposal` records imported from a shadow-workspace patch.
> Those approvals use `proposal_id` / correlation id instead of `transaction_id`;
> the dispatcher resolves proposal ids first and falls back to native
> `FileEditTransaction` ids for existing compiled tools.
> As of v0.6.773, `execution::coding_engine` provides the first Pi adapter
> seam: `PiCodingEngineAdapter` launches `pi --mode rpc` in a shadow workspace,
> normalizes JSONL events, computes shadow-vs-real text patches, stages
> `CodeChangeProposal` records, and returns the same `pending_approval` /
> `diff_approval` payload shape used by native file-edit tools. The richer
> task-card/VibeDev event bridge and engineering-agent routing are intentionally
> separate Phase 1 wiring.
> As of v0.6.774, `run_coding_task` is the first compiled runtime entrypoint
> for that adapter. It prepares a per-run shadow workspace under the active
> scope, invokes Pi there, imports the resulting patch as a proposal, and
> returns the canonical `pending_approval` / `diff_approval` payload that
> Attention and VibeDev already understand.
> As of v0.6.775, the existing engineering-agent org uses that path directly:
> the frontend, junior frontend, junior software, senior software, and
> principal software workers grant `run_coding_task`; the engineering manager
> delegates to those workers; and the architect stays design/review-only. Direct
> Claude/Codex/Gemini/OpenCode CLI grants were removed from these active
> engineering templates and scoped runtime copies.
> As of v0.6.777, coding model choice is Magician-owned rather than Pi-exposed:
> `magician-config.yaml > coding.profiles` maps operator-facing profile ids to
> existing `llm.router.profiles`, `run_coding_task` accepts `coding_profile`,
> and `/api/magician/v2/coding/profiles` exposes the catalog for VibeDev-style
> model selectors. Grok's row is `observe_grok_readiness` (filesystem +
> cached overlays); the GET never spawns `grok --version`.
> Ready also requires `~/.grok/auth.json` or a filtered `XAI_API_KEY`,
> plus positive ACP isolation evidence (present-and-empty `mcpServers`
> and a non-leaky tool list). Unsigned-in hosts surface `auth_required`;
> unattested isolation stays `unqualified`.
> `POST /api/magician/v2/coding/engines/grok_acp/refresh` returns 202
> checking without probing the CLI.
> As of v0.6.1273, Claude Code (`claude-default`) and Antigravity
> (`agy-default`) join the catalog the same way. Observe is filesystem +
> cached overlays; GET never spawns `claude --version` or `agy --version`.
> Claude Ready requires CLI ≥ 2.1.229, Max/OAuth, and isolation (empty MCP
> plus a clean tool list). Magician's `ANTHROPIC_API_KEY` is not inherited
> unless `coding.claude.use_api_key` is true.
> Agy Ready requires CLI ≥ 1.1.19, Antigravity OAuth (Gemini/Google keys do
> not count unless `coding.agy.use_api_key` is true), and an isolation
> receipt whose identity includes path + mtime + length. Catalog
> `search_web` is not Ready-incompatible; runtime use fails closed.
> `POST /api/magician/v2/coding/engines/claude_code/refresh` and
> `POST /api/magician/v2/coding/engines/agy_cli/refresh` return 202
> checking without probing the CLI.
> The checked-in config declares those coding profiles
> explicitly; there is no hardcoded fallback catalog in the binary. Pi
> provider/model arguments are derived internally.
> The current VibeDev catalogue keeps `coding-balanced` on GPT-5.6 Terra and
> `coding-premium` on GPT-5.6 Sol, and also exposes GPT-5.6 Luna, DeepSeek V4
> Flash, DeepSeek V4 Pro, DeepSeek V4 Flash Vision Exp, and MiniMax M3.
> Flash and Pro stay text-only; Terra, Sol, Luna, Astra, Fable, Opus 5, Flash
> Vision Exp, and M3 advertise image input. Kimi K3 is
> not exposed because no routed Kimi profile is configured.
> As of v0.6.778, `run_coding_task` streams Pi RPC progress as `coding.*`
> runtime events carrying task/execution/chat-turn context. Request activity
> cards, the in-flight chat progress bubble, and VibeDev's coding activity rail
> consume those events while the existing `diff_approval` HITL flow still owns
> final apply/reject.
> As of v0.6.780, `run_coding_task` performs credential preflight before Pi
> launches. The selected coding profile's `api_key_env` resolves from the
> Magician process environment or the scoped provisioned-secret store, and
> failures surface as explicit `credential_preflight` errors. Proposal-backed
> Pi diffs also support file-level partial apply through HITL `selected_paths`;
> native file-edit transactions remain all-or-nothing.
> As of unified-ui v0.0.461, the coding profile catalog also exposes
> `supports_user_image_inputs`. VibeDev uses that capability bit to gate the
> image attachment affordance while still allowing generic file references.
> VibeDev stages attachments through its dedicated `#vibedev` chat session,
> serializes attachment ids/names/mime types into the coding task prompt, and
> embeds the shared Voice control so dictation feeds the coding prompt before
> task launch.
> As of unified-ui v0.0.462, those attachment references can be carried through
> to `run_coding_task` via `attachment_session_id` and `attachment_ids`.
> The compiled coding handler resolves the chat-session file index inside the
> active scope, validates MIME type and size, rejects image inputs unless the
> selected coding profile is vision-capable, and copies accepted files into the
> selected repo path inside the Pi shadow workspace at
> `.cache/magician/vibedev_attachments/`. That cache directory is ignored by
> the final shadow-vs-real diff, so attachments are read-only coding context
> unless the task explicitly asks Pi to create a real workspace file from them.
> As of unified-ui v0.0.463, VibeDev uses `/vibe?task=<task_id>` as its active
> coding-run context. Selecting a recent run turns the composer into follow-up
> mode, creates a new `#vibedev` task with explicit parent-task context, passes
> completed parents as `reference_task_ids` when safe, and sorts pending
> `diff_approval`/HITL rows from the selected run chain to the top of the local
> Review panel.
> As of v0.6.782, `run_coding_task` derives Pi continuation for those VibeDev
> chains inside the compiled handler. It reads the current V3 task, walks
> `Parent task:` links back through VibeDev tasks, enables Pi session
> persistence for the chain, and names the Pi session after the root task
> (`vibedev-<root_task_id>`). UI prompts no longer need to set
> `session_name`/`persist_session`; the persisted Pi history is advisory, while
> each run still receives a fresh shadow workspace and the current filesystem
> remains the source of truth.
> As of v0.6.794, `run_coding_task` accepts `repo_path` / `project_repo_path`
> as a scoped relative folder under `<scope>/workdirs/home`, `~` / `$HOME`, or
> an existing absolute directory anywhere the Magician backend process can
> access. The handler copies the selected repo root into the shadow workspace,
> launches Pi at that shadow root, records the real apply root on the staged
> `CodeChangeProposal`, and the existing diff-approval HITL apply path writes
> accepted changes back to that selected repo root. Attachments are materialized
> under the selected repo's `.cache/` directory inside the shadow copy.
> As of unified-ui v0.0.465, VibeDev also has a first Preview bridge. The UI
> reuses live interactive-session state, reads non-draining PTY replay buffers,
> extracts local HTTP(S) URLs from workbench/dev-server output, and embeds the
> selected URL in an iframe. This is a UI bridge only: backend-owned
> `DevServerSession` lifecycle, health checks, ports, and stop/restart remain
> the later hardened process manager.
> As of unified-ui v0.0.472, the VibeDev prompt surface is a dedicated
> `VibeComposer` component. The route owns orchestration state, while the
> component owns the chat-style input shell: profile selection, session link,
> file/image attachment controls, voice control, and go-arrow submit. The
> `#vibedev` thread remains the backing lane but is no longer displayed as
> header chrome; the visible session pill links to
> `/t/vibedev/chat?session=<id>`, and the chat panel honors that session
> deep-link.
> As of unified-ui v0.0.473, code-approval policy stays in the floating
> `VibeReviewPanel` component: the auto-apply toggle lives there, the panel
> auto-opens when manual review is active or approval items arrive, and the
> route no longer owns Review markup/styles beyond orchestration and API
> mutation handlers.
> As of unified-ui v0.0.474, the VibeDev Agents surface is a three-pane
> code-mode workspace over existing Magician state. The left rail is real
> `#vibedev` task history plus pending proposal file paths, the center is the
> functional `VibeComposer` and coding-event response stream, and the right
> inspector switches between Preview, Logs, and Tests. Preview still uses the
> live interactive-session URL bridge, Logs reads the existing `coding.*` event
> stream, and Tests remains an empty structured result surface until the
> test-run pack and result projection are implemented.
> As of unified-ui v0.0.475, the inspector deliberately does not embed the
> interactive Workbench. The dedicated Workbench tab remains the full-screen
> manual CLI surface; any future terminal-like VibeDev view should be a
> read-only run log tied to the selected VibeDev task chain.
> As of unified-ui v0.0.477, that read-only run log exists as an event-backed
> first slice. It filters to the selected run chain when possible, otherwise
> shows recent coding events, and has no CLI selector, stdin, Start button,
> session launcher, or Workbench embed.
> As of unified-ui v0.0.478, the Logs tab hydrates selected run chains from
> `GET /api/magician/v2/vibedev/runs/{task_id}/logs` and merges that backend
> projection with the live coding tail. The projection validates the task in the
> resolved scope, reads persisted `coding.*` events from the workspace event
> log, normalizes Pi messages/lifecycle/approval/tool rows, and safely adds
> compact VibeDev-scoped PTY snippets, local dev-server URLs, and test-output
> snippets when live interactive sessions are present. It is still read-only
> and does not expose Workbench process controls.
> As of v0.6.787 / unified-ui v0.0.480, VibeDev has a first-class Project
> wrapper over `#vibedev` chat sessions. `#vibedev` remains the UI thread/app
> lane, while each Project persists `project_id`, name, backing
> `chat_session_id`, optional repo/preview metadata, and latest root task under
> the active scope's `vibedev/projects.json`. The
> `/api/magician/v2/vibedev/projects` API lists, creates, updates, and
> activates Projects, auto-adopting pre-existing `#vibedev` sessions on first
> read. Activating a Project uses the chat store's existing one-active-session
> invariant for that thread, so attachments, chat deep links, and VibeDev
> prompts all point at the same canonical backing session.
> As of v0.6.788 / unified-ui v0.0.481, that Project wrapper also owns
> lifecycle controls. Rename writes both the Project record and backing chat
> session title. Archive updates the Project record and archives the backing
> session without deleting history. Permanent delete first clears chat-session
> runtime state, deletes the backing chat session through the chat store, and
> then removes the Project record, so project deletion does not leave orphaned
> chat messages or attachment references.
> As of v0.6.789 / unified-ui v0.0.488, Projects automatically bind their repo
> to the scoped coding workspace root as `repo_path: "."` when created or
> auto-adopted. Listing Projects repairs missing or invalid legacy bindings
> back to that default. As of v0.6.791 / unified-ui v0.0.490, the repo folder is
> chosen only at creation time: blank/`.` maps to `workdirs/home`, relative
> subfolders under `workdirs/home` are created when needed, and PATCH rejects
> repo moves for existing Projects. Optional preview pins remain mutable,
> validated as local HTTP(S) URLs, and take priority over live-session URL
> discovery in the Preview tab. The creation UI uses the existing server-side
> directory listing API as a backend-usable folder picker across Workspace,
> Home, and Root. Actual success still depends on the backend process having
> permission to read and write the selected folder.
> The shadow workspace is the trust boundary for Pi: Pi edits a per-run copy
> under the active scope, Magician computes the shadow-vs-real patch, and only
> an approved `CodeChangeProposal` writes into the real scoped workspace.

1. UI creates a task → `magician` creates the initial execution run for that task.
2. `trigger_execution` enters the task-backed agentic loop with runtime context always enabled. There is no per-run execution strategy selector. The direct path derives `allowed_action_types` from the agent definition (restricts the LLM to only declared action types) and always starts with `execution_mode: "bash"` (no eager browser launch — the browser is launched on-demand when the LLM chooses a browser action). If the task has no active execution, the scheduler creates and binds one before execution.
   Task-backed runs also receive the `task_state` capability as a runtime-injected system tool even when the owning agent definition omits it from `tools:`. The pack YAML remains the metadata source of truth, while the runtime injects the tool only when `task_id` is present and preserves it across pause/resume and owner-profile transitions.
3. Planning is optional outer-loop state. User-facing planning and edits stay on `PlanGraph`; execution receives an approved graph, task metadata, prior outputs, and memory as advisory runtime context. The executor no longer converts plans into mutable taskplan markdown, and task-backed execution-owned resume/continue paths plus direct status updates persist canonical V3 outcomes immediately: `PlanningComplete` projects to `Ready`, `WaitingUser` / `Paused` stay `Paused`, `WaitingChildren` stays `Running`, and non-active root states clear `active_root_execution_id`. Runtime-to-Artifact-V2 status projection remains awaited, but the shared runtime-snapshot and caller-supplied-outcome entry points cross fresh execution-runtime task roots before task-record recovery and typed JSON decoding. This protects orchestration, HTTP, eval, and chat-pack callers uniformly, so a deep caller poll chain cannot consume the stack needed by otherwise ordinary persistence work; the handoff explicitly preserves the shared execution token meter and does not use enlarged worker stacks.
4. The agentic executor loop sends a runtime-context prompt every iteration: durable system/persona instructions, the current goal and task frame, recent assistant/tool turns, compact ledger state, available artifacts, memory, page-shape cues, and bounded prior environment knowledge. Context compaction is budget-driven and produces a fresh prompt dump for debugging instead of `taskplan_live.md`, taskplan revisions, or projected plan summaries. Outer and inner prompt projections, inner-loop traces, runtime-context snapshots, and browser artifacts are execution-scoped under `<task>/executions/<exec>/...`, so debug state follows the run that produced it without task-scope duplication. Outer prompt/history context labels true outer turns as `Outer iteration <n>` and staged inner primitives as `Inner iteration <n> (... parent_outer_iteration=<m> ...)`, so local inner-loop counters never appear as a confusing continuation of the outer counter. Large explicit browser observations are compacted before prompt insertion: screenshot/page-shape metadata, spatial surfaces, scroll/reveal breadcrumbs, and bounded actionable samples remain visible while raw screenshot-heavy payloads are kept as artifacts. On mixed DOM-over-canvas pages, observe-time surface-ownership metadata remains runtime context for the model rather than a separate canvas planner.
   Full execution is lazily constructed on a dedicated four-worker execution runtime using Tokio's ordinary stack policy. The direct executor, resume-validation, cycle, and refinement APIs erase their concrete future types at their definitions; joined scheduler-root tasks also isolate owner convergence, staged context retrieval, provider decisions, HITL continuation, exact resume, and pipeline stages while reinstalling scoped authority, cancellation, coding, and token-meter context. Compiled providers are assembled in bounded construction frames, while external JSON and accessibility data use iterative traversal plus explicit retained-depth ceilings. These are ownership, input, and poll-chain boundaries—not enlarged-stack workarounds—and keep tutor/copilot, streaming, and VibeDev executions from embedding the complete agentic state machine in an Actix worker. The launch and qualification contract is documented in [Runtime async stack boundaries](components/magician/runtime-async-stack-boundaries.md).
5. Per iteration: the outer decision LLM chooses a native tool call such as a pack capability, shell, file, HTTP, delegation, or terminal control. Browser work is no longer a direct outer browser-action lane: choosing the `browser` capability starts the browser inner loop, which uses the pinned `agent-browser` CLI and `browser.yaml` primitives until the browser subtask completes or blocks. Command-backed packs now use the same inner-loop request/ledger/transcript machinery through the generic CLI-template dispatcher, including Google Workspace, csvkit, marimo, OCR, Metabase, media generation, messaging CLIs, web/search providers, extraction tools, coding CLIs, and text utilities. The outer tool schema stays shallow; the inner loop sees the full pack guide and `native_action_schemas.run` primitive. Each inner-loop seed prompt includes `loop=inner`, focused capability, objective id, parent outer iteration, inner run index/id, and a note that inner iteration numbers are local to that inner run. Inner-loop terminal success marks that specific `(capability, goal frame)` objective complete, not the whole root task; if the outer loop asks for the same completed objective again, the executor skips the duplicate and feeds back the prior evidence so the model can choose distinct remaining work or finish. Approved PlanGraphs are rendered into runtime context for this loop; the remaining PlanGraph lowering compatibility API emits a generic browser pack intent and never lowers browser steps into Magicutor action JSON.
6. For delegation steps: the executor dispatches to the delegate agent via `DelegationDispatcher`. The delegate runs with its own tools, prompt pipeline, and memory. Equivalent delegated work is identified by a canonical parent+target fingerprint: repeated targets in one response collapse, and completed or in-flight children are reconciled across resumes/restarts instead of spawned again; failed/cancelled work may start a new retry revision. `DelegateToAgent` is always allowed regardless of `allowed_action_types`, but disabled source or target agents are rejected before child execution creation and disabled descendants are omitted from wildcard delegate discovery. Delegation prompts list each target's display name, canonical `agent_id`, aliases, and capabilities; runtime validation canonicalizes display-name/alias inputs such as `Halo` back to their scoped `agent_id` before spawning children. A delegated coding child with a pending `CodeChangeProposal` keeps the parent parked in `WaitingChildren` until the human apply/reject path resolves the review; this review gate applies to harness and non-harness parents alike.
7. Tool results flow back into append-only execution history, assistant/tool events, artifacts, and compact runtime-context ledgers. Successful actions also persist bounded, redacted `tool_call_evidence` artifacts so prior SQL, scripts, command parameters, API/tool inputs, and result previews are discoverable by follow-up work without replaying full traces. UI progress is derived from canonical execution/task state, PlanGraph lifecycle events, step signals, and execution-panel projections rather than mutable taskplan markdown.
8. Large tool outputs can still persist as artifacts when needed, but artifacts are storage/audit aids rather than control-flow primitives. Interpreted capability outputs can auto-capture explicit generated files from wrapped `tool_output` JSON into both execution-scoped and task-scoped output roots as `tool_output_file` artifacts; history rewrites point at the immutable execution copy, while the persisted artifact payload retains the task-scoped latest-view path and download URL for reruns, finalization, and inline rendering. Terminal task finalization additionally emits a compact agent-facing `role=continuation_context` output that indexes summaries, output refs, artifact refs, event anchors, and exact read/download paths; linked follow-up tasks receive that index before raw output refs.
9. `goal_reached` remains an agent decision, but the executor only accepts it when completion evidence is not self-contradictory and any explicit reports or runtime calls named in the goal have actually been observed in successful action history. If final result evidence includes failed or pending test counts, the completion is rejected even when the model claims success.
10. UI receives progress through WebSocket realtime events (`v2Events`).

### Chat Mode
1. UI sends message → `POST /chat/sessions/{id}/messages` (sync) or `/messages/stream` (SSE streaming). Sync sends may include staged `attachment_ids`, and both send paths now treat the backend's ordered `messages[]` response batch as the authoritative continuation projection for the completed turn.
2. Chat service persists the user turn, loads bounded recent session context, and starts a scoped `GoalSource::ChatInline` run for the session agent when the agentic runtime is available.
3. **Streaming path**: the chat surface receives persisted chat messages and progress updates from the backing execution. Streaming is a transport projection; runtime tool choice, continuation, pause, and terminal status are owned by the agentic execution loop.
4. **Sync path**: the chat send waits for the backing runtime turn to finish or pause, then returns the ordered persisted message batch for the completed projection.
5. Runtime-owned action loop:
   - chat-inline direct executions are the only native calls that use `tool_choice:auto` and accept assistant text as terminal answer evidence
   - task controls such as `list_tasks`, `get_task_details`, `run_task`, and `stop_task` are execution-native runtime tools, not chat-local wrappers
   - the old chat-local `ChatToolRegistry`, YAML chat tools, confirmation endpoint, and proposal-continuation path are removed
   - `ToolCallProposal` transcript records and confirmation compatibility have been removed; `ToolCallExecuted` remains renderable as a delivery artifact
   - chat persists structured `UserTurn` / `ToolResultRich` transcript entries alongside user-facing display messages, so multimodal attachment and rich-tool UI state can diverge from the provider-facing continuation transcript without losing chronology or replay fidelity
   - task creation/update now persists `output_mode` (`accumulate` default, `overwrite` optional) so task-level projected file outputs and primary task agent/user outputs can either keep per-rerun copies side by side or reuse stable latest-wins ids/names; overwrite-mode terminal reduction also prunes stale accumulated primary refs from the task output listing instead of leaving historical primary snapshots visible forever
   - `create_task.reference_task_ids` is the explicit progressive-continuation hook: chat and native task creation validate completed prior tasks in the current scope, then attach them as read-only linked context for the spawned run; `delegate_to_agent.reference_task_ids` and `handover_to_agent.reference_task_ids` follow the same rule and persist as task `depends_on`, so child agents receive the referenced continuation pack and artifact index at execution start; unrelated work leaves the list empty
   - `get_task_details` is result-bearing and continuation-aware, returning the primary user summary, output refs/previews, execution artifact previews/counts, and `continuation_context_outputs` / `continuation_context_previews` so chat can answer follow-ups or create linked continuation work without rerunning the source task
   - recent task context is exposed both through prompt variables and a server-appended `<task_context>` fallback block so chat-side refinement routing stays reliable even when prompt assets lag behind runtime support
   - user attachments and rich tool outputs are indexed under scoped chat session storage; image attachments can become prompt-visible multimodal input on provider-safe paths, while non-image files remain UI-visible and prompt-visible as text metadata only
   - rich file cards persist both the scoped relative output path and, when available, the original absolute save path; web chat clients can reveal the containing folder through `POST /api/magician/v2/chat/sessions/{id}/outputs/open-folder`, and the backend only honors paths inside scoped roots or exact absolute paths already referenced by persisted chat output blocks for that same session
   - tool-authorization and sandbox-override escalations on scoped executor runs now route through the shared `UserRequestService` instead of only the executor-local pause path; transport emits `user_request.pending` / `user_request.resolved`, the first channel to answer wins, and web responders carrying a persisted `request_id` must answer through `POST /api/magician/v2/user-requests/{id}/respond` with their workspace-bound bearer rather than `agentic-resume`
   - the backend now owns a real `Ask` / `Plan` split: `mode: "plan"` creates or resumes the
     task-scoped V3 planning loop, clarification replies target an explicit `(plan_task_id,
     plan_question_id)` pair, and generic plan-mode sends create new planning work instead of
     implicitly answering the newest pending question in the thread
   - chat session lifecycle now enforces one active session per `(principal, workspace, ui_thread)`
     on both the backend and UI projection paths; restore/new-session flows archive competing
     same-thread active sessions, re-sync lifecycle subscriptions, and keep the thread transcript
     aligned with whichever session is now authoritative
   - normal chat ask-mode now reaches media/domain packs through the personal-agent `GoalSource::ChatInline` runtime. The old chat-local tool registry, YAML chat tools, and confirmation endpoint have been removed in favor of direct capabilities. The shipped image capability exposes `quality_tier` (`auto`, `fast`, `balanced`, `pro`) across Gemini image tiers with `auto` falling back neutrally to `balanced`; the shipped video capability exposes `quality_tier` (`auto`, `fast`, `balanced`) across the direct Gemini API Veo preview models with the same neutral `auto -> balanced` fallback. The Nano Banana helper emits direct `generate_content` and image-save timing to stderr, the Veo helper emits direct `generate_videos` submit/poll/download/save timing to stderr, and both packs run under a 1200-second outer watchdog with slightly shorter internal Google GenAI HTTP timeouts. The shipped Veo path is deliberately the direct Gemini Developer API route: setup is one scope-local API key in `capabilities/config/.env.development`, not Vertex AI project/location/ADC/GCS provisioning.
6. Streaming config gate: only profiles with `metadata.streaming: true` use the streaming provider path for the first provider turn. Others fall back to sync invoke plus the final `done` event.
7. Bots (Telegram, WhatsApp, Gmail) use the sync endpoint exclusively. Channel clients render persisted messages; they do not implement their own tool loops.
8. Web chat surfaces (`/chat`, `/t/[name]`, bubble overlay) apply one extra display-only projection on top of the persisted transcript: consecutive `task_status_update` messages are grouped into one task panel per task wave with per-execution subcards, while channels and raw chat history continue to receive/store each update as its own message. Those same web chat surfaces now also render user/assistant/system freeform text through the shared safe Markdown component rather than raw plain text, while channels still receive normal text payloads.
9. **Profile chooser**: `GET /chat/profiles` returns profiles with `tool_choice: auto` metadata that are safe for rich chat turns. OpenAI profiles pinned to `openai_api_mode: chat` are excluded and returned as warnings instead. The selected profile is stored in this browser and shared by chat Settings and the composer. `SendMessageRequest.profile` overrides the default `chat_completion` operation mapping, routing to the selected profile via `router_profile_override`.

### Active V3 Task Slice
1. The active storage/runtime slice now lives under `magician_data_v3/`.
2. The code lives under `magician_v2::artifact_v2` and related scoped capability/bot loaders, and is now the live source of truth for task, execution, publication, and scoped capability ownership.
3. The active V3 endpoints are:
   - `POST /api/magician/v3/tasks`
     - accepts optional `output_mode: "accumulate" | "overwrite"`
   - `POST /api/magician/v3/tasks/{id}/execute`
     - accepts optional `refinement`, `overwrite`, and
       `llm_routing_overrides`; the routing override applies to that execution
       tree and is not persisted into the task manifest or agent definition.
       It also accepts optional `delegate_to_agent`, a typed first owner
       transition that still uses canonical scoped delegation admission but
       does not spend a root-model turn rediscovering the named route. Chat
       `refine_task` sets `overwrite = true`.
   - `GET /api/magician/v3/tasks/{id}`
   - `GET /api/magician/v3/tasks/{id}/executions`
   - `GET /api/magician/v3/tasks/{task_id}/executions/{execution_id}`
   - `GET /api/magician/v3/tasks/{task_id}/outputs`
   - `GET /api/magician/v3/tasks/{task_id}/outputs/{artifact_path:.*}`
4. This slice currently proves the folder model, canonical event log, reduced state files, task-owned execution indexing, task-scoped output serving, and output/finalizer plumbing.
5. The current V3 execute path now provisions one real root execution through the existing orchestrator/runtime in background, registers that V3 execution scope with a central canonical runtime event sink, appends canonical execution events natively into `events.jsonl`, and supports multi-turn reruns through execution-local refinement overlays plus prior-output prompt reseeding from the task's prior execution tree. Execution cancellation owns one active token per execution; outer decision/action awaits and nested browser/YAML inner-loop provider/primitive awaits race that token so cancellation stops active LLM/tool work instead of waiting for another iteration boundary.
6. Interpreted capability outputs that surface explicit file paths are persisted as `tool_output_file` artifacts with immutable execution-scoped history plus task-scoped latest-view copies, and the same typed file metadata flows through delegated-child summaries, synthesis, finalization, and inline task-output serving.
7. V3 task manifests and task summaries now carry `ui_thread_id`, so V3-backed feed items and execution-panel views can stay thread-scoped instead of falling back to `"general"` everywhere.
8. The V3 execution-panel adapter now derives pending questions, recent activity, timeline entries, shell/debug detail, and observation/screenshot metadata from scoped V3 event-log, runtime-store, and screenshot-storage reads.
9. The shared feed store is now the single owner for V3 task feed items on the active path; `FeedApi` no longer synthesizes V3 task items during read/list/count/attention requests.
10. Execution-panel resolution is now V3-owned on the active HTTP/runtime path. `ExecutionPanelDelta` remains only as the live projection/delivery bus for subscribers, not as a non-V3 panel fallback.
11. V3 now exposes a normalized task progress projection at `GET /api/magician/v3/tasks/{task_id}/progress`, built from canonical V3 task state, execution tree, grouped attention, and output summaries.
11. V3 followed-task chat updates now publish deterministic progress diffs into the existing progress router, keeping `ChatChannel` as the delivery surface while using V3 state, execution tree, recent runtime events, and output summaries as the source of truth for V3-backed followed work.
12. The progress router now has a direct normalized-message publish seam for V3 chat/attention/progress-channel cutovers, so those paths do not need to spoof legacy realtime events.
13. V3 grouped requested-input and confirmation items now surface through the attention API and attention bar for V3-backed work, while the V3 execution panel no longer owns pending-question prompt lists. Those grouped attention/activity summaries now include delegated child execution events instead of only the selected root execution's recent events, and the active read path now prefers a persisted V3 attention snapshot under the task workspace instead of recomputing everything from recent events on each read.
14. V3 progress projection diffs are now restart-stable because the last published normalized projection snapshot is persisted under the task workspace index.
15. Projection-origin terminal chat summaries are treated as self-contained V3 truth, and projection-origin root `execution.progress` notifications are suppressed in `ChatChannel` so followed chats do not receive duplicate root progress updates.
16. Projection-origin V3 progress messages are deliberately ignored by `agent_memory` so derived grouped attention/output notifications do not create synthetic episodic memory entries.
17. The dead filesystem/bootstrap execution fallback has been removed from the active V3 service path, and the execution file store itself no longer reads or writes the legacy `runtime_v2` execution-generation directory on the live path. Active execution documents, PlanGraph edit history, listings, and pause persistence all resolve through the scoped V3 workspace.
18. V3 now derives an execution-scoped canonical `schedule.json` from the stored V3 plan reference and exposes deterministic schedule readiness at `GET /api/magician/v3/tasks/{task_id}/executions/{execution_id}/schedule`. The active V3 scheduler path no longer invents a synthetic one-step fallback schedule.
19. For V3-scoped executions, delegated child creation is now gated by that V3 schedule-readiness model through a V3-aware delegation-dispatch path. `delegation.launch_authorized` is audit/progress only, and the actual child execution creation step now runs through the V3-owned launch path directly instead of reusing the older dispatcher as the decision boundary.
20. When V3 child results satisfy the active parent schedule gate, V3 now resumes the parent through a V3-owned readiness decision, persists a delegation-results summary scoped to the active child set, and routes the resumed parent through a V3-native direct continuation path instead of the old task-store-backed resume helper.
21. The old child discovery/lifecycle compatibility bridge has been removed from the active V3 path. Child execution activity still uses the shared V3 runtime bridge for reducer/projection sync, but child discovery and terminal reconciliation are now V3-owned instead of waiting for `ExecutionResponsibilityChanged` or child terminal broadcaster heuristics.
22. The active V3 path now has a central typed canonical event catalog in `magician_v2::artifact_v2::events`. Runtime producers emit through one shared runtime broadcaster surface: `emit(...)` mirrors canonicalizable execution-scoped facts into `events.jsonl` and also delivers them to transport subscribers, while `emit_transport_only(...)` is reserved for view/projection traffic that is intentionally non-canonical. Canonical event envelopes keep a stable top-level field order, and payloads are enriched with the envelope context (`seq`, `task_id`, `execution_id`, `event_type`, `timestamp_ms`) when those fields are absent so flat JSONL inspection stays readable across inner-loop, bridge, finalizer, and output events.
23. View/update transport such as feed deltas, chat message inserts, execution-panel deltas, and agent UI deltas remains `RuntimeTransportEvent`-only and is not canonical execution truth. Execution lifecycle facts such as pause/resume/cancel, agentic iteration/execution lifecycle, and status/responsibility changes now route through `emit(...)` instead of bypassing the canonical sink on side paths. Feed task CRUD no longer depends on transport-side task lifecycle replay: task create/update/delete writes scoped feed rows directly and only uses `FeedItem*` deltas for delivery.
   AgentRuntime cancellation now forces the affected orchestrator execution tree into terminal `Cancelled` before V3 outcome projection, and cooperative agentic cancellation outcomes map to `UserCancelled` instead of failure. This prevents stopped pipelines from persisting task snapshots that still appear `running`.
   V3 task/internal-task list reads also perform bounded terminal recovery from
   canonical `execution.outcome_observed` rows when process shutdown or
   interruption left execution state behind the event log, and agentic
   `hitl.requested` rows can backfill requested-input attention summaries for
   executions that are still legitimately `waiting_for_user`.
24. Scheduler wake/context is now fully scoped on the live path. `ScheduleContext` carries `(principal, workspace)`, the scheduler agent uses only scoped runtime cycle APIs, and the durable scheduler state persists real scoped automation task ids instead of default-scope-derived keys. Invalid or impossible cron expressions have no next fire time; recovery repairs older entries to `next_run_at = null` instead of turning them into fallback hourly schedules.
25. V3-backed delegation scheduling now fails closed if V3 task scope or schedule readiness cannot be loaded; it no longer silently falls back to the legacy dispatcher for V3-owned work.
26. When multiple delegated steps target the same agent, V3 launch matching now requires a unique context/title match; ambiguous same-agent launches are rejected instead of binding to an arbitrary ready step.
27. V3 terminal finalization now records one scoped episodic-memory record per terminal execution under `magician_data_v3/scopes/<principal>/<workspace>/memory/agents/<agent_id>/episodes/<execution_id>.json`, including delegated child executions and resumed/background-failure root paths. These records carry provenance back to canonical task/execution state, refs, events, and synthesized outputs. This is runtime-owned post-finalization recording, not a progress-channel side effect.
28. Scope-aware memory resolution now exists for the active V3-backed read and write paths: task-backed agent-cycle strategy effectiveness lookup, manual prompt-pipeline assembly, feedback transforms, task API background execution episode recording/consolidation, agent-cycle post-execution persistence/consolidation, pending-memory checks, chat memory/search/preference flows, progress-channel memory recording, UI-thread memory, and scoped agent-memory read endpoints all resolve memory from `magician_data_v3/scopes/<principal>/<workspace>/memory/...`. Episodic records live under `memory/agents/<agent_id>/episodes/`, tier documents under `memory/agents/<agent_id>/tiers/`, user-shared profile/knowledge under `memory/users/{profile,knowledge}.json` plus user-tier files, and consolidation run-state under `memory/agents/<agent_id>/consolidations/memory_consolidation_runs.json`.
   Memory evals are enforced inside Magician: every periodic/manual run writes
   a scoped `memory/eval_status/regression_status.json` snapshot, emits a
   `memory_regression_status` analytics row, and exposes
   `/api/magician/v2/memory/regression/status` plus internal-data and `/memory`
   dashboard surfaces for `healthy`, `degraded`, `failing`, and `unknown`
   states.
   Memory temperature is an orthogonal scoped overlay rather than a rewrite of
   canonical tier files. App-sourced memories also carry a source-eligibility
   envelope: prompt and search re-resolve the live app store, and package code
   cannot assign temperature or force prompt inclusion. Owner-facing personal
   agents query enabled apps through `app_data_query`/`app_data_search` and
   broker typed results into another app with `app_data_compose`. Prompt rendering syncs
   `memory/index/temperature_overlay.json`, records usage/outcome counters, and
   applies config-backed scope totals from
   `magician-config.yaml > memory.prompt_scope_budgets` plus semantic lane
   caps from `memory.prompt_lane_budgets`. Useful/load-bearing
   memory may also gain compact active projections in
   `memory/index/hot_projections.json`; projections keep the source candidate
   key as feedback identity and carry source refs, source tier/item, projection
   policy version, regeneration metadata, and lifecycle metadata when
   deactivated for hash mismatch, stale verification, policy drift, or old
   never-injected state. T0 projection writes are restricted to stable
   preference/procedure/project/entity lanes; episode projections compact to T1
   and raw source evidence hydrates from canonical memory. The current derived
   state is exposed through
   `/api/magician/v2/memory/temperature/status` and `/memory` Observability.
   Contradictory consolidation decisions preserve the older canonical item as
   `memory_lifecycle: superseded`; conflict review considers both high
   similarity and same-durable-key pairs so changed facts can supersede older
   memory even when wording differs. Normal conflict review routes mini-first
   with a full-model retry, while high-risk user/global targets stay on the
   stronger review chain. Missing reviewer decisions preserve both items, and a
   bounded batch-time contradiction sweep reviews same-key memory already
   present in canonical stores without repeatedly re-reviewing accepted
   keep-both pairs. Memory/evidence helper operations are explicitly mapped;
   evidence distillation and review verification use a mini-first profile with a
   full-model retry, and best-effort distillers run in the dispatch queue's
   Background lane. The overlay records successor/reason metadata and makes
   superseded entries audit-only by forcing T3/score `0.0` and excluding them
   from prompt/search recall; the temperature status API exposes bounded
   supersession chains and the manual maintenance endpoint can run resync,
   projection lifecycle checks, temperature maintenance, and the sweep. New LLM
   transform output also strips explicit obsolete-value explanatory suffixes
   from active insight text before apply, while preserving conflict/evidence and
   structured supersession history.
29. The old no-scope runtime memory injection seams have been removed from the active path. `AgentRuntime` and `MagicianV2Orchestrator` no longer carry a global fallback memory service; scope-owned V3 resolution is the only memory routing model for the current runtime path, and scoped memory HTTP paths derive principal/workspace from the verified bearer instead of accepting caller-selected scope headers or silently using `anonymous/default`.
29. Scoped V3 memory now stores native V3 episode and tier documents on disk, and the active runtime/API path now reads and writes those native V3 records directly for consolidation, retrieval, feedback, chat memory, task memory synthesis, progress-channel memory delivery, artifact registration, and surface publication. The active post-cycle/task episode persistence path, feedback loop resolution, memory APIs, consolidator, prompt pipeline, and remaining test fixtures now all construct and consume `V3EpisodeRecord` / `V3MemoryTierRecord` directly. The old `EpisodeRecord`, `TriggerEvent`, `TierData`, and their conversion helpers are removed from the repo-local active code path.
30. Memory consolidation is no longer wired once into the main orchestrator at startup. The active design builds scoped `MemoryConsolidator` instances at the actual episode/sweep boundary using explicit `(principal, workspace)` scope plus the resolved execution/task agent definition. LLM consolidation over episode sources is batch-oriented for shipped agents (`min_episodes: 10`, `max_staleness_hours: 24`) rather than every-cycle; any remaining cycle-completed rule is goal-scoped and capped, and episode payloads are shaped as counts plus bounded excerpts instead of full action/artifact blobs. Planning tool filtering likewise now resolves agent definitions explicitly instead of reading hidden startup-global state.
30a. Memory consolidation can ask non-blocking scoped clarification questions when an LLM transform finds a high-value durable entity, preference, relationship, workflow, or account reference that is too ambiguous to store confidently. The LLM may emit an optional `_memory_questions` control array; `MemoryConsolidator` strips it before tier merges, dedupes by a stable question key, posts `memory_clarification` requests through `UserRequestService`, and turns answered questions into provenance-bearing clarification-answer candidates routed by `LearningMemoryBridge`. These answers are not labeled as explicit user "remember this" requests and are not eligible for automatic promotion; they follow the review-gated inferred-memory path. Skips and timeouts are logged as learning events and never block the original consolidation rule.
31. Active non-memory HTTP/session entrypoints now use the same explicit scope model as V3 memory. Feed, UI-thread, execution-panel, progress-channel subscription, task, chat, websocket, and V2 execution create/list/analyze surfaces reject missing scope instead of silently synthesizing `anonymous/default`. Feed attention and execution-panel HTTP reads are now V3-required on the live path rather than “try V3 and fall back.” Internal agent-cycle ownership follows the same rule now: manual/scheduled/autonomous cycle bootstrap requires an explicit agent principal, task-backed cycle roots no longer derive scope from `MAGICIAN_DEFAULT_*`, declarative progress subscriptions skip unscoped agent definitions instead of injecting router defaults, and startup hydration no longer seeds a global unscoped personal assistant. Chat identity enrollment now persists in real scoped storage under `magician_data_v3/scopes/<principal>/<workspace>/chat/enrollments.json`: pending and auto-approved records live in the configured default principal scope for that workspace, and approval migrates confirmed identities into the target principal scope.
32. Agent templates and live agent definitions are now distinct. Builtin/system agent templates live under `magician_data_v3/system/agent_templates/`, while usable agent definitions materialize into `magician_data_v3/scopes/<principal>/<workspace>/agent_runtime/` on first scoped read/list. The active CRUD/chat/GAUI/manual-trigger paths operate on those scoped materialized definitions, and the active approval, proposal, scheduler, and paused-agent persistence paths now resolve per-scope stores/caches instead of sharing one global control-state owner. Startup hydration now rebuilds runtime state from scoped materialized definitions instead of registering a shared system-agent overlay. The active trigger/delegation/manual/scheduled/autonomous cycle path now also uses scope-qualified definition lookup, scoped scheduler task ids, scoped failed-cycle bookkeeping, scoped timeout cancellation, and scope-qualified cycle-control state on the live path. Scoped cycle reservations now use scope-qualified `cycle_id` values, so pending/manual dispatch bookkeeping no longer collides when the same `agent_id` exists in multiple scopes. The scoped runtime/control path no longer depends on bare-`agent_id` compatibility helpers, and the scheduler no longer has an in-memory/unscoped fallback mode.
33. Trust policies now follow the same template/materialization pattern as agents and scoped DuckDB stores. The global template source lives under `magician_data_v3/system/trust_policy_templates/`, and each scoped `agent_runtime/system/` hardens `trust_policies.template.yaml`, `trust_policies.default.yaml`, and the live `trust_policies.yaml` from that source on first base-layout. Agent CRUD and trust-policy enforcement therefore read/write scoped live policy files instead of depending on compiled assets or a hidden global live policy owner.
33. Feed, UI-thread, and analytics DuckDB stores now follow the same template/materialization split. Global schema templates live under `magician_data_v3/system/db_templates/{feed,ui_threads,analytics}/`, while scoped live databases materialize under `magician_data_v3/scopes/<principal>/<workspace>/ui/feed/feed.duckdb`, `magician_data_v3/scopes/<principal>/<workspace>/ui/threads/ui_threads.duckdb` (plus the scoped thread lock file), and `magician_data_v3/scopes/<principal>/<workspace>/analytics/analytics.duckdb`. Feed/UI-thread stores materialize on first use and at startup for existing scopes; analytics materializes lazily on the first scoped analytics event or analytics API query. UI-thread deletion is tombstone-backed (`deleted_at`) so visible lists hide deleted threads while sync still sees tombstoned ids and does not recreate them from older task/session records; `#general` remains non-deletable. Writable UI-thread templates are checked for current columns (`display_mode`, `plan_mode`, `deleted_at`) during first scoped materialization and are refreshed from the embedded DDL when stale, while read-only seed deployments remain immutable. The analytics event table is bounded per `event_type`: rows newer than 24 hours are retained, and at least the newest 1000 rows survive even when older. The live path no longer depends on one shared global DuckDB file for those stores.
34. Published surfaces now have a scoped V3 publication model as the active write path: task-backed auto-surface publication and explicit V3 publication commands write `PublishedSurfaceRecord` manifests and a scoped publication index under `magician_data_v3/scopes/<principal>/<workspace>/ui/...`, and the V3 API can list, publish, unpublish, republish, and read those records directly. These publication records are refs over canonical task outputs, not replacement content, and the active path no longer depends on the older `surface_publish` runtime tool. Runtime-native dashboard/publication actions operate through the V3 publication API and can materialize V3-owned MUIJ documents from canonical `task + user` output. The `create_task(... publish_dashboard = true ...)` auto-surface path now requests that same V3-owned `muij_surface` materialization and workspace-pinned dashboard placement instead of stopping at a plain `/briefing` output ref.
35. Published-surface consumers now also have a scoped V3 projection layer over those publication refs: the V3 API can return joined publication projections with task title/status, source-output media type/summary, and grouped pinned top-of-feed sections (`global`, `workspace_pinned`, `thread`) without reading lifecycle artifacts or durable surface manifests directly. Explicit publication records that do not yet materialize a durable MUIJ surface are surfaced as `output_ref_only` instead of pretending a rendered dashboard exists.
36. Published-surface consumers now also have an explicit V3 render contract: `GET /api/magician/v3/published-surfaces/{surface_id}/render` resolves a publication either to a durable MUIJ surface (`render_origin = durable_surface`, `render_kind = muij_surface`) or to a direct canonical output render (`markdown`, `html`, `json`, `plain_text`, `xml`) loaded from the underlying `task + user` output. This keeps rendering ownership attached to publication refs and canonical outputs instead of lifecycle-artifact inference.
37. The first published-surface UI consumers now use that V3 contract directly: Today `/today` (`/home` and `/desk` redirect aliases), `/briefing`, `/briefing/[id]`, and the shared `PublishedScrollCanvas` helper path now resolve surfaces entirely through V3 publication projection/render APIs. Durable MUIJ publications are loaded through the same V3 render record as direct markdown/html/json/text/xml publications, realtime refresh keys off the scoped V3 `published_surface.changed` event, and the old `surface.published` feed/refresh path is no longer active. Detail pages dispatch by V3 `render_kind` before source media type so materialized `muij_surface` records render the same themed dashboard as the canvas; `/briefing` tiles expose a direct Open action to the individual route. Today's Briefings strip is intentionally a preview/navigation surface: cards navigate to `/briefing/<surface_id>`, and the section action navigates to `/briefing` rather than mounting a duplicate canvas modal inside Today.
37. Old durable-surface `reaper` / `recovery` behavior has been replaced with V3-native publication maintenance. Superseded or unpublished V3 publication records now retire their materialized MUIJ payloads and delete the layout file when no other record references it, startup reconciliation rebuilds each scope’s `published_surfaces.json` index from publication-record truth, missing V3 materialized layouts are either re-materialized from canonical `task + user` output or downgraded back to `output_ref_only`, and orphaned MUIJ surface layouts are removed from the global surface-layout namespace.
37a. Feed items follow the same orphan-symmetry contract as published surfaces. The task-delete path on `ArtifactV2Service` (`archive_task_with_options(.., remove_files=true)` and `delete_internal_task`) cascades through `cleanup_task_external_artifacts` → `V3FeedProjectionAdapter::remove_task_summary` → `FeedStore::remove_task_items` and emits `RuntimeTransportEvent::FeedItemRemoved` per row, so live task deletions never produce orphans. Both `FeedApi::list_feed` and `FeedApi::feed_counts` apply the same `retain_existing_tasks` filter against the authoritative task list — counts and the rendered list are guaranteed to agree, and stale rows referencing deleted tasks neither surface nor count. `POST /api/magician/v2/feed/purge-orphans` is a maintenance endpoint that physically deletes rows whose `task_id` is no longer valid (used to clean up legacy orphans from before the cascade); it refuses to run when the authoritative task list is unavailable.
38. The old startup-global durable-artifact root has been removed from the active path. Non-system durable artifacts now resolve per `(principal, workspace)` under `magician_data_v3/scopes/<principal>/<workspace>/durable_artifacts/`. Resource-authority request handlers and analytics HTTP/query surfaces now also require explicit `(principal, workspace)` scope and resolve through scoped V3 roots on the live path. Skill Evolution generation is scoped on the active runtime path; only helper constructors remain outside that strict request/runtime invariant.
39. The active code, prompt, and script trees no longer reference legacy `.magician_data` or `runtime_v2` roots on the live path. Remaining mentions are historical docs / changelogs / archived design material, not active runtime ownership.
40. Skills and bots follow the AgentSkills v1 template/materialization model. Source-of-truth lives in the repo at `skillshub/`. Install materializes every selected package into `$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/skills/<skill>/` (default `~/MagicianNotes`); the checked-in `magician_data_v3/` tree is seed data, not a live install root, and there is no active system/scope catalog split. Each built-in tool has one governed runtime/auth/action contract in `SKILL.md`. Installed CLIs run directly; a retained adapter owns only irreducible protocol, continuation, normalization, artifacts, policy, browser/native behavior, or verification/support. Credentials come from the scoped Auth Broker through exact declared bindings; private per-skill env files are migration compatibility, not catalog authority.
   - The `<scope>/capabilities/` umbrella has been removed entirely. Bots, auth, workdirs, and skills now live as first-class siblings under the scope root: `<scope>/{bots,auth,workdirs,skills}/`.
   - Bot binaries install as self-contained esbuild bundles at `<scope>/bots/<bot>/dist/index.js`; per-bot env files land at `<scope>/bots/<bot>/.env.<account>` (gmail multi-account) or `<scope>/bots/<bot>/.env.development` (single-instance bots), generated by `setup_bot_envs.py` from `skillshub/bots/bot_configs.yaml`.
   - Pack dispatch is registry-only: `CapabilityRegistry` is loaded from embedded compiled defs plus governed scoped skill packages. All 64 built-in tools use their governed `SKILL.md` contract; the 63-package Phase 7 migration was followed by the governed-from-inception `document-to-markdown` package. Retired implementations were deleted at Phase 7 closure. There is no `<scope>/capabilities/packs/<name>.yaml` lookup at dispatch time and no same-call fallback from a governed route to a retired wrapper.
   - A registry publishes only the compiled packs it can serve. Deferred compiled providers bind late in place — the boot binds `task_state`, `internal_data` and the data binders from the base registry's published definitions; a scope snapshot binds the handler-registered family — so `build_compiled_registry` leaves every compiled definition published, and the owner withholds what stayed unbound only after its late binding is complete (`CapabilityRegistry::withhold_unbound_compiled_packs`: the boot for the base registry, retiring the same names from the local tool surface; the scope resolver before it builds its tool index). The catalog any engine reads is projected from those definitions, so an advertised tool is a dispatchable one; a switched engine following the catalog no longer meets `No provider registered`.
   - Skill resolution is workspace-first / system-fallback via SKILL.md presence (single helper: `resolve_skill_dir`). Inner-loop dispatcher and `apply_vars` template surface use the same helper.
   - Bot templates keep routine reconnect/auth/listening notices on stdout/info at the source and reserve stderr/error for genuinely failure-like conditions, so `/channels` severity reflects producer intent rather than a UI heuristic.
   - Forge UI `pack_type` enum exposes all 5 `ImplementationType` variants: `compiled`, `composite`, `javascript`, `primitive`, `command`. The `RUST_BACKED` group covers `compiled`, `primitive`, and `command`.
41. Direct agentic `waiting_for_user` pauses now share the same execution-panel question path as ask-loop clarifications, while service-backed scoped escalations share that same UI shell through `user_request.pending` records. The V3 execution-panel adapter reads `FullPauseStore` for execution-owned pause requests and resumes them through `/api/magician/v2/executions/{id}/execution/agentic-resume`, but scoped tool/sandbox escalations answer through `/api/magician/v2/user-requests/{id}/respond`. Approval responses use the canonical HITL dispatcher at `/api/magician/v2/hitl/{approval_id}/respond` with `source: "approval"`; `/api/magician/v2/approvals/{approval_id}/resolve` is intentionally retired and returns `410 Gone`. The public `agentic-resume` path now validates that the stored pause `(principal, workspace)` matches the caller scope before resuming, and resumed execution-owned pauses rebuild `merged_agent_tools` from that stored scoped pause context instead of falling back to an empty tool surface.
42. Canvas-native coordinate actions now carry a `canvasBinding` snapshot end-to-end from Magician to Magicutor. Point actions and `drag_path` reproject against the live bound surface rect before dispatch, including same-origin and cross-origin iframe-hosted surfaces, and the runtime fails closed with `canvas_binding_changed` if viewport chrome changed and the bound surface can no longer be resolved.
43. Scoped agent-definition reads now stamp `principal/workspace` ownership onto older materialized YAML on read, and owner-handover/direct-agent override flows resolve definitions plus merged tool catalogs from the active scoped store rather than an unscoped fallback.
44. Taskplan generation and update now normalize `Delegate: <agent_id>` metadata from merged tool provenance after each planner pass. Delegate-owned capabilities gain the correct owner line, known-local capabilities lose stale delegate metadata, and delegate-only task blocks without `Capability:` metadata are cleaned before downstream schedule/use.
45. DuckDB and delegated-child lifecycle hardening now fail closed on the active path: `duckdb` actions normalize shell-style placeholders inside the first `sqlite_scan(...)` path literal before prepare/execute, and parent-task cancellation walks both the active delegation group and linked child execution ids while delegated child startup rechecks parent cancellation and child terminal state before launching.
46. iMessage reads now use a dedicated compiled SQLite provider instead of a composite DuckDB wrapper. The `imessage` tool executes read-only SQLite SQL against `~/Library/Messages/chat.db` through bundled Rust-native SQLite (`rusqlite`), requires macOS Full Disk Access, accepts the same `sql`/`output_format` surface, rewrites legacy `sqlite_scan(..., '<table>')` wrappers to direct table names for compatibility, and leaves DuckDB for analytics/persistent finance storage rather than Apple Messages access.
47. Capability execution is inner-loop by default for command-backed domain packs in task/agent execution: outer loops see shallow routing cards, while selected packs load their full guide and primitive action catalog inside a focused inner loop. Chat ask-mode now enters the same personal-agent outer loop by starting a scoped `GoalSource::ChatInline` run for the active session agent, subscribing the chat to the backing execution, and rendering terminal progress back into the conversation. The chat YAML `pack_ref` bridge remains command-only compatibility code for fallback/unwired deployments and direct chat controls. Platform/chat control tools and compiled providers such as task creation, task execution, cancellation, chat switching, preference writes, files, shell, HTTP, DuckDB, and iMessage remain direct until compiled/composite provider dispatchers are added. The archived migration plan is tracked in `docs/archive/plans/2026-05-03-capability-inner-loop-default-migration.md`.
47. `time_math` is a compiled core utility tool for deterministic calendar and timestamp calculations. It returns current time and half-open date ranges with local/UTC RFC3339 values, Unix seconds/millis, and Apple absolute seconds/nanoseconds so agents can prepare SQL predicates before querying sources such as iMessage. Because it is tagged `core_utility`, it appears in merged tool catalogs for every agent unless that agent explicitly excludes the tool name or category.
48. LLM-call telemetry is captured as a single flat row per call through the existing `LLMResponseReceived` broadcaster event and persisted to date-partitioned Parquet under `<scope>/analytics/llm_calls/dt=YYYY-MM-DD/batch_<ulid>.parquet` by `LlmParquetSink` (subscribes to the broadcaster, buffers per-`(principal, workspace)`, flushes 30s / 100 rows / shutdown, writes via DuckDB `COPY ... TO ... (FORMAT PARQUET, COMPRESSION 'zstd')`). The runtime does no aggregation: slice/dice happens in SQL via `POST /api/magician/v2/analytics/llm_calls/query`, while dashboard fan-out coalesces through `POST /api/magician/v2/analytics/llm_calls/query_batch` so several widgets share one in-memory DuckDB setup. The query path registers a `llm_calls` view via `read_parquet('dt=*/*.parquet', hive_partitioning = true)` and allowlists `SELECT`/`WITH` only. The Parquet lakehouse is isolated from the singleton `analytics.duckdb`: separate pool, no shared schema, retention runs as per-partition `rm` (default 90 days, configurable via `analytics.llm_calls_retention_days`). `LLMResponseReceived` carries `task_id` / `root_execution_id` / `execution_id` / `agent_id` / `delegated_agent_id` / `chat_session_id` / `operation` / `profile` / `attempt` / `response_kind` / `started_at_ms` so the cold store has enough context to answer per-task, per-tree, per-agent, per-operation, and chat-vs-autonomous questions without runtime schema changes. Root, child, resume, synthesis, and reflection calls retain one root id, including operation telemetry from legacy adapters without trace receipts, so the root-filtered ledger is the authoritative tree cost rather than a copied execution counter. Chat-inline rows, including Personal Tutor and Live Concept Tutor model turns, carry the resolved provider/model/profile and compute cost from provider token usage through `magicllm::compute_cost`; failed chat-inline rows still carry attempted provider/model/profile metadata when no usage is available. The implementation is archived at `docs/archive/plans/2026-05-12-llm-calls-lakehouse.md`; the operator surfaces are Today's Pulse and the `/llm` page in unified-ui. Today's Pulse ranks the top provider/model pair by call share so zero-cost local Gemma/Ollama traffic is visible with both model and provider shown, while `/llm#today` shows the top three models by today's call volume alongside spend/calls/tokens and yesterday comparison rows.
49. Task-scoped user output synthesis (`task_user_output_synthesize_system` v1.2.0) is now multi-format and chart-aware. `OutputClass::TaskUser` candidate media types are `application/json` / `text/markdown` / `text/html` / `text/plain` / `application/xml` / `text/xml`, so the synthesizer can emit MUI-JSON dashboards, allowlist-sanitized HTML with `data-magician-source` placeholders, raw JSON arrays (auto-rendered as sortable tables with auto-derived companion charts), or markdown with KPI auto-extract. Charts use live SQL bindings: `dataSource: {kind:'llm_calls_sql', sql:'SELECT ...'}` on `BarChart` / `LineChart` / `PieChart` / `MetricCard` / `Table` resolves at render time against the lakehouse SQL endpoint. The prompt enforces a table+chart pairing rule (every Table must be accompanied by a chart visualising the same data on the same SQL) and a default dashboard recipe (KPIs → timeseries → categorical → drill-down table). Dashboards land with one of 5 themes (`editorial` / `brutalist` / `refined` / `terminal` / `studio`) loaded from embedded YAML via `DashboardThemeRegistry` and applied client-side as `--theme-*` CSS variables.
50. DuckDB graduates from a single-action `query` tool to a 7-action inner-loop workflow tool. The `CompiledProviderDispatcher` (`magician/src/magician_v2/execution/primitive_dispatch/compiled_provider.rs`, formerly `execution/inner_loop/`) injects a synthetic `__action_name` provenance parameter into `step.parameters` before calling `provider.lower`, mirroring the existing `__principal` / `__workspace` / `__task_id` injection. `lower_duckdb_action` (`magician/src/magician_v2/execution/lowering.rs`) branches on `__action_name` with `"query"` as the backward-compatible default so direct-dispatch callers (outside the inner loop) are unaffected. The new actions — `preview`, `describe`, `list_tables`, `read_parquet`, `export`, `attach` — are pure SQL shape-builders that lower through the existing `DuckDbAction::query()` path; no new provider, no new `ExecutableAction` variant, no new connection lifecycle. The `read_parquet` action carries a `llm_calls@<principal>/<workspace>` path shorthand that expands to the lakehouse Parquet glob so per-call telemetry queries fit one parameter. The agent-facing operating guide is the inline `guide:` field of `embedded_pack_defs/duckdb.yaml` (rewritten action-first: per-action description list + a "Typical recipe" walking the preview → describe → read_parquet → query → export loop + one example payload per action), loaded via `include_str!` at compile time and read as `pack.guide.as_deref()` (originally by the inner-loop runner in `inner_loop/request.rs`, which no longer exists; today `execution/flat_loop/tool_index.rs` reads it). Compiled-inner-loop packs do not materialise SKILL.md from disk — `load_pack_defs_from_skills_dir` only merges SKILL.md for skills loaded from disk, not for compiled packs — so the inline yaml `guide:` block is the canonical and only agent-facing skill source, matching the pattern of the other 27 compiled packs. The plan is at `docs/archive/plans/2026-05-12-duckdb-inner-loop-expansion.md`.
51. Every agentic execution now records a `CapabilitySequence` — an ordered list of `SequenceStep` entries with each action's correlated capability id (when one exists), concrete request, response status + truncated 4 KB response body, action binding id, and `executed_via` (`Browser` | `ApiReplay`). Sequences persist to `<scope>/api_mining/<origin_key>/sequences/<id>.json` via `SequenceStore`. The `SequenceRecorder` lives on `ActionExecutors.api_mining_sequence_recorder` alongside the existing `api_mining_action_events`, so it observes the same per-action lifecycle the router and trace manager already drive; no second event bus, no parallel storage. The recorder is `Arc<Mutex<Option<SequenceRecorder>>>` so the slot is always present and lazy-init on first action lets `origin_key` reflect the actual workflow. API-replayed steps record through the browser primitive API-takeover path; browser-executed steps record after `agent-browser` primitive dispatch succeeds or returns usable structured output; the branches don't overlap. Browser-executed raw JSON document/detail loads can still carry a capability id, method, request params, status, and response body when the dispatcher has promoted the page to a read-only GET capability; these steps remain truthfully `executed_via=Browser` while becoming replayable in compiled workflows. Per-scope `SequenceMetrics` (`started` / `finalized` / `with_browser_only_steps`) live in the existing `SCOPED_METRICS_REGISTRY` next to `RouterMetrics` and `ProjectionMetrics`. Three new read-only HTTP endpoints surface the capture: `GET /api-mining/sequences/{origin_key}`, `GET /api-mining/sequences/{origin_key}/{sequence_id}`, `GET /api-mining/sequence-metrics`. This is Phase 1 of the workflow-replay plan (`docs/archive/plans/2026-05-13-workflow-sequence-capture-phase-1.md`); Phase 2 (deterministic + LLM compilation into a `WorkflowGraph`) and Phase 3 (browserless replay engine) layer on top.
52. When `SequenceStore::count(origin) >= 2` after a sequence save, the orchestrator triggers two-pass compilation into a `WorkflowGraph` and persists it to `<scope>/api_mining/<origin_key>/workflows/<id>.json` via `WorkflowStore`. **Pass 1** (`workflow_compiler::infer_auto_data_flows`) is deterministic cross-step string matching with short-value and trivial-value filters and `(source, target, target_param)` dedup; it now prefers the upstream JSON key that actually carried the matched value, including a lightweight key-near-value scan for truncated sequence bodies, so differently named params such as `story_id` -> `{item_id}` compile without site-specific code. **Pass 2** (`workflow_compiler::compile_with_llm`) is an LLM call via `LLMOperation::WorkflowCompilation` (new typed variant; profile in `magician-config.yaml`) with the `workflow_compilation_system v1.0.0` prompt; validates referential integrity (step_index matches array position, data_flow source precedes target, skip_if references a PRIOR step, compiled_from_sequence_ids references real input sequences); retries once on validation failure with errors fed back; falls back to a `Draft` graph built from the longest captured sequence + auto-inferred flows when both attempts fail or when the LLM drops replayable canonical steps. The deterministic graph fills captured params as `DataFlow` sources where possible and `Literal` sources otherwise, so operators always get something replayable. `WorkflowGraph` is linear in v1 (linear steps + optional `skip_if`; branching/DAG defers to v2). `WorkflowMetrics` (`compile_started` / `_succeeded` / `_failed` / `_fell_back_to_draft`) lives in the existing `SCOPED_METRICS_REGISTRY`. Three new read-only HTTP endpoints: `GET /api-mining/workflows/{origin_key}`, `GET /api-mining/workflows/{origin_key}/{workflow_id}`, `GET /api-mining/workflow-metrics`. This is Phase 2 of the workflow-replay plan (`docs/archive/plans/2026-05-13-workflow-compilation-phase-2.md`); Phase 3 (browserless replay engine via the compiled `WorkflowGraph`, browser fallback on step failure, active auth management, workflow invalidation on capability demotion) layers on top.
53. Compiled `WorkflowGraph`s execute browserlessly via `WorkflowReplayEngine` (`magician/src/magician_v2/api_mining/workflow_replay/engine.rs`). The engine walks steps in order, evaluates `skip_if` conditions (fail-open on missing source), resolves each step's parameters from the four `ParamSource` kinds via `resolve_param_source` (`Literal` / `DataFlow` JSONPath into prior responses / `UserInput` from caller map / `SessionAuth` from session-values map), preflights each referenced capability through `ApiRunner::can_replay`, executes one HTTP call per step via `StepExecutor` wrapping the existing `ApiRunner::replay_with_reqwest`, and stores parsed responses in an in-memory `HashMap<step_id, Value>` so downstream `DataFlow`s can JSONPath into them. On any API-only step failure the engine returns a typed `ReplayError` (8 variants: `UnknownCapability` / `ParamResolutionFailed` / `HttpFailure` / `NetworkError` / `BrowserOnlyStep` / `JsonPathMiss` / `AuthMissing` / `WorkflowStale`) carrying the failing step id; the inline JSONPath subset supports `$.<field>[.<sub>...]` and `$..<field>` recursive descent without a new crate dep. Successful replays promote `WorkflowMaturity` via the ladder Draft→Candidate (1 success) → Validated (3 successes) → Trusted (10 total with <10% failure rate). Per-scope `ReplayMetrics` (`replay_started` / `_succeeded` / `_failed_*` / `_promoted_to_*`) live in the existing `SCOPED_METRICS_REGISTRY`. Two HTTP endpoints: `POST /api-mining/workflows/{origin_key}/{workflow_id}/replay` accepts `ReplayInputs` and returns the API-only `ReplayResult`; `GET /api-mining/replay-metrics` returns the counter snapshot. The endpoint persists the mutated workflow (stats + maturity) on every replay, success or failure. Direct workflow replay now hydrates `SessionAuth` values from captured session context and keeps in-flight HTTP bounded by the remaining workflow timeout budget.
53a. API mining live takeover is wired at the flat browser primitive boundary, not the legacy browser action enum. `build_primitive_exec_ctx` forwards the scoped `ApiRouter`, API-mining base path, replay metadata slot, last-page URL, action-event buffer, and sequence recorder to `dispatch_browser_primitive`. Top-level browser navigation stays on the browser rail: `browser__open` and batches containing `open` still emit deterministic synthetic `PageLoad` `ActionContext`s from origin/path/query keys for correlation, but they are not eligible for direct API/workflow takeover because replacing navigation with HTTP would leave the live tab un-navigated. Click/fill/type/press/select/toggle primitives build deterministic `ActionContext`s and use the action-context router path; `browser__eval` also emits a generic bindable `Click` context when the script performs a DOM click and a stable text label or script digest can be inferred, so JavaScript-clicked controls learn through the same action-binding rail. Raw JSON document responses from `browser__open`/`browser__get` have a narrow promotion path that registers a safe read-only GET capability with parameterized path/query params, without broadening the miner's normal XHR/Fetch focus to all Document resources. CDP trace drain captures auth headers and auth-looking query params from unredacted traffic into the encrypted captured-auth store before persisted traces are redacted, and returns the just-drained trace burst to the dispatcher for immediate action-to-network correlation; when a mined capability already exists, the dispatcher persists the learned action binding and records the browser step with the correlated capability id. Replay then resolves `auth_requirements.headers` and `auth_requirements.query_params` from `SessionContext`. Read-like POST endpoints such as search/query/lookup/suggest/autocomplete are classified as read-only unless the URL/body looks like checkout/order/payment/cart/admin/mutation work. A successful `ApiRunner::replay_with_reqwest` returns a normal browser-shaped result with `api_replayed=true`, updates `api_replay_last_outcome`, records an `ApiReplay` sequence step, and skips the browser command. Replay failure records fallback metadata and then runs the original browser primitive. Browser fallback/execution drains CDP-proxy traces, records deterministic action events for correlation, updates last-page URL on `open`, `tab new/open`, and simple `location.*` eval navigations, skips URL-less helper reads before sequence start, and records either a browser-only sequence step or a browser-captured capability step for replayable JSON documents or action-correlated XHR/fetch capabilities. Multi-mutation `browser__batch` calls stay on the browser rail; a batch can be taken over only when it has exactly one takeover candidate and does not contain navigation. Runbook: `docs/runbooks/2026-06-15-api-mining-live-takeover.md`.
53b. Browser-discovered raw JSON GET capabilities recover through normal observation and replay verification. `browser__open`/`browser__get` promotion uses the post-navigation/current page URL, merges repeat observations into the existing capability sample stream, and sequence finalization backfills browser-observed JSON API steps into the same capability when a compiled workflow would otherwise remain browser-only. `ApiRunner::replay_with_reqwest` verifies against the full bounded response body before truncating stored/returned previews, supports JSON Schema sketches as sketches (`properties` optional unless `required`, sampled `null` treated as nullable/unknown), and `StepExecutor` treats `ReplayResult.success=false` as a workflow step failure via `ReplayError::VerificationFailed` even when HTTP returned 2xx. This keeps workflow replay stats aligned with capability replay counters: a workflow cannot report success while an underlying capability records a verification failure.
53c. API mining registry integrity is repaired at scoped registry open. `CapabilityRegistry::health_snapshot` compares `registry_index.json` with loadable capability files, reports indexed/loadable/stale/unindexed counts, confidence-tier counts, replayable capability count, action-binding count, and takeover-ready binding count, and `repair_index_if_needed` rebuilds stale or version-mismatched indexes before the router can select those summaries. A binding is takeover-ready only when it has enough samples and can resolve the capability's URL/header/body template placeholders from learned action-param mappings or stable default params; action-context routing also skips URL-unresolvable bindings before replay ranking. `GET /api-mining/registry-health` exposes the same per-scope snapshot with origin-policy annotations and per-origin inactive reasons; the API Mining Routing Activity tab renders it next to router/projection counters.
53d. API mining passive validation is wired into the mining pipeline. When `api_mining.enable_xhr_validation` is enabled, browser-observed first-party XHR/Fetch traces route through `ApiRouter::route_xhr_for_validation_with_context`; matching capabilities replay in the background via `ApiRunner::validate_with_reqwest`, compare against the browser-observed response, and record replay success/failure evidence without replacing the browser task. Read-only/read-like endpoints can promote from passive evidence; write-like endpoints still require router idempotency gates plus per-origin replay policy. `api_mining.xhr_validation_max_per_pipeline` caps validation requests per pipeline run. Per-scope `PassiveValidationMetrics` live in the shared API-mining metrics registry and are exposed at `GET /api-mining/passive-validation-metrics`; the API Mining Routing Activity tab renders them separately from active router replay counters.
53e. Successful active browser-to-API replay now feeds the projection pipeline. After `dispatch_browser_primitive` receives a verified `ApiRunner::replay_with_reqwest` success, it resolves the capability URL template, parses the replayed JSON response body, and calls the shared per-scope `ProjectionPipelineState::ingest_response` handle under `<scope>/api_mining/projections`. Projection ingest is best-effort and never turns a successful replay into browser fallback: non-JSON/not-projectable responses are skipped, Pending projections wait for operator approval, approved projections ingest rows, and migration/storage failures are logged separately. This makes active takeover responses update the same Learned Resources / `query_known_resource` store used by projection reads.
53f. Capability Evolution's generated API-replay surface is catalog-only until a real generated-pack provider exists. `CapabilityPackStore::load_runtime_pack_defs` suppresses legacy generated definitions that encode replay as a composite `browser` step with `action=api_replay`, and `skill_emitter::emit_evolved_skill` skips API-mined replay records while removing stale evolved skill folders from earlier runs. This keeps the live runtime free of nonexistent browser replay actions and nonexistent `magician internal-replay` wrappers without affecting the active `ApiRouter`/`ApiRunner` takeover path.
53g. Phase 5 mixed workflow replay is consumed by live browser primitive execution. `dispatch_browser_primitive` records the browser primitive action and original argument JSON into `SequenceStep.browser_action` / `browser_arguments`; `workflow_compiler::draft_fallback` carries that into `WorkflowStep.browser_fallback`. Before one-off API takeover, the flat browser dispatcher checks stored compiled workflows for conservative matches: the workflow must have multiple steps, the first step's capability must match the current routed browser action, and every step must carry executable browser fallback data. `WorkflowReplayEngine::replay_until_browser_fallback` executes the API prefix of the selected workflow with captured `SessionContext`, a bounded live timeout, stale/demoted capability preflight, data-flow/skip evaluation, and redacted resolved request metadata on each `StepReplayOutcome`. Successful all-API runs return a browser-shaped `workflow_replayed=true` result, update replay stats/maturity, set `api_replay_last_outcome`, and record API-prefix `SequenceStep`s. Mixed runs record the API prefix, then consume `BrowserFallbackRequest` inside the active `agent-browser` session via the same browser dispatch helper used by normal primitives, preserving trace drain, no-effect probing, JSON promotion, screenshots/artifacts, and browser sequence recording. Live workflow replay checks every referenced capability against the per-origin replay policy first, so compiled workflows cannot bypass write/HITL restrictions; explicit `SessionAuth` params now hydrate from captured session context while observability redacts the resulting values.
53h. Phase 6 origin replay policy is enforced at the live router boundary. `OriginPolicyStore` now persists explicit modes (`observe_only`, `validate_only`, `replay_reads`, `replay_writes_with_hitl`, `replay_trusted_writes`) while preserving the old `allow-replay` boolean endpoint as a compatibility shim (`true` -> `replay_reads`, `false` -> `validate_only`). Scoped `ApiRouter::with_base_path` opens the same per-scope policy file as the capability registry and checks policy before returning `RouteDecision::Replay`: missing policy keeps historical read-only replay compatibility, `observe_only` blocks passive validation and replay, `validate_only` permits passive comparison but blocks live takeover, `replay_reads` blocks write-like replay, `replay_writes_with_hitl` returns `RouteDecision::ReplayRequiresHitl`, and `replay_trusted_writes` permits Trusted write capabilities directly while routing lower-confidence writes through HITL. The flat browser primitive path threads `UserRequestService` into `PrimitiveExecCtx`; write replay that needs approval emits a scoped `api_replay_approval` user request with redacted request preview, origin/capability/method metadata, side-effect class, confidence, policy reason, and request fingerprint. Approval executes the already prepared request once in the same paused primitive; denial, timeout, or missing HITL service returns a failed browser-shaped result and stops before the equivalent browser mutation. Direct manual capability replay and direct workflow replay endpoints enforce the same origin policy but block writes that require HITL because those HTTP endpoints do not own a resumable execution context. `GET /api-mining/registry-health` includes the live replay mode so operators can distinguish confidence/binding gaps from explicit policy blocks.
53i. API-mining phase-closure hardening adds config-aware operator visibility and deterministic fixture coverage. `RegistryHealthSnapshot` now carries warnings for index drift, disabled mining, disabled live replay, disabled/dry-run validation, and origins that remain inactive for takeover; the API Mining UI renders that warning stack and includes replay mode in origin-readiness context. Direct workflow replay derives backend-only session aliases from captured `SessionContext` for explicit `SessionAuth` params (`authorization`, bearer/basic aliases, `query:<key>`, `cookie:<key>`, local/session storage keys) while `StepReplayOutcome` redacts session-auth params plus matching URL/body/cookie/query values before returning observability payloads. The local fixture harness under `api_mining::fixture_harness` uses wiremock to prove warm action-bound replay, replay failure fallback signaling, and heterogeneous search -> detail workflow replay with data-flow parameter binding.
53j. Task Recipes add a task-start Rail 1 above per-origin workflow replay. A successful browser task compiles its reported answer backwards through redacted traces and action/sequence evidence into a scoped, cross-origin `TaskRecipe`; the next matching task re-extracts current inputs and attempts bounded HTTP replay before constructing the agentic loop. Reads may hand off at the exact failed step after schema/auth drift, while writes require a verified read step plus a redacted per-step-shape grant and never fuzzy-match, retry, or browser-fallback after sending. Recipe, grant, run-ledger, pack, emitted-skill, and projection records share the scoped API-mining storage boundary and the process/scope master switch. `magician-core` owns the versioned compile/match prompt constants, `magician-bin` wires the read/mutation APIs and optional bounded verifier worker, and `magician-learning`/Unified UI project recipe outcomes into task timelines and the four-tab API Mining operator surface. Provider-free release evidence is owned by `make test-task-recipes-eval`, `make test-task-recipes-safety`, and the focused API-mining library lane; the explicitly opt-in live evaluator owns the cold -> warm -> variant -> drift proof. Acceptance state and report paths live in `docs/runbooks/2026-09-05-task-recipes-e2e.md`.
54. Agent-growth learning Phase 2 adds a best-effort post-run reflection boundary. `LearningReflectionRuntime` reads one persisted V3 episode plus related episodes, open learning candidates, and optional evaluation context, calls the schema-bound `learning_reflection` operation, writes `learning_reflection_*` audit events, and creates inert `LearningCandidate` proposals when durable learning is warranted. It is wired after V3 terminal task episodes, agent-cycle/manual-trigger episodes, and meaningful chat-turn episodes, guarded by `(episode_id, boundary)` completed events for idempotence. Reflection does not promote memory, prompts, skills, wrappers, code, evals, personas, or program state; later learning phases own routing, evaluation, and promotion.
55. Agent-growth learning Phase 3 routes memory candidates through `LearningMemoryBridge`. Reflection prompt v1.1.0 emits routeable `proposed_change.memory` payloads for `memory_fact`, `memory_preference`, and `memory_procedure` candidates. The bridge auto-promotes only explicit, low-risk, confidence-qualified user memory requests/corrections into the existing tiered `memory/users/knowledge.json` store, using the user-knowledge file lock and writing promotion provenance under `_meta.learning_promotions`. Inferred memories, agent/agent-goal memories, malformed payloads, low-confidence candidates, secret-like values, and higher-risk candidates are triaged for review. Reviewed memory promotion through the learning-candidate transition API writes the same memory shape and records learning events plus memory analytics rows before transitioning to `promoted`.
56. Agent-growth eval routing adds `LearningEvalBridge` as the next candidate router. Reflection prompt v1.2.0 emits routeable `proposed_change.evaluation` payloads for `evaluation_case` candidates. The bridge writes reviewable backlog items to `scopes/<principal>/<workspace>/learning/evaluations/backlog/<candidate_id>.json`, emits `learning_eval_candidate_routed`, and transitions the source candidate to `triaged`; it does not generate tests, run harness jobs, or apply capability changes directly. Read-only inspection is available through `/api/magician/v2/learning/evaluations` and the `internal_data` actions `list_learning_evaluations` / `read_learning_evaluation`, and the learning gap audit counts queued eval backlog items.
57. Agent-growth Phase 4 starts with generic skill/tool-pack evolution backlog routing. New Skill Evolution records are written to `scopes/<principal>/<workspace>/skill_evolution/...`; existing `capability_evolution/...` records remain legacy read-only compatibility input for listings, reads, counts, and growth-eval evidence. The current runtime surface is scoped `skills/<skill>/...`, not a separate legacy `capabilities/` tree. Reflection prompt v1.3.0 emits routeable `proposed_change.capability_evolution` payloads for `capability_update`, `tool_schema_update`, and `tool_wrapper_fix` candidates. `LearningCapabilityEvolutionBridge` writes reviewable items to `scopes/<principal>/<workspace>/skill_evolution/backlog/<candidate_id>.json`, emits `learning_capability_candidate_routed`, and transitions the source candidate to `triaged`; it deliberately does not edit `tool_schema.yaml`, `SKILL.md`, wrappers, docs, examples, prompts, or generated pack catalogs. Skill Evolution backlog items now carry deterministic `dedupe_fingerprint` metadata, recurrence/user-pain/blocked-task/validation-failure counters, local-validation flags, priority scores/reasons, owner hints, and superseded duplicate candidate ids. Equivalent open candidates merge into the canonical backlog item and duplicate source candidates transition to `superseded` with decision-log provenance; backlog listing returns highest-priority actions first. Preferred REST inspection is available through `/api/magician/v2/learning/skill-evolution`; `/api/magician/v2/learning/capability-evolution` remains an alias. `internal_data` actions `list_learning_capability_evolution` / `read_learning_capability_evolution` read both roots.
58. Skill Evolution proposals are the next Phase 4 review surface. A backlog item can now have a typed proposal at `scopes/<principal>/<workspace>/skill_evolution/proposals/<candidate_id>.json` with status, capability id, proposed files, change plan, optional patch summaries, eval plan, validation plan, and promotion gate. Proposal creation marks queued backlog items `in_review` and emits `learning_capability_proposal_recorded`, but still does not mutate capability files or runtime catalogs. REST inspection/upsert lives under `/api/magician/v2/learning/skill-evolution/proposals*`, and internal analysts use `internal_data` actions `list_learning_capability_proposals` / `read_learning_capability_proposal`.
59. Skill Evolution proposal review decisions are separate from proposal drafting. `/api/magician/v2/learning/skill-evolution/proposals/{candidate_id}/decision` accepts approved/rejected/superseded/archived decisions with reviewer actor, reason, evidence refs, and optional payload. Approving a scoped skill change requires non-empty proposal `eval_plan` and `promotion_gate` fields, and high/critical-risk approvals also require explicit review evidence through an evidence ref or non-empty payload before the proposal can become approved. The handler updates the proposal, synchronizes the backlog status (including explicit `superseded`) and source learning candidate state, appends candidate decision history, and emits `learning_capability_proposal_decided`. This remains authorization metadata only; no capability files, generated catalogs, prompts, wrappers, skills, docs, or runtime packs are modified by the review endpoint.
60. Skill Evolution validation reports are the Phase 4 evidence gate after review approval. `/api/magician/v2/learning/skill-evolution/proposals/{candidate_id}/validation` records pass/fail/blocked evidence for approved proposals into `scopes/<principal>/<workspace>/skill_evolution/validations/<candidate_id>/<validation_id>.json`; list/read surfaces are available under `/api/magician/v2/learning/skill-evolution/validations*` and `internal_data` actions `list_learning_capability_validations` / `read_learning_capability_validation`. Passing validation requires material evidence through at least one command, evidence ref, non-empty metrics value, or non-empty payload, then marks the backlog item `validated` and transitions the candidate to `evaluated`; failed or blocked reports remain diagnostic evidence only. Validation still does not mutate capability files, generated catalogs, prompts, wrappers, skills, docs, runtime packs, or live capability state.
61. Skill Evolution implementation bundles separate concrete patch/file evidence from final promotion. `/api/magician/v2/learning/skill-evolution/proposals/{candidate_id}/implementation` requires an approved proposal, validated backlog state, and a passed validation report, then stores `scopes/<principal>/<workspace>/skill_evolution/implementations/<candidate_id>/<implementation_id>.json` with applied files, patch payloads, evidence refs, payload, actor, and summary. List/read surfaces are available under `/api/magician/v2/learning/skill-evolution/implementations*` and `internal_data` actions `list_learning_capability_implementations` / `read_learning_capability_implementation`.
62. Skill Evolution application records are the controlled apply path for implementation bundles. `/api/magician/v2/learning/skill-evolution/implementations/{candidate_id}/{implementation_id}/apply` requires an approved proposal, validated backlog state, passed validation report, and non-terminal candidate, defaults to dry-run, and only applies when the caller sends `apply: true`. Records live at `scopes/<principal>/<workspace>/skill_evolution/applications/<candidate_id>/<application_id>.json` with dry-run/applied mode, changed file audit, previous/new content, actor, summary, evidence, and target-surface metadata. The endpoint accepts only full replacement text in patch metadata and rejects unified diff application, absolute paths, parent traversal, symlink writes, large unaudited files, and escaped roots. `target_surface` defaults to `scoped_skill`; `system_skill` maps reviewed `skills/<skill>/...` paths into the system skill layer, and `source_skill` maps them into source-controlled `skillshub/<skill>/...` while still refusing hidden segments and `node_modules`. List/read surfaces are available under `/api/magician/v2/learning/skill-evolution/applications*` and `internal_data` actions `list_learning_capability_applications` / `read_learning_capability_application`.
63. Skill Evolution manual promotion records close the current Phase 4 audit chain. `/api/magician/v2/learning/skill-evolution/proposals/{candidate_id}/promotion` requires an approved proposal, validated backlog state, and a materially evidenced passed validation report, optionally verifies a referenced implementation bundle and/or applied application record, and requires an applied application record covering at least one scoped skill target whenever proposal patches/proposed files, implementation patches/applied files, or promotion applied files point under `skills/`. It then appends `scopes/<principal>/<workspace>/skill_evolution/promotion_audit.jsonl`, marks the backlog `implemented`, transitions the learning candidate to `implemented`, and emits `learning_capability_promotion_recorded`. List/read surfaces are available under `/api/magician/v2/learning/skill-evolution/promotions*` and `internal_data` actions `list_learning_capability_promotions` / `read_learning_capability_promotion`.
64. Skill Evolution worker endpoints make the Phase 4 chain operable without raw file edits. `/api/magician/v2/learning/skill-evolution/proposals/draft` drafts proposals for queued backlog items, seeding change plans, eval plans, validation plans, inferred scoped `skills/<skill>/...` targets where possible, promotion gates, and reviewable full-file skill-guidance patches when a skill target can be inferred, then moves queued backlog items to `in_review`. `/api/magician/v2/learning/skill-evolution/proposals/{candidate_id}/evaluation/generate` materializes a proposal `eval_plan` into `learning/evaluations/backlog/<candidate_id>.json` for meta-harness review. `/api/magician/v2/learning/skill-evolution/proposals/{candidate_id}/validation/run` ensures that eval backlog item exists, executes validation and regression commands extracted from approved proposal plans under the scoped workspace root with command/time/output limits, records a normal validation report, and emits `capability_evolution_eval_started` / `capability_evolution_eval_completed`. If a proposal promotion gate sets `regression_required`, promotion requires the passed validation report to carry regression evidence. The `/memory` page includes a themed skill-evolution panel for ranked backlog actions, priority reasons, owner hints, proposals, eval cases, validation reports, implementation bundles, applications, promotions, drafting, approval, eval generation, validation runs, implementation bundle recording, target-surface dry-run/apply, and promotion actions.
65. Agent-growth Phase 5 routes reusable skill/workflow learning through that same evolution chain. Reflection prompts v1.4.0 can emit `skill_update` and `workflow_template` candidates with routeable `proposed_change.skill_update` / `proposed_change.workflow_template` payloads. `LearningReflectionRuntime` supplies `extra_context.workflow_detection`, a cheap normalized grouping of related successful episodes by source agent, bounded tool sequence, and bounded action sequence; the LLM must still decide whether the group is a reusable procedure before creating a candidate. `LearningCapabilityEvolutionBridge` writes these candidates to `skill_evolution/backlog/` with `learning_skill_candidate_routed` / `learning_skill_candidate_route_failed` audit events, reusing the existing proposal, eval, validation, implementation, apply, and promotion gates. Generated SKILL.md patches include AgentSkills frontmatter for new skill targets so applied workflow templates are discoverable by `SkillLoader`; applied scoped/system skill changes record a `runtime_catalog_refresh` visibility report in the application payload and event, promotion refuses applied records whose post-apply discovery failed, and the `/memory` panel surfaces that refresh status.
66. Agent-growth Phase 6 turns reflection into the meta-harness judge and adds the eval-backlog worker. Reflection prompts v1.5.0 can attach `proposed_change.meta_harness_diagnosis` with outcome correctness, evidence quality, hallucinated completion, missed requirements, tool misuse, memory/retrieval misses, capability and prompt weaknesses, eval gaps, and a recommended candidate type. `LearningEvalBridge` preserves those diagnoses in `learning/evaluations/backlog/<candidate_id>.json`. The new `/api/magician/v2/learning/evaluations/{candidate_id}/run` endpoint consumes backlog items by extracting `commands` / `regression_commands` from `case_spec` (or caller overrides), runs them through the existing scoped validation command runner, stores durable reports at `learning/evaluations/runs/<candidate_id>/<run_id>.json`, emits `learning_evaluation_run_started` / `learning_evaluation_run_recorded`, marks non-blocked cases `evaluated`, and exposes list/read surfaces through REST, `internal_data`, the learning audit, and the `/memory` panel. This reuses the existing command runner and typed learning substrate rather than creating a second eval pipeline.
67. Agent-growth Phase 7 makes OPC program state explicit without adding another runner. `program.md` and focus-specific program documents remain durable user-authored specs; local-file workspaces load them from scoped `programs/`, while SilverBullet workspaces load them from visible `Programs/` in the Space. Mutable runtime state is stored separately at `scopes/<principal>/<workspace>/programs/state/*.json` with the active phase/step, open loops, last-run summary, next-action hints, blocked/escalation status, stop-condition status, recent learning candidate links, the latest meta-harness verdict link, and bounded update provenance. Harness contexts now surface the state path, `evaluate_harness` includes the active program state, and compiled harness tools `read_program_state` / `update_program_state` let harness agents inspect and write runtime state through the normal capability registry. Terminal harness task reflections include the active program state in `extra_context`; reflection prompts v1.6.0 can emit routeable `program_state_update` payloads, and `LearningProgramStateBridge` applies only low-risk review-free updates or reviewer-promoted updates to the runtime state document. Program spec/policy changes still go through proposals or skill/tool/doc work.
68. Agent-growth Phase 8 makes explicit user teaching a first-class learning input. `magician_v2::learning::teaching` accepts `remember`, `forget`, `correct`, `make_reusable`, `improve_tool`, `never_do_this`, `this_was_useful`, and `this_was_wrong`, writes `learning_user_teaching_recorded`, creates typed candidates, and routes them through the existing memory, eval, or skill/tool-pack evolution bridges. `POST /api/magician/v2/learning/teaching` exposes the same recorder to operator surfaces, and the chat-runtime `record_teaching_feedback` tool lets personal-agent chat preserve direct user corrections without burying them in transcript text. Low-risk explicit user memory requests can auto-promote through `LearningMemoryBridge`; eval and skill/tool-pack candidates remain review/backlog items. The `/memory` panel includes a themed teaching form plus recent-candidate list so users can see durable changes and queued review work, and promoted memory candidates expose an audited "forget" action that records a normal `forget` teaching event.
69. Agent-growth Phase 9 adds the growth evaluation suite as a persisted rollup, not a second harness. `magician_v2::learning::growth_eval` reads existing learning candidates/events, eval backlog and run reports, Skill Evolution validation and promotion evidence, OPC runtime state files, current procedure YAML, procedure feedback/retrieval/deprecation events, memory eval Parquet rows, and LLM-call token telemetry, then writes `learning/evaluations/growth_runs/<run_id>.json`. Reports score memory recall/precision/correction, skill reuse, capability improvement success, autonomous program progress, user feedback incorporation, false-learning prevention, prompt-token growth, tool-failure reduction, and Phase 15 procedure-quality dimensions: extraction precision, retrieval relevance, misuse rate, helped/hurt outcome signal, stale correction, duplicate procedure rate, and procedure-to-skill promotion quality. Scenario checks include the original growth scenarios plus procedure teaching/reuse, repeated success convergence, irrelevant-procedure withholding, correction updates, harmful/stale deprecation, and evidence-gated graduation into skill evolution. Procedure YAML is treated as current state while events/backlogs stay windowed; correction, stale/harmful deprecation, retrieval relevance, and withholding scenarios require temporally correlated target evidence, correction/deprecation checks account for every targeted procedure id independently, and retrieval relevance fails on any correlated explicit negative or bad judgement instead of majority-voting mixed feedback. `too_broad` / `too_narrow` verdicts create correction pressure rather than stale/harmful deprecation pressure unless deprecation is explicitly recommended, and procedure-to-skill promotion quality requires materialized candidate plus capability/eval backlog records rather than a promotion event alone. `POST /api/magician/v2/learning/growth-evaluations/run` creates a report; list/read endpoints, `internal_data`, learning audit counts, and the `/memory` panel expose the result. `blocked` means the system lacks enough recent evidence; only measured failures become `failed`.
70. Memory retrieval now uses the derived LanceDB index as a hybrid signal rather than a BM25-only sidecar. Rebuilds write candidate-level `documents.jsonl` plus a chunk-level `memory_candidates` LanceDB table with BM25 FTS text and a local embedding column; runtime indexes use Ollama `hf.co/mykor/pplx-embed-v1-4b-GGUF:Q6_K`, while deterministic hash vectors are compiled only through an explicit test-only feature used by tests. Long candidates split into overlapping chunks, and oversized `task_progress.notes` raw execution traces are compacted into deterministic index digests before embedding while the full canonical candidate stays in `documents.jsonl`. Search hits aggregate chunk scores back to the parent candidate key before prompt ranking, so large documents can match on their relevant section without one huge vector representing the whole item. If embedding is unavailable, rebuild fails and direct candidate ranking remains the runtime fallback. Compatible LanceDB tables now update through row-level `merge_insert` keyed by `chunk_key`: `row_hash` changes update rows, new chunks insert, and chunks missing from the current source snapshot delete. Missing or incompatible tables still fall back to an atomic full replacement. Chunk embeddings are progressive: provider/model/dimension-scoped cache entries are keyed by the exact embedding input, so new or edited files embed only changed chunks while unchanged vectors are reused. Row-level merges run best-effort LanceDB optimize maintenance when at least 500 rows changed or at least 20% of a 100+ row table changed; this compacts files, prunes old versions, and moves recently merged rows into index structures without making optimize success part of retrieval correctness. Background and HTTP-triggered rebuilds write `memory_index_lancedb_rows_updated`, `memory_index_lancedb_replaced`, and `memory_index_lancedb_optimize_*` memory-event rows so dashboards and `internal_data.query_memory_events` can inspect row mutations and optimize health. Fresh or soft-stale prompt rendering uses LanceDB's native hybrid query/RRF path to fuse FTS and vector ranks, then applies the existing deterministic boosts and budget packing over canonical tier JSON candidates; hard-stale, missing, or unreadable indexes still fall back to direct ranking. Per prompt render the hybrid index is scored **once** and the result is shared across all memory tiers rather than re-scored per tier: `score_hybrid_index_for_prompt` produces a single scored candidate set and `render_memory_tiers_for_prompt_with_scores_result` recomposes every tier block from it (`memory_prompt_blocks.rs`, re-exported via `agents/mod.rs`; called from the `v2_orchestrator.rs` memory inject path), removing the prior 3×-per-render hybrid scan on the run-startup critical path with no change to ranking output. Soft-stale means source hashes or document counts changed while a compatible index exists, so the existing index remains usable as a ranking hint until the background rebuild catches up. Agent-definition hashes are computed from stable definition YAML bytes instead of serialized Rust structs to avoid false source-change loops from unordered map fields. The manifest backend is `lancedb-hybrid-v2` and records provider/model/dimensions so incompatible indexes are rebuilt. The memory-index maintainer coalesces dirty scoped memory/definition changes behind `MAGICIAN_MEMORY_INDEX_DEBOUNCE_SECS`, periodically scans scoped memory stores, and rebuilds stale/missing indexes; eval rows compare `direct` against `lancedb_hybrid`, and hard-stale/missing/unreadable indexes surface as `direct_fallback`. Operators can run the same scoped index path offline through `magician.bin memory-index status|rebuild|optimize`; `--force` is opt-in for hard rebuilds, while `make run-supervisor` prewarms with `memory-index-prewarm` immediately before starting the supervisor unless `MAGICIAN_MEMORY_INDEX_PREWARM=0` is set, rebuilding only hard-stale indexes before startup. A process-wide hybrid result cache, on by default, may reuse a revision-bound score map for an exact expanded query against the same authorized snapshot; `MAGICIAN_MEMORY_HYBRID_RESULT_CACHE=off` is the kill switch. Request-path hybrid search checks out an atomic pair of independent pooled table handles for the concurrent vector and FTS legs and may retain them in a generation-keyed pool; writers invalidate that pool before replacing or quarantining the live directory; `MAGICIAN_LANCE_TABLE_POOL=off` disables cross-request reuse. An exact-query vector LRU (on by default) may skip a repeat embedding for the same contract-plus-query identity; distinct same-contract queries already queued may share one physical `/api/embed` call (Magician default window 3 ms / max items 8), but a lone chat miss never waits for the gathering window (`MAGICIAN_QUERY_VECTOR_CACHE=off` and `MAGICIAN_EMBEDDING_QUERY_BATCH=off`). The hybrid vector leg Magician default is ANN (`runtime.retrieval.vector_search: ann`: IVF_PQ shortlist plus exact L2 rerank, flat fallback). `MAGICIAN_VECTOR_SEARCH=flat` restores exhaustive KNN. `ann_shadow` still serves flat keys after FTS+flat using leftover retrieval budget. Each hybrid request captures vector-search settings once for both the result-cache identity and the vector leg; cache keys include ranking epoch, candidate_multiplier, and nprobes.
71. The Activity learning feed under Today is a curated user-facing projection over learning candidates, high-signal learning events, evaluation reports, growth rollups, and evaluation backlog items. Reviewable user-memory candidates project as `learning_candidate`; informational insights project as `learning_insight`; generic assistant chat messages are no longer materialized as Today/Activity rows. Learning insight actions are source-backed: archive writes a tombstone, save-to-memory creates a reviewable memory candidate, and create-follow-up creates an approved V3 task with idempotency recovered from either the learning event or follow-up marker. Attention Bar status buckets filter back to urgent task/approval/escalation rows, while internal debugging uses `internal_data list_learning_feed_insights` to inspect the same source projection and summary counts as the user-facing feed. Task visibility is a two-state model with no archival concept: user-visible tasks live at `<scope>/tasks/<id>/` and are returned by `list_tasks` / the regular `/tasks` view; internal tasks (chat-spawned delegate transients, sub-goals, handovers, system seeds, debug-page execution runs) live at `<scope>/internal_tasks/<id>/` and are returned by `list_internal_tasks` / the `/tasks?type=internal` view. The discriminator is `TaskManifest.lifecycle: TaskLifecycle` (`Persistent` for user-visible, `Internal` for everything chat/runtime-spawned); `task_is_user_visible` keeps legacy `system:` prefix and `__system__` agent/created_by heuristics as fallbacks for pre-schema on-disk data. `Internal` collapses the former `EphemeralOwnedByChat` + `InternalDebug` variants — both old on-disk strings deserialize as `Internal` via serde alias, and auto-cleanup keys solely on `chat_session_id` presence so debug runs (no session) are never swept. Write-time routing happens in `create_task` via `ensure_task_workspace_for_lifecycle`, which materializes the right `<root>/<id>/` before any derived path is computed; reads go through the `task_dir` probe (`internal_tasks/<id>/` first, fall back to `tasks/<id>/`) so the same call site routes correctly for both folders. Deletion (`archive_task_with_options(.., remove_files=true)` and `delete_internal_task`) cascades through `remove_task_dir_for_delete` (which probes both folders) and `cleanup_task_external_artifacts` (which drops published-surface JSONs + materialized muij layouts + index rewrite, then removes the feed-projection summary). Episode records and chat-pack execution dirs are intentionally NOT cascaded — episodes are agent-scoped and outlive tasks by design; chat-pack execution dirs are cleaned by the chat-session deletion path (`chat/storage.rs::cleanup_ephemeral_tasks_for_session`) using `TaskManifest.chat_session_id`. The plan doc is `docs/archive/plans/2026-05-21-task-visibility-two-folder-model.md`; cascade-delete operational spec is `docs/components/magician/task-lineage-cleanup.md`; API reference is `docs/components/magician/internal-tasks-api.md`.
72. Scoped Parquet lakehouse DuckDB access is serialized through an analytics-only process guard. `/llm` and `/memory` query handlers, internal-data lakehouse queries, growth-eval rollups, LLM-call Parquet writes, and memory-event Parquet writes all open short-lived in-memory DuckDB connections behind that guard and set `PRAGMA threads=1`. Memory-event analytics rows additionally flow through a small in-process batcher that flushes every 5 seconds or 100 rows per scoped analytics root. To further optimize dashboard load times under the guard, both `llm_calls` and `memory_events` analytics query handlers parse SQL time boundaries (e.g. `interval X days` or `timestamp_ms >= XXX`) to aggressively prune Parquet partition inputs before establishing DuckDB views. This keeps observability fire-and-forget while preventing dashboard fan-out or memory-consolidation bursts from opening many concurrent DuckDB scans/writers over the same scoped Parquet files.
73. Runtime event and catalog health warnings are treated as correctness signals. Progress-channel normalization must resolve scope from execution lineage, then envelope-level `principal`/`workspace`, then payload fields so scoped chat and agent lifecycle events are not dropped. Harness program-state tools are compiled harness tools and must remain in both compiled-provider allowlists and deferred-provider allowlists until the harness provider registers them. Scoped skill YAML must parse cleanly at startup; long action descriptions that contain colon-heavy command guidance should use block scalars. Native memory episode indexes are derived caches: missing files referenced by a fresh-looking index trigger a quiet index rebuild instead of repeated WARN rows, memory-index rebuild failures log/persist the full error chain for LanceDB/Ollama diagnosis, and stale question-restoration references to already-deleted executions are debug noise rather than operator-facing warnings.
74. Agentic approval policy normalizes flat toolskill leaves before HITL matching. A call surfaced as `<pack>__<action>` is policy-visible as `tool: <pack>` with the leaf action as fallback; if the call carries a nested `tool_name` argument, that nested value becomes the approval action. Toolskill wrappers that pass JSON strings in `arguments_json` may expose selected parsed booleans such as `confirmOrder` for `param_matches`, so final external side effects can use normal Attention approvals instead of wrapper-local chat confirmations. Zepto checkout is wired this way on the executive-assistant agent only: `zepto-mcp` is not a personal-assistant grant, and Vera's approval policy gates Zepto order/payment tools when `confirmOrder` is true.
75. Swiggy MCP uses the same remote-toolskill approval shape as Zepto, but one `swiggy-mcp` wrapper selects the service endpoint by `service` (`food`, `instamart`, or `dineout`) and keeps `mcp-remote` OAuth cache state per service. Live OAuth verifies these as three distinct MCP servers with separate client/token cache directories, even if the browser login session lets later services authorize quickly. Vera owns this tool grant; Presto does not. The documented and live final Swiggy actions are approval-gated at the nested remote action boundary: Food `place_food_order`, Instamart `checkout`, and Dineout `book_table` pause through normal HITL/Attention before execution, while service reads and cart/slot preparation remain normal tool calls.
76. Scoped `analytics.duckdb` durability is decoupled from DuckDB's on-disk storage format so a bundled-DuckDB version bump can never silently drop events. The embedded DuckDB is pinned exactly (`duckdb = "=1.10502.0"` in `magician/Cargo.toml`), which transitively locks `libduckdb-sys` and therefore the storage format; the format changes only in a deliberate PR that edits the pin. The analytics event sink dual-writes every batch to a version-stable Parquet mirror (`<scope>/analytics/events/dt=YYYY-MM-DD/batch_<ulid>.parquet`), independent of DuckDB's format. On open, an `analytics.duckdb` this binary cannot read is renamed to `analytics.duckdb.incompatible-<ts>` (preserved, never deleted), logged at ERROR, recreated fresh, and its `events` table rebuilt from the Parquet mirror; lock conflicts are never treated as corruption. Per scope the dispatcher owns one shared read-write pool while query/read paths use a coexisting read-only connection, removing the multi-opener RW-conflict class. The `LlmParquetRetention` sweep now ages out both `llm_calls/dt=*` and `events/dt=*` partitions. Deliberate cross-version migration uses the `magician analytics export|import` subcommand (`make analytics-export` / `make analytics-import`): EXPORT with a binary built at the old pin, IMPORT with the new. Runbook: `docs/runbooks/2026-06-13-analytics-duckdb-version-migration.md`.

### LLM Routing
1. `ConfiguredRouter` resolves profile → provider → model from `magician-config.yaml` operation mappings.
   - `minimax` is a first-class provider kind in `magicllm`, backed by MiniMax's **OpenAI-compatible Chat Completions API** (`/v1/chat/completions`) for text, vision, tool calling, tool results, streaming, and reasoning. M2.x route through it as text-only; M3 additionally accepts image input. (The earlier Anthropic-compatible Messages wrapper has been removed — the native `chatcompletion_v2` surface blocks image input and the Anthropic surface rejects multimodal tool results, so the OpenAI-compatible surface is the only one that supports the full vision + tool-result loop.)
   - `deepseek` is a first-class provider kind in `magicllm`, backed by DeepSeek's Anthropic-compatible Messages API. Configured profiles use `deepseek-flash` (DeepSeek-V4.1-Flash) for text, vision, thinking, tool, JSON, and streaming. Thinking defaults on; profiles that need a fast path send `thinking.type: disabled`.
   - Vision is enabled for `minimax` M3 profiles (image content-parts via the OpenAI-compatible surface) and `deepseek-flash` profiles. MiniMax M2.x remain text-only; the routing gate checks the selected model, not just its provider.
   - The `agentic_decision` outer-loop decider routes to GPT-5.6 Terra by default; DeepSeek V4.1 Flash and M3 (vision `when_has_images` fallback) and Yutori-on-image are config-selectable alternatives. `deepseek-flash` can take images itself. The transport-cohort guard permits the cross-provider swap when at most one side rides the OpenAI Responses stateful chain.
   - Anthropic-family `tool_result.content` is normalized to spec (string or content-block array) — a bare object is serialized to a JSON string so the stricter DeepSeek Anthropic-compatible deserializer accepts it (Claude tolerated the object). `reasoning_tokens` for the Anthropic family is estimated from the captured thinking text, since Anthropic-style usage folds thinking into `output_tokens` with no separate field.
2. Profiles carry metadata: `openai_api_mode` (chat/responses/auto), `tool_choice` (auto/any/specific), `streaming` (true/false).
3. `route()` (sync) and `route_stream()` (streaming) apply the same profile/metadata resolution. Streaming checks the `streaming` gate before calling `invoke_stream`.
4. Fallback profiles are supported in `route()` (retry loop). `route_stream()` does not retry — streaming failures are terminal.
5. For non-streaming calls, provider adapters may retry once on documented token-truncation stop reasons (`max_tokens`, `length`, `MAX_TOKENS`, or token-related `incomplete_details.reason`) by increasing `max_output_tokens`, with one backed-off retry if the larger budget itself is rejected.
6. Magician's operation router preserves provider `finish_reason` and refuses to parse still-truncated structured responses, so long agentic and planning calls fail as truncation instead of surfacing later as generic `{}` parse errors.
7. For rich chat, OpenAI-backed profiles should prefer `openai_api_mode: auto` over `chat`; `chat` remains intentionally limited for multimodal/tool-result replay, while `auto` can move eligible turns onto Responses without forcing every OpenAI request onto that surface.
8. Operation-focused OpenAI reasoning profiles that combine reasoning with tools are pinned to Responses mode where required. High-budget reasoning profiles remain available for PlanGraph planning, memory insight distillation, and user promotion.
9. Operator-facing Settings can live-reload `magician-config.yaml` into the running process. The reload path rebuilds the operation router, multi-LLM service, native tool-calling policy, tool-authorization policy, and complexity-routing config from the current YAML and returns warnings when a parsed config cannot produce a live in-memory router. The LLM dispatch queue routes through the operation router's current configured router (`live_dispatch_router`), so a reloaded operation mapping takes effect on the next call without a restart. Other config areas still remain restart-only and are reported back as such.
10. The implemented intent-routing foundation is now archived at `docs/archive/plans/2026-04-10-intent-based-model-routing-design.md`, while the remaining consensus/MoA follow-on work stays live at `docs/archive/plans/2026-04-12-consensus-model-routing-design.md`.
11. Workspace HTTP clients require reqwest 0.12.28's null-safe system-proxy stack. Explicit proxies and `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` (plus `NO_PROXY`) remain supported, and macOS platform proxy discovery is used when SystemConfiguration is available; an unavailable dynamic store now yields no platform proxy instead of panicking during client construction. Because the workspace intentionally does not commit `Cargo.lock`, every direct workspace reqwest declaration carries the audited 0.12.28 lower bound and explicit `system-proxy` feature. Provider defaults still honor `MAGICIAN_DISABLE_SYSTEM_PROXY=1` as an explicit operator override. The Makefile test target sets that override for hermetic HTTP tests, not as a crash workaround.
12. A background operation follows the engine that started its flow (the chat mouth, the run engine, or a connected CLI's family) — resolved per request from the flow's routing overrides or the `parent_engine` task-local, never a process value — unless the operation's selector says `engine: pinned` or the owner pins it from Settings; a local default or a tool-carrying request never follows. The owner's per-operation pins live beside the profile overrides (`system/llm_routing_engine.json`, store over config) behind `GET /llm/routing` (`affinity_scope: flow`, `driving_engines`, per-operation `follows_parent` / `parent_profiles`) and `PUT/DELETE /llm/routing/{operation}/engine`, registered in `magician-bin` beside the profile-override routes. See [llm-routing-overrides.md](components/magician/llm-routing-overrides.md).

### Delegation
1. Personal agent's planner emits `Delegate: <agent_id>` hints in the taskplan.
2. The agentic decision LLM sees delegation targets and can emit `DelegateToAgent` decisions.
3. The executor validates target against `delegation_targets` (supports wildcard `*`).
4. Delegation chains are allowed (Personal→Worker→Worker), bounded by `max_delegation_depth` in `CoordinationConfig` (default 2). Cross-agent chain depth is enforced in `dispatch_delegation` with cycle detection. Within-agent sub-goal depth is enforced in the executor at `ctx.depth >= ctx.max_delegation_depth`.
5. Delegate tools are merged into the personal agent's tool list with `providing_agent_id` tags. `system:*` agents are excluded from normal runtime delegate catalogs, wildcard target expansion, planner delegate workers, and child-spawn targets; they still run through internal scheduler/meta-agent paths but cannot appear as the owner for user-facing tools such as `browser`.
6. Chat ask-mode now enters the active session agent through `GoalSource::ChatInline`, so specialist routing uses the normal agentic delegation and capability catalog. The old chat-local registry path is removed; domain work must be exposed as capabilities available to the active agent.

## Ports
- `3002`: `magician`
- `3003`: `magicutor`
- `8081`: `magic-supervisor` control plane

## Durable Artifact Tag Index

`DurableArtifactStore` maintains an in-memory `TagIndex` (shared via `Arc<RwLock<TagIndex>>`) that maps derived tags to artifact references. Tags are computed from frontmatter provenance fields — never persisted:
- Namespace tag (e.g., `execution_artifacts`, `task_state`, `surfaces`)
- `task:<task_id>` from `source_task_id`
- `agent:<agent_id>` from `source_agent_id`
- `execution:<execution_id>` from `source_execution_id`

The index is built from disk on startup, updated on `write()`/`delete()`, and refreshed via `reindex_artifact()` after out-of-band frontmatter mutation (executor provenance stamping). `list_by_tags()` returns artifacts matching all provided tags (intersection). Runtime artifact-listing tools query by `task:<id>` provenance tag instead of aliasing `task_id` to namespace.

### Resource Authority Layer 2 + Phase D (v0.6.590 + v0.6.592)

The resource-authority design (`docs/archive/plans/2026-05-20-agent-resource-authority-design.md`, implementation history archived at `docs/archive/plans/2026-05-20-resource-authority-layer-2.md`) is mechanism-complete. Declarative budgets in `magician-config.yaml > resource_authority` lazy-issue `SpendToken`s through `ConfigSpendTokenResolver`; every compiled-pack dispatch path routes through the caller-agnostic `execution::compiled_dispatch` primitive (`try_dispatch_compiled_pack`, `dispatch_with_gating`, `execute_maybe_gated`) carrying a shared `CompiledDispatchAuthority` bundle. Five paths consume it: chat fast path (`active_owner: chat:{session_id}`), autonomous outer-loop pack branch (`autonomous:{execution_id}`), inner-loop `CompiledProviderDispatcher`, inner-loop heavyweight `non_inner_loop_pack` branch, and the skill cli-template path (all three inner-loop entries namespace with `inner-loop:{thread_id}`). Every `CapabilityProvider::lower()` impl honours `execution.spend:` uniformly through the shared `pack_provider::maybe_wrap_with_spend_gate(...)` helper. `CapabilityProvider::execute_direct` is strict-mode: it errors on `Gated` instead of running unbilled.

**Phase D (v0.6.592)** finishes the dispatch model with per-`(principal, workspace)` authority resolution shared by the gate and `/budget` REST API (`scoped_authority::DiskBackedScopedResolver` — `OnceCell`-per-key cache, `spawn_blocking` cold-load), gate auto-persistence after every dispatch (atomic tmpfile + fsync + rename + parent-dir fsync via `persistence::atomic_write`), `allow_transitive_delegation` enforcement and delegation-time spend-token validation in `validate_delegation_request`, path-traversal whitelist (`scoped_authority::is_safe_scope_id`) on scope ids, and a lock-ordering deadlock fix between gate and three REST handlers that previously inverted `token_store` → `ledger` acquisition order. Per-scope state is now the single source of truth — the `/budget` UI reads the same in-memory Arcs the gate writes to. Integration tests cover the gate hot path, per-scope isolation, and path-traversal rejection.

The config flag `resource_authority.enabled` ships `true` with live `EMAIL_SENDS` / `WHATSAPP_SENDS` / `INR` / wildcard `USD` budget rows. Chat dispatch, MCP checkout (chat and app governed-MCP owners), REST reserve, and app compiled/OS-jail tool I/O share `spend_session::admit` (Magician `0.7.3`). App MCP does not wrap a second spend gate. System/token ceilings occupy **live in-flight** reservations (stacked `batch_id` counted once), so a hold that started before midnight still fills today's cap. Existing config tokens copy YAML ceiling/period/carryover on first lookup after restart. After remote MCP checkout success, `InputRequired` / transport errors / app label-or-settlement faults return a non-retry success instead of inviting a second INR debit. REST ceiling remaining uses the same in-flight figure (`0.3.5`). See `docs/quickstart.md` §6 and `docs/components/magician/resource-authority-api.md`.

### Mid-flight cancellation chain (v0.6.594)

Cancellation propagates end-to-end across chat-turn cancel (`DELETE /chat/sessions/{id}/run`), SSE client disconnect, and task cancel (`POST /v3/tasks/{id}/executions/{eid}/cancel`). Chat- and task-cancel now use the same `tokio::select!`-against-LLM-future discipline plus shared cascade primitives.

**Chat-turn path.** `ChatService::active_chat_runs` holds a per-session `CancellationToken`. `cancel_chat_run(session_id)` atomic-removes + cancels it. `process_chat_inline_turn` races the outer LLM call against `cancel_token.cancelled()` — dropping the future closes the reqwest HTTP/2 stream, sending `RST_STREAM CANCEL` upstream so OpenAI / Anthropic stop generating and stop billing within ~50ms. Iteration boundary, per-tool break, and pre-persist short-circuits prevent orphan history rows. The token threads through `dispatch_chat_tool_call` → `dispatch_capability_pack` → `dispatch_inner_loop_action_anyhow`, and is also stamped on `InnerLoopExecCtx::cancellation_token` as a fallback (dispatch consumes explicit-or-ctx).

**Cascade to spawned executions.** Chat-dispatched delegate / handover executions spawn bounded watchdogs that race `parent_token.cancelled()` against 5-second `get_task` polling + 3-hour safety deadline. On chat-cancel the watchdog calls `ArtifactV2Service::cancel_execution_by_id` — same path `dispatch_stop_task` uses — which routes into `Orchestrator::cancel_execution_tree` (DFS over the generation-tagged `active_execution_controls` + `active_delegation_group` + `child_execution_ids`). When the spawned work terminates naturally, the watchdog exits cleanly without firing cancel.

**SSE client-disconnect → cancel.** `SseDisconnectCancelGuard` is wrapped around the streaming response via `futures_util::stream::unfold((Box::pin(stream), guard), …)`. On Drop (browser tab close, network drop, `AbortController.abort()`), the guard inspects `chat_response_slot` first — if the watched turn already wrote its response there, any subsequent drain belongs to a different turn and we skip the cancel (cross-turn race avoidance). Otherwise calls `cancel_chat_run(session_id)`. Runtime-aware via `Handle::try_current()`.

**Task path.** Pre-existing; unchanged by this release. `execute_agentically` had `tokio::select!` against the decision LLM future and action execution already. With this release, chat-spawned inner loops share the exact same iteration-boundary + select! discipline because the token now reaches them via `InnerLoopExecCtx`.

**Known gaps.** DuckDB `spawn_blocking` is not cancellable by future drop (inherent duckdb-rs limitation; deferred). Voice orchestrator's cancel surface is not yet a `CancellationToken` — `dispatch_external_tool_call` already accepts the parameter (one-line wiring when voice migrates).

## Removed Surfaces
- `magictunnel` crate/binary
- `/magictunnel` UI routes
- `ui/unified-ui/src/lib/magictunnel/*`

## Related Docs
- `README.md`
- `docs/quickstart.md`
- `docs/components/magic-supervisor/supervisor.md`
- historical material in `docs/archive/`
### V3 Progress Channels

V3 task workspaces now publish normalized progress projections into the shared progress-channel router rather than reconstructing external delivery from legacy task mirrors. Webhook and chat delivery consume those normalized `ProgressMessage`s directly, and V3-origin messages are explicitly marked with `source: projection`. `agent_memory` still listens on the same router, but now deliberately ignores projection-origin messages so only real execution/lifecycle progress becomes episodic memory.

### First-class agentic stream taxonomies (v0.6.460)

Magician emits three structured event taxonomies modeled on AG-UI / assistant-ui / CopilotKit, all delivered through the existing `AgentEventEnvelope` transport (no enum touch):

- **Plan**: `plan.snapshot` (full structured tree at approve), `plan.step.started`, `plan.step.finished` — UI consumers no longer have to parse the markdown plan body to know step structure or status.
- **Tool**: `tool.call.started`, `tool.call.args` (reserved for streaming), `tool.call.finished` — emitted around every inner-loop primitive dispatch with arguments, success, duration, exit code, and error. Governed app arguments cross durable event boundaries only as an omission marker, canonical digest, and bounded shape.
- **Reasoning**: `reasoning.start` / `.content` / `.end` — emitted whenever the LLM response carries non-empty extended-thinking output, keyed by `{thread_id}::iter_{N}`.

Helpers live on `RuntimeTransportBroadcaster` (`magician/src/magician_v2/realtime_events.rs`). Plumbing threads `Option<Arc<RuntimeTransportBroadcaster>>` through `InnerLoopExecCtx → InnerLoopRequest`. Tests / orphan dispatches default to `None`. UI consumes via `unified-ui/src/lib/stores/agenticStreamStore.ts`.

Streaming `reasoning.content` and `tool.call.args` deltas wire end-to-end as of magician v0.6.461 / magicllm v0.1.33. magicllm's `StreamDelta` enum gained six lifecycle variants, the Anthropic provider's streaming parser fires them, and the inner-loop runner installs a fan-out sink via `magicllm::scoped_stream_event_sink` around each LLM call (a `tokio::task_local!` so the router/chain layers stay unchanged). `plan.step.started` is back-filled in the executor before every `plan.step.finished` so consumers always see a started→finished pair. HITL standardization (review item 4) remains deferred.

### Internal Diagnostics Capability (v0.6.512)

`internal_data` is a provider-backed inner-loop compiled capability for Magician-internal analysis. The outer agent sees a shallow diagnostic tool; the inner loop loads the focused YAML guide and native action catalog, then dispatches primitives through `InternalDataProvider`. It is read-only and anchored to `ArtifactV2Workspace`, exposing scoped discovery, analytics `SELECT`/`WITH` queries, LLM-call Parquet queries, memory-event Parquet queries and summaries, task/execution listing, task output reads, `events.jsonl` reads, and execution file reads for prompt projections/runtime context/traces. `internal-system-analyst` uses this as its primary evidence gateway before moving into broader DuckDB analysis.

### Delegate Observability + Output-Format Rebalance (v0.6.721)

A delegate that a chat turn dispatches runs as its own task execution, so its typed transport events (the `tool.call.*` / `llm.*` / reasoning taxonomies described above) are stamped with that execution's `task_id`/`execution_id` but carry no `chat_turn_id`. The per-chat-turn event sink (`magician/src/magician_v2/chat/chat_turn_event_sink.rs`) keys on `chat_turn_id`, so before this release it silently dropped every delegate event — the originating chat turn went dark for the entire delegated run while the panel still showed the parent waiting. The fix is a cycle-safe recovery path anchored on the new `ChatFanoutResolver` (`magician/src/magician_v2/realtime_events.rs`), which holds `Arc<DashMap>` views of `chat_fanout_by_task` and `canonical_scopes` — deliberately *not* the broadcast `Sender`, so the sink can reverse-resolve a turn without re-entering the broadcaster and forming a reference cycle. It exposes `has_fanout()` and `resolve_turns(task_id, execution_id)`. When an inbound event lacks `chat_turn_id`, the sink deep-scans the event's `task_id`/`execution_id`, asks the resolver for the originating chat turn(s), stamps `chat_turn_id` plus `principal` and `workspace` back into the event data, and then persists and broadcasts once per resolved turn. The effect is that a delegate's live activity re-attaches to the chat surface that spawned it, and the same enriched event feeds the deep-work inspection panel.

Those same events now carry real LLM telemetry instead of zeros. The flat-loop agentic decision call emitted `RuntimeTransportEvent::LLMResponseReceived` with every token field — input, output, cache-read, cache-creation, cost, model — pinned at `0`, because `decide_next_action` (`magician/src/.../execution/agentic/decision.rs`) was handed an unused `_telemetry_slot` and the `ExecutionNativeResponse` usage was discarded after parsing. The decision metadata type `NativeDecisionMetadata` (`execution/agentic/native_integration.rs`) gained a `telemetry: Option<LlmCallTelemetry>` field populated in `from_envelope_and_response` from the response's prompt/completion/cached/cache-creation token counts, and `decide_next_action` now writes that telemetry into the slot the executor reads when it emits `LLMResponseReceived`. So the LLM-decision signal now flows end-to-end — decision call → telemetry slot → `LLMResponseReceived` event → `events.jsonl` — and a new run's delegate calls report genuine usage. Runs already on disk stay frozen at `0`; only fresh executions backfill.

The execution-panel adapter surfaces that richer signal per event. `run.activity_log` is assembled in `execution_panel/v3_adapter.rs` by `event_to_feed_item`, which previously copied only `{event_type, execution_id, storage_backend}` into each item's metadata and threw away the rest of `event.payload`. A new `activity_metadata()` helper now lifts `latency_ms` onto every event, token counts plus `model`/`provider`/`cost` onto `llm.*` events, and tool `name`/`action_type` onto `tool.*` events, straight from `event.payload`. The deep-work panel reads those fields to render per-call stat chips (model, in→out tokens, percent cached, latency), so a delegated run is now legible at the per-event grain rather than as an opaque "running" badge.

Finally, the task-user output format default flipped from Markdown to HTML. The synthesis system prompt was rebalanced to `data/magician_v2/prompts/task_user_output_synthesize_system_v1.3.0.json`: `text/html` is now the default media type for substantive deliverables, `text/markdown` is reserved for extremely rudimentary output, and `application/json` MUI dashboards remain the choice for data-rich results. The earlier prompt picked Markdown for anything "prose-heavy," which meant nearly every deliverable landed as a `.md` file. The pinned constant `TASK_USER_OUTPUT_SYNTHESIZE_SYSTEM` is bumped `1.2.0 → 1.3.0` in `prompts/constants.rs`, and `artifact_v2/writers.rs` `preferred_media_type(TaskUser)` flips `text/markdown → text/html`. Both target rendering paths already existed — the sanitized HTML allowlist and the `data-magician-source` live-chart dashboard renderer — so this is a format-selection change, not new rendering machinery.

### Agentic-Loop Termination: Bounding Refinement (v0.6.722)

The flat agentic loop has a characteristic failure tail: once the substantive work is done, the decision LLM keeps re-globbing the artifact store, re-reading its own outputs, and re-fetching records it has already seen to re-confirm state it already knows. Each of those turns is a paid LLM call that surfaces nothing new — the "refinement overdone" pattern. This release adds two composable termination paths that bound that tail without retiring genuinely useful refinement, plus a durable-state prerequisite that makes the stronger of the two reliable.

That prerequisite is a Patch UPSERT in `apply_outer_task_state_action`'s `Patch` arm (`execution/agentic/executor.rs`). The decider routinely jumps straight to recording micro-goal progress through a `TaskStateActionKind::Patch` without ever having emitted a `Create`, and the previous arm rejected those with "patch action has no existing durable task state," so the durable task state was never actually maintained. The arm now treats patch-before-create as create-then-patch: when `load_persisted_durable_task_state` returns `None`, it synthesizes a baseline through the same `synthesize_durable_task_state` the `Create` arm uses, then applies the patch onto it via `apply_durable_task_state_patch`. This removes the rejection warnings and — critically — keeps the micro-goal ledger live, which is what the definition-of-done path below reads to know when a run is finished.

Phase 1 is a no-progress cutoff layered onto the orchestrator's existing stuck-auto-yield. `try_synthesize_stuck_auto_yield` (`execution/agentic/decision.rs`) already short-circuits repeated-same-error loops before the next LLM call; it now also walks the iteration history in reverse and counts the trailing run of consecutive *successful read-only / inspection* steps. At `NO_PROGRESS_AUTO_YIELD_THRESHOLD` (10) it synthesizes a `Decision::Yield` and concludes without another LLM call. The classifier `iteration_is_read_only` marks a step read-only only when it succeeded *and* its action is a known inspection tool — `read_file`, `grep`, `glob`, `find`, `ls`, `tool_search`, `*_get` / `*_list` / `*_describe` / `*_help` / `*_status` suffixes, browser reads (`snapshot`, `state`, `find`, `network`, `console`, `screenshot`, `eval`) — or a `shell` / `http` / `duckdb` step that carries no mutation marker. Later correction (kept in `decision.rs` and `0.6.1235` docs): `*_search` and `*__run` are progress, not inspection — treating them as read-only falsely Failed research agents. That suffix table is the agent-loop detector only; app-tool IO class is schema-driven in `app_tool_bind` and must not be imported here. `sig_has_mutation_marker` flags the acting signatures (`requests.post`/`put`/`patch`/`delete`, `curl -X` / `--data`, `card_update`/`create`/`delete`, SQL `insert`/`update`/`delete`/`create`, `rm`/`mv`/`mkdir`/`sed -i`, output redirects, `.write(`, and so on), and any typed mutating, artifact-producing, terminal, or yield action breaks the streak outright. The bias is deliberately conservative — a run that is genuinely *acting* is never cut, only one that has lapsed into pure re-inspection.

Phase 2 is the definition-of-done auto-conclude, which ends the run the instant its tracked requirements close rather than waiting for the phase-1 streak to accumulate. `all_durable_micro_goals_resolved(ctx, executors)` (`executor.rs`) is true only when the durable task state tracks at least one micro-goal *and* every one is resolved (`completed` or `blocked`). It is deliberately stricter than `open_durable_micro_goals(..).is_empty()`, which is also vacuously true when the task tracks no durable state at all — requiring real micro-goals means a task that simply isn't using durable tracking, or one whose freshly synthesized baseline still carries the `in_progress` `mg_initial` goal, is never falsely concluded. The orchestrator computes it once per iteration and threads `definition_of_done_met` into `decide_next_action`; when set, the decision path returns a synthetic `Decision::Completed` plus `NativeDecisionMetadata::synthetic("definition_of_done", …)`, mirroring the stuck-yield synthesiser so the loop's terminal handling and locals stay intact. The safety net is that the executor's `Decision::Completed` arm re-runs `terminal_success_rejection` — the *same* deterministic evidence gate plus open-micro-goal check that a model-issued completion must clear — against the synthetic completion, so a premature or empty durable state can never force a false success: if the gate rejects, the synthetic terminal is discarded and the run continues.

The two compose into a single bound. Phase 2 is checked each iteration before the phase-1 streak threshold can fire, so in practice a run concludes the moment its durable micro-goals are all resolved; phase 1 is the backstop for runs whose durable state is *not* being maintained (no `Create`/`Patch` ledger to read), catching the same overdone pattern purely from the trailing read-only streak. Together they cap the refinement tail from both directions — by tracked-requirement closure and by observed inactivity — while leaving any run that is still mutating, producing artifacts, or surfacing new findings free to keep refining.

The deep-work panel surfaces the decision content behind these terminals. `execution_panel/v3_adapter.rs`'s `activity_summary` now gives substance to LLM and decision rows — `llm.succeeded`/`llm.failed` render a `decision_summary` (the concrete `Execute: pack:tool_search(…)`-style action), and `agentic.decision_made` renders the model's real reasoning when present, otherwise the chosen `action_summary`, skipping the synthetic `"Native pack tool call:"` placeholder. `humanize_event_type` produces cleaner titles (`Decision` / `LLM call` / `LLM response`), and `activity_metadata` copies `capability` onto `llm.*` rows so the UI can title them "Thinking with `<capability>`". (The model's true chain-of-thought is not surfaced by gpt-5.6-terra Responses — only the decision/action content is shown; a CoT-capable model would flow through this same path automatically.)

### Capability-Aware Multi-Agent Decomposition & Ordered Delegation (v0.6.723)

A chat turn binds a task to exactly one agent and never decomposes it, so a single ask spanning multiple capabilities — *research → ELI5 → 12-panel comic* — landed wholesale on `web-researcher`, which owns none of the comic-strip / image-generation capabilities (only `creative-mind` does). The deliverable was impossible from the start and nothing detected the mismatch. Because the flat agentic loop is the sole executor and `PlanGraph`/`PlanStep` is dormant for execution, the fix lives in the live decision layer, not a plan-step executor: the orchestrator decomposes by capability and runs *ordered delegation rounds*, threading each stage's produced artifacts into the next.

The first move un-brakes `delegate_to_agent`. Its catalog description (`native_catalog.rs`) previously branded a sequential A→B delegation an "anti-pattern"; it now teaches ordered rounds — delegate stage A, then on resume read A's results under a `## RECENT DELEGATION RESULTS` block, then delegate stage B seeding A's produced artifact ids into B's `input_artifact_ids`. The supporting context is rendered so the orchestrator can route without extra round-trips:

- `build_delegation_prompt_section` (`decision.rs`) now emits a `capabilities: …` line per delegate target, so capability-based routing needs no separate `get_agent_details` call.
- `build_delegation_results_section` (`decision.rs`, threaded through `build_decision_prompt_from_manager`) surfaces a parent execution's `delegation_results_ready` summary plus the produced artifact ids into the very next decision — the glue that threads stage-N output into stage-N+1.
- A new compiled handler `find_agents_for_capability(capability)` (`compiled_handlers/find_agents_for_capability.rs`, registered in `compiled_providers.rs`, the `native_integration.rs` hot list, `flat_loop/catalog.rs`, and `embedded_pack_defs/find_agents_for_capability.yaml`) is the inverse of `get_agent_details`: given a skill/pack capability, leaf, or agent id/name/alias it returns every reachable agent that owns it (`web_research` finds `web-researcher`).
- The `personal-assistant` persona gains an addendum: for an ask spanning capabilities held by different agents, decompose by capability and run ordered delegation rounds, seeding each stage with the prior stage's artifacts.

Phase 2 adds a determinism guard. `DelegationTargetRequest` (`actions.rs`) and the delegate schema (`native_catalog.rs`) gain an optional `required_capability` field; `validate_delegation_request` (`executor.rs`) cross-checks that the named target agent actually owns it and, on mismatch, returns a *retryable* `CapabilityNotOwned` error naming the real owner. The loop records the error and the LLM self-corrects on the next iteration. The check fails open when `required_capability` is unset, so existing delegations are unaffected. Phase 3 (durable per-stage ownership, a cached capability index, chat-dispatch bias) is pending and likely cut. Plan: `docs/archive/plans/2026-06-04-capability-aware-multi-agent-routing.md`.

### Inlining the Primary Media Artifact (v0.6.723)

A creative task generated a 12-panel comic as a 3.1MB JPG, but the final deliverable showed only the write-up — the headline image was silently dropped. The serving endpoint and UI inline render already worked; the loss was upstream tracking. The tool wrote the JPG to `/private/tmp`, the executor's capture gate rejected that path, and even a captured file never became a task `OutputRef`. Three changes close the path end-to-end so a headline media artifact renders inline:

- **Capture gate widen (M2a).** `is_allowed_tool_output_capture_path` (`executor.rs`) now also accepts `/tmp` and `/private/tmp` (which alias on macOS), so the lane-agnostic `capture_pack_action_artifacts` promotes media files a tool writes there into the task `outputs/` dir, typed and with a `task_download_url`.
- **Media selection pin.** `split_execution_artifacts` (`synthesis.rs`) ranks produced media files into the surviving set past the `MAX_SELECTED_ARTIFACTS = 30` cap. Media is detected from the `tool_output_file` payload's `content_type` (`image/`, `video/`, `audio/`) — read from the payload because the record's top-level `content_type` is the JSON wrapper, not the file's.
- **`media_outputs` on `FinalizedOutputs` (M3).** A new `media_outputs: Vec<OutputRef>` field (`models.rs`) plus a `collect_media_output_refs` finalizer helper (`service.rs`) register promoted media files as `audience=user`, `role=user_media` `OutputRef`s (`relative_path` = the path under `outputs/`, `media_type` from the payload). The reducer (`reducer.rs`) upserts them into `task.refs.outputs`, after which the existing serving endpoint, the `ChatContentBlocks` inline `<img>`, and `append_media_artifacts_to_document`'s HTML embed all light up.

M1 (CLI-dispatcher artifact discovery) was cut as redundant with the lane-agnostic executor capture. Phase 2 (video/audio inline + prominence) and Phase 3 are pending and likely cut. Plan: `docs/archive/plans/2026-06-04-inline-primary-media-artifact.md`.

### Run-Startup Latency & Double-Synthesis Root-Gate (2026-06-18, magician 0.6.851)

This effort cut per-run startup cost on the completion/critical path and removed redundant task-level synthesis on delegating runs. The engineering-manager coordinator is kept (deciding who implements is its job) — only its cost is reduced, with no hardcoded agent IDs. Plan: `docs/archive/plans/2026-06-17-vibedev-run-startup-latency.md`.

- **Task-level synthesis root-gate and terminal ownership.** Task-level output synthesis was extracted into `synthesize_task_level_outputs` (`artifact_v2/service.rs`) and is gated on `is_root_task_projection` at the 1.2/1.3 finalization boundary, so a delegating run synthesizes its task-level outputs **once at the root** instead of re-synthesizing on each non-root child (whose copy the root always overwrites). Controlled by `config.rs > task_synthesis_root_gate_enabled` (default-on); kill-switch `MAGICIAN_TASK_SYNTHESIS_ROOT_GATE=0` restores the prior always-synthesize behavior. The generic V3 terminal hook now detects canonical root lineage and converges with the child watcher on one child-terminal single-flight lock. Child transitions cannot own task status, root pointers, task-level outputs, or synthesis readiness; any legacy child-owned pointer is repaired to the recorded root without rewriting a concurrently edited task manifest. Tree-level learning reflection runs only for the accepted root output revision, while child episodes may still feed scoped memory consolidation. A concurrent observer waits and then sees the committed output, or retries if the first owner failed. The child's stage-1.1 `execution_output` still feeds the parent via `child_output_refs`.
- **Exact or deterministic parent continuation.** Ordinary model-selected delegation captures a durable exact checkpoint and resumes the same provider conversation with the completed child-result delta; pre-migration executions retain the bounded cold reconstruction path. Dynamic continuation sections are fingerprinted after an accepted provider turn, so unchanged observations, durable state, plans, tool inventories, and summaries are omitted; native tool results are not duplicated as prose, section removal emits a tombstone, and authority changes force a full bootstrap. A typed `delegate_to_agent` one-child launch has no unresolved parent decision, so it skips model-facing child-summary reconciliation and directly commits the child’s digest-verified deliverable before root completion. The exact projection is byte-preserving through execution, task-agent, and task-user outputs and bypasses duplicate synthesis/grounding. Execution-local routing is stored beside the root and each delegated child in `llm_routing_overrides.json`, read from the already-known task scope on hot paths, and applied to decision, synthesis, repair, precision judging, reflection, and memory consolidation, so an eval profile cannot fall back to the agent's production model after delegation or finalization while the task/agent configuration remains unchanged.
- **First simple task-agent projection.** On the first Markdown/plain-text projection, the already-synthesized execution output is persisted directly as the same-audience task-agent output instead of paying for a semantically redundant rewrite. Accumulation/reruns, structured formats, and user-audience synthesis retain their existing model-backed paths.
- **API mining detached from the completion path.** `run_mining_pipeline` (`v2_orchestrator.rs`) is now `tokio::spawn`'d off the run-completion path via a `weak_self` handle rather than awaited inline (inline fallback when the handle is unset, e.g. tests), so terminal finalization no longer waits on the cross-task re-mine. The mined-capability output is unchanged; only the invocation point moved off the critical path.
- **Memory embed-once.** The hybrid index is scored once per prompt render and shared across all memory tiers (see item 70 above), replacing the prior per-tier re-scan.
- **Pre-bound coding tools.** When a coding tool is directly granted, `flat_loop/catalog.rs` pre-binds `run_coding_task` / `apply_code_proposal` / `run_project_checks` / `list_proposals` into the hot tier, so a coding run does not pay a `tool_search` round trip to discover them at startup (see `docs/components/magician/execution/FLAT_LOOP.md`).
- **Perceived latency.** The cockpit conversation spine self-advances its empty-state through the real setup→route→spawn phases on an elapsed timer (`ConversationSpine.svelte`, unified-ui v0.0.529) instead of a frozen spinner.

Owned component docs updated alongside: `docs/components/magician/execution/README.md` (synthesis root-gate), `api-mining-pipeline.md` (detach), `memory-index.md` (embed-once), `execution/FLAT_LOOP.md` (coding hot pre-bind).

## Startup terminal-work sweep (2026-09-06 notes)

`magician-bin` spawns `reconcile_stale_synthesis_at_startup` without a
sweep-wide timeout; the sweep budgets each task (20 s) and is idempotent, so a
slow boot no longer strands every task after a cut. The sweep skips
re-attaching delegated children of a task that is already terminal, the same
rule the child watcher applies, because those watches only started, read the
terminal task, and stopped while their event bridges replayed projections
older than the persisted one. The legacy pause reaper reads the execution
state on disk and treats a missing record as gone, so pauses left by
pre-V3 executions are reaped instead of surviving every boot.

### Online DuckDB maintenance lifecycle

The composition root passes `database_maintenance` budgets to the Channel Assist
and Feed store owners and starts the automatic governance worker after HTTP
bind. Shutdown joins this worker before storage/event sinks drain. Compaction
drains all admitted connections, publishes a verified copy with durable crash
recovery, then rebinds handles before allowing new work. Status reads use small
scoped sidecars; activity events share the existing runtime activity transport.
See [Storage Governance](components/magician/storage-governance.md#automatic-channel-assist-and-feed-maintenance).

Decision Engine owns per-operation local/cloud primary and backup routing, validation, and atomic settings persistence; Magician authenticates and proxies the settings API, while the shared UI reports saved versus active routes. See [structured decisions](components/magician/structured-decision.md#model-selection-and-operation-mappings).

## Concurrent personal voice requests

Voice receipts retain the server-resolved UI thread and exact seed-context session
alongside parent and execution branch IDs. Cross-session chat display copies carry
`context_origin` pointing to their canonical branch/request. Context-only message
reads, transcript fallback and realtime resume exclude these projections before
applying message limits, including legacy copies identified by their exact IDs.
Read acknowledgements (`read_at`) persist separately from audio receipts and suppress
automatic playback without changing work state. All three clients show active work,
unread answers and selected context, hiding consumed unselected rows.

The chat owner persists `InternalVoiceSession` metadata for hidden execution
branches and one coordinator per principal/workspace. Ordinary chat indexes
exclude both kinds, including after rebuilding the index. Normal session
constructors (including comms and API placeholders) leave this metadata empty.
A branch freezes completed conversational text; it never inherits opaque
provider continuation state, unresolved tool calls or another request's token.
Each newly accepted request runs on the dedicated execution runtime with its
own cancellation token. Subsequent input and call disconnect do not cancel it.

`magician-bin` registers authenticated acceptance, listing, cancellation and
playback-control routes implemented by `magician-api::chat_api::concurrent_voice`.
The HTTP admission key is idempotent within its authenticated scope; changed
payloads under that key fail. Scope comes from verified request identity.
The client can report playback but cannot mint execution-completion events.
The storage owner uses existing durable chat metadata commits and segmented
message storage. No separate ungoverned queue files or runtime credentials are
introduced. Interrupted executors are reconciled without replaying side effects.

The media orchestrator negotiates `concurrent_requests` for personal voice
surfaces. Hands-free submits each admitted final transcript independently.
Realtime's `delegate_to_chat` returns durable acceptance and leaves delivery
to the web coordinator; unnegotiated clients retain the prior protocol.
Room/public surfaces cannot opt into owner work through this flag. A transient
provider response's governed app credential is not promoted into a lasting
background grant.

Work and speech have separate state machines. One scoped output lease and a
fenced playback attempt serialize spoken delivery. Provider completion does
not acknowledge listening: backend PCM uses its playback sources; direct
WebRTC tracks output-buffer events. The app's TTS completion callback reports
its result to the durable coordinator. See [concurrent voice](components/unified-ui/concurrent-voice.md)
for current limits and validation status.

### Composer queue and background-session history (2026-09-29)

Generated concurrent chat branches are server-classified Automated; parent
conversations keep their own lane, and coordinator sessions stay hidden. Exact
branch provenance survives listing and source-message navigation. The chat API
adds explicit queue admission and queue actions (parallel / stop-and-send).
Queue promotion and FIFO drain share the session admission lock to prevent a
request from running through both paths. Desktop HUD and web share these controls;
Android and iOS use the same endpoints. Details and qualification status:
[Concurrent requests](components/unified-ui/concurrent-voice.md).
