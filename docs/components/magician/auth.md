# Auth — scope is proven, not claimed

Plan:
`docs/archive/plans/2026-08-26-auth-identity-implementation-plan.md`
(from the
identity design
and the
workspace design).
Module: `magician/src/magician_v2/auth/`.

Covers ScopeRef privatization, bearer-bound workspace sessions/PATs,
middleware enforcement, browser WebSocket authentication, and workspace
rotation across v2 and v3.

## Configuration

`auth:` in `magician-config.yaml` (all three surfaces; live provider
secrets stay in env — each provider reads only its own
`MAGICIAN_AUTH_{GOOGLE|GITHUB}_CLIENT_SECRET`, never the other's):

```yaml
auth:
  mode: open            # bootstrap posture: anonymous/default only while NO identity exists (default)
  allow_signup: true    # false ⇒ login only for existing identities; also gates member provisioning
  session_ttl_days: 30
  providers:            # empty client ⇒ that provider's routes answer 503
    google: { client_id: "" }
    github: { client_id: "" }
```

`mode: open` is a **bootstrap posture, not a steady state**: while the
identities file is empty, a missing bearer resolves to the local
`anonymous/default` scope; the first login creates the owner identity
(adopting `scopes/anonymous/`), and from that moment bearerless requests
are refused exactly as in credentials mode — open never stays open past
the identity it was scaffolding, so a forget-to-flip window cannot exist.
`mode: credentials` is the same refusal from first boot: every
authenticated route requires a valid general API bearer (`mag_` session or
`mag_pat_` token) and answers `401` with `WWW-Authenticate: Bearer`
without one. Every session and API token is minted for exactly one owned
workspace. Clients cannot change authority with headers, query parameters, or
JSON fields; switching workspace rotates the session at
`POST /auth/session/scope` and revokes the previous session token.
`plt_` terminal grants are accepted only by the self-authenticating
`/plane/mcp` door; presenting one to an ordinary V2/V3 route is refused before
the handler so its tool allowlist cannot be bypassed through REST.

## The enforcement middleware

Scripted app pages have one additional credential transport: the live,
unguessable session in their asset URL authorizes `GET` of that installation's
reviewed page files. A sandboxed frame cannot send the owner's bearer header.
The API installs its process-shared surface verifier at the auth gate; successful
verification attaches a file-serving proof, not a general API identity.
Host-open, bridge calls, entity queries, and lifecycle operations still require
the normal bearer. Explicit invalid bearers are never rescued by the asset
credential, and caller scope headers are discarded. Expiry, teardown, a changed
installation, or changed package/surface/grant revisions refuse serving.
Cloudflare Access remains an independent edge check. See
[custom surface credentials](custom-surfaces-v1.md#browser-asset-authentication).

`auth::middleware::authenticate_request` — a `from_fn` layer on the
`/api/magician/v2` **and** `/api/magician/v3` scopes (the v3 task plane —
tasks, monitors, events), registered so it runs before the Cloudflare Access
layer. It classifies the bearer prefix, resolves its embedded principal and
workspace against the single store, and **engraves** the proven
`x-principal`/`x-workspace` in
place (the `cloudflare_access` pattern), so handlers calling
`api_scope::resolve_*` read verified values unchanged. Caller versions of those compatibility headers are discarded first.
It also stamps `AuthenticatedRequest` (and the `VerifiedRequestIdentity`
the Access layer's consumers read) into request extensions for handlers that
want provenance — for **every** bearer it accepts, runtime-minted bot tokens
included. Engraving the headers without the stamp is not "accepted": a
handler that reads the proven caller sees nothing and treats the request as
anonymous.

The Cloudflare Access layer (`cloudflare_access::verify_access_middleware`)
runs next and sees the same request. Its only interest in the `Authorization`
header is the paired-device case: a bearer is a device credential **only
when `X-Magician-Device-Id` names the pairing it belongs to**, and a device
id without a bearer is an incomplete credential (401). A bearer on its own is
the caller's session or API token, already resolved or refused by the auth
gate, and the access layer leaves it alone (reading it as an incomplete device
credential would refuse every fresh session). `auth_api` tests wire both layers
in production order.

Successful password login emits one bounded `[AUTH] login minted session`
event with identity and scope names but never the password or bearer. A
rejected `GET /auth/session` likewise emits a bounded rejection reason from
the bearer gate, without credential material. These paired events distinguish
password failure from a minted-then-rejected session without making the auth
log a token side channel.

Public paths (never general-bearer-gated): `POST /auth/login`, the social
`start`/`callback` routes, the one-time device enrollment exchanges, the exact
ESP `POST /devices/pair` bootstrap, the plane MCP door, and the
admin-secret-gated reconciler. ESP pairing still requires an actual loopback
peer with no forwarded-client metadata, or a verified Cloudflare Access
assertion, before it can mint a device bearer. A cloudflared connection whose
TCP peer happens to be loopback is therefore still treated as proxied. CORS
preflight `OPTIONS` requests pass through (they never carry credentials
by design). In credentials mode everything else answers 401 +
`WWW-Authenticate: Bearer` without a valid bearer — and in open mode the
same 401 applies once any identity exists, so the owner's adopted tree is
never reachable without a credential in either mode — and because the gate
short-circuits from the outermost wrap, the 401 carries the API CORS
headers itself (same constants the `magician_v2::cors` layer uses) so
browser clients read the error instead of an opaque network failure. That
CORS layer also answers every `OPTIONS` itself, before routing, because a
nested `web::scope` (`/notes`, `/apps`, `/workspace-storage`, …) never falls
back to a sibling catch-all route and its preflight would 404. A revoked session
fails the *next* request while an upgraded SSE/WebSocket stream
finishes. Unknown usernames burn identical argon2 work at login, so
response timing is not an enumeration oracle — and the password door is
**throttled**: an in-memory sliding-window ledger answers
`429` + `Retry-After` after 8 failures per username or 40 per peer in 15
minutes, before any argon2 work. Process-local by design — an attacker
able to restart the process to clear the ledger already owns the
machine. A successful sign-in clears that username's window so an honest
owner is never locked out; peer failures persist, so a spray cannot be
reset by alternating with one success.

Routes (additive): `POST /auth/login` (also the **first-identity
bootstrap**: an empty identities file with `allow_signup` creates the
owner, adopting `scopes/anonymous/`), `POST /auth/logout` (sessions only),
`GET /auth/session` (identity, current workspace, and owned workspaces; for
a bot token — see below — `identity` is `null`, `method` is `bot_token`, `bot`
names the daemon, and `workspaces` is empty),
`POST /auth/session/scope` (rotate to an owned workspace), and the
terminal-grant routes below. `POST /auth/tokens` accepts an owned workspace and
returns a PAT permanently bound to that workspace. Login and session payloads
return `principal` separately from `identity.name`: the first owner's login name
may differ from its adopted `anonymous` scope root, and clients must use the
server-returned principal for UI cache/event partitioning.

## Shared installs — members

Roles are not modeled. The **first identity in the registry is the owner**,
and only it may provision members via `POST /auth/identities`
(`{username, password, display_name?}`, gated by `allow_signup`).
Provisioning demands a login **session** (`403 session_required` for a PAT
or terminal grant), returns the identity never a token, and mints a fresh
scope root plus `default` workspace — only the first identity ever adopts
`anonymous`. Members cannot provision further identities
(`403 owner_required`) and cannot name workspaces they do not own. A
workspace switch rotates only the calling session's scope.

Consumer-channel enrollment is member-aware: `POST /chat/enroll` writes an
auto-approved record into the **caller's tree**. A global pre-scan returns
an address any principal already holds (idempotent, no takeover); enroll
and approve serialize under one process-wide lock. Pending records live
only in the default tree. A pending code travels once, in the creation
response — status reports `pending` without repeating it.
`enrollment.default_principal` is the unauthenticated fallback only before
the first identity exists.

`POST /chat/enroll/revoke`: a member revokes their own channel; the owner
revokes anyone's. No in-place reassignment — revoke, then the new holder
enrolls. Workspace is never a request field (engraved session scope);
the admin-secret approve door is the exception, taking it in the body.

`POST /chat/enroll/cancel` uses the creation **code** (~122-bit UUID) or,
as owner, the channel identity. Cancel and approve are single-use.

`POST /chat/enroll/approve` is a public auth path like
`/auth/admin/orphaned-scopes`. It carries `MAGICIAN_ADMIN_SECRET` as the
Authorization bearer — never a `mag_` token — takes workspace in the body
(default `default`), and both admin doors share the password-door throttle
under a reserved ledger key.

Web `(app)` routes require a live session (`/login` carries `redirectTo`).
TopBar workspace switch rotates the session. The Tauri tray shows the tray
bearer's resolved scope. ESP pairing is member-aware: only an interactive
session may mint a device key into a member scope; no bearer keeps
anonymous/default first-boot bootstrap. Magios begin is auth-gated and
carries the creating session's proven scope.

## Social login

`GET /auth/social/{google|github}/start` mints a single-use state ticket
(bounded at 64, 10-minute TTL) holding the server-side PKCE verifier,
and 302s to the provider (Google `openid email profile`; GitHub
`read:user user:email`). `GET /auth/callback/{provider}` consumes the
state (replay-dead), exchanges the code server-side — the client never
sees the access token — and resolves the profile **by provider subject
id, never by email**. Known subject → session; unknown subject with
`allow_signup` → derived identity (`{provider}-{subject-slug}`, hash
tail when sanitizing cannot produce a legal name, numeric suffixes on
collision); unknown with signup off → 403. `POST
/auth/link/{provider}/start` (behind a session) binds the ticket to the
session's identity so the callback attaches a credential instead of
creating one — no auto-link by email, ever. The callback answers JSON
for programmatic clients and an HTML status page for the browser flow.

RFC 7636 PKCE is implemented directly over `sha2`/`base64`/`rand`
rather than pulling the `oauth2` crate (compiled without HTTP in this
workspace) plus an adapter — four `reqwest` calls behind a `SocialHttp`
trait that tests fake, so every flow test is offline.

The `/authorize` + `/token` + DCR authorization server is deferred into
the plane program: the plane plan already specifies the loopback MCP
endpoint that every terminal harness binds via `--mcp-config` with a
bearer, and the OAuth AS exists solely to serve exactly those clients.
The interim MCP door is the pasted `plt_` grant (terminal grants,
below). Browser WebSocket identity uses an auth-only
`magician-bearer.<token>` offer after a stable application protocol. Middleware
consumes the bearer and the handler selects only the application protocol, so
the credential is never echoed in `Sec-WebSocket-Protocol`.

The read-only reconciler: `GET /auth/admin/orphaned-scopes`
behind `MAGICIAN_ADMIN_SECRET` (constant-time compare) reports scope
roots with no owning identity before any enforcement flip, recognizing
`system`, `live-eval`, `local`, and `storage-live-eval` as legitimate
non-login roots. It deletes and moves nothing.

Accepted v1 risks, from the OAuth review: every byte interpolated into
a callback page is HTML-escaped (provider `error` params included);
`redirect_uri` is derived from the request Host but gated by the
provider's exact-match registration; a stolen link-ticket state could
attach an attacker's provider credential to the victim's identity —
mitigating that needs browser-binding (cookie/nonce), which arrives
with the `/authorize` plan, and stealing it requires reading the
victim's traffic on a loopback deployment.

## The trust boundary

`ScopeRef` — the `(principal, workspace)` pair every principal-scoped API
resolves through — lives in `magician_v2::auth` with **private fields**.
`artifact_v2::service` re-exports it, so historical import paths keep
resolving, but constructing one outside the auth module is a compile error.

Two ways to build a scope, by design:

| Constructor | For | Why |
|---|---|---|
| `ScopeRef::system_internal_unauthenticated(principal, workspace)` | schedulers, reconcilers, migrations, startup seeding — anything with no HTTP request behind it | the name is deliberately ugly so every new call site is greppable in review; nothing client-supplied may reach it |
| `ScopeRef::from_session(scopes_root, identity, workspace)` | authenticated resolution during bearer mint/rotation | principal comes only from the identity's `scope_root`; workspace must be owned before it is embedded in the credential |

## The workspace ownership registry

`scopes/<principal>/workspaces.json` is the **authorization** source of
truth; directories remain the **isolation** mechanism. The default
workspace (`default`, displayed "Personal") is minted with the identity,
cannot be deleted or renamed, and is created lazily on first touch —
including for the adopted `anonymous` root, whose data already exists.
Creation is explicit only, with slugs validated by the same pattern as
principals (workspaces are directory names too). Deletion refuses the
default and refuses any workspace whose directory still has content —
clearing data is a deliberate manual act, and only the registry row is
ever deleted.

API tokens: `POST/GET /auth/tokens`, `DELETE
/auth/tokens/{id}` — minting is behind a *session* only (a token cannot
mint tokens), the value is shown exactly once at mint, and listings
carry id/label/timestamps only.

## Share grants (P1)

`scopes/<principal>/<workspace>/share_grants.yaml` is the sharing layer of
the workspace design §5b, shipped as `magician_v2::auth::share_grants`
(terminology: **share grants** — `plt_` grants own the word "grant" in
HTTP-land):

```yaml
schema_version: 1
grants:
  - grantee: "<principal-name>"   # validated like every principal name
    classes: [memory_users, notes]
```

The rules, enforced at load, not delegated to callers:

- **Deny by default** — no file means today's behavior exactly: zero
  federation, no error.
- **Per class, never blanket; read never confers write; nothing is
  copied** — federation happens at query time, so revocation (delete the
  file) takes effect on the next read.
- **Non-grantable classes fail loudly** — `memory_index` (derived,
  rebuildable), `secrets`, and `auth` can never be listed; a file naming
  one refuses to load (`ShareGrantsError::NonGrantableClass`), and unknown
  class names fail the same way (typo protection).
- **Fail closed on corruption** — an unreadable or corrupt grant file is
  an error, never "no grants"; a source is never silently skipped.

P1 lookup (`federated_sources`/`federated_read`) federates the **workspace
axis only**: sibling workspaces of the *same* principal that grant a class
to the reader's principal, excluding the reader's own workspace (own scope
is read directly, never "federated"). Results are source-stamped
(`FederatedRecord { source, value }`), and federation returns *granted
sources only* — callers union with their own-scope results themselves.
Cross-principal grants (§5b's consent axis) are out of P1: those files are
never read, and P2 adds them behind credentials-mode enforcement with a
`scope_auth_required` refusal until it flips.

The read-path wiring (memory.users, notes) is deliberately unwired: the only
file-read seam is the read half of read-modify-write cycles, where federation
would launder federated rows into the reader's own store on next persist — a
"nothing copied" violation. Candidate seams: the comms `GET /memory/entries`
handler (`magician-comms/src/channel_assist/memory_api.rs`) and/or
`agents/prompt_pipeline.rs` for memory.users; the notes store's `scopes_root`
(`artifact_v2/workspace.rs`) for notes.

## Terminal grants

`plt_` terminal grants — the plane's durable door — are the third mint path
of the same credential store. The live `PlaneGrantRegistry`
(`execution/plane/grant.rs`) is the run-scoped sibling: same `plt_` prefix, not
persisted, revoked when the run settles. The plane endpoint resolves the live
map first, then this store. The surface those grants mint on is
`InvocationSurface::Plane` — owner audience, not a default direct
surface; see [agent-definition-reference](agents/agent-definition-reference.md).
Durable grants are minted behind a **session** only, an opaque value shown
exactly once, stored **hashed**, revoked by `DELETE /auth/grants/{id}`
(204/404), listed without value or hash via `GET /auth/grants`. Unlike API tokens
they always **expire**: `ttl_hours` 1..=2160 (an hour to a hard 90-day
ceiling — no perpetual grants); out-of-range ttl answers `400
invalid_ttl`, and minting sweeps expired rows so the file stays bounded.

Three properties make a grant a grant:

- **The workspace is engraved at mint.** `POST /auth/grants` refuses a
  workspace the minting identity does not own (`400 unowned_workspace`)
  and engraves it into the record. The plane MCP door resolves every grant
  request to exactly that workspace; the general API middleware rejects the
  grant class. Caller-supplied principal/workspace
  headers or query fields are ignored; scope is not a per-request choice.
  The mint-time ownership check re-runs per request. The principal is the minting identity's
  `scope_root`, as with sessions.
- **A `NEVER_ON_THE_PLANE` floor.** `shell`, `run_coding_task`,
  `apply_code_proposal`, `run_project_checks`, `claude`, `codex`,
  `agy`, `opencode`, `list_proposals`, `interactive_process` can never be
  granted. Mint filters them out of
  `allowed_tools` and reports exactly what it dropped
  (`floor_filtered_tools` in the mint response — a documented drop, not
  a silent one), and `TerminalGrant::permits` re-checks the floor on
  every call (belt and braces: no hand-edited record widens past it).
- **Empty allowlist = unscoped-but-floored.** `allowed_tools: []`
  permits everything *except* the floor — empty is "the catalog minus
  the denied families", never "all of the dangerous catalog". A
  non-empty list permits only listed tools, and never a floored one.

`harness_engine` (`claude_code` / `magician` / …) is required and
explicit — pinning `magician` is a deliberate act, never a fallback.
Minting is an authorization decision, not a form: label, workspace,
agent identity, harness engine, allowlist, expiry. Unknown, revoked, and
expired grants resolve identically (`None`), so a terminal cannot tell
them apart. The in-memory `PlaneGrant` must adopt this store rather than
persist its own.

HTTP surface: `GET/POST /workspaces`,
`PATCH/DELETE /workspaces/{id}`, always behind an authenticated bearer
(even in open mode — new surfaces never ride the legacy claim path).
`GET` returns summary cards; the counts
(`agent_count`, `active_task_count`, `last_activity`) are best-effort
directory-level reads, and `frozen` stays `false` until a
resource-authority freeze marker gains a cheap read path.

`DELETE /workspaces/{id}` refuses a workspace whose directory still holds
files. `DELETE /workspaces/{id}?purge=true` deletes it with its data and
answers `202` with `"data_removal": "scheduled_for_next_start"`: the
registry row goes at once — so every request for the scope is refused from
that moment — and the id is queued in `workspaces.json`'s `pending_purge`.
The directory is removed at the next server start by
`workspace_registry::drain_pending_purges`, which runs on the server path
only, after the runtime lease is held and before any store discovers scopes.
Not on the spot, because the running service cannot let go of a hydrated
scope: per-scope caches (analytics, feed and UI-thread DuckDB, social SQLite,
the events log, agent-scheduler caches) hold handles with no eviction path,
background workers do not pass the ownership gate, and the LLM trace journal
would recreate a deleted directory from its in-memory sequence. Every boot sweep enumerates
directories, so a directory removed before them cannot be resurrected. The
default workspace is never purged; a queued id that is not a valid workspace
name, or has been registered again, is skipped rather than obeyed; and
`POST /workspaces` refuses a slug still waiting to be purged — `409
workspace_pending_purge`, distinct from `workspace_has_live_state` so a caller
can say which it is — since the new workspace would otherwise inherit the old
one's data. Settings → **Workspaces** drives all of this
([settings: workspaces](../unified-ui/settings-workspaces.md)).
The serde shape is unchanged (`{"principal": …, "workspace": …}`), so durable
records — agentic pause states above all — roundtrip identically.

The security property, one line: **a client may choose a workspace, but only
from the set its principal owns; a client may never choose a principal.**

## Bot tokens

`mag_bot_` tokens (`auth/bot_tokens.rs`) are the bearer the runtime mints
for each bot daemon it spawns from `scopes/<principal>/<workspace>/bots/`.
The runtime already knows the bot's scope with certainty; the token only
re-establishes that fact at the HTTP boundary. It is minted in memory
immediately before the spawn, injected as `MAGICIAN_BEARER_TOKEN`, revoked
when the bot stops, and never written to disk — so there is no TTL and no
refresh. A runtime restart drops every grant, which is correct: the bots
are its children and restart with it. Classified before the `mag_` session
arm (it shares the prefix) and resolved against the in-memory registry
before the store is consulted; unknown or revoked fails closed.

The middleware stamps it like any other accepted bearer: scope engraved
from the grant (a bot cannot name its own), `AuthenticatedRequest` with
`bearer: BearerKind::Bot { bot_name }` and **`identity_name: None`** — no
person stands behind it, the runtime is the minter. The consequences fall
out of that `None`:

- `GET /auth/session` answers with the engraved scope (`identity: null`,
  `method: "bot_token"`, `bot`, empty `workspaces`). The bot SDK's
  `resolveBearerScope()` calls exactly this before the adapter starts, so it
  is how a bot learns its workspace.
- The session-only doors — `POST /auth/tokens`, `/auth/grants`,
  `/auth/session/scope`, `/auth/identities` — refuse it with
  `session_required` (`require_session` narrows to a `SessionRequest` whose
  identity is a fact of the type). `POST /auth/logout` answers
  `not_a_session`: the runtime revokes it. Owner checks (enrollment revoke /
  cancel) never match a caller with no identity, spelled out so an install
  with no identities yet cannot let a bot pass as `None == None`.
- Enrollment reads the bot's principal off the stamp instead of falling back
  to the configured default.

A bot bearer must be both engraved *and* stamped: the bot SDK calls
`resolveBearerScope()` → `/auth/session` before the adapter starts, so an
unstamped bot token 401s and restart-loops. Test:
`bot_token_resolves_its_session_scope_but_cannot_mint`.

## The unified credential store

`system/auth/` under the resolved runtime root holds four small JSON
documents, each written atomically (temp+rename+fsync) with owner-only
permissions (`0600`):

| File | Holds |
|---|---|
| `identities.json` | who exists — name, display name, `scope_root` (login name and data directory are decoupled) |
| `credentials.json` | how they prove it — argon2 password hashes (PHC strings) and OAuth links keyed on **provider subject id, never email**; identity ↔ credential is 1:N |
| `sessions.json` | `mag_…` login sessions — stored **hashed** (SHA-256), 30-day default TTL, swept on mint |
| `tokens.json` | `mag_pat_…` API tokens and `plt_…` terminal grants, both stored hashed; grants carry their engraved workspace, floored allowlist, and expiry |

Bearer classification is longest-prefix-first (`mag_pat_` before `mag_`,
because the former starts with the latter); `plt_` terminal grants
resolve from the same store but are admitted only through the plane MCP door
(see [Terminal grants](#terminal-grants)).
The store is one subsystem by
design (identity doc §7): one resolve door (`AuthStore::resolve_bearer`)
serves sessions, tokens, and grants, so no mint path grows a parallel
registry.

Notable semantics: the **first** identity created on an install adopts
`scopes/anonymous/` (`scope_root: "anonymous"`) — migration is aliasing, not
moving. `verify_password` returns `None` for unknown user, missing
credential, malformed hash, and wrong password alike, and burns comparable
argon2 time for unknown users so timing is not an enumeration oracle.
Concurrent mutations hold one lock across read-modify-write, so two mints
cannot lose an update; reads are mtime-checked pass-throughs costing one
`stat` per request.

## Internal compatibility seam

`magician_v2::api_scope::resolve_required_scope_ref` remains the compatibility
path that builds a `ScopeRef` for existing handlers. New handlers should extract
`api_scope::ResolvedScope`, which maps the engraved headers through
`LocalPermissive` into `magician_storage::ScopeId`. The bearer middleware first
resolves the credential and engraves proven internal `x-principal`/`x-workspace`
values; caller values are overwritten and query/body selectors are ignored as
authority. In local `open` mode without a bearer, middleware engraves only
`anonymous/default` while the identity store is empty—a caller cannot manufacture
another scope. Once the first identity exists, the same request is refused. A
later token→scope `ScopeResolver` can replace `LocalPermissive` without
sweeping handlers.
