# Magic Supervisor

`magic-supervisor` manages long-running workspace services.

## Managed Services
- `magician` (default port `3002`)
- `magicutor` (default port `3003`)
- `decision-engine` (optional; Unix socket, managed only when
  `decision-engine.bin` is installed — see below)

## Quick Start

```bash
make build-all-release
make run-supervisor
```

## Control Plane
The supervisor exposes a TCP command interface on `127.0.0.1:8081` (default).

Supported operator commands (via `supervisor-ctl`):
- `restart-magician`
- `restart-magicutor`
- `stop-magician`
- `stop-magicutor`
- `restart-decision-engine`
- `stop-decision-engine`
- `status-decision-engine`
- `status`
- `status-magician`
- `status-magicutor`
- `host-status`
- `host-presence-start`
- `host-presence-stop`
- `host-presence-restart`
- `health`
- `shutdown` (stops managed services and exits the supervisor process)

The `host-*` commands are thin proxy calls to `MAGICIAN_HOST_GATEWAY_URL`
(`http://127.0.0.1:3017` by default).
They let the runtime supervisor ask the host tray gateway for host-native
status or macOS presence-host lifecycle changes, but the tray gateway remains
the owner of native GUI processes.

## Magicutor defaults

Magicutor is launched as `./magicutor.bin` on macOS/Linux and
`./magicutor.exe` on Windows, with no arguments; server mode is forced through
the environment at spawn (`MAGICUTOR_FORCE_SERVER=1` /
`MAGICUTOR_MODE=server`). Its port resolves from `MAGICUTOR_PORT`, then CLI
args, then `3003`, and the health check hits that HTTP port with a 2 s TCP
fallback. Raw control-plane commands for it:

- `{"RestartMagicutor":{"args":null}}`
- `{"StopMagicutor":null}`
- `{"StatusMagicutor":null}`
- `{"HealthCheck":null}` (aggregate across both services)

Smoke check: start `magic-supervisor`, run `./supervisor-ctl status`, then
`curl http://127.0.0.1:3003/health`.

## Decision engine

The structured-decision engine runs as
`./decision-engine.bin` when that binary exists; without it the supervisor
logs once and manages nothing (`status` reports `not_installed`, and the
aggregate `health` does not count it). It starts before Magicutor and
Magician, so Magician (which asks only the engine) finds it from its
first step — and until it answers, Magician's step judges fall through to
the LLM.

For a paired decision-contract rollout, start the supervisor with
`MAGICIAN_SUPERVISOR_START_MAGICIAN=0 make run-supervisor`. It starts the engine
and Magicutor but holds Magician. Verify the engine's socket health, contract,
restrictions and active config, then run `make restart-magician` and check
Magician health. Normal startup keeps its short two-second head start and does
not wait for a local model to load.

- Socket: `decision_engine_socket` when set, else whatever the engine
  reports (`decision-engine --print-socket`). The engine resolves it as
  Magician resolves its default — `MAGICIAN_ROOT_DIR`, then
  `MAGICIAN_STORAGE_PATH`, then `~/MagicianNotes`, plus
  `/run/decision-engine.sock` — and is the only place that does: the
  supervisor never resolves the runtime root itself (typed-storage
  boundary; `decision-engine/src/main.rs` is the reviewed resolver). The
  path is passed back explicitly as `--socket` and health-checked there.
  The engine creates it owner-only.
- Settings: the engine reads `<root>/decision-engine.yaml`, else the
  `decision:` block of `<root>/magician-config.yaml`.
- Health: `GET /health` over the socket every 20 s
  (`decision_engine_health_check_interval`).
- Restarts: after an unexpected exit the health task restarts it, bounded
  by the same cooldown and rolling-window budget as the other services
  (`decision_engine_restart_cooldown`, `decision_engine_max_restart_attempts`,
  `decision_engine_restart_attempt_window`); a deliberate stop is never
  fought. `decision_engine_enabled: false` turns management off.
- Raw commands: `{"RestartDecisionEngine":{"args":null}}`,
  `{"StopDecisionEngine":null}`, `{"StatusDecisionEngine":null}`.
- Build and swap alone, without a Magician rebuild:
  `make build-decision-engine-release` (builds and installs the `.bin`),
  then `make restart-decision-engine`; or
  `make replace-restart-decision-engine` from an existing build.

Smoke check without the supervisor:
`decision-engine --config magician-config.yaml --socket /tmp/de.sock`, then
`curl --unix-socket /tmp/de.sock http://x/health`.

## Restart Guard

`restart-magician` and `restart-magicutor` enforce two operator protections:

- A short cooldown between consecutive restarts (`*_restart_cooldown`,
  default 10 seconds).
- A rolling attempt window (`*_max_restart_attempts` inside
  `*_restart_attempt_window`, default 5 attempts in 5 minutes).

The window is intentionally rolling, not lifetime-based. Old successful
development restarts age out, while a genuine bad restart loop still trips the
guard with an error that includes the service name, attempt count, and window.
Status responses include both the total successful `restart_count` and the
current `restart_attempts_in_window`.

## Health Checks

Each managed service (Magician, Magicutor) has a background health-check task that
polls the process's HTTP `/health` after an initial grace, then on a fixed interval
(`*_health_check_interval`, default 20 s).

Health-check logging is **transition-aware**, keyed to the exact process generation
(`pid` plus supervisor restart count). Failures before that generation's first success
log at DEBUG ("still initializing"), because a fixed grace cannot cover a multi-minute
cold start (Magician binds `/health` only after heavy init); PID reuse after a restart
gets a fresh grace. A WARN is emitted only on a **healthy → unhealthy** transition for
the same generation. Steady-state successes log at DEBUG. An unhealthy-but-running
process is never restarted by the health check (restarts come from the control plane
or an unexpected exit, below).

## Subprocess log relay

Both managed services write to the supervisor's own log, prefixed `[Magician]` or
`[Magicutor]`, with each relayed line assigned a level. Assignment is by inspection,
because the supervisor sees text rather than structured events:

1. **Process-death shapes log at ERROR.** A line beginning `Error:` (what
   `main() -> Result` prints through `Termination` when it returns `Err`) or
   containing `panicked at`. This is checked **first**, ahead of the `/health`
   rule below, so a startup failure that happens to name the health endpoint
   cannot be silenced.
2. **`/health` chatter logs at DEBUG.** Poll traffic is high-volume and uninteresting.
3. **An embedded level token wins** — ` TRACE `, ` DEBUG `, ` WARN `, ` ERROR `.
   The children emit their own `tracing` output on stderr, and it carries a level,
   so it is classified by that level regardless of stream.
4. **Anything left is unlabelled**, and splits by stream: stdout at INFO,
   **stderr at WARN**.

Rule 1 exists because a dying child's explanation (e.g. `Error: acquiring default
scope lease: conflict`) carries no tracing level. Rule 4 treats unlabelled stderr as a
diagnostic; it cannot affect `tracing` output, which matches at step 3. Explicit child
`INFO` records (including lowercase `info` and `[INFO]`) stay informational on stderr.

## Process-tree ownership

On Unix hosts, the supervisor launches Magician as the leader of a dedicated
process group. The group is the lifecycle boundary for process-owned sidecars:
`stop-magician`, `restart-magician`, and supervisor shutdown signal the group,
wait for the Magician parent, then reap any remaining descendants. If a native
dependency terminates Magician before Rust can perform normal teardown, the
next process-health observation logs the real exit status and immediately
reaps the surviving group. This prevents the FluidAudio listener on `3029`
from becoming a PID-1 orphan and blocking the next start.

`make stop-macos-audio-engine` is a compatibility sweep for leftover orphans. It resolves the listener on `3029`, verifies that
its command contains the exact `magician-macos-audio-engine.bin` basename, and
otherwise leaves the port owner untouched. `make stop-magician` and
`make stop-supervisor` invoke this sweep after the control-plane stop.

## Thread Stack Size

Magician uses ordinary Rust/Tokio spawned-thread stack defaults. The launcher
explicitly removes inherited `RUST_MIN_STACK` values so oversized synchronous
frames or async poll chains cannot be hidden. Full agentic futures are
definition-level erased and lazily constructed on a dedicated runtime;
external JSON and accessibility trees use iterative traversal and retained
depth ceilings.

For incident recovery only, an operator may set
`MAGICIAN_EMERGENCY_RUST_MIN_STACK` before `make run-supervisor`. The launcher
maps that value to `RUST_MIN_STACK` and emits a warning. This is not a supported
steady-state setting, and the normal Rust report runner always removes it.

## Notes
- Magician default args include `--config tool-runtime-config.yaml`.
- `make run-supervisor` stops an existing supervisor first and surfaces any remaining port listeners before launch.
- Release binaries are refreshed via an atomic temp-file swap so rebuilding does not require manually killing a running `.bin` first.
- `make build-all-debug` copies `target/debug/{magician,magicutor,magic-supervisor,decision-engine}` into the same local `.bin` files used by `make run-supervisor` (`build-all-release` likewise from `target/release`).
- `make replace-restart-magician` and `make replace-restart-magicutor` copy from `target/$(BUILD_PROFILE)` and then restart the managed service via `supervisor-ctl` (`BUILD_PROFILE=debug` by default).
- UI `/presto` depends on Magician realtime events (`/api/magician/v2/realtime/ws`).
- The real-binary supervisor smoke test is environment-sensitive. If packaged
  binaries launch but never report healthy in the local developer environment,
  the test shuts the supervisor down, prints the last health/status payload, and
  skips instead of failing the whole suite for a local port/profile issue.

## Restart after an unexpected exit

When the Magician health task (every `magician_health_check_interval`, default 20 s)
observes that the child exited on its own, `MagicianProcess` records it
(`unexpected_exit`, the exit status as text) and the next tick restarts the child with
the recorded launch args (`restart(None)`). Log lines: `Magician exited on its own (…);
restarting it`, then `Magician restarted after an unexpected exit`.

- **A deliberate stop is never resurrected.** `stop()` clears the flag and leaves no
  exit status for `is_running()` to observe.
- **The restart budget applies** (`magician_restart_cooldown` 10 s; 5 per
  `magician_restart_attempt_window` of 5 min). Past the budget the task warns every
  tick until the window rolls; the flag stays set so the next window tries again.
- A successful spawn clears the flag, so one crash yields one restart.
- Magicutor's health task remains observe-only.

Tests: `an_unexpected_exit_is_remembered_until_a_deliberate_stop`,
`restart_after_an_unexpected_exit_clears_the_flag`.
