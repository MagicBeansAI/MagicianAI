# Auth — scope is proven, not claimed

Plan:
`docs/archive/plans/2026-08-26-auth-identity-implementation-plan.md`
(from the
identity design
and the
workspace design).
Module: `magician/src/magician_v2/auth/`. Covers ScopeRef privatization,
bearer-bound workspace sessions/PATs, middleware enforcement, browser WebSocket
authentication, and workspace rotation across v2 and v3.

## Configuration

`auth:` in `magician-config.yaml`. A provider's secret is
`providers.<provider>.client_secret`, or, when that is empty, only its own
`MAGICIAN_AUTH_{GOOGLE|GITHUB}_CLIENT_SECRET` env var
(`AuthProviderConfig::effective_secret`).

```yaml
auth:
  mode: open            # bootstrap posture: anonymous/default only while NO identity exists (default)
  allow_signup: true    # false ⇒ login only for existing identities; also gates member provisioning
  session_ttl_days: 30
  providers:            # empty client_id or secret ⇒ that provider's routes answer 503
    google: { client_id: "" }
    github: { client_id: "" }
```

- `mode: open` is bootstrap only: while the identities file is empty, a missing
  bearer resolves to `anonymous/default`. The first login creates the owner
  (adopting `scopes/anonymous/`); from then on bearerless requests are refused
  as in credentials mode, so there is no forget-to-flip window.
- `mode: credentials` refuses from first boot: every authenticated route needs a
  general API bearer (`mag_` session or `mag_pat_` token), else `401` with
  `WWW-Authenticate: Bearer`.

Every session and API token is bound to exactly one owned workspace; headers,
query parameters and JSON fields cannot change authority. Switching workspace
rotates the session at `POST /auth/session/scope` and revokes the old token.
`plt_` terminal grants work only at the self-authenticating `/plane/mcp` door;
ordinary V2/V3 routes refuse them before the handler, so the grant's tool
allowlist cannot be bypassed through REST.

## The enforcement middleware

`auth::middleware::authenticate_request` is a `from_fn` layer on the
`/api/magician/v2` **and** `/api/magician/v3` scopes (tasks, monitors, events), run
before the Cloudflare Access layer. It classifies the bearer prefix, resolves
its principal and workspace from the store, discards caller
`x-principal`/`x-workspace`, and **engraves** the proven values (the
`cloudflare_access` pattern), so `api_scope::resolve_*` reads verified values.
It stamps `AuthenticatedRequest` (and `VerifiedRequestIdentity`) for **every**
accepted bearer, bot tokens included; engraving without the stamp would read as
anonymous.

`cloudflare_access::verify_access_middleware` runs next and reads
`Authorization` only for paired devices: a bearer is a device credential **only
when `X-Magician-Device-Id` names its pairing**; a device id without a bearer is
401; a bare bearer is left to the auth gate. `auth_api` tests wire both layers
in production order.

**Scripted app pages** get a file-serving proof (never an API identity) from
the live session in their asset URL, checked by the process-shared surface
verifier at the auth gate; it authorizes only `GET` of that installation's
reviewed page files. Host-open, bridge calls, entity queries and lifecycle ops
still need the bearer; an explicit invalid bearer is never rescued; caller scope
headers are discarded; expiry, teardown, a changed installation or changed
package/surface/grant revisions refuse serving. Cloudflare Access stays an
independent edge check. See
[custom surface credentials](custom-surfaces-v1.md#browser-asset-authentication).

**Logging**: password login emits one bounded `[AUTH] login minted session`
event (identity and scope names only); a rejected `GET /auth/session` logs a
bounded reason. Neither carries credential material.

**Public paths**: `POST /auth/login`, the social `start`/`callback` routes, the
one-time device enrollment exchanges, the exact ESP `POST /devices/pair`
bootstrap (real loopback peer with no forwarded-client metadata, or a verified
Cloudflare Access assertion; cloudflared on loopback counts as proxied), the
plane MCP door, the admin-secret-gated reconciler, and CORS preflight `OPTIONS`.

Everything else answers 401 + `WWW-Authenticate: Bearer` without a valid bearer
(open mode too, once any identity exists), with the API CORS headers
(`magician_v2::cors` constants) so browsers can read it. That CORS layer answers
every `OPTIONS` before routing, or a nested `web::scope` (`/notes`, `/apps`,
`/workspace-storage`, …) would 404 its preflight.

- A revoked session fails the *next* request; an open SSE/WebSocket stream
  finishes.
- Unknown usernames burn identical argon2 work (no timing oracle).
- Password-door throttle: an in-memory sliding window answers `429` +
  `Retry-After` after 8 failures per username or 40 per peer in 15 minutes,
  before argon2 (`LoginThrottle` in `magician-api/src/auth_api.rs`).
  Process-local by design; a successful sign-in clears that username's window,
  peer failures persist (one success cannot reset a spray).

Routes: `POST /auth/login` (also the **first-identity bootstrap**: with an empty
identities file and `allow_signup` it creates the owner, adopting
`scopes/anonymous/`), `POST /auth/logout` (sessions only), `GET /auth/session`
(identity, current and owned workspaces; for a bot token `identity` is `null`,
`method` is `bot_token`, `bot` names the daemon, `workspaces` is empty),
`POST /auth/session/scope` (rotate to an owned workspace), `POST /auth/tokens`
(PAT permanently bound to an owned workspace), and the terminal-grant routes
below. Login and session payloads return `principal` separately from
`identity.name`; clients partition UI caches and events by `principal` (the
first owner's scope root is `anonymous`).

## Shared installs — members

Roles are not modeled. The **first identity in the registry is the owner**;
only it may provision members via `POST /auth/identities`
(`{username, password, display_name?}`, gated by `allow_signup`). That needs a
login **session** (`403 session_required` for a PAT or terminal grant), returns
the identity and never a token, and mints a fresh scope root plus `default`
workspace (only the first identity adopts `anonymous`). Members cannot provision
identities (`403 owner_required`) or name workspaces they do not own. A
workspace switch rotates only the calling session.

Consumer-channel enrollment:

- `POST /chat/enroll` writes an auto-approved record into the **caller's
  tree**; an address any principal already holds is returned, not taken over.
  Enroll and approve share one process-wide lock. Pending records live only in
  the default tree; the code appears once, in the creation response (status
  says `pending` without it).
  `enrollment.default_principal` applies only before the first identity exists.
- `POST /chat/enroll/revoke`: members revoke their own channel, the owner
  anyone's; no reassignment (revoke, then re-enroll). Workspace is never a
  request field (engraved session scope); approve is the exception.
- `POST /chat/enroll/cancel` takes the creation **code** (~122-bit UUID) or, as
  owner, the channel identity. Cancel and approve are single-use.
- `POST /chat/enroll/approve`, like `/auth/admin/orphaned-scopes`, is public
  but takes `MAGICIAN_ADMIN_SECRET` as bearer (never a `mag_` token), with
  workspace in the body (default `default`); both admin doors share the password
  throttle under a reserved ledger key.

Web `(app)` routes require a session (`/login` carries `redirectTo`); the TopBar
workspace switch rotates the session; the Tauri tray shows the tray bearer's
resolved scope. Only an interactive session may mint an ESP device key into a
member scope; no bearer keeps the anonymous/default first-boot bootstrap. Magios
begin is auth-gated and carries the creating session's proven scope.

## Social login

`GET /auth/social/{google|github}/start` mints a single-use state ticket
(bounded at 64, 10-minute TTL) holding the server-side PKCE verifier and 302s to
the provider (Google `openid email profile`; GitHub `read:user user:email`).
`GET /auth/callback/{provider}` consumes the state (replay-dead), exchanges the
code server-side (the client never sees the access token) and resolves the profile **by provider subject id, never by email**:
known subject → session; unknown with `allow_signup` → derived identity
(`{provider}-{subject-slug}`, hash tail if sanitizing fails, numeric suffix on
collision); unknown with signup off → 403. `POST /auth/link/{provider}/start`
(session) binds the ticket to the session's identity so the callback attaches a
credential instead; no auto-link by email. The callback answers JSON to
programmatic clients and an HTML status page to the browser flow.

PKCE (RFC 7636) is hand-rolled over `sha2`/`base64`/`rand` (no `oauth2` crate);
`reqwest` calls sit behind a `SocialHttp` trait so flow tests run offline.

The `/authorize` + `/token` + DCR authorization server is deferred to the plane
program (its only clients are harnesses binding the loopback MCP endpoint via
`--mcp-config`); the interim door is the pasted `plt_` grant. Browser WebSocket
identity uses an auth-only `magician-bearer.<token>` offer after a stable
application protocol; the handler selects only the application protocol, so the
credential is never echoed in `Sec-WebSocket-Protocol`.

`GET /auth/admin/orphaned-scopes` (`MAGICIAN_ADMIN_SECRET`, constant-time
compare) reports scope roots with no owning identity, treating `system`,
`live-eval`, `local` and `storage-live-eval` as legitimate; it deletes and moves
nothing.

Accepted v1 risks: `redirect_uri` derives from the request Host (gated by the
provider's exact-match registration); a stolen link-ticket state could attach an
attacker's provider credential to the victim until browser-binding arrives with
`/authorize` (stealing it needs the victim's traffic on a loopback deployment).
Callback pages HTML-escape every interpolated byte, provider `error` included.

## The trust boundary

`ScopeRef`, the `(principal, workspace)` pair every principal-scoped API
resolves through, lives in `magician_v2::auth` with **private fields**;
`artifact_v2::service` re-exports it, but constructing one outside the auth
module is a compile error.

| Constructor | For | Rule |
|---|---|---|
| `ScopeRef::system_internal_unauthenticated(principal, workspace)` | schedulers, reconcilers, migrations, startup seeding (no HTTP request behind it) | ugly name keeps call sites greppable; nothing client-supplied may reach it |
| `ScopeRef::from_session(scopes_root, identity, workspace)` | bearer mint/rotation | principal only from the identity's `scope_root`; workspace must be owned before it is embedded |

Serde shape `{"principal": …, "workspace": …}`, so durable records (agentic
pause states) roundtrip.

## The workspace ownership registry

`scopes/<principal>/workspaces.json` is the **authorization** source of truth;
directories are the **isolation** mechanism. The `default` workspace (shown
"Personal") is minted with the identity, cannot be deleted or renamed, and is
created lazily (also for the adopted `anonymous` root). Creation is explicit,
slugs validated like principals.

API tokens: `POST/GET /auth/tokens`, `DELETE /auth/tokens/{id}`. Minting needs a
*session*, the value is shown once, listings carry id/label/timestamps only.

`GET/POST /workspaces`, `PATCH/DELETE /workspaces/{id}` always need a bearer
(even in open mode). `GET` returns summary cards (`workspace_summary`):
`agent_count` counts entries under `agent_runtime/definitions`,
`active_task_count` counts every entry under `tasks/` (finished ones included),
`last_activity` is the newest mtime among the workspace directory's direct
children; an unreadable directory yields `0` or no `last_activity`. `frozen` is
always `false` (no resource-authority freeze marker is read). UI:
[settings: workspaces](../unified-ui/settings-workspaces.md).

**Deletion.** `DELETE /workspaces/{id}` refuses the default and any workspace
whose directory holds files, and removes only the registry row.
`DELETE /workspaces/{id}?purge=true` answers `202` with
`"data_removal": "scheduled_for_next_start"`: the row goes at once and the id is
queued in `workspaces.json`'s `pending_purge` (every request for the scope is
refused from then);
`workspace_registry::drain_pending_purges` removes the directory at the next
server start, after the runtime lease and before any store discovers scopes (in
place is unsafe: per-scope caches have no eviction path, background workers skip
the ownership gate, the LLM trace journal would recreate the directory). Boot
sweeps run after the drain, so nothing resurrects it. The default is never
purged; an invalid or re-registered queued id is skipped. `POST /workspaces`
refuses a slug awaiting purge with `409 workspace_pending_purge`, distinct from
`workspace_has_live_state`.

Either DELETE also stops work on the scope, best effort and logged per step
(none can fail the request): `WakeUpQueue::cancel_scope`,
`AgentApiServices::evict_scope` (also cancels running goal pipelines),
`AuthStore::revoke_workspace_bearers` (sessions, tokens, `plt_` grants),
`BotTokenRegistry::revoke_scope`, `ScopedBotRuntime::shutdown_scope` (never
respawned or minted a token; the bot auth arm refuses and revokes a token whose
workspace is not live), and paired devices
and push registrations removed.

`auth::workspace_retirement` retires rows outside the directory at the next
start. Only a tombstone (`workspaces.json` `deleted`, or `pending_purge`)
triggers destructive retirement (`is_deleted_workspace`); mere absence
(`is_retired_workspace`, deleted or unlisted) only skips background work and
refuses to create a missing scope directory. A principal without a registry is
neither; an unreadable, empty or implausible registry fails open; the default
workspace and reserved sinks are never either. `create` lifts a slug's tombstone
once its purge has drained. Registry read-modify-writes hold
`workspaces.json.lock`.
Details: retiring a scope.

The security property: **a client may choose a workspace, but only from the set
its principal owns; a client may never choose a principal.**

## Share grants

`scopes/<principal>/<workspace>/share_grants.yaml`, in
`magician_v2::auth::share_grants` (workspace design §5b; "share grants" because
`plt_` owns "grant"):

```yaml
schema_version: 1
grants:
  - grantee: "<principal-name>"   # validated like every principal name
    classes: [memory_users, notes]
```

- **Deny by default**: no file, no federation, no error.
- **Per class, never blanket; read never confers write; nothing is copied.** Federation is at
  query time, so deleting the file revokes on the next read.
- `memory_index`, `secrets`, `auth` and unknown classes refuse the whole file
  (`ShareGrantsError::NonGrantableClass`).
- A corrupt or unreadable file is an error, never "no grants"; no source is
  silently skipped.

P1 (`federated_sources`/`federated_read`) federates the **workspace axis
only**: the same principal's sibling workspaces that grant a class to the
reader, excluding the reader's own workspace (read directly), as source-stamped `FederatedRecord { source, value }` for callers to
union. Cross-principal grants are not read; P2 adds them behind credentials
mode, refusing `scope_auth_required` until then.

The read path is deliberately unwired (today's only seam is a read-modify-write,
which would copy rows). Candidates: `GET /memory/entries`
(`magician-comms/src/channel_assist/memory_api.rs`), `agents/prompt_pipeline.rs`,
the notes `scopes_root` (`artifact_v2/workspace.rs`).

## Terminal grants

`plt_` terminal grants are the plane's durable door and the store's third mint
path, minted on `InvocationSurface::Plane` (owner audience, not a default direct
surface; [agent-definition-reference](agents/agent-definition-reference.md)).
`PlaneGrantRegistry` (`execution/plane/grant.rs`) is the run-scoped sibling:
same prefix, unpersisted, revoked when the run settles; the plane endpoint checks
it first, then this store.

Durable grants need a **session** to mint, are shown once, stored **hashed**,
listed without value or hash via `GET /auth/grants`, revoked by
`DELETE /auth/grants/{id}` (204/404), and always **expire**: `ttl_hours`
1..=2160 (90-day ceiling), else `400 invalid_ttl`; minting sweeps expired rows.
Mint fields: label, workspace, agent identity, harness engine, allowlist, expiry.

- **Workspace engraved at mint.** `POST /auth/grants` refuses an unowned one
  (`400 unowned_workspace`); the plane door re-checks ownership per request and
  ignores caller principal/workspace fields. Principal = minter's `scope_root`.
- **`NEVER_ON_THE_PLANE` floor**: `shell`, `run_coding_task`,
  `apply_code_proposal`, `run_project_checks`, `validate_skill`, `test_skill`, `install_skill`, `claude`, `codex`, `agy`,
  `opencode`, `list_proposals`, `interactive_process`. Mint drops them from
  `allowed_tools` (reported as `floor_filtered_tools`) and
  `TerminalGrant::permits` re-checks on every call.
- **`allowed_tools: []`** permits everything except the floor; a non-empty list
  permits only listed, non-floored tools.

`harness_engine` (`claude_code` / `magician` / …) is required; `magician` is
never a fallback. Unknown, revoked and expired grants all resolve to `None`. The
in-memory `PlaneGrant` must adopt this store rather than persist its own.

## Bot tokens

`mag_bot_` tokens (`auth/bot_tokens.rs`) are minted in memory for each bot the
runtime spawns from `scopes/<principal>/<workspace>/bots/`, injected as
`MAGICIAN_BEARER_TOKEN`, revoked when the bot stops, never written to disk (no
TTL, no refresh; a restart drops all and restarts the bots). Classified before
the `mag_` arm, resolved against the in-memory registry; unknown or revoked
fails closed.

The middleware engraves the grant's scope (a bot cannot name its own) and
stamps `bearer: BearerKind::Bot { bot_name }` with **`identity_name: None`**:

- `GET /auth/session` returns the engraved scope (`identity: null`,
  `method: "bot_token"`); the bot SDK's `resolveBearerScope()` learns its
  workspace this way.
- Session-only doors (`POST /auth/tokens`, `/auth/grants`,
  `/auth/session/scope`, `/auth/identities`) refuse with `session_required`
  (`require_session` → `SessionRequest`); `POST /auth/logout` answers
  `not_a_session`. Owner checks never pass as `None == None`.
- Enrollment reads the bot's principal from the stamp.

A bot bearer must be engraved *and* stamped, or `/auth/session` 401s and the bot
restart-loops. Test: `bot_token_resolves_its_session_scope_but_cannot_mint`.

## The unified credential store

`system/auth/` under the runtime root: four JSON files, written atomically
(temp+rename+fsync), mode `0600`.

| File | Holds |
|---|---|
| `identities.json` | name, display name, `scope_root` (login name and data directory decoupled) |
| `credentials.json` | argon2 PHC hashes and OAuth links keyed on **provider subject id, never email**; identity ↔ credential 1:N |
| `sessions.json` | `mag_…` sessions, **hashed** (SHA-256), 30-day default TTL, swept on mint |
| `tokens.json` | `mag_pat_…` tokens and `plt_…` grants, hashed; grants carry workspace, floored allowlist, expiry |

Classification is longest-prefix-first (`mag_pat_` before `mag_`). One door,
`AuthStore::resolve_bearer`, serves sessions, tokens and grants (`plt_` admitted
only via the plane door; see [Terminal grants](#terminal-grants)). The first
identity aliases `scopes/anonymous/` (`scope_root: "anonymous"`).
`verify_password` returns `None` for unknown user, missing credential,
malformed hash and wrong password alike. Mutations hold one lock
across read-modify-write; reads cost one mtime `stat`.

## Internal compatibility seam

`magician_v2::api_scope::resolve_required_scope_ref` still builds a `ScopeRef`
for existing handlers. New handlers extract `api_scope::ResolvedScope`, mapping
engraved headers through `LocalPermissive` into `magician_storage::ScopeId`;
caller headers are overwritten and query/body selectors are not authority. Open mode without a bearer engraves
only `anonymous/default`, only while the identity store is empty. A token→scope
`ScopeResolver` can later replace `LocalPermissive` without touching handlers.
