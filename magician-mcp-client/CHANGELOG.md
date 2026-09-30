# Changelog

All notable changes to `magician-mcp-client` are documented here.

## [Unreleased]

### Changed

- `Cargo.toml` `authors` now carries the org (`MagicBeansAI`), not an individual — operator-identity scrub (go-live plan §4 A5).

## [0.1.22] - 2026-08-11

### Added

- Added a bounded duplex-JSON transport for an already-authenticated
  bidirectional text channel. Embedding runtimes retain socket, identity, and
  reconnect ownership while this crate continues to own rmcp framing and
  lifecycle behavior.

### Changed

- Duplex clients explicitly use MCP `2026-07-28` discovery; existing stdio and
  Streamable HTTP clients retain automatic discover-first compatibility with
  the legacy initialized lifecycle.

### Verification

- Added configuration and Android fake-server coverage for explicit discovery,
  validated tool calls, and request-bound tool-list subscriptions.

## [0.1.21] - 2026-08-09

### Added

- Added the Phase 7 product bearer-token handoff as a zeroizing value after the
  official SDK's refresh behavior, allowing the shared Auth Broker to connect remote
  Streamable HTTP MCP packages without exposing SDK or OAuth internals.

### Changed

- Swiggy and Zepto production routing now consumes this official-SDK boundary through
  one provider-neutral Magician executor; handwritten Node transport is retired.

### Verification

- Product projection, OAuth binding, SDK ownership, and no-local-wire-adapter coverage
  passes in the completed Phase 7 verification. The canonical workspace run completed
  all 10,158 executed Rust tests, plus Rust doctests, without a failure.

## [0.1.20] - 2026-08-07

### Fixed

- Live OAuth callback completion and cancellation now share the coordinator's exact-
  binding lifecycle lock with status, refresh, logout, invalidation, scope upgrade, and
  restart completion. Public SDK callers can no longer race an outstanding browser
  attempt against a mutation on the same binding.
- A client rejected by process-wide continuation admission now closes and awaits its
  already-connected MCP service before returning the capacity error.
- Remote-task poll accounting failures release the exact private task-notification hint,
  and duplicate remote tool diagnostics no longer include provider-controlled names.

### Changed

- Removed the obsolete OAuth vault namespace-listing contract. The coordinator uses only
  fixed exact-binding credential/state slots and the one exact pending-flow key, so vault
  implementations no longer need to expose a scan primitive.

### Verification

- Added focused concurrency, connected-service cleanup, continuation overflow cleanup,
  and value-free diagnostic regression coverage.

## [0.1.19] - 2026-08-07

### Fixed

- OAuth state and pending records now occupy stable exact-binding slots, so status,
  expiry, cancellation, and logout use fixed-key operations instead of scanning shared
  vault namespaces. A malformed record for another profile can no longer poison the
  selected binding.
- Successful live and restart token exchanges are no longer reported as failures when
  pending-record cleanup is temporarily unavailable. An encrypted pre-flow credential
  revision lets status reconcile the committed exchange after restart.
- Stdio transports now own an isolated server process group and terminate/reap the
  complete group on close or drop.

### Security

- Continuation clients reserve from a process-wide 512 MiB/512-entry budget in addition
  to their per-client limits. The default client reservation is 64 MiB, preventing each
  connected server from independently claiming the former 512 MiB allowance.

### Verification

- The complete isolated client suite passes 185/185 tests. Test-only fixture clients use
  isolated continuation owners so parallel test scheduling cannot consume the real
  process-wide production budget; the production constructors still fail closed at the
  global ceiling.
- Coverage includes committed live/restart OAuth exchange with deferred pending cleanup,
  rejected wrong-state callbacks, exact process-group teardown, 24 concurrent calls
  under an explicit test result budget, and stable source-integrity evidence.

---

Older entries: `docs/archive/changelogs/magician-mcp-client.md`
