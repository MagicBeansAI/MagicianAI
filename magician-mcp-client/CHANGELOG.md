# Changelog

All notable changes to `magician-mcp-client` are documented here.

## [Unreleased]

### 2026-10-04 — 0.1.23

- Retry legacy Streamable HTTP initialization on a separately bounded transport.

## [0.1.22] - 2026-08-11

### Added

- Bounded duplex-JSON transport over an already-authenticated bidirectional channel.

### Changed

- Duplex clients use MCP `2026-07-28` discovery; stdio and Streamable HTTP clients keep discover-first compatibility with the legacy lifecycle.

## [0.1.21] - 2026-08-09

### Added

- Bearer-token handoff from the official SDK so the shared Auth Broker can connect remote Streamable HTTP MCP packages.

### Changed

- **Removed:** Swiggy and Zepto routing now uses one provider-neutral executor on the official SDK; the handwritten Node transport is retired.

## [0.1.20] - 2026-08-07

### Fixed

- OAuth callback completion and cancellation can no longer race other mutations on the same binding.
- A client rejected for capacity closes its connected service first.
- Remote-task accounting failures release their notification hint; duplicate-tool diagnostics no longer include provider-controlled names.

### Changed

- **Breaking:** OAuth vaults no longer need a namespace-listing (scan) primitive.

## [0.1.19] - 2026-08-07

### Fixed

- OAuth state uses fixed per-binding slots, so a malformed record for another profile cannot poison the selected binding.
- Successful token exchanges are no longer reported as failures when pending-record cleanup is briefly unavailable.
- Stdio transports kill and reap the server's whole process group on close.

### Security

- Continuation clients share a process-wide 512 MiB / 512-entry budget; the per-client default is 64 MiB.

---

Older entries: `docs/archive/changelogs/magician-mcp-client.md`
