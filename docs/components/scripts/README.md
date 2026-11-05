# Scripts Docs

## Model catalog tools

Production uses the Rust catalog resolver. `magician_config_text.py` and
`llm_catalog_config.py` expose a read-only dispatch view for setup/evaluation
clients; never serialize that view as runtime configuration. Normal readers
reject authored profile/adaptive tables, aliases and reserved dispatch metadata,
including empty declarations. Rust/Python route/default parity is required.
The Python view resolves each model's request-shape `wire` facts (`resolved_wire()`) like the Rust catalog.
Reader contracts also verify the generated `chat_history_compaction` route keeps tool use disabled.

| Command | Purpose |
| --- | --- |
| `python3 scripts/migrate_llm_catalog.py --config /old/magician-config.yaml --output-dir /new` | Read historical profiles and write an intermediate catalog without editing inputs. |
| `make prepare-llm-profile-retirement` | Inventory retired selectors in config, sources, templates and owned runtime records; no writes. |
| `make prepare-llm-alias-free-config ARGS="--config /path/magician-config.yaml --output-dir /tmp/new-preview"` | Prepare a matched alias-free main/router pair with request/fallback parity and hash manifest. Never installs it. |
| `make migrate-agent-model-pins ARGS="--inventory /tmp/inventory.json --backup-dir /tmp/backup --config /tmp/matched/magician-config.yaml"` | Preview current scoped pins through `AgentDefinitionStore`; `--apply` requires verifiable API shutdown. |
| `make llm-preference-migration-codegen` / `make llm-preference-migration-check` | Generate/check Swift and Kotlin copies of the frozen historical Web preference asset. |

Inventory options `LLM_RETIRE_CONFIG`, `LLM_RETIRE_ROOT` and `LLM_RETIRE_REPORT`
select the config, runtime root and private report. Optional `LLM_RETIRE_BACKUP`
creates a new private snapshot with hashes; existing directories or stale inputs
are refused. Reports omit record bodies/credentials, identify each storage owner
and explicitly list unscanned client, queue and sealed-state owners. Bounded
lifecycle observations cannot authorize rewriting executions or approvals.

Config retirement preserves comments, anchors, Rust enum tags and unrelated
settings. It resolves selectors and processing-binding keys together, removes
authored coding rows and retains assignment fallback closure. Use
`--rename-assignment OLD=WORKLOAD_ID` for semantic names and `--keep-assignment ID`
for source-owned workloads not rooted in configuration. Unknown roots, collisions,
cycles and conflicting declarations fail preparation. The manifest remains
`activation_ready: false`; state owners and matching clients require separate review.

Pin application uses verified source/backup hashes, expected versions, original
version archives and rerun detection. It does not edit execution snapshots,
templates, journals or approvals. Restart after offline application to reload
runtime caches. See migration ownership and recovery.

`ui/unified-ui/src/lib/llm/legacySelections.v1.json` is immutable historical
conversion data, not a live alias table. Never regenerate it from the current
catalog. Typed preferences win; converted choices must validate against the
current model/engine catalog, and unsupported choices require replacement.

Evaluation clients resolve canonical assignments to typed model/thinking through
server metadata. Memory lifecycle snapshots preserve the original config pair;
logical-chunking candidates clone their local model record and repoint only their
assignments. Alias expansion is restricted to the one-time before/after migration
comparison. Inventories read inert source with `expand_catalog=False`.
See [catalog validation](../../testing.md#llm-model-catalog) for test targets.

## Build and CI tools

`magios-run.sh` uses `apple_xcode_auth.py` to add an imported App Store Connect key only for the selected matching Xcode team; signing assets stay outside Git (see [Magios](../magios/README.md)).

`make cargo ARGS="clippy …"` keeps the machine-wide compiler gate outside
Clippy's own wrapper. mbx still manages targets and native caching; it bypasses
Rust result caching for this wrapped lint invocation.
Pinned `make cargo ARGS="install … --version …"` commands also initialize
compiler temporary storage and use the cache router on a fresh host.

`make clippy-ci` rejects new workspace warnings against `scripts/clippy_baseline.json`
(Rust 1.92), attributing macro warnings to workspace call sites; existing findings stay visible.
Review `python3 scripts/check_clippy_baseline.py --refresh` changes explicitly;
Desktop continues to require zero warnings.

`make audit` checks both committed Rust lockfiles without regenerating them.
Any vulnerability finding or audit-tool failure blocks CI; no advisories are ignored.

Codegraph endpoint discovery accepts rustfmt's trailing commas in multiline
Actix routes and resource routes; `python3 scripts/test_codegraph_routes.py`
guards that formatting does not hide endpoints from architecture evidence.

`make install-source` runs the macOS 14+ Apple Silicon source-checkout path.
The default macOS `make install` dev/local flow delegates to it after resolving
the data folder and flow; container installation retains its existing pipeline.
`scripts/install-source-checkout.sh` is shared by the shell, TUI and Desktop:
prepare required software and the embedding model, `make build-source-debug`,
create/check/install the native package, start Desktop and a per-user backend
LaunchAgent, then verify core readiness. Use `ARGS=--dry-run` to print actions
without seeding files, remembering choices, installing software or starting
services. `--phase prepare|start|verify` lets the frontends own their sequence;
`--desktop-running` avoids launching another Desktop instance.
Shell and TUI startup restart the built source Desktop so the new gateway and
permission probes are active. A listener from another application on port 3017
is refused before backend startup or Desktop restart.
LaunchServices receives `MAGICIAN_PREFIX` as well as the data root, so a chosen
software folder stays consistent when Desktop opens its native installation plan.
`build-source-debug` builds only the three runtime executables and their
dependencies, the decision engine, document converter, web UI, native audio and
speech helpers, and Desktop. It excludes graph indexing and iOS; ordinary
`build-all-debug` adds those development lanes. `package-source` includes the compiled frontend via
`MAGICIAN_PACKAGE_FRONTEND_DIR`; the service serves `share/ui` independently of
Vite and the original checkout. Source setup explicitly uses dev/local
prerequisites and resolves the standard Cargo/Homebrew paths for GUI launches.
On Apple Silicon with MLX enabled, source setup provisions the MLX compiler
before building and uses `MLX_RUST_TOOLCHAIN` (default: the `rust-toolchain.toml`
channel, 1.95.0) throughout.
This avoids invalidating shared-target artifacts by alternating Rust versions.

`native-runtime-service.py` seeds absent configuration and both private runtime
env files with complete-file publication. It preserves existing files and
rejects overlapping software/data directories, including symlink aliases.
The LaunchAgent is rebound when its prefix, root, installed binaries or runtime
inputs change; an unchanged rerun only ensures the service is running. Its log
is `<data root>/logs/native-supervisor.log`. The helper ships inside native
packages so Desktop and the terminal share lifecycle ownership.
Its `run-native-backend.sh` entrypoint ensures the configured Ollama services
and resident embedder are restored at login before executing the supervisor.

`install-verify.sh --core --wait 120` waits for Magician `/health`, Magicutor
`/health`, pinned Pi, a resident local embedder and the macOS host gateway.
Generation models are optional in this mode. Failure returns nonzero; an empty
capability plan still requires this sweep. `make test-source-install` covers
dry-run immutability, non-clobbering seeds, interrupted copies, package/build
failure boundaries, root/software/credential rebinding and readiness failures.
Source frontends also pass `--native-prefix PATH`, which checks the LaunchAgent's
reviewed root/revision and that its process tree owns every backend listener.
An unrelated healthy dev stack cannot satisfy the installation's final check.

`make test-ui-work` runs focused Unified Work preservation regressions.
`make test-ui-work-browser` runs `qualify-unified-work-browser.py` against an
isolated localhost UI server, with in-memory API fixtures and all 24 themes
at phone, tablet and desktop sizes, including the 960px inspector boundary and
130% phone text. Themes are applied through the installed theme store so each
costume's font stylesheet loads. Required views and fonts must load before
measuring overflow, control clipping, usable task-feed height and inspector
roles. The matrix includes an open More panel and a fully rendered deep Plan Inspector. Finish dependency-installing Make checks
before starting the preview server. The browser script accepts `--origin`,
`--session` and `--output` for isolated runs. Its core matrix includes Work thread
context and full member create/edit, rules and goal-tier screens at 320, 375, 430,
768, 959, 960 and 1280px, plus enlarged phone text. Repeat `--route` to qualify
specific specialist journeys; this measures their containers without requiring
a Work task inspector or New work button. Specialist runs must include valid
`from` context. `make test-ios-work` runs the native Work
API/deep-link/theme and Tasks/Attention/Work XCUITest lane; select its simulator with
`IOS_TEST_DESTINATION`. `make test-magdroid-work ANDROID_SERIAL=<device>` runs
actual Compose interactions using a device-local HTTP fixture, including exact
artifact/capture review, operator decisions and all native theme variants.
Neither native fixture enrolls a device or calls a live provider. See
[Unified Work](../unified-ui/unified-work.md) and its
[qualification record](../unified-ui/unified-work-qualification.md).

`eval-agentic-native-tool-contract.py` audits the active 1.3.11 / system 1.0.8
decision prompts for native invocation shapes and retained routing contracts.
It is a provider-free structural check; it does not qualify real-model
question selection, materiality, answer reuse or native continuation.

`make test-relentless` runs the generic Relentless library's controller, budget,
review and recovery fixtures. Storage boundary and durable-write guards include
that library. The typed storage scanner applies excluded directory names to
paths relative to the checkout, so a test checkout under compiler `TMPDIR`
(`target/tmp` on a machine without SSD1) is still scanned.
Generated `.local` sync/qualification snapshots, like `.worktrees`, are excluded
from the parent checkout's manifest scan; each is still checked when selected
as the root. This prevents a nested repo's allowed composition dependencies
from being mistaken for bypasses in the active workspace.

`make test-relentless-api` checks verified owner identity, idempotent root recovery,
cancellation and exact task-output publication without extra model synthesis.
`make test-relentless-integrations` batches the native Relentless adapters and
shared verification runner/snapshot regressions in one library test invocation.
It complements the core/API/UI targets; run these after the source batch closes,
on the configured SSD1 target/TMPDIR, before rebuilding the entire shared debug
stack. The target's presence is not passing qualification evidence.
`make test-gemini-structured-output` verifies the stateless provider's response
format on captured HTTP requests without calling a remote model.
`make test-anthropic-structured-output` verifies exact schema and thinking effort
in all three native Anthropic invocation paths, streaming responses, single-call
spending and refusal of unqualified output formats before HTTP I/O.
`make test-ui-relentless` checks the
draft workspace's contract review, admission retries, workspace changes and
approval of the currently displayed artifact bytes.

`setup-magios-vosk.sh` repairs an incomplete bundled model when `am/final.mdl` is missing, matching the `check-all` prerequisite verifier.

`channel_classify_review.py` prepares private review packets; `eval-channel-classify-decisions.py` can collect pending predictions and replay dated receipts after approval, then fit development thresholds and validate held-out gold. `make qualify-channel-classify` builds private qualification candidates from reviewed gold and 200 current live comparisons, without editing settings. See [Channel Assist rollout](../magician/mail-assist.md#structured-classifier-rollout).
Owner-authorized delegated adjudication is reported separately from individual human labels; `make eval-channel-classify-shadow ARGS='--fixtures <fresh-private-packet> --output <private-ledger>'` collects read-only real-thread comparisons through the shared host runner using an existing debug binary.

Landing page for workspace automation scripts: what each script or Make lane is
for, the contracts it enforces, and where its full contract lives. Sibling pages:
[background jobs](background-jobs.md), [container runtime](container-runtime.md),
[local prebuild/OCI](container-local-oci.md),
[container integration harness](container-integration-harness.md),
[CUA setup](cua-setup.md), [execution harness eval](execution-harness-eval.md),
[source-scanning guards](source-scanning-guards.md),
[screen-draw smoke](screen-draw-smoke.md),
[memory hardening review](memory-hardening-review.md).

## Canonical References

- [Scripts Directory](../../../scripts/)
- [Code Graph Explorer](../../codegraph/README.md)
- [Documentation Governance](../../process/documentation-governance.md)
- [AI Docs Hook Setup](../../process/ai-doc-hooks.md)
- [Contributing](../../../CONTRIBUTING.md)

## Planning and auth helpers

- `scripts/magician-token.sh` prints a bearer token for the local stack:
  it logs in via `/auth/login`, then mints a token at `/auth/tokens`.
- `scripts/eval-planning-decisions-live.py` drives V3 task planning end to end
  against the running stack. It answers every HITL ask itself (feed, task
  plan and agentic pauses, including external actions), so a run needs no
  user. `--run-timeout 0` stops after the plan; `--attach-run` follows an
  existing run. It no longer replies "in thread" to a legacy V2 slot
  (`POST /executions/{id}/message` is direct-only).
- `scripts/replay-plan-parameter-priority.py` replays recorded planner
  cases (`data/magician_v2/evals/plan_parameter_priority/`) through the
  decision engine socket and reports agreement with the LLM at a confidence
  threshold (default 0.8).

## Build and toolchain

Registry connection-pool contracts use the same idle-reaper trait path as the
production pool, both in the standalone admission-check crate and when the
contracts are included by the binary integration tests.

`make test-build-cache-integration` verifies portable cache routing, primary
checkout and linked-worktree fallback layouts, and explicit target overrides.
The fixture selects each checkout mode explicitly, so these checks work when
run from either the primary checkout or an integration worktree.

### rustc job gate

`scripts/rustc-job-gate` is a `RUSTC_WRAPPER` / Cargo `build.rustc-wrapper` that
holds a flock slot for the lifetime of each real rustc compile. Cargo's `jobs`
cap is per invocation and rustc has no RSS limit, so two or three unconstrained
`cargo` processes on a 12-core / 32 GiB host jetsam the machine. The wrapper is a
machine-wide cap (default 4 compiling rustc processes) shared by every agent.
`rustc -vV`, `--print` and `--version` probes skip the slot.

- When chaining a compiler cache, the waiting wrapper owns the slot and forwards
  signals; the cache child does not inherit the lock (a new sccache daemon would
  otherwise hold it and stall a one-slot build). Direct rustc invocations `exec`.
- The Makefile defaults `SCCACHE_IDLE_TIMEOUT=0` so long compiles keep their
  cache daemon; restart an already running daemon to pick it up.
- Host-local, not CI: copy/symlink to `~/.cargo/rustc-job-gate` and pin in
  `~/.cargo/config.toml`:

```toml
[build]
jobs = 4
rustc-wrapper = "/Users/<you>/.cargo/rustc-job-gate"
```

The root Makefile chains it before sccache when that path exists
(`CARGO_BUILD_JOBS` and `RUSTC_JOB_GATE_SLOTS` default to 4; disable with
`make RUSTC_JOB_GATE=`; empty `RUSTC_JOB_GATE_NEXT` skips sccache, needed by
Cranelift lanes). Tests: `python3 scripts/test_rustc_job_gate.py`. Concurrent
`--release` (thin LTO) and `FAST=1` (`-Zthreads`) still raise per-process RAM.

### Other build helpers

- Makefile-driven Cargo gives linked worktrees isolated target dirs under
  `/Volumes/build/magician/builds/wt/` when that volume exists, avoiding Cargo's
  build-directory lock.
- `make build-magician-debug` selects `magician-bin` explicitly (a workspace-wide
  selection can pull unrelated feature requests) and copies through the guarded
  debug-binary replacement script.
- The build router maps an explicit `CARGO_INCREMENTAL=0` to
  `MBX_EAGER_INCREMENTAL=0` for dev checks, tests and builds and to
  `MBX_LEARNED_INCREMENTAL=0` for every profile, so a bounded-disk batch does
  not silently recreate either kind of incremental state. Explicitly supplied
  mbx incremental policies remain authoritative.
- `scripts/prune-incremental.sh` (`make prune-incremental`) bounds the shared
  `$CARGO_TARGET_DIR/*/incremental` cache oldest-first under `--budget-gb`
  (default 40), skipping directories held open by a live rustc (`lsof`).
  An open-file listing is preserved even when `lsof` returns 1; inspection
  failures also preserve the cache. Only an empty no-match result permits
  removal. `make test-build-cache-maintenance` covers these cases and dry runs.
  `--crate <name>` drops one crate's cache — the recovery after an
  `incremental compilation error with evaluate_obligation` ICE, whose corrupted
  cache can also yield a binary that disagrees with unchanged source.
  `--workspace-artifacts` (`WORKSPACE_ARTIFACTS=1`) removes stale workspace-crate
  generations under `deps/` via `cargo clean -p` (~10 GB per hash for
  `magician`), refused while any rustc/cargo runs. `make check-build-space` fails
  below `INCREMENTAL_MIN_FREE_GB` so builds report the real cause, not ENOSPC
  mid-link.
- `scripts/bg_run.sh` / `scripts/bg_wait.sh` — detached long-command runner and
  waiter (`make check-bg-run`); see [Background Jobs](background-jobs.md).
- `scripts/setup-rust-test-report-deps.sh` — installs pinned `cargo-nextest`
  0.9.140, `cargo-llvm-cov` 0.8.7 and `llvm-tools-preview` (also in
  `make setup-all`). `scripts/nextest-report.toml` is the tracked nextest profile
  (no fail-fast, JUnit with failure output).
- `scripts/install-prerequisites.sh` — macOS host prerequisites; the non-slim lane
  includes Homebrew `protobuf`, `ffmpeg` and `bubblewrap`, verifying `protoc`,
  `ffmpeg`, `ffprobe`. Slim installs skip host-only build deps.

### Test report runners

- `scripts/run-rust-tests-with-report.sh` (`make test-rust` /
  `test-rust-verbose`) — workspace-wide (`--workspace`) runner: provisions `protoc`/`ffmpeg`/`ffprobe`,
  removes inherited `RUST_MIN_STACK`, defaults `MAGICIAN_EXECUTION_DRIVER=stateless`
  (the production driver; explicit overrides kept, e.g. `inprocess` for the rollback driver), runs nextest plus all doctests, collects LLVM
  coverage under `coverage/rust/results/`, and returns the original failing
  phase after writing the dashboard (`scripts/rust_test_report.py` →
  `coverage/rust/latest.html`). Four test processes run concurrently, so it
  exports `MAGICIAN_GATE3_ENFORCE_LATENCY=0`; `make test-storage-budgets` enforces
  wall-clock lines one test at a time. **It refuses to be sourced**: its exports
  (dummy API keys, a `$TMPDIR` storage path) would persist in an interactive
  shell and poison anything launched later, including a supervisor.
- `scripts/run-all-tests-with-report.sh` (`make test` / `test-verbose`) — runs
  every non-live suite even after failures (Rust, frontend, Magdroid Gradle before
  Magios XCTest; Android skips when its pinned JDK/SDK is missing), then decides
  live evals: `live_evals=true|false` is explicit; unspecified, an interactive
  terminal asks after all suites finish (type-ahead is drained first; empty/EOF =
  No), a non-interactive run skips. It consumes only fresh child reports, gives
  iOS a run-scoped correlation ID, and returns the first failing child status.
  The Make target provisions a pinned per-worktree venv under
  `$(CARGO_TARGET_DIR)/test-suite-runner-venv` from
  `scripts/test-suite-runner-requirements.txt` (`make setup-test-suite-runner-deps`,
  `verify-test-suite-runner-deps`). Regressions: `make test-suite-runner`.
- `scripts/test_suite_summary_report.py` — `coverage/latest.html` dashboard plus
  timestamped archive and JSON manifest; links immutable current-run child
  dashboards and validates the correlated iOS artifact.
- `scripts/run_magios_tests_with_report.py` (`make test-ios`) — isolated XCTest
  wrapper; see [magios Tests](../magios/README.md#tests).
- `scripts/write_eval_harness_report.py` — wraps a cargo-test or bash recipe,
  always writes `report.html`/`report.json`/`output.txt`, and exits with the
  wrapped status so `/evals` can link harness lanes.

## Skill and bot software setup

The setup wizard and root Make targets share five script entry points:
`setup-skill-runtime.sh`, `setup-bot-runtime.sh`, `setup-media-tools.sh`,
`setup-bundled-browser.sh` and optional `setup-browser-engines.sh`. Each delegates
to `skillshub/Makefile`, verifies the installed files and links them into the
selected runtime root and scope. Account login is a separate step. Bot checks
include their shared CLI packages and the SQLite addon under the pinned Node
runtime; media checks require pack-local ffmpeg/ffprobe links and the filters
and encoders used by `media_edit`, including `drawtext`, `libx264`, AAC and MP3.
`setup-media-tools.sh --check-binaries` performs the same dependency check as
`make check-ffmpeg`; `--install-binaries` provisions only those host dependencies.
The prerequisite installer reuses this binary setup, and install verification
checks the same operation dependencies rather than reporting a minimal build ready.
macOS setup selects Homebrew `ffmpeg-full` when the installed build lacks them,
retaining any minimal formula while switching its binary links. Browser engine
checks require the native executables as well as their Python SDKs.

`skillshub/scripts/verify_runtime.py` writes file inventories that the component
graph rechecks on every probe; with `--data-root` it also checks every scope's
`bin/` entries and names the make target that rebuilds a missing one
(`make skills-check-scopes`). Optional runtime PATH companions are not installer
prerequisites. `link_runtime.py` and `install_bot_bundles.py`
repair source links while preserving regular scoped files, credentials and bot
state; legacy dependency pruning is explicit. Setup runs `link_runtime.py
--all-scopes`, so workspaces created after the first setup are linked too, and
pinned native executables are copied into `<data root>/system/skill-bin/`
(`pinned_bin_store.py`) so a cleaned or moved checkout cannot break them.
`make skills-link-scopes` repeats that repair on demand. See the
[setup walkthrough](../../quickstart.md) and [skill runtime](../../../skillshub/README.md).

The shared `system/skillshub` link must point to a verified software payload,
including its generated `.runtime/` inventories. Source-only qualification
snapshots are not installed software roots. Preserve that runtime link when
building a frozen source snapshot; verify the installed root and scoped links
after any setup command that can repoint them. Keep frozen source outside the
Cargo artifact directory. When switching frozen checkouts in a shared target,
verify actual changed-crate compilation and the deployed behavior; cached
absolute dependency records can otherwise identify an older source snapshot as
fresh. Preserve and invalidate the affected fingerprints if that happens.

`package-release.sh` stages the tracked skill sources, generated CLI binaries,
bot bundles and exact Python/npm dependency locks through
`skillshub/scripts/stage_release.py`. It rejects missing artifacts, mismatched
native targets and stale Metabase generation inputs. Packaged setup verifies the
manifest instead of running Go or Rust generators. `sign-release.sh` signs the
native skill payloads alongside service binaries and seals their final checksums;
both native installer scripts copy the skill payload. Only the `fixture` package
test may omit it. `make test-skill-runtime` covers readiness and missing-file
regressions; `make test-native-package` covers the installer contract.

## Extracted-library lanes

`with_extracted_library.py` reads full Git revisions from root workspace
dependencies, prepares revision-specific checkouts under `.extracted/`, refuses
mismatched/edited checkouts, and runs only an explicitly requested lane.
`make setup-extracted-libraries` fetches source without compiling;
`make sync-extracted-lockfile` performs dependency resolution only. Checked-in
`.cargo/config.toml` sets `net.git-fetch-with-cli = true` so fetches use the
normal clone helper. MagicRun and MagicVault are public; never put credentials
in manifests, URLs, build arguments or container layers.

- `make test-magicrun`, `test-magicvault`, `test-magicvault-compatibility` and
  `check-extracted-libraries` run their named gates. `test-rust` includes the two
  upstream suites, reported separately from monorepo LLVM coverage. A preparation
  or upstream failure is a failure, never a skipped green. Artifacts live under
  the build directory in `extracted/<library>`.
- MagicRun `test` and `test-lifecycle` execute test binaries through
  `scripts/magicrun-fixture-temp.sh`. On macOS that sets the test process
  `TMPDIR` to the canonical Darwin user temp (`realpath` of
  `DARWIN_USER_TEMP_DIR`, under `/private/var/folders`). Compiler temps stay on
  the Makefile `TMPDIR`. The jail is unchanged: a group-writable ancestor is
  still refused, and the canonical path keeps the child's `$PWD` equal to the
  tempfile path the working-directory test prints.
- `test-magicvault-compatibility` covers upstream core/primitive compatibility
  plus Magician's async durable writer/facade/secrets cases, using `--lib` for
  unit tests so unrelated integration binaries are not compiled.
  `test-magicvault-secrets` reruns only the product secrets group;
  `test-magicvault-foundation` selects service/CLI/MCP fixtures at the same
  revision and does not replace product compatibility.
- `check-all` includes `check-extracted-libraries`. The Phase 5G guard reads its
  runtime evidence from the clean pinned checkout; the Python guard stays offline
  and fails on missing source.
- MagicVault is pinned at `4902803b35211ea2a1dc7e6bdb112cb9da03000f` (standalone
  0.9.3). Qualification runs the upstream gate on the exact clean consumer
  revision and never rewrites its baseline.
- Inventory/classification/replay targets use the pinned checkout but keep
  Magician's working directory for product-relative paths. No library source is
  copied back or patched via a hidden Cargo override. `.extracted/` is excluded
  from Git, Docker context and the product's manifest storage scan.
- Durable-I/O ratchet counts for MagicRun (11) and MagicVault core (3) live in
  their own repositories; removing them from the product baseline is not a claim
  the debt was fixed.

## Worktree creation and hooks

`make worktree-add WT_PATH=… WT_BRANCH=… [WT_BASE=origin/main]` creates a linked
worktree via `scripts/worktree_add.sh`: `WT_BASE` applies only when creating a new
branch (an existing branch attaches at its tip; without it a new branch starts at
`HEAD`). It refuses an existing path and configures `.githooks`. `make setup-hooks`
configures hooks in an existing clone.

The pre-commit hook runs `scripts/signing_guard.sh` before the docs guard: it
refuses staged `*.pbxproj` hunks adding a non-empty `DEVELOPMENT_TEAM`. Teams
belong in gitignored `magios/Signing.local.xcconfig`; a Team picked in Xcode is
written into `project.pbxproj`, overrides the xcconfig and would ship one
person's team to every clone, so leave the target's Team as "None".
`SIGNING_GUARD_DISABLE=1` bypasses it deliberately.

## Coding-agent docs reminder

`scripts/docs_remind_hook.sh` is the shared non-blocking docs-guard reminder
(`--working-tree --remind-only`), called by Gemini CLI, ZCode and Antigravity
(`agy`) hook configs; Claude and Grok keep `claudecode_docs_hook.sh` and
`grok_docs_hook.sh`. `agy` must get `{}` on stdout (reminder on stderr).
`DOCS_HOOK_DISABLE=1` silences every reminder; manual: `make docs-remind`. See
[AI Docs Hook Setup](../../process/ai-doc-hooks.md).

## Source and drift guards

The general rules for text-scanning guards are in
[source-scanning-guards.md](source-scanning-guards.md). All below run from
`make check-all` unless noted.

### Typed storage boundary ratchet

`scripts/check_typed_storage_boundaries.py` (`make check-typed-storage-boundaries`)
is the Task 21 guard for runtime-root I/O, Parquet globs, object SDKs, ambient
backend construction and `StorageRuntime::install`. `scripts/docs_guard.py` runs
it in the same CI/pre-commit gate as the skillshub linter. Allowlist:
`scripts/typed_storage_boundary_allowlist.yaml`. See
[task-21-typed-storage-boundaries.md](../magician-storage/task-21-typed-storage-boundaries.md).
Its walks skip `target`, `node_modules`, `tests`, `examples`, `.git`, `.extracted`
and `.claude` (gitignored agent worktrees are full repo copies and would
otherwise flag their crates).

### Skillshub runtime-root linter

`scripts/skillshub_runtime_root_linter.py` is the Task 16A ratchet for skillshub
files resolving `MAGICIAN_ROOT_DIR` or `MAGICIAN_STORAGE_PATH`; the approved shim
is `skillshub/scripts/runtime_root_shim.py`. Run by `scripts/docs_guard.py`.

### Storage catalog

`scripts/storage_catalog_guard.py` (`make check-storage-catalog`) treats
`docs/components/magician/storage-catalog.yaml` as the source of truth for
storage-owner identity, class, tier and readiness. Every Rust `StorageEntry.id`
must match a catalog `governance_id`; on-disk `Connection::open` /
`open_with_flags` files are allowlisted; readiness states after `discovered`
require every predecessor evidence key. See
[Storage Abstraction](../magician/storage-abstraction.md).

### Universal tool-runtime conformance

- `scripts/check_phase5g_conformance.py` (`make check-phase5g-conformance`) pins
  the official MCP conformance package and Rust SDK revision, verifies every local
  OAuth/strategy gate names durable source evidence, and keeps production routing
  disabled. The expected matrix is hardcoded.
  `data/tool-runtime-inventory/phase5g-conformance-v1.json`'s `component_versions`
  is the **attested** matrix and changes only if conformance is re-run; it stores
  run/evidence paths, not results, so a crate bump is not a matrix edit.
  Working-tree crate versions are deliberately not pinned — do not re-add a
  `Cargo.toml` currency check.
- `scripts/mcp-conformance/run-official-client-conformance.sh` builds the official
  Rust SDK client at the reviewed commit and runs the client suite for MCP
  `2025-11-25` and `2026-07-28` with no expected failures
  (`make setup-mcp-official-conformance`, networked
  `make test-mcp-official-conformance`; `make test-phase5g` adds the local OAuth
  and cross-strategy adversarial suites).
- `data/tool-runtime-inventory/phase0-live-agentic-v1.yaml`
  `offline_baseline.catalog_digest` is the **offline** baseline's digest, and
  `scripts/test_tool_runtime_phase0_agentic_eval.py` asserts that same value
  (`build_catalog` only returns when the computed digest equals it).

### Rank-recompute semantics drift

`scripts/check_rank_recompute_semantics_drift.py`
(`make check-rank-recompute-semantics`) proves the rank-recompute
result-semantics vocabulary agrees across backend constants, the frozen contract
fixture and the web parser. The failure is otherwise silent: the web parser
returns `null` for an unknown label behind an exact key allowlist, so the page
renders normally with every attributed result absent.

### Service-name boundary

- `scripts/check_service_name_boundary.py` (`make check-service-name-boundary`)
  guards model- and user-facing surfaces against presenting the backend
  compatibility name as an assistant, agent, product or app identity. Bounded
  technical forms (`/api/magician`, env/config identifiers, `Magician backend`)
  and literal CLI invocations (`magician app …`, `magician storage …`,
  `restart-magician`, `stop-magician`) are allowed.
  Its `ACTIVE_PROMPTS` list names each checked prompt file by its current
  version, so bumping one of those prompts updates the list.
- Desktop's capability onboarding and HTML title are covered too. A standalone
  uppercase constant declaration for the exact wire ID `"magician"` may carry
  `presentation-identity-allow: stable-operational-identifier`; that annotation
  does not exempt visible labels or longer strings. Setup product copy uses the
  generated presentation identity.
- `scripts/test_service_name_boundary.py` freezes rejected, allowed, and
  full-repository cases.
- Its `ACTIVE_PROMPTS` list names only live prompt versions. The retired
  `task_agent_output_synthesize_system` prompt was removed with the
  agent-audience task-output stage (2026-10-01).
- `scripts/migrate_agent_service_identity.py` applies exact idempotent phrase
  replacements to scoped `definition.agent.yaml` files; `--check` is a read-only
  drift gate.

### Other guards

- **Presentation identity:** `scripts/presentation_identity_codegen.py` generates
  constants for backend, Unified UI, Magios, Magdroid and both Tauri halves from
  `data/presentation_identity.json`. `make presentation-identity-codegen` updates;
  `make presentation-identity-check` is a byte-for-byte drift check that also
  rejects raw product literals in migrated consumers (in `check-all` and both
  all-binary builds).
- **Ollama single chokepoint:** `scripts/check_ollama_single_chokepoint.py`
  (`make check-ollama-chokepoint`) fails if any `/api/generate`, `/api/chat` or
  `/api/embed` request is hand-built outside `OllamaProvider`
  (`magicllm/src/providers/ollama.rs`), the `num_predict:1` prewarm ping
  (`dispatch_glue/prewarm.rs`) and tests. Its scanner is string/comment-aware
  (URLs, literals, `mod tests;`).
- **Stack-frame budgets:** `scripts/stack_frame_budget.py`
  (`make check-stack-frames`). It reads each listed function's poll-frame size
  from its arm64 prologue in the built debug `magician.bin`, including the
  stack-probe loop large frames use. It fails when a frame exceeds its budget,
  or when a listed function is missing (renamed).
  Also guarded (2026-10-14): `CapturedRunTaskLocals::scope`, the plane
  dispatch entry points, and the god-mode task-local wrappers
  (`with_run_trust`, `with_session_and_trust`). Each must wrap a boxed future:
  unboxed, `scope` reached 1,419 KiB and overflowed every VibeDev engineer run.
  - The delegated-child launch is capped (2026-11-03): the job block in
    `spawn_delegated_children_from_runtime_inner` (420 KiB measured, budget
    440) and the child's `execute_agentic_direct_with_outcome_inner` (559,
    budget 570). At 755 KiB the job block put the child's agentic loop at
    2,063 of the worker's 2,080 KiB and every delegation overflowed.
  - The durable task-planning recovery chain is capped (2026-10-25): the
    recovery monitor, its spawner, `run_task_planning` and the planner's
    per-parameter classification items. Unboxed they filled the 2 MiB worker
    on the way to the LLM router and crash-looped the service on restart.
  - The native run chain's largest frames are capped too (2026-10-15):
    `execute_agentically_run`, the resolve phase, `advance_iteration_inner`
    and `advance_under_lease`. Together they use ~1.4 MiB of the 2 MiB worker
    before a tool runs. A frame budget does not measure depth: every passed
    while one more task-local layer on the run (~40-60 KiB of poll frames,
    boxed future or not) overflowed a god-mode chat run mid-`shell`. Set
    task-locals that go together in one layer, and shrink these frames
    rather than raise their caps.
  - A frame budget also cannot see a heavy leaf called at that depth. A
    capability-snapshot refresh (config re-parse, skill-catalog hashing) ran
    inline at the bottom of the decision chain and overflowed every time a
    settings write invalidated its cache (2026-10-15). Run such work on the
    scheduler root (`run_on_agentic_scheduler_root`), not in the run's frame.
  - The list covers the deep chat → tool dispatch and hold-resume paths.
    Workers keep the default 2 MiB stack.
  - `--list` prints every budgeted frame; `--match REGEX` prints the largest
    matching frames, for measuring a new path.
  - Not in `check-all`: it needs the binary, and `nm` plus `llvm-objdump`
    take a minute or two.
- **Device bridge protocol:** `check_device_bridge_protocol_drift.py`
  (`make check-device-bridge-protocol`) keeps the Android bridge's MCP contract
  aligned: Kotlin keeps the exact `2026-07-28` tools-only methods, safety
  annotations, byte bound and list-change subscription; Rust keeps the governed
  duplex transport, discover lifecycle, roster subscription, connection
  generation and invalidation refresh. It also requires the old private envelope,
  Ktor listener, local auth/PIN manager, loopback helper and `android_tools` pack
  to stay absent — their return would create a second authority surface.
- **LLM trace coverage:** `scripts/audit-llm-trace-coverage.py`
  (`make llm-trace-coverage`) checks production LLM invocation boundaries,
  exclusions, operation-family assignments and validator ownership, and fails
  when a production operation key (`LLMOperation::Other("…")` or `*OPERATION*`
  constant) is missing from the operation-family contract — such an operation
  silently rides the router default profile.

### Generated-and-committed artifacts

[`scripts/codegen_drift.py`](../../../scripts/codegen_drift.py) is the shared
gate for artifacts generated from a source of truth and committed. `stamp` embeds
a `SOURCE_HASH:` comment (syntax chosen by output extension); `check` recomputes
it in milliseconds and runs as a build prerequisite, failing with a remediation
line.

| Command | Writes | From |
| --- | --- | --- |
| `make event-taxonomy-codegen` | `ui/unified-ui/src/lib/realtime/event-taxonomy.ts` | `realtime_events.rs` and the taxonomy crate |
| `make component-graph` | `component-graph.html` (repository root) | `magician-components/src/graph.yaml`, its page template and renderer |

Both whole-file hash their sources (may over-trigger, never under-trigger). Gates
`event-taxonomy-check` and `component-graph-check` run from `check-all`,
`test-rust` and both release builds. The stamp is placed after any shebang,
doctype or leading block comment, and exactly where the previous run put it, so
identical regenerations produce no diff and "DO NOT EDIT" headers stay first.

### Changelog retention

`make check-changelogs` fails when a per-crate `CHANGELOG.md` exceeds its entry
limit; `make trim-changelogs` fixes it (both wrap
[`scripts/trim_changelogs.py`](../../../scripts/trim_changelogs.py), which only
reports without `--apply`). Older entries move to
`docs/archive/changelogs/<name>.md` (excluded from the public mirror). An entry is
a non-`Unreleased` level-2 heading, or a level-3 heading directly under
`Unreleased`. The newest entry is stamped with the declared version only when
that version is not already on an older heading (otherwise the file is reported
as needing a bump). Moved entries' relative links are re-resolved; run
`make check-links` afterwards. The repository-root `CHANGELOG.md` (curated
product release log) is skipped.

## Installer and runtime root

### Why the installer has no per-component gates

`scripts/install.sh` installs Ollama, the macOS tray, meeting audio (BlackHole
16ch on macOS; Pulse tools and Xvfb on Linux) and, for `FLOW=local`, the Pi
coding agent CLI unconditionally. Nothing downstream knows how to behave when a
component is absent, so a gate produces a **silently broken install rather than a
smaller one**:

- **The macOS tray is not optional.** It binds the host gateway on `:3017`, which
  relays the mac skills and carries Apple-Events automation, macOS pairing and
  Android authority. It already no-ops on Linux.
- **Notes are part of Magician.** Web, command palette, voice control, observe
  panel, iOS and Android all open the in-process library.
- **Declining Ollama breaks memory, not just chat.** The embedding provider is
  Ollama on `:11435`; changing it is a profile edit plus a full re-embed. Agent
  definition storage, personal retrieval, memory prompt blocks and the wake-up
  queue read through it.

`MAGICIAN_ENABLE_FUNNEL` is the exception: the tunnel has always been opt-in and
every consumer handles its absence.

#### What has to exist before gates return

1. **A component state registry** — a persisted, queryable, post-install-editable
   record of what is installed and configured. Gates write it; probes read it.
2. **Consequence propagation** — Web, iOS and Android derive navigation and
   feature availability from that registry.
3. **Then** gates are safe and become the wizard's install actions.

The `gate` helper in `install.sh` is kept for the funnel because it validates
(`MAGICIAN_ENABLE_FUNNEL=ture` fails loudly) and is the seam future gates will
use. Design: `docs/plans/2026-09-02-setup-wizard-rust.md`.

### Container trusted peers

Without a bridge token Magicutor admits only loopback and trusted peers to its
bridge and automation routes. On a bridge network, a port published to the
host arrives from the container's default gateway, so launchers that publish
Magicutor on host **loopback** (`-p 127.0.0.1:<port>:3003`: `make
run-container` and `scripts/install.sh` on macOS Docker and Apple container,
the desktop's Docker and Apple container launches) also pass
`MAGICUTOR_TRUST_GATEWAY=1`. Only then does `scripts/container-entrypoint.sh`
set `MAGICUTOR_TRUSTED_PEERS` to the default IPv4 gateway (from
`/proc/net/route`, logged; a warning if there is no default route). Linux
`--network host` launches never set it — the gateway there is the LAN router —
and trust stays loopback-only. An operator-set `MAGICUTOR_TRUSTED_PEERS` is
kept. Magician's own port (3002) is published as before. Website `Origin`
headers are refused regardless.

### Runtime root seeding

`scripts/install.sh`, `scripts/run-supervisor.sh` and
`scripts/container-entrypoint.sh` prepare the runtime root without clobbering
operator files:

- `magician-config.yaml` (with sibling `llm-router.yaml`) and
  `decision-engine.yaml` are copied from the repo-root seeds into
  `$MAGICIAN_ROOT_DIR` (default `~/MagicianNotes`, `/data` in containers) only when
  absent. There is no separate template copy; a missing repo-root seed fails
  loudly. Magician's `decision:` block keeps only the socket and timeout; the
  engine's models, operations, thresholds and rollout live in
  `decision-engine.yaml`.
- Existing `.env`, `operator-config.yaml`, `magician-config.yaml` and scoped
  harness files are preserved. The default Harness SRE seed copies the non-secret
  `harness_reliability.md` plus `harness-sre`, `cto` and
  `internal-system-analyst` definitions into `scopes/anonymous/default/…` when
  missing.
- Container startup materializes the browser skill into the default scope from
  `/app/skillshub` (`scripts/materialize-container-browser-skill.sh`, then
  `skills get core --full` through the scoped binary), failing before supervisor
  launch if either breaks.
- `make run-container` / `dev-container-rebuild` prepare the host root before
  bind-mounting (`MAGICIAN_RUNTIME_ROOT`, then `MAGICIAN_ROOT_DIR`, then
  `~/MagicianNotes`) so Docker does not create it as root. Desktop-managed
  container startup does the same from packaged seeds.
- Notes need no seeding: they are Magician's own Markdown files, created on
  first write and browsed at `/notes`. There is no notes server, seed script or
  `notes.<zone>` tunnel route; `ensure-magician-tunnel.sh` publishes none, and
  `install.sh` warns when the runtime root holds iCloud `dataless` files.

### Identity layer

`make setup-identity` (`scripts/setup-identity.sh`) is the **single writer of
the operator identity layer**: it prompts (or takes env answers), validates, and
writes `MAGICIAN_OWNER_NAME`, `MAGICIAN_PRINCIPAL`, `MAGICIAN_AGENT_EMAIL`,
`MAGICIAN_AGENT_WHATSAPP_JID`, `MAGICIAN_INGRESS_MODE` and (for `named` ingress)
`MAGICIAN_TUNNEL_ZONE` into the data root's `.env` (0600, other lines preserved),
plus the gmail `channel_providers` `domains:` list in
`skillshub/operator-config.yaml` while it still carries the template default.
Re-running is safe; `MAGICIAN_SETUP_IDENTITY_YES=1` (implied without a TTY) is
non-interactive; invalid answers write nothing. Keys are documented in
`magician_data_v3/.env.example` under "OPERATOR IDENTITY".

**No default zone anywhere:** `ensure-magician-tunnel.sh`,
`ensure-connect-access.sh` and `ensure-ui-access.sh` resolve
`MAGICIAN_TUNNEL_ZONE` from the process env, then the identity layer, and die
when it is unset. `app-platform-processing-profile.sh` and
`eval-preplan-flow-live.py` derive config paths from `MAGICIAN_ROOT_DIR`.

### Supervisor and local services

- `scripts/run-supervisor.sh` starts magic-supervisor plus Magician/Magicutor on
  ordinary Rust/Tokio stack defaults (removes inherited `RUST_MIN_STACK`; the
  incident-only `MAGICIAN_EMERGENCY_RUST_MIN_STACK` logs a warning), logging to
  `magician.log` unless `SUPERVISOR_LOG_FILE`. It passes
  `--magician-config <runtime root>/magician-config.yaml` and forwards its own
  arguments (the desktop watch flags from `make run-supervisor`); see the
  [supervisor guide](../magic-supervisor/supervisor.md#desktop-app-macos).
  `scripts/run-native-backend.sh` forwards the LaunchAgent's extra arguments
  the same way; `native-runtime-service.py` takes `--supervisor-args-json` on
  `start` and a `sync-supervisor-args` action, both merging with the recorded
  arguments by the guide's binding rule. An empty, corrupt or non-dict
  registration counts as absent: `start` rewrites a clean one,
  `sync-supervisor-args` leaves it alone. Its `seed` (the From-source path)
  creates `.env` from `.env.example` but never `.env.development`, whose empty
  `KEY=` lines would shadow `.env` (both loaders read `.env.development` first
  and never overwrite). `scripts/install.sh` (local dev) runs `make
  run-supervisor SUPERVISOR_MANAGE_DESKTOP=0`, since it launches the tray
  itself; `make run-desktop-tray-debug` skips `open` when a tray already holds
  the gateway port, and `restart-desktop-tray-debug` refuses to open while the
  stopped tray still holds it.
  It raises the fd soft limit to
  8192 when lower: macOS's default 256 starves the memory-index maintainer at
  boot, leaving the LanceDB manifest missing and memory queries on direct
  ranking for the process lifetime. Python skills run through
  `skillshub/.venv` (the cli_template dispatcher prepends its `bin` to PATH);
  without it they fall back to host `python3` with a startup warning.
  `make -C skillshub setup-all` builds the venv early and ends with
  `verify-python`. See
  [Runtime async stack boundaries](../magician/runtime-async-stack-boundaries.md).
- `scripts/setup-ollama-host.sh` + `scripts/resolve-ollama-config.rb` — the
  resolver reads model tags, the `runtime.ollama.local_generation` gate, tiers and
  footprints from config, so the installer names no model. The embedding model is
  always pulled (memory is indexed locally in both processing modes). Generation
  is gated on OS/arch and physical memory (`min_memory_gb`; unreadable memory
  counts as 0), picking the first tier satisfied (36 GB+ → 27B, 24–35 GB → 12B,
  16–23 GB → Woof 4B), warning on currently free memory, then writing
  `local_generation.selected` (read by every Ollama profile via a YAML anchor) —
  failing if that write fails. `woof-4b` (MLX safetensors) is delegated to
  `setup-local-generation-model.sh`.
- `scripts/setup-local-generation-model.sh` + `data/magician_v2/local_generation_catalog.yaml`
  — `make setup-local-generation` lists the catalog; `MODEL=woof-4b` downloads
  and `ollama create --experimental`s; `SELECT=1` pins it. Guided Desktop setup
  uses `make setup-ollama-embedding` then `make setup-local-generation-selected`
  for the one chosen model. Seed default: Gemma 4. See
  [local channel LLM](../magician/local-channel-llm.md).
- `scripts/run-ollama.sh` / `stop-ollama.sh` — generation on `:11434`; a second
  owned embedding-only daemon on `:11435` (`run-ollama-embedding.sh`) with one
  model, the configured physical request capacity, prewarm, `/api/ps` context
  verification and `keep_alive: -1`. `scripts/verify_ollama_residency.py` applies
  Ollama's default registry/namespace/`latest` identity exactly
  (`make test-ollama-residency`). Embedding model/context/batching come only from
  `runtime.ollama`; remote endpoints are never replaced or spawned. Embedding QoS
  (`scripts/ollama-embedding-qos.sh`, `MAGICIAN_OLLAMA_EMBEDDING_QOS`) defaults to
  background (`taskpolicy -b` / `nice -n 10` / optional `taskset` cpuset); `off`
  disables it; QoS is part of the daemon settings signature. Magician workers are
  never pinned beside CPU-only Ollama. Stop terminates only owned PIDs.
- `scripts/install-verify.sh` requires generation reachability, a resident model
  on the embedding daemon, runnable `ffmpeg`/`ffprobe` (needed by `media_edit`),
  and for `FLOW=local` `pi` at the pinned version.
- `scripts/setup-decision-models.sh` (`make setup-decision-models [MODELS=…]`) —
  downloads the decision engine's ONNX Runtime (1.30.0, SHA-256 pinned per
  platform; Kev needs ≥ 1.30) and models (`laya-multilingual`, `kev-0.8b`,
  `kev-4b`, and MLX variants `laya-english-mlx`, `laya-multilingual-mlx`,
  `kev-*-mlx`) at pinned revisions with SHA-256 checks; mismatches are refused
  and removed. Defaults: `onnxruntime laya-multilingual` into
  `<root>/lib/onnxruntime/` and `<root>/models/decision/<name>/`
  (`DECISION_MODELS_ROOT`, else runtime root). `make build-decision-engine-mlx-release`
  / `-debug` build the `mlx` feature under `MLX_RUST_TOOLCHAIN` (default: the
  `rust-toolchain.toml` channel)
  after checking CMake and the Metal Toolchain. `scripts/laya-{onnx,mlx}-reference.py`
  regenerate parity fixtures in `magician-decision/tests/fixtures/laya/`.
- `scripts/setup-pi-coding-agent.sh` — reconciles the Pi CLI to
  `@earendil-works/pi-coding-agent@1.3.0` (`npm install -g --ignore-scripts`;
  Node ≥ 22.19). Required `phase_pi` in the local installer right after
  prerequisites, because the runtime lists an engine only when its binary is on
  PATH. The Dockerfile bakes the same pin under the Skillshub Node into
  `/opt/pi` (Debian's `nodejs` is too old) with a `/usr/local/bin/pi` wrapper.
  No Pi login is needed: Magician passes the selected profile's API key.
- `scripts/setup-meet-bot.sh` (`make setup-meet-bot`, installer `phase_meet_audio`,
  non-fatal) — macOS: `blackhole-16ch` cask (the bot's virtual microphone; needs a
  reboot), `switchaudio-osx`, and `magician-macos-meet-audio.bin`; listening uses
  ScreenCaptureKit, not BlackHole. Linux: `pactl`/`parec`/`pacat` and Xvfb,
  adding PipeWire-Pulse only when no Pulse layer works; an existing PulseAudio is
  not replaced. Without sudo credentials it prints the command. See
  [Meetings](../magician/meetings.md).
- `scripts/setup-desktop-vosk.sh` / `scripts/setup-magios-vosk.sh` — wake-spotting
  `libvosk` and `vosk-model-small-en-us-0.15` for desktop (runtime-root model plus a
  gitignored Tauri resource copy; idempotence checks model files, since `build.rs`
  creates an empty placeholder. Both installs build beside their final path and
  rename into place; an incomplete Tauri copy is rebuilt, and an incomplete
  runtime-root model is kept as `vosk-model.incomplete-<timestamp>` before a
  fresh download) and iOS (xcframework from device/simulator slices
  plus bundled model). The iOS binary comes from a pinned third-party tag and runs
  on the microphone path, so every slice is **SHA-256 pinned and a mismatch is a
  hard failure**. `--verify-only` backs `make verify-magios-vosk`. Run setup
  before any `xcodegen generate`.
- `scripts/setup-desktop-pnpm.sh` — installs the exact `pnpm@…` from
  `desktop/package.json` into `.cache/desktop-pnpm`, so desktop lanes need neither
  Corepack nor a global pnpm; an explicit `make PNPM=…` must match the pin.
- `scripts/setup-wizard.sh` (`make setup-wizard`) — builds `magician-setup` in
  release only when `magician-setup/src` or `magician-components/src` changed,
  honouring `CARGO_TARGET_DIR`, then execs the wizard.
- `scripts/ensure_make.sh` (`make ensure-make`) — bootstraps GNU `make` for
  Finder-launched Tauri apps and verifies it on the system default PATH
  (`/usr/bin:/bin:/usr/sbin:/sbin`), exiting 2 when installed only elsewhere.
- `scripts/stop-macos-audio-engine.sh` — idempotent FluidAudio cleanup on port
  3029, killing only a verified `magician-macos-audio-engine.bin` (bounded
  graceful exit first); called by `stop-magician` and `stop-supervisor`.

## Networking and access

- `scripts/ensure-magician-tunnel.sh` (`make ensure-tunnel`, prerequisite of
  `make run-all`) — idempotent Cloudflare Tunnel on `MAGICIAN_TUNNEL_ZONE`
  (required). Modes (`MAGICIAN_TUNNEL_MODE`): `browser` (locally managed config
  and DNS, `brew services`) and `token` (dashboard-managed; API-set hostname/DNS;
  macOS connector as user launchd job `com.magican.magician-cloudflared-token`,
  credential via a 0600 token file, never in process args). Ingress:
  `webhook.<zone>` path-split: exactly `^/agentmail-webhook/?$` → AgentMail bot
  `:3011` (`AGENTMAIL_WEBHOOK_PORT`; the bot also verifies Svix signatures), the
  rest → Kapso receiver `:3010`; optional `ui.<zone>` → Vite `:5173`;
  `connect.<zone>` path-split: `/host/*` → host gateway `:3017`, `/health` and
  `/api/*` → the selected backend (`MAGICIAN_CONNECT_BACKEND=local|container|remote`;
  remote needs an HTTPS `MAGICIAN_CONNECT_REMOTE_URL` distinct from the public
  host). `MAGICIAN_TUNNEL_PRESERVED_REMOTE_HOSTS` keeps explicitly listed aliases
  during a hostname migration; a failed remote-config read aborts rather than
  risking deletion. Never gate the webhook host with Access. The webhook, UI and
  connect hosts come from the process env, then the runtime env files
  (`MAGICIAN_WEBHOOK_HOST`, `MAGICIAN_UI_HOST`, `MAGICIAN_CONNECT_HOST`), and only
  then the shared `webhook.`/`ui.`/`connect.<zone>` names, so a machine set up by
  `ensure-machine-tunnel.sh --install` keeps its own hostnames. A webhook or UI
  host that fell back to the shared default is written to the runtime env as
  `MAGICIAN_WEBHOOK_HOST` / `MAGICIAN_UI_HOST` (honours `MAGICIAN_INSTALL_DRYRUN`);
  the Kapso and AgentMail bots register their webhooks from it on start.
- `scripts/select-connect-backend.sh` — `make connect-local|connect-container|connect-remote CONNECT_REMOTE_URL=…`
  preflights `/health`, reconciles the tunnel, verifies both ingress rules, and
  persists the choice to both runtime env files; `make connect-status` is
  read-only. Listener discovery never changes the route. It does not make
  concurrent writers safe: a native backend and the managed container must not
  share one runtime root.
- `scripts/magesp-connect.sh <terminal-ip> [base-url]` — points an ESP32 voice
  terminal at this machine (default: its `api_origin` in `remote-machines.yaml`)
  through the terminal's `/config`, with the device Access pair from the runtime
  env and `repair=1`, so it pairs again. Prompts for the passphrase on the
  terminal's screen (no echo); prints no secret. Needed after a terminal's
  hostname is retired (e.g. the shared `connect.<zone>` → `<machine>.<zone>`).
- `scripts/ensure-machine-tunnel.sh` — `make machine-tunnel MACHINE=<name> [INSTALL=1]` / `PARK=<host>`.
  Setup examples use `example.com`; configure your own zone and account through
  the private environment as described in the [Cloudflare setup guide](../../setup/cloudflare.md).
  One remotely-managed Cloudflare tunnel per Magician machine (`magician-<name>`),
  so machines never share connectors (Cloudflare load-balances a tunnel's
  connectors: OAuth callbacks and paired phones landed on the wrong machine).
  Hosts: `<name>.<zone>` (`/api`, `/health` → `:3002`; `/host` → `:3017`; behind
  Access via `ensure-connect-access.sh` with per-machine Access object names),
  `ui-<name>.<zone>` (`:5173`, Access via `ensure-ui-access.sh`) and
  `webhook-<name>.<zone>` (Kapso `:3010`, agentmail `:3011`; public). Without
  `--install` it creates the tunnel, ingress and DNS from any machine;
  `--install` on that machine switches the `com.magican.magician-cloudflared-token`
  connector to it and sets `MAGICIAN_MACHINE_NAME`, `MAGICIAN_CONNECT_HOST`,
  `MAGICIAN_MOBILE_PUBLIC_ORIGIN`, `MAGICIAN_UI_HOST`, `MAGICIAN_WEBHOOK_HOST`
  and `CLOUDFLARED_TOKEN` in the live env. After a Magician restart the Kapso
  and AgentMail bots re-register their webhooks on the new host (Kapso also
  retires the other machines' hooks for its number). Each run records the machine in
  `$MAGICIAN_ROOT_DIR/remote-machines.yaml` (owner data, not in git; sample:
  `magician_data_v3/remote-machines.example.yaml`). `park
  <host>` rewrites any tunnel rule for that host to `http_status:404`.
- `scripts/ensure-connect-access.sh` — Cloudflare Access boundary for iOS,
  Android and ESP32. The hostname stays behind the service-token policy; a
  more-specific bypass covers only the two device bootstrap exchange paths
  (ordinary companion enrollment and attested Android Apps enrollment,
  `POST /api/magician/v2/devices/enrollment/exchange`). It writes
  `MAGICIAN_MOBILE_PUBLIC_ORIGIN`, the outer credential, Access audience and
  issuer to both env files, so Magician verifies the assertion and never treats
  the service-token name as a principal. The bypass is usable only with a 256-bit,
  five-minute, single-use capability; ordinary calls also need a revocable device
  token. Origin Access mode defaults to `require` (an existing `verify` is kept for
  a loopback-only Apple container port). Rotation is make-before-break (stage →
  replace device profiles → finalize re-verifies before expiring the old secret;
  `status`/`extend` never revoke). GETs retry transient failures; mutations do not
  (rerun the idempotent provisioner). Honours `MAGICIAN_INSTALL_DRYRUN=1`; never
  prints credentials. `MAGICIAN_CONNECT_ACCESS_MOBILE_ENROLLMENT_ENABLED=0`
  disables both bootstrap exceptions.
  `MAGICIAN_IOS_*` and `ensure-ios-access.sh` remain read-only compatibility.
- `scripts/ensure-ui-access.sh` — gates `ui.<zone>` behind an Access app with an
  interactive email One-Time PIN policy (browsers cannot attach service-token
  headers). Requires `MAGICIAN_UI_ACCESS_EMAILS` or `…_EMAIL_DOMAIN` and a
  `CLOUDFLARE_API_TOKEN` with Access Apps/Policies Edit; session
  `MAGICIAN_UI_ACCESS_SESSION` (default 730h, applied at app creation). Publish via
  `MAGICIAN_TUNNEL_UI=1` or `make serve-ui-tunnel`. It also merges the app's
  audience into `MAGICIAN_CF_ACCESS_UI_AUD` in the runtime `.env` files: the UI
  host proxies `/api` to Magician with an assertion for this app, which the API
  refuses (`InvalidAudience`) unless that audience is accepted. Restart Magician
  afterwards.

## Desktop automation drivers

### Desktop driver pin

[`scripts/setup-cua-driver.py`](../../../scripts/setup-cua-driver.py)
(`make setup-cua-driver` / `make check-cua-driver`) installs exactly one CuaDriver
release, `CUA_DRIVER_VERSION` (0.28.2), from that release's own SHA-256-checked
installer scripts (never the moving `cua.ai` copy), passing
`CUA_DRIVER_RS_VERSION`. Any other installed version is replaced and
`cua-driver --version` must match afterwards; `--check` fails on drift. A
python.org macOS Python with an empty CA store falls back to `/etc/ssl/cert.pem`.
Desktop onboarding installs the same pin from `magician-components/src/setup_catalog.yaml`;
`make test-cua-setup` fails if the two differ.

To bump: change `CUA_DRIVER_VERSION` and hashes, confirm the macOS
`CuaDriver.app` passes `codesign --verify --deep --strict` (0.28.3 shipped
unsigned), install, then run `sync_desktop_action_enum.py`. Full setup:
[cua-setup.md](cua-setup.md).

### Desktop action enum

[`scripts/sync_desktop_action_enum.py`](../../../scripts/sync_desktop_action_enum.py)
writes the `macos-ui-automation` skill's `action_name` enum from
`cua-driver list-tools` plus the controller's `serve` / `status` / `stop`. An
unconstrained string makes models guess names, and the driver rejects unknown
names as `has no reviewed risk classification`, which reads like a gated
capability rather than a typo. Run after a driver upgrade; `--check` fails when
stale.

## Packaging, signing and release

- `scripts/package-release.sh` (`make package-release`) — native backend artifact:
  `magician`, `magicutor`, `magic-supervisor`, `decision-engine` (`.bin` /
  `.exe`), the pinned ripgrep as `rg.bin` / `rg.exe` (the `@vscode/ripgrep`
  copy from `skillshub/node_modules` for the target; when that is absent, as
  on a clean CI checkout or a cross-target package,
  `scripts/fetch-pinned-ripgrep.sh` downloads the tarball
  `skillshub/package-lock.json` pins and checks its sha512 integrity;
  `MAGICIAN_PACKAGE_RG` names another binary. A real package refuses without
  it, a fixture may omit it),
  `tool-runtime-config.yaml`, non-clobbering seeds, the transitive script
  closure, `MANIFEST.yaml` and `SHA256SUMS`. It packages only existing reviewed
  binaries (`MAGICIAN_PACKAGE_RELEASE_DIR` selects a target dir without
  relabeling). `make stage-desktop-native-backend` stages the unpacked tree for
  Tauri; `verify-desktop-native-backend.sh` rejects checksum, version, commit,
  target and signing drift, and a non-fixture package without its ripgrep, and `verify-desktop-native-bundle.sh` repeats it
  inside the finished macOS app, AppImage or NSIS installer.
- `scripts/package-installer.{sh,ps1}` verify `SHA256SUMS` and the exact target
  before copying (Intel Macs are refused: Apple Silicon on macOS 14+ only);
  Windows Desktop performs the same checksum-gated copy natively.
  `scripts/package-uninstaller.{sh,ps1}` remove only the recorded install and keep
  the data root unless its exact path is typed after an explicit delete request.
  `scripts/test-native-package.{sh,ps1}` (`make test-native-package`) test layouts,
  tamper rejection, uninstall and data preservation without a Rust build.
- `scripts/sign-release.sh` — distribution signing with a verified **Developer ID
  Application** identity, hardened runtime and timestamp; records
  `signing: developer-id|adhoc|local|none` in `MANIFEST.yaml`. Source packages
  set `MAGICIAN_SIGN_LOCAL=1`, reusing `replace-debug-bin.sh` for Desktop's
  identity selection, stable runtime code identifiers and revocation fallback.
  Local signing is distinct from distribution/notarization. (Ad-hoc is one-machine
  code identity; downloaded ad-hoc copies are quarantined and killed.)
- `scripts/replace-debug-bin.sh` signs a temporary copy and verifies its stable
  code identifier and Team ID before atomically replacing the launch path.
  Failed or interrupted signing preserves the previous executable.
  On APFS it clones the temporary copy to reduce staging's peak disk usage;
  other filesystems use a normal copy.
  `replace-release-bin.sh` signs and verifies the final copied executable.
  The debug stager checks
  Gatekeeper (`spctl --assess --type execute`, captured into a variable — under
  `pipefail` a piped `grep -q` is false for every rejected binary), because the
  keychain's validity flag lags Apple's revocation and a revoked-cert binary is
  killed and deleted about a minute after launch. On `CSSMERR_TP_CERT_REVOKED` it
  re-signs ad-hoc and warns: the binary runs, but desktop-owner sockets (Android
  Apps enrollment approval, macOS app pairing) stay unavailable until a new
  certificate is minted and runtime plus desktop app are re-staged. With
  `MAGICIAN_SIGN=1` the release copier requires Developer ID with hardened runtime.
- `scripts/apple_signing_transfer.py` — moves the release identity between trusted
  Macs without Git: `make export-apple-signing-setup OUTPUT=…` writes an
  AES-256/PBKDF2, HMAC-authenticated archive (Developer ID and Apple Distribution
  PKCS#12, the App Store Connect API key, the Tauri updater keypair);
  `verify-apple-signing-bundle`, `import-apple-signing-setup` (`--dry-run`,
  `--replace`) and `verify-apple-signing-setup ARGS=--network` complete it. Paths
  inside the checkout are refused. Development identities and profiles are
  excluded (let Xcode create them per Mac).
- `scripts/release_desktop_signed.py` (`make release-desktop-signed JOBS=2`) —
  local macOS release gate: reads identity and App Store Connect metadata from the
  protected signing directory and the updater password from Keychain, signs
  bundled services before Tauri packages them, lets Tauri sign/notarize/staple the
  `.app`, then signs, notarizes, staples and Gatekeeper-assesses the `.dmg` and
  verifies the updater archive. Fails if a sensitive signing file is tracked.
- `scripts/validate-desktop-release-env.sh` requires updater signing and
  notarization credentials. `scripts/generate-tauri-update-manifest.sh` requires
  signed updater archives for exactly macOS arm64/x64, Linux x64 and Windows x64
  and emits those four platform keys (`scripts/test-generate-tauri-update-manifest.sh`
  rejects an empty signature). Supported matrix: macOS DMG, Linux AppImage,
  Windows NSIS, each with updater.
- `scripts/local-update-server.sh` (`make dev-update-server`, port 8432) serves a
  generated `latest.json` and signed artifacts from the local bundle directory.
- `scripts/patch-macos-info-plist.sh` — legacy repair helper for older or
  hand-built `.app` trees; see
  [macOS media permissions](../desktop-tray/macos-media-permissions.md).

### Desktop debug app

- `verify-desktop-debug-app.sh` — preflight for `make run-desktop-tray-debug` /
  `restart-desktop-tray-debug`. Under hardened runtime a real-signed executable
  may load only dylibs with the same Team ID (ad-hoc counts as matching), so it
  compares the executable against every dylib in `Contents/Frameworks` and runs
  `codesign --verify --deep --strict`, failing before launch rather than dying in
  dyld.
- `materialize-desktop-debug-app.sh` — assembles `.local/Magican-Debug.app`
  (binary, icon, `Info.plist` seed with `LSMinimumSystemVersion` 14.0) and stages
  `magician-macos-speech-helper.bin` into `Contents/MacOS/` (the exe-sibling path
  the host gateway resolves) as nested code.
  - Identity order: **Developer ID Application**, else the first Apple Development
    identity (`MAGICIAN_DESKTOP_SIGN_IDENTITY` overrides), else ad-hoc with a
    warning. Developer ID comes first because Apple Development certificates are
    per-machine and revoked when re-minted elsewhere. `replace-debug-bin.sh` uses
    the same order so app and runtimes share one Team ID (the Android Apps owner
    socket requires it). Switching teams changes code identity once, so macOS
    re-asks Automation/Accessibility/Screen Recording.
  - Identities are picked by SHA-1 from `security find-identity -v`, skipping
    entries marked `CSSMERR_TP_CERT_REVOKED`/`_EXPIRED` (a re-minted cert shares
    its revoked predecessor's name). After signing, a revoked Gatekeeper verdict
    re-signs ad-hoc **without** hardened runtime (an ad-hoc signature has no Team
    ID, so library validation would refuse the bundled `libvosk.dylib`). Anything
    staged with a revoked cert must be re-materialized before its next restart.
  - Signing is not cosmetic: Automation grants key on stable code identity, so an
    ad-hoc signature loses them each rebuild (`-1743` without re-prompt).
  - Nested code is signed before its container; a real-signed app's nested
    runtime must share its Team ID.

## Mobile

- `scripts/magios-run.sh [build|deploy|run]` (`make ios-debug-build`,
  `ios-debug-run`) — targets a connected iPhone (a `physical` `devicectl` row with
  an 8–16 hex UDID) else a simulator. Routine builds do not run XcodeGen. The fixed
  `/tmp/magios_dd` DerivedData lets deploy find the build; only the exact stale
  explicit-module missing-PCM failure gets one cache-clearing retry. No credential
  seeding (endpoints are QR enrollment state). Plain builds omit APNs so Personal
  Team profiles sign; `ios-debug-build-push` / `-run-push` opt into sandbox push.
  Deploy ignores `*-Runner.app`, and refuses a stale build: a source newer
  than the build marker in the app, any extension target (including
  `MagiosNotificationService`) or `project.yml` stops it with "device build is
  stale". The Xcode log uses
  `mktemp "${TMPDIR:-/tmp}/magios-xcodebuild.log.XXXXXX"` — BSD `mktemp`
  substitutes only trailing Xs. Requires Xcode 15+.
- Physical-iPhone opt-in lanes (`MAGIOS_DEVICE_ID`, `MAGIOS_LIVE_TEST_HOST`):
  `make test-ios-live-notes` (Menu → Notes opens the library),
  `make test-ios-live-recovery` (terminate an active server-owned chat turn; one
  canonical reply returns after relaunch), `make test-ios-live-programs`
  (`MAGIOS_LIVE_PROGRAMS_FIXTURE_JSON`: save a program edit, render and answer a
  previewed approval, save a crew focus area's shared documents), and the lanes in the
  [integration harness](container-integration-harness.md).
- `magdroid-device.sh` (`make install-magdroid` / `run-magdroid` / `logs-magdroid`
  / `screenshot-magdroid`; `make check-magdroid` builds and tests). Resolves `adb`
  from `ANDROID_SDK_ROOT`/`ANDROID_HOME`/default SDK. It refuses with more than one
  device (set `ANDROID_SERIAL`) or no debug APK, and `install`/`run` build first.
  `run` uses the LAUNCHER intent (the activity is correctly not exported). Android
  drops the selected input method when its package is replaced, so install/run
  note and **restore** (never impose) this app's keyboard selection, enabling
  before selecting.
- `scripts/generate-magican-app-icons.py` (`make generate-magican-app-icons`) —
  renders the Outfit Regular `magican` wordmark and `m` mark into iOS, Android and
  Tauri icons, centred on painted coverage at 86% canvas width.
  `scripts/generate-magican-control-symbol.sh` converts the `m`+mic SVG into the
  custom SF Symbol for Control Center via SwiftDraw.
- `scripts/generate-wordmark.py` renders the README wordmark to
  `docs/assets/wordmark-{light,dark}.svg` as **paths** (GitHub strips `<style>`
  and blocks webfonts), two files because an `<img>` cannot inherit
  `currentColor`, cropped to real ink bounds. Run with the skillshub venv.

## Container scripts

Contracts: [container runtime](container-runtime.md),
[local prebuild/OCI](container-local-oci.md),
[integration harness](container-integration-harness.md).

- `make install` — composed installer: `MODE=dev FLOW=local` (default),
  `MODE=dev FLOW=container`, `MODE=user FLOW=container` (released image),
  `RUNTIME=apple-container|docker`, `YES=1` non-interactive.
- `make run-container` / `stop-container` follow `CONTAINER_RUNTIME`
  (`auto`/`docker`/`apple-container`), with host-service forwarding on Mac and
  `--network host` on Linux. `make dev-container-rebuild` rebuilds from source.
- `make prepare-container-image-local` — the normal image entry point;
  `make container-oci` packages/verifies without compiling;
  `make prebuild-container-artifacts` owns the cached Linux build (Zig on macOS).
- Qualification: `make test-container-tooling` (provider-free; entrypoint,
  router seeding/preservation, OCI tests), `make qualify-container-release`,
  `make test-container` / `test-container-full`, `make qualify-container-e2e`
  (disposable, builds by default), `make qualify-container-e2e-live` (live stack,
  explicit confirmation), `make qualify-container-integration`,
  `make qualify-mobile-connectivity` (offline checks:
  `make test-mobile-connectivity`), `make measure-container-image` (optional byte
  budgets).
- `scripts/test-container.sh` qualifies an image on configurable loopback ports
  with schema-versioned JSON (health, ports, non-root, agent-browser skill, bind
  mounts, limits, non-clobbering config, recreation persistence, restart recovery).
  `scripts/qualify-container-e2e.sh` composes setup, installer, health, host
  probes, restart persistence and live-root fingerprints.
- `scripts/prepare-container-keyring.py` — custody shared by installer and desktop
  (`make test-container-keyring-provisioning`); see
  [container runtime](container-runtime.md#persistent-device-pairing-on-headless-linux).
- `scripts/install-container-tools.sh` installs and verifies image tools in sync
  with the embedded `shell` pack, failing on any missing critical binary (`curl`,
  `jq`, `rg`, `bash`, `git`, `python3`, `node`, `npm`, `grep`, `sed`, `awk`, `find`,
  `convert`, `pdftotext`, `tesseract`); installs `uv` + `marimo` under `/opt/uv`.
  The Dockerfile makes `/usr/bin/bwrap` setuid for the Linux app OS jail (see
  [App OS-jail egress](../magician/app-os-jail-egress.md#linux-requirements)).
- `skillshub/browser/scripts/verify-skill-discovery.sh` proves scoped
  `skills get core --full` resolves in a relocated browser mirror and that
  operator `config/.env` survives rematerialization.
- `scripts/cleanup.sh` — manifest-driven tiered uninstall (`tools-and-data`,
  `only-tools`, `only-data`) for container and native installs; Desktop's
  native service and `~/.magician` prefix are removed only when Desktop
  installed them, and `--only-data` re-seeds and restarts that backend; see
  [install manifest](../desktop/install-manifest-cleanup.md).

## Codegraph

- `scripts/generate_code_graph.py` — `docs/codegraph/{graph,stats,payload_profiles}.json`
  over Rust, TypeScript, Python, SwiftPM and XcodeGen apps (Magios targets grouped
  into one crate; test directories classified; XCTest→production edges as
  `test_calls`), the flat `magicutor/extension/*.js` as a virtual crate, and
  inner-loop packs resolved to their three Rust dispatch engines.
  Actix routes are found with rustfmt's multiline trailing commas too
  (`scripts/test_codegraph_routes.py`).
- `scripts/graph_index_stamp.py` lets `make graph-index` skip unchanged input
  (digest of `HEAD`, `git diff HEAD` content, and untracked non-ignored
  path/size/mtime); it over-triggers, never under-triggers. `FORCE=1` /
  `make graph-index-force` reindexes; a missing output forces one.
- `scripts/query_code_graph.py`, `simulate_code_flow.py`, `find_flows.py` —
  symbol lookup and static flow discovery. `scripts/codegraph_exclude.txt` is the
  shared exclude list.
- `scripts/extract_contracts.py` → `docs/codegraph/contracts.json` (Rust, TS bot
  SDK, Python capability tools).
- `scripts/c4_model.py` / `c4_code_ladder.py` / `generate_c4_index.py` — merge the
  curated `docs/architecture/architecture.yaml` with a derived code ladder into
  `docs/codegraph/c4.json`; `make graph-check` fails when a `code_refs` entry no
  longer exists. `scripts/codegraph_ext/c4_slices.py` serves `GET /api/c4/slice`.
- `scripts/codegraph_ext_cli.py` / `codegraph_ext/*` — enrichers (AgentSkills
  metadata, Tauri config).
- `scripts/audit_code_graph.py`, `find_dead_code.py`, `test_coverage.py` — graph
  vs filesystem audit, dead-code candidates (test callers do not keep code alive),
  static test-coverage triage.

## Landing brand assets

`python3 scripts/build_landing_quest_mascot.py` extends the checked-in `on-it`
Lottie rig with a seamless coin-collecting side-quest loop, preserving the star's
face, proportions and floating companion. It writes the Day/Night `side-quest`
animations to both `docs/assets/brand/animations/` and
`ui/unified-ui/static/brand/animations/`, and adds the mood to the self-contained
animation preview. Review both appearances in that preview and the root hero
after regenerating. See the [brand animation catalog](../../assets/brand/animations/README.md)
and [root landing contract](../unified-ui/root-landing-native-route.md).

## Media and audio

- `scripts/audio-engine-control.py` — status/prewarm/unload through Magician's
  authenticated media API, never the sidecar directly (`make audio-engine-status`,
  `audio-model-prewarm`, `audio-model-unload`). Builds stage
  `magician-macos-audio-engine.bin`; `make test-macos-audio-engine` is
  provider-free, with explicit hardware gates via `MAGICIAN_FLUID_AUDIO_*` env.
- `scripts/verify-media-audio-rollout.py` fails closed on media-config drift
  across repo/live copies, default/order drift, invalid prewarm IDs, SDK-pin
  drift, missing Tauri resource mapping, or sidecar inclusion in the container.
- Fixtures and benchmarks (none are acceptance evidence for human speech):
  `generate-media-audio-fixtures.sh`, `generate-media-local-speech-fixtures.sh`,
  `benchmark-media-recording-stt.py` (never calls online providers without
  `--allow-online`), `media_offline_audio_eval.py` (`make benchmark-media-offline-audio`;
  provider-free `make test-media-offline-audio-eval`; see
  [Offline Audio Model Evaluations](../magician/fluid-audio-offline-evals.md)),
  and `eval_realtime_local_transcript_live.py` (Live Call caption acceptance;
  fails closed on missing or cross-call evidence).
- `scripts/screen-draw-*` smoke scripts: [screen-draw-smoke.md](screen-draw-smoke.md).
- Meet-bot spikes (`scripts/meet-bot/summarize_spike.py`, `transcribe_loop.py`)
  are throwaway pipeline proofs.

## Evals

Evals write JSON/HTML under `$(COVERAGE_BASE_DIR)/evals/<name>/` and appear on
`/evals`. Provider-free harness targets run in `make test`; cost-bearing live
lanes run only when selected.

**Aggregate runner.** `scripts/run-live-evals-with-report.sh` backs
`make test live_evals=1` / `make test-live-evals`: runs every live evaluator even
after failures, writes a linked dashboard and manifests under
`coverage/evals/live-suite/`, and returns the first failing gate.
`LIVE_EVAL_ONLY=<lane>` selects one child. Memory temperature runs last among
cost-bearing lanes (it can make local models resident), followed by the Phase 2F
reconciliation audit so every earlier call is included after the compatibility
mirror's flush (`LLM_PHASE2F_SETTLE_SECONDS`, default 35). Pre-plan uses fixture
HITL answers so the aggregate never blocks. `scripts/live-eval-preflight.sh`
checks the cloud key, both Ollama daemons and the server (`--check` read-only;
`--fix` starts daemons and may rebuild/restart Magician after `make check-magician`;
`--no-rebuild`). Authenticated server probes use `MAGICIAN_BEARER_TOKEN`;
missing or denied credentials are reported separately from stale builds.

**Decision and prompt contracts** (provider-free `.sh` gate plus opt-in live A/B
`.py`, reports under `coverage/evals/<name>/`): task-state schema compaction,
decision-metadata compaction, agentic terminal contract (active surfaces
advertise only `yield`; historical `goal_reached`/`cannot_proceed` still lower),
agentic native-tool contract, and decision rationale (≤ 48 schema bytes per tool;
240-character policy; history audit script). Live variants support `--dry-run`,
`--variant current` and `--self-test`.

**Preference alignment (judgment lane):**
`scripts/eval-preference-alignment-judgment.py` (plan archived in
`docs/archive/plans/`) validates the portable
`preference_alignment_case.v1` corpus (`data/magician_v2/evals/preference_alignment/`,
with its data card) and scores any judge's `alignment_decision.v2` predictions.
- Every prediction is host-validated first (scorer v2, `host_validate`, mirroring
  `contract::validate_decision`). A decision the host rejects scores as `malformed`
  with its `host_rejection`, and an over-strong one is lowered before matching.
  - **What the checks cover.** They include production's type and enum checks,
    such as a `null` list or an unknown resolution choice.
  - **The v2 rules.** `holding_intentions` must be non-empty exactly when
    waiting. A confirm that cites explicit evidence but rests on none is
    rejected.
  - **`holds_on` lint.** `validate` checks that it sits within `must_cite`
    and never in `must_not_cite`.
- Failure kinds are kept separate: missed conflict, unnecessary wait and so on.
- Rates carry Wilson intervals and are stratified by domain, evidence force and
  split.
- It never calls a model.
- `make test-preference-alignment-judgment-eval-harness` is provider-free.
- `make eval-preference-alignment-judgment PREDICTIONS=… [OUT=…]` scores a
  predictions file. `score` takes several `--predictions` files (repeats) and
  reports each run plus their mean and range.
  - `--raw` scores without the host validator, which is the prompt on its own.
  - `--budget-ms` counts a live call slower than the budget as unresolved, as
    production would.
  - Unresolved calls count as held: never a missed conflict, and an
    unnecessary interruption where no intervention was expected.
- `make eval-preference-alignment-judgment-live` **spends model calls.**
  - **How it runs.** It runs the bound reference judge over the corpus
    through the production call (`judge::reference_call`, an `#[ignore]`
    test in `preference_alignment/live_eval.rs`).
  - **Config.** It reads no live config. The router tables, judge route and
    budgets come from this branch's repository seed. Only the provider key
    comes from the live `.env`, which it reads and never writes. It fails
    loudly if the router cannot be built.
  - **Output.** It writes `predictions-r{N}.jsonl` with raw answers, latency
    and tokens, then the three reports: `report-governed.json`,
    `report-raw.json` and `report-budget.json`.
  - **Knobs.** `PA_LIVE_CASES`, `PA_LIVE_REPEATS`, `PA_LIVE_LIMIT`,
    `PA_LIVE_BUDGET_MS`, `PA_LIVE_OUT`.
- `make test-preference-alignment` runs this harness plus the Rust lanes: the
  corpus-to-host consistency gate, the scripted journeys and the routing
  regressions.

**Preference alignment (live journeys):**
`scripts/eval-preference-alignment-journeys-live.py --out DIR [--base-url …]
[--only ID…] [--keep-workspace]` **spends model calls.** It runs real chat
turns against the running service, with the real chat model and the live
judge. Each run gets a throwaway `lmeval-pa-*` workspace, which is purged
afterwards unless `--keep-workspace` is given.
- Journeys lane: an explicit rule holds the act. Cancel captures nothing,
  allow-once captures it, an unrelated act passes, and an "I know, but"
  proceeds.
- Adaptation lane: a preference said in chat governs the next act, and a
  correction said in chat supersedes it.
- Tool-loading lane: with no rule in play, two sends in one message are both
  captured (one load serves both), and a second send in a later turn of the
  same session is captured with the tool its session already loaded. A chat
  model that doubts a loaded tool is callable re-selects it until the spin
  guard ends the turn, which fails these.
- Outward acts are captured, never sent. The script refuses to run unless
  `outward_actions.capture_only` is true.
- A held act keeps the chat turn open until the owner answers. Each turn is
  sent on a worker thread while the script watches the session and answers
  both kinds of ask: the chat model's own question (a user request) and the
  alignment gate's hold. Every ask is answered, so an unexpected one fails
  the journey rather than holding the turn open until the send times out.
  More than one preference check for one act fails the journey: a needless
  second interruption. A question about what to write (a subject, an
  agenda) is answered with text and reported as `model_content_ask`; it is
  not a preference check. Each request spells out the subject and body, so
  such questions should not arise.
- Outward acts go through the assistant's own AgentMail inbox: a throwaway
  workspace has none of the owner's connected accounts. Chat sends with its
  own `agentmail-send` pack. Chat is offered the whole-pack entry
  (`runs_as_subrun`), and the pack's sub-run gets the typed, gated `send`
  leaf. It may also delegate.
- Chat follows a pack sub-run or a delegate after its turn returns, and the
  send (and any hold on it) happens in that run. So after each turn the script keeps answering
  asks until every delegated run's task card in the session is terminal (up
  to 300 s). Every pending request in the throwaway workspace is taken as
  this journey's, including a delegate's.
- Auth comes from `MAGICIAN_EVAL_USERNAME` / `MAGICIAN_EVAL_PASSWORD` (the
  environment first, then `$MAGICIAN_ROOT_DIR/.env.development`, then
  `.env`).
- One run swings widely (2/6 to 6/6 on one build, 2026-10-12): judge a change
  over several runs. Results and open failures are in the
  backlog.

**Agent and tool surfaces:**
`agent_surface_runtime_cache_eval.rs` (`make test-agent-surface-runtime-eval`),
`tool_result_projection_context_live_fixtures.rs` +
`eval-tool-result-projection-context-live.py` (`--rescore-report` reapplies gates
without new calls), `provider_replay_live_eval.rs` (interrupted tool history per
provider family), `capability_recall_eval.rs` (`make test-capability-recall-eval`),
`agent_tool_visibility_eval_catalog.rs` +
`eval-agent-tool-visibility-authorization-live.py` (cache-normalized cost gates;
`--render-report`), and `eval-tool-runtime-phase0-agentic-live.py` (decision-only
baseline over the frozen 416-schema catalog; never dispatches tools).

**Harness conformance:** `scripts/eval-harness-conformance-live.py` runs lanes
`chat|run|execution|voice|plane` against external harness engines, grading
effect over method plus proof of who answered (a silent fallback to Magician's
own mouth is a `fail`). Verdicts: `pass | partial | fail | cli_unavailable |
inconclusive`. Engine switches persist to live config and are restored in
`finally`. Execution lane: [execution-harness-eval.md](execution-harness-eval.md);
run lane: [eval lanes](../magician/eval-lanes.md#harness-conformance-run-lane);
fixes: [execution harness conformance](../magician/execution-harness-conformance.md).
Provider-free: `make test-harness-conformance-eval-harness`,
`test-voice-harness-eval`, `test-plane-harness-eval`.

**Product flows:**
- `eval-preplan-flow-live.py` — full pre-plan lifecycle via public APIs: every
  clarification must be visible in both `/plan` and `/feed/attention` with
  matching identity, clear after response, and pair with `hitl.resolved`, including
  grouped questions through their form's canonical request ID. Tasks are retained unless `--delete-tasks` is set.
  `--prompt` runs an arbitrary request; `make eval-preplan-flow-live-interactive`
  reads answers from `/dev/tty`. Its config reader accepts legacy scalar mappings
  and block selectors' `default` (inline or in `llm-router.yaml`); a selector
  missing or referencing an undefined `default` fails validation.
- `eval-web-researcher-live.py` — direct and delegated web-research tasks with the
  natural prompt, exact tool-roster preflight (asserted against the seeded
  definition), citation probes (≤ 4 MiB decoded text per page) judged by
  `evidence_precision_judge` on opened text only, and per-execution journal
  evidence. Analytics contention exhausts to an explicit `inconclusive` (exit 2).
- `eval-runtime-performance-live.py` — cross-flow resource benchmark; the first
  fully passing run is recorded once as the baseline and failures never promote.
- `eval-working-set-routing-live.py` — lane-off vs lane-on web-researcher runs
  by editing only `activation.enabled_lanes` in the live config (text surgery,
  restored in `finally`), with safety gates; `--as-configured` writes nothing.
- `eval-thinking-map-live.py`, `eval-monitor-change-ledger.py` (provider-free),
  `eval-monitor-live.py` (isolated `live-eval/monitoring` scope, physical cleanup),
  `task_recipes_eval.rs` / `task_recipes_live_eval.rs`, `eval-shared-decision-live.py`
  (native lane defaults to `chat-gpt61sol-responses-vision-toolsauto-fast`;
  `--workflow browser`, protocol v5), `vibe-plan-run-smoke.sh`.

**Content retrieval:** `test_content_reader_extraction_eval.py` +
`content_reader_quality_eval.rs` (`make test-content-reader-eval`; tables reach
text exactly once, never from nav/footer), `eval-content-browser-fixtures.py` +
`content_retrieval_runtime_live_eval.rs` (`make test-content-retrieval-eval`;
network lane `content-retrieval-runtime`), `eval-observable-sources-live.py`
(`--allow-network` resolves every request and redirect to public addresses).

**Memory and decisions:**
- `eval-memory-lifecycle.py`, `eval-memory-connections.py`,
  `eval-memory-response-journeys.py`, `summarize-memory-connections-eval.py` —
  see [memory lifecycle](../magician/memory-lifecycle.md#focused-qualification)
  and [memory connections](../magician/memory-connections-evaluation.md).
- `eval-memory-human-review.py` — `--batch-size`, `--expected-strategy
  per_item|shared_chunk`, `--locality cloud|local` (`--require-local-calls
  --local-model` rejects remote calls); partitioned datasets need `--partition`.
  Groups only equal operation/context cases, rejects drift before marking a
  comparison valid, and prices each physical request once.
  `make eval-decision-memory-human-review` preserves frozen labels and receipts
  and saves per-call prices in `pricing.json`; see
  [memory-hardening-review.md](memory-hardening-review.md).
- `decision-shadow-report.py` (`decision_memory_report.py`) — explicit scope,
  optional `--ledger`; missing usage, prices or human review stay INCOMPLETE or
  unknown, never zero, and the report never enables a gate
  (`make test-decision-memory`).
- `bench-decision-engine.py` — one model (or `--config` file) through the real
  engine binary: load time, latency percentiles, RSS and peak footprint;
  `--startup-timeout` default 600 s. See
  [structured decisions](../magician/structured-decision.md#eval-lanes).
- `memory_temperature_live_eval.rs`, `eval-ollama-logical-chunking.py`
  (isolated eval config so bake-offs cannot touch live routing;
  `ollama-llama-server-bridge.py` is eval-only), `compare-*-evals.py`,
  `make benchmark-chat-context-retrieval` (retrieval critical path; live lane
  requires hybrid backends and at most one physical embedding per turn).

**LLM observability** (`scripts/test_llm_trace_phase*.py`,
`eval-llm-observability-phase*.py`): provider-free drift gates for each trace
phase (journal durability, materialization, governed reads, product access,
activation, lineage) plus read-only content-free audits. Journal writer readiness
waits for the persistent index; Parquet publication catches up independently.
See [canonical trace storage](../magician/llm-training-data-observability.md#canonical-recorder-journal-and-materializer).
`compare_llm_context_latency.py` measures latency/cost/cache against captured
traces (6.1 Sol cached input priced at $0.10/M).

**Other:** `eval-storage-governance-live.py` (no provider calls or
production-scope deletion), `eval-task-state-schema-compaction.sh`,
`golden-eval-channel.py` + `probe-*` (channel-assist local models; requires the
stack down so it owns Ollama; `think:false` avoids thinking-field diversion and
repetition degeneration), `eval-swift-qwen38.sh` (eval-only bake-off).

## App platform

- `make test-app-indexed-query` — production index-only planner over isolated
  SQLite fixtures (10,000 records, ordering, stale indexes, deletes); pair with
  `make test-app-typescript-sdk`.
- `make test-app-reconciliation`, `make test-app-registry-admission` (controlled
  Tokio clock; isolated SQLCipher databases; per-connection SQL profiling),
  `make check-app-runtime`, `make test-app-contextual-round` (fair selection,
  identities, cancellation, recovery; also real store recipes) — none load the
  monolith or a model.
- `scripts/test-app-typescript-{consumer,real-server}-canary.sh`,
  `check-app-platform-private-scale.sh` (`make app-platform-private-check`),
  `app-platform-processing-profile.sh` (`make app-platform-processing-status|local|remote`),
  `qualify-app-custom-surface-host.sh`, `test-app-platform-p7-qualification.sh`.
  Provider-free processes never count as real Browser, CUA, Android or
  Artifact/workflow-owner canaries. Runbook:
  App Platform private-scale readiness.
- `scripts/verify-recurring-app-tasks.py` — `login` saves a 0600 session;
  `apps`/`health`/`details`/`dispatch`/`inventory`/`posts` inspect;
  `retry-blocked` requeues an exact refused fire through the owner API;
  `cleanup` (requires `--verified-recurring-task` and `--out`) retires legacy
  ambient tasks with a receipt journal, never touching files or app records.
- `make test-envoy-claims-review` — Envoy receipt, transcript-ingestion and bot
  channel regressions with mocked providers.

## Maintenance scripts

All default to dry-run/read-only and require `--apply`:

- `scripts/repair_attention_decision_links.py` — outcomes store raw candidate ids
  while decision items use `<surface>:<id>`, so the naive join is empty. For each
  unlinked outcome it takes the latest decision at or before it that contained the
  candidate and writes `decision_id` to both `attention_outcomes` and
  `attention_rank_recompute_jobs` (the worker reads the job's column). Resolvable
  jobs are requeued; the rest become terminal `no_served_decision`. It deletes
  nothing and never fabricates `impression_id`/`delivery_id` (client claims).
  `--apply` writes an fsynced undo file; `--undo <file>` reverts; re-runs are
  no-ops.
- `scripts/backfill_published_surface_feed_deliveries.py`,
  `purge_legacy_agent_message_feed.py` — explicit feed maintenance (optional
  `--principal`/`--workspace`), kept out of normal reads so removed cards are not
  recreated.
- `scripts/memory_audit.py` — memory shape/duplicate/noise audit with explicit
  rewrites and `--agent-rule-audit`; see [memory evals](../magician/memory-evals.md).
- `scripts/backfill_task_summaries.py` — uses the governed `task_summary`
  operation with the pinned prompt (`task_summary_system` v1.0.1); a
  legacy-local pass needs all three Ollama overrides.
- `purge_task_recipes_eval_workspaces.sh` — removes marked `recipes-eval*`
  workspaces and registry rows; refuses while `magician.bin` runs.
- `scripts/queue_residue_workspaces.py` — reports, per principal, workspace ids
  that are not registered but still have a `scopes/` directory or host-database
  rows (residue from deletions before tombstones, or wake-recreated dirs);
  `--apply` (magician stopped, under `workspaces.json.lock`, backup kept) appends
  them to `pending_purge` so the next boot purges, tombstones and retires them.
  Never queues a `default`, a registered id, reserved sinks, or principals
  without a registry (reported only). Run once on a data root upgraded to 0.7.97+.
- `scripts/validate_skill_artifact.py` — validates Skill Evolution
  `tool_schema.yaml` without executing skill code.
- `vibedev-cloudflare-pages-{check,deploy}.sh` — Pages preflight (non-mutating
  unless `--publish-smoke`) and Direct Upload wrapper printing
  `VIBEDEV_PUBLIC_URL=…`; need `CLOUDFLARE_ACCOUNT_ID` and a Pages/API token.
- `scripts/publish-marketing-site.sh` — deploys `magician-marketing` to Pages with
  custom domain `next.magican.ai`. A real `404.html` stops missing assets falling
  through to HTML; prerendered `/`, `/privacy`, `/manifesto`, `/terms`; HTML is
  `no-store` and `/_app/immutable/*` is immutable; service-worker tombstones
  retire legacy registrations. `make publish-marketing-preview` ships a noindex
  preview without changing the domain; `ARGS=--skip-build` reuses reviewed output.
- `scripts/public-site-assets.mjs` writes shared social/crawler assets and
  Explain's plain HTML overview; `scripts/verify_public_site.py` checks shipped
  metadata, canonical/schema, text, images, sitemaps and preview headers before
  either publisher uploads. [Public-site contract](../unified-ui/public-site-metadata.md).
- `scripts/publish-showcase.sh` (`make publish-showcase`, `make publish-showcase-preview`)
  deploys the anatomy showcase to Pages project `magican-explain`, with custom domain
  `explain.magican.ai`. It builds `showcase/anatomy/dist/` with `explorer/site.mjs`
  (three.js vendored, `_headers`) and refuses Git LFS pointer files.
  - `--preview` deploys to the `preview` branch and leaves the domain alone.
  - `--no-narration` ships the music but no voice clips; `--no-media` ships no audio.
  - `--dry-run` builds and reports without uploading.
  - With Make, pass these through `ARGS=`.
- `showcase/anatomy/refresh.mjs` (`make showcase-refresh`) brings the anatomy
  explorer up to date with the code. It re-extracts, moves tour and feature
  pointers to the code they follow, rebuilds `explorer.html` (stamping each box's
  last commit), and prints a drift report: broken pointers, new modules, big
  modules no feature covers, and unvoiced narration. `ARGS=--narrate` voices new
  lines; `ARGS="--min-lines N"` sets the uncovered-module threshold.

## Make Targets

- `make graph-index`, `graph-audit`, `graph-dead-code`, `graph-coverage`,
  `graph-check`, `graph-serve` (UI and allowlisted make-run API),
  `graph-serve-stop`, `graph-serve-restart`
- `make dev-desktop-setup` — generate the Ed25519 signing keypair and install
  desktop JS deps; `make dev-desktop-build` — build with the local update endpoint
  via `TAURI_CONFIG`
- `make tail-magician-v2-log` — tail `magician.log` `[MAGICIAN-V2` lines into
  `magician_v2.log`
- `make test-database-maintenance` — online Channel/Feed compaction, recovery,
  admission, scheduling and status API

Desktop build, check, test, signing and release targets resolve one `PNPM` at
`.cache/desktop-pnpm/node_modules/.bin/pnpm` (see `setup-desktop-pnpm.sh`).

`eval-longmemeval-live.py` preserves source conversation boundaries: digest
chunks from one original transcript are sent into the same runtime session.
`--max-digest-chars` remains conservative for legacy runtimes; source-complete
versions can use larger sends within ingress limits. The driver's self-test
checks session grouping, typed model/thinking sends and payload limits.
LongMemEval uses `--ingest-model`, `--answer-model` and `--judge-model`, each
with a corresponding `--*-thinking` option (default `auto`). Explicit choices
are validated against `/chat/models?engine=magician` before instance workspaces
are created. Retired profile flags are not accepted. `--resume` skips instances that
already have a judged record in the output directory's `results.jsonl`, so a
long serial run killed by a server restart continues where it stopped.
Before asking, the settle step also waits until the instance scope's memory
index has documents and has applied every journaled write (`manifest.json`
with a non-zero `document_count`, no entries in `changes.json`; `--index-wait-timeout`, default 600 s); until
then retrieval is lexical only or misses the latest sessions. The record keeps
the two verdicts apart: `settle_found` (the content probe became searchable)
and `index_status` (`ready`, `timeout`, `rebuild_required` when the
maintainer paused the scope, or `unchecked`), summarised as
`settle_found_rate` and `index_ready_rate` (reports from 2026-11-01 folded the
manifest check into `settle_found`). The index is checked
only for a loopback `--base-url`, under `MAGICIAN_ROOT_DIR`, then
`MAGICIAN_STORAGE_PATH`, else `~/MagicianNotes`. With no principal or no visible root the wait is skipped
with a warning. The question is sent
with a fixed framing that asks for answers from memory, with explicit
computation for counts, dates and ordering. Memory product correctness uses
`make test-memory-recall`; completed fixes are recorded in the
archived recall plan.
The full paired performance run remains spend-gated under the
active benchmark plan.

`eval-tau2-live.py` runs tau2-bench domains through the public chat API as a
registered `magician_agent` (`make test-tau2-live`; `TAU2_DOMAIN`,
`TAU2_EVAL_LIMIT`, `TAU2_EVAL_ARGS` for flags such as
`--answer-model` and `--answer-thinking`). Explicit choices use the same typed
model preflight and request contract as LongMemEval. Its HITL answerer must be tracking the task's
workspace before the opening send — opening turns can raise user requests
on their own. Questions the agent asks the customer through its input tool
(`need_user_input`, free text) are relayed, not auto-answered: while a turn
waits, the driver polls `GET /user-requests` for the session, hands the
question to tau2's user simulator as the agent's message (reflecting any
tool calls made before it first), and posts the customer's next message as
the answer (`POST /hitl/{id}/respond`, `{"type":"text"}`) to the SAME paused
turn — repeated for every question in the turn; a question that vanished
(answered elsewhere, withdrawn, or the turn finished) falls back to sending
the words as an ordinary message. The chat POST runs on its own thread in
this mode, since a paused turn holds it open. Confirmations and choices stay
auto-answered; `--no-hitl-relay` restores the old canned free-text answer,
which now tells the agent the customer only sees chat replies. Each task
records `hitl_questions_relayed`. Before a task workspace is purged its
transcript (messages, turn events with tool calls, bridge executions and
relay entries, sorted by timestamp) is exported to
`<output-dir>/transcripts/<task_id>-<slug>.json` — a failed export only
warns — and `--keep-workspaces` skips the purge. Rewards are read from the
run's `reward_info`. The
prompt-mediated tool route scored 5/5 tasks reliably but zero tool calls
(the chat agent only acts through its mounted tool surface), so the driver
now mounts each domain's tools NATIVELY: one governed-CLI skill per tool
(curl POST to a driver-hosted localhost bridge, arguments passed verbatim
via `canonical_json_stdin`), installed per task workspace with the scope
agent definition pre-seeded with the tool grants. The bridge executes each
call once against tau2's live environment and memoizes it; a wrapped
environment answers the orchestrator's replay of the adapter-reflected
calls without re-mutating, keeping db/action checks authoritative. Fresh
task workspaces need no stateless-loop seal (retired 2026-10-08), so the
driver no longer shells out to `magician.bin`. List parameters are
generated as `string_array` with explicit `max_items`/`max_item_bytes`
inside the runtime's 64 KiB combined argument budget — unbounded arrays
failed to compile before tool-runtime-core 0.1.82, silently dropping every
list-taking tool (retail exchange/return/modify-items) from the scope — and
each task fails before its first turn if the skills catalog reports a
mounted skill as `invalid`
(design and route verdict in the
benchmark plan).
Live validation of the mounted route is pending a runtime restart.
A turn is judged by progress, not length: a chat POST whose response is lost
is never re-sent (that duplicated the customer's message); the driver watches
`GET …/turns/{chat_turn_id}/events` and the tool bridge, and gives up only
after `--stall-seconds` (default 180) with neither moving, or past the
`--turn-max-seconds` backstop (default 3600, 0 = none) — cancelling the turn
via `DELETE /chat/sessions/{id}/run` and counting the calls it already made.

The `long-running-harness` nextest profile reuses the canonical Rust result
renderer for `make test-long-running-harness`; its report covers production
policy with injected time and short real child processes, without paid calls.

## Optional Cloudflare and Clarity installer configuration

`configure-cloud-services.py` is shared by source installation and the
`setup-cloudflare` / `setup-clarity` targets. It collects only explicit optional
choices, preserves unrelated env settings in both private files, and delegates
Cloudflare effects to `ensure-machine-tunnel.sh`. A zone change that leaves
saved hostnames on the previous domain clears those hostnames and then also
runs `ensure-magician-tunnel.sh`, so this computer’s tunnel and the original
shared tunnel follow the new zone. Old DNS records stay until each hostname is
parked with `make machine-tunnel PARK=<hostname>`. Clarity is runtime-configured
and off by default; `make setup-clarity ARGS=--disable` writes an explicit empty
setting without rebuilding. Non-interactive source preparation saves only explicitly provided optional
inputs; missing account prerequisites remain in the later capability plan.
A conflicting pair of private env values requires an explicit choice rather
than silently replacing one; writes flush data before atomic replacement.
Dry runs never prompt for credentials, save files or provision resources.
`test_cloud_services_setup.py` runs against temporary roots and injected dispatch.

Website Clarity has a separate `setup-clarity-website` target and catalog choice.
The build wrapper passes only its tracking ID from private runtime files to
`build-marketing-site`; `build-ui` explicitly blanks website recording input.
This prevents website consent from enabling personal-app recording.

Tunnel setup also routes immutable MCP profile JSON on each computer’s own
HTTPS hostname and adds an Access bypass limited to that public-profile path.
