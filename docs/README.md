# Documentation Hub

[Concurrent voice requests](components/unified-ui/concurrent-voice.md)
documents implemented independent realtime, Hands-free and dictation work with
internal conversation branches, coordinated playback and durable result delivery.
The runtime contract records signed-in web validation and remaining rollout work;
the original design plan
is archived.

[Claims Review](components/unified-ui/claims-review.md) connects Envoy channel
replies to a native review queue and the permissioned app projection. Validate
the connection with `make test-envoy-claims-review` (including lost-response,
recipient-binding, extended receipt-outage recovery, partial-import source
binding, saved-confirmation recovery, provider retry/template safety, Gmail
exact-body, WhatsApp server acknowledgements, authenticated AgentMail webhooks
and app pagination regressions). The lane also checks email sender attribution
and prevents transcript retries from replacing a decided segment's claim identity.
Telegram provider fixtures verify message receipts and bounded sends; AgentMail
regressions cover stalled success/error bodies after headers arrive.

[Scripted app page authentication](components/magician/custom-surfaces-v1.md#browser-asset-authentication)
uses scoped live-session credentials for embedded page files while keeping data
and actions bearer-authenticated. `make test-app-scripted-surfaces` exercises
the session kernel and production authentication layers.

Decision Engine participation is controlled in Magician Settings (All Engines / Magician Only / Off); `make test-decision-mode` checks persistence and access control. The same Settings card provides per-operation local/cloud primary and backup models, model-specific thresholds, and live routing status.

[Shared tool decisions](components/magician/structured-decision.md) use one Decision Engine path across managed chat and agentic harnesses. Memory classification uses contract v5 and decision crates `0.4.2`, requiring a paired host/engine upgrade; existing tool thresholds remain 0.7. `make test-decision-memory DECISION_MEMORY_GROUP=engine|host|report` selects a focused regression group, including observation admission checks in the host lane (omit the selector for all).
The host lane can run Magician and comms separately with `make test-decision-memory-magician` and `make test-decision-memory-comms`; `make test-decision-memory-owners-build` compiles the opt-in real-owner fixture before model startup. Four small hardening review batches are diagnostics with reviewed labels (the first retains one unresolved head); the approved 600-case H3 evaluation found that no shared-chunk operation passes its current-route gate.

Memory owner recovery and rollout evidence live in the archived memory decision plan and hardening record. Per-item Jev/Kev decisions use 0.7 for routine questions and 0.8 for memory-changing questions by operator authorization; observation sampling is off and shared chunks failed qualification. The approved human-review batch records earlier fallback results and incomplete empirical qualification.

[Runtime startup and readiness](components/magician/startup.md) describes the
early HTTP/UI surface, feature admission, parallel initialization and deferred
background maintenance.

Current evaluation and acceptance Make lanes use GPT-6 Luna and GPT-6.1 Sol
profiles; the GPT-5.6 names remain available as explicit fallbacks. Old
`gpt6sol` IDs alias the new model for saved selections. See [chat profiles](components/magician/chat-profile-routing.md#openai-default).
`make test-gpt61-sol` checks pricing, routing, Responses, and Pi contracts.
Sarvam 105B is available as a text-only Indic-language chat profile; see [provider behavior](components/magicllm/multi-llm.md) and [API key setup](setup/api-keys.md).

[Stable device connection routing](quickstart.md) uses
`connect.<zone>` for iOS, Android, and ESP32. `make connect-local` and
`make connect-container` select a healthy local backend; `make connect-remote`
and Magican Desktop Settings can instead select a verified remote HTTPS
Magician behind the same unchanged origin. `make connect-status` never mutates
routing.

[Managed container credential provisioning](components/scripts/container-runtime.md#persistent-device-pairing-on-headless-linux)
shares private keyring custody between installer and desktop, with replacement
preflight, `make test-container-keyring-provisioning` regressions and the opt-in
`make test-desktop-managed-container` backend acceptance lane.

[Mac-native Linux image builds](components/scripts/container-local-oci.md) use
Zig with two host jobs for routine Rust/UI updates, then package those ELF
artifacts onto a reviewed OCI runtime base without a compiler container. Run
`make prepare-container-image-local` every time; it selects adoption, reuse or a
runtime-layer rebuild automatically. Add `ARGS=--force-rebuild-from-scratch`
only when deliberately discarding base reuse.

[Cross-platform CUA setup](components/scripts/cua-setup.md) covers desktop
installation and read-only checks on Windows, Linux and macOS, plus the
headless backend's desktop relay.

Physical iPhone process-loss acceptance runs through
`make test-ios-live-recovery`; its setup and evidence contract are documented in
[Magios testing](../magios/README.md#tests).

[Desktop delivery](components/desktop/README.md) covers macOS DMG (Apple
Silicon, macOS 14+; Intel Macs are not supported — the Desktop CI matrix and
`make release-desktop-target` no longer offer `x86_64-apple-darwin`), Linux
AppImage (x86_64), and Windows NSIS (x64) builds, local-versus-remote engine selection, and
which host capabilities remain platform-specific. Its guided capability setup
also owns the local-versus-remote processing choice, the required PPLX install,
one selected local-generation model, and the bundled browser-extension check.
The standard Linux and Windows releases ship native Desktop Edge applications
for the Magician Linux OCI image. Their setup can install and manage a local
container, connect to one already running, or connect to a remote container;
Windows uses Docker Desktop and private Local AppData credential custody. macOS
release builds may bundle native services. `make release-desktop-native` retains the explicit
cross-platform native-service path for future development and acceptance, and
`make test-native-package` checks its package/install contract without building
Rust. Tagged macOS jobs additionally require Developer ID and Gatekeeper
verification. Desktop packaging clears cached Tauri bundle output before each
build; the Windows lane accepts exactly one NSIS installer carrying the current
Desktop version. Manual runs of the same workflow can select `windows` for an
isolated Windows acceptance build; release calls retain the full platform
matrix.
`make component-graph` validates both the component graph and its declarative
setup catalog before refreshing the committed install-surface artifact.
`make test-edge-transport` covers the shared wire/session contracts, admission,
and both the outbound connector and local CUA/browser/iMessage dispatcher.
The root target invokes desktop tests through `desktop/src-tauri/Cargo.toml`.
`make build-decision-engine-release` / `-debug` build and install only the
structured-decision engine (`decision-engine.bin`). On Apple Silicon they, and
`make build-all-debug` / `build-all-release`, build it with Kev on the GPU
(`kev-mlx`; needs Rust 1.95, CMake, and Xcode's Metal Toolchain) through the
`-mlx-` targets; `DECISION_ENGINE_MLX=0` forces the CPU engine, which skips
every `kev-mlx` model.
`make setup-decision-models [MODELS="onnxruntime laya-multilingual laya-multilingual-mlx kev-0.8b kev-0.8b-mlx"]`
installs the ONNX Runtime 1.30 its local models need and the models
themselves (laya, Kev), each pinned and SHA-256 checked, under the runtime
root, and
`make restart-decision-engine` / `stop-` / `status-decision-engine` drive it
through the supervisor, so a decision change ships without a Magician rebuild
([supervisor](components/magic-supervisor/supervisor.md#decision-engine)).

Secure HITL credentials and time-bound OTPs
plans consistent secure input across chat and automation, MagicVault custody,
OTP expiry and single-use submission, urgent Kapso/Telegram alerts, secure
email/SMS code retrieval, and verified destination delivery.
The built-in [one-time browser credential flow](components/magician/jit-browser-credentials.md)
implements the JIT browser subset;
[critical-request delivery](components/magician/critical-request-delivery.md)
(P5) alerts the owner's verified Kapso/Telegram/push destinations with a
value-free card and a link to the exact request, with `make
test-hitl-delivery` as its focused lane;
[verification-code retrieval](components/magician/verification-code-retrieval.md)
(P6) lets a code that arrives in a permitted inbox, the local Messages store
or on the owner's Android phone answer a live verification challenge without
ever reaching a model (`make test-verification-codes`); the in-process
qualification lane (P7) drives real executions against a fixture login
service with canary sweeps over every storage class
(`make test-secure-hitl-qualification`).

Structured decision plane
plans a vendor-neutral Choice/Score/Noul runtime for Channel Assist land/lane
judgments, with TypeSafe Jev as the first adapter and later Jev-style models as
config plus adapter changes. SOL/Qwen stay for briefs; Thompson sampling stays
for personal ranking. Proposed only; idle-until-bound, shadow before flip.

Magician-owned Electron browser
plans a Chromium daily-driver shell beside Tauri Magican Desktop, so browsing,
right-click Magician actions, and signed-in automation do not require the
Chrome MV3 extension. Proposed only; first slice is the shell and native
context menu.

Identity/workspace resource limits
plans a single-server hosted step: shared host limits, independent pair quotas
and fair scheduling, plus a workspace count cap per identity. Proposed only;
implementation and runtime changes are deferred.

[Agentic execution harness conformance](components/magician/execution-harness-conformance.md)
documents the execution-engine replacement seam, focused contract tests and
the launch-pinned local read/write evaluation. `make test-execution-harness`
and `make test-execution-harness-live` keep this lane separate from chat-engine
evaluation and do not change the shared engine settings.
The structured-decision plane has its own focused lane:
`make test-decision-rail` (shared action service and policy) and
`make test-decision-rail-host` (host, selected harness and proposal grants), plus
`make test-decision-crate` (crate + adapter contract tests),
`make bench-decision-jev` (live Jev selection and stale-evidence refusal through `/v1/action`,
needs `TYPESAFE_EVAL_KEY`, never in CI); see
[structured-decision.md](components/magician/structured-decision.md#eval-lanes).
The live adapter targets, including the precompiled-binary runner, carry eval
metadata for `evals/harness-conformance/execution/adapters/latest`. The task
recipe fixture lane keeps its live annotation on the executable test target.
The plane door has its own pair: `make test-plane-harness-eval` (provider-free
contracts) and `make test-plane-harness-live` (a scripted MCP client and the
installed CLIs on `plt_` grants; see the plane lane in
[eval lanes](components/magician/eval-lanes.md#harness-conformance-plane-lane)),
and so does voice delegation: `make test-voice-harness-eval` and
`make test-voice-harness-live` (GPT Realtime and GPT Live 1 driven by
synthesised speech; see the
[voice lane](components/magician/eval-lanes.md#harness-conformance-voice-lane)).

[Memory consolidation and lifecycle](components/magician/memory-lifecycle.md)
documents shared writes, deduplication, correction, retirement and durable HITL
reconciliation, focused test results and retained model comparison failures.
The [Evals page](components/unified-ui/evals-page.md#memory-lifecycle) supports
repeat runs, configured-profile comparisons and report history.

[Memory connections and owner attention](components/magician/memory-connections.md)
documents how related memories inform Worth a look, For you and durable HITL
clarifications, including source checks, budgets and background latency.
The real-model evaluation
records the scoped-profile handoff fix, passing restart journeys and remaining
held-out/device acceptance gaps.

This `docs/` folder is the canonical entry point for active workspace documentation.
The C4 architecture canvas is generated from
[`architecture/architecture.yaml`](architecture/architecture.yaml) into
[`codegraph/c4.html`](codegraph/c4.html). It now names the replaceable
runtime (plane + chat mouth), apps platform, and background ops.
The current runtime/storage contract separates checked-in V3 seeds from live
runtime state: repo seeds live under `magician_data_v3/system/...` plus the
non-secret default scope seed at `magician_data_v3/scopes/anonymous/default/...`,
while live scoped state lives under
`$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/...` (default
`~/MagicianNotes/scopes/<principal>/<workspace>/...`).
The raw storage base path is no longer treated as a scoped root by itself even if
it contains top-level `scopes/` or `system/`; active runtime ownership always
resolves through an explicit runtime root.
Durable placement is independent of compute placement. The typed storage
contract is [Storage Abstraction](components/magician/storage-abstraction.md);
copying a scope directory is not a supported migration. Default startup stays
local embedded storage. `make check-typed-storage-boundaries` (part of
`make check-all`) rejects new implicit-path bypasses.
Config resolution prefers the live runtime root file
`$MAGICIAN_ROOT_DIR/magician-config.yaml` (default
`~/MagicianNotes/magician-config.yaml`), then the git-backed repo-root
`magician-config.yaml` (with its sibling `llm-router.yaml`); the former
`magician_data_v3` template pair was removed on 2026-09-24. During development,
a new configuration property or default must be applied to both surfaces:
mirror the repository seed into the live runtime config while preserving any
deliberate operator-only overrides. The TinyFish-first retrieval rollout
follows this contract: both currently set
`content_acquisition.progressive_retrieval.discovery_attempt_timeout_ms` to
`15000`.
Town Square's reviewed `compose_post` operation is declared in the config seed,
with matching remote and local processing routes in the shipped
`llm-router.yaml`. The older `engagement_gate` entry remains for
compatibility with immutable installed packages. Existing deployments need the
same entries in their live config and router to dispatch an already granted
behavior. See [Town Square](components/magician/town-square-app.md) for the
independent scheduling and posting controls.

Create development worktrees with `make worktree-add WT_PATH=... WT_BRANCH=...
[WT_BASE=...]`. The target delegates to the repository's guarded worktree
helper, refuses existing destination paths, and configures `.githooks` in the
new checkout. Run `make setup-hooks` for an existing clone or worktree. When
the SSD build root is available, Makefile-driven Cargo commands automatically
isolate linked-worktree artifacts under `/Volumes/build/magician/builds/wt/`.
`make build-magician-debug` selects the `magician-bin` package explicitly so
unrelated workspace test dependencies do not join its build selection. Learning
and chunking enable the monolith's fixture feature only for tests or explicit
fixture consumers, keeping it out of this production debug graph.
For recurring Apps scheduling, see [Recurring App tasks](components/magician/recurring-app-tasks.md).
`make test-app-recurring` covers occurrence identity, scheduling and execution
history; `make test-app-recurring-ui` covers the paginated task drawer.

For mechanical Apps synchronization, `make test-app-reconciliation` exercises
the production reconciliation planner from the binary crate without enabling
the monolith's fixture dependency graph. Pair it with
`make test-app-typescript-sdk` and the package CLI checks described in
[App Recipe IR](components/magician/app-recipe-ir.md).

Unified UI imports the Apps TypeScript SDK source. Its build, check, setup and
dev Make targets also install the SDK's committed dependency lockfile, so a
fresh checkout resolves BLAKE3 without a separate SDK test/build first. See
[Unified UI local development](components/unified-ui/README.md#local-development).

`make test-terminal-outbox` checks terminal delivery ordering and retirement of
proven deleted-task recovery debt. This library test lane explicitly enables
the shared fixture feature required by satellite fixture imports; production
debug builds continue to use `make build-magician-debug` without that feature.

The MagicVault extraction is complete and archived. Magician consumes pinned
`magicvault-core` `0.1.6` and `magicvault-primitives` `0.1.2` at
`5849709138e1d0ee9f2b7da4ca71b896eedbf3b1` through existing secrets facades; it
does not link the standalone MagicVault daemon, CLI, or MCP. The temporary `[patch]` that linked the sibling `../MagicVault-secure-hitl` worktree has been removed now that the secure-HITL custody work landed on MagicVault `main`; the workspace pins only literal revisions. Both MagicRun and
MagicVault are public Git repositories. Historical Magician extraction records:
design,
Phase 1 ledger,
Phase 2 foundation,
static review,
targeted tests,
second deep review,
and meetable_bot integration.
Standalone MagicVault product work continues upstream.

Current development versions include Magician `0.7.89` and Magician API `0.3.31`
(CLI `0.2.20`, Event Taxonomy `0.1.5`, Apps `0.2.7`, Unified UI `0.1.31`,
Desktop `0.3.18`); the complete inventory is in [Project Versions](project-versions.md).
The 2026-09-12 recurring App task update reuses one internal task per scheduled
behavior, with independently governed runs, completion-based scheduling and
paginated execution history. Its implementation and live cleanup are recorded
in the completed plan. The 2026-09-06 Task Recipes development checkpoint adds
answer-directed API recipe compilation, scoped replay with guarded writes and
browser continuation, durable recovery, and task-centric API-mining controls.
The integrated runtime also carries task-write deadlock recovery, bounded
startup reconciliation, smaller async stack frames, and the browser session
gate fixes. The Task Recipes runbook
defines the acceptance procedure. Typed storage Track A is
closed: default startup stays `local_embedded`, the catalog is 89/74/10/5, and
remote adapters stay unselected. The 2026-09-02 follow-up keeps rollback,
SecretRef, CLI CWD, parquet, and JSONL commit-marker contracts fail-closed. Chat turns can think with
an installed roster harness via `chat.harness_engine` (default `magician`);
hands stay on the plane. `make test-chat-turn-engine-eval` is the `/evals`
harness lane; `make test-harness-conformance-live-eval` swaps the chat engine
for each installed harness and grades the same turns by effect and by who
answered. The 2026-09-02 evidence-queue implementation checkpoint adds the
bounded `evidence_data` app binder, a claims/commitments review package with
declarative companions and a sandboxed console, revision-bound durable decision
receipts, and the separately signed `magician.claims-decision` destination seam.
It deliberately requires one explicit agent for evidence/entity reads and one
explicit audience for commitment reads; package ledgers remain projections, not
claim authority. The 2026-09-01 domain-cutover release removes legacy custom
schemes and the retired operator domain's configuration, moves the public site to
`next.magican.ai`, publishes directly openable policy pages, pins Google
Workspace tools to `reach.magican@gmail.com`, and verifies Kapso webhook
signatures before dispatch. It coordinates Unified UI `0.1.8`, Desktop
`0.3.3`, Magios `0.2.3` (build 196), Magdroid `0.4.4` (code 19), Skillshub
`0.2.2`, and `@magician/bot-kapso` `0.2.2`.
The preceding 2026-09-01 go-live review release makes **Magican** the
alias-free primary identity, carries `wake_spellings` through backend, web,
iOS, and Android, adopts `gpt-transcribe` for non-diarized recording, and gives
runtime-managed bots process-lifetime credentials that never rest in env files.
It coordinates Magios `0.2.2` (build 195) and Magdroid `0.4.3` (code 18).
The preceding 2026-08-31 boundary-hardening release keeps scope authority
inside bearer credentials, restricts terminal grants to the plane MCP door,
closes proxied ESP pairing and cross-origin credential leaks, and coordinates
Desktop `0.3.2`, Unified UI `0.1.4`, Magios `0.2.1` (build 194), Magdroid
`0.4.2` (code 17), Magicutor `0.1.89` (extension `0.2.1`, named Magican), MageSP `1.9.3`,
and Bot SDK `0.3.5`.
It builds on the stateless lifecycle integration closure: durable accepted
launch, exact recovery and lifecycle ownership, canonical terminal projection,
and warning-free Rust all-target compilation. The spend Resource Authority review
shipped in Magician `0.7.3` / API `0.3.5`: one `spend_session::admit` writer,
live in-flight ceiling occupancy, YAML token resync after restart, and
non-retry MCP checkout after remote success. The 2026-08-30 stateless hardening release
is Magician `0.7.2`, Magician API `0.3.4`, Magician CLI `0.2.2`, runtime-core
`0.1.7`, Event Taxonomy `0.1.2`, Unified UI `0.1.1`, and Skillshub `0.2.1`. New runs resolve to the stateless driver when
`MAGICIAN_EXECUTION_DRIVER` is unset, blank, `stateless`, or unrecognized;
`inprocess` is the explicit rollback. Resolving the driver does not bypass
rollout admission: a scope must carry the immutable legacy-writer cutover seal
before it can launch fresh stateless work. Boot now owns interrupted-run recovery,
exact Sleeping retry wakes, terminal outbox projection, and parked-run
reconciliation; fixed-roster stages retain one durable logical identity across
retry and exact continuation. Exact placement retries preserve a bounded complete
continuation, bind the immutable source segment and due generation at both
Artifact and runtime admission, and use cross-process queue transactions plus
per-execution activation fencing so rolling processes cannot lose or double-own
a wake. The two production wake dispatchers release their preliminary exact-row
guard before entering Artifact lifecycle admission; that lifecycle reacquires
the same non-reentrant per-execution lock and revalidates the queue row around
the authoritative `Sleeping -> Executing` transition, avoiding a self-deadlock
without weakening ownership. Their checkpoint is scope-bound and HMAC-sealed,
can be hydrated on demand after a cold restart, and is authorized only by the
matching durable retry generation. Index-only startup hydration preserves shared
`Executing` rows instead of normalizing them to `Runnable`. Canonical
interrupted-run recovery classifies the base execution as live, one exact
recoverable segment, terminal-settlement pending, absent, or uncertain. Live,
settlement-pending, and uncertain results defer without changing shared
Runtime/Artifact state; exact recovery carries the immutable segment address
into admission, while absence is authority only for the separately fenced
pre-seed crash window. Both candidates acquire a bounded durable admission token
before async composition and revalidate that token, the Runtime row generation,
and any exact segment/revision under the execution lifecycle exclusion before
the first status/control mutation. Exact Sleeping timers retain their own
queue/body proof and durable lease/pin fence, and startup republishes a missing
queue generation.
During a rolling upgrade from binaries that do not publish reverse bindings,
indexed absence and exact recovery remain fail-closed until an operator drains
every legacy writer and publishes the immutable per-scope cutover. Startup uses
one bounded positive-discovery pass per scope before that seal and the indexed
catalog afterward; discovery never grants recovery authority. A sealed scope
with more than 20,000 active base executions is classified in index-only chunks
so the store's request bound does not starve the tail. An unsealed large scope
does not repeat compatibility walks: it remains fail-closed after its single
bounded discovery. Follow the
legacy-writer cutover runbook
for the required post-drain catalog refresh, irreversible seal, and rollback
boundary. Fresh stateless launches need the same authoritative absence and are
therefore refused before the seal as well; rollout must either keep new traffic
on the explicit `inprocess` arm or seal the scope before serving stateless work.

HTTP acceptance is also restart-safe. Direct message execution, explicit
starts, and direct or planned create-with-initial-message persist a
scope-HMAC-sealed accepted-launch envelope before returning 202. The envelope
binds the exact Runtime generation, Artifact/root/owner identities, mode,
message, and bounded launch options, then moves through a renewable
`Pending -> Claimed -> Consumed` lifecycle. The verified envelope remains exact
composition authority through `Planning`/`PlanningComplete` and the pre-seed or
first-loop `Runnable`/`Executing` crash window. The paginated startup scan
re-reads dormant Runtime rows and verifies their task-backed envelopes as a
prefilter before taking cross-process locks; missing intent is normal dormancy,
not deferred work. Active rows proceed through Artifact's exact intent loader
and typed exact/pre-seed base-authority admission. Planned launches use one
deterministic inbound turn id; unsupported stores refuse without mutation, and
the file store returns the identical turn only when all bound content matches.

The accepted envelope is not consumed merely because its detached future
returned. It stays recoverable until a cross-layer handoff is proved by a typed
waiting/terminal Runtime state or live, exactly recoverable, or
settlement-pending loop authority. A pre-seed/exact admission transfers the
sealed composition into the ordinary typed recovery binding before retirement.
If a crash lands after durable `Consumed` publication but before unlink,
startup performs a bounded pass over at most 20,000 envelopes per scope and
removes only regular, bounded, HMAC-valid, exact-path entries that remain
Consumed after a lifecycle-fenced re-read. Invalid, active, and truncated-tail
entries are retained rather than guessed away. In the same startup sweep,
legacy/taskless interrupted PlanGraph executions converge through their
retained durable plan and typed base-execution recovery authority instead of
being closed merely because they have no Artifact task.

A newer exact `LoopState`
continuation takes precedence over the retained admission body: recovery
recomposes its committed owner before setup and resumes its cursor and absolute
deadline only after the complete live authority identity matches. Durable
steering is bound to the actual HMAC-sealed runtime-control generation. Control
state, pause, steer, and exact resume consult that sealed per-execution epoch
before the serving process's driver default, so mixed `inprocess`/stateless peers
cannot disable or misroute a foreign stateless run. Legacy continue/cancel
routes authorize principal and workspace before any pause is selected or
removed, then revalidate the recovered pause scope. A
cross-process sealed tree-pause transaction makes the immutable member roster
visible atomically and shares a durable per-execution admission exclusion with
pause, resume, delegation, settlement, and recovery. Typed
phase, terminal-control, and manual-pause fences make late input force one more
Decide, erase only explicitly superseded terminal rows, and carry manual input
losslessly through exact resume. A retained manual source checkpoint is hidden
from public pause consumption and remains available to boot-only recovery until
the resumed execution reaches a durable replacement or terminal boundary. Tree
resume is likewise a sealed fixed-roster transaction: preparing records roll
back together, committed records roll forward, and a status-only revision keeps
incidental metadata writes from creating or hiding Paused-state ABA.
Durable cancellation is consumed at every stateless phase and before outward
Apply work. Absolute deadlines and already-exhausted work/token/cost ceilings,
plus a provably lost durable runtime scope, are concluded inside the claimed
driver rather than by an outer resident-only return that leaves the cursor
runnable. Deadline closure always projects an honest, non-resumable failure and
uses runtime-closure steering admission, so it cannot become a cancellation or
manual-pause result or livelock into another unreachable Decide. Apply's final
pre-fire poll covers those boundaries as well as cancellation/manual pause.
Intents that are cancelled, paused, expired, resource-fenced, scope-fenced, or
otherwise skipped before dispatch are positively settled as not dispatched;
recovered parallel results retain their admitted causal order and failure
barriers. Terminal projection independently tracks canonical event receipts,
steering receipts, and Artifact/runtime settlement debt for every
Artifact/runtime terminal or public continuation ending except `HandedOff`.
Ordinary final, `WaitingForUser`,
`WaitingForConfirmation`, `PausedByUser`, and taskless direct runtime endings
carry a scope-HMAC-sealed exact-outcome receipt. A resumable continuation is
persisted before the terminal `LoopState` CAS as a hidden exact generation; its
key, revision, body digest, successor segment, pipeline stage/attempt axis, and
delegated diff-approval disposition are bound into the receipt so retries and
successor adoption cannot cross generations or stages. Approval replay,
applicable resource release, runtime settlement, and Artifact projection finish
before the canonical HITL/approval request is durably published. Resume,
continue, and cancel calls made while that exact generation remains staged
receive a retryable settlement-pending response with no mutation. Exact pause
activation is the final actionable-publication step, after the request is
recoverably visible.
Delegated children retain canonical child-failure policy, while fixed-roster
pauses and terminals first reduce through their outer cursor. `HandedOff`
remains outer-owner segment retirement, and deadline-reconciler `CannotProceed`
retains its receipt-less durable-retirement compatibility path. The projector
uses stable content-derived event keys and a bounded replay-dedupe window, waits
for canonical append receipts, and advances its fenced cursor only after
canonical/steering acknowledgement and cross-layer settlement acceptance. These
steps are idempotent convergence; downstream live transport remains at least
once until it supplies keyed acknowledgements. A process loss during Decide may
repeat provider cost because no durable provider job id/result exists to adopt;
process-local secrets and runtime proofs remain live-host-only and fail closed.
Canonical persistence and runtime settlement receive separate bounded
lease windows. Both lifecycle owners are retained and joined during shutdown.
Scoped cross-process agent and exact-execution lifecycle exclusions serialize
terminal preparation/publication, cancellation, public controls, tree
pause/resume, deletion, owner handoff, and every manual, scheduled, promoted,
delegated, or internally triggered execution admission. Agent-wide discovery
uses durable paginated state and exact pause presence is refreshed beneath the
execution fence. Startup searches terminal debt by canonical runtime identity,
including pipeline and resumed segment names. Autonomous sleeping and
`WaitingChildren` parks remain owned by their durable wake/handoff mechanisms,
not terminal receipts. A stronger cancellation or delegated-child disposition
suppresses and acknowledges a stale immutable HITL batch instead of publishing
it.
Fixed-roster pipeline terminal output is stored in a protected exact-stage
sidecar whose digest, length, media type, segment, stage/attempt, and terminal
sequence are receipt-bound. Sidecar replacement requires the source revision to
remain current with no committed receipt; outer settlement and cancellation
share the task mutation fence. Bounded cumulative progress exposes exact omitted
text/media counts and visible markers rather than silently losing later output.
Interrupted cancellation/startup recovery also reconciles a HMAC-valid sidecar
abandoned before terminal-state CAS while preserving a generation backed by an
independently valid receipt. Peer-created pause, delegation, and placement
authority is refreshed from its scoped durable envelope instead of inferred
absent from a startup cache. An integrity-verified live protected-delegation
claim is a typed deferral: the retry owner leaves Runtime/Artifact projection
unchanged and waits for the bounded claim rather than treating it as corruption
or reconstructing the parent. Exact delegation bodies remain durable until a
successor, outer cursor, or terminal owner commits. Internal source and target
agents are both fenced and revalidated at binding and immediately before
polling, every first-time host-free runtime settlement reacquires its required
lifecycle exclusion, and retry ACK/adoption binds immutable due/source identity.
Stateless lock files also reject post-flock inode replacement.
The filesystem journal uses a bounded, integrity-
checked derived tail index for replay/projection and falls back to the complete
authoritative parser whenever exact prefix coverage cannot be proved. Filesystem
runnable/parked scans reserve truncated parent-level sentinels for the next page,
and wake rows require canonical names plus exact token/resolution/path binding.
Deterministic stateless event-id lookup beyond the recipe log's 8 MiB cap uses a
bounded derived index with chunked authoritative-journal fallback and
best-effort repair; stale index data cannot hide an exact prior event.
Fixed-roster documents are byte-bounded on both
compact write and streaming recovery read, and their absolute stage deadline
encloses validation, interpretation, and the whole exact-resume future.
Delegated children publish their input artifact before the integrity-bound
launch-ready composition, then commit the parent active group/`WaitingChildren`
before scheduling. An interrupted ordinary `Runnable` child is recomposed from
that exact binding instead of being deferred forever, while task-backed child
wakes stay with the Artifact readiness owner until the waiting-parent checkpoint
and handoff are durable. Post-commit and startup reconciliation repair a child
that finished before its parent projection landed. The handoff itself is a
bounded, scope-HMAC-sealed descriptor binding principal, workspace, task,
parent, and a canonical capped child roster; prepare/consume serialize through
a cross-process sidecar, and consume reopens the exact descriptor and receipt
before mutating a loop wake.
Both wake consumers reserve one handler slot plus any immediately available
bounded-page capacity before asking the durable queue to lease rows, and the
claim count cannot exceed those permits. Every returned row therefore reaches
an owned handler without aging behind a local semaphore; task-addressed work
then renews its exact generation and acknowledges only after successful
handoff. The production watcher retains and joins those claimed handlers during
shutdown.
Lifecycle workers do not start until runtime, Artifact, delegation, and
authority/configuration composition is complete. A process already running an
older binary must be rebuilt/deployed and restarted before its behavior changes.
The source/config default alone is therefore not proof of an operational
migration. **Operational cutover has not occurred in this workspace at this
checkpoint:** no live per-scope cutover seal evidence was found. A deployment
is switched only after the new binary is running, every legacy writer for each
principal/workspace is drained, the immutable scope seal is published, and
`/health/execution-driver` reports `stateless` for the serving process.
The completed cutover review and its subsequent adversarial wiring,
crash-window, rolling-recovery, and fresh-admission reviews are preserved in the
archived closure record,
with the living behavior in the
[flat-loop contract](components/magician/execution/FLAT_LOOP.md). Existing
scopes remain fail-closed for generic rolling recovery until legacy writers are
drained and the immutable per-scope seal is published using the
operator runbook.
That pre-seal gate also covers first stateless launch, not only restart recovery.
Verification evidence is recorded in the closure record: `make check-magician`
completed without Rust warnings or errors; `make check-all` passed repository
guards and Android build/tests before exposing downstream fixture drift; and
the corrected `magician-learning` and `magician-api` all-target checks passed.
The aggregate command was not rerun after those fixture-only corrections.

The historical 2026-08-25 integration preserved the split-crate topology while merging the
complete `meetable_bot` runtime. The core `magician` library owns orchestration
and authority seams; Apps, comms, media, learning, surfaces, and logical
chunking remain independently compiled satellite crates. The integrated tree
passes warning-free `make check-all` and the complete non-live `make test`
gate. See [Architecture V2](ARCHITECTURE_V2.md) and the component index for
ownership at that commit; those results are not verification of the current
release, whose evidence is recorded above.

The plan for safe agent-scale concurrency (revision-bound retrieval
reuse, Lance/embedding HOL, provider-isolated LLM dispatch, hard admission,
and M2 Max vs 4-core runtime profiles) is archived at
Runtime concurrency and head-of-line isolation.
Owner live gates remaining on that work are listed in
whattotest.md.

The LLM routing chokepoint closure and the processing-locality policy are
implemented and archived: see the
chokepoint closure plan
and the locality policy design;
the living behavior docs are
[local-channel-llm](components/magician/local-channel-llm.md) and the
[LLM observability contracts](../data/magician_v2/llm_observability/README.md).
The 2026-08-26 development configuration selects the reviewed cloud arms for
all 18 locality-aware generation operations while keeping embeddings local.
The repository seed and live runtime config are byte-identical; an absent privacy section still defaults to local processing.
Streaming Chat and hybrid memory rendering cross definition-erased boxed-future
boundaries before their large state machines can reach an ordinary Tokio
worker stack.

The bounded app-platform implementation program is complete and archived in the
App Platform Completion ledger,
with its original platform design
and tool-authority remediation.
The provider-free P7.7–P7.9 gate passed at `pass^3`; live physical-owner,
assembled-product, scale, and release evidence is maintained in
whattotest.md. Living behavior belongs in the component
documentation. Optional protocol/language, marketplace, attention/task,
advanced Recipe, and advanced UI/event extensions remain deferred breadth, not
pending work from the archived core program.
The program that moved product lanes (Brainstorm, Channel Assist, Monitors,
VibeDev, Tutor/Copilot, and the rest) off the core library and onto Layer 1
seams — under the four-layer responsibility model (kernel, shared
primitives, product apps, admin surfaces) — is implemented and archived:
Phases 0–4 plus the owner-ratified 1.6 custom interactive surfaces landed
with their gates green. The completion record is the
Platform layering and app-extraction plan;
Phase 5 (consolidation — rollback switches, compat shims, deprecated enum
arms) is specified by the
Phase 5 removal inventory,
which succeeds it as the working spec.
Decoupling is the end goal; full app packages are opt-in (harness-SRE and
skill packs are the committed dogfoods).
The matching
[App Platform truth baseline](components/magician/app-platform-truth-baseline.md)
defines the only permitted meanings of kernel-present, production-wired,
user-reachable, focused-verified, activated-path verified, release-qualified,
and complete. It also fixes run/error mapping, payload-minimal observability,
storage ownership, and the permanent canary assignments for parallel work.
The interactive owner contract is documented in
[App interactive capability kernel and browser vertical](components/magician/app-interactive-capabilities.md):
the shared move-only grant/session/observation/effect contract and the exact
Browser, macOS, and Android action rosters share the durable workflow caller.
Live physical-owner qualification is listed in `whattotest.md`.
The first complex-app foundation is documented in
[Bounded app workflow values and recipe IR](components/magician/app-recipe-ir.md).
It defines content-addressed workflow schemas, labeled/provenanced bounded
values, and a closed recipe topology contract. The Ready Query/Get/Map/
Validate/EmitValue/Sequence/Parallel/Switch lowering uses the canonical V3
schedule/reducer owners. The implementation-Ready gate admits only an installed immutable
Recipe runner whose member, schemas, topology, compiled plan, supported node
set and framed implementation digest match at package-lock, launch and
current-lock revalidation. Retry, MarkUncertain, and effectful/reasoning/
deferred nodes remain denied future breadth.
The authenticated native lifecycle behind the typed macOS owner is documented
separately in
[Apps macOS pairing owner](components/magician/app-macos-pairing.md), including
its durable activation, rotation, revocation, crash-recovery, and exact reviewed
launch/focus/snapshot/click/type/key/scroll/drag paths. The fixed-loopback,
owner-code, Keychain-backed Ed25519 pairing, trusted Settings review, staged
opened-file `--no-daemon` CUA execution, and immediate pre/post TCC plus running
application-identity fences are sealed. Live host qualification remains in
`whattotest.md`.

The 2026-08-28 platform layering release coordinates Magician `0.7.0`,
magician-api `0.3.0`, magician-bin `0.2.0`, magician-app-contract `0.2.0`,
magician-apps `0.2.0`, Magican Desktop `0.3.0`, Unified UI `0.1.0`, Magios
`0.2.0` (build 193), Magdroid `0.4.0` (version code 15), and the skill
bundle `0.2.0`. It ships the conversational lane seam with the published
invoke-grammar catalog, the attention relocation and the
comms/monitors/crew/media-ux/VibeDev seams with definition-driven harness
gating, the MUIJ graph family, the attention-lane and app LLM operation
ports (supported-public contract `1.4.0`), the learning review console with
its host-read binder and the thinking-map binder, and the custom interactive
surfaces capability — manifest, kernel, web/iOS/Android hosts, and the
brainstorm-canvas reference consumer. See the
platform layering plan,
[custom surfaces v1](components/magician/custom-surfaces-v1.md), and the
[app-platform threat model](components/magician/app-platform-threat-model.md).

Magdroid `0.4.1` (version code 16) restores the tracked, bounded App Pilot
command history and makes standard Base64 enrollment-challenge decoding work
identically on Android and the local JVM without weakening the exact 32-byte
gate. Magician API `0.3.2` advances the Android Apps attestation pin to that
packaged build (`ai.magicbeans.magican`); the former code-15 identity fails
closed and must re-enroll after upgrading so its persisted identity and policy
digest bind the reviewed version.

Magician `0.6.1283` and Unified UI `0.0.810` turn autonomous Town Square
activity into bounded visible conversations. Agents reload the committed public
thread before admission, choose a structured pass/reply/new-topic action, and
publish only after a transactional active-root and reply-cap recheck. The UI
keeps direct, nested, and paginated orphan replies in activity-ordered thread
trees. See [Fleet social network](components/magician/fleet-social-network.md).

Magician `0.6.1282` (API `0.2.6`, app-contract `0.1.4`) pins Magican Desktop's
signed identifier and Magdroid's installed package `ai.magicbeans.magican`
(`versionCode` 14) for Android Apps owner review.

Magican Desktop `0.2.78`, Magios `0.1.195` (build 192), and Magdroid `0.3.9`
(version code 14) present as Magican: new bundle/application IDs
(`ai.magicbeans.magican.desktop`, `com.magicbeans100x.magican`,
`ai.magicbeans.magican`), Caveat M icons, and `magican://` deep links.
`magican://` links open Magican on desktop, iOS, and Android; no alternate
custom scheme is registered.

Unified UI `0.0.809` is the Magican landing with HowTrack: five stations,
product screens that cycle inside one chrome window, carousel icons, Today
CTAs, the real chat composer, and a slow float. See
[root landing](components/unified-ui/root-landing-native-route.md).

Magician `0.6.1273` and Unified UI `0.0.805` redesign the landing hero and BrandReveal lockup.\
Magician `0.6.1273` and Unified UI `0.0.804` add Claude Code and
Antigravity (`agy`) as named VibeDev coding-engine options next to Pi,
Codex, and Grok. The picker rows are **Claude** (`claude-default`) and
**Antigravity** (`agy-default`). Each is selectable only when its kill
switch is on, exactly one reviewed binary is found, the frozen CLI
minimum is met, subscription/OAuth is signed in, and a fail-closed
isolation receipt matches the current identity plus version. Magician's
`ANTHROPIC_API_KEY` / `GEMINI_API_KEY` are not inherited unless
`coding.claude.use_api_key` / `coding.agy.use_api_key` are true.
Antigravity identity includes path + mtime + length so an in-place CLI
replace cannot inherit a Ready receipt; catalog `search_web` is not
Ready-incompatible, but runtime use fails closed; resume uses
`--conversation` (never `--continue`); token usage never invents `$0.00`
(`cost_known: false`). HTTP refresh returns 202 checking and never probes
the CLI. Live cockpit gates remain in whattotest.md.
See the [Claude Code coding contract](components/magician/claude-coding-engine-contract.md)
and the [Antigravity coding contract](components/magician/agy-coding-engine-contract.md).
The Magician plane — a governed MCP door for a foreign harness — is
[plane.md](components/magician/plane.md). Its
[typed input contract](components/magician/plane.md#typed-input) covers
originating-session text, choices, forms, approvals and delegated-run input.
The completed implementation ledger
is archived with 16 passing focused tests. See
[focused elicitation testing](testing.md#plane-typed-elicitation) for the test
command and validation scope.

The 2026-08-25 governed Apps and client-maintenance checkpoint coordinates Magician `0.6.1279`,
magician-api `0.2.3`, magician-bin `0.1.3`, magician-app-contract `0.1.2`,
Magician Desktop `0.2.76`, Unified UI `0.0.807`, Magios `0.1.193` (build
190), Magdroid `0.3.8` (version code 13), private `@magician/apps`
`0.1.0-dev.2`, and Research Planner `0.1.1` with its `0.2.0` update package.
The supported-public wire
contract was independently versioned at `1.3.0` at this checkpoint (it has
since advanced additively to `1.4.0`): its exact eight-operation
artifacts and client mirrors are regenerated and provider-free qualified at
`pass^3`. Physical-owner and assembled release canaries remain in
`whattotest.md`. See the [generated contract evidence](contracts/app-platform/v1/README.md),
[TypeScript SDK](components/magician/typescript-apps-sdk.md), and
archived completion ledger.

The same release candidate completed the canonical non-live repository gate:
14,385/14,385 Rust tests plus doctests, the full frontend and desktop suites,
macOS audio, deterministic evaluation harnesses, the 239-test composed runner,
and iOS/Xcode all passed. The remaining C01–C16 assembled-product and physical-
owner evidence is intentionally tracked in whattotest.md,
not conflated with this provider-free qualification.

The contextual desktop release (magician-api `0.2.0`, magician-desktop
`0.2.74`, Unified UI `0.0.802`, Magicutor `0.1.270`) completes the
multi-context plan: the left-Option hotkey works anywhere (screen-context
fallback; Finder selections; a page-level "Ask Magician about this page"
browser item), writing actions close the loop in place (Tab inserts with
focus-verified paste, R refines, Esc dismisses), the HUD accepts dropped
files and one-tap screen-context attachment, screen captures carry
app/window provenance, and the backend owns the contextual contract —
`GET /contextual-writing/catalog`, closed `state`/`targetTextKind`
vocabularies, `frameUrl`, and honest `createsTask` (real V3 tasks,
`taskId` in the response). See
[contextual assist](components/desktop/contextual-assist.md),
[HUD](components/desktop/hud-overlay-window.md), and
[the contextual-writing contract](components/magician/contextual-writing.md).
The planned fast lane for one-line asks is deferred with its design stub
in the contract doc. This release is not yet compiled or tested — it
lands with the agreed fix-all pass.

Magician `0.6.1235` adds the app-tool bind+contain kernel:
classification is pack/skill schema, not a `list_`/`_search`/`android_`
name family. Wired in-process classes (pure transform, clock, Bound
HTTP GET, files, structured DuckDB) mint and run. Exact reviewed
authority-free ToolSkill pure transforms can also run through the sealed OS
jail; host-read, ambient side-effect/device authority, networked jail actions,
and every raw jail/path/argv/environment surface remain refused. See
[app-tool-bind](components/magician/app-tool-bind.md).

Magician `0.6.1234` adds owner approve/enable for
`ready_for_review` apps: Apps directory Review, `magician app
approve`, and `GET/POST /apps/installations/{id}/review|approve` on
the existing `commit_reviewed_installation` kernel.

Magician `0.6.1233` lists app-authoring skills only from the current
scope `skills/` folder, not repo `skillshub/`.

Magician `0.6.1232` drops `skill_type: facade`; those skills are
`tool`.

Magician `0.6.1231` lets apps lock the same tools agents use
(`http`, `files`, `macos_automation`). Grant of the
tool is the control; attestation is internal.

Magician `0.6.1230` adds the app authoring catalog: `magician app
tools|agents|personalities|procedure list` and VibeDev App options
share `GET /api/magician/v2/apps/authoring/*`.

Magician `0.6.1227` adds the Phase 7 killable custom-surface worker:
scripted `surfaces/*.js` runs in a Magician-spawned OS child with only
admitted `surfaces/` bytes, no network, and CPU/RSS/wall kill. The
display iframe stays no-script. Web/desktop host qualification is
that no-script Unified UI host plus the worker. `Iframe.svelte` is
unchanged.

Magician `0.6.1225` records the Phase 8 discovery/lock/lifecycle kernel
checkpoint:
enabled app capabilities appear in the existing scoped USR catalog only
when the locked document is USR-executable and disappear on disable/revoke by
evicting the overlay on the lifecycle write. The release's same-name
replacement behavior was subsequently removed: current collision handling
skips the app pack and preserves the platform primitive. Chat and
personal-agent dispatch
require live overlay membership. The overlay admits at most 100
installations and 100 tool names. Ranking cannot change that set.
Invented names stay absent. App-workflow dispatch still requires the
revision fence. Besides the reviewed in-process compiled-provider lane, only
exact locked authority-free ToolSkill actions and sealed typed
browser/macOS/Android snapshot owners can become dispatchable. Raw USR/CLI,
general MCP, networked jail, coordinate/device, and unprojected physical-owner
surfaces remain refused; deployment-specific owner prerequisites may still make
a Ready implementation unavailable.

Magician `0.6.1224` continues Phase 8: app-workflow USR pack dispatch
must present the exact locked capability fence. Invented names fail
closed. There is no second executor.

Magician `0.6.1223` starts app-platform Phase 8: deferred computed
capabilities come only from the exact package lock and current grant.
Disable, quarantine, revoke and uninstall hide them. An invented tool
name fails closed.

Magician `0.6.1222` continues Phase 7: the live custom-surface host
serves the no-script envelope, inlines admitted CSS, executes admitted
bridge query/mutate/invoke on the owner data plane, and tears sessions
down from the projection worker. Unified UI renders that envelope in
`AppCustomSurfaceHost.svelte` with an empty sandbox. `Iframe.svelte`
is unchanged. Browser red-team remains later.

Magician `0.6.1221` continues Phase 7: the no-script host envelope
binds to a staged package candidate and loads only live `surfaces/`
members.

Magician `0.6.1220` continues Phase 7: a resolved HTML document
becomes a no-script `srcdoc` envelope. It cannot emit the general
Iframe sandbox. `Iframe.svelte` is unchanged.

Magician `0.6.1219` continues Phase 7: custom surfaces load only
`surfaces/` bytes from the exact live package revision. Path escape,
tamper and disabled installations fail closed.

Magician `0.6.1218` starts app-platform Phase 7: custom-surface
isolation and bridge admission. App documents cannot reuse the general
Iframe sandbox. The first release is declarative/no-script. The live
host remains later.

Magician `0.6.1217` closes app-platform Phase 6: declarative
schema migrations dry-run before any active pointer changes,
authority expansion re-enters review, and data-rewind rollback is
explicit. Combined archives stay encrypted by default.

Magician `0.6.1216` records the first production-wired Phase 5C checkpoint:
owner-facing
`app_data_query`, `app_data_search` and `app_data_compose` tools query
enabled installations through the canonical data plane. Later corrective
slices add bounded discovery, direct action invocation, and real destination
launch with same-key recovery. Incompatible fields are refused; delegated and
outward audiences fail closed. Phase 5C remains `in_progress` until the live
two-app, restart, stale-authority, cancellation, policy, and converged
regression gates pass.

Memory tiering has its own provider-free gate, separate from the retrieval
evals: `make test-memory-tier-health-eval` reads a scope's temperature overlay
read-only and reports whether the tier a memory landed in was *earned* —
unearned active ratio, working-set ratio, tier lift, and key hygiene. See
`docs/components/magician/memory-evals.md`.

Magician `0.6.1215` closes app-platform Phase 5B: source-linked memory
candidates persist in registry schema v13, record mutate and
disable/retain/quarantine/forget settle them in the same transaction,
and prompt plus search re-resolve live store evidence before an
enveloped memory can re-enter. Temperature remains a derived overlay
and cannot be package-assigned. Cargo tests and checks were not run
for the whole crate.

Magician `0.6.1214` closes app-platform Phase 5A's app/procedure
authoring path: a verified VibeDev build writes only a task-bound
handoff claim, Magician selects the artifact, apps enter
`ready_for_review`, and standalone procedures enter immutable scoped
storage. Chat, voice and cockpit share that build signal; observe mode
cannot publish; executable-capability selections publish inert scoped
revisions and still cannot activate or enter a Phase-8 package lock.

Magician `0.6.1213` and MagicLLM `0.2.26` complete app-platform Phase 4:
reviewed app actions run as idempotent V3 workflows, one canonical resource
authority meters every physical boundary, labeled disclosure remains bound to
an attested provider partition, and private/standalone procedures resolve only
through immutable package locks. Focused closure passes 961 tests and the
warning-free Magician all-target check. See the
app-platform design,
[contract kernel](components/magician/app-platform-contract-kernel.md), and
[threat model](components/magician/app-platform-threat-model.md).

Magician `0.6.1212` and Unified UI `0.0.797` type user-knowledge
preferences (trust/kind/scope), page confirmable `/memory` entries, and
let Today/Square join Worth-a-look cards for why-now and contextual
actions. Shadow attachment can explain; it does not hide or rerank.
See [Resurfacing](components/magician/resurfacing.md) and
[Memory native route](components/unified-ui/memory-native-route.md).

Magician `0.6.1211`, Unified UI `0.0.796`, and Magios `0.1.190` complete
app-platform Phase 3: strict default surfaces hydrate from authenticated indexed
entities; durable entity/lifecycle outboxes drive identifier-only realtime
hints; `/apps`, top-bar and command-palette discovery are server paginated; and
web/iOS Today show at most eight explicitly pinned enabled views. Browser and
iOS clients recover through bounded canonical REST reads, while the iOS
exact-origin proxy rejects redirects, cookies, hostile paths and
cross-installation access. See [App default surfaces](components/magician/app-default-surfaces.md)
and the app-platform design.

Magician `0.6.1209` and Unified UI `0.0.795` add the scoped
[Fleet Social Network](components/magician/fleet-social-network.md): every
configured principal/workspace has an independent Town Square roster, durable
feed, mention inbox, budget and retention boundary. Exact `@agent-id` mentions
wake a bounded background worker and parent-bound replies acknowledge the
delivery atomically; ordinary reply notifications cannot create response
loops. The web surface uses the scoped v2 API with server pagination,
single-flight polling and explicit unavailable/paused/degraded states.
`make check-all` runs the workspace compile **and** `cargo check -p magician
--lib`. The second is not redundant: a `--workspace` build unifies
`magician/test-fixtures` on (magician-api and magician-comms request it in their
dev-dependencies), which compiles every fixture into magician's plain library
target where nothing calls it. The crate root suppresses `dead_code` in exactly
that configuration, so the default feature-off build is the only place real dead
code in magician's production source appears — see
`docs/components/scripts/source-scanning-guards.md`.

`make check-all` runs `make check-service-name-boundary` as a prerequisite, a deterministic
guard that keeps the backend compatibility name out of assistant, agent,
product, and app presentation while retaining explicit technical identifiers
such as API routes, config keys, storage paths, and backend-service diagnostics.
The guard covers active prompts, compiled fallbacks, agent definitions, skills,
tool schemas, and native/web presentation surfaces; its implementation and the
idempotent scoped-definition migration are documented in the
[scripts reference](components/scripts/README.md#service-name-boundary).
Product-facing names have a separate positive source of truth:
`data/presentation_identity.json` generates the Rust, web, iOS, Android, and
desktop constants used by migrated presentation surfaces. `make
presentation-identity-check` runs before `check-all`; see
[presentation identity](components/magician/presentation-identity.md).
The app-platform contract keeps its wire and policy language server-owned.
The current generator source derives JSON Schema, a supported-public OpenAPI
3.1 document, a canonical operation inventory, and shared web/Swift goldens
from the pure `magician-app-contract` crate plus the Rust data-plane types.
Route registration consumes the same operation metadata. Under the active
no-generator instruction, however, the checked-in contract/OpenAPI/schema/
fixture/client mirrors still describe the earlier component-only snapshot and
must not be published until `make app-contract-codegen` and
`make app-contract-check` pass. (Both targets were repaired on 2026-08-26 to invoke
`cargo run -p magician-apps --bin app_contract_codegen` — the pre-split
`-p magician --bin app-contract-codegen` invocation had been broken since the
magician-apps crate extraction.) Strict procedure-loader
separation plus authenticated request, dispatch, scoped-store and reviewed-
approval fences now sit beside typed query/relation/cursor/idempotency/value-
mapping semantics, while the policy kernel joins labels, attests endpoint
locality, fences hidden consumers and partitions continuations. Phase 1D now
enables only authenticated app-package control routes, not app execution.
Phase 1E adds a pre-runtime provider-free `magician app` loop that derives its
JSON/TypeScript and fixtures from the same strict manifest, checks one admitted
snapshot, and emits package-only reproducibility evidence without granting
authority.
Phase 3A adds a pure deterministic default-surface compiler over that admitted
manifest: List/Table/Tree/Timeline produce empty validated MUIJ skeletons and
the matching complete binding generation. Registry-correlated package/schema
evidence is required, schema v7 atomically stores every generation member,
Tree input is iteratively bounded, and EntityGrid preserves canonical server
pages. Component/view identity is stable across retries, Board fails
explicitly, and no record body, cursor, scope or grant enters the render
document. Phase 3B validates and resolves the complete active generation
under authenticated scope, rejects overlapping and concealed routes, converts
explicitly typed route parameters into the existing flat query predicate,
derives the Phase-2 indexed query from compiler-owned metadata, returns its
canonical page unchanged and detects route, byte or revision drift. Persisted
blob-size gates run inside SQLite before materialization. Phase 3C now exposes
responsive browser/desktop app routes, hydrates canonical pages into existing
MUIJ, keeps app interactions off the agent socket, uses shared opaque-cursor
pagination, and submits bounded optimistic CRUD through a server-derived
surface authority fence. Timeline remains read-only. Phase 3D consumes durable
entity/lifecycle outboxes through one projection worker, emits identifier-only
deltas, refetches on gaps/resets and reconciles app-owned published navigation
without fabricated task owners. Phase 3E adds the metadata-only paginated Apps
directory, web/desktop launchers, explicit Today pins and an enrolled-origin,
exact-scope iOS launcher with a bounded ephemeral WebView proxy. See
[app default surfaces](components/magician/app-default-surfaces.md). Phase 3's
post-review hardening now also gives ambiguous mutations an exact same-ID retry,
uses durable visible-page polling when realtime is unavailable, rejects partial
Trees instead of inventing roots, batches directory metadata reads, keeps Today
to eight pinned views globally, and prevents task/startup maintenance from
overwriting concurrent shared publications. The iOS launcher accumulates
bounded response chunks rather than iterating individual bytes.
Phase 4A now binds declared app actions to the existing V3 task/execution
runtime. One process-shared workflow owner reconstructs immutable package/
procedure material, intersects current authority, admits protected model/tool
boundaries and injects a typed terminal commit. Deterministic task and mutation
identities make direct, explicit child/delegated, retry, resume, repair and
server-owned schedule/event launch paths idempotent; a committed task result
short-circuits before any replacement model, tool or resource work. Protected
pause continuations use monotonic registry generations, scoped exact lookup,
heartbeated claims and crash-owned resource reconciliation. Superseded full
control snapshots are pruned while the authoritative head and one prepared
pause remain protected. Focused Phase-4 verification is recorded slice by
slice in the app-platform plan as each boundary stabilizes.
Phase 4D now closes the immutable procedure-dependency slice: package-private
workflows and exact vendored/registry procedure revisions are package-lock
bound, every explicit procedure tool ceiling narrows the complete workflow,
and app manifests remain absent from the global procedure catalog. The focused
lock, registry, workflow-cache and 100-app isolation suites pass. Publishing a
new standalone registry procedure now enters only through Phase 5A's bounded,
authenticated writer; vendored procedures remain available end to end.
Phase 5A's app/procedure authoring path is now closed in Magician
`0.6.1214` with one producer-neutral reviewed-candidate boundary.
External SDK/CLI authors can submit a packed archive to
`POST /api/magician/v2/apps/packages/candidates`; the server independently
rechecks generated artifacts, provider-free fixtures and the trusted local
dependency lock before publishing only an inert `ready_for_review`
installation. Exact replay is idempotent and no route grants activation
authority. The server-owned VibeDev handoff now accepts only a canonical
`Verified` gate whose active sealed attestation is green for the same scope,
project, root task/execution, candidate, generation and whole-repository
snapshot. Every admitted app member is matched back to that snapshot before
the exact same review publisher runs; no request body can assert VibeDev
provenance. A green VibeDev build now consumes a task-bound, repository-owned
handoff claim, derives the least-powerful artifact kind with the same selector
as `magician app select`, and routes apps, standalone procedures and
executable capabilities into their inert publication boundaries. The model
never supplies the artifact kind, verification state or activation
authority. External authors can validate `magician app procedure check` or
`magician app capability check` (a new tool `SKILL.md` must include a
USR contract and typed actions; apps declare existing tools by real
name and the engine snapshots eligible catalog skills or typed compiled pack YAML) and send exact `SKILL.md` bytes to
`POST /api/magician/v2/apps/procedure-revisions` or
`POST /api/magician/v2/apps/capability-revisions`.
Magician derives identity, atomically allocates the scoped revision, stores the
bytes write-once and publishes neither global discovery nor activation
authority. Portable app archive v2 carries the complete lock claim; candidate
publication reloads each named procedure from the authenticated registry and
rebuilds the lock before review. Chat, hands-free voice and cockpit builds all
reach the same `VibeDevRunService` build signal consumed after verification;
the stored-task convergence and app/procedure publication canaries pin that
path. The executable-capability branch now uses the same inert
write-once publisher as procedures; mutable names cannot satisfy a
later lock.
The package kernel additionally admits only strict bounded manifests
and complete safe member inventories, canonically digests every bundle byte,
and refuses mutable-name dependency locks. Phase 1A now adds a lazy per-scope
SQLite registry and an atomic, idempotent publication of the immutable package
revision, conformance attempt and inert `ready_for_review` installation. It is
scope-bound, symlink-rejecting and blocking-lane-only. Phase 1B adds the
authenticated descriptor-pinned filesystem reader and immutable
content-addressed package-byte owner: bounded source snapshots are written to a
private sibling, flushed and promoted with a create-only atomic rename; exact
replay verifies final bytes, and stale partial staging has a bounded per-scope
recovery path. Phase 1C consumes that move-only staged proof at conformance,
revalidates the immutable bytes, and atomically persists reviewed
grant/schema/surface revisions, single-use approval consumption, installation
generation and a metadata-only projection event. Ordinary lifecycle and grant
revocation use the same generation CAS; outbox leases and recovery are finite,
explicit and scope-local. Phase 1D consumes middleware-issued request identity
for exact scoped installation/attempt reads and bounded package-only transfer.
Imports rebuild the strict candidate and stop at immutable staging with fresh
local conformance and permission review still required; exports stream only the
portable package manifest and exact bundle bytes.
The source-linked memory contract also prevents an accepted app-derived claim
from outliving its exact current record/policy state: a non-deserializable
eligibility fence is required for retrieval. The memory destination, signed
owner accept/reject/revoke path and source-linked prompt projection are live; the
separate personal-assistant retrieval projection independently reopens current
authority before and after its private snapshot. Bounded Apps/desktop history
now shows current and compacted source state, while physical qualification
remains pending.
The same dormant boundary now separates package sharing from personal-data
movement: data/combined migrations default to authenticated encryption,
unsigned imports receive local-fork identity, foreign grants never transfer,
data-import previews allocate local IDs, and purge receipts enumerate every
local/shared/provider store without overstating deletion. The whole-installation
owner now persists an exact preview, waits for memory/retrieval and lifecycle
acknowledgements, deletes installation-owned registry authority/data rows, and
commits the receipt plus terminal lifecycle CAS atomically. Archive crypto and
the remaining non-registry Storage Governance adapters remain later
load-bearing work.
The canonical resource contract keeps root, child, retry, resume,
repair, tool/browser, synthesis and reflection spend under one fenced tree and
installation-period identity. It characterizes fragmented existing meters as
observation sources—not budget authorities—uses checked additive accounting
plus parallel active-time union, and retains uncertain external reservations
until trusted reconciliation. Current registry schema v12 and the Phase-4B
owner provide durable CAS, and the Phase-4A workflow/executor path now adopts the
move-only root/operation/settlement/accepted-cleanup seams at physical I/O.
Phase 0 reserves the canonical per-scope `apps/` topology and exposes its
SQLite/WAL, package bytes, attachments, exports, captures and
evaluation artifacts through read-only Storage Governance inventory. Missing
paths stay missing, existing symlinked path components are rejected, child
links are skipped, and inventory itself activates no store, route or cleanup
owner. Phase 1A is now the sole owner allowed to create `app_store.sqlite3`,
and does so only on an authenticated registry write. Phase 1B is the sole owner
allowed to create immutable `apps/packages/<bundle-digest>/` bytes, and does so
only after authenticated descriptor-pinned admission and create-only atomic
promotion. Phase 1C is the sole owner of reviewed lifecycle revision rows and
the registry outbox. Phase 1D remains the package-transfer HTTP owner. Phase 4A
adds only the authenticated app-action invoke/result route over reviewed
installations and the existing V3 execution runtime; schedule/event launch
authority is server-owned and cannot be minted through HTTP.

See the [contract kernel](components/magician/app-platform-contract-kernel.md),
[app authoring CLI](components/magician/app-authoring-cli.md),
[TypeScript Apps SDK](components/magician/typescript-apps-sdk.md),
[threat model](components/magician/app-platform-threat-model.md),
[app events and owner notifications threat model](components/magician/app-events-owner-notifications-threat-model.md), and the
[generated contract catalog](contracts/app-platform/v1/README.md).
For private-scale experimentation, also use the
App Platform private-scale readiness runbook
and `make app-platform-private-check` before candidate churn.
It also runs `make check-rank-recompute-semantics`, which keeps the
rank-recompute result-semantics vocabulary identical across the backend
constants, the frozen contract fixture, and the web parser — a mismatch there
fails silently, with the page rendering while every attributed result is
dropped.
`make setup-magdroid-build` prepares an Android toolchain, and `make
build-magdroid` / `make test-magdroid` build and test the companion. On macOS,
the root `make setup-all` now invokes that idempotent bootstrap; non-macOS hosts
skip it because the installer is Homebrew-specific. `make setup-all` also runs
`make setup-meet-bot`: BlackHole 16ch on macOS (reboot once after a fresh
install) and Pulse tools plus Xvfb on Linux. Linux join uses those Pulse
devices; it does not use BlackHole. A plain
checkout cannot build it for three reasons that each fail without naming
themselves: the workspace JDK is newer than Gradle 8.13 accepts, there is no
Android SDK, and upstream never committed `gradle-wrapper.jar`. A fourth is
pinned around rather than fixed — AGP 8.x uses a Gradle internal API removed in
9.6, so the build uses a `gradle@8` keg instead of whatever `gradle` resolves to.
`make check-magdroid` runs the build and the tests as one gate, and
is part of `make check-all`. The composed `make test` runner also executes
`test-magdroid`, reports it as the Android (Magdroid) suite, and continues to
the Magios lane afterward. Both aggregate paths report a skip rather than a
false failure when the JDK or Android SDK is unavailable. The bootstrap may
install roughly 3 GB of Android SDK data plus a Gradle cache, so operators can
still run `make setup-magdroid-build` explicitly before a focused Android lane.
`install-magdroid` / `run-magdroid` / `logs-magdroid` / `screenshot-magdroid`
drive an attached handset — each builds first, because installing a stale APK
and then hunting for a change that was never compiled is the failure this
replaces. Android and iOS receive their customer endpoint and credentials from
the same one-time QR enrollment; no build-time mobile secret scaffold is
required.

`make check-device-bridge-protocol` joins `check-all`, guarding the Android
device bridge's bounded MCP contract across the Kotlin server and governed Rust
client. It pins the discover revision, methods, annotations, request-bound roster
subscription, duplex transport, and permanent removal of the private envelope
and listener.

The 2026-08-10 `magdroid` import has grown into both Android halves of Magican: the
device-automation engine vendored from NeuralBridge_mcp (Apache 2.0), and the
native Magician client. The engine exposes screen structure, gestures, input,
and bounded screenshots through MCP on the authenticated outbound device bridge;
the legacy listener and local API-key surface are deleted. The native client now owns Chat, Today, Tasks,
Monitors, Settings, App Pilot, the Magican keyboard, Audio Notes, realtime and
hands-free voice, and dynamically addressed wake listening. Published
Briefings render bounded GAUI/MUIJ documents as native Compose UI.
Android setup is now owner-driven scan-to-pair: Settings renders a local,
five-minute, one-use QR; App Pilot validates and confirms the Magician host,
stores the scoped device credential with Android Keystore, and reconnects the
outbound MCP bridge automatically. The QR contains no durable token, paired
devices remain visible and revocable in Settings, and manual connection inputs
are recovery-only.
`make build-magdroid` and `make test-magdroid` skip with a clear message when no
Android SDK is present, the same way the iOS targets skip without xcodebuild. See
the plan and
[magdroid/README.md](../magdroid/README.md).

The 2026-08-14 mobile at-a-glance release advances Magician to `0.6.1202`,
Magios to `0.1.186` (build 184), and Magdroid to `0.3.4` (version code 9), and
consolidates each platform around one
Home Screen widget: Needs You first, then active work, then Talk. The same
paired-device boundary now accepts private APNs/FCM application routes for
generic Attention alerts plus temporary task-bound progress routes. iOS task
Live Activities can update while the app sleeps; Android refreshes its widget
and the selected ongoing task notification without subscribing to every run.
Missing deployment provider credentials falls back to canonical Today polling,
and neither observation nor ambient microphone activity can be remotely started
or extended. See [device pairing](components/magician/device-bridge.md), the
[iOS companion](../magios/README.md), and the
[Android companion](../magdroid/README.md).

The 2026-08-14 reliability release advances Magician to `0.6.1201` and
MagicLLM to `0.2.25`. A full startup-log audit removed stale-state amplification:
memory archive checkpoints are retention-safe, logical chunk operations have a
separate end-to-end deadline, attention maintenance is wave-bounded, thinking
map writes share one normalized-root lock, cancelled executions cannot respawn
synthesis, auxiliary summaries have their own background capacity, and missing
Kapso executables are surfaced without repeated failed spawns. See
[logical chunking](components/magician/ollama-logical-context-chunking.md),
[agent memory](components/magician/agents/agent-definition-reference.md),
[attention routing](components/magician/attention-routing-funnel.md), and
[Live Thinking Map](components/magician/live-thinking-map.md).

The 2026-08-14 mobile enrollment release advanced Magician to `0.6.1200`,
Unified UI to `0.0.787`, Magios to `0.1.183` (build 181), and Magdroid to
`0.3.2` (version code 7). Generic mobile builds no longer contain a customer
hostname or shared Cloudflare credential. An authenticated owner creates a
platform-specific, short-lived QR from the deployment-owned public origin;
the native client confirms and verifies that origin before securely storing a
revocable profile whose scope and capabilities come from the server roster.
See [device pairing](components/magician/device-bridge.md), the
[iOS companion](../magios/README.md), and the
[Android companion](../magdroid/README.md).

The 2026-08-13 development release advances Magician to `0.6.1194` and Unified
UI to `0.0.786`. Observe now has one scoped, deterministic startup catch-up
policy across message ingest, local understanding, Published Notes, public
feeds, and calendar: owner-selected age/count/admission bounds replace the
mail-only selector and hidden backfill override, while the live Observe panel
reports this boot's progress and each source's honest replay limitations. See
[Observe controls](components/unified-ui/observe-channel-toggles.md#startup-catch-up)
and [content sources](components/magician/content-sources.md#uniform-restart-catch-up).

The 2026-08-09 drop-in skill update advances Magician to `0.6.1169`, Skillshub
to `0.1.22`, and tool-runtime-core to `0.1.68`. Product-specific content and
Observe declarations now share the owning bounded `SKILL.md`; installed actions
self-enroll into their declared retrieval rung without central action IDs, and
AnyDoc is exposed by the standalone `document-to-markdown-cli 0.1.0` executable.
The 2026-08-08 development release aligns Magician `0.6.1155`, MagicLLM
`0.2.24`, Unified UI `0.0.780`, Magician Desktop `0.2.72`, Magios `0.1.177`,
Skillshub `0.1.18`, and tool-runtime-core `0.1.64`. The universal skill runtime
carries scoped
credential material through a sealed value/filesystem boundary and permits dynamic
output to cross into product persistence only as a bounded, value/path-redacted batch.
Its metadata-only receipt is written to the exact scope-owned audit journal before the
matching analytics event; concurrent writes remain complete JSON lines and native
audit failures expose only `audit_failed`. Google Workspace is its first enabled
production family: all six packages and 98 actions now use the governed coordinator;
their interactive, autonomous, and background content routes resolve the same scoped
durable audit authority. Missing authority fails before provider work, while an audit
write failure prevents action dispatch or result exposure.
Phase 7B is complete for twelve profile-free static/multi-secret packages: Tavily,
Exa, OpenAI/Claude web search and deep research, KLIPY GIF search, Imgflip,
anonymous-or-token GitHub search, Nano Banana image generation, Veo video generation,
and native Metabase. Their declared required, optional, paired, or alternative secrets
cross the same authorization-before-auth boundary, with canonical
scoped vault precedence and a private per-skill `.env` bridge during storage
migration. Their public typed contract is now the sole input schema: the governed
runtime applies defaults and emits bounded canonical JSON stdin, while the Python
executables retain provider protocol/normalization only and contain no duplicate CLI
flag parser. Google Workspace calls native `gws` and Metabase calls native
`metabase-pp-cli` directly without skill wrappers. GitHub omits a missing optional
token; Veo requires either of its two declared keys and omits the absent alternative.
Slice 7C completed the CLI-owned-session family. Higgsfield remains the
batch CLI-owned example. The `skillshub/{claude,codex,agy,opencode}`
coding-delegate skills and their PTY controller were later retired;
agents code through `run_coding_task`, and operators launch those CLIs
from Developer Mode.
`tool-runtime-core 0.1.44` includes the complete dormant Phase 3 qualification and hardening gate: bounded
batch redaction now covers credentials split across adjacent persistence records, and
test-only canary, malformed-output, cancellation, crash-recovery, concurrency, and
small-stack qualification keeps values and paths out of every public escape surface.
Exact profile key/revision readiness now gates path use, registry reads/locks detect
growth and replacement, and zeroizing redaction indexes enforce explicit work budgets.
Remote MCP OAuth is bound to its exact canonical resource endpoint, persistence receipts
are derived from the exact materialized plan, and audit attribution retains selected
revision plus complete selected/implicit binding metadata. Ambiguous post-rename
registry durability reports `commit_state_unknown` and requires read-before-retry
reconciliation; dead scratch-authority cache entries are pruned incrementally.
The exact matcher deliberately does not claim transformed-output detection.
Phase 4A–4E now add exact, registry-bound CLI lifecycle command authority, bounded
declarative status interpretation, a process-local verified-status cache, and single-owner
interactive login coordination plus a dormant governed process owner. Fixed
profiles cannot be replaced; selected profile identity and revision remain exact;
lifecycle argv and provider output stay outside model-facing and debug/serialization
surfaces. Exact or explicitly ASCII-case-insensitive identity checking gates readiness,
and login/refresh success always requires a fresh status observation. Cache entries are
bounded by exact plan, process, policy, profile, directory, and expiry identity; stale
in-flight observations cannot restore invalidated readiness. Login leases are exact-target,
revision-bound, cancellable, and expose safe browser-callback/device-code/OTP/QR/operator
pending classes without storing their payloads. Stale cleanup remains single-owner and
holds the target until resources are confirmed terminal. A cache hit is advisory and does
not authorize credential use. Lifecycle execution resolves an exact executable from a
clean bounded `PATH`, revalidates missing/expired/ready profile directories without
weakening ordinary ready-only execution, clears the environment, invokes no shell, and
owns bounded batch/PTY children as isolated process groups. Provider interaction values
remain borrow-only/zeroizing and only typed pending state reaches the coordinator. The
executor and fake-CLI matrix remain dormant; all six focused process tests and the
Magician all-target compile gate pass, and no production route is enabled.
Phase 5B now adds one transport-free adapter contract for MCP, browser-profile,
native-permission, and delegated-credential strategies. It reuses the canonical auth and
approval vocabulary, exposes only payload-free status/pending/invalidation metadata,
keeps provider results opaque, and distinguishes safe pre-dispatch retry from confirmed
or ambiguous post-dispatch delivery. MCP protocol ownership composes with delegated or
native authentication, while optional/conditional authentication is decided from an
explicit invocation demand. OAuth, browser/native execution, delegated grants,
catalog publication, and every production migration remain disabled.
Phase 5C is now underway in `magician-mcp-client 0.1.5`. Its first slices connect the
official SDK's credential/state store traits to an opaque product vault contract while
keeping OAuth protocol mechanics SDK-owned. Domain-separated record keys bind the exact
scope, provider, profile, resource, and issuer; bounded versioned secret documents remain
non-cloneable and zeroizing; and PKCE callback state is create-only, ten-minute bounded,
and atomically consumed so concurrent callback replay has one winner. Magician now
implements the dormant concrete adapter over a separate scope-owned encrypted
SecretStore partition: disk work is moved to the blocking pool; create/replace/take/
delete are bounded and serialized; restart recovery is durable; and corruption is
isolated without converting keychain or I/O outages into empty state. Phase 5C3a adds SDK-owned published discovery,
registration selection, PKCE authorization, exact issuer-aware live/restart callback
exchange, a redacted trusted-browser URL capability, one active flow per exact binding,
and bounded iterative expiry reclamation. Phase 5C3b now mounts the exact opaque
callback route and adds a process-wide bounded live-flow/coordinator registry, no-shell
system-browser launch, query-only callback reconstruction, and scoped canonical pending/
resolved UX. The broker remains dormant until the governed catalog registers a
coordinator, so no production skill has migrated. Phase 5C4 adds exact-binding
secret-free status, SDK-owned refresh, definitive-rejection-only invalidation,
idempotent local logout cleanup, and bounded explicit scope upgrades. Product lifecycle
mutations are one-owner per registered binding and cannot overlap browser/callback work;
success and stable failure classes produce value-free lifecycle telemetry. Local logout
does not claim unsupported provider-side revocation. Stable callback slots carry a
separate random attempt id in canonical pending UX, and dismissal reclaims that exact
pending/PKCE attempt before resolution. The isolated client suite passes 65 tests plus
strict all-target Clippy. Catalog work and all production strategy routing remain
disabled. Magician also hard-caps the SDK OAuth trace target at INFO so
authorization codes and token responses cannot be enabled through debug log settings.
Phase 5D1 now compiles bounded exact-tool MCP policy authored locally before remote
catalog publication. Deny wins over allow, the skill-wide floor can only be strengthened,
and unclassified tools require conservative conditional-external-side-effect approval.
Remote annotations and prose have no authorizing effect; only a bounded trusted local
description may later reach a model catalog. Effective entries share their immutable
base policy across the maximum 512-tool projection, and empty policy maps preserve legacy
serialized catalog bytes. Phase 5D2 is now implemented in `magician-mcp-client 0.1.6`
and `tool-runtime-core 0.1.41`: eligible SDK descriptors are projected atomically through
an iterative bounded JSON-Schema subset, exact non-lossy dotted names, and an 8 MiB
aggregate budget. Denied descriptors are removed before their schemas are interpreted;
remote prose and schema annotations cannot enter the sole explicit serializable model
view. The 74-test client and 353-test core suites pass with strict all-target Clippy.
Phase 5D3 is now implemented in `magician-mcp-client 0.1.7`. A dormant process-local
owner publishes complete bounded cross-skill `Arc` snapshots atomically under optimistic
revisions, rejects namespace/tool collisions without disturbing the prior snapshot, and
avoids revision churn for identical candidates. Opaque tool ids bind the exact connected
client plus discovery generation, so rediscovery or another client invalidates stale
authorities even when a remote name is unchanged. The Phase 5D3 client suite passed 86
tests and its 14-source Phase 0D3 handoff was frozen by `tool-runtime-core 0.1.42` with
strict Clippy passing for both crates. Magician does not instantiate this owner, publish
its snapshots to the live catalog, or route any production skill through it. Phase 5D4
is now complete in `magician-mcp-client 0.1.8` and `tool-runtime-core 0.1.43`:
model-facing schemas omit bounded remote regex prose and reject malformed patterns,
non-portable property names, oversized scalar literals, and
type/bound contradictions; projected catalogs are move-only with metadata-only debug;
retained local policy strings are included in byte accounting; and failed or cancelled
rediscovery preserves the prior callable generation while notifying the server. The
isolated client suite passes 91 tests, the isolated core suite passes 355, the refreshed
14-source handoff remains at 62 fixtures and 20 terminal classes, and strict all-target
Clippy passes for both crates.
Phase 5E1 is now complete in `magician-mcp-client 0.1.9` and frozen by
`tool-runtime-core 0.1.44`. Tool calls reserve finite continuation count and retained
bytes before dispatch; incomplete SDK replies become opaque exact-client/tool/revision
handles with monotonic local and server-TTL bounds, while complete, failed, timed-out,
and dropped requests release their reservation. No provider request state, input request,
task id, original argument, or SDK type crosses serialization or debug. The isolated
client suite passes 100 tests and the handoff now freezes 15 sources; governed live MRTR
resume is Phase 5E2, and production routing remains disabled.
Phase 5E2a is now complete in `magician-mcp-client 0.1.10` and frozen by
`tool-runtime-core 0.1.45`. Exact active input-required revisions expose only opaque
payload-free response slots; move-only responses are complete/duplicate-free, bound
privately to SDK keys, and validated against sampling, roots, and primitive elicitation
result contracts under finite JSON limits. The isolated client suite passes 112 tests,
strict Clippy passes, and the handoff freezes 16 sources. Atomic claim, SDK retry,
next-revision retention, round cancellation, capability advertisement, and production
routing remain disabled.
Phase 5E2b is now complete in `magician-mcp-client 0.1.11` and frozen by
`tool-runtime-core 0.1.46`. Prepared MRTR responses can be atomically leased only by
their exact active owner/revision after kind, monotonic deadline, identity, and retained
bytes are rechecked under one lock. The move-only claim is payload-free and exclusive;
dropping it before dispatch restores the same unexpired revision, while expiry removes
it. Concurrent, stale, removed, cross-owner, over-capacity, exhausted-identity, and
poisoned claims fail before transport. SDK retry, revision advance, capability
advertisement, and production routing remain Phase 5E2c onward.
Phase 5E2c is now complete in `magician-mcp-client 0.1.12` and frozen by
`tool-runtime-core 0.1.47`. One exact claim drives one official `rmcp 3.1.0`
typed call round, preserving original arguments and echoing SDK-owned request
state unchanged. A validated terminal response settles ownership; another
input-required response or task atomically installs the next exact revision. Exact
streaming serialization bounds request retention without a duplicate payload buffer,
and remote failure, timeout, or dropped retry future releases local ownership
fail-closed. The isolated client suite passes 131 tests, core remains at 355, and strict
Clippy plus the 16-source handoff pass.
Phase 5E2d is now complete in `magician-mcp-client 0.1.13` and frozen by
`tool-runtime-core 0.1.48`. Independent state-only and interactive round counters cap
each continuation under immutable 64-round ceilings. A sticky payload-free caller token
uses the official SDK cancellable-request handle: a token cancelled before retry entry
restores the claim, while cancellation after the retry boundary, timeout, future drop,
and transport loss consume the dispatched revision and release local capacity. The
isolated client suite passes 139 tests, core remains at 355, and strict Clippy plus the
refreshed 16-source handoff pass. Capability/presentation qualification was deferred to
Phase 5E2e; automatic round driving, task resumption, and production routing remained
disabled.
Phase 5E2e is now complete in `magician-mcp-client 0.1.14` and frozen by
`tool-runtime-core 0.1.49`. MRTR presentation capabilities are immutable and empty by
default; trusted callers may independently declare only base sampling,
schema-validated form elicitation, and roots. The official SDK advertises that exact
set, and both initial and replacement responses reject unadvertised kinds, URL
elicitation, and sampling tools/tool choice/context before retention. The isolated
client suite passes 145 tests, core remains at 355, and strict Clippy plus the refreshed
16-source handoff pass. Automatic round driving, task resumption, and production routing
remain disabled; Phase 5E3's governed task lifecycle is next.
Phase 5E3 is now complete in `magician-mcp-client 0.1.15` and frozen by
`tool-runtime-core 0.1.50`. The official Tasks extension is explicit, default-off, and
bilaterally negotiated. Opaque exact-revision handles drive one bounded `tasks/get`,
`tasks/update`, or `tasks/cancel` operation at a time; task ids and status/input payloads
remain private. TTL and poll intervals are honored under local lifetime, poll-floor,
count, and byte bounds. Coalesced task notifications only permit an earlier
authoritative poll, while update/cancel acknowledgements retain ownership until a
validated terminal result. The isolated client suite passes 157 tests and the handoff
freezes 17 sources. Production routing and restart recovery remain disabled; Phase 5E4
subscriptions and invalidation are next.
Phase 5E4 is now complete in `magician-mcp-client 0.1.16` and frozen by
`tool-runtime-core 0.1.51`. Notification capabilities are explicit and empty by default;
MCP `2026-07-28` uses one exact bounded SDK subscription stream, while legacy
`2025-11-25` resource updates use the official subscribe/unsubscribe operations and
typed callbacks. Payload-free revision epochs coalesce notifications, initial activation
forces authoritative loading, and full tool discovery swaps callable authority only
after every page validates. Failed refresh keeps the old generation dirty, a racing
notification cannot be acknowledged away, stream loss dirties every accepted category,
and existing continuations survive catalog replacement. The isolated client suite passes
168 tests and the handoff freezes 18 sources. Production routing and durable restart
recovery remain disabled; Phase 5E5 transport-loss/restart qualification is next.
Phase 5E5 is now complete in `magician-mcp-client 0.1.17` and frozen by
`tool-runtime-core 0.1.52`. Remote Tasks can produce a bounded, secret-free durable
checkpoint bound to the exact scope/profile/resource and product continuation slot;
MRTR remains SDK-session-bound and fails closed. Recovery is prepared before dispatch
so its operation count/revision can be durably replaced, then a fresh connection must
revalidate protocol, server, discovered tool, immutable task identity, deadline, and
current state through official `tasks/get`. The isolated client suite passes 179 tests
and the handoff freezes 19 sources. Production routing remains disabled; Phase 5F owns
browser/native/delegated adapters.
Phase 5F1 is complete in `tool-runtime-core 0.1.53`. Browser-profile authority stays
inside the browser controller: selected and implicit profiles are bound to the exact
scope/provider, selected registry revision where applicable, session class, and browser
session revision. A move-only delegation prepared from one ready observation must be
revalidated against a fresh observation immediately before dispatch. Profile/session
replacement, sign-out, and class drift fail closed, while cookies, CDP credentials,
browser storage, and copied profiles have no adapter representation. Phase 5F2 is now
complete in `tool-runtime-core 0.1.54`: authoritative OS observations for Accessibility,
Screen Recording, Microphone, Notifications, and Automation project to common status
plus exact request, await, settings, restricted, unavailable, or recheck guidance. An
opaque non-serializable binding prevents subject or Automation-target crossing. The pure
adapter has no permission handle or prompt/settings/grant/dispatch method, and `ready`
remains observational pending an OS recheck at use. Twelve focused tests include stdio
MCP composition and 100,000 projections on a 64 KiB stack. Phase 5F3 is now complete in
Magician `0.6.1142`: a move-only verified-executor admission binds the exact delegated
scope, secret reference, provider, agent, and route, then moves the existing 30-second
single-use grant directly into sealed material preparation. Raw-token construction is
test-only, and drop/failure paths revoke unredeemed grants before zeroization. Production
routing remains disabled. Phase 5G conformance closure is now complete in
`magician-mcp-client 0.1.21`, `tool-runtime-core 0.1.67`, and Magician `0.6.1162`.
The exact-pinned official Rust SDK client suites cover MCP `2025-11-25` and
`2026-07-28` with no expected-failure baseline, while the checked local matrix covers
both governed transports, OAuth resource/profile isolation, hostile discovery and
continuations, browser no-export authority, native states, delegated grants, and every
cross-strategy auth/invalidation combination. The offline closure guard is included in
`make check-all`. Phase 6 is implemented in `tool-runtime-core`: exact
move-only CLI intents now join authorization-before-auth admission, clean credential
materialization, executable/cwd authority, bounded batch or explicitly interactive PTY
ownership, cross-segment redaction, declared artifacts, typed terminal dispatch, and a
metadata-only audit settlement. PTY input is queued outside the control loop so a
non-reading child cannot suppress timeout/cancellation. Filesystem credential targets
without an explicit process-placement contract fail closed. Production routing remains
disabled for unmigrated families. Phase 6 closure passes 58 focused governed-runtime
tests. Phase 7 migrated all 63 tool packages that existed at closure. The subsequently
added `document-to-markdown` package was governed from inception, bringing the active
catalog to 64 packages. Each package compiles from one governed `SKILL.md`; direct CLIs
have no package-owned
plumbing wrapper; exact secret references replace credential-name guessing; and 39
retained adapter/support files have explicit irreducible ownership. Former skill
implementations and replaced wrappers were deleted after migration closure; selected
schema-only compatibility baselines live under `magician/tests/fixtures/` and remain
outside discovery and same-call fallback.

Remote Swiggy and Zepto MCP now use the official Rust SDK through the shared Auth
Broker and a provider-neutral executor. Registration, inventory, classification, and
replay share the same five stable MCP product controls while remote tool schemas remain
live SDK discovery data. Local commerce approval, cart/amount verification, Resource
Authority settlement, bounded iterative result projection, and durable audit remain
product-owned. Current generated evidence records 60 skills, 39 adapters, and 423
actions, and `anonymous/default` is rematerialized from the governed source. The
owner-deferred verification is complete: `make check-all`, all 10,158 executed Rust
tests, and the Rust doctest pass are clean. The final migration commit and explicit
frozen-evidence cleanup decision remain; see the
migration ledger.
The post-closure hardening binds CLI spawn to the validated executable descriptor,
bounds lifecycle reader cleanup and process admission, makes OAuth callback settlement
commit-aware with fixed exact-binding vault keys, adds explicit coordinator unregister,
owns stdio server process groups, and enforces one process-wide continuation budget.
OAuth live-attempt completion/cancellation now participates in that same exact-binding
lifecycle serialization, rejected clients synchronously close an already-connected
service, and the obsolete vault namespace-scan surface has been removed.
Grant payloads remain borrow-only and zeroizing; batch redemption is atomic, bounded,
scope/provider/agent/route exact, and retains typed expiry/replay failures. Durable
continuation inputs are now consume-once,
execution-local LLM routes remain sealed across pause/resume, child terminal
reducers merge against fresh task state without moving clocks backward, and
background task summaries retain the source execution's route and delegated
agent attribution. Native iPhone Tutor and browser Tutor now enter the same
lazy, ordinary-stack execution boundary; the non-streaming iOS route no longer
polls the complete Chat/Tutor future on an Actix worker, and the agentic core
uses an execution-local scheduler lane instead of an enlarged worker stack.
Channel reminders now remove their resolved follow-up row,
and Worth-a-look contextual actions retain their typed API contract. Canonical
Attention now exposes the full learning pipeline and preserves the exact
rank/decision binding required for verified impressions; Follow-ups retain
their rich controls and share native Apple Reminder creation with Worth a look.
Gmail incremental sync keeps and repairs
the owning mailbox identity, so existing and new Open links route through an
encoded `authuser` instead of Chrome's default `/u/0`. The same release hardens
Ambient Orb wake ownership, half-duplex/gapless speech, realtime cancellation,
ordinary-stack guided flows, provider-neutral context reuse, Pi `0.83.0`
settlement, evidence-repairing web research, and legacy research-memory
normalization. Final web-research deliverables are grounded against their
actual durable artifact content before publication, with one bounded repair and
a non-generative fallback; analytics contention is reported as retryable and
eval-inconclusive instead of being confused with answer failure. Terminal task
state is now validated prospectively and committed only after the evidence
gate; delegated children cannot take task-root ownership; accepted exact
deliverables remain byte-preserving; continuation prompts send only changed
sections; and execution-local model routing now covers synthesis, judging,
reflection, and consolidation as well as the agent loop. See
[Attention and HITL](components/magician/hitl-attention.md),
[Mail Assist](components/magician/mail-assist.md),
[local channel LLM](components/magician/local-channel-llm.md),
[Realtime Media Rails](components/magician/realtime-media-rails.md) (Gemini
3.8 Live and 3.8 Live Extended Thinking are selectable engines beside 3.1;
`make test-gemini-live-models` is the provider-free lane and
`make test-gemini-live-models-live` the opt-in live one),
[Mac Notch Orb](components/desktop/mac-notch-orb.md), and
[Pi coding contract](components/magician/pi-coding-engine-contract.md),
[Grok Build CLI coding contract](components/magician/grok-coding-engine-contract.md),
and
[Claude Code coding contract](components/magician/claude-coding-engine-contract.md),
and
[Antigravity coding contract](components/magician/agy-coding-engine-contract.md).
Magician `0.6.1114` unifies scalar and concurrent web acquisition behind the
single canonical `content_search` and `content_read` capabilities. Each accepts
either its existing scalar arguments or a bounded `requests` vector; there are
no separately registered batch tools or duplicate model-visible schemas.
Vector branches retain the same scope, policy, approval, provider ladder,
receipts, telemetry, and typed failure behavior as scalar calls, while nested
parameter denies and approval conditions are evaluated for every branch.
Magician `0.6.1112` and Magic Supervisor `0.1.8` harden local runtime
lifecycle. Ambient Dictation and other voice-note submissions now transfer an
owned transcript-to-chat closure to the dedicated execution runtime before the
Chat, memory-retrieval, and LanceDB/DataFusion future is constructed; the Actix
upload worker only awaits the resulting join handle. Those four workers use
Tokio's ordinary stack policy, and memory-snapshot overlap construction uses
explicit loops instead of the measured stack-heavy nested iterator chain.
Attention-dismissal
imports no longer enter DuckDB's crashing
native conflict-merge/index-replay path; supervised Magician launches own an
isolated process group so FluidAudio cannot survive a stop, restart, or native
backend crash. Unexpected backend exit status is now explicit in supervisor
logs, and the compatibility stop sweep targets only Magician's audio-engine
binary on `3029`. Notes are Markdown files Magician serves itself; there is
no separate notes process on `3021`.
Magician `0.6.1109`, Unified UI `0.0.757`, Magios `0.1.170`, and Magician
Desktop `0.2.65` separate the
human-visible notes folder from canonical runtime storage. Fresh setup
serves only `$MAGICIAN_ROOT_DIR/MagicanNotes/spaces/<principal>/<workspace>` (normally
`~/MagicianNotes/MagicanNotes/spaces/<principal>/<workspace>`). Web, iOS, and
Android open that library inside Magician. Web Chat now has the same opt-in Audio Notes retention
contract as iOS, backed by a bounded IndexedDB outbox with cross-tab transcript
and upload leases. Notes tunnel publication is fail-closed behind a fresh,
time-limited verification of Cloudflare service-token, owner Allow policies,
and the notes-only Space boundary. Desktop settings preserve those three
independent fields and surface an unsafe legacy-root fallback instead of
silently re-saving the older conflated model.
See
[Notes Provider](components/magician/notes-provider.md).
Magician `0.6.1107` makes cascaded Hands-free voice safe on ordinary runtime
stacks: finalized utterances enter the dedicated execution runtime, all
non-streaming Chat entry points heap-isolate their complete turn futures, and
segmented streaming STT finalizes one session before opening the next. This
prevents both Actix-worker stack exhaustion and FluidAudio's overlapping-session
failure without restoring `RUST_MIN_STACK`; pre-ready audio diagnostics are also
aggregated and rate-limited. See [Realtime Media Rails](components/magician/realtime-media-rails.md)
and [Runtime Async Stack Boundaries](components/magician/runtime-async-stack-boundaries.md).
The same development release adds a public-API pre-plan live qualification
lane. It uses the active real-profile mapping and treats Plan-panel visibility,
Attention visibility, canonical resolution cleanup, rejection/replan, and
linked LLM facts as hard gates. Use `make test-preplan-flow-live-eval` for the
fixture-backed `/evals` lane or `make eval-preplan-flow-live-interactive` to
answer every HITL from the terminal. The interactive runner consumes the same
canonical input type and schema as Web/iOS: it validates numbered single and
multi-choice answers, masks passwords, supports multiline text/guidance and
file lists, and renders confirmation, external-action, tool/sandbox, and
full/partial diff approvals without collapsing them into text. See [V3 Complete Flow](components/magician/v3-complete-flow.md),
[HITL / Attention](components/magician/hitl-attention.md), and the
[scripts reference](components/scripts/README.md). Its first production run
found a real split-brain state: plan-only HITL was visible in the Plan panel
but missing from Attention because planning executions are not normal
execution-tree roots. The projection now reconciles both surfaces from the
authoritative latest plan and refreshes them immediately after each response.
The writer and Attention API also share one injected feed-store handle, closing
the second split-brain path where the HITL row existed in DuckDB's WAL but an
already-open API database instance still returned an empty lane.
The eval surface now also includes a task-level web-research lane for the
highest-traffic, highest-variance agent workflow. It runs both a direct
`web-researcher` task and a parent-agent delegation using current official web
sources, measures answer-ready latency without imposing an evaluator deadline,
requires the unified `content_search` capability across each case's two
independent authority branches, and records specialist lineage, paired tool
events, citation authority and reachability, final redirect targets, opened-page
text faithfulness, profiles/models, tokens, failures, cost, and cleanup. The
governed evidence judge now fails an answer whose material claims are absent
from the cited pages even when every URL is reachable. The delegated fixture
requests both branches in one vector
call; deterministic handler coverage pins the scalar/vector dispatch contract.
Use
`make test-web-researcher-eval-harness` without providers or
`make test-web-researcher-live-eval` against the running service; Ctrl-C safely
cancels and cleans its active disposable task. Both lanes
and their exact HTML report directories are discoverable on `/evals`. The
same case drives the working-set router's lanes:
`make test-working-set-routing-live-eval` is the lane-off/lane-on A/B by
live-config swap (the off arm is the gate shut, whatever the file holds),
and `make test-working-set-routing-as-configured` is the check after an
operator opens or closes a lane — no config writes, every run must be
decided under the lane the file opens (activated only when the task's scale
qualifies) and none under a lane it keeps shut. See
working sets,
[Eval Lanes](components/magician/eval-lanes.md) and the
[scripts reference](components/scripts/README.md).
The cross-flow runtime performance lane turns the serialization/memory work
into a repeatable measurement rather than an architectural claim. Run
`make test-runtime-performance-live-eval` after services are rebuilt and
started. It covers direct and delegated tasks, pre-plan HITL/resume, web and
iPhone Tutor, concurrent Attention polling, and retrieval contention while
recording RSS peak/recovery, bounded store growth, context and queue/provider
timing, event-writer pressure, process continuity, and new crash-log signatures.
The Make and `/evals` lane atomically captures the first fully passing run as a
stable baseline; failed or inconclusive runs cannot promote themselves, and
later runs fail metric-specific regressions automatically. A different
baseline remains selectable with `RUNTIME_PERFORMANCE_LIVE_BASELINE=...`.
The provider-free harness is included in the normal aggregate test dashboard.
See
[Runtime performance and resource lane](components/magician/eval-lanes.md#runtime-performance-and-resource-lane).
Magician `0.6.1106` upgrades the pinned browser driver to upstream `v0.33.1`
while retaining Magician's download reliability, native file-chooser handling,
window handoff, CloakBrowser diagnostics, and relocatable skill discovery. It
adds upstream agent-oriented reads, accessibility audits, session/restore,
renderer recovery, WebGPU, and stronger allowed-domain containment. Magician
continues to own browser lifecycle, so the new upstream headless idle timeout is
disabled unless an operator or engine resolver explicitly configures it.
Magician `0.6.1105` adds Lightpanda as a fast DOM-first engine behind the
existing browser tool. It is a soft preference only for isolated public
headless reads, especially high-volume/fan-out work, with a bounded fallback
through the configured full-fidelity engine and then bundled Chrome for Testing.
CloakBrowser remains the default for visual, identity-bearing, meeting, and
preview work and supports both headless and headed operation. Magician drives Lightpanda through agent-browser CDP, so
Lightpanda's own LLM is never started. See [Content Sources and Retrieval](components/magician/content-sources.md).
Magician `0.6.1103`, Unified UI `0.0.754`, and Magios `0.1.167` complete the
guided-voice Tutor/Tutor Quick rollout. Voice ingress uses deterministic,
auditable admission for feature, visual source, device capability, and exact
screen-lock safety; it does not replace the agentic Tutor/App Copilot runtime.
After admission, the existing LLM/tool workflow interprets the open-ended
request, reasons, narrates, draws, and acts. Lock transitions cancel in-flight
capture/run work, rejected requests never revive after unlock, and iOS keeps
backend-spoken guidance transient. See [Personal Tutor and App
Copilot](components/magician/personal-tutor.md).
Magician `0.6.1102` and MagicLLM `0.2.21` retire GPT-5.4 from active routing.
OpenAI adaptive chat now has distinct Luna Instant, Terra Normal, and Sol
Advanced tiers; Instant is the shipped/live default, while each tier can
self-escalate from a reasoning-disabled profile to a high-reasoning sibling.
The July 30 GPT-5.6 Terra/Luna prices are effective-dated rather than replacing
launch history, including cache-write and long-context pricing, and the bounded
historical reprice path corrects stored costs without relabeling the model that
actually served a call. See [Chat Profile Routing](components/magician/chat-profile-routing.md)
and [DuckDB Analytics](components/magician/duckdb-analytics.md). Magician
`0.6.1108` and Unified UI `0.0.756` add
[Browser Engine Observability](components/magician/browser-engine-observability.md):
actual command-boundary engine attempts, sanitized links, typed work ownership,
explicit fallback rows, server pagination with selectable page size, and an
Observe dashboard. Its scope-owned ledger is bounded to 90 days and 50,000
attempts. Native agent-browser executables are rebuilt from the committed
upstream tag + patch instead of being stored in Git.
Magios `0.1.169` gives Ambient Listening its own
device-local Dictation, Hands-free/FluidAudio, or Live/realtime mode. Dictation
is a bounded multi-turn recording → STT → agent → TTS loop, not merely a
composer action. The backend preference seeds a new installation once; later
Ambient changes do not rewrite Chat's voice mode, and system voice launchers
pass per-call overrides without mutating it. See
[Magios Ambient Mode](components/magios/ambient-mode.md).
The Web `/warroom` Ambient Orb now follows the same three-way, one-time-seeded
device-local contract. Its Dictation mode is likewise an iterative bounded
recording → STT → agent → TTS → follow-up loop, while ordinary composer
dictation remains one-shot. See
[War Room Ops Deck](components/unified-ui/warroom-ops-deck.md).
The native Mac Ambient Orb now follows that three-mode contract too, with its
own one-time backend seed and a native bounded Dictation loop that survives
without an open browser tab. Backend-proxied Live now waits for an explicit
provider transport-ready edge behind bounded handshake/readiness deadlines and
falls back once to Hands-free on fatal native Orb failure. Dictation and
Hands-free share prebuffered, drain-aware native playback with underrun
telemetry. The compact and expanded Orb can be parked independently by dragging
their glass surfaces; normalized device-local positions survive restart.
Cascaded Hands-free is half-duplex on raw macOS capture: native playback and a
bounded acoustic tail gate microphone upload, with the backend enforcing the
same boundary before STT. Streaming Orb sessions release their full microphone
after the configured semantic follow-up window and return to lightweight
wake-only listening; Live realtime remains full-duplex for barge-in.
Magician Desktop `0.2.64` transports the scoped primary-agent identity through
the realtime `session.ready` and Orb caption event, so visible and spoken
captions use the resolved speaker rather than a role-based hardcoded label.
Magician Desktop `0.2.63` applies the same separation to the Mac Notch Orb:
wake and **Talk Now** snapshot the Orb's local conversation engine rather than
other voice-surface settings. Wake recognition is biased to the configured phrase,
and the ready surface shows the exact invocation. See
[Mac Notch Orb](components/desktop/mac-notch-orb.md).
Magician Desktop `0.2.62` adds the [Mac Notch Orb](components/desktop/mac-notch-orb.md):
a process-owned ambient voice lifecycle, notch-aware all-Spaces NSPanel, and
voice-reactive WebGL aurora whose silhouette, size, internal current, rim, and
halo mutate continuously as it moves losslessly between compact notch,
expanded, and center-screen Spotlight sizes. Native wake and hands-free conversation continue through idle/lock
without claiming custom lock-screen UI; tray and Settings provide bounded
leash, pause, battery, phrase, shortcut, and Rest/Wake controls. The custom panel
now owns macOS focusability after its native class conversion, preventing Tao's
private-ivar focus setter from aborting application startup while preserving
resting click-through and explicit expanded-card focus.
Unified UI `0.0.752` and Magios `0.1.163` complete the Unified Task Panel as the
single capability-driven task-detail contract across web and native iOS,
including truthful paused/archived states, bounded live refresh, stable output
revalidation, and honest 200-row long-run windows.
See [Unified UI task panel](components/unified-ui/unified-task-panel.md) and
[Magios task verdict](components/magios/task-verdict.md).
Magician `0.6.1100` activates progressive Slice-1 attention learning for the
next rebuild/restart: prior typed owner labels migrate behind an immutable
cutoff, semantic coverage and rank recomputation run in bounded background
workers, and Bayesian order applies within the existing Follow-up or Worth-a-
look lane. Legacy action logs now form durable repair tails, and unresolved
local embeddings use fair reclaimable leases plus bounded backoff; exhausted
work remains visible as `dead` instead of silently blocking other scopes.
Accepted labels therefore survive transient store/model outages. Exact
cross-lane identity dedupe remains authoritative, while
learned lane movement and suppression remain snapshot/canary gated. See
[Attention Routing Funnel](components/magician/attention-routing-funnel.md).
Magios exposes one **Talk to Magican** action across the Home Screen, Lock Screen,
StandBy, Control Center, Action Button, Shortcuts, and Settings. A tap begins the
first conversation immediately in the phone's selected **Dictation**,
**Hands-free**, or **Live** Ambient mode. When that turn ends, the same bounded
window stays available for wake-word follow-ups, with availability and Stop in
the Dynamic Island. The previous one-shot route remains hidden compatibility
for installed automations, while a durable App Group handoff covers cold and
warm launches.
See the [Magios guide](components/magios/README.md).
Magios Settings also derives Siri guidance from the current primary Crew
identity. The alias-free shipped identity exposes the plain **Ask Magican** App
Shortcut; its redundant app-qualified duplicate is suppressed. A guided native
Shortcuts path can optionally create **Ask Magican** or **Ask Magican AI**.
Magios `0.1.138` ships that runtime-named Siri setup, the system-wide voice
launcher, and tap-or-hold Dictate control. Unified UI `0.0.747` makes War Room
run inspection preserve canonical planned steps and renders focused runs as a
layered dependency graph rather than a clipped radial cluster.
Magician `0.6.1095`, MagicLLM `0.2.19`, Unified UI `0.0.749`, and Magios
`0.1.144` unify tool-result consumption and automatic turn context across
Chat, realtime voice, and autonomous tasks. Complete safe results are stored
once under their existing lifecycle owner; models receive exact bounded
records and typed omissions, while Web/iOS can reconstruct authenticated full
results losslessly. Fast memory, hybrid memory, and reusable procedures now
return independently under one absolute per-surface deadline, so a slow sibling
cannot erase useful evidence. The normal provider-free suite and optional
six-repeat counterbalanced live tail publish linked JSON/JSONL/HTML evidence. See
[Chat Mode](components/magician/chat-mode.md),
[Realtime Media Rails](components/magician/realtime-media-rails.md),
[Task Result Summary](components/magician/task-result-summary.md), and
[Eval Lanes](components/magician/eval-lanes.md).
Magician `0.6.1095` also repairs persisted interrupted tool-call batches before
native provider replay. It retains every complete call/result pair, omits only
unmatched protocol entries from provider input, and discards continuation
anchors that may have chained through the interruption—without deleting the
durable transcript or requiring the user to clear a chat session. The first
successful final response produced from that repaired history becomes a
durable trusted checkpoint, so subsequent turns resume native continuation
instead of paying permanent full-history replay cost.
The normal provider-free suite now includes the replay/report harness, while
the optional `provider-replay` live lane sends the same repaired synthetic
history through every configured tool-capable provider family. OpenAI
Responses additionally proves a persisted clean `response_id` can be reused
and is invalidated by a newer interruption; other providers are held to their
local native or flattened replay contracts without an OpenAI-specific
assumption. JSON, JSONL, and clickable HTML evidence is published under the SSD
coverage tree and linked by `/evals` and the aggregate live dashboard.
Magician `0.6.1092` makes archive-old-episodes consolidation durable across
deadlines, retries, and process loss. Persisted workflow/session group plans
commit at most six episodes per checkpoint and advance the source cursor only
after the complete bounded snapshot succeeds. Explicit source episode IDs make
replay idempotent, transient model failures retain capped retry liveness, and
named runs share the scheduled executor without a redundant quality-model pass.
Deterministic and live-shadow contracts cover exact membership, atomic cursors,
legacy migration, and crash recovery. See
[Ollama Logical Context Chunking](components/magician/ollama-logical-context-chunking.md)
and the [Agent Definition Reference](components/magician/agents/agent-definition-reference.md).
Magician `0.6.1090` aligns resurfacing candidate and embedding timestamps at
the scorer boundary. Retention can now prune an expired terminal card and its
expired vector together without discarding genuinely recent, contract-qualified
resume-cache entries. The deterministic scorer-to-retention regression covers
the production, replay, and injected-clock contract. See
[Proactive Resurfacing](components/magician/resurfacing.md).
Magician `0.6.1100`, `magicllm` `0.2.20`, and Unified UI `0.0.752` added Web-first
voice launch for Tutor, Tutor Quick, their screen and blackboard modes, and App
Copilot across Dictation, Hands-free, and Live. Screen requests use a fresh
server-attested capture; blackboard remains source-free. Provider response
identity is fenced from response creation or the first caption delta so a
racing ordinary answer or tool call cannot escape after guided takeover. Native
iOS support followed in the release above without enabling device screenshots.
See [Personal Tutor and App Copilot](components/magician/personal-tutor.md) and
[Realtime Media Rails](components/magician/realtime-media-rails.md).
Magician `0.6.1087` and vector-index `0.1.10` complete the foreground-safe
LanceDB and Ollama embedding path. Physical prompts respect both model context
and runner batch ceilings with truncation disabled; exact embedding contracts
invalidate incompatible caches; request reads take priority over bounded
maintenance; and memory prompt tiers reuse an immutable, revision-invalidated
30-second projection. The live retrieval gate covers real writer and optional
background contention while retaining its 500 ms p50 / 800 ms p95 budgets.
See [Memory Index](components/magician/memory-index.md),
[Memory Evals](components/magician/memory-evals.md), and
[Vector Toolkit](components/magician/vector-toolkit.md).
Magician `0.6.1081`, `magicllm` `0.2.16`, Unified UI `0.0.719`, and Magios
`0.1.133` add selectable GPT/Gemini realtime profiles while retaining GPT as
the default. Gemini protocol JSON is accepted in text or binary WebSocket
frames; PTT/open-mic authority survives startup and provider rotation; Web and
iOS keep their surface choices independent; and active voice sessions
temporarily suppress ordinary reply narration without changing the saved
auto-speak preference. Magios also aligns its Live and reply-speaker split
controls under one neutral, stable-size visual contract. See [Realtime Media Rails](components/magician/realtime-media-rails.md)
and the [Magios guide](components/magios/README.md).
Web voice startup now absorbs two transient control-proxy resets before
failing, while iOS adds themed voice/profile sheets, explicit accent
foreground parity, and compact Attention message actions ordered Useful,
Dismiss, Acknowledge, Snooze.
Magician `0.6.1079` hardens the visual teaching rails without merging their
behavior. Normal Tutor now uses an adaptive progressive milestone plan and
Tutor Quick a smaller condensed plan on both screen overlays and blackboard;
App Copilot retains its separate action workflow with persisted rail identity,
single-use storyboard-bound automation checks, cancellation-before-verification,
terminal-child recovery, and enforced demo cleanup. See
[Personal Tutor and App Copilot](components/magician/personal-tutor.md).
Magician `0.6.1075` stabilizes recurring-monitor agent execution by hashing
authorization sets in canonical order while preserving fail-closed rejection
for real policy changes. Its live evaluator uses an isolated scope, cancels
unfinished work, physically removes scratch monitors, preserves partial
reports, and targets the standard API port. The same release aligns the Phase
2D/2E structural contracts with centralized Parquet and embedding projections
and gracefully joins the singular storage-maintenance runtime before final LLM
observability batches are drained. See
[Recurring Monitors](components/magician/recurring-monitors.md),
[LLM Training-Data Observability](components/magician/llm-training-data-observability.md),
and [Storage Governance](components/magician/storage-governance.md).
The same storage owner now rolls active canonical LLM partitions every five
minutes: governed reads combine one verified compacted prefix with a bounded
raw tail, preserving immediate append visibility while avoiding thousand-file
daily query fan-out. Existing version-1 canonical manifests upgrade without a
raw-scan window.
Compaction impact is retained in a bounded, content-free per-scope ledger:
`/storage` distinguishes apparent file bytes reclaimed from canonical query fan-out
avoided, breaks results down by dataset, shows recent runs and the ledger's own
footprint, and can clear only those metrics through a guarded action.
Magician `0.6.1074` hardens the observable-source regression contracts without
changing the public API. Privacy coverage now rejects concrete feed URLs,
intents, and observed text while permitting content-free aggregate metric names;
the production resolver fixture also carries the provider-neutral validator
map required by conditional discovery.
Magician `0.6.1072` and Unified UI `0.0.702` complete observable web-source
delivery. Product Hunt, arXiv, and custom RSS selections now leave the durable
ingress through a bounded, retry-safe consumer, enter the shared resurfacing
curator as typed web candidates, and can appear under **Today > Worth a look**.
The source dashboard separates discovered, selected, processed, pending, and
quarantined records; opening an accepted item uses its canonical URL. An
unchanged RSS `304` remains a successful check with zero new candidates. See
[Content Sources And Reader Adapters](components/magician/content-sources.md#observable-sources-on-observe)
and [Proactive Resurfacing](components/magician/resurfacing.md).
Magician `0.6.1071` and Unified UI `0.0.701` add governed local-storage
maintenance. Store-owned verified copy-compaction reclaims DuckDB churn while
preserving exact rows, schema, indexes, views, sequence state, mail lifecycle state, and thread
tombstones. Generation-selected Parquet objects keep prior rows authoritative
through crashes and include late batches until merge; completed-day compaction
and a unified 90-day observability lifecycle now cover memory, local embeddings, and LLM tool-call
telemetry without touching mail, restricted content, tasks, memory facts, or
the recovery journal. `/storage` exposes scoped apparent/allocated/WAL sizes,
file counts, ranges, live retention and guarded actions, backed by deterministic
and isolated live gates. See
[Storage Governance](components/magician/storage-governance.md).
Magician `0.6.1070` and Unified UI `0.0.700` add a metadata-only Calls view to
`/llm` and separately capture local embedding batches for cost, latency, token,
model, and operation analysis without widening the ordinary LLM-call dataset.
See [DuckDB Analytics](components/magician/duckdb-analytics.md).
Magician `0.6.1068` hardens the scoped authority boundary shared by chat,
tasks, monitors, and resumed agentic executions. API admission and execution
now resolve through the orchestrator's agent-definition and trust-policy
stores; a missing owner snapshot cannot masquerade as an authoritative empty
catalog; and shared surface caches require a matching capability revision.
Trust denials are terminal before approval or catalog recovery, while explicit
Tutor and App Copilot lanes retain their delegation envelope. This release
also removes redundant repeated-term memory excerpts, classifies agentic
ledger compaction in the LLM-observability planning family, pins image analysis
to tool-free Responses mode, and hardens the trace replay validators. The full
test command is green across 7,294 Rust tests and every frontend, Python,
extension, desktop, macOS audio, iOS, and deterministic-eval suite. See
[Flat Agentic Execution](components/magician/execution/FLAT_LOOP.md),
[Agent Definition Reference](components/magician/agents/agent-definition-reference.md),
[Chat SSE Streaming](components/magician/chat-sse-streaming.md), and
[Memory Evaluation](components/magician/memory-evals.md).
Magician `0.6.1067` completes Web Reader Phase 8 and hardens flat-loop resume.
Provider-neutral reads can now progress through verified API replay, static
HTTP, deterministic rendered browsing, explicit public/headed handoff, and
approved authenticated CDP without silently crossing authority boundaries.
Browser-engine selection is configuration-owned and shared across retrieval,
ordinary browser primitives, screenshots, and meeting join. Two independent
live gates cover direct transports and the production scoped resolver plus
compiled handlers; the runtime gate passes all six scenarios. Agentic
pause/resume now restores the model's actual append-only conversation, and
task-backed success cannot rely only on harness-contradicted no-effect or
mismatch evidence. See [Content Sources And Reader Adapters](components/magician/content-sources.md)
and [Flat Agentic Execution](components/magician/execution/FLAT_LOOP.md).
Magician `0.6.1066` and Unified UI `0.0.697` complete LLM-observability Phase
4. Model-proposed tools now carry content-free lineage through validation,
authorization, approval, execution, result consumption, delegation, branch
selection and authoritative rollback; governed REST and `internal_data` trace
reads feed a responsive `/llm` explorer. The focused exit target is green over
204 Rust regressions, seven static contract tests and its evaluator self-test.
The representative live cohort remains pending because the current bounded
audit found zero eligible executions and correctly returned `skipped`. See
[LLM Training-Data Observability](components/magician/llm-training-data-observability.md).
Magician `0.6.1065` hardens the live LLM-observability exit path. Durable
journal replay now verifies exact raw-record checksums across JSON float
round-trips, governed economics accepts only adjacent one-ULP monetary replay
drift, and the canonical DuckDB reader follows the current executed-cursor
metadata contract. Strict isolated Phase 1 and Phase 2F gates pass with six
calls, complete attempt accounting, and 100% correlation/mirror/dispatch
parity; Phase 3 passes with 12 sanitized revisions, six redactions, and zero
privacy violations. The optional restricted API probe remains deliberately
setup-token-gated, and the runtime has returned to metadata-only capture. The
same release also restores scoped agent-catalog hydration across the historical
bare-integer memory-retention form while continuing to serialize canonical
`!days N` YAML, preventing an older peer definition from making Presto
unavailable to task and monitor admission.
Magios `0.1.119` hardens the canonical Thinking Map under strict Swift
concurrency checking: the isolated demo/UI-test transport dispatches under
`NSLock.withLock`, ambient Listen's best-effort detach results are handled
explicitly, and release documentation now reflects the complete canonical-only
map library, spatial lenses, Loom frontier, Listen, promotion, and share
ingestion experience. See the [Magios Thinking Map guide](components/magios/README.md#live-thinking-map)
and [canonical client architecture](components/magios/thinking-map-canonical.md).
Magician `0.6.1064` completes heuristic-reduction Phase 4 with default-on,
LLM-led agentic ledger compaction. A routed model proposes bounded
keep/summarize/discard patches while Rust protects structured failures,
terminal evidence, success criteria, and the newest turn; enforces the hard
provider budget; and preserves the compacted projection across pause/resume.
Focused, golden, live semantic, and bounded server A/B acceptance passed. See
[LLM-led ledger compaction](components/magician/execution/FLAT_LOOP.md#llm-led-ledger-compaction-available-default-off)
and its archived implementation record.
Magician `0.6.1063`, `magicllm` `0.2.15`, and Unified UI `0.0.696` complete
LLM observability Phases 0-3: stable call/attempt/product lineage,
journal-backed canonical economics and reliability facts, governed analytics
and `/llm` capture health, plus policy-gated sanitized content in a separate
audited namespace with one-use reveal grants and restart-safe deletion.
Provider-free and compilation verification is green; strict isolated live
correlation, reconciliation, and sanitized-storage evidence was subsequently
recorded by `0.6.1065`. The safe active default remains metadata-only. See
[LLM Training-Data Observability](components/magician/llm-training-data-observability.md).
Desktop `0.2.53` hardens macOS and Linux distribution while explicitly
deferring Windows packaging. Release publication now fails closed unless the
desktop updater and macOS signing/notarization inputs are present, verifies all
generated updater signatures before publishing `latest.json`, and preserves a
rollback copy during installer-driven macOS app replacement. The desktop host
also exposes configured loopback runtime endpoints so the packaged Magicutor
extension can discover non-default Magician and bridge ports safely.
Release signing can be moved to another trusted Mac without Git through
`make export-apple-signing-setup OUTPUT=...` and
`make import-apple-signing-setup BUNDLE=...`. The encrypted bundle carries only
the Developer ID, Distribution, App Store Connect, and Tauri updater material;
Xcode creates machine-local development identities and managed provisioning
profiles after the operator signs in on the destination Mac. See the
[scripts reference](components/scripts/README.md) for verification and recovery
commands.
`make release-desktop-signed JOBS=2` is the local production entrypoint: it
signs the native payload and app with Developer ID, notarizes and staples the
app and DMG, checks Gatekeeper, and produces the signed updater archive without
copying private material into the checkout. Tagged CI has the same DMG gate and
continues to fail before building when its external secret store is absent.
Magician `0.6.1058`, Desktop `0.2.54`, and Magicutor extension `0.1.267`
complete the code-addressable container productization pass. Managed backends
rewrite only loopback host-service endpoints through Docker's or Apple's
official host alias, Apple setup replaces the guessed vmnet gateway with
administrator-configured localhost forwarding, and configured service ports
remain reachable from the host. Container qualification now retains JSON
runtime evidence on Ubuntu 22.04/24.04 plus amd64/arm64 compressed-layer and
expanded-size reports; production credentials, real clean-machine runs, and
initial budget values remain explicit release gates.
Use `make qualify-container-e2e` for disposable local acceptance evidence, or
`make qualify-container-e2e-live CONTAINER_E2E_LIVE_CONFIRM=1` to exercise the
composed installer against the selected live runtime root.
The [2026-09-16 container integration checkpoint](components/scripts/container-runtime.md)
supersedes those historical completion claims for the current workspace:
the image recipe now uses a bounded Debian/glibc build of the split packages,
includes the compiled Apps contract, and seeds the required router sibling on
fresh/older roots. `make test-container-tooling` covers these boot and preservation
contracts without compilation. Linux ARM64 build, boot and direct skill probes
passed; desktop/browser acceptance remains pending. The checkpoint distinguishes
local bind mounts from the future remote desktop/file transport.
The Apple Container build runbook
records the isolated SSD-backed build and its resource limits. The container
Make target uses 16 codegen units for the monolithic library; host release
profiles and dependency profiles keep their existing settings.
The [local prebuild and OCI packaging flow](components/scripts/container-local-oci.md)
provides `make prepare-container-image`: snapshot committed HEAD on SSD1,
refresh/reuse the runtime base, build Linux artifacts, verify and import the OCI
image without replacing the running stack (`ARGS=--dry-run` previews).
Its offline orchestration tests do not qualify a fresh full build.
`make build-container-prebuilt-tools-image` packages already-qualified Linux tool
executables with checksum and non-root inventory checks, without recompilation.
Apple layer builds repeat inspected builder limits to avoid cache loss from
default resource settings; image/environment drift is refused before building.
The individual steps also provide reusable Linux artifacts and a cached runtime base. `make container-oci`
assembles a standard OCI archive without compilation or an image builder;
`make prebuild-container-artifacts` owns the separate cached Linux compile lane.
The [container host/browser/skill harness](components/scripts/container-integration-harness.md)
now supplies attach-only probes plus an isolated test-extension preparation mode.
`make test-container-integration-harness` runs its offline tests;
`make qualify-container-integration CONTAINER_INTEGRATION_ARGS='…'` records live
per-stage evidence without building or restarting the stack. Partial coverage
and unavailable skills cannot produce a full passing result.
Managed local containers now reach host automation through the desktop-owned
[private exec relay](components/desktop/README.md); its isolated wire tests run
with `make test-container-host-relay`.
The expanded Linux tools and host audit
records all 63 runtime skill definitions, distinguishes functional probes from
startup checks, and tracks the remaining host, extension and mobile gaps.
Magician `0.6.1059`, `magicllm` `0.2.14`, Unified UI `0.0.694`, Desktop
`0.2.55`, and Magios `0.1.116` move backend-proxied Live Call user captions to
Magician's canonical FluidAudio-first streaming-STT chain. Partial hypotheses replace one dimmed
ephemeral row; only the final transcript enters addressing, Tutor, current-turn
context, durable Chat, and normal final-caption rendering. Paid provider input
transcription is disabled only after local STT attaches and is restored
automatically if local startup or runtime processing fails. Direct browser
WebRTC retains provider transcription because its microphone PCM does not pass
through Magician.
The completion hardening adds generation-safe clear/commit handling, supervised
asynchronous fallback with bounded uncommitted-audio replay, exact partial
cleanup on web and iOS, a configuration-owned vendor recovery model, and an
acknowledged restore barrier before PTT commit. Use
`make test-realtime-local-transcript` for the focused provider-free lane and
`make test-realtime-local-transcript-live` with audited call evidence for the
quality, latency, fallback, real-device, and provider-cost gate.
Magician `0.6.1061` and Skillshub `0.1.3` establish the provider-neutral
content-acquisition foundation for user-defined Feeds. Existing comms-assist
distillations, RSS/Atom/JSON Feed, manifest-driven Exa discovery, and cached
static public-page extraction now converge on bounded candidate/document
contracts with explicit invocation source, cost enforcement, scoped cache
lifecycle, and selection evidence. Reader Phase 6 adds the process-owned,
scope/revision-bound acquisition service, readiness and operation telemetry,
skill-refresh invalidation, shared spend authority, coherent revision publication,
per-skill failure isolation, revision/depth-aware cache identity, bounded resolver state,
and runtime dependency injection. Phase 7 adds configured typed discover/read ladders,
deterministic evidence goals and quality boundaries, scope-bound selection receipts,
bounded cost/deadline/attempt/concurrency enforcement, generic `content_search` and
`content_read` tools, and declarative source migration for Exa, Tavily, Reddit, arXiv,
GitHub, Product Hunt, and Hacker News. Phase 8 adds verified read-only API replay,
deterministic public rendering, explicit public/headed/authenticated browser handoffs,
scope/domain/action/time-bound CDP authority, config-selected browser engines, lifecycle
cleanup, sanitized transport traces, and provider-free plus capped real-browser evals as
described in the reader design. Phase 10 adds strict manifest-declared Product Hunt and arXiv RSS
offers plus custom RSS on Observe, scoped revisioned subscriptions, exact-action deterministic
polling, conditional checkpoints, bounded intent selection, durable provider-neutral enrichment
handoffs, server pagination, and an independently failure-gated public-RSS live evaluator as
described in the archived
reader design. The only remaining
reader-runtime optimization is the
content retrieval budget scheduler and within-rung adaptation plan. Feed contracts,
storage, pagination, Web, Today, and iOS remain a separate
product plan.
Magician `0.6.1060` hardens memory-temperature recall without weakening scope
isolation or prompt budgets. Hybrid search now filters to the active agent's
authorized memory before bounded top-K selection, automatic prompts exclude
search-only environment knowledge, and oversized relevant memories use
query-focused fair-share excerpts instead of being dropped. A mixed real,
synthetic, and Ollama-backed utility eval runs last in opt-in live suites and
publishes linked SSD-hosted HTML evidence; the release gate recalled all 15
eligible real anchors and achieved 100% hybrid, synthetic, and utility coverage.
Magician `0.6.1057` completes and hardens the unified agent-surface runtime.
Chat and autonomous decisions cache revision-keyed static prompt prefixes while
keeping memory, procedures, files, and state dynamic; tasks retrieve semantic
context at explicit checkpoints rather than every model iteration. Production
Chat, realtime voice, and tasks now have one shared catalogue-projection path,
retired rollout switches are rejected, unsupported realtime profiles must use
an explicit grounded fallback, and missing task/run bindings fail closed. The
final 160-call live gate achieved 100% candidate exact/authorized selection,
34.1% fewer input tokens, 37.6% fewer schema bytes, 22.2% lower
cache-normalized cost, and 73 ms lower median latency; the provider-free gate
passed 15/15 scenarios. Unified UI `0.0.693` exposes the static-prefix cache in
Crew beside the existing effective-tool cache status.
Magician `0.6.1056` makes harness delivery progression fail closed: control-only
runs cannot claim completion, promoted terminal work remains pending until an
evidence-based acceptance or rework review, and the CPO/Harness SRE delivery
policy remains declarative and synchronized across templates and active scoped
definitions. Magician `0.6.1055`, `magicllm` `0.2.13`, and Unified UI `0.0.692`
activate the unified agent-surface tool runtime across Chat, realtime voice,
and autonomous tasks. They share scoped immutable capability/index snapshots
and bounded working sets while retaining surface-specific hot tools; deferred
`search_memory` and tool-family loading remain available through the same
authorization ceiling. Realtime voice now gates each response on bounded
current-utterance context and commits catalogue expansion only after a
correlated provider acknowledgement. Crew exposes the true post-filter tool
inventory plus cache provenance and a narrowly scoped refresh control. The
same Magician release closes harness delivery leaks with material-evidence
inspection, explicit acceptance/rework, and a durable Delivered state. Normal
test suites finish before the optional live-eval prompt, and explicit
`live_evals=1|0` keeps CI deterministic. Magios `0.1.114` adds Thinking Maps to
the native discovery guide.
Magician `0.6.1054` closes and archives the Worth a Look post-release
verification ledger by owner direction. The feature implementation remains
complete; its unchecked environment-dependent checks are retained only as
historical QA ideas, while any future production defect should be tracked as a
focused bug rather than reactivating the ledger.
The same release and Unified UI `0.0.691` make Crew an auditable view of each
agent's canonical post-policy tools for its selected interactive surface,
including server-owned internal actions and exact delegation ceilings without
exposing provider schemas. Realtime voice's `delegate_to_chat` escape hatch now
explicitly pins the thinking half of an adaptive chat profile for both
streaming and non-streaming reasoning instead of silently resolving back to
its fast half.
The unified agent-surface runtime is the configuration-free default. Chat,
realtime voice, and autonomous execution share immutable registry/index
snapshots plus the same process-owned, bounded surface-plan and working-set
stores while retaining their own initial hot-tool policies. Task owner frames
use a stable authority revision that excludes only the mutable loaded/deferred
split; policy or registry changes clear loaded state before the next provider
request, and handover removes the prior owner's binding. Retired rollout fields
are rejected; normal config contains cache, working-set, and realtime budget
tuning rather than activation switches. Production task binding and
authorization mismatches fail closed. Run
`make test-agent-surface-runtime-eval` for the provider-free production-catalog
gate and HTML report; the existing cost-bearing authorization evaluator adds
Chat deferred discovery, already-loaded family reuse, and voice-memory cases.
Magician `0.6.1053` reduces chat context-retrieval latency without changing
selection or fallback semantics. Memory and reusable procedures now receive one
shared semantic query, run concurrently, and coalesce identical query embeddings
through vector-index `0.1.8`. The normal `make test` run retains hermetic Rust
coverage for timing, concurrency, coalescing, fallback, and keep-alive behavior;
`make test live_evals=1` additionally gates real hybrid coverage, physical versus
reused embedding calls, and p50/p95 wall latency against the resident local
embedding daemon and active scoped indexes.
Magician `0.6.1052` now resolves one immutable effective tool-policy snapshot
per agent decision boundary. Chat, task-backed execution, deferred discovery,
structural delegation, handover, introspection, and realtime voice share the
same provider and dispatch ceiling; trust, approvals, ownership, denials, and
Resource Authority remain conjunctive pre-side-effect checks. Tutor, App
Copilot, and Thinking Map use typed surface/feature bindings, Loom is
allowlist-only and absent from ambient/wildcard routing, Public Envoy remains
tool-free, and live voice rejects calls whose advertised snapshot became stale.
The opt-in `make test live_evals=1` dashboard includes the five-repeat tool
visibility/authorization semantic evaluator and the resident local
chat-context retrieval gate alongside the six other live gates. The normal,
provider-free `make test` path also protects the authorization design with a
1,200-cell Rust policy matrix plus production-catalog provenance,
provider/dispatch equality, comparison-gate, profile-routing, targeted-runner,
and aggregate-report regressions; `make test-agent-tool-visibility-live-eval`
runs only the cost-bearing baseline/candidate lane when focused evidence is
needed.
Magios `0.1.112` completes the native chat activity and theme-parity pass.
Assistant responses now hydrate the canonical per-turn event projection across
live and historical views, expose the latest five steps plus the complete log,
and route Run, Stop, generated-file, and markdown-link actions to the same
logical targets as web. Persisted task/rich-content cards retain their actions
after reload, chat uses compact native navigation and jump-to-latest controls,
and semantic theme tokens update cards, actions, sheets, and navigation chrome
without requiring an app restart.
Magician `0.6.1045` closes the scoped LLM accounting gaps for direct operation
calls outside chat and the agentic decision loop. Memory consolidation and
promotion, utility/conflict review, learning reflection, artifact and evidence
helpers, channel/resurfacing processing, planning/query/slot operations,
API-mining compilation, media helpers, and clarification generation/answer
interpretation now publish priced provider usage into the canonical scoped
`llm_calls` analytics stream. Calls without genuine provider usage do not create
zero-token rows, and parent-owned chat/agentic calls retain their existing
single emitter to avoid double counting.
Magician `0.6.1044` and Magios `0.1.73` complete the mobile card-action parity
pass. Task cards use the same state-aware plan/run/reset actions as web, expose
the remaining metadata and lifecycle operations in a native Actions sheet, and
keep destructive swipe actions confirmed. Chat queue, artifact, and escalation
cards now expose their actionable controls without relying on long press. An
explicit task reset can reopen a terminal task to pending/ready after settling
its active root, while preserving historical runs and rejecting every other
terminal-state transition. Contextual and harness execution records are routed
to Internal tasks unless they represent an explicit user commitment.
Magician `0.6.1043`, Magicutor `0.1.87`, and Unified UI `0.0.687` replace API
Mining's passive auth browser opener with a deterministic, short-lived CDP
capture. Raw request credentials use a bounded memory-only drain, browser
cookies/storage are restricted to the target origin and learned auth names,
durable traces remain redacted, and the UI reports capture plus safe read-only
verification without exposing credential values.
Magician `0.6.1035` with `magicllm` `0.2.11` introduced Phases 1–6 of generic
Ollama logical-context chunking. Phase 7 now supplies the production structured
runner boundary and synchronized local candidate mappings. Provider-free
readiness plus the composite five-repeat synthetic local/current-cloud gate pass
for all six memory adapters with zero durable writes; reports live under
`coverage/evals/ollama-logical-chunking/{readiness,qualification}/`. The
activation manifest remains unapproved with empty canary scopes: broad
deterministic suites, normal-queue canary monitoring, durable-write observation,
and exercised rollback were not claimed by the archived implementation plan;
future findings in those operational areas receive new dated plans.
The logical-chunk evaluation lane can also benchmark the same production
adapters through native Ollama, llama-server, or an MLX-LM OpenAI-compatible
server while retaining the Ollama provider boundary used by Magician. MLX runs
are explicitly reported as prompt-constrained, server-context-owned, and based
on observed stream timing rather than native prompt/decode phase telemetry.
`LOCAL_CHUNK_EVAL_RUNTIME`, `LOCAL_CHUNK_EVAL_SERVED_MODEL_LABEL`, and
`LOCAL_CHUNK_EVAL_CONTEXT_TOKENS` carry those controls through the focused Make
targets and the optional `make test live_evals=1` lane; see the
[logical-chunk eval fixtures](../data/magician_v2/llm_chunking_evals/README.md)
for launch and cross-runtime comparison commands.
Magician `0.6.1032` makes outer-loop durable task-state metadata optional and
inline. Omitted `task_state_action` now deterministically means no mutation;
present create/patch/close envelopes remain strictly validated and atomic with
the selected work call, while legacy explicit no-op envelopes remain accepted.
Decision prompt v1.3.4 carries the complete mutation contract once, and every
tool advertises only a minimal open-object sidecar. The focused eval measures a
98.6% task-state schema reduction and a 56.5% reduction in a representative
29-tool catalog while running omitted, valid-present, malformed-present, and
legacy regression coverage.
Decision prompt v1.3.5 also consolidates the six sparse hover, vision, step,
and plan-revision signals into one optional `decision_metadata` object. The
runtime maps that sidecar onto the existing execution envelope, continues to
decode historical flat fields, rejects conflicting dual representations, and
strips both forms before pack dispatch. The deterministic eval measures a 93%
metadata-block reduction and 14,993 bytes saved in the representative 29-tool
catalog; the companion live A/B verifies real-model signal parity, sparse
omission, tool selection, latency, and token usage.
Magician `0.6.1031` resolves local generation identity and context exclusively
from Ollama profiles referenced by `llm.router.operation_mapping`; that
release's profiles selected `gemma4:26b-a4b-it-qat` at 32K. Local Ollama
generation now pins `qwen3.8-ud2-mtp` — see
[local channel LLM](components/magician/local-channel-llm.md).
Embedding model,
dimensions, context, evaluation batch tokens, and HTTP request batch size are
an explicit, config-only `runtime.ollama` contract. Embeddings run on a separate
`:11435` Ollama daemon with pinned residency and read-first admission, while
generation stays unchanged on `:11434`. There are no compiled or
environment model-contract fallbacks, and unavailable configured models fail
closed. This release also moves Worth a look's
semantic quality decision into the bound local curator. Its early prefilter is
now structural only; terse, title-like, generic-sounding, and recency-only
candidates reach prompt v1.2 with bounded content and signal evidence, while
stale revisions, invalid scores, and empty candidates still fail closed. The
deterministic path remains the conservative fallback when local curation is
unavailable.
Magician `0.6.1030` restores Channel Assist after upgrades from database schema
v10: scoped stores now migrate and backfill the materialized Today
`attention_lane` before current indexes are created, so sync, reconciliation,
distillation, feedback, resurfacing, and Today reads load normally on schema
v11.
Magician `0.6.1029` makes memory consolidation schema-bound and fail-closed,
canonicalizes learned program targets against their harness-agent definitions,
and reduces Today projection cost without changing latest-actionable semantics.
`magician-vector-index` `0.1.5` supplies the typed memory-operation contract.
Magicutor `0.1.86` with extension `0.1.265` prevents bridge reconnect/session
fanout through single-flight connection and active-generation ownership; the
unchanged extension remains compatible with Magicutor `0.1.87`.
Magician `0.6.1028` completes Gmail Mail Assist phases 7 and 8: full
`historyId` reconciliation handles manual sends, new mail, archive, confirmed
whole-thread deletion, trash/spam, and stale-draft review; repeated
sender/domain dismissals tune noisy recommendations; exact writing preferences
have candidate/promotion UI and a registered user-memory tier; and classifier,
draft-usefulness, Today, and Gmail-render boundaries have explicit testable
quality and latency budgets. Raw drafts and message bodies remain
non-persistent, and no automatic send path is enabled. Unified UI `0.0.678`
ships the corresponding Today/Attention review and Writing style surfaces;
extension `0.1.264` ships the measured renderer boundary.
Magician `0.6.1027` makes execution controls coherent across backend, web, and
iOS. Pause, resume, steer, and cancel now operate on validated active execution
trees with serialized status changes, durable manual checkpoints, authoritative
capabilities, and terminal-state protection against stale resume settlement.
Web and native clients share per-execution coordination, reject historical
targets, reconcile every mutation, and retain explicit retry state when a
capability refresh fails.
Magician `0.6.1023` gives task-backed research overruns one bounded final
synthesis turn before the no-progress loop guard concludes them. The runtime
requires the requested user-facing deliverable rather than another inspection
or progress report; if no material result follows, the task remains explicitly
partial/open and the orchestrator's synthetic status is not promoted into
completed work. Magician `0.6.1022` makes managed prompt JSON authoritative for Channel Assist
and Worth a look: distillation, repair, classification, pattern synthesis,
curation, and deeper summary no longer carry independent Rust prompt copies.
Prompt identities are centralized, missing assets degrade without substituting
different instructions, packaged resources resolve beside the executable, and
debug diagnostics never log prompt-variable values.
Magician `0.6.1021` completes trust-tiered agent-created task dispatch: the
one-time orphan reconciler now retires duplicate task records durably before
launching a keeper, holds dispatch on partial retirement failure, and is exposed
through a scoped dry-run-first API and CLI. Cross-owner harness work continues
through the durable owner-directed shared backlog lane rather than dead-pausing
an autonomous run. Magician `0.6.1020` hardens Worth a look end to end: action attempts are
revision/source revalidated and lease-bound, scheduled reminders are accepted
durably before their occurrence is consumed, routing/backfill state is scoped
and restart-safe, malformed derived rows fail locally, and persisted safe briefs
redact secrets without erasing concrete dates, amounts, or policy/change text.
Magician `0.6.1019` hardens startup and derived stores: initialized Channel
Assist databases migrate before current index DDL runs, repeated feed
projections avoid no-op writes, corrupt feed quarantine retention is bounded,
and resurfacing centrality uses config-backed batch and timeout budgets.
`scripts/install.sh`, `scripts/seed-silverbullet-space.sh`,
`scripts/run-supervisor.sh`, the desktop-managed container startup path, and the
container entrypoint copy the tracked repo-root config into the runtime root only
when the runtime copy is missing, and copy the default Harness SRE program plus
required default agent definitions into `scopes/anonymous/default/...` only when
those runtime files are missing, so operator edits are not clobbered by reruns.
Current execution hardening also assumes that placeholder shell taskplans are ignored during taskplan init, taskplan spill artifacts stay readable from scoped execution artifacts, semantically critical spill outputs remain visible in prompt history with prompt-visible artifact previews, spill reads never respill, browser-extension execution calls attach their configured workspace-bound bearer and never assert a fallback scope, and delegate-owned specialist work now defaults to child delegation while same-execution handover is reserved for true live-session continuity only.
The active LLM routing path also preserves provider `finish_reason`, applies bounded provider-local retries for documented non-stream token truncation, rejects still-truncated structured responses before parsing, and checks HTTP status before consuming streaming byte streams (returning `LLMError::Provider` on 4xx/5xx). Prompt-projection/browser-result chunk dumps now stay at debug level instead of info by default. Runtime-context prompts carry bounded recent assistant/tool history plus compact ledgers, and large explicit browser observations are compacted so prompt-facing history keeps scroll/reveal breadcrumbs plus bounded actionable samples without replaying raw screenshot-heavy payloads. Page-shape grounding honors observe-time surface-ownership hints for mixed DOM-over-canvas pages, while the `spatialSurfaces` wire contract explicitly accepts rendering kinds like `canvas_2d`, `webgl`, `webgpu`, and `dom_custom` end-to-end.
Operator settings now also support live reload of the active `magician-config.yaml`: `POST /api/magician/v2/settings/magician-config/reload` and the Unified UI Settings page can refresh the in-memory operation router, multi-LLM chat service, native-tool-calling policy, and tool-authorization policy without restarting the process, while still reporting restart-only config sections explicitly. Settings → On-device generation (`GET`/`PUT /api/magician/v2/settings/local-generation`) switches `runtime.ollama.local_generation.selected` among the kitty models, warns when the RAM-tier rule is violated, still allows the switch, and reloads Ollama. The historical nano complexity classifier is no longer part of the current runtime; outcome-, cost-, latency-, queue-, and safety-aware selection is tracked in the Adaptive LLM Routing design and implementation plan.
UI-backed runtime settings that write env/config files should use the centralized `magician_v2::runtime_settings` helpers plus feature-owned allowlists. The first user-facing consumer is VibeDev Deploy settings, which writes Cloudflare Pages credentials only to the active runtime env file (`.env.development` in development/debug, `.env` in production/release), rewrites only the top-level `vibedev_deploy:` block in the live config, and exposes the resolved paths in the VibeDev project settings panel.
Developer Mode / VibeDev Workbench CLI launch options are also server-owned config now: `interactive_process.operator_cli_programs` backs `GET /api/magician/v2/interactive-sessions/cli-runtimes`, so the UI does not hardcode operator CLIs; Pi remains internal to `run_coding_task`.
VibeDev's model picker is likewise server-owned. The active coding catalogue
now offers GPT-5.6 Terra, Sol, and Luna; DeepSeek V4 Flash and Pro; and MiniMax
M3. Kimi K3 is omitted until an actual router profile is configured, preventing
an attractive but non-runnable selector from reaching users.
Canonical HITL delivery is aligned across Magician `0.6.1018`, Unified UI
`0.0.673`, and Desktop `0.2.49`. Typed direct-open targets carry an exact
origin scope and source-owned response identifier; planning keeps workflow/task
response identity separate from durable planning-execution identity and binds
approve/reject decisions to the displayed plan version. Web and native callers
fail closed on malformed or cross-scope payloads, projected surfaces open the
shared review prompt instead of mutating approvals directly, and diff responses
reconcile against persisted transaction ownership. See
[HITL / Attention](components/magician/hitl-attention.md) and the
archived implementation plan.
Meta Harness / Autonomous OPC is now part of that active runtime contract as well: harness is a capability on `kind: personal`, `harness` defaults plus per-focus-area `schedule` / `program` / `scope` compile into stable scoped harness goals, harness read/action/evaluation tools run through the normal capability registry, structural agent changes stay proposal-backed and approval-gated, and the default dogfood topology now routes engineering oversight through the seeded CEO -> CTO harness chain rather than a separate special-case meta-agent loop. Relay (`harness-sre`) dogfoods that harness by auditing deterministic scheduler/anomaly/backlog health and delegating deep forensics to `internal-system-analyst` when operational evidence is needed. `program.md` / focus program documents remain durable user-authored specs; scoped local-file runtimes load active program specs from `scopes/<principal>/<workspace>/programs/`, while mutable OPC runtime state lives separately under `programs/state/*.json` with current phase, open loops, stop state, next-action hints, and update provenance. Harness tools can read/update that state and low-risk `program_state_update` learning candidates can apply to it through the same learning pipeline. The main human operating surface for that loop is the crew detail/operator view plus owner-briefing timeline in Unified UI.
The seeded default scope now also includes CMO and CRO personal-agent harnesses for marketing and revenue operating loops. These definitions live under the scoped agent-runtime root, carry their own focus-area schedules/programs, and delegate only to normal runtime agents rather than system agents.
Agent definitions can also carry `disabled: true` as a rollout/rollback control. The runtime treats the configured root plus reachable delegation descendants as unavailable for scheduling, manual triggers, planning, and delegation, while the crew surface renders the derived hierarchy and disabled status so operators can see the active topology without deleting definitions.
Reviewable user-memory learning candidates now project into the scoped feed as `learning_candidate` cards. The feed reconciles from `LearningStore` directly rather than only learning events, and Desk actions confirm, edit-and-confirm, or archive through the existing learning-memory bridge and candidate state machine. Desk feed admission is being tightened around durable user-facing cards — learnings, deliveries/routines, failed task outcomes, and completed task outcomes with summaries/artifacts/outcomes — while urgent approvals, user-input escalations, and running/paused task controls stay in Attention Bar. Published surfaces now emit `data_delivery` feed cards from `published_surface.changed` events, and scheduled task terminal executions emit `routine.result_published` into stable routine delivery cards; older active surfaces can be backfilled explicitly with `scripts/backfill_published_surface_feed_deliveries.py`. Generic assistant chat messages stay in chat instead of being duplicated as feed rows; legacy `agent_message` feed rows are removed by the explicit one-time `scripts/purge_legacy_agent_message_feed.py` maintenance script rather than by hidden feed-read cleanup.
Agent-growth learning now includes the Phase 3 memory bridge, Phase 4 skill/tool-pack evolution review chain, Phase 5 reusable skill/workflow candidate routing, Phase 6 structured eval-worker evidence, Phase 7 OPC program-state routing, Phase 8 explicit user teaching/correction, Phase 9 growth evaluation rollups, Phase 10 procedure storage, Phase 11 reusable procedure extraction, Phase 12 active procedure retrieval/prompt injection, Phase 13 procedure feedback/update/deprecation, Phase 14 procedure-to-skill promotion routing, and Phase 15 procedure quality evaluation. Skill Evolution evidence now also records compiled skill/tool calls, primitive CLI-template tools, browser primitives, direct native file/http/shell actions, and chat runtime tools under `learning/skill_invocations/dt=YYYY-MM-DD/<invocation_id>.json`, appending compact `skill_invocation_*` learning events with redacted input-shape fingerprints, structural result summaries, failure classes, and task/execution/chat provenance. Repeated failures cluster under `learning/skill_invocation_failure_clusters/` and high-confidence clusters route one review-gated Skill Evolution backlog item through the existing bridge. Skill Evolution backlog routing now fingerprints equivalent candidates, merges open duplicates into the canonical backlog item, supersedes duplicate source candidates with decision-log provenance, ranks queue actions, and exposes owner hints in the `/memory` panel. The existing API/storage substrate is still named `capability_evolution`, but the current runtime surface is scoped `skills/<skill>/...`, not a separate legacy `capabilities/` tree. Terminal task episodes, agent-cycle episodes, and meaningful chat turns can trigger a best-effort, schema-bound `learning_reflection` operation that writes audit events and learning-candidate proposals. Direct user instructions such as `remember`, `forget`, `correct`, `make_reusable`, `improve_tool`, `never_do_this`, `this_was_useful`, and `this_was_wrong` now flow through `POST /api/magician/v2/learning/teaching` or the chat-runtime `record_teaching_feedback` tool, producing the same learning events, candidates, and bridge routing instead of remaining only as transcript text. Reflection-created memory fact/preference candidates are routed immediately; only explicit low-risk user memory requests/corrections can auto-promote into `memory/users/knowledge.json`, while inferred, risky, agent-level, or malformed candidates stay review-gated. Reflection-created `memory_procedure` candidates route to draft procedure YAML under `learning/procedures/draft/` after dedupe against existing draft/active procedures, while reviewed promotion merges updates, activates draft procedures, and can apply reviewed deprecation proposals; deprecated/archived procedures are not reused as live dedupe targets. Reusable procedures therefore do not get buried in semantic memory or mutate skills directly. Active procedures are retrieved beside memory and injected as bounded task/chat/inner-loop prompt guidance with rationale events, including zero-selected audit events when candidates were withheld; retrieved procedures now receive post-use feedback that updates `last_used_at`, evidence refs, and source links; success/failure counters move only from structured procedure judgements or targeted correction, and repeated judged failures or explicit negative user correction can deprecate them so they stop being injected. Repeatedly successful active procedures can now create review-gated `workflow_template` skill-promotion candidates and paired `evaluation_case` backlog entries through the existing capability/eval bridges; source procedures remain active and keep provenance to promotion candidates. Growth-evaluation reports now distinguish memory recall from procedure reuse by scoring procedure extraction precision, retrieval relevance, misuse, helped/hurt signal, stale correction, duplicate active procedures, and procedure-to-skill promotion quality. Reflection-created `evaluation_case` candidates are written to `learning/evaluations/backlog/<candidate_id>.json` with optional meta-harness diagnosis fields; eval-worker run reports live at `learning/evaluations/runs/<candidate_id>/<run_id>.json`; Phase 9 growth-evaluation reports live at `learning/evaluations/growth_runs/<run_id>.json`; reflection-created `program_state_update` candidates can update `programs/state/*.json` only when low-risk and review-free or explicitly promoted by a reviewer; reflection-created `capability_update`, `tool_schema_update`, and `tool_wrapper_fix` candidates are written to `capability_evolution/backlog/<candidate_id>.json`; and reflection-created `skill_update` / `workflow_template` candidates use the same backlog when the proposal is about skill artifacts rather than a draft procedure. Reviewable skill/tool-pack fix/eval plans live separately at `capability_evolution/proposals/<candidate_id>.json`, proposal-generated eval cases live at `learning/evaluations/backlog/<candidate_id>.json`, validation evidence lives at `capability_evolution/validations/<candidate_id>/<validation_id>.json`, implementation bundles live at `capability_evolution/implementations/<candidate_id>/<implementation_id>.json`, dry-run/applied file-change records live at `capability_evolution/applications/<candidate_id>/<application_id>.json`, bounded steward run reports live at `capability_evolution/steward_runs/<run_id>.json`, and promotion evidence is appended to `capability_evolution/promotion_audit.jsonl`. Proposal drafting and review are separate surfaces: the draft/upsert path cannot set terminal statuses, while `/api/magician/v2/learning/capability-evolution/proposals/{candidate_id}/decision` records approved/rejected/superseded/archived reviewer decisions with provenance and synchronizes proposal, backlog, and candidate state; scoped skill approvals require non-empty `eval_plan` and `promotion_gate` fields, and high/critical-risk approvals also require explicit review evidence. The proposal drafter can seed queued backlog items with reviewable plans and full-file skill-guidance patches, including AgentSkills frontmatter for brand-new reusable workflow skills; `/evaluation/generate` can materialize a proposal `eval_plan` for meta-harness review, and the validation runner can ensure that eval backlog item exists before executing approved proposal commands under the scoped workspace root with regression evidence when requested. `POST /api/magician/v2/learning/capability-evolution/steward/run` runs one conservative steward cycle that drafts safe proposals, generates evals, runs only approved low-risk allowlisted local validations, prepares reviewable implementation bundles and scoped dry-run application records, and reports review/apply/promotion gates as attention-required actions without approving, applying, or promoting by itself. Approved proposals can then receive pass/fail/blocked validation reports; passing validation now requires material evidence and moves the backlog to `validated` and the candidate to `evaluated` without applying files. Implementation bundles bind approved proposals and passed validations to concrete file/patch payloads for inspection; application records can dry-run or apply those full-file patches to the scoped, system, or source skill surface through an explicit `target_surface`, and applied scoped/system skill records include runtime catalog visibility metadata so operators can see post-apply skill discovery, and promotion refuses applied records whose post-apply discovery failed. Promotion recording may reference either the bundle or an applied application record, but any promotion whose proposal patches/proposed files, implementation patches/applied files, or promotion applied files target `skills/` paths must reference an applied application record covering at least one of those skill targets before the backlog/candidate can be marked implemented; proposals whose `promotion_gate` requires regression checks also need validation evidence that regression ran. These substrates are exposed through the learning API, `internal_data`, and the `/memory` skill-evolution panel, which now includes ranked backlog recommendations, priority reasons, owner hints, explicit teaching, recent-candidate review, procedure review, procedure-to-skill promotion, and the Phase 9 growth eval runner; proposal/validation/implementation records remain inert until the explicit apply endpoint is called.
Phase 4 Skill Evolution drafting adds `/api/magician/v2/learning/capability-evolution/proposals/{candidate_id}/implementation/draft` for approved low-risk scoped `SKILL.md` guidance, `tool_schema.yaml`, and wrapper script proposals. Proposal drafting scaffolds schema/wrapper full-replacement patch metadata from observed failure evidence plus local artifact metadata with `metadata.reviewed_patch_content=false`; implementation drafting refuses those artifacts until a reviewer confirms `metadata.new_content` and sets the marker to `true`. It creates reviewable implementation bundles from validated full-replacement proposal patches, stamps exact target-file, artifact-kind, rollback, docs/changelog/version, and review-before-apply metadata, and leaves source-skill and non-low-risk implementation bundles on the manual review path.
Phase 5 Skill Evolution validation drafting now seeds `validation_plan.phase5_generated` with target-specific command suggestions for scoped skill changes: AgentSkills frontmatter validation for `SKILL.md`, scoped `tool_schema.yaml` shape validation through `scripts/validate_skill_artifact.py`, Python wrapper syntax checks, wrapper `--help` regression smoke commands, scoped skill-local Python unittest discovery or shell test scripts when `skills/<skill>/tests` already exists, and plan-only `fixture_cases` with schema-shaped sample inputs when real failure evidence exists. The fixture validator can confirm the wrapper/schema pair can produce a redacted regression fixture without running arbitrary skill code; validation reports and evaluation run payloads now record fixture cases, command-result coverage, and covered observed failure classes. The existing validation runner and steward still apply regression filtering and local command allowlists before executing anything.
Phase 6 Skill Evolution application records now add `payload.phase6_rollback_snapshot` with per-file restore/remove actions, previous/new content hashes, byte sizes, and restorable-file counts while keeping full bodies in `changed_files`; runtime catalog refresh payloads also include `changed_skill_discovery` so operators can see whether each changed skill was rediscovered, which manifest path won, and which changed skills were missing before promotion. Phase 6 rollback recommendations are recorded under `capability_evolution/rollback_recommendations/<candidate_id>/<recommendation_id>.json` when catalog refresh fails after apply or a later validation failure/blockage can be tied back to an applied change, project into Attention as Skill Evolution escalation rows while `recommended`, and can be reviewed, dismissed, or superseded from the Skill Evolution dashboard.
Phase 7 Skill Evolution post-promotion monitors are recorded under `capability_evolution/post_promotion_monitors/<promotion_id>.json` when promotions are recorded or rerun from `POST /api/magician/v2/learning/capability-evolution/promotions/{promotion_id}/monitor`; monitors compare bounded skill invocation evidence before and after promotion, list success-rate and failure-class deltas, surface in the Skill Evolution dashboard, create rollback recommendations or review-gated follow-up `skill_update` candidates when regressions are detected, project unresolved monitor-only regressions into Attention, and feed `post_promotion_stable_promotions` plus `post_promotion_regression_rate` dimensions into growth evaluation.
Agentic execution now uses the runtime-context harness directly. The inner loop no longer creates `taskplan_live.md`, taskplan revision sidecars, projected plan summaries, or `TaskPlanUpdated` transport updates; prompt projections are stored under the owning execution as prompt dumps for debugging, while progress and evidence live in canonical execution history, runtime context, artifacts, and assistant/tool events. Inner-loop prompts and outer history carry explicit parent outer iteration plus inner run ids, so local inner iterations do not appear as a flat continuation of outer iterations. Successful inner-loop terminal outcomes are objective-scoped, so repeating the same completed browser objective feeds prior evidence back to the outer loop instead of restarting the browser task.
Task decomposition is optional outer-loop state. Preplanning still uses `PlanGraph` for user-facing planning and edits, but approved plans are supplied to execution as advisory context rather than converted into mutable markdown taskplans.
GAUI/MUIJ layout snapshots are also scoped runtime state: live `ui_layout.muij.json` files belong under `$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/agent_runtime/agents/...`, while `magician_data_v3/system/agent_templates/...` stays template-only. Ownership handovers and direct agent overrides now resolve agent definitions inside the active execution scope, and scoped definition reads re-stamp `principal/workspace` ownership even for older on-disk YAML that predates those serialized fields. The direct override contract includes normalized per-agent operation routing, and both V3 task start and resume use that shared scoped policy before applying task-only controls; a task-backed specialist therefore cannot silently fall back to the global operation profile. Scheduled cron dispatch follows the same rule: due triggers now carry their owning `principal/workspace` from the scoped scheduler state instead of rediscovering scope through a global definition lookup.
Impossible or invalid cron expressions now keep scheduler entries dormant with `next_run_at = null` instead of silently normalizing to a fallback hourly fire. Recovery rewrites legacy impossible entries to that disabled state so intentionally impossible schedules do not launch autonomous tasks.
Multi-turn task refinement is now part of the active runtime contract: chat injects sanitized recent-task metadata for the current `ui_thread_id`, `refine_task` rides the normal V3 execute path with an execution-local refinement overlay, and tasks now carry a persisted `output_mode` contract. `accumulate` is the default task behavior and projects each run's captured files and primary finalized outputs into task outputs with a root-execution suffix so prior reruns stay visible side by side even when delegated child executions contribute files; `overwrite` keeps stable task-level output ids and filenames so the newest run replaces the projected copy and stale accumulated primary task outputs stop surfacing through the task output listing. `refine_task` executes with `overwrite = true`, while execution-scoped history remains immutable in both modes. Prompt seeding now reloads prior task outputs from the task's earlier execution tree, including delegated child outputs where applicable, while synthesis/finalization consume typed `tool_output_file` metadata instead of reparsing raw artifact JSON.
Progressive task continuation now uses explicit task references plus compact continuation-context outputs. Chat, harness, and V3 task creation accept optional `reference_task_ids` for follow-up work, validate that referenced tasks are completed in the current scope, and map them onto the Artifact V2 linked-task rail. Terminal task finalization writes an agent-facing `role=continuation_context` JSON output that indexes summaries, output refs, captured artifacts, bounded tool-event anchors, and exact read/download paths; successful tool actions also persist redacted `tool_call_evidence` artifacts so prior SQL, scripts, command parameters, API/tool inputs, and bounded previews are discoverable without injecting full traces into later prompts. `get_task_details` surfaces those continuation outputs/previews first, so normal chat can answer result follow-ups or create a linked continuation task without rerunning the original work.
Rich chat is now part of that same active contract: chat sessions can stage attachments into scoped session-local output storage, persist structured user turns plus rich tool results, serve those files back through safe inline/download responses, and project followed task progress into grouped task/run cards on web chat surfaces without changing the raw per-message channel delivery model. Rich file cards now also preserve the original absolute save path when available, show that full path in the web UI, and expose a safe `open-folder` reveal action that accepts either scoped output roots or already-persisted session-referenced external output paths. Provider-native continuation state is preserved where needed (Anthropic tool replay, OpenAI Responses continuation, Gemini multimodal function replay), and normal chat ask-mode now reaches media and expression work through the personal-agent runtime plus the direct `image-generation`, `video-generation-via-veo`, `gif-search-via-klipy`, and `meme-generation-via-imgflip` capabilities instead of YAML chat wrapper tools. Legacy `tool_call_proposal` records, proposal confirmation callbacks, and proposal cards are no longer part of the active chat/channel contract; runtime actions are execution-owned and channels render text, executed-tool summaries, and task status. The image capability supports Nano Banana 2 Lite (`gemini-3.1-flash-lite-image`), Nano Banana 2 (`gemini-3.1-flash-image`), and Nano Banana Pro (`gemini-3-pro-image`) with `quality_tier: auto|fast|balanced|pro`; the video capability uses the direct Gemini API Veo preview models (`veo-3.1-fast-generate-preview`, `veo-3.1-generate-preview`) with `quality_tier: auto|fast|balanced`, long-running `generate_videos(...)` polling, and local MP4 downloads. Tier/model choices now live in capability descriptions, guides, and parameter docs instead of hidden runtime heuristics or chat-specific wrapper descriptions. Setup for the shipped Veo path is scope-local and API-key-based: provide `VEO31_API_KEY` (or the declared alternative `GEMINI_API_KEY`) as a scoped secret — the vault, or the skill's private `$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/skills/video-generation-via-veo/config/.env` bridge, ensure `python3` and `google-genai` are available, and do not provision Vertex AI, ADC, or GCS just to use the default `video-generation-via-veo` flow. Both packs use a 1200-second outer watchdog, both Python helpers set slightly shorter internal Google GenAI HTTP timeouts, and tool execution logs now emit explicit started/finished/failed timing with elapsed milliseconds plus direct provider/save timing from the Python helpers themselves. The chat profile chooser now hides OpenAI `openai_api_mode: chat` profiles that cannot safely replay rich-chat state while exposing warnings for those hidden entries. On the web chat surfaces, freeform user/assistant/system text now renders through the shared safe Markdown component instead of raw plain text.
Unified UI chat routing now uses one canonical `ChatPanel` implementation for
`/chat`, `/hud`, and the `/t/[name]` Chat tab. Thread routes still own their
Tasks and Settings panes, but the old route-local thread chat composer,
message-rendering, voice, escalation, and workbench branches have been removed.
Tool-authorization and sandbox-override escalations are now part of that same scoped runtime contract: scoped executor runs route those prompts through the shared `UserRequestService`, transport emits `user_request.pending` / `user_request.resolved` facts for cross-surface delivery, and all service-backed web responders submit to canonical HITL at `/api/magician/v2/hitl/{correlation_id}/respond` with their workspace-bound bearer and the matching `source` (`"user_request"` or `"approval"`). The older `/api/magician/v2/user-requests/{id}/respond` and `/api/magician/v2/approvals/{approval_id}/resolve` URLs return `410 Gone`; the public `agentic-resume` path validates that the stored pause scope matches the bearer scope before resuming execution-owned pauses.
API-mining live browser-to-API takeover also follows that scoped HITL contract. Read replay can run automatically when origin policy allows it, but write replay under `replay_writes_with_hitl` emits an `api_replay_approval` user request with a redacted request preview and executes the already prepared request once only after approval. Denial, timeout, or missing HITL service stops before the equivalent browser mutation, while direct manual capability/workflow replay endpoints enforce origin policy and block write replay that requires HITL because they do not own a resumable execution context.
Compiled API-mining workflows are also consumed from the live browser primitive path when a conservative first-step capability match exists: Magician replays the workflow API prefix, records mixed-mode sequence metadata, and dispatches any typed browser fallback through the active `agent-browser` session rather than a separate replay rail.
API-mining registry health now also reports config/policy warnings for disabled or dry-run replay states, direct workflow replay can hydrate explicit `SessionAuth` params from captured session context without exposing those values in observability payloads, and the local fixture harness covers warm replay, replay-failure fallback signaling, and dependent search-to-detail workflow replay.
Task-backed execution control now also persists canonical V3 status immediately on the active path: execution-owned `agentic-resume`, continuation, direct status updates, and chat-side `stop_task` cancellation write back the resulting task/execution projection, `PlanningComplete` normalizes to `ready`, non-active root states clear `active_root_execution_id`, and resumed runs rebuild the scoped merged tool surface instead of falling back to `0 tools`. Active cancellation propagates through outer decision/action awaits and nested browser/YAML inner-loop awaits, so stopped work does not keep issuing LLM calls behind the cancelled task.
The default seeded `anonymous/default` personal-assistant runtime definition now carries `onboarding_completed: true`, aligning the materialized default scope with the server-authoritative onboarding model. Consumer-channel bot templates also now keep ordinary retry/reconnect/listening/auth-guidance lines on stdout/info at the source, reserving stderr/error rows on `/presto/bots` for genuinely error-like conditions.
Task-backed executions also now treat `task_state` as a platform-owned runtime capability rather than an agent-YAML opt-in. The `task_state` pack remains the source of truth for description, guide, parameters, and compiled provider binding, but the runtime injects it whenever a real `task_id` exists, hides it for non-task runs, and preserves it across pause/resume continuation plus owner/delegation profile reloads.
Core utility tools are now visible across agent-filtered catalogs unless explicitly excluded. The default core utility set includes `time_math`, a compiled deterministic calendar/timestamp provider for current time, half-open date ranges, Unix boundaries, and Apple-epoch nanosecond SQL predicates. iMessage reads now use a compiled read-only SQLite provider over `~/Library/Messages/chat.db` through bundled `rusqlite`, with legacy `sqlite_scan(..., '<table>')` wrapper normalization for older prompts.
Execution-native tool calling is now the only outer decision rail. Browser automation and command-backed domain capabilities are exposed as inner-loop capabilities: the outer loop selects the capability, while the focused nested loop loads that pack's guide and primitive schema. Browser drives the pinned `agent-browser` CLI; generic command packs drive scoped CLI/Python helpers through the shared CLI-template dispatcher. The current browser driver is `0.38.1-Magician.0`, rebased on upstream `v0.38.1`; its README and accumulated source patch are tracked at `skillshub/browser/_vendor/v0.38.1-Magician.0/`, while native binaries are gitignored build artifacts. It adopts upstream agent-oriented reads, accessibility audits, session/restore lifecycle, renderer recovery, WebGPU, and strengthened allowed-domain containment while retaining Magician's browser-root download setup, 60-second socket margin, event-plus-stable-filesystem completion, JS user-gesture click, `close --keep-browser`, file-chooser interception, direct CSS iframe-ID resolution, CloakBrowser exit-code diagnostics, and scoped bundled-skill discovery. Magician explicitly disables upstream's new default headless idle shutdown unless an operator or engine resolver supplies `AGENT_BROWSER_IDLE_TIMEOUT_MS`. On a clean clone, `make setup-agent-browser` rebuilds from the pinned tag + patch, then deploys exact copies of the ad-hoc-signed arm64 artifact over the stock npm install and into the skill source; `make verify-agent-browser` validates its version, signature, and bundled core-skill resolution through a fresh copied mirror. The Docker build uses Rust 1.92 to compile and verify the same patch for native Linux. See the vendor README for the retained-delta audit, rebuild instructions, and upstream-bump checklist; the download behavior remains sibling to upstream issue #1300.

The Metabase capability is now a single `metabase` pack driving `metabase-pp-cli`, a Go CLI generated by [Printing Press](https://github.com/mvanhorn/cli-printing-press) (pinned at v4.0.3) from a curated 27-operation OpenAPI subset at `skillshub/metabase/scripts/spec.json`. Replaces the previous Specli + 3-wrapper + 3-pack stack: one binary, one capability pack, no shell wrappers — auth is just `METABASE_BASE_URL` + `METABASE_API_KEY` in the scope's `.env` (PP reads them directly). The pack invokes the binary with `--json --no-input --no-color --yes` baked into its command prefix. Setup: `make setup-printing-press` (installs go1.23.0 toolchain + the generator) followed by `make regen-metabase-cli` (generates Go source from `spec.json` and builds the CLI; binary is gitignored). `regen-metabase-cli` reads only the committed `spec.json` — no live instance needed. Spec refresh when Metabase upgrades: `make refresh-metabase-spec` re-fetches `$METABASE_BASE_URL/api/docs/openapi.json`, runs `postprocess-spec.py` (operationId rewrites + drop unused mutations + relax required-with-defaults), and writes the new `spec.json`. The pack guide teaches the `find_search` → `card_get` → `card_run`/`card_export csv` → fall back to `dataset_query --stdin` workflow. Ad-hoc SQL bodies pass via `--stdin` to eliminate the JSON-shell-escape pain that plagued the Specli flow.
Veil's SOTA canvas/spatial fixtures are page-owned applications that initialize
their visible canvas/SVG/iframe targets on load; reset controls must keep the ids
used by the fixture scripts so missing-element errors cannot defer target
visibility until the first click.

## Setting Up

- [Guided TUI installer](components/magician-setup/README.md) — capability-first setup, machine probes and current installer boundaries
- [Keys and paid services](setup/api-keys.md) — model providers, research, media
- [Channels](setup/channels.md) — WhatsApp, Telegram and email, yours or the agent's own
- [Connecting a Google account](setup/google-workspace.md) — Cloud project, OAuth, Pub/Sub
- [Building the phone apps](setup/mobile.md) — Android toolchain, Xcode, devices

## Start Here

- [Quick Start](quickstart.md)
- [Ecosystem Map](ecosystem/README.md) — product pillars and their authoritative docs
- [Feature Map](features/README.md)
- Live Thinking Map — 100X Spoken Thoughtspace Design (implementation plan)
- Unified Agent Tool Visibility, Delegation, And Invocation Authorization Design
- [Architecture V2](ARCHITECTURE_V2.md)
- MagicVault extraction plan — archived Magician extraction record; Magician consumes pinned MagicVault core/primitives
- [Chat Mode Architecture](components/magician/chat-mode.md)
- Unified Tool-Result Projection And Staged Context Retrieval
- [LLM Training-Data Observability](components/magician/llm-training-data-observability.md)
- LLM Training Data And Observability Plan
- Adaptive LLM Routing Plan
- [Ollama Logical-context Chunking](components/magician/ollama-logical-context-chunking.md)
- iOS Embedded SilverBullet WebView Plan
- [Consumer Channels](consumer_channels.md)
- [Deployment](DEPLOYMENT.md)
- Magician Presence, Mascot, Voice, And Screen Context Plan
- [Testing](testing.md)
- [Project Versions](project-versions.md) — the `magician` version contract,
  every Rust crate version, and versioned product surfaces
- [Project-wide Dependencies](dependencies.md) — package manifests, committed
  resolution files, local inspection commands, and current lock gaps
- Retired Canvas Mode Execution Board
- Canvas Mode Arbitration Design
- [Code Graph Explorer and MCP](codegraph/README.md) — C4, 2D/3D exploration,
  search, impact, flows and coding-agent code intelligence
- [Documentation Governance](process/documentation-governance.md)
- [AI Docs Hook Setup](process/ai-doc-hooks.md)

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
runtime. It runs **first** in `setup-all`, before `make -C skillshub setup-all`:
skillshub's chain includes `setup-media-fetch`, which symlinks the host
`ffmpeg`/`ffprobe` into `skillshub/media-fetch/bin/` and exits non-zero when
they are absent. That chain is `&&`-linked, so an ffmpeg installed later in the
target would be installed too late — on a fresh machine skillshub setup would
abort first and nothing after it would run.

Automation note: use `make event-taxonomy-codegen` after editing the `taxonomies!` macro in `magician-event-taxonomy/src/lib.rs` to regenerate `ui/unified-ui/src/lib/realtime/event-taxonomy.ts`; `make event-taxonomy-check` is the drift gate (wired into build/test) that fails CI if the TypeScript mirror falls out of sync with the Rust source. Use `make graph-index`, `make graph-contracts`, `make graph-all`, `make graph-check`, and `make graph-serve` for codegraph workflows and prefer documented Make targets over ad-hoc commands. `make test-storage-gate1` runs the disposable Decision Gate 1 spike (synthetic SQLite; optional `MAGICIAN_GATE1_POSTGRES_URL`). `make test-storage-s3` runs dormant S3 object/dataset adapter tests against a hermetic in-memory backend. `make test-storage-state` runs dormant SQLite/Postgres repository and SQL-lease tests. `make test-storage-migration` runs the dormant owner-closure and migration coordinator (synthetic owner; not default startup). The root `Makefile` prefers `/Volumes/build/magician/builds` when that location is writable and otherwise exports a checkout-local, gitignored `target/` path for Makefile-driven Rust checks, builds, clippy, docs, tests, and Tauri builds; raw `cargo` commands should use the target directory printed by `make help` when cache placement matters. Override `CARGO_TARGET_ROOT` or `CARGO_TARGET_DIR` to select a different location. That directory's `*/incremental` caches are shared by every agent and worktree on the volume and do not bound themselves — they reached 368 GB once and 94 GB again, and a full volume fails every build with `No space left on device`, which reads like a compile error. `make prune-incremental` prunes them oldest-first to `INCREMENTAL_BUDGET_GB` (40) and never removes one another process has open (`DRY_RUN=1` to preview; `CRATE=<name>` clears a single crate's caches, which is the recovery after an `internal compiler error: incremental compilation error`). `make check-build-space` reports free space and fails below `INCREMENTAL_MIN_FREE_GB` (60) so a build stops with the real reason instead of dying of ENOSPC mid-link. The larger leak is not incremental at all: every build of a workspace crate whose dependency or feature hash changed leaves its previous artifacts under `deps/` — 32 generations of the 850k-LOC `magician` crate at ~10 GB each were 339 GB on 2026-09-19, beside a 36 GB incremental cache — and `make prune-incremental WORKSPACE_ARTIFACTS=1` cargo-cleans every workspace member (current artifacts included; the next build recompiles the workspace crates, never the third-party graph), refusing while any rustc or cargo runs. Incremental is deliberately left on: disabling it would charge every debug rebuild of an 850k-LOC crate, and release builds do not use it at all. `make test-working-set-eval-harness` runs Boundary B's deterministic working-set suite plus the live evaluator's self-test, and `make test-working-set-live-eval` asks a model every planted probe from each lane's bytes and gates on correctness beyond and within the pack budget, judged quality and cost (`WORKING_SET_LIVE_EVAL_REPEAT`); see working sets. Desktop build, check, test, signing, and release targets invoke `make setup-desktop-pnpm`, which reads the exact `pnpm@…` pin from `desktop/package.json` and installs it under `.cache/desktop-pnpm` with npm when missing. This works without Corepack (including Node 25) and does not trust a drifting global pnpm; an explicit `make PNPM=…` override remains supported but must report the pinned version. `make run-supervisor-chat-trace` runs the supervisor with `MAGICIAN_CHAT_TRACE=1` and `RUST_LOG=info,magician=debug` for chat regression debugging — dumps every chat turn's prompt, LLM response, tool calls, and tool results under the session's trace dir. `make test`, `make test-verbose`, and `make test-capabilities` run through package-scoped targets where needed; `make test` and `make test-verbose` inject dummy API keys to prevent macOS Keychain prompts during CI/test runs and cap Rust test harness parallelism with `RUST_TEST_THREADS=4` while also raising the open-file soft limit via `ulimit -n $(TEST_FD_LIMIT)` (default `16384`, falling back to `4096`) by default to avoid macOS file-descriptor pressure in the large Magician suite — macOS ships a 256 `maxfiles` soft limit that even a 4-thread run exhausts once tests open DuckDB db+WAL+lock handles, surfacing as intermittent `Too many open files (os error 24)` (EMFILE) failures in otherwise-unrelated tests (`ui_threads`, `analytics`, …); `make test-capabilities` validates checked-in capability YAML through the Magician test package. Provider-free offline-audio evaluator regressions are also part of `make test`; the separate `make benchmark-media-offline-audio` and per-stage VAD/STT/TTS/diarization targets own a loopback FluidAudio sidecar and never start Magician. `make build-macos-speech-helper` (debug; `-release` too) builds `native/macos-speech-helper` and stages `./magician-macos-speech-helper.bin` and `./magician-macos-meet-audio.bin` — the Apple Speech helper behind the desktop Speech permission, `/host/speech/transcribe` and `macos_tts`, and the meet-audio tap behind the meeting bridge; `build-all-debug`, `build-all-release` and `dev-desktop-build` include it, and `build-desktop-tray-debug` builds it first so `materialize-desktop-debug-app.sh` can stage it inside the debug bundle beside the tray binary. The runtime build/test flow is decoupled from skill-side setup: `make build-all-debug` builds the Rust debug binaries, graph index, unified UI production bundle, Tauri tray/gateway, without chaining into `setup-bots` / `setup-capability-tools` / `setup-metabase-cli`; `make build-all-release` remains Rust release binaries plus `graph-index`. `make build-marketing-site` and `make publish-marketing-site` build and deploy the standalone public static landing page to Cloudflare Pages. `make test` and `make test-verbose` keep one inline guard — `$(MAKE) -C skillshub verify-agent-browser` — because npm install during normal dev work can silently replace the patched fork (`agent-browser 0.38.1-Magician.0`) with stock 0.38.1, which has the 6 documented download-flow bugs that hang browser tests against Chrome 148. The gate checks the version and signature, constructs a fresh copied mirror, then calls `skills get core --full` with no discovery override; failure prints a one-line "stale binary" hint pointing at `make -C skillshub setup-agent-browser`. All other skill artifacts (bot bundles, marimo venv, metabase-pp-cli) are owned by `make setup-all`, where each `setup-*` target self-validates after install. Run `make -C skillshub setup-all` to bootstrap everything skill-related; reach for surgical refreshes via `make -C skillshub setup-bots|setup-marimo|setup-metabase-cli|setup-agent-browser|setup-kapso-cli`. `make -C skillshub setup-kapso-cli` (also chained into `setup-all`, plus a root `make setup-kapso-cli` passthrough) verifies the exact-pinned Kapso WhatsApp CLI backing the `kapso-whatsapp-read`/`kapso-whatsapp-send` skills (Presto's own WhatsApp identity, distinct from the user's WhatsApp-Web `whatsapp` tool). The package is installed once under `skillshub/node_modules/.bin` by `setup-deps`; every workspace reuses that immutable executable while credentials, work directories, and writable CLI homes remain scope-owned. Node dependencies for tools and bots live in the skillshub root npm workspace (`skillshub/node_modules/`, installed by `make -C skillshub setup-deps`); the legacy scope-owned `<scope>/capabilities/node_modules/` tree is retired and `make -C skillshub clean-legacy-scope` removes it. `make setup-bots`, `make build-bots`, and `make check-bots` build against that workspace and require the project-local Node pinned in `skillshub/.node-version` (`24.20.0`, unpacked under `skillshub/.node/` by `make -C skillshub setup-node`). `make -C skillshub setup-python` (the shared `skillshub/requirements.txt` venv) is also the Python install path for the shipped Gemini media packs: it installs `google-genai` (plus `Pillow`, still needed for Nano Banana) for both `image-generation` and `video-generation-via-veo`. For local desktop update testing, use `make dev-desktop-setup`, `make dev-desktop-build`, `make dev-update-server`, and `make dev-container-rebuild`. `make run-supervisor` now boots the supervisor/services on ordinary Rust/Tokio stack defaults and removes inherited `RUST_MIN_STACK` values, and `make tail-magician-v2-log` tails the live `magician.log` stream into `magician_v2.log` for focused V2 tracing. `make run-all` starts the Vite UI dev server, Kapso Cloudflare Tunnel, codegraph server, magic-supervisor stack, and host tray/gateway in one foreground terminal so all logs stream together; `make stop-all` tears those local processes down (the Cloudflare Tunnel is a persistent `brew services` daemon left up across restarts — `make stop-tunnel` takes it down). `scripts/install-prerequisites.sh` now installs `cloudflared` alongside other dependencies. `make skills-validate`, `make skills-list`, `make skills-list-source`, and `make skills-install-scope SCOPE=<id>` (alias `scripts/magician-skills <subcommand>`) are the canonical entry points for managing AgentSkills v1 skills installed under `$MAGICIAN_ROOT_DIR/scopes/<scope>/skills/` from the source folder at `skillshub/`. `make setup-all` now runs `make -C skillshub setup-all`, which ends in `install-scope SCOPE=anonymous/default` (override with `SCOPE=`), so canonical setup populates the runtime tree before personality lookup or skill activation runs. The agent-browser vendored binary moved from `vendor/agent-browser/` to `skillshub/browser/_vendor/`; `make setup-agent-browser` mirrors the patched binary into `skillshub/browser/bin/` and the legacy npm-deployed path simultaneously, so existing executor flows keep working until full dispatch swap (5b-3) lands. `make setup-pi-coding-agent` idempotently reconciles the Pi CLI to the reviewed 0.87.1 pin backing `run_coding_task`; it is chained into `make setup-all`.
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

Stack-safety note: `make run-supervisor`, `make test-rust`, and the composed test
runner remove inherited `RUST_MIN_STACK` values and use ordinary Rust/Tokio
stack defaults. Agentic execution and resume are lazily constructed on a
dedicated runtime, external JSON/accessibility traversal is iterative and
depth-bounded, and `make test-agentic-default-stack` adds the 1,000-iteration
stress qualification. The incident-only
`MAGICIAN_EMERGENCY_RUST_MIN_STACK` override logs a warning; see [Runtime async
stack boundaries](components/magician/runtime-async-stack-boundaries.md).

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
flows share one local gateway contract.

Agent-definition automation note: the backend now caches parsed per-scope
definition lists and one-time built-in-template materialization, while normal
definition writes invalidate the affected scope automatically. The agent
scheduler runs on a 30-second cadence against that cache. After editing an
agent's YAML directly on disk, run `make refresh-agent-definitions` to call the
scoped refresh API, clear the shared cache, and eagerly reload all scopes
without restarting Magician. See the [agent definition reference](components/magician/agents/agent-definition-reference.md#runtime-definition-cache)
for overrides and the response contract.

Codegraph note: `make graph-index` is the full regeneration target for
`graph.json`, stats, payload profiles, behavioral contracts, dead-code
candidates, static test-coverage buckets, and a final filesystem/graph audit.
`make graph-contracts`, `make graph-audit`, `make graph-dead-code`, and
`make graph-coverage` remain available for surgical refreshes; `make graph-all`
is now an alias for the full index. `make graph-serve-stop` and
`make graph-serve-restart` manage the local codegraph explorer without tying up a
foreground shell. The right panel includes `Flow` + `Guide` tabs for simulation,
guided explain mode, runbook export/save, and guide auto-rebuild on artifact
changes.

UI dev note: `make run-ui-dev` runs the Vite dev server on
`http://localhost:5173` without touching the tunnel. `make run-all` starts that
server for the default full-stack local run and streams Vite logs beside
supervisor/tray logs; `make tail-service-log
SERVICE=stack|magician|magicutor|ui|host-tray` attaches to one
already-running service log, and `make open-service-log SERVICE=host-tray`
opens a macOS Terminal tail. `make preview-ui` builds and serves the production
bundle on `:5173`. Web app routes are desktop-only; mobile device work belongs
in the native iOS/Android applications.

Analytics note: `make analytics-export DB=<analytics.duckdb> OUT=<dir>` and `make analytics-import IN=<dir> DB=<analytics.duckdb>` wrap the `magician analytics export|import` subcommand (with `MAGICIAN_SKIP_KEYCHAIN=1`, so they run headless) for deliberate cross-version DuckDB migration — EXPORT with a binary built at the old `duckdb` pin, IMPORT with the new one. Normal version bumps need no manual step: the analytics event sink mirrors every batch to version-stable Parquet (`<scope>/analytics/events/dt=*/batch_*.parquet`), and an unreadable `analytics.duckdb` is loudly quarantined and rebuilt from that mirror. The full policy + procedure is `docs/runbooks/2026-06-13-analytics-duckdb-version-migration.md`. `make analytics-reprice-llm` wraps `magician analytics reprice-llm-calls` — a corrective recompute of historical `llm_calls` `cost_usd` against the active effective-dated pricing table; dry-run by default, `ARGS="--apply"` rewrites changed Parquet files atomically (details in `docs/components/magician/duckdb-analytics.md`, Pricing section).

## Component Docs (Project Layout Mirror)

- [Component Index](components/README.md)
- [Magician](components/magician/README.md)
- [Magicutor](components/magicutor/README.md)
- [MagicLLM](components/magicllm/README.md)
- [Magic Supervisor](components/magic-supervisor/README.md)
- [Tool Runtime Core](components/tool-runtime-core/README.md)
- [MCP Client Boundary](components/magician-mcp-client/README.md)
- [Runtime Core](components/runtime-core/README.md)
- Remix
- [Unified UI](components/unified-ui/README.md)
- [Capabilities](components/capabilities/README.md)
- [OfficeCLI Tool Skills](components/magician/officecli-tools.md)
- [Authentication and workspace isolation](components/magician/auth.md)
- [Scripts](components/scripts/README.md)
- [iOS Companion (Magios)](../magios/README.md) — native Today, chat, Attention, and Tutor surfaces. Run it on your phone/simulator with **`make ios-debug-run`** (build + install + launch on a connected iPhone, else a simulator; **`make ios-debug-build`** / **`make ios-debug-deploy`** are the build-only and install-only halves). Builds contain no customer host or Cloudflare credential: connect the installed app with a short-lived iPhone QR created in Magician Settings. Run unit coverage with **`make test-ios`** and the on-demand UI smoke lane with **`make test-ios-ui`**; the blocking unit lane uses the checked-in Xcode project, never invokes XcodeGen, gives each attempt an isolated Core Foundation home/temp/cache/DerivedData environment, and enters the logged-in user bootstrap when available. It retries only the proven pre-test host abort once (and suppresses that futile retry when no Aqua bootstrap exists), caps only Xcode's post-test simulator diagnostic collector at 30 seconds, serializes report publication, and atomically writes a current-run-only dashboard to **`coverage/ios/latest.html`**. Aggregate `make test` runs use a correlation manifest to retain their immutable run-specific iOS report even if a later direct run replaces `latest.html`. Assertion/test failures are never retried, and stale result bundles are never reused. One-time host setup: **`make ios-full-setup-debug`** (mobile Cloudflare Access, UI email Access, tunnel routing, and debug build) or **`make connect-access`** (the customer-owned device Access boundary used by iOS, Android, and ESP32). `make build-all-debug` also compiles Magios via `ios-debug-build` (installable); `make check-all` runs the Rust all-target check, Unified UI Svelte/TypeScript checker, and the fast `ios-debug-check` compile gate against the generic iOS Simulator destination with no signing or installed-device dependency. The compile gate places DerivedData under `$(CARGO_TARGET_DIR)/magios-debug`, so it does not fall back to the system Xcode cache or consume code-signing services. The iOS step skips gracefully when `xcodebuild` is unavailable.

Magios's ordinary `ios-debug-build` and `ios-debug-run` automation is compatible
with Personal Team provisioning and therefore omits APNs. Paid-team developers
can explicitly select the push-enabled Debug lane with
`ios-debug-build-push` or `ios-debug-run-push`.


Automatic database upkeep, status events, resource budgets and the focused
`make test-database-maintenance` verification lane are documented in
[Storage Governance](components/magician/storage-governance.md#automatic-channel-assist-and-feed-maintenance).

Local debug/check builds automatically prune incremental caches older than seven days when their profile exceeds `INCREMENTAL_BUDGET_GB` (40 GB default), under Cargo’s build lock. `make maintain-build-cache` runs this explicitly; caches belonging to active builds are skipped.

Decision Model metering (Jev/local models): `make test-decision-model-metering` verifies attempt receipts, pricing, canonical capture, and the `/llm` breakdown. See [structured decisions](components/magician/structured-decision.md#decision-model-usage-and-pricing).
