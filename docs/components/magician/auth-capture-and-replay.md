# Captured Auth For API Replay

## Purpose

Authenticated replay depends on preserving live session credentials from browser
traffic without storing unredacted traces durably.

## Architecture

The runtime uses the shared `SecretStore` captured-auth partition rather
than a standalone vault module. Auth extraction, persistence, status checks, and
replay lookup all flow through that shared store.

MCP OAuth is a separate, dormant integration boundary rather than captured-auth
replay. Magician supplies `magician-mcp-client` with an exact-scope encrypted
vault adapter and secret-free HTTP lifecycle endpoints. Credentials, PKCE state,
and pending callback metadata occupy distinct opaque namespaces bound to the
principal, workspace, provider, profile, MCP resource, and issuer. Authorization
completion, cancellation, status, refresh, logout, and scope upgrade serialize on
that exact binding; they never scan another binding's records. A callback route is
stable for pre-registration, while a random attempt id prevents an old UI action
from mutating a replacement flow. Removing or rejecting a live coordinator first
settles its exact authority, and the product surfaces expose only bounded status
and fixed error classes—not authorization URLs, callback codes, tokens, vault
keys, or provider error payloads. Production MCP routing remains disabled.

## Capture Flow

1. Magicutor observes the raw CDP request in process memory.
2. Normal network traces are redacted at the Magicutor boundary.
3. Auth-bearing headers, cookies, and auth query values enter a separate,
   bounded, one-shot auth buffer keyed by CDP session.
4. Explicit refresh sessions also snapshot origin-matching browser cookies,
   including HttpOnly cookies, plus keys from the current page's local and
   session storage. Capability-declared names are accepted exactly; conservative
   auth-name detection supplies a fallback while excluding analytics and normal
   preference state. Storage is accepted only while the page's live origin
   matches the requested origin, so an SSO redirect cannot be misattributed.
5. Magician drains and merges those transient sources directly into the encrypted,
   per-origin captured-auth store. The unredacted values are not logged or
   written to API-mining trace files.
6. Durable trace storage receives only the redacted network event, including
   redaction of auth-bearing query values, sensitive JSON body fields, and
   auth-bearing initiator URLs. Magician sanitizes the drained event again at
   its process boundary before correlation or sequence recording.

That ordering keeps replay functional without leaking live credentials into
persisted trace history.

## Durability Of The Partitions

`SecretStore` keeps three encrypted partitions per scope — provisioned, captured,
and the MCP OAuth vault — sharing one durable write discipline. A partition
write needs all five properties:

- **One writer at a time, per partition file.** The snapshot is cloned under a
  *read* guard, which several writers hold at once, so without exclusion two of
  them can clone different generations and rename in the opposite order — the
  older one lands last and the newer entries are gone from disk while still in
  memory, a loss that surfaces only at the next restart. The lock is held across
  the snapshot *and* the rename, which is what makes the file monotone. One lock
  per partition, because a captured flush encrypts and fsyncs every origin and a
  provisioned write must not queue behind it.
- **A temp name unique per write.** Every writer of a partition shares its
  directory, so a fixed `*.tmp` would put two writers inside one file.
- **`sync_all` on the temp before the rename**, or the rename can be durable
  while the contents are not, and the crash window leaves a vault that is
  present, zero-length, and undecryptable.
- **`sync_all` on the parent directory after it**, because the rename is itself a
  directory mutation.
- **The temp removed on every failure path**, so a failed write does not leak the
  encrypted vault into the scope under a name nothing cleans up.

### An unreadable partition reads as empty, and says so

When a partition will not decrypt or will not parse, it is moved aside and the
store carries on with that partition **empty** — it does not refuse to start.
Partitions are per-scope, so refusing would take the whole daemon down over one
scope's damaged file, and an empty partition already fails every credential
lookup closed, which is the safe direction.

What empty must not be is *silent*. Empty is otherwise indistinguishable from a
scope that never provisioned anything, and that is how an operator ends up
re-provisioning over data that was still recoverable. So `partition_status`
reports `Unavailable` for a partition whose load failed, distinct from `Loaded`
with no entries, and the quarantine is logged at `warn` with the path it went to.

Two details make that hold:

- The quarantine name is **unique per event** (`…​.corrupt-<uuid>`): with a fixed
  name a second occurrence would rename over the first — the copy that still
  held the entries.
- Only *corrupt bytes* are quarantined. A key backend that cannot answer leaves
  the vault intact, and the scoped resolver keeps refusing that case so a later
  attempt can still find the real data. Corruption, which the quarantine has
  already acted on, resolves to a usable store reporting `Unavailable` rather
  than a scope that silently looks unprovisioned.

Writes stay permitted against an `Unavailable` partition: the bytes are already
preserved beside the vault, so a later write replaces nothing quarantine has not
saved, and refusing would leave the scope with no way forward but manual repair.

### `SecretEntry` never prints its values

`SecretEntry::fields` holds plaintext — provisioned provider keys, and for
captured entries the live `Authorization` headers and session cookies of whatever
the browser was signed into. Its `Debug` is written by hand and emits field
*names* and value *lengths* only: the entry is reachable from a `Debug` store
state, so a derived impl would put the whole vault one `tracing::debug!(?state)`
away.

## LLM Boundary

Raw captured-auth events are consumed only by `SecretStore`; they are not
members of ambient page signals, API-mining traces, tool results, telemetry, or
prompt inputs. Ambient browser distillation receives metadata-only page signals
and is pinned to a verified local provider.

Workflow compilation can be operation-mapped to a remote provider, so its user
prompt contains a scalar-free structural projection instead of stored sequence
values. The projection keeps sequence and capability ids, ordering, HTTP method,
parameter names, JSON field shape, response status, and locally inferred flow
seeds. It omits headers, cookies, storage, query/path values, task/execution ids,
browser arguments, descriptions, and all request/response scalar values. Local
deterministic post-processing applies replay literals after the LLM response.

## Replay Flow

- Replay resolves the request origin first.
- The runtime asks the captured-auth store for auth for that origin.
- Replay injects the available auth state and executes the request.
- Task Recipe replay maintains a request-local cookie jar. Valid `Set-Cookie`
  responses are applied to later steps only when domain, path, and `Secure`
  constraints match. The runner first resolves URL parameters, then loads auth
  for the concrete URL before resolving cookie-backed headers or bodies. A
  template such as `/tenants/{tenant}/items` must not filter out cookies scoped
  to the actual tenant's path. The auth-retry path uses the same resolution.
- SecretStore supplies cookie identity metadata (name/domain/path) only in
  memory; it is neither serialized nor accepted from session JSON. Response
  cookies replace the matching captured identity, preserving other same-name
  cookies and path ordering. Cookie parameter names are case-sensitive.
  `Expires` and `Max-Age` are honored, with `Max-Age` taking precedence regardless
  of attribute order. Expired entries remain in-run deletion markers so later
  captured-session lookups cannot resurrect them. The 256-cookie update bound
  includes these markers; exceeding it stops replay rather than silently losing
  a deletion. The durable captured-auth partition is not changed by this jar.
- Legacy/in-process session providers without routing metadata retain their
  name-based merge, with URL-scoped deletion suppression. Production SecretStore
  providers use exact identities.
- Newly compiled cookie-bearing steps retain a value-free `cookie_header`
  session requirement. If no cookies are available after concrete-URL lookup
  and in-run updates, the step fails before HTTP instead of accepting a 200
  anonymous response as task completion. Unauthenticated bootstrap steps do not
  inherit this requirement. Recipes carrying only an aggregate session hint fail
  conservatively when cookies are absent; recompile them to gain per-step
  cookie requirements.
- HTTP 401 marks the exact captured session generation stale. HTTP 403 remains
  an auth-related diagnostic but does not automatically poison the lease,
  because it can represent valid authentication with insufficient permission,
  tenant context, CSRF state, or another required application header.

### Automatic Task Recipe healing

Task Recipes use the same portable captured-auth refresh core as the explicit
origin refresh flow. The recipe runner creates a short-lived healer for its
scope and asks it only after a response is classified as stale authentication.
The refreshed session is looked up again and the same request is retried on the
same transport. Successful healing discards earlier in-run cookie updates for
that origin's cookie scope, so an old rotation or deletion cannot override the
fresh browser capture; unrelated origin cookies remain intact. Healing is bounded by `api_mining.auth_max_failures` across the
whole recipe run, not once per step, so a multi-origin recipe cannot create an
unbounded refresh loop.

Session values remain late-bound: recipe JSON stores only schemes such as
`authorization`, `query:apikey`, or `body:password`. Cookies, bearer tokens,
CSRF values, password fields, query credentials, and volatile signatures never
enter the recipe, matcher payload, run ledger, or lifecycle event. An unresolved
session/volatile value falls back before a transport send.

Auth failures increment their own recipe metric and appear in the run ledger,
but do not demote recipe maturity. Authentication is environmental state, not
evidence that the learned request shape is wrong. Schema/HTTP/network failures
continue to update replay statistics normally.

## Status And Consumers

The same store serves:

- replay execution
- executor and orchestrator auth-aware paths
- API Explorer auth-status surfaces

Origin-level fresh or stale state lets the runtime and UI distinguish expired
auth from general replay failures. A replay that succeeds with a captured
session lease marks the same captured generation fresh again, so transient
auth failures do not leave the UI pinned to stale after the material is proven
usable.

## Deterministic Refresh

The API Mining `Refresh Auth` action starts a short-lived Magicutor CDP session
for the selected scoped origin. It uses the signed-in browser profile, opens a
dedicated browser window, and waits up to 60 seconds for an authenticated
request. No agent or LLM participates.

The UI polls a non-secret status resource through these phases:

- starting the secure browser
- waiting for an authenticated request
- captured
- verifying with a concrete read-only capability
- verified, captured without an eligible verifier, timed out, or failed

The runtime closes and clears the dedicated CDP session after a terminal result.
Automatic verification respects origin replay policy and never chooses a write
capability. If an application does not make a protected request on initial page
load, the operator can navigate in the opened window while capture remains
active. Both the HTTP drain and WebSocket CDP connection resolve from the
configured `execution.magicutor_base_url`, including non-default deployments.
The drain reuses the orchestrator's configured Magicutor client, so its API-key,
timeout, and proxy policy remain authoritative.

## Explicit Non-Goals

- No standalone `AuthVault`; the `SecretStore` partition is the store.
- Auth refresh does not automate login, CAPTCHA, MFA, or application-specific
  navigation. It deterministically captures the authenticated traffic produced
  by the signed-in profile or by operator navigation inside the refresh window.
