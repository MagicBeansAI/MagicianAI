# MCP Client Boundary

`magician-mcp-client` is Magician's isolated client adapter over the official Rust MCP
SDK. It keeps protocol churn out of `tool-runtime-core` and prevents MCP SDK types from
becoming product-wide APIs. Per-slice history lives in the
[crate changelog](../../../magician-mcp-client/CHANGELOG.md).

## Current contract

- `rmcp` is exact-pinned at `3.1.0`; Git versions of the MCP SDK and handwritten
  MCP wire logic are prohibited. `tool-runtime-core` inherits the exact
  workspace Git revision from MagicRun.
- `ClientLifecycleMode::Auto` prefers stateless MCP `2026-07-28` and falls back to
  `2025-11-25` only after `server/discover` returns method-not-found. Stdio and Streamable
  HTTP use that auto lifecycle; the duplex-JSON channel (Android device bridge) requires
  `2026-07-28` discovery and does not own WebSockets, pairing, device identity, product
  scope, or reconnect.
- Supported transports are governed stdio children, Streamable HTTP, and a
  bounded duplex-JSON channel supplied by an embedding runtime that already owns
  authentication and framing. The retired 2024 HTTP+SSE client transport is not
  recreated.
- Stdio executables and working directories must already be resolved absolute paths.
  The child starts with a clean environment and receives only declared bindings.
  Stdio close/drop owns the complete isolated server process group.
- HTTP requires HTTPS except for loopback development endpoints. Credentials and query
  strings are forbidden in endpoint URLs; a bearer credential is supplied separately
  and is redacted from diagnostics.
- Discovery issues SDK-owned `tools/list` through a bounded page loop, rejects
  repeated cursors / overflow, and atomically replaces the callable set only after
  every page validates. Failed rediscovery preserves the previous generation; old
  ids are revoked only after a successful swap. A call requires an opaque
  `McpToolId` from the current snapshot. Timeouts and dropped futures send
  `notifications/cancelled`.
- SDK `CredentialStore` / `StateStore` adapt to a scoped encrypted vault. Opaque
  keys bind principal, workspace, provider, profile, MCP resource, and issuer;
  PKCE callback state is create-only, 10-minute TTL, atomically consumed. The
  coordinator owns discovery/PKCE/exchange/persistence; product handlers supply
  only the raw callback query. Local logout does not claim provider revocation
  (`rmcp 3.1.0` exposes none).
- A policy-first projector mints dotted local names from a finite JSON-Schema
  subset and rejects the complete candidate on any failure. Tool ids bind client
  instance + discovery generation. Continuations reserve count/byte budget
  (shared 512 MiB / 512-entry pool). Tasks and subscriptions are opt-in; only
  remote Tasks survive SDK-session restart. Magician's product surface compiles
  `status`, `auth_start`, `list_tools`, `call_tool`, `clear_auth` and reports
  durable continuation unavailable (no automatic retry) on MRTR or a pending task.

## Untrusted-server controls

Remote server identity, instructions, tool names, descriptions, annotations, schemas,
content, and structured results are untrusted. The boundary applies immutable hard
ceilings plus lower configurable limits to tool/page count, text, catalog/schema/
request/result size, raw stdio/HTTP messages, SSE events, JSON depth, and JSON node
count. Stdio uses the SDK's bounded JSON codec; HTTP bodies are capped while streaming,
before JSON decoding. JSON traversal and rejected owned-argument destruction are
iterative so adversarial nesting cannot create a recursive validation or drop stack.

Schema projection is a deliberately smaller contract than general JSON Schema. Remote
prose-bearing annotations and free-form patterns are not model-visible. Portable
property keys, finite scalar enum/constant literals, standard primitive types, bounded
composition, common structural/numeric limits, and a fixed format allowlist remain;
authorization never derives from any of them. Literal values are still untrusted remote
data, so execution remains subject to the locally authored policy and future product
dispatcher rather than schema claims.

Remote error messages and error data do not cross the boundary. Callers receive stable
failure classes and, for JSON-RPC failures, only the numeric error code. Bearer values
are retained in a zeroizing credential wrapper and materialized only into a transient,
sensitive authorization header.

Annotations are exposed only as hints. They never authorize an action, classify it as
safe, select a credential profile, or reduce approval/resource-authority policy.

## Ownership

The official SDK owns MCP models, JSON codecs, correlation, lifecycle/version
negotiation, Streamable HTTP state machines, SSE parsing, and OAuth protocol mechanics.
This crate owns resource ceilings around those SDK primitives, the narrow
transport/session wrapper, cancellation discipline, safe SDK-independent projection,
and the adapter from SDK store traits to a product-owned encrypted vault. The adapter
does not implement discovery, PKCE, token exchange, refresh, or scope upgrades itself.
The Auth Broker and Magician runtime own principal/workspace/resource/issuer/profile
policy, browser callback UX, approvals, Resource Authority, product catalog
registration/routing, telemetry, and artifacts.

## Verification

`make test-phase5g-local` runs the isolated client suite, core strategy suite, and
focused delegated-material, encrypted-vault, and OAuth registry/callback product
boundaries. `make test-mcp-official-conformance` runs the networked official lane
against the pin at `rmcp-v3.1.0` for both `2025-11-25` and `2026-07-28`.
`make test-phase5g` runs both. The offline pin/evidence guard is included in
`make check-all`.

## Canonical references

- Universal skill runtime and Auth Broker plan
- [Crate changelog](../../../magician-mcp-client/CHANGELOG.md)
- [Official Rust MCP SDK](https://github.com/modelcontextprotocol/rust-sdk)
- [Official MCP conformance suite](https://github.com/modelcontextprotocol/conformance)
