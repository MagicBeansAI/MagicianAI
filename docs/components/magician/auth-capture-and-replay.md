# Captured Auth For API Replay

## Purpose

Authenticated replay needs live session credentials from browser traffic,
without storing unredacted traces durably.

## Architecture

The runtime uses the shared `SecretStore` captured-auth partition
(`magicvault_core::store`, the pinned MagicVault git dependency, re-exported by
`magician/src/magician_v2/secrets/store.rs`), not a standalone vault module. Auth extraction, persistence, status checks and replay
lookup all go through that store.

MCP OAuth is a separate boundary, not captured-auth replay. Magician gives
`magician-mcp-client` an exact-scope encrypted vault adapter and secret-free HTTP
lifecycle endpoints; governed MCP dispatch registers the coordinator.

- Credentials, PKCE state and pending callback metadata live in distinct opaque
  namespaces bound to principal, workspace, provider, profile, MCP resource and
  issuer. Completion, cancellation, status, refresh, logout and scope upgrade
  serialize on that exact binding and never scan another binding's records.
- The callback route is stable for pre-registration; a random attempt id stops
  an old UI action from mutating a replacement flow. Removing or rejecting a
  live coordinator first settles its exact authority.
- Product surfaces expose only bounded status and fixed error classes, never
  authorization URLs, callback codes, tokens, vault keys or provider error
  payloads.

## Capture Flow

1. Magicutor observes the raw CDP request in process memory.
2. Normal network traces are redacted at the Magicutor boundary.
3. Auth-bearing headers, cookies and auth query values enter a separate,
   bounded, one-shot auth buffer keyed by CDP session.
4. Explicit refresh sessions also snapshot origin-matching browser cookies
   (including HttpOnly) and keys from the page's local and session storage.
   Capability-declared names are accepted exactly; conservative auth-name
   detection is the fallback and excludes analytics and preference state.
   Storage is accepted only while the page's live origin matches the requested
   origin, so an SSO redirect cannot be misattributed.
5. Magician drains and merges those sources into the encrypted, per-origin
   captured-auth store. Unredacted values are never logged or written to
   API-mining trace files.
6. Durable trace storage gets only the redacted event (auth-bearing query
   values, sensitive JSON body fields and auth-bearing initiator URLs
   redacted). Magician sanitizes the drained event again at its process
   boundary before correlation or sequence recording.

## Durability Of The Partitions

`SecretStore` keeps three encrypted partitions per scope (provisioned, captured
and the MCP OAuth vault) with one durable write discipline:

- **One writer at a time per partition file.** The lock is held across the
  snapshot *and* the rename, so the file is monotone; without it two writers
  can clone different generations and the older one lands last, losing entries
  at the next restart. One lock per partition, so a provisioned write never
  queues behind a captured flush that encrypts and fsyncs every origin.
- **A temp name unique per write**, since every writer shares the directory.
- **`sync_all` on the temp before the rename**, or a crash can leave a present,
  zero-length, undecryptable vault.
- **`sync_all` on the parent directory after it**, since the rename is a
  directory mutation.
- **The temp removed on every failure path**, so no encrypted copy leaks into
  the scope.

### An unreadable partition reads as empty, and says so

A partition that will not decrypt or parse is moved aside and the store carries
on with it **empty**; it does not refuse to start. Refusing would take the whole
daemon down over one scope's file, and an empty partition already fails every
credential lookup closed.

Empty must not be *silent*, or an operator may re-provision over recoverable
data. `SecretStore::partition_status` reports `SecretPartitionStatus::Unavailable` for a failed load, distinct from
`Loaded` with no entries, and the quarantine is logged at `warn` with its path.

- The quarantine name is **unique per event** (`…​.corrupt-<uuid>`), so a
  second occurrence cannot overwrite the first.
- Only *corrupt bytes* are quarantined. A key backend that cannot answer leaves
  the vault intact, and the scoped resolver keeps refusing so a later attempt
  can still find the data. Corruption resolves to a usable store reporting
  `Unavailable`.

Writes stay permitted against an `Unavailable` partition: the bytes are already
preserved beside the vault, and refusing would leave only manual repair.

### `SecretEntry` never prints its values

`SecretEntry::fields` holds plaintext: provisioned provider keys and, for
captured entries, live `Authorization` headers and session cookies. Its
hand-written `Debug` emits field *names* and value *lengths* only, because the
entry is reachable from a `Debug` store state.

## LLM Boundary

Raw captured-auth events are consumed only by `SecretStore`; they are never in
ambient page signals, API-mining traces, tool results, telemetry or prompt
inputs. Ambient browser distillation gets metadata-only page signals and is
pinned to a verified local provider.

Workflow compilation can be operation-mapped to a remote provider, so its prompt carries a
scalar-free structural projection: sequence and capability ids, ordering, HTTP
method, parameter names, JSON field shape, response status and locally inferred
flow seeds. It omits headers, cookies, storage, query/path values,
task/execution ids, browser arguments, descriptions and all request/response
scalar values. Local deterministic post-processing applies replay literals
after the LLM response.

## Replay Flow

- Replay resolves the request origin, asks the captured-auth store for that
  origin's auth, injects it and executes the request.
- Task Recipe replay keeps a request-local cookie jar. Valid `Set-Cookie`
  responses apply to later steps only when domain, path and `Secure` match. The
  runner resolves URL parameters first, then loads auth for the concrete URL
  before resolving cookie-backed headers or bodies, so a template such as
  `/tenants/{tenant}/items` does not filter out cookies scoped to the real
  tenant path. The auth-retry path resolves the same way.
- SecretStore supplies cookie identity metadata (name/domain/path) only in
  memory; it is never serialized or accepted from session JSON. Response
  cookies replace the matching captured identity, keeping other same-name
  cookies and path ordering. Cookie parameter names are case-sensitive.
  `Expires` and `Max-Age` are honored (`Max-Age` wins regardless of order).
  Expired entries stay as in-run deletion markers so later lookups cannot
  resurrect them. The 256-cookie update bound includes markers; exceeding it
  stops replay rather than losing a deletion. The jar never changes the durable
  partition.
- Legacy/in-process session providers without routing metadata keep their
  name-based merge, with URL-scoped deletion suppression. Production SecretStore
  providers use exact identities.
- Newly compiled cookie-bearing steps keep a value-free `cookie_header` session
  requirement. With no cookies after concrete-URL lookup and in-run updates,
  the step fails before HTTP instead of accepting an anonymous 200 as
  completion. Unauthenticated bootstrap steps do not inherit it. Recipes with
  only an aggregate session hint fail conservatively when cookies are absent;
  recompile them to gain per-step requirements.
- HTTP 401 marks the exact captured session generation stale. HTTP 403 is an
  auth-related diagnostic but does not poison the lease, since it can mean
  valid auth with insufficient permission, tenant context, CSRF state or a
  missing header.

### Automatic Task Recipe healing

Task Recipes share the captured-auth refresh core with the explicit origin
refresh. After a response is classified as stale authentication, the runner's
short-lived scoped healer refreshes, the session is looked up again and the same
request is retried on the same transport. Successful healing discards earlier
in-run cookie updates for that origin's cookie scope, so an old rotation or
deletion cannot override the fresh capture; other origins' cookies stay.
Healing is bounded by `api_mining.auth_max_failures` across the whole recipe
run, not per step.

Session values stay late-bound: recipe JSON stores only schemes such as
`authorization`, `query:apikey` or `body:password`. Cookies, bearer tokens, CSRF
values, password fields, query credentials and volatile signatures never enter
the recipe, matcher payload, run ledger or lifecycle event. An unresolved
session or volatile value falls back before a transport send.

Auth failures have their own recipe metric and appear in the run ledger but do
not demote recipe maturity: authentication is environmental state, not evidence
the learned request shape is wrong. Schema/HTTP/network failures update replay
statistics normally.

## Status And Consumers

The same store serves replay execution, executor and orchestrator auth-aware
paths, and API Explorer auth-status surfaces. Origin-level fresh/stale state
separates expired auth from general replay failures. A replay that succeeds with
a captured session lease marks that generation fresh again.

## Deterministic Refresh

The API Mining `Refresh Auth` action starts a short-lived Magicutor CDP session
for the selected scoped origin, using the signed-in browser profile in a
dedicated window, and waits up to 60 seconds for an authenticated request. No
agent or LLM participates.

The UI polls a non-secret status resource through: starting the secure browser;
waiting for an authenticated request; captured; verifying with a concrete
read-only capability; then verified, captured without an eligible verifier,
timed out, or failed.

The runtime closes and clears the CDP session after a terminal result.
Verification respects origin replay policy and never picks a write capability.
If the app makes no protected request on load, the operator can navigate in the
window while capture is active. The HTTP drain and WebSocket CDP connection
resolve from `execution.magicutor_base_url`; the drain reuses the
orchestrator's Magicutor client, so its API-key, timeout and proxy policy apply.

## Explicit Non-Goals

- No standalone `AuthVault`; the `SecretStore` partition is the store.
- Auth refresh does not automate login, CAPTCHA, MFA or app-specific
  navigation. It captures the authenticated traffic produced by the signed-in
  profile or by operator navigation inside the refresh window.
