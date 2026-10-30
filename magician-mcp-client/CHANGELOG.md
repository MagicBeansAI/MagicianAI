# Changelog

All notable changes to `magician-mcp-client` are documented here.

## [Unreleased]

- 0.1.26: Support public catalog schemas and JSON-null acknowledgments, classify initialization failures, and bind short-lived artifacts to opaque session fingerprints.

- Upgrade the official MCP SDK to 3.7.0, including current OAuth discovery and legacy-server compatibility fixes.

### 2026-10-24 — 0.1.25

- The request-timeout ceiling rises to the manifest maximum (24 h); packs that declare no timeout keep 60 s.

### 2026-10-23 — 0.1.24

- `call_tool_reporting_dispatch` reports whether a request was sent; `ToolCallError::{NotSent, Unknown}` lets callers retry only calls that never reached the server.

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

---

Older entries: `docs/archive/changelogs/magician-mcp-client.md`
