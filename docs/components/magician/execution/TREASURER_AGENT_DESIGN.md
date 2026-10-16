# Unified Secret Management Architecture

Current-state contract for Magician secret handling. Policy, grants, injection,
sanitization, and audit live in `magician/src/magician_v2/secrets/`. `treasurer`
is a compiled capability adapter over that module, not an LLM agent and not the
storage root.

## Problem

Three secret sources share one execution pipeline — store a value, issue an
opaque reference, resolve at execution time, inject into the action, sanitize
the observation:

1. **Provisioned** — user-saved vault secrets (API keys, card details).
2. **Captured** — browser-trace auth (headers, cookies, storage tokens).
3. **Ephemeral** — user-typed values during a run (passwords, OTPs, PINs).

## Goals

1. One in-process module for types, storage, policy, injection, and sanitization;
   one executor injection path; one result sanitizer over actual known values.
2. No LLM in the secret path — resolution is compiled Rust.
3. Policy on provisioned secrets only; captured and ephemeral skip policy because
   their authority is the user or the browser.
4. Fail closed when the OS keychain is unavailable: disable durable
   provisioned/captured features, keep ephemeral, surface the state.

Non-goals: payment processing, LLM-based secret management, `treasurer` as the
architectural root, cross-process vault sharing or locking.

## Layering Principle

1. **Foundation:** `magician_v2/secrets/` is the secret service.
2. **Runtime path:** the executor and API-mining replay call that service.
3. **Provisioning path:** localhost API + `/vault` UI write provisioned entries.
4. **Adapter path:** optional compiled `treasurer` for planner-visible grants.

The adapter is thin. It must not grow a second policy engine, approval table,
audit log, or store.

## Architecture Overview

```text
        SecretStoreResolver  (one SecretStore per principal/workspace)
     magician_data_v3/scopes/<principal>/<workspace>/secrets/
  ┌─────────────────────────────────────────────────────────────┐
  │  provisioned_secrets.vault | captured_secrets.vault         │
  │  mcp_oauth.vault           | secret_audit.jsonl (append)    │
  │  ephemeral: in-memory only                                  │
  │  Vault | Grants | Policy | Inject | Sanitize                │
  └─────────────────────────────────────────────────────────────┘
           ↑                         ↑
    bootstrap_secret_runtime   executor / replay / vault API
           │                         │
           └──────────┬──────────────┘
                      │
         SecretBroker contract (same request/response)
           LocalSecretBroker      — in-process over a bound store
           ScopedSecretBroker     — resolves store from with_secret_scope
           CapabilitySecretBroker — planner-visible treasurer transport
```

### The Unified Pipeline

```text
selector or grant -> resolve -> policy check -> inject -> execute -> sanitize
                         ↑            ↑
                  vault lookup    actual runtime action
                  or task store   + actual target domain (HTTP URL)
```

Provisioned secrets have two transports:

- **Local runtime:** stable `credential_id` on the action.
- **Adapter:** short-lived `credential_token` / grant from `treasurer`.

Both redeem through `SecretStore::redeem_grant` and inject only after the grant
binding matches `action.trust_tool_action()` plus the derived domain.

## Three Secret Sources - One Engine

| | Provisioned | Captured | Ephemeral |
|---|---|---|---|
| **What** | Cards, API keys, passwords the user saved | Auth headers, cookies, storage, query params from traces | OTPs, passwords typed during a run |
| **Input** | `/vault` UI / localhost `/api/magician/v2/secrets` | Browser capture → `store_captured*` | `PendingInput` via `is_secret_param_name` |
| **Storage** | Encrypted on disk | Encrypted on disk, per origin | In memory only |
| **Encryption** | AES-256-GCM + OS keychain master key | Same | Not persisted |
| **Reference** | `credential_id` locally; `credential_token` via adapter | `SecretRef::Session(origin)` on the replay path | `SecretRef::Placeholder(id)` as `[REDACTED:id]` or `[REF:id]` |
| **LLM visibility** | id / label; grant id on the adapter path | None | Placeholder only |
| **Placement** | `InjectionTarget` at provision time | Replay injects headers/cookies | LLM places the placeholder; executor substitutes |
| **Policy** | Full: routes, domains, budget, approval | None | None |
| **Staleness** | N/A | Generation + CAS (`mark_stale` / `mark_stale_lease`) | N/A |

## `secrets` Module

In-process Magician module, not a separate crate. Security comes from how the
executor uses the API, not from a crate boundary.

```
magician/src/magician_v2/secrets/
    mod.rs                         types + bootstrap_secret_runtime
    store.rs                       facade: SecretStore, SecretStoreResolver (magicvault-core)
    encryption.rs                  AES-256-GCM, MasterKeyProvider, app-data keys
    injection.rs                   inject / inject_inline / sanitize / cookies
    policy.rs                      facade: SecretPolicy, grants, approvals (magicvault-core)
    classify.rs                    is_secret_param_name name/wording heuristics
    challenge.rs                   typed authentication challenges
    sinks.rs                       approved credential sinks
    broker.rs                      SecretBroker + Local / Scoped / Capability
    credential_lifecycle_executor.rs   governed CLI auth process tree
    credential_material_adapter.rs     sealed tool-runtime-core materialization
    mcp_oauth_vault.rs             MCP SDK OAuth vault over SecretStore
    operator_profile_adapter.rs    non-secret operator profile projection
    runtime_credential_audit.rs    metadata-only governed-runtime receipts
```

The durable partitions, grant/approval tables and `StoreConfig` live in the
`magicvault-core` crate (MagicVault git dependency, root `Cargo.toml`);
`store.rs` / `policy.rs` re-export them and add the product scope layout.

### Core Types

```rust
struct SecretEntry {
    id: String,
    label: String,                          // safe for LLM context
    fields: HashMap<String, String>,        // plaintext values; Debug redacts them
    source: SecretSource,
    injection: InjectionTarget,
    policy: Option<SecretPolicy>,           // provisioned only
    created_at: i64,
}

enum SecretSource {
    Provisioned,
    Captured { origin: String, generation: u64, stale: bool, expires_hint: Option<i64> },
    Ephemeral { task_id: String },
}

enum SecretRef {
    Provisioned(String),   // stable local id
    Grant(String),         // single-use credential_token
    Session(String),       // captured origin; not LLM-visible
    Placeholder(String),   // ephemeral inline ref
}

enum InjectionTarget {
    Header { name: String, prefix: Option<String> },
    FormFields(HashMap<String, String>),
    Cookies(Vec<CookieSpec>),
    Inline,                // ephemeral only; store_provisioned rejects this
}

struct CookieSpec {
    name: String,
    domain: String,
    path: String,
    secure: bool,
    http_only: bool,
    same_site: Option<SameSite>,   // Strict | Lax | None
    expires: Option<i64>,
}
```

### Policy (Provisioned Secrets Only)

```rust
struct SecretPolicy {
    allowed_tools: Vec<String>,     // exact `tool:action`, e.g. "http:post", "browser:execute"
    allowed_domains: Vec<String>,   // exact host or `*.example.com`
    max_uses_per_day: Option<u32>,  // in-memory UsageTracker
    requires_approval: bool,
}
```

Write-time catalog: HTTP verbs `http:get|post|put|patch|delete|head|options`
and `browser:execute`. Empty `allowed_tools` / `allowed_domains` means unrestricted
on that axis. Grants and approval challenges default to **300s** TTL
(`StoreConfig::default_grant_ttl_secs`, `ApprovalTable::new(300)`).

### `fields` Convention

| Source | Keys |
|--------|------|
| Provisioned | User-defined (`card_number`, `api_key`, …) |
| Ephemeral | Single `"value"` |
| Captured | `header:<name>`, `query_param:<name>`, `cookie:<index>:<name>`, `local_storage:<key>`, `session_storage:<key>` |

Cookie metadata lives on `InjectionTarget::Cookies`; `fields` holds values.
Indexed cookie keys preserve duplicate names across path/domain scopes.
`filter_cookies_for_url` uses host, RFC 6265 path-prefix, `Secure`, and expiry,
then prefers the most specific path/domain when names collide.

### Encryption

AES-256-GCM, layout `12-byte nonce || ciphertext || tag`. `MasterKeyProvider`
implementations: `KeychainProvider` (macOS Keychain / Linux Secret Service /
Windows Credential Manager) and `InMemoryKeyProvider` (tests/CI).

`bootstrap_secret_runtime()` probes the OS backend once. If the keychain is
missing or the master key cannot be loaded, `SecretRuntimeCapabilities` sets
provisioned and captured to **Disabled**, leaves ephemeral **Available**, and
records a startup warning. Treasurer is not registered unless
`treasurer_enabled()` (provisioned Available). The runtime does not silently
persist “encrypted” vault files under an in-memory key.

Corrupt partition files are quarantined as `*.corrupt-<uuid>`. The store stays
up with that partition empty and `SecretPartitionStatus::Unavailable`, so a
failed decrypt is not presented as “this scope has no secrets.”

## Sanitization

`sanitize_result(result, known_values)` walks the observation returned to the
model. `known_values` is a `KnownSecretValues` — the plaintext resolved during
this turn's injection, held only to scrub output: it zeroizes on drop, prints
names and lengths only under `Debug`, and is pinned `!Clone + !Serialize`.

Replacements come from one function, `known_value_replacements`, shared with
the browser dispatcher and the evidence redactor. Empty values are dropped;
every remaining value is replaced exactly regardless of length (OTPs and PINs
included). For values of **eight bytes or more** the encoded forms a
model-authored command can trivially produce are replaced too: base64 (standard
and URL-safe, padded and not), hex (both cases), percent-encoding, and JSON
string escaping (so `echo $SECRET | base64` does not leak). The 8-byte floor
keeps a short value's hex from over-redacting ordinary text. Token:
`[REDACTED]`; longer strings are applied first. Not covered: HTML entities,
values split across lines, arbitrary transforms (rot13, xor), encodings of
values under 8 bytes. Redaction is defense in depth, never the boundary — the
boundary is that the model never holds the value.

| `ActionResult` | Strategy |
|----------------|----------|
| `Text` | Substring replace in `content` |
| `Http` | Body and each header value |
| `Browser` | Recursive string walk of the JSON tree |
| `List` | Each item |
| `Binary` | Skipped (encoding makes substring match unreliable) |
| `Success`, `Bool` | No string content |

A separate last-mile `sanitize_json_for_provider` exists for provider/telemetry
boundaries. It is **not** the canonical-result redactor; dispatch uses the
exact injected values and their encoded variants.

## Staleness (Captured Secrets Only)

```text
get_session(origin, replay_url) → (SessionContext, CapturedSessionLease)
execute replay
on 401/403: mark_stale_lease(&lease)  // CAS per origin/generation in the lease
```

`get_session` filters cookies for the **replay URL**, not the raw template, and
may merge cookies from other captured origins whose cookies match that URL.
`mark_stale(origin, generation)` remains the single-origin CAS primitive.

Captured writes merge (headers, cookies, storage, query params) instead of
replacing the origin entry. Duplicate cookie names keep indexed values through
outbound replay headers.

## Scope Resolution

Stores are **not** a process-global singleton. `SecretStoreResolver` keys
`Arc<SecretStore>` by `(principal, workspace)` under

`magician_data_v3/scopes/<principal>/<workspace>/secrets/`

containing `provisioned_secrets.vault`, `captured_secrets.vault`,
`mcp_oauth.vault`, and `secret_audit.jsonl`. All four are written owner-only
(`0600`) inside an owner-only directory (`0700`): the partitions after the
durable write, the OAuth partition at temp-file creation so the rename
publishes owner-only, and the journal on every append — which also tightens a
journal that predates the rule.

- Vault HTTP handlers call `resolve_for_scope` from the verified request scope.
- Treasurer uses `ScopedSecretBroker`, which reads `with_secret_scope`.
- The agentic executor is handed the already-resolved `executors.secret_store`
  and uses `LocalSecretBroker` against that store.

Each `SecretStore` uses `RwLock` state plus per-partition write locks. Ephemeral
entries never hit disk.

### Ephemeral lifecycle

Scope id: `execution:{id}` if present, else `task:{id}`, else a legacy thread
key, else `goal:{hash}`. `sync_ephemeral_secrets` clears the scope, then
`register_ephemeral(scope_id, input_id, value)` for each resolved input whose
parameter matches `is_secret_param_name` (`password`, `secret`, `token`, `cvv`,
`otp`, word-bounded `pin`) in `secrets/classify.rs` (re-exported from
`execution/agentic/types.rs`).
`EphemeralSecretScopeGuard` calls `clear_ephemeral` on drop.

Pause persistence blanks secret-named `resolved_inputs`, so a resumed run cannot
re-register user-typed secrets from JSON.

```rust
fn register_ephemeral(scope_id: &str, input_id: &str, value: String)
    -> Result<SecretRef, SecretStoreError>;
// SecretSource::Ephemeral { task_id: scope_id }, InjectionTarget::Inline
// → SecretRef::Placeholder(input_id)

fn inject_inline(action, store, ephemeral_scope_id: Option<&str>)
    -> (ExecutableAction, HashMap<String, String>);
```

`inject_inline` substitutes `[REDACTED:<id>]` and `[REF:<id>]`. Browser delivery
does not mutate a browser action variant; the browser skill dispatcher calls
`resolve_inline_placeholders` on argv/stdin immediately before spawn. Both return
the resolved action or text together with the turn's `KnownSecretValues`.

## Brokers

`SecretBroker` is the shared contract:

```rust
struct BrokerAccessRequest {
    credential_id: String,
    tool: String,
    action: String,
    domain: Option<String>,
    ttl_secs: Option<i64>,
}

enum BrokerAccessResponse {
    Issued { credential_id, credential_token },
    Denied { credential_id, reason },
    NeedsApproval { credential_id, challenge_id },
    NotFound { credential_id },
}
```

`list_available` returns `{ id, label }` only — never field names, policy, or
values.

| Broker | Binding |
|--------|---------|
| `LocalSecretBroker` | A concrete `Arc<SecretStore>` (executor fast path) |
| `ScopedSecretBroker` | `SecretStoreResolver` + task-local `with_secret_scope` |
| `CapabilitySecretBroker` | Invokes the compiled `treasurer` pack |

`TreasurerCapabilityProvider` must be backed by a non-capability broker
(`ScopedSecretBroker` at startup). Wiring `CapabilitySecretBroker` into the
provider would recurse.

## Unified Executor Flow

`prepare_action_with_secrets` in `execution/agentic/executor.rs`:

1. `inject_inline` for ephemeral placeholders.
2. Reject specifying both `credential_id` and `credential_token`.
3. `has_unresolved_refs` → block execution (no placeholder leak).
4. Provisioned path (requires `treasurer_enabled()`):
   - Derive `(tool, action)` from `trust_tool_action()`.
   - Derive domain from the **HTTP URL** (`derive_secret_target_domain`; other
     action types currently yield `None`).
   - `credential_id` → `LocalSecretBroker::request_credential` (issues a grant
     after `check_policy` against that real route/domain).
   - `credential_token` → redeem the already-issued grant.
   - `NeedsApproval` pauses through the existing HITL/approval UI.
   - Redeem, `binding.matches(tool, action, domain)`, then `inject()`.
5. Execute the action.
6. `sanitize_result` with every value injected this turn; then `record_usage`.

`inject()` applies `Header` / `FormFields` / `Cookies` to **HTTP** actions.
`FormFields` requires a JSON object body (`InjectionError::NonJsonBody`).
`Inline` is not valid on `inject()` — use `inject_inline`. Captured replay stays
on the API-mining path (`get_session`), not this provisioned inject.

## Optional Treasurer Capability Adapter

Pack: `magician/src/magician_v2/execution/embedded_pack_defs/treasurer.yaml`
(embedded compiled def, not a `capability_templates/packs/` file).

Provider: `execution/treasurer_provider.rs`, registered in
`build_compiled_registry` only when a secret broker is supplied.

Operations:

- `list_available`
- `request_credential` (`credential_id`, `target_tool`, `target_action`,
  optional `target_domain`, optional `ttl_secs`)

Statuses: `issued`, `denied`, `needs_approval`, `not_found`. Trust policy can
restrict `treasurer` like any other compiled provider.

Use the adapter for cross-process / remote / external callers that need an
explicit grant handoff. Local execution does not need to call the tool; it uses
`credential_id` through `LocalSecretBroker`.

## Provisioning

UI: `/vault` (`ui/unified-ui/src/routes/(app)/vault/+page.svelte`). Not `/presto/vault`.

API: localhost-only routes in `magician-api/src/secret_vault_api.rs`, mounted
under `/api/magician/v2`:

| Method | Path |
|--------|------|
| GET/POST | `/secrets` |
| GET/PUT/DELETE | `/secrets/{id}` |
| GET | `/secrets/approvals` |
| POST | `/secrets/approve` |
| GET | `/secrets/setup-token` |
| POST | `/secrets/setup-token/acknowledge` |
| POST | `/secrets/setup-token/rotate` |
| POST | `/secrets/app-data-root-key/rotate` |

All handlers refuse non-loopback peers. Create/update/delete and
`include_fields=true` on GET detail require `X-Magician-Setup-Token`. List
responses are metadata only. Setup token is generated at first run, shown once,
stored hashed.

## Adjacent adapters

These sit on `SecretStore` / `SecretStoreResolver` without replacing the three
source pipeline:

- **`credential_material_adapter`** — sealed `tool-runtime-core` credential
  preparation. Static-secret governed runtimes use it in production. Delegated
  grant construction is reserved (adapter grant TTL 30s when used).
- **`credential_lifecycle_executor`** — process-tree owner for governed CLI
  authentication (cleared environment, output caps, process-group reap). Google
  Workspace is the first production consumer; other providers stay gated.
- **`mcp_oauth_vault`** — `SecretStoreMcpOAuthVault` implements the MCP SDK
  OAuth persistence trait over the encrypted `mcp_oauth.vault` partition
  (sync IO on the blocking pool).
- **`operator_profile_adapter`** — non-secret profile metadata from
  `tool_runtime_profiles`; no provider/env naming policy in this adapter.
- **`runtime_credential_audit`** — metadata-only receipts appended to
  `secret_audit.jsonl` then emitted as analytics. Dormant until the governed
  execution integration gate opens; never writes secret values.

`AppControlPlaneSigner` (in `secrets/mod.rs`) is a process-owned HMAC derived
from the same OS master key, domain-separated from vault material, used to seal
app control-plane records.

## Security Properties

| Property | Mechanism |
|----------|-----------|
| LLM never sees values | Opaque ids / grant ids / placeholders / implicit captured session; resolve/inject/sanitize are compiled |
| Policy uses real actions | `trust_tool_action()` + HTTP URL domain, not caller-claimed strings alone |
| Domain lock | `allowed_domains` before grant issue; binding re-checked on redeem |
| Grants | Single-use, default 300s TTL, UUID |
| Injection is post-LLM | Executor injects after the decision is final |
| Universal sanitization | One `sanitize_result` / `known_value_replacements` for executor, browser dispatch, evidence (see Sanitization) |
| Encryption at rest | AES-256-GCM, OS keychain master key |
| Captured CAS | Generation counter + lease targets |
| Keychain outage | Durable features disabled; ephemeral remains |
| `list_available` | `id` + `label` only |
| Audit | Append-only `secret_audit.jsonl` (ids, tool, action, never values) |
| At rest, permissions | Every vault file `0600`, the scope's `secrets/` directory `0700`; pre-existing looser files tightened on next write |
| `Debug` | `SecretEntry`, `CookieWithMetadata`, `SessionCookie`, `SessionContext`, the executor's `PreparedSecretAction`, and `KnownSecretValues` all print names and lengths, never values |
| Redaction table lifetime | `KnownSecretValues` zeroizes on drop; `!Clone + !Serialize` enforced by `assert_not_impl_any!` |

The store is in-process; vault files are per V3 scope, not a network service.

## Known Limitations

1. Adapter callers still do `request_credential` then a second action with the
   grant. Local `credential_id` does not need that round-trip.
2. `FormFields` on HTTP is JSON-only. Form-encoded bodies are not injected here.
3. `UsageTracker` is in-memory; daily budgets reset on process restart. Audit
   JSONL is the forensic record.
4. No cross-process vault locking. Isolation is per `(principal, workspace)`
   directory, not a lock manager.
5. Policy domain for provisioned inject is the HTTP action URL. Embedded
   checkouts (merchant page, Stripe iframe) are not frame-aware; put both hosts
   on `allowed_domains`. Non-HTTP actions contribute no domain.
6. `is_secret_param_name` is heuristic. `confirmation_code` is not detected.
7. Captured replay and provisioned `inject()` targets are HTTP-only.
8. `secret_audit.jsonl` has no rotation.
9. Grants are consumed on redeem even if the later action fails. Request a new
   grant; `credential_id` can issue another.
10. Captured sanitization is best-effort: the executor redacts values from the
    session context it actually loaded for that replay URL.
11. Caller-declared adapter target is advisory; executor re-checks binding.
12. `sanitize_result` coverage is bounded (see Sanitization).
13. `runtime_credential_audit` and parts of the MCP OAuth coordinator remain
    dormant pending their integration gates; the vault adapter and partition
    already exist.

## Integration Points

| What | Where |
|------|--------|
| Types, bootstrap, signer | `magician_v2/secrets/mod.rs` |
| Scoped stores | `secrets/store.rs` (`SecretStoreResolver`) |
| Brokers | `secrets/broker.rs` |
| Executor inject/sanitize | `execution/agentic/executor.rs` (`prepare_action_with_secrets`) |
| Ephemeral heuristic | `secrets/classify.rs` (`is_secret_param_name`, re-exported by `execution/agentic/types.rs`) |
| Treasurer provider | `execution/treasurer_provider.rs` |
| Pack def | `execution/embedded_pack_defs/treasurer.yaml` |
| Registry | `execution/compiled_providers.rs` (`build_compiled_registry`) |
| Startup | `orchestrator/v2_orchestrator.rs` (resolver + `ScopedSecretBroker`) |
| Vault API | `magician-api/src/secret_vault_api.rs` |
| Capture / replay | `api_mining` + `magician-api/src/api_mining_api.rs` |
| UI | `ui/unified-ui/src/routes/(app)/vault/+page.svelte` |
