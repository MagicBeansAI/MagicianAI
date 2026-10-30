# Magic Supervisor Docs

Landing page for `magic-supervisor` documentation.

For macOS allocation diagnostics, start the supervisor with
`MAGICIAN_MALLOC_STACK_LOGGING=lite` for current allocation stacks, or `full`
for on-disk history. `MAGICIAN_MALLOC_STACK_LOGGING_DIR=/path/on/ssd` requests
the disk location, but macOS may enforce its own; verify the startup log.
Settings apply to Magician and its automatic restarts; logging is off by default.

The supervisor accepts optional `MAGICIAN_HTTP_HOST` and
`MAGICIAN_FRONTEND_DIR` startup environment values and forwards nonempty values
as Magician's `--host` and `--frontend-dir` arguments. Desktop-managed native
startup defaults the host to `0.0.0.0`, which makes the explicitly selected
Same Wi-Fi mobile route reachable; set `MAGICIAN_HTTP_HOST=127.0.0.1` for a
deliberately loopback-only service. Container images also bind `0.0.0.0`, but
mobile pairing does not advertise an unreachable guest address.

## Canonical References

- [Supervisor Runtime Guide](supervisor.md)
- [Desktop app watch (macOS)](supervisor.md#desktop-app-macos): relaunch
  after a crash, never after Quit (`supervisor.manage_desktop`, default on)

## Workspace-Level Cross Links

- [Deployment](../../DEPLOYMENT.md)
- [Quick Start](../../quickstart.md)

## Child log severity

Explicit child `INFO` records, including the native audio sidecar's lowercase
`info` and FluidAudio's `[INFO]`, remain informational even on stderr, alongside
TRACE, DEBUG, WARN and ERROR. Unlabelled stderr remains a warning; fatal
`Error:` and panic lines remain errors even when they mention `/health`.

`supervisor-ctl` waits up to 30 seconds for a response (override with
`SUPERVISOR_CTL_TIMEOUT`). The timeout must exceed the supervisor's ten-second
graceful-stop window, or the restart result is lost to a broken pipe.
`magic-supervisor client <command>` waits 30 seconds too.
