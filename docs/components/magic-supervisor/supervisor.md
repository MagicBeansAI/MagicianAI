# Magic Supervisor

`magic-supervisor` manages long-running workspace services.

## Managed Services
- `magician` (default port `3002`)
- `magicutor` (default port `3003`)
- `decision-engine` (optional; Unix socket, managed only when
  `decision-engine.bin` is installed; see below)

## Quick Start

```bash
make build-all-release
make run-supervisor
```

## Control Plane
The supervisor exposes a TCP command interface on `127.0.0.1:8081` (default).

Operator commands (via `supervisor-ctl`): `restart-magician`,
`restart-magicutor`, `stop-magician`, `stop-magicutor`,
`restart-decision-engine`, `stop-decision-engine`, `status-decision-engine`,
`status`, `status-magician`, `status-magicutor`, `host-status`,
`host-presence-start`, `host-presence-stop`, `host-presence-restart`,
`health`, and `shutdown` (stops managed services and exits).

Ctrl+C and Unix SIGTERM also drain managed services before exit, including
during startup and after a server startup error. Magician's process group is
reaped even after a crash recovery, so old `/health` listeners cannot survive
a stack shutdown.

The `host-*` commands are thin proxy calls to `MAGICIAN_HOST_GATEWAY_URL`
(`http://127.0.0.1:3017` by default) for host-native status or macOS
presence-host lifecycle; the tray gateway stays the owner of native GUI
processes.

## Magicutor defaults

Magicutor launches as `./magicutor.bin` (macOS/Linux) or `./magicutor.exe`
(Windows) with no arguments; server mode is forced through the environment
(`MAGICUTOR_FORCE_SERVER=1` / `MAGICUTOR_MODE=server`). Its port resolves from
`MAGICUTOR_PORT`, then CLI args, then `3003`; the health check hits that port
with a 2 s TCP fallback. Raw control-plane commands:

- `{"RestartMagicutor":{"args":null}}`
- `{"StopMagicutor":null}`
- `{"StatusMagicutor":null}`
- `{"HealthCheck":null}` (aggregate across both services)

Smoke check: start `magic-supervisor`, run `./supervisor-ctl status`, then
`curl http://127.0.0.1:3003/health`.

## Decision engine

The structured-decision engine runs as `./decision-engine.bin` when that
binary exists; otherwise the supervisor logs once and manages nothing
(`status` reports `not_installed`, and aggregate `health` ignores it). It
starts before Magicutor and Magician; until it answers, Magician's step judges
fall through to the LLM.

For a paired decision-contract rollout, start with
`MAGICIAN_SUPERVISOR_START_MAGICIAN=0 make run-supervisor`: it starts the
engine and Magicutor but holds Magician. Verify the engine's socket health,
contract, restrictions and active config, then `make restart-magician` and
check Magician health. Normal startup launches the engine, then Magicutor,
then Magician two seconds later; it does not wait for the engine's local model
to load.

- Socket: `decision_engine_socket` when set, else what the engine reports
  (`decision-engine --print-socket`). The engine resolves it like Magician's
  default (`MAGICIAN_ROOT_DIR`, then `MAGICIAN_STORAGE_PATH`, then
  `~/MagicianNotes`, plus `/run/decision-engine.sock`) and is the only place
  that does: the supervisor never resolves the runtime root itself
  (`decision-engine/src/main.rs` is the reviewed resolver). The path is passed
  back as `--socket` and health-checked there. The engine creates it
  owner-only.
- Settings: `<root>/decision-engine.yaml`, else the `decision:` block of
  `<root>/magician-config.yaml`.
- Health: `GET /health` over the socket every 20 s
  (`decision_engine_health_check_interval`).
- Restarts: after an unexpected exit the health task restarts it, within the
  same cooldown and rolling budget as the other services
  (`decision_engine_restart_cooldown`, `decision_engine_max_restart_attempts`,
  `decision_engine_restart_attempt_window`); a deliberate stop is never
  fought. `decision_engine_enabled: false` turns management off.
- Raw commands: `{"RestartDecisionEngine":{"args":null}}`,
  `{"StopDecisionEngine":null}`, `{"StatusDecisionEngine":null}`.
- Swap without a Magician rebuild: `make build-decision-engine-release`
  (builds and installs the `.bin`), then `make restart-decision-engine`; or
  `make replace-restart-decision-engine` from an existing build.

Smoke check without the supervisor:
`decision-engine --config magician-config.yaml --socket /tmp/de.sock`, then
`curl --unix-socket /tmp/de.sock http://x/health`.

## Restart Guard

`restart-magician` and `restart-magicutor` enforce:

- a cooldown between restarts (`*_restart_cooldown`, default 10 seconds);
- a rolling attempt window (`*_max_restart_attempts` inside
  `*_restart_attempt_window`, default 5 attempts in 5 minutes).

The window is rolling, so old development restarts age out while a real
restart loop still trips the guard with an error naming the service, attempt
count and window. Status includes the total `restart_count` and the current
`restart_attempts_in_window`.

## Health Checks

Magician and Magicutor each have a health task that polls HTTP `/health` after
an initial grace, then every `*_health_check_interval` (default 20 s).

Logging is **transition-aware**, keyed to the process generation (`pid` plus
supervisor restart count). Failures before that generation's first success
log at DEBUG ("still initializing"), since Magician binds `/health` only after
a cold start that can take minutes; PID reuse gets a fresh grace. WARN is
emitted only on a **healthy → unhealthy** transition. Steady-state successes
log at DEBUG. The health check never restarts an unhealthy-but-running
process.

## Subprocess log relay

Both services write to the supervisor's log, prefixed `[Magician]` or
`[Magicutor]`. Each relayed line gets a level by inspection:

1. **Process-death shapes log at ERROR**: a line beginning `Error:` (what
   `main() -> Result` prints through `Termination` on `Err`) or containing `panicked at`. Checked
   **first**, so a startup failure naming `/health` cannot be silenced.
2. **`/health` chatter logs at DEBUG.**
3. **An embedded level token wins**: ` TRACE `, ` DEBUG `, ` WARN `, ` ERROR `,
   so the children's `tracing` output on stderr keeps its level.
4. **Anything left is unlabelled**: stdout at INFO, **stderr at WARN**.

A dying child's explanation (e.g. `Error: acquiring default scope lease:
conflict`) carries no tracing level, hence rule 1. Explicit child `INFO`
records (including lowercase `info` and `[INFO]`) stay informational on
stderr.

## Process-tree ownership

On Unix, the supervisor launches Magician as the leader of its own process
group, the lifecycle boundary for its sidecars. `stop-magician`,
`restart-magician` and supervisor shutdown signal the group, wait for the
parent, then reap remaining descendants. If a native dependency kills Magician
before normal teardown, the next health observation logs the real exit status
and reaps the group, so the FluidAudio listener on `3029` cannot become an
orphan that blocks the next start.

`make stop-macos-audio-engine` sweeps leftover orphans: it resolves the
listener on `3029`, checks its command contains the exact
`magician-macos-audio-engine.bin` basename, and otherwise leaves it alone.
`make stop-magician` and `make stop-supervisor` run this sweep after the
control-plane stop.

## Thread Stack Size

Magician uses ordinary Rust/Tokio thread stack defaults. The launcher removes
inherited `RUST_MIN_STACK` so oversized frames or async poll chains cannot be
hidden. Full agentic futures are erased and lazily built on a dedicated
runtime; external JSON and accessibility trees use iterative traversal with
depth ceilings.

For incident recovery only, `MAGICIAN_EMERGENCY_RUST_MIN_STACK` set before
`make run-supervisor` is mapped to `RUST_MIN_STACK` with a warning. It is not
a supported steady-state setting, and the Rust report runner always removes
it.

## Notes
- Magician default args include `--config tool-runtime-config.yaml`.
- `make run-supervisor` stops an existing supervisor first and reports any
  remaining port listeners before launch.
- Release binaries are swapped via an atomic temp file, so a rebuild does not
  require killing a running `.bin`.
- `make build-all-debug` copies
  `target/debug/{magician,magicutor,magic-supervisor,decision-engine}` into the
  `.bin` files `make run-supervisor` uses (`build-all-release` likewise from
  `target/release`).
- `make replace-restart-magician` and `make replace-restart-magicutor` copy
  from `target/$(BUILD_PROFILE)` and restart via `supervisor-ctl`
  (`BUILD_PROFILE=debug` by default).
- UI `/presto` depends on Magician realtime events
  (`/api/magician/v2/realtime/ws`).
- The real-binary supervisor smoke test skips (after printing the last
  health/status payload) when packaged binaries never report healthy locally,
  instead of failing the suite for a local port/profile issue.

## Restart after an unexpected exit

When the Magician health task (every `magician_health_check_interval`, default
20 s) sees the child exited on its own, `MagicianProcess` records it
(`unexpected_exit`, exit status as text) and the next tick restarts it with
the recorded launch args (`restart(None)`). Log lines: `Magician exited on its
own (…); restarting it`, then `Magician restarted after an unexpected exit`.

- **A deliberate stop is never resurrected.** `stop()` clears the flag and
  leaves no exit status for `is_running()` to observe.
- **The restart budget applies** (`magician_restart_cooldown` 10 s; 5 per
  `magician_restart_attempt_window` of 5 min). Past it the task warns every
  tick; the flag stays set so the next window tries again.
- A successful spawn clears the flag, so one crash yields one restart.
- Magicutor's health task stays observe-only.

Tests: `an_unexpected_exit_is_remembered_until_a_deliberate_stop`,
`restart_after_an_unexpected_exit_clears_the_flag`.
