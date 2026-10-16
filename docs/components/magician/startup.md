# Runtime startup and readiness

Magician opens a small HTTP application after configuration validation and
storage ownership have been established. Core service construction proceeds
while that application answers. Static deployments display a responsive
startup page which opens the workspace automatically when initialization ends.

## HTTP contract

| Endpoint | During initialization | Once the full application is ready |
| --- | --- | --- |
| `GET /live` | 200, process HTTP liveness | 200 |
| `GET /startup` | 200, `status: starting`, `ready: false`, coarse phase | 200, `status: ready`, `ready: true` |
| `GET /api/magician/v2/startup` | Same public status, for web/native transports | Same public status |
| `GET /health` | 503 with `Retry-After: 1` | Existing aggregated health response |
| Feature API requests | 503, `code: service_starting`, `retryable: true` | Normal handlers and authorization |

Startup status contains no user, scope, credential, or filesystem information.
It also reports current vector availability, which can become ready later than
HTTP. A failed worker initialization reports `status: failed`; it never opens
the readiness barrier. Unexpected loss of the HTTP task releases readiness
waiters with an error, including cancellation before its first poll. Shutdown
reports `ready: false` and `status: stopping`. An operator stop during startup
exits normally after cleanup. Liveness is not a substitute for readiness.

The full application is installed on the existing Actix workers and listener.
There is no secondary port, reverse proxy, request replay, or listener rebind.
Original request bodies, peer addresses, headers and streaming transports reach
the original application. All workers finish application construction before
feature requests are admitted. With multiple HTTP workers, route construction
reserves one worker's capacity for startup requests while other workers build
their routes concurrently. A single-worker configuration must briefly finish
its route construction before it can answer again.

The web startup gate waits only for a recognized starting service. Missing
endpoints on older servers, static public hosting, and network failures retain
the existing login/offline behavior. Public marketing and notification-overlay
surfaces bypass the gate. The bootstrap API uses the existing CORS policy,
including its exception for the Apps owner channel.

## Initialization order

Environment and validated configuration are loaded before runtime construction.
The binary passes that same configuration snapshot into the service builder,
avoiding another environment/config initialization during parallel startup.

Ollama publishes its configured embedder handle synchronously. Daemon startup,
model verification, prewarm and subsequent monitoring run in one owned,
cancellable task. Vector availability stays false until warm-up succeeds;
consumers can retain the correct handle while it is warming. Missing models
are not downloaded or substituted during startup. CLI callers that explicitly
await the old `start()` API still wait for the boot attempt to finish.

Chat-store initialization, persisted user-request loading and system-package
admission overlap. Canonical journal recovery already runs independently beside
package admission. Host speech, automation and CUA probes also overlap. Their
results are installed before the corresponding feature handlers are exposed.

Package admission still finishes before any app-backed request or worker can
observe the registry. Journal capture, chat/event consumers, authority and
execution wiring still precede producers. The execution lifecycle latch is
released after full HTTP readiness; agent hydration, scheduler loops and
restart recovery retain their existing idempotency, lease and pending-state
rules. A starting feature must not be presented as an empty or missing store.

## Work admitted after readiness

- Enabled bots, FluidAudio model prewarm and optional dashboard seeding.
- Feed/UI-thread scope warm-up and declarative progress-subscription repair.
- Channel sync, distillation, classification, reconciliation and evidence,
  feedback and pattern bridges.
- Historical attention bootstrap, semantic extraction, rank recomputation,
  model training, resurfacing and observed-source scheduling.
- App projection/behavior workers and ambient distillation.
- Memory indexing, evaluation and utility maintenance. Configured startup
  delays begin after readiness, not during HTTP initialization.
- The initial retention/Parquet/transcript maintenance sweep, plus the existing
  automatic database-maintenance lifecycle.

Finite database warm-up, dashboard/progress backfills, initial memory indexing
and the initial storage-maintenance sweep share one admission slot. A permit
covers a finite pass, never the lifetime of a periodic worker. Request-triggered
database initialization remains protected by each store's existing scope and
migration locks. This defers background work; it does not remove migration or
recovery correctness barriers. The slot limits overlap between these startup
jobs; it is not a process memory budget. A single indexing or maintenance pass
can still allocate substantial memory, and normal requests and periodic work
have their existing concurrency rules.

Embedded/CLI users that do not install the HTTP startup barrier keep immediate
worker startup. Workers with their own shutdown token can cancel while waiting
for readiness. SIGINT, SIGTERM and SIGQUIT stop HTTP and release startup waits;
owned Ollama boot/health work is cancelled and joined before reaping its child.
Pre-existing Ollama daemons remain outside Magician's ownership.
The app-behavior supervisor waits at its ownership boundary; startup
cancellation releases that owner. Supervisor watchdogs observe process
cancellation even while core initialization is finishing.

## Verification

Use `make test-startup`, `make test-startup-ui`, and `make check-all`. Transport
tests exercise startup UI/status, mutation rejection before readiness,
authentication/body/query/peer preservation after publication, WebSocket
upgrades and frames, incremental SSE delivery, failed initialization, and
shutdown or loss of the server task before publication.

Performance measurements must distinguish first startup HTTP response, full
API readiness, optional provider readiness, and memory after background work
settles. Compare equivalent build profiles and observation windows; merely
moving a measurement earlier does not establish a memory improvement.

### Measurement context — 2026-09-24

Measurements and allocation evidence live in the
memory profile. Background workload (for
example a memory-index rebuild with uncached chunks) varies between restarts,
so equal elapsed windows do not mean equal work; the separate Ollama process's
model memory is outside backend RSS.

### Observed results

Serializing heavy first passes (one admission slot) brings startup HTTP up
well before full health, but it moves work later rather than reducing memory:
RSS can stay high after active work releases allocations (allocator
fragmentation and resident malloc regions). Warm restarts do not predict first
launch after rebuilding/signing, a cold boot, or a release build, which remain
unprofiled.
