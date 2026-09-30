# Scripts Docs

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

## Build and toolchain

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
- `scripts/prune-incremental.sh` (`make prune-incremental`) bounds the shared
  `$CARGO_TARGET_DIR/*/incremental` cache oldest-first under `--budget-gb`
  (default 40), skipping directories held open by a live rustc (`lsof`).
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
  removes inherited `RUST_MIN_STACK`, defaults `MAGICIAN_EXECUTION_DRIVER=inprocess`
  (explicit overrides kept), runs nextest plus all doctests, collects LLVM
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
- `test-magicvault-compatibility` covers upstream core/primitive compatibility
  plus Magician's async durable writer/facade/secrets cases, using `--lib` for
  unit tests so unrelated integration binaries are not compiled.
  `test-magicvault-secrets` reruns only the product secrets group;
  `test-magicvault-foundation` selects service/CLI/MCP fixtures at the same
  revision and does not replace product compatibility.
- `check-all` includes `check-extracted-libraries`. The Phase 5G guard reads its
  runtime evidence from the clean pinned checkout; the Python guard stays offline
  and fails on missing source.
- MagicVault is pinned at `5849709138e1d0ee9f2b7da4ca71b896eedbf3b1` (standalone
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
- `scripts/test_service_name_boundary.py` freezes rejected, allowed, and
  full-repository cases.
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

### Runtime root seeding

`scripts/install.sh`, `scripts/seed-silverbullet-space.sh`,
`scripts/run-supervisor.sh` and `scripts/container-entrypoint.sh` prepare the
runtime root without clobbering operator files:

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
- `seed-silverbullet-space.sh` seeds the notes folder skeleton only; there is no
  notes server or `notes.<zone>` route (notes are the in-process Markdown index).

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
  `magician.log` unless `SUPERVISOR_LOG_FILE`. It raises the fd soft limit to
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
  / `-debug` build the `mlx` feature under `MLX_RUST_TOOLCHAIN` (default 1.95.0)
  after checking CMake and the Metal Toolchain. `scripts/laya-{onnx,mlx}-reference.py`
  regenerate parity fixtures in `magician-decision/tests/fixtures/laya/`.
- `scripts/setup-pi-coding-agent.sh` — reconciles the Pi CLI to
  `@earendil-works/pi-coding-agent@0.87.1` (`npm install -g --ignore-scripts`;
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
  creates an empty placeholder) and iOS (xcframework from device/simulator slices
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
  `webhook.<zone>` → Kapso receiver `:3010`; optional `ui.<zone>` → Vite `:5173`;
  `connect.<zone>` path-split: `/host/*` → host gateway `:3017`, `/health` and
  `/api/*` → the selected backend (`MAGICIAN_CONNECT_BACKEND=local|container|remote`;
  remote needs an HTTPS `MAGICIAN_CONNECT_REMOTE_URL` distinct from the public
  host). `MAGICIAN_TUNNEL_PRESERVED_REMOTE_HOSTS` keeps explicitly listed aliases
  during a hostname migration; a failed remote-config read aborts rather than
  risking deletion. Never gate the webhook host with Access.
- `scripts/select-connect-backend.sh` — `make connect-local|connect-container|connect-remote CONNECT_REMOTE_URL=…`
  preflights `/health`, reconciles the tunnel, verifies both ingress rules, and
  persists the choice to both runtime env files; `make connect-status` is
  read-only. Listener discovery never changes the route. It does not make
  concurrent writers safe: a native backend and the managed container must not
  share one runtime root.
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
  `MAGICIAN_TUNNEL_UI=1` or `make serve-ui-tunnel`.

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
  `.exe`), `tool-runtime-config.yaml`, non-clobbering seeds, the transitive script
  closure, `MANIFEST.yaml` and `SHA256SUMS`. It packages only existing reviewed
  binaries (`MAGICIAN_PACKAGE_RELEASE_DIR` selects a target dir without
  relabeling). `make stage-desktop-native-backend` stages the unpacked tree for
  Tauri; `verify-desktop-native-backend.sh` rejects checksum, version, commit,
  target and signing drift, and `verify-desktop-native-bundle.sh` repeats it
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
  `signing: developer-id|adhoc|none` in `MANIFEST.yaml`. (Ad-hoc is one-machine
  code identity; downloaded ad-hoc copies are quarantined and killed.)
- `scripts/replace-debug-bin.sh` / `replace-release-bin.sh` sign the final copied
  executable and verify the Team ID at the launch path. The debug stager checks
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
  Deploy ignores `*-Runner.app`. The Xcode log uses
  `mktemp "${TMPDIR:-/tmp}/magios-xcodebuild.log.XXXXXX"` — BSD `mktemp`
  substitutes only trailing Xs. Requires Xcode 15+.
- Physical-iPhone opt-in lanes (`MAGIOS_DEVICE_ID`, `MAGIOS_LIVE_TEST_HOST`):
  `make test-ios-live-notes` (Menu → Notes opens the library),
  `make test-ios-live-recovery` (terminate an active server-owned chat turn; one
  canonical reply returns after relaunch), and the lanes in the
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
  `only-tools`, `only-data`); see
  [install manifest](../desktop/install-manifest-cleanup.md).

## Codegraph

- `scripts/generate_code_graph.py` — `docs/codegraph/{graph,stats,payload_profiles}.json`
  over Rust, TypeScript, Python, SwiftPM and XcodeGen apps (Magios targets grouped
  into one crate; test directories classified; XCTest→production edges as
  `test_calls`), the flat `magicutor/extension/*.js` as a virtual crate, and
  inner-loop packs resolved to their three Rust dispatch engines.
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
`--no-rebuild`).

**Decision and prompt contracts** (provider-free `.sh` gate plus opt-in live A/B
`.py`, reports under `coverage/evals/<name>/`): task-state schema compaction,
decision-metadata compaction, agentic terminal contract (active surfaces
advertise only `yield`; historical `goal_reached`/`cannot_proceed` still lower),
agentic native-tool contract, and decision rationale (≤ 48 schema bytes per tool;
240-character policy; history audit script). Live variants support `--dry-run`,
`--variant current` and `--self-test`.

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
  matching identity, clear after response, and pair with `hitl.resolved`.
  Disposable tasks are cancelled/deleted so nothing leaks into user surfaces.
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
  operation; a legacy-local pass needs all three Ollama overrides.
- `purge_task_recipes_eval_workspaces.sh` — removes marked `recipes-eval*`
  workspaces and registry rows; refuses while `magician.bin` runs.
- `scripts/validate_skill_artifact.py` — validates Skill Evolution
  `tool_schema.yaml` without executing skill code.
- `vibedev-cloudflare-pages-{check,deploy}.sh` — Pages preflight (non-mutating
  unless `--publish-smoke`) and Direct Upload wrapper printing
  `VIBEDEV_PUBLIC_URL=…`; need `CLOUDFLARE_ACCOUNT_ID` and a Pages/API token.
- `scripts/publish-marketing-site.sh` — deploys `magician-marketing` to Pages with
  custom domain `next.magican.ai`. A real `404.html` stops missing assets falling
  through to HTML; prerendered `/privacy`, `/manifesto`, `/terms`; HTML is
  `no-store` and `/_app/immutable/*` is immutable; service-worker tombstones
  retire legacy registrations.

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
