# Testing Guide (Current Stack)

## LLM model catalog

`make test-llm-model-catalog` covers resolver units, provider HTTP serialization,
routing, Rust/Python parity, migration and Web selection controls without live
provider calls. See the [catalog contract](components/magicllm/model-catalog.md).

| Target | Focus |
| --- | --- |
| `test-llm-chat-model-api` | Typed Chat/coding/Plane admission, engine qualification, readiness and retired-input refusal; includes migration-owner guards. |
| `test-llm-runtime-model-catalog` | Config, harness defaults, VibeDev and cached coding readiness. |
| `test-llm-channel-models` | Bot typed sends and backend-owned queue observation, deduplication, stop and failure; fake transports only. |
| `test-llm-script-readers` | Catalog-preserving eval snapshots, isolated candidates, canonical routing fixtures and source immutability. |
| `test-llm-chat-model-eval-args` | LongMemEval/tau2 typed request bodies, default/fixed/Off validation and retired-flag refusal. Full tau2 self-test additionally requires `TAU2_REPO`. |
| `test-llm-profile-retirement` / `test-llm-alias-free-config` | Read-only inventory, private snapshots, fallback closure, preserved source bytes and stale-source refusal. |
| `test-llm-agent-pin-migration` | Versioned store updates, archives, idempotence, stale-source and verifiable shutdown guards. |
| `llm-preference-migration-check` | Frozen returning-client conversion assets and native codegen parity. |
| `test-magdroid` / `test-ios-model-catalog` | Native metadata, defaults, all-engine choices and run-body compatibility. |

Run these through `make`. `make check-all` compiles the workspace and all clients.
The catalog frontend lane uses `test-ui-installed`, preserving Vite's installed
dependency graph. Inventories and config previews exclude some live state owners;
passing them alone cannot qualify cutover. See
migration boundaries.

## macOS source installer

`make test-source-install` runs the TUI suite and hermetic shared-script/service
tests. `make test-component-runtime` checks the shared contract;
`make test-native-package` exercises package verification, installation and
Desktop staging, plus real Mach-O local signing and reinstall of a running
fixture binary on macOS. `make check-all` includes TUI and Desktop compile checks.
Desktop frontend checks run with `npm run check && npm test` inside `desktop/`.
The rendered Setup screen uses native adapter fixtures to check reviewed source
consent, data-folder propagation, visible build failure and the owner sign-in gate.

Qualification includes a real binary `make setup-wizard ARGS=--dry-run` against
a fresh temporary `MAGICIAN_ROOT_DIR` and `MAGICIAN_PREFIX`, confirming neither
folder nor `.setup-answers` is created; test from outside the checkout too.
A prepare-only run is
`make install-source ARGS="--phase prepare --data-dir /absolute/temp/notes --prefix /absolute/temp/software"`.
It builds and checks actual packages without starting another backend.
Startup refuses listeners owned by another application, including a dev stack;
run live start/verify stages with the standard ports available. Confirm Desktop's
native plan names its source checkout, points to `logs/source-install.log`,
reaches capability onboarding after core health, and keeps setup incomplete on
build, service-registration or readiness failure. Permission grants and account
authentication remain owner acceptance steps.

## Memory connections: real model behaviour

`make test-memory-connections-live-eval` is the focused, cost-bearing lane for
the production connection worker. It starts with 12 synthetic smoke cases and
records actual model replies, evidence and delivery. See the
[evaluation contract](components/magician/memory-connections-evaluation.md) for
partitions, repeats, budgets, reports and separate journey/owner-review gates.
`PYTHONDONTWRITEBYTECODE=1 python3 scripts/test_memory_connections_eval.py` checks
only report accounting and escaping; it is not a live-model evaluation.

## Memory lifecycle

Evals exposes `test-memory-lifecycle` (focused regressions) and
`test-memory-lifecycle-live-eval` (billed frozen journeys, optional profile
comparison), with separate retained reports per run. The matching Make targets
also run from the CLI. `make test-memory-lifecycle-evals-integration` checks only
the wrapper and Evals API/history wiring without model calls. See the
[lifecycle contract](components/magician/memory-lifecycle.md#focused-qualification)
for controls and the distinction between test, model and device coverage.

## Concurrent voice

The focused lanes are `make check-concurrent-voice`, `make test-concurrent-voice`
and `make test-concurrent-voice-ui`. The [runtime contract](components/unified-ui/concurrent-voice.md)
tracks what they cover and the separate signed-in web qualification. Unit tests
alone do not establish microphone/provider interoperability or real playback.
`MAGIOS_DEVICE_ID=<id> make test-ios-live-mixed-input` checks typed queue drain
during a connected physical-iPhone call with capture closed in push-to-talk mode.
`make test-terminal-artifact-links` verifies artifact file addresses, including
recovery of existing absolute-only records and cross-execution path rejection.
`make test-chat-completion-repeat` checks in-flight delegation reuse, independent
new questions, final-result projection replacement across chat segments, stale
replay, read/playback receipts and typed completion evidence. Web store tests
also cover replacement during an active streaming answer.

## Long-running harness policy

`make check-long-running-harness` compiles the focused production-source
contracts. `make test-long-running-harness` runs the coding watchdog, ordinary
budget grants, lossless budget-pause input fences, canonical cold Attention
projection and execution-harness regressions with a retained JUnit/HTML
report at `coverage/evals/long-running-harness/latest/latest.html`. It appears
as a provider-free harness lane in Evals. Tests inject monotonic instants past
24 hours, force small call slices, serialize/restore native continuations,
check exactly-once effects and cumulative spending, and launch controlled
real child processes for quiet dispatch, stall, cancellation and deadline
checks. No test needs a 24-hour sleep or a provider call. These tests verify
Magician's execution policy; they do not claim a day-long provider soak test.

## Rust Checks

Relentless source changes are qualified together after implementation closes:
`make test-relentless`, `make test-relentless-integrations`,
`make test-relentless-api`, `make test-ui-relentless` and `make check-ui`, with
`make check-all` for shared compilation. The UI target includes workspace component
tests and scoped note-reference resolution tests. Server paging regressions check
filter-before-page counts, compact rows, scoped detail isolation, bounded/invalid
queries, independent open reviews, capture pages and out-of-order search replies.
The deep completion regressions cover damaged-journal recovery isolation,
blocked catalog rows, same-workspace draft/reference races, paired guidance
qualification/adoption, stale procedure projection after revocation, disconnected
native admission and exact campaign/template capture boundaries. Capture fixtures
verify recipient/scope/file pins, finite capacity, audit rejection, revocation,
and that simulated approval never increments production trust.
Include `make test-relentless-proposal-visibility`, `make test-agentic-resource-guard`
and `make test-llm-resource-guard` for native discovery/deny rules, budget metering
and cancellation. `make test-anthropic-structured-output` and
`make test-gemini-structured-output` validate provider wire behavior against
local fixtures. These lanes do not contact live model providers.
`test-relentless-api` includes the API's catalog helper tests. Install UI/SDK
dependencies explicitly with `make setup-ui-deps` before the first check.
`make check-ui` verifies the installed dependencies and type checks without
reinstalling them, so the live Vite module graph stays intact. Run tests on that graph with
`make test-ui-installed UI_TEST_ARGS="path/to/test"`. This target uses the same
Vitest/report runner as `test-ui` and requires `make setup-ui-deps` beforehand.
Keep dependency installation (`npm ci`) and production builds sequential with UI tests and dev
previews; they replace shared dependencies/generated files. Restart with
`make stop-ui-dev` followed by `make run-ui-dev` after those operations before
using the live dev UI. The integrations target includes native
task/recovery adapters, the shared verification runner/snapshot modules, media
editing (including real pinned-FFmpeg still rendering and silent composition),
governed CLI dispatch and workspace authority, local-capture lineage and the
persisted HITL task-spawn ceiling. It also exercises bounded native owner
refresh and durable continuation with full operation guides, current schemas,
removed grants and delegated-tool exclusion.
The root-owner pause fixtures exercise a real durable pause, committed journal
and scope-authenticated terminal receipt across a fresh store, preserving a zero
spawn cap. They refuse forged seals, changed generations/identities, already
adopted successors and rehashed body tampering, including altered review rules.
Cold recovery preserves conditional review requirements without granting an
approval; already granted one-time approvals remain ineligible. Missing
verification leaves the source intact; recovery cannot overwrite a same-boot
authority mismatch. The API lane verifies the full continuation hash and retains
the existing tampered-elevation regression. Both lanes reject owner-marker
removal; the cold-store fixture also rejects stripped source markers and a
request naming a different execution, without consuming the durable pause.
The result-identity regression covers stable replay, distinct controller
attempts, continuation iterations and parallel records. The real silent-media
fixture first proves the default audio-inclusive join fails with a complete
bounded diagnostic, then verifies video-only composition and its decoded frames.
Mock-server
fixtures require localhost bind access; sandbox denials are environment failures,
not passing or failing feature outcomes.
After rebuilding a UI served by `make preview-ui`, restart that preview process
before reloading native windows or browser pages. SvelteKit's preview keeps its
application manifest in memory; replacing the files alone can serve stale module
names and break native session imports. Verify that the served entry modules and
native session chunk match the current build before qualifying sign-in.
Use the configured SSD1 Cargo target and compiler TMPDIR for the whole batch.
These commands are a validation recipe, not evidence that the current source
has passed. Rebuild the complete shared debug stack before any runtime restart.

```bash
make check-all
make setup-rust-test-report-deps  # first run, or use make setup-all
make test-rust
make test
cargo test -p magicutor
cargo test -p magic-supervisor
make test-magicrun
make test-magicvault
make test-magicvault-compatibility
make test-magicvault-foundation
```

`make check-all` includes the Rust workspace check (`CHECK_DEAD_CODE=1` adds the
default-library dead-code pass, which CI always runs), desktop and Unified UI checks, Android build/tests, and an iOS simulator
compile when the platform toolchains are installed. `make ios-debug-check`
retains normal Xcode output and propagates its exit status. Xcode 27's
`-quiet` mode can emit misleading errors reporting `exit code 0` for successful
Swift compilation; see the [upstream reproduction](https://github.com/expo/expo/issues/50311).

The MagicVault foundation lane covers the standalone protocol/service/CLI/MCP
crates using synthetic keys and injected human fixtures, including actual CLI
and MCP subprocesses through local IPC. It does not operate a native keychain,
approve a real dialog, or qualify LaunchAgent behavior. Compatibility coverage
also remains in Magician's `magicvault_facade` tests. Targeted Magician
extraction evidence is in the
archived targeted results;
that round was not a full suite.

Use `make test-rust` for every Rust unit test, integration test, and doctest in
the workspace, plus the separately owned MagicRun/MagicVault suites. The external
suites use the root-manifest Git pins and can require a network fetch; MagicRun
and MagicVault are public Git repositories. Product coverage percentages
do not include the external suites' coverage. The product lane runs the
unit/integration matrix through nextest, runs the doctest harness, collects LLVM source
coverage, retains the raw JUnit/XML, logs, JSON, and source-annotated coverage,
and prints a clickable link to `coverage/rust/latest.html` whether tests pass or
fail. `make test-rust-verbose` uses the same artifact flow with detailed
terminal output. Stable Rust cannot instrument doctests or emit branch regions,
so doctest pass/fail results are included while coverage percentages reflect
the unit and integration matrix; unavailable branch metrics are shown as N/A.

On macOS, the pinned MagicRun qualification defaults to one test thread because
its jail teardown requires consecutive quiet scans of host processes. Parallel
fixture launches can prevent that proof within its unchanged three-second bound.
`MAGICRUN_TEST_THREADS` overrides this dependency-specific setting; other hosts
default to `RUST_TEST_THREADS`. The product nextest lane retains its four-thread
default. No process-cleanup assertion or deadline is disabled.
Host process churn can still fail this guard in a serial run. Retain that
failure; passing isolated retries do not qualify the full dependency suite.
To diagnose the product matrix after an external-suite failure, use
`make -o test-extracted-libraries test-rust` and report its result separately.

Use `make test` for the full repository suite. It runs Rust, Unified UI, the
Magicutor extension, desktop tray, macOS audio engine, the provider-free eval
harnesses, the test-runner self-tests, Android (skipped without
`MAGDROID_JAVA_HOME`/an Android SDK) and iOS in sequence, continues after child
failures, and then prints a clickable `coverage/latest.html` summary. The
summary records every suite's pass/fail/skip state and links the detailed Rust,
frontend, and iOS dashboards produced by that same run. Timestamped summaries
and JSON manifests remain under `coverage/summary/results/`; the command still
returns non-zero when any suite fails. `make test-verbose` uses the same flow
with detailed Rust and frontend terminal output.

`make test-attention-historical-bootstrap-eval` runs the production-worker
regression in `magician-comms`, the malformed-event admission regression in
`magician-api`, and the learning/resurfacing store regressions in `magician`.
Keep these package selections aligned with their owners after crate extraction;
a test-name filter in the wrong package can succeed with zero tests.

The iOS suite in `make test` is the `MagiosTests` unit gate (`make test-ios`, coverage
enabled); the `MagiosUITests` XCUITest smoke suite is intentionally excluded from the
blocking gate and runs on demand via the non-blocking `make test-ios-ui` lane. See
`docs/components/magios/README.md` for why (XCUITest app-teardown timing flakiness) and
how the UI tests launch offline via `--ui-test`.

Rust targets inject dummy API keys, isolate storage under a temp root, verify
the patched agent-browser, and cap nextest parallelism with
`RUST_TEST_THREADS=4` by default. Override with `make test-rust
RUST_TEST_THREADS=8` only when you are not running other Cargo jobs in the same
workspace.
Pairing tests use fixture seal keys and generation anchors in both unit and `test-fixtures` builds; they never open the OS keychain.

The report runner overrides inherited runtime paths with one private
`/tmp/magician-cargo-test-*/magician_data_v3` fixture. It unsets `RUST_MIN_STACK`
and defaults to production's `stateless` driver; `MAGICIAN_EXECUTION_DRIVER`
allows an explicit rollback-driver check. Nextest 0.9.98 or newer is required.
Override report locations when needed:

```bash
make test-rust RUST_TEST_REPORT_DIR="$HOME/Desktop/rust-test-report"
make test TEST_SUMMARY_REPORT_DIR="$HOME/Desktop/magician-test-report"
```

**Wall-clock budgets are not measured in the report lane.** Running four test
processes at once measures the runner, not the adapter: the Gate 3 local
small-object drill was recorded at 246 ms and 247 ms in two report runs
against its old 100 ms line while passing at 32–45 ms on an idle machine. So
`make test-rust` exports `MAGICIAN_GATE3_ENFORCE_LATENCY=0`, which still runs
every budget drill and prints each measurement but asserts only the declared
lines (quotas, TTLs, backlogs, flags). Enforce the wall-clock lines with:

```bash
make test-storage-budgets
```

That lane runs the drills one at a time with enforcement on. Run it on an
otherwise idle machine, or its numbers mean no more than the report lane's.
The accepted numbers and their scope live in
`docs/components/magician-storage/adr-2026-09-01-performance-capacity.md`.

**Where coverage lives.** Coverage artifacts (Rust LLVM coverage, iOS
`.xcresult` bundles, Vitest V8 reports) are large — multiple GB — and fully
regenerable, so they are kept off the main disk on the same SSD as the Rust
build cache. All four per-lane report dirs default under `COVERAGE_BASE_DIR`
(`/Volumes/build/magician/coverage`): `RUST_TEST_REPORT_DIR`,
`UI_TEST_REPORT_DIR`, `IOS_TEST_REPORT_DIR`, and `TEST_SUMMARY_REPORT_DIR`.
The repo-root `coverage/` is a gitignored symlink to that directory, so the
clickable `coverage/…/latest.html` links (and any tooling that writes to a
repo-relative `coverage/…` path) resolve to the SSD transparently. Set
`COVERAGE_BASE_DIR=…` to relocate every lane at once (e.g. back onto the main
disk when the SSD is unavailable).

`make setup-rust-test-report-deps` idempotently installs the pinned
`cargo-nextest` and `cargo-llvm-cov` versions plus Rust's
`llvm-tools-preview` component. `make setup-all` invokes the same setup target;
Rust test targets provision missing pinned tools and verify their versions.

`make cargo ARGS="test -p magician …"` supports focused local checks, but
the full Magician lib suite has thousands of tests that open temp stores, mock
sockets, DuckDB files, event logs, and Tokio runtimes. Running all of it at the
Rust harness default parallelism can hit macOS `EMFILE` / `ENFILE` pressure.
V3 artifact I/O, UI-thread scoped-store bootstrap, and transport-log paths keep
bounded retries for transient file-descriptor exhaustion, but the Makefile
concurrency cap is the primary full-suite stability guard.

## Plane typed elicitation

Run the focused production-door and form-adapter contracts with:

```bash
make test-plane-elicitation \
  CARGO_TARGET_DIR=/Volumes/SSD1/magician/builds/plane-elicitation
```

This target selects `magician-bin/tests/plane_elicitation_contract.rs` and avoids
the monolith's fixture feature. It covers typed input, the pinned official MCP
SDK, session isolation, approval captures and replay, cancellation, timeout,
delegated-run ownership boundaries and the shared response owner. Fixtures use
temporary storage and a local HTTP server; no live model-backed run is launched.
The target does not invoke workspace tests or system-wide checks, though Cargo
still compiles the required production dependencies when their cache is stale.
The dedicated SSD directory isolates its build artifacts from other Cargo jobs.

See the [plane input contract and verification record](components/magician/plane.md#typed-input)
for supported forms, protocol requirements and the archived implementation record.

## Final-answer live eval

`make test-final-answer-live-eval` (`magician-bin/examples/final_answer_live_eval.rs`)
replays finished runs' finalization — execution output, the task-user
projection, its claim check and repair — on copies of their task directories
with the live model router, and checks the published answer keeps the
figures the run established. It spends model calls and needs provider keys.

- Cases: `data/magician_v2/evals/final_answer/cases.json` (execution ids in the
  active runtime, `must_contain` / `must_not_contain` strings). Positive cases
  are correct answers of different kinds (data, OCR, creative, research, a
  failed outward action) that must publish without the caveat.
- Negative cases plant an error with `alter_answer` (`from`/`to` replacements
  in the agent's own terminal files and outcome summary, never in tool
  results) and list it in `must_flag`: each must be left out of the published
  answer or the answer must carry the "Not fully verified" caveat. They catch a
  claim check made too lenient.
- Each case directory keeps the rendered synthesis bundles, both outputs and
  the claim check; `report.json` under `evals/final-answer/live/latest`.
- `FINAL_ANSWER_LIVE_EVAL_ARGS="--case <id>"` runs one case;
  `FINAL_ANSWER_LIVE_EVAL_REPEAT=n` repeats each.
- The runtime is only read: replays write under the output directory.

## Secure HITL qualification

```bash
make test-secure-hitl-qualification
```

`magician/tests/secure_hitl_qualification.rs` proves the secure-HITL claim
(`docs/archive/plans/2026-09-15-secure-hitl-credentials-and-otp.md` §8) against the
runtime itself rather than mocks of it. Each journey builds an in-process
`MagicianV2Api` (`tests/support/v2_api_harness.rs`: a stateless-driver
scope, the scope's owner agent, the embedded compiled packs, a realtime
broadcaster wired to the artifact service and the terminal-outbox projector),
a scripted model whose responses carry trace receipts, and a wiremock fixture
login service (`tests/support/secure_hitl_fixture.rs`: `/login`, `/otp` with
a reject-first-code mode, `/api/private` behind `401 WWW-Authenticate:
Basic`) with credentials and a code generated per run. A journey creates and
starts an execution through the API, waits for its `hitl.requested` on the
broadcaster, answers through `POST /hitl/{id}/respond`, lets the run finish,
then asserts the fixture's receipts (the destination really received the
value — redaction can never pass as a login) and sweeps the runtime's
storage root, every published event and the execution record for the canary
bytes and their base64 / URL / hex / JSON-escaped encodings. No network, no
model, no third-party account; nothing is left in a live data root. Nine
journeys run live (a tenth is `#[ignore]`d, reproducing a gap it found); the
plan's Task 7.2 and 7.3 notes list the five runtime defects they found and
the two items recorded rather than fixed.

## Task Recipes / API Mining

Task Recipes has five dedicated Makefile lanes. This is a command reference,
not an instruction to run them during a source-only or no-tests session.

- `make test-capability-recall-eval`: provider-free `tool_search` and
  `find_agents_for_capability` recall over every shipped pack leaf and agent
  template, plus golden queries such as `web research` → Sleuth.
- `make test-task-recipes-eval`: provider-free fixture compilation, replay,
  answer/input-variant matching, relevance, and privacy/safety measurements.
- `make test-task-recipes-safety`: 13 mutation/secret-safety contracts plus two
  HTTP boundary tests for ungranted writes and disabled mining.
- `make test-task-recipes-live-eval`: explicitly opt-in, cost-bearing
  cold/warm/variant/drift acceptance against running Magician and Magicutor.
  It must not be substituted with a passing provider-free harness self-test.
  The signed-in Keka write case (`p6`) targets `MAGICIAN_KEKA_ORIGIN`
  (e.g. `https://<company>.keka.com`); unset, it points at a placeholder tenant.
- `make test-task-recipes-fixture-eval`: the autonomous lane. The harness
  serves four local fixture sites (search + list→detail + Document answer,
  cookie session + auth heal, guarded write + HITL, GraphQL + drift), runs one
  cold browser task per case through the real agent + LLM, then proves the
  warm / variant / heal / write / drift phases complete with
  `outcome_type=recipe_replay`, the correct answer, and **no browser** — the
  fixture's own request log is the oracle (a fresh page nonce or a beacon
  means a page executed). It runs in the disposable auth workspace
  `recipes-eval` (created on first use; purged before and after unless
  `--keep-scope`; no seal step — fresh workspaces run on the stateless driver
  as they are). Auth: `MAGICIAN_BEARER_TOKEN` bound to that workspace, or
  `MAGICIAN_EVAL_USERNAME` + `MAGICIAN_EVAL_PASSWORD`. `--cases c1,c5`
  narrows a run; `--self-test` is the provider-free smoke the aggregate
  runner calls. Report: `coverage/evals/task-recipes/fixture/{report.json,latest.html}`;
  a failing gate carries the recipe, the execution's `recipe.*` events, the
  matching `magician.log` lines, and the fixture request log.
- `make test-task-recipes-fixture-iterate`: one fix-loop iteration —
  `build-magician-debug` + `build-task-recipes-fixture-eval` → stop →
  `scripts/purge_task_recipes_eval_workspaces.sh` (earlier `recipes-eval-*`
  workspaces, only while Magician is stopped) → `replace-restart-magician` →
  wait for `/health` → the prebuilt harness binary, so the eval never measures
  a stale binary and never waits on another agent's cargo lock after the
  restart.
- `magician/evals/task_recipes/run.py`: the no-build companion lane. It drives
  the same live cases against an already-running stack
  (`python3 run.py --cases p1,p2,p3,p4`; `p6` is the opt-in signed-in write
  case), so a case can be edited in `cases.py` and re-run without a cargo
  build. Exits 0 only when every selected case passed and writes a
  `report.json` beside `--output`. Use it to iterate on case definitions; use
  the compiled fixture lane above as the gate.

The acceptance runbook owns the safe
public-read anchor, service/config prerequisites, report locations, and any
separately authorized private-write exercise. The 2026-09-06 development
checkpoint records completed checks and 797 passing focused regressions, but
the final full-suite/check reruns were stopped on request and live/manual gates
remain pending. Do not treat those partial results as a green final-tree suite.

## UI Checks
```bash
npm --prefix ui/unified-ui ci
npm --prefix ui/unified-ui run -s check
```

## Bots Checks
```bash
make check-bots
```

## Runtime Smoke
```bash
./target/release/magic-supervisor
./supervisor-ctl status
./supervisor-ctl health
```

## Resource Authority Integration Tests

Phase D (v0.6.592) ships end-to-end tests for the resource-authority dispatch pipeline alongside the existing unit tests for individual primitives.

**Location**: `magician/src/magician_v2/resource_authority/scoped_authority.rs::integration_tests` (inline `#[cfg(test)] mod`).

**Coverage**:

- `gate_fires_persists_and_exhausts` — boots a `DiskBackedScopedResolver` against a `tempfile::tempdir`, enables a 3 USD principal budget, dispatches three 1-USD calls (all succeed), verifies the on-disk JSONL ledger has reserve+commit entries, asserts the 4th call rejects with a budget-exhausted error. Validates the full chain `config → resolver → gate → ledger → atomic_write → on-disk`.
- `per_scope_isolation_alice_and_bob` — two principals with independent 2 USD budgets. Alice drains her budget; bob's budget is untouched. Verifies Phase D's per-`(principal, workspace)` refactor actually keeps state separate on disk and in memory. Asserts no cross-contamination between principals' ledgers.
- `rejects_path_traversal_in_scope_ids` — gated dispatch with `..` principal, `evil/path` workspace, or empty workspace all hard-error before reaching `resolve_scope`. Confirms `is_safe_scope_id` fires at the gate level and no junk scope directories are created on disk.
- `is_safe_scope_id_accepts_typical_ids` + `is_safe_scope_id_rejects_unsafe_ids` — whitelist boundary cases (alphanumerics, dashes, dots, @-style emails accepted; `.`, `..`, separators, control chars, >255 chars rejected).

**Unit-level tests** (existing, predate Phase D):

- `resource_authority/ledger_tests.rs` — double-entry invariants, period-aware queries, burn rate, projected exhaustion, persistence round-trip.
- `resource_authority/gate_tests.rs` — reserve/commit/rollback, velocity limits, lazy expiry, stale reservation detection, `find_authorized_active_token` owner-chain semantics.
- `resource_authority/spend_token_resolver.rs::tests` — config-driven resolver: disabled→empty, multi-scope stacking, idempotent issuance, zero-sum funding entry.
- `resource_authority/config.rs::tests` — YAML config validation (duplicate rows, unknown ceiling refs, empty fields).
- `resource_authority/token_store.rs::tests` — token store load/save round trip, revoke + expire transitions.
- `magician-api/src/resource_authority_api.rs::tests` — REST endpoint shape tests (bootstrap, token CRUD, ledger query, audit).

**Run**: `cargo test -p magician resource_authority::` for the resource-authority module; the integration tests are picked up by `make test` along with the rest of the workspace suite.

**Known gap**: no direct integration test for `validate_delegation_request`'s Phase D additions (`allow_transitive_delegation` enforcement, per-target spend-token validation). Building one would require ~200 lines of `AgenticContext` + `ActionExecutors` scaffolding (no `Default` impl on the latter). The new logic is short composition of already-tested primitives (`AgentDefinitionStore::get_definition` in `agents/definition_store.rs::tests`; `token_store.get` in `token_store.rs::tests`; `is_safe_scope_id` covered by the integration tests above) so code-review confidence is high. Add a focused integration test only if the delegation safety path produces a runtime regression.

## What Changed
- Old Magictunnel-specific integration suites are no longer part of the active runtime path.
- Use the local capability/registry flow (`tool-runtime-core`) when validating tool matching behavior.


For distillation queue history, `make test-distill-history` runs the actual
DuckDB retirement, writer-isolation, source-preservation and worker tests,
existing distillation/retry cases, and UI queue/throughput tests. Live checks
follow a successful SSD1 Make debug build and authorized Make restart.

For App runtime changes, `make check-app-runtime-tests` checks Magician's actual
library test target and integration-test targets without linking the test binary.
A passing `make check-app-runtime` covers production libraries only and does not
qualify test-only imports, fixture constructors or compile-time assertions. Both
use the Makefile's external `CARGO_TARGET_DIR`; execute the targeted regression
cases separately as required by the App runtime verification plan.

`make test-app-native-owner` exercises boot admission, concurrent canonical App
actions, encrypted mutation receipts, identical-request replay and receipt-free
no-change completion and an attested roster host read with an unavailable counting
model implementation. It
asserts zero model attempts and requires current generated system App artifacts.

## App collection pagination

Use `make test-app-indexed-query` for the production SQL planner (100,001 rows,
25-row pages, bounded SQLite VM work, typed ordering, live mutations and scoped
cursor-cache reclamation under row and byte pressure),
`make test-app-keyset` for encrypted-registry upgrade and authenticated cursor
continuation, and `make test-app-pagination` for frozen snapshot regressions.
Pair these with `make test-app-typescript-sdk` and the focused live-collection
UI tests. Cargo artifacts and compiler temporary files belong on SSD1.
The live acceptance check opens 25 posts, loads 25 older posts and observes a
new post at the top while retaining the previously opened history. No synthetic
scale-test posts are inserted into the user's workspace.

### App older-data cleanup

Run `make test-app-data-cleanup` for temporary SQLCipher registry fixtures:
selection above 10,000 records, explicit digest-bound confirmation and expiry,
reference and concurrent-edit preservation, pause/reopen/resume, installation
fences, erasure history and registry v35-to-v36 migration, plus existing record
forget regressions. The migration fixture launches a fresh process through the
Apps owner, avoiding warm schema/connection caches and a separate monolith test
build. Both package test targets share one Cargo feature graph. In Unified UI run
`npm test -- src/lib/apps/appDataCleanup.test.ts src/lib/apps/AppDataCleanup.component.test.ts`
for date validation, counts, explicit confirmation, saved progress, failed
preview and changed-cutoff handling. Use SSD1 for Cargo, TMPDIR and isolated UI
build artifacts on the development machine.

Live verification should exercise options and preview only unless the owner has
selected a real cutoff and authorized deleting those records. Browser interception
can exercise confirmation/progress UI without sending destructive live requests.

### Memory Decision Engine v5

Run `make test-decision-memory` for the contract,
engine, memory adapters and report checks (`DECISION_MEMORY_GROUP=engine|host|report`
selects a lane). On this development machine the Makefile sends Cargo artifacts
and compiler temporary files to SSD1. Upgrade the host and engine together;
restricted memory outputs require exact reviewed qualifications even under an
operator enabled gate.
For a Magician-only edit, run `make test-decision-memory-magician`; the separate
`test-decision-memory-comms` target uses another Cargo feature graph and can be
run when comms changes.
`scripts/eval-memory-human-review.py` checks the v5 discovery contract before
replaying a frozen human review set. The current small approved batches do not
meet the 200-reviewed-item shared-chunk gate.
Use `--batch-size N --expected-strategy per_item|shared_chunk` for paired
multi-item replays; it groups only identical operation/context and prices each
physical request once. The evaluator requires exact wire, policy, request and
item identities before a replay counts as valid. `--locality local
--require-local-calls --local-model MODEL` checks that a strict-local arm never
uses a remote call or answer. Freeze labels before either arm runs.

The opt-in `make test-decision-memory-live` calls real Jev and incumbent profiles
through the production applicability and attachment functions. Its integration
test lives in `magician/tests` so boot adapters and the router share one
Magician registry. Set
`MAGICIAN_MEMORY_EVAL_CONFIG` to the live host config, `DECISION_ENGINE_SOCKET`
to an isolated compatible engine, and `MAGICIAN_MEMORY_EVAL_OUTPUT` to a fresh
folder on the external artifact volume. It uses synthetic inputs, writes a
scoped canonical trace journal, checks semantic/mutation outcomes, and exports
comparison events plus ledger receipts. This smoke run is not qualification.
The v2 synthetic suite declares a 90% minimum semantic success rate separately
for each consumer. Every outcome is exported, including missing incumbent
verdicts; mutation and receipt invariants must all pass. Qualification remains
stricter and reports failed paired outcomes and unknown cancelled-call prices.
This replay installs the configured LLM dispatch queue and uses the same
background lane and retry policy for selected observation calls. Full service
queue contention is covered separately by dispatch tests and runtime checks.
For paired synthetic tests, `MAGICIAN_MEMORY_EVAL_MODE=fixture_gate` requires a
`/tmp/memory-decision-…` socket and qualifications explicitly labelled
`unreviewed-synthetic-test:…`. Use only an isolated engine configuration, restore
it afterward, and never copy these test qualifications to live settings. Both
consumers must actually exercise qualified answers; task ids correlate each
fixture's decision, fallback and text costs across the ledger.

`make test-decision-memory-owners-live` uses the same config/socket/output variables
and cloud locality to exercise the seven later memory operations through public
owners. Its default mode requires shadow-only bindings; operator-gate mode is
explicitly selected as described below. It creates synthetic scoped memory on the
external volume, verifies saved effects (or grounded connection output), and
requires each operation to make a real Decision Model call. It reconciles physical
Decision Model and text-call IDs against the canonical LLM ledger exactly once.
When RAM is tight, run `make test-decision-memory-owners-build` before starting
Kev; the later live target reuses that compiled fixture.
Optional `MAGICIAN_MEMORY_EVAL_OPERATIONS` selects comma-separated owner operation
names for a focused outage replay; omit it for all seven owners. A successful
provider response without its expected saved outcome or receipt fails the replay.
The owner replay verifies connection review and citation boundaries; the separate
publication target below exercises the comms workflow. These fixtures are not held-out qualification. The same
`fixture_gate` mode accepts only isolated bindings explicitly labelled
`unreviewed-synthetic-test:`. It preserves configured confidence thresholds,
requires a qualified benign conflict decision, and permits ordinary incumbent
fallback for uncertain outputs. Restore the scratch engine config afterward.

`make test-decision-memory-publication-live` uses the same variables and a
shadow-only connection binding. It uses the real memory source adapter, isolated
canonical memory, live providers, feed/HITL storage and the production comms pass.
It requires publication plus withdrawal after a changed source, no second review
on withdrawal, and exact ledger receipt joins. Policy is kept warm during initial
semantic recall; cold-policy fallback is tested separately.

Full HTTP chat/agentic regression replay also needs an isolated runtime with its
[stateless scope activated](components/magician/storage-abstraction.md#activating-a-test-scope-for-the-stateless-driver-2026-09-04)
and normal keychain access for the operator-control signer. `MAGICIAN_SKIP_KEYCHAIN=1`
prevents those executions. Preserve cross-file YAML anchors when copying router
settings. Chat tool receipts arrive inside `AgentEvent.data.event`; verify
`tool.call.finished.success` and join the exact turn/tool-execution ids.

Operator-rollout live replays also accept `MAGICIAN_MEMORY_EVAL_MODE=operator_gate`
on an isolated socket. They require the explicit operator gate setting and record
abstentions and remaining semantic failures; no fabricated review records are
installed. With sampling disabled, gated comparison records show
`reference_attempted: false`; enabled isolated sampling reports each reference
attempt, skip or unknown dispatch separately.
For a test-only real-model conflict mutation probe, select only
`memory_conflict_review` and set `MAGICIAN_MEMORY_EVAL_QUALIFIED_CONFLICT=1` with
both exact synthetic qualifications in the isolated engine. The fixture records
policy and before/after stored facts for both replacement directions; it rejects
qualifications lacking the `unreviewed-synthetic-test:` marker.
Set `MAGICIAN_MEMORY_EVAL_REVOKE_ENGINE_CONFIG` to that isolated engine's
`decision-engine.yaml` to remove those test qualifications mid-fixture and
verify a same-process repeat cannot reuse supersession authority; the fixture
accepts only a config under SSD1's `memory-hardening-*` temporary directories.

Produce a scoped qualification report with
`python3 scripts/decision-shadow-report.py comparisons.json --ledger ledger.json --principal PRINCIPAL --workspace WORKSPACE --output report.json`
(`decision_memory_report.py` remains a compatible entry point).
Optional `--reviews` and `--paired` files must contain actual reviewed evidence
for the reported identity. Missing human reviews, full-path prices or paired
rollout checks remain `INCOMPLETE`; the script never changes gate settings.

`make test-decision-memory-diagnostic` uses the same config/output variables to
capture the synthetic lexical-similarity case's raw incumbent JSON and receipt.
It is separate from scored evidence and reads no stored owner memories.

Both evidence files are JSON maps keyed by the report's `qualification_id`:

- A review entry contains `review_ref`, `reviewer`, `dataset_id`,
  `held_out_cases` (`case_id:item_id` strings), and `cases` keyed by
  `case_id:item_id:question`, with each value carrying `expected` and `held_out`.
  Reference corrections require a new reference revision and replay.
  Only eligible cases inside that manifest count toward held-out sample size,
  label balance and quality metrics; training cases cannot supply qualification.
  Human annotations must have a nonempty expected label and `held_out: true`
  for an eligible manifest case. Empty annotations do not count as review.
- A paired entry contains `evidence_id`, `budget_ms`, `invariants` (rollback,
  cache_invalidation, provider_outage_recovery, receipts_reconciled,
  mutation_invariants), and `cases` containing `case_id`,
  `expected_outcome_passed`, `receipts_complete`, `gated_total_usd`,
  `incumbent_total_usd`, and `foreground_ms`. Unknown costs stay null.

Keep human review references separate from synthetic fixture annotations. Each
entry must link to inspectable evidence for the exact reported identity; do not
mark an invariant true merely because a test was scheduled.

## Envoy Claims Review

`make test-envoy-claims-review` runs the app projection recipes, focused claim/receipt Rust contracts, channel send and retry tests with mocked transports, and native page/navigation tests. It sends no external messages. See [Claims Review](components/unified-ui/claims-review.md).

`make test-model-dispatch` checks shared LLM/Decision Model queue fairness, cancellation, bounded admission and background inference timing.

Decision routing settings: `make test-decision-routing test-decision-routing-api test-decision-routing-locality test-decision-routing-ui` covers both fallback directions, single models, locality permission, thresholds, revision conflicts, owner authentication, and applied-state UI.

### Compact voice queue on native devices

`make check-magdroid` includes the Android coordinator regressions. Run
`make test-concurrent-voice-ios` for the iOS coordinator tests, or select an
unlocked physical phone with `IOS_TEST_DESTINATION` and the existing device
DerivedData via `MAGIOS_DEBUG_DERIVED_DATA`. The physical test bundle is signed.
`MAGIOS_DEVICE_ID=<id> make test-ios-live-concurrent-voice` exercises the actual
iPhone UI and its existing connection, submits two synthetic questions, inspects
a saved result and checks real playback completion. It does not mock the backend.
See [concurrent voice](components/unified-ui/concurrent-voice.md) for qualification
scope and recorded device evidence.

### Concurrent answer links

`make test-ios-live-answer-links MAGIOS_DEVICE_ID=<paired-phone>` runs a real-server
UI check with the iPhone's saved connection: one synthetic typed background
question, its **View original answer** cue, and the canonical destination. It
preserves existing drafts and uses neither microphone capture nor speech playback.
`make test-concurrent-voice-ios` also covers persisted link decoding and navigation
to an answer older than the newest message page. Web link/pagination regressions
live in `chatMessageLinks.test.ts` and `chatStore.sessionSelection.component.test.ts`;
Android's wire and exact-target preservation checks run in `make check-magdroid`.

### Composer queue and Automated history

After implementing both surfaces, run `make test-chat-queue-actions` and
`make test-concurrent-voice`, then the web composer/session-selection tests.
Qualify live web queue waiting, Stop & send, queue promotion, direct parallel
send, Personal/Automated history and exact parent/source navigation. Check the
HUD's collapsed/expanded controls before proceeding to Android/iOS device tests.
`MAGIOS_DEVICE_ID=<physical-id> make test-ios-live-text-queue` uses the phone's
saved connection to send a long reply request and a short typed follow-up. It
checks queue admission, explicit Stop & send/Run in parallel controls and the
follow-up's eventual answer. It preserves an existing draft and uses no microphone.


## Unified Work migration checks

Run `make check-ui`, then `make test-ui-work -o setup-ui-deps` for focused
regressions. Use `make test-ui -o setup-ui-deps UI_TEST_ARGS=--maxWorkers=4`
for the complete report on memory-constrained hosts. Setup/check lanes run
`npm ci`; finish them before starting Vite or test workers.
`make test-ui-work-browser` covers fixture theme/layout checks. See the
[qualification record](components/unified-ui/unified-work-qualification.md)
for coverage and live/native limits.

Run `make test-ios-work IOS_TEST_DESTINATION='platform=iOS Simulator,id=…'`
for Work API/deep-link/theme tests, the mature task-detail and monitor view-model
regressions, and actual Work UI journeys. Use
`make check-magdroid`, then `make test-magdroid-work ANDROID_SERIAL=…` for
Android compilation, debug/release units and the selected device's Compose
journeys. Include completed references, title/thread/schedule edits, corpus
filters, partial internal cleanup and paged App history in native acceptance.
Use compact viewports and enlarged text; keep fixture/simulator evidence
distinct from physical-device signing and live-provider acceptance.

`make test-delivery-scheduling` runs the hermetic send/correlation/intake/
suppression and negotiation/calendar-contract/Work-routing regressions,
including cold reads, source/recipient/scope refusals and signed webhook cases.
Its Rust portion uses the existing nextest harness with one fixed package/target
selection: four library test executables plus the agent-template and skill
contract tests. All focused filters run in that single build/run, including
catalog, native integration, agent types, AGY, learning feeders and stateless
worker-driver regressions. The latter cover a deep Decide poll on a default
worker stack and headroom across Pending while lease heartbeats still renew.
Agent-type coverage also polls a spawned decision child on a default stack,
before and after suspension, with real run-local scopes and shared token usage.
Each test runs in a fresh process, keeping the small-stack regression cold without
another Cargo invocation. It reuses normal Cargo output and the existing bot
workspace; it sends no mail or calendar invitations. Provider/live qualification
remains in the integration plan.

The delivery/scheduling batch can resume its API, template, skill and bot portion
with `make test-delivery-scheduling-api` after the core and scheduling suites
have passed. It retains the same build selection and narrows execution to the
API/template/skill filters, avoiding feature-selection changes between batches.
`make test-delivery-scheduling-rust` runs just the batched Rust portion.
The typed-storage boundary gate excludes generated `.local` snapshots from a
parent checkout; selecting a snapshot as the qualification root still scans it.

Delivery/scheduling follow-up regressions reopen the durable evidence index to
verify failed outcomes remain `success: false`, accepted checkpoint answers are
available to review, passwords are redacted and protected app answers stay out
of generic evidence. Auth bootstrap fixtures use the real governed path authority
to check legacy `0755` rejection, repair to `0700`, idempotence and symlink refusal.
Live company-calendar success requires an authenticated company OAuth profile;
local restart/reply success does not satisfy that separate provider gate.
The final-review consumer regression retains a failed dispatch outcome even
when the provider body contains positive text, and excludes latency metadata
from factual result evidence.
Google Workspace profile fixtures include the typed calendar companion and
make it fail if invoked during lifecycle/help checks. This detects an auth
planner that selects a companion instead of the declared package entrypoint;
identity mismatch and selectable-profile isolation remain must-pass boundaries.
Claim-grounding regressions include a compound claim surrounded by overlapping
agent reports: its exact structured result value and adjacent provenance must
reach review. Whole-value boundaries reject partial identifiers and word suffixes;
agent-written and argument documents do not supply exact-result anchors.

Answer-verification regressions cover hyphenated reply references surviving
strict numeric checks, unsupported revenue still blocking a model-supported
claim, and separated/currency negative values remaining material figures.

## Optional cloud service setup

`make test-cloud-services-setup` runs the shared graph, TUI, private env writer,
HTTP health and source installer fixtures without real credentials or remote
provisioning. Frontend: `make test-ui UI_TEST_ARGS="src/lib/runtimeClarity.test.ts src/lib/landing/coldLoad.contract.test.ts" COVERAGE=0`.
API: `make cargo ARGS="test -p magician-api --lib runtime_env_api::tests"`.
