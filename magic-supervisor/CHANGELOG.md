# Changelog

All notable changes to the Magic Supervisor project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---
## [Unreleased]

- Native package defaults now launch `magician.exe` and `magicutor.exe` on
  Windows while preserving the existing `.bin` names on macOS and Linux.

### Fixed - process-tree cleanup and crash visibility (v0.1.8)

- Magician now launches in an isolated Unix process group. Stop, restart,
  supervisor shutdown, and unexpected native-process exit reap the whole
  process-owned tree, including the FluidAudio sidecar.
- Unexpected Magician exit status is logged instead of being silently consumed
  by the health poll. A compatibility cleanup target safely reaps only a stale
  `magician-macos-audio-engine.bin` listener on port `3029`; it leaves
  SilverBullet and unrelated listeners untouched.
- A dying child's own explanation is no longer relayed at INFO. Subprocess
  lines were levelled by substring (` ERROR `, ` WARN `, …) with everything
  unrecognised defaulting to INFO, and stdout and stderr shared one path. But
  `main() -> Result` returning `Err` prints `Error: {err:?}` through
  `Termination`, and a panic prints `thread '...' panicked at ...` — neither
  carries a tracing level, so the single line saying why startup died was
  invisible to an error-level filter (observed as
  `INFO [Magician] Error: acquiring default scope lease: conflict`). Those
  shapes now log at ERROR, checked **before** the `/health` noise rule so a
  crash mentioning that endpoint cannot be silenced to DEBUG. Unlabelled
  stderr logs at WARN rather than INFO; level tokens still win wherever they
  appear, so the children's tracing output is classified exactly as before.

### Changed - compatible macOS system-proxy discovery (v0.1.7)

- Upgraded supervisor HTTP calls to `reqwest` 0.12.28 with explicit
  system-proxy integration, keeping health and host-gateway clients on the same
  macOS proxy dependency generation as the rest of the workspace.

### Fixed — health-check warmup no longer logs false "unresponsive" (v0.1.6)
- The Magician/Magicutor health-check tasks now track whether the process has
  ever passed a check. Failures **before** the first success are logged at DEBUG
  ("still initializing") — a hardcoded 10s grace can't cover a multi-minute cold
  start, which spammed misleading `Health check failed … may be unresponsive`
  WARNs during boot. Only a healthy→unhealthy transition logs WARN now.

### Added
- Added host gateway proxy commands. `magic-supervisor` can now call
  `MAGICIAN_HOST_GATEWAY_URL` (`http://127.0.0.1:3017` by default) for host
  status and macOS presence-host start/stop/restart requests without owning
  host-native process lifecycle.

### Changed
- Restart limiting for `magician` and `magicutor` now uses a rolling attempt
  window instead of a lifetime counter. The default guard is still 5 attempts,
  but only attempts inside the last 5 minutes count, so normal development
  restarts no longer permanently exhaust the supervisor process.

---

---

Older entries: `docs/archive/changelogs/magic-supervisor.md`
