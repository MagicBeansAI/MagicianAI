# Development Workflow Reference

Operational notes for building, testing and running Magican locally: test entrypoints
and eval lanes, setup targets, runtime automation and the focused verification lanes.
They used to sit in the documentation hub; the hub is now an index. Run `make help`
for the full target list, and see [Testing](testing.md) for per-suite detail.

## Testing and coverage

- `make test-skill-runtime` checks shared CLI/scope wiring, installer readiness
  failures, the PowerPoint PDF exporter plugin (stubbed) and offline WhatsApp
  MCP compatibility without provider calls.
  `make test-magesp-conversations` checks ESP conversation callbacks on the host.

Repository test entrypoints: `make test-ui` runs both Unified UI Vitest
projects, `make test-rust` runs all workspace unit/integration tests plus
doctests, and `make test` composes those with the native app suites and the
Magicutor extension bridge lifecycle tests. The full target completes every
suite even after a child failure, then prints a clickable
`coverage/latest.html` dashboard with suite status and links to the detailed
Rust, frontend, and iOS reports from that run. After every non-live suite has
finished, an interactive `make test` asks whether to run real-provider and
resident-model evals. Use `make test live_evals=true` or
`make test live_evals=false` to answer non-interactively (`1`/`0` remain
accepted), or `make test-live-evals` to run that lane alone. Non-interactive
runs without an explicit value skip live evals rather than hanging.
Both forms create `coverage/evals/live-suite/latest.html`, link it from the main
summary when applicable, and preserve each evaluator's detailed HTML report.
When live evals are included in `make test`, the final repository dashboard
also shows direct status and report links for all eleven individual evaluators.
Any failed live gate fails the command. Override bounded defaults with
`LIVE_EVAL_RUNS`, `LIVE_CHUNK_EVAL_RUNS`, `LIVE_AUTH_EVAL_RUNS`,
`LIVE_EVAL_WORKERS`, `CHAT_CONTEXT_RETRIEVAL_LIVE_RUNS`, the corresponding
`CHAT_CONTEXT_RETRIEVAL_LIVE_P50_MAX_MS` / `P95_MAX_MS` gates, or
`MEMORY_TEMPERATURE_LIVE_*` quality/latency gates, or `LIVE_EVAL_CONFIG`. The
memory-temperature recall/utility evaluator runs last and links its HTML report
into both dashboards. The focused `make ollama-chunking-readiness` target is
provider-free; `make ollama-chunking-shadow-eval` is explicitly cost-bearing.
`make eval-monitor-golden` is provider-free: the deterministic Recurring
Monitors change-ledger scenarios (no LLM, no server).
`make eval-monitor-live` is a LIVE lane that needs a running rebuilt server.
LLM-led agentic compaction evals were removed with the feature.
Every eval lane the `/evals` page can list declares itself in this Makefile with
a `## eval:` comment directly above its rule. Seven lanes that were missed by the
first rollout are now annotated —
`llm-trace-phase{0-baseline,1-audit,2f-audit,3-audit,4-audit}`,
`ollama-chunking-readiness` and `test-realtime-local-transcript-live`. The total
is now 76, most recently the provider-free and live runtime-performance lanes,
which compose existing correctness evaluators with RSS, store-growth,
contention, and crash-safety measurements. `test-task-verdict-eval` remains the first lane whose subject
is frontend logic, replaying a captured corpus of task shapes through the task
panel's adapters so that a backend status the verdict does not model fails by
name instead of rendering as `Finished`.
`magician/tests/eval_lane_contract.rs` now enforces the contract
in **both** directions, so a new Makefile target that writes a report under
`evals/` fails the suite until it is annotated or explicitly excluded with a
reason. Annotation format, placement rules (a blank line above the rule silently
orphans the annotation) and the exclusion list live in
[Eval lanes](components/magician/eval-lanes.md).
`make test-verbose live_evals=true` runs the same suites and live evals with additional
Rust/frontend terminal detail; it does not add tests or increase live repeats.
The Rust target retains nextest
JUnit, doctest logs, LLVM coverage JSON, and annotated source coverage at
`coverage/rust/latest.html`; the UI targets do the equivalent at
`coverage/frontend/latest.html`. Reports are written after successful or failed
runs. Use `test-rust-verbose`, `test-ui-verbose`, or `test-verbose` when
detailed terminal output is needed. These multi-GB, fully regenerable coverage
artifacts are kept off the main disk: all per-lane report dirs default under
`COVERAGE_BASE_DIR`. It prefers `/Volumes/build/magician/coverage` when that
location is writable and otherwise falls back to the checkout-local,
gitignored `target/coverage`; macOS audio and Swift test caches use the same
portable Cargo target root. On SSD-backed development machines, the repo-root
`coverage/` can remain a gitignored symlink to the preferred directory so the
`coverage/…/latest.html` links resolve transparently. Override
`COVERAGE_BASE_DIR=…` to relocate every report lane at once.
`make setup-rust-test-report-deps` and `make setup-ui-deps` are the focused,
version-verifying installers; `make setup-all` invokes both automatically.
Runnable `make test-rust`, `make test-rust-verbose`, `make test-ui`, and
`make test-ui-verbose` targets also invoke their idempotent setup prerequisites,
so fresh checkouts continue after installing missing deterministic tooling.
Their `verify-*` counterparts remain read-only for CI and explicit diagnostics.
Skill CLIs run from each scope's own skills layer. `make skills-check-scopes`
reports every scope's skill `bin/` entry that does not run, with the make
target that rebuilds it; `make skills-link-scopes` relinks every scope offline.
The service repairs and installs scope layers itself at startup and when a
workspace is first used. See [skillshub](../skillshub/README.md#every-scope-and-scopes-that-move).

## Setup targets

Higgsfield follows the shared pinned npm setup: `make setup-higgsfield-cli`
installs and verifies its native executable and is included in skillshub's
`setup-all`. `make verify-higgsfield-cli` checks the installed versions without
downloads. Login is separate: after `./skillshub/higgsfield/bin/higgsfield auth
login`, run `make setup-higgsfield-auth SCOPE=<principal>/<workspace>` to copy
credentials into that scope's isolated HOME. See [API keys](setup/api-keys.md#higgsfield).
Agent-browser setup similarly bootstraps PyYAML into `skillshub/.venv` (or the
system user site when `uv` is unavailable) before skill classification and
materialization.
On a fresh macOS development checkout, run `make setup-prerequisites` before the
first Rust build; it installs Homebrew `protobuf` so LanceDB's build scripts can
resolve `protoc`, plus `ffmpeg`/`ffprobe` for media-edit execution. `make
check-all` preflights `protoc` (or a valid explicit `PROTOC` executable) before
starting the long workspace compile, while `make test-rust` also verifies or
installs the media-edit binaries before running the Rust suite. `make setup-all`
now also invokes `setup-ffmpeg` directly (not only through `test-rust` or
`setup-prerequisites`), so a normal, non-Rust-test install provisions the
`ffmpeg`/`ffprobe` binaries the shipped `media_edit` provider shells out to at
runtime. Setup/checks verify the required filters and encoders, including
`drawtext` for captions; macOS setup selects Homebrew `ffmpeg-full` when the
installed build is missing them. It runs **first** in `setup-all`, before `make -C skillshub setup-all`:
skillshub's chain includes `setup-media-fetch`, which symlinks the host
`ffmpeg`/`ffprobe` into `skillshub/media-fetch/bin/` and exits non-zero when
they are absent. That chain is `&&`-linked, so an ffmpeg installed later in the
target would be installed too late — on a fresh machine skillshub setup would
abort first and nothing after it would run.

## Code generation and user-memory tools

Automation note: use `make event-taxonomy-codegen` after editing the `taxonomies!` macro in `magician-event-taxonomy/src/lib.rs` to regenerate `ui/unified-ui/src/lib/realtime/event-taxonomy.ts`; `make event-taxonomy-check` is the drift gate (wired into build/test) that fails CI if the TypeScript mirror falls out of sync with the Rust source. `make memory-user-report` (read-only size report, no record contents), `make memory-compact-user` (dry run of the one-time user-memory clean-up), `make memory-compact-user-apply` (backs up, then archives everything that leaves `knowledge.json`), `make memory-dedupe-user` (dry run: records saying one thing in different words, plus what the write-time fold archives next) and `make memory-fold-user-duplicates` / `-apply` (list / fold stored near-duplicates into the most-confirmed copy) are the user-memory growth-bounds tools; see [memory lifecycle](components/magician/memory-lifecycle.md#size-bounds-and-archive). Use `make graph-index`, `make graph-contracts`, `make graph-all`, `make graph-check`, and `make graph-serve` for codegraph workflows and prefer documented Make targets over ad-hoc commands. `make test-storage-gate1` runs the disposable Decision Gate 1 spike (synthetic SQLite; optional `MAGICIAN_GATE1_POSTGRES_URL`). `make test-storage-s3` runs dormant S3 object/dataset adapter tests against a hermetic in-memory backend. `make test-storage-state` runs dormant SQLite/Postgres repository and SQL-lease tests. `make test-storage-migration` runs the dormant owner-closure and migration coordinator (synthetic owner; not default startup). All Cargo-backed operations run through Make and its managed build cache; `make build-cache-status` reports active routing. Host storage overrides stay outside git. `make test-working-set-eval-harness` runs Boundary B's deterministic working-set suite plus the live evaluator's self-test, and `make test-working-set-live-eval` asks a model every planted probe from each lane's bytes and gates on correctness beyond and within the pack budget, judged quality and cost (`WORKING_SET_LIVE_EVAL_REPEAT`); see working sets. Desktop build, check, test, signing, and release targets invoke `make setup-desktop-pnpm`, which reads the exact `pnpm@…` pin from `desktop/package.json` and installs it under `.cache/desktop-pnpm` with npm when missing. This works without Corepack (including Node 25) and does not trust a drifting global pnpm; an explicit `make PNPM=…` override remains supported but must report the pinned version. `make run-supervisor-chat-trace` runs the supervisor with `MAGICIAN_CHAT_TRACE=1` and `RUST_LOG=info,magician=debug` for chat regression debugging — dumps every chat turn's prompt, LLM response, tool calls, and tool results under the session's trace dir. `make test`, `make test-verbose`, and `make test-capabilities` run through package-scoped targets where needed; `make test` and `make test-verbose` inject dummy API keys to prevent macOS Keychain prompts during CI/test runs and cap Rust test harness parallelism with `RUST_TEST_THREADS=4` while also raising the open-file soft limit via `ulimit -n $(TEST_FD_LIMIT)` (default `16384`, falling back to `4096`) by default to avoid macOS file-descriptor pressure in the large Magician suite — macOS ships a 256 `maxfiles` soft limit that even a 4-thread run exhausts once tests open DuckDB db+WAL+lock handles, surfacing as intermittent `Too many open files (os error 24)` (EMFILE) failures in otherwise-unrelated tests (`ui_threads`, `analytics`, …); `make test-capabilities` validates checked-in capability YAML through the Magician test package. Provider-free offline-audio evaluator regressions are also part of `make test`; the separate `make benchmark-media-offline-audio` and per-stage VAD/STT/TTS/diarization targets own a loopback FluidAudio sidecar and never start Magician. `make build-macos-speech-helper` (debug; `-release` too) builds `native/macos-speech-helper` and stages `./magician-macos-speech-helper.bin` and `./magician-macos-meet-audio.bin` — the Apple Speech helper behind the desktop Speech permission, `/host/speech/transcribe` and `macos_tts`, and the meet-audio tap behind the meeting bridge; `build-all-debug`, `build-all-release` and `dev-desktop-build` include it, and `build-desktop-tray-debug` builds it first so `materialize-desktop-debug-app.sh` can stage it inside the debug bundle beside the tray binary. The runtime build/test flow is decoupled from skill-side setup: `make build-all-debug` builds the Rust debug binaries, graph index, unified UI production bundle, Tauri tray/gateway, without chaining into `setup-bots` / `setup-capability-tools`; `make build-all-release` remains Rust release binaries plus `graph-index`. `make build-marketing-site` and `make publish-marketing-site` build and deploy the standalone public static landing page to Cloudflare Pages. `make test` and `make test-verbose` keep one inline guard — `$(MAKE) -C skillshub verify-agent-browser` — because npm install during normal dev work can silently replace the patched fork (`agent-browser 0.41.0-Magician.0`) with stock 0.41.0, which has the 6 documented download-flow bugs that hang browser tests against Chrome 148. The gate checks the version and signature, constructs a fresh copied mirror, then calls `skills get core --full` with no discovery override; failure prints a one-line "stale binary" hint pointing at `make -C skillshub setup-agent-browser`. All other skill artifacts (bot bundles, marimo venv) are owned by `make setup-all`, where each `setup-*` target self-validates after install. Run `make -C skillshub setup-all` to bootstrap everything skill-related; reach for surgical refreshes via `make -C skillshub setup-bots|setup-marimo|setup-agent-browser|setup-kapso-cli`. `make -C skillshub setup-kapso-cli` (also chained into `setup-all`, plus a root `make setup-kapso-cli` passthrough) verifies the exact-pinned Kapso WhatsApp CLI backing the `kapso-whatsapp-read`/`kapso-whatsapp-send` skills (Presto's own WhatsApp identity, distinct from the user's WhatsApp-Web `whatsapp` tool). The package is installed once under `skillshub/node_modules/.bin` by `setup-deps`; every workspace reuses that immutable executable while credentials, work directories, and writable CLI homes remain scope-owned. Node dependencies for tools and bots live in the skillshub root npm workspace (`skillshub/node_modules/`, installed by `make -C skillshub setup-deps`); the legacy scope-owned `<scope>/capabilities/node_modules/` tree is retired and `make -C skillshub clean-legacy-scope` removes it. `make setup-bots`, `make build-bots`, and `make check-bots` build against that workspace and require the project-local Node pinned in `skillshub/.node-version` (`24.20.0`, unpacked under `skillshub/.node/` by `make -C skillshub setup-node`). `make -C skillshub setup-python` (the shared `skillshub/requirements.txt` venv) is also the Python install path for the shipped Gemini media packs: it installs `google-genai` (plus `Pillow`, still needed for Nano Banana) for both `image-generation` and `video-generation-via-veo`. For local desktop update testing, use `make dev-desktop-setup`, `make dev-desktop-build`, `make dev-update-server`, and `make dev-container-rebuild`. `make run-supervisor` now boots the supervisor/services on ordinary Rust/Tokio stack defaults and removes inherited `RUST_MIN_STACK` values, `make check-stack-frames` reads the arm64 prologues of the deep chat and resume poll frames in the debug `magician.bin` and fails when one exceeds its budget in `scripts/stack_frame_budget.py` (workers keep the default 2 MiB stack, so a frame is shrunk rather than a stack grown), and `make tail-magician-v2-log` tails the live `magician.log` stream into `magician_v2.log` for focused V2 tracing. `make run-all` starts the Vite UI dev server, Kapso Cloudflare Tunnel, codegraph server, magic-supervisor stack, and host tray/gateway in one foreground terminal so all logs stream together; `make stop-all` tears those local processes down (the Cloudflare Tunnel is a persistent `brew services` daemon left up across restarts — `make stop-tunnel` takes it down). `scripts/install-prerequisites.sh` now installs `cloudflared` alongside other dependencies. `make skills-validate`, `make skills-list`, `make skills-list-source`, and `make skills-install-scope SCOPE=<id>` (alias `scripts/magician-skills <subcommand>`) are the canonical entry points for managing AgentSkills v1 skills installed under `$MAGICIAN_ROOT_DIR/scopes/<scope>/skills/` from the source folder at `skillshub/`. `make setup-all` now runs `make -C skillshub setup-all`, which ends in `install-scope SCOPE=anonymous/default` (override with `SCOPE=`), so canonical setup populates the runtime tree before personality lookup or skill activation runs. The agent-browser vendored binary moved from `vendor/agent-browser/` to `skillshub/browser/_vendor/`; `make setup-agent-browser` mirrors the patched binary into `skillshub/browser/bin/` and the legacy npm-deployed path simultaneously, so existing executor flows keep working until full dispatch swap (5b-3) lands. `make setup-pi-coding-agent` idempotently reconciles the Pi CLI to the reviewed 0.87.1 pin backing `run_coding_task`; it is chained into `make setup-all`.

## iOS Vosk

`make setup-magios-vosk` is the iOS counterpart of
`make setup-desktop-vosk`: it fetches and SHA-256-verifies the `libvosk` iOS
static archive plus the on-device wake model for Magios ambient mode, both
gitignored. Runnable `make test-ios` and `make test-ios-ui` lanes invoke this
idempotent setup automatically. It remains deliberately absent from generic
`make setup-all` because it is macOS/Xcode-only and occupies about 230 MB once
installed. `make verify-magios-vosk` is the read-only integrity preflight used
by `make check-all`: it checks both pinned archive digests, the xcframework
manifest, and the model sentinel, then points to the setup target without
downloading anything when an artifact is missing. Run setup before any
intentional `xcodegen generate`, because the app target references the unpacked
model directory as a folder reference; see [Magios ambient
mode](components/magios/ambient-mode.md).

## Stack safety

Stack-safety note: `make run-supervisor`, `make test-rust`, and the composed test
runner remove inherited `RUST_MIN_STACK` values and use ordinary Rust/Tokio
stack defaults. Agentic execution and resume are lazily constructed on a
dedicated runtime, external JSON/accessibility traversal is iterative and
depth-bounded, and `make test-agentic-default-stack` adds the 1,000-iteration
stress qualification. The incident-only
`MAGICIAN_EMERGENCY_RUST_MIN_STACK` override logs a warning; see [Runtime async
stack boundaries](components/magician/runtime-async-stack-boundaries.md).

## Ollama lifecycle

Ollama lifecycle note: `make run-supervisor` owns a deterministic local Ollama
daemon. It reuses a matching Magician-owned launch, restarts one whose recorded
launch settings changed, and by default replaces a conflicting local Ollama GUI
daemon; configured remote endpoints are never stopped. Defaults come from
`runtime.ollama` plus the mapped Ollama profiles: the current seed has one
generation runner at 32K on `:11434` and one embedding-only runner at 8K on
`:11435`, both with quantized `q8_0` KV caches and flash attention. Generation
keeps its 10-minute idle policy; the embedding model is prewarmed, pinned with
`keep_alive: -1`, and served as one verified physical sequence. Foreground
retrieval takes priority at each bounded background provider boundary.
`make stop-supervisor` terminates only daemon PIDs Magician launched. Runtime
env files remain temporary emergency overrides; normal embedding endpoint,
residency, scheduling, timeout, and model settings are config-first.

## Desktop and workspace automation

Workspace automation note: `make build-all-debug` stages the Tauri host at the
repository root as `magician-desktop.bin` and builds the native audio engine.
The retired presence-host source tree is not a build dependency. The
speech and Google-Meet ScreenCaptureKit helpers are staged by
`make build-macos-speech-helper-debug` (which `build-all-debug` and
`build-desktop-tray-debug` invoke) and are located through `MAGICIAN_MACOS_SPEECH_HELPER_BIN` and
`MAGICIAN_MACOS_MEET_AUDIO_BIN`. `make run-desktop-tray-debug` and
`make run-all` launch those staged binaries so build and run paths cannot drift
onto stale scratch artifacts. Since 2026-09-06 the tray itself is opened as the staged debug app bundle
(`.local/Magican-Debug.app`) through LaunchServices, after
`scripts/verify-desktop-debug-app.sh` proves the executable and its nested
dylibs share a signing team; the raw `magician-desktop.bin` cannot load the
ad-hoc vendor libvosk under the hardened runtime, and that abort used to be
visible only in `magician-host-tray.log`. The tray run path passes `MAGICIAN_HOST_GATEWAY_URL`
plus the staged speech-helper binary path to the Tauri host process,
and supervisor startup receives the same host gateway URL so browser/native voice
flows share one local gateway contract. On macOS `make run-supervisor` and
`make run-all` hand magic-supervisor the built bundle to relaunch after a
crash, never after Quit (`supervisor.manage_desktop`;
`SUPERVISOR_MANAGE_DESKTOP=0|1` per run). `make stop-desktop-tray` writes the
clean-quit marker (`DESKTOP_QUIT_MARKER`); the tray launch targets clear it and
pass it as `MAGICIAN_DESKTOP_QUIT_MARKER`
([supervisor guide](components/magic-supervisor/supervisor.md#desktop-app-macos)).

## Agent definitions

Agent-definition automation note: the backend now caches parsed per-scope
definition lists and one-time built-in-template materialization, while normal
definition writes invalidate the affected scope automatically. The agent
scheduler runs on a 30-second cadence against that cache. After editing an
agent's YAML directly on disk, run `make refresh-agent-definitions` to call the
scoped refresh API, clear the shared cache, and eagerly reload all scopes
without restarting Magician. See the [agent definition reference](components/magician/agents/agent-definition-reference.md#runtime-definition-cache)
for overrides and the response contract.

## Codegraph

Codegraph note: `make graph-index` is the full regeneration target for
`graph.json`, stats, payload profiles, behavioral contracts, dead-code
candidates, static test-coverage buckets, and a final filesystem/graph audit.
`make graph-contracts`, `make graph-audit`, `make graph-dead-code`, and
`make graph-coverage` remain available for surgical refreshes; `make graph-all`
is now an alias for the full index. `make graph-serve-stop` and
`make graph-serve-restart` manage the local codegraph explorer without tying up a
foreground shell. The right panel includes `Flow` + `Guide` tabs for simulation,
guided explain mode, runbook export/save, and guide auto-rebuild on artifact
changes. `make setup-codegraph-mcp` (also run by `make setup-all`)
pre-builds the venv the codegraph MCP server runs in; the server builds or
repairs it itself on launch, so clients no longer need `mcp` in their own
`python3`.

## UI development

UI dev note: `make run-ui-dev` runs the Vite dev server on
`http://localhost:5173` without touching the tunnel. `make run-all` starts that
server for the default full-stack local run and streams Vite logs beside
supervisor/tray logs; `make tail-service-log
SERVICE=stack|magician|magicutor|ui|host-tray` attaches to one
already-running service log, and `make open-service-log SERVICE=host-tray`
opens a macOS Terminal tail. `make preview-ui` builds and serves the production
bundle on `:5173`. Web app routes are desktop-only; mobile device work belongs
in the native iOS/Android applications.

## Analytics

Analytics note: `make analytics-export DB=<analytics.duckdb> OUT=<dir>` and `make analytics-import IN=<dir> DB=<analytics.duckdb>` wrap the `magician analytics export|import` subcommand (with `MAGICIAN_SKIP_KEYCHAIN=1`, so they run headless) for deliberate cross-version DuckDB migration — EXPORT with a binary built at the old `duckdb` pin, IMPORT with the new one. Normal version bumps need no manual step: the analytics event sink mirrors every batch to version-stable Parquet (`<scope>/analytics/events/dt=*/batch_*.parquet`), and an unreadable `analytics.duckdb` is loudly quarantined and rebuilt from that mirror. The full policy + procedure is `docs/runbooks/2026-06-13-analytics-duckdb-version-migration.md`. `make analytics-reprice-llm` wraps `magician analytics reprice-llm-calls` — a corrective recompute of historical `llm_calls` `cost_usd` against the active effective-dated pricing table; dry-run by default, `ARGS="--apply"` rewrites changed Parquet files atomically (details in `docs/components/magician/duckdb-analytics.md`, Pricing section).

## Magios build lanes

Magios's ordinary `ios-debug-build` and `ios-debug-run` automation is compatible
with Personal Team provisioning and therefore omits APNs. Paid-team developers
can explicitly select the push-enabled Debug lane with
`ios-debug-build-push` or `ios-debug-run-push`.

## Storage, build caches and delivery lanes

Automatic database upkeep, status events, resource budgets and the focused
`make test-database-maintenance` verification lane are documented in
[Storage Governance](components/magician/storage-governance.md#automatic-channel-assist-and-feed-maintenance).

Local debug/check builds automatically prune incremental caches older than seven days when their profile exceeds `INCREMENTAL_BUDGET_GB` (40 GB default), under Cargo’s build lock. `make maintain-build-cache` runs this explicitly; caches belonging to active builds are skipped.

Decision Model metering (Jev/local models): `make test-decision-model-metering` verifies attempt receipts, pricing, canonical capture, and the `/llm` breakdown. See [structured decisions](components/magician/structured-decision.md#decision-model-usage-and-pricing).

- [Decision Model dashboard](components/unified-ui/decision-models.md): scoped quality, application outcomes, latency, cost and monitoring controls; verify with `make test-decision-metrics`.

Run `make test-task-delivery` for task shell-directory scoping, output registration and MIME metadata, synthesis fallback, partial-completion labels, and file-card layout regressions.

Delivery and scheduling integration
connects existing ledgers, skill runtime, capture and Work; local qualification
passed, while external calendar/delivery verification remains open.
`make test-delivery-scheduling` runs the focused hermetic batch.

## Focused lanes

Verification and evaluation lanes that were described in passing in the old hub.
Each target exists in the Makefile; component docs carry the detail.

Decision Engine participation is controlled in Magician Settings (All Engines / Magician Only / Off); `make test-decision-mode` checks persistence and access control. The same Settings card provides per-operation local/cloud primary and backup models, model-specific thresholds, and live routing status.

Channel classification uses the [Decision Engine](components/magician/mail-assist.md#structured-classifier-rollout): owner-enabled Jev at 0.7, ordinary LLM fallback and 10% background batch comparisons; `make test-channel-classify`, `test-channel-classify-pack`, and `test-channel-classify-report` cover fallback, provenance and private calibration.

[Shared tool decisions](components/magician/structured-decision.md) use one Decision Engine path across managed chat and agentic harnesses. Memory classification uses contract v5 and decision crates `0.4.2`, requiring a paired host/engine upgrade; existing tool thresholds remain 0.7. `make test-decision-memory DECISION_MEMORY_GROUP=engine|host|report` selects a focused regression group, including observation admission checks in the host lane (omit the selector for all).
The host lane can run Magician and comms separately with `make test-decision-memory-magician` and `make test-decision-memory-comms`; `make test-decision-memory-owners-build` compiles the opt-in real-owner fixture before model startup. Four small hardening review batches are diagnostics with reviewed labels (the first retains one unresolved head); the approved 600-case H3 evaluation found that no shared-chunk operation passes its current-route gate.

- `make replay-preference-alignment ANSWERS=… CASES=… OUT=…` recomposes a recorded typed-model run with no model calls and no `magician` build.
- `make eval-preference-alignment-engine-live SOCKET=… CASES=… OUT=…` runs the typed path against a disposable Decision Engine (never the live socket), also with no `magician` build.
- `make test-terminal-outbox` checks terminal delivery ordering and retirement of proven deleted-task recovery debt. It explicitly enables the shared fixture feature that satellite fixture imports need; production debug builds keep using `make build-magician-debug` without it.
- `make test-chat-turn-engine-eval` is the `/evals` harness lane; `make test-harness-conformance-live-eval` swaps the chat engine for each installed harness and grades the same turns by effect and by who answered.
- `make test-web-researcher-eval-harness` runs without providers; `make test-web-researcher-live-eval` runs against the running service and cleans its disposable task on Ctrl-C. Both lanes and their HTML report directories are discoverable on `/evals`.
- `make export-apple-signing-setup OUTPUT=...` and `make import-apple-signing-setup BUNDLE=...` move release signing to another trusted Mac without Git. The encrypted bundle carries only the Developer ID, Distribution, App Store Connect and Tauri updater material; Xcode creates machine-local development identities after the operator signs in on the destination Mac.
- `make test-container-integration-harness` runs the [container harness](components/scripts/container-integration-harness.md) offline tests; `make qualify-container-integration CONTAINER_INTEGRATION_ARGS='…'` records live per-stage evidence without building or restarting the stack, and partial coverage or unavailable skills cannot produce a full pass. `make test-container-host-relay` runs the isolated wire tests of the desktop-owned [private exec relay](components/desktop/README.md).
