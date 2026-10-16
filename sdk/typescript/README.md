# `@magician/apps` (prepublication)

Current development package version: `0.1.0-dev.3`. See
[`CHANGELOG.md`](CHANGELOG.md). The supported-public wire contract is
independently versioned at `1.5.0`.

`AppLiveCollection` composes `queryData` and `readEntityChanges` for reusable
paginated live lists. It defaults to 25 rows, retains a separate older-page
cursor and change sequence, and merges canonical edits/inserts/deletes by record
identity. `loadMore()` fetches one additional page; `synchronize()` processes
one bounded change page. It does not run actions or models. See the
[collection guide](../../docs/components/magician/typescript-apps-sdk.md#paginated-live-collections)
for integration and recovery behavior.

Local `file:` consumers run the package's `prepare` lifecycle so the declared
`dist/` exports exist after a clean install. Public packing remains separately
refused by the qualification gate; `prepare` does not make this private SDK a
publishable release.

Recipe authoring uses the closed `recipeNode` helpers plus
`defineRecipeBundle`. The builder surface matches the implementation-ready
Rust/V3 subset: Query, Get, pure bounded Map, Validate, EmitValue, Sequence,
bounded Parallel and Switch. Get carries only an opaque entity projection and
Map carries an immutable operation body plus its exact digest. It intentionally
has no Retry, MarkUncertain or effectful builder until those nodes have
canonical runtime owners. Parallel maps are
sorted by semantic key, require at least two branches and set an exact
per-node parallelism ceiling; nested Parallel remains rejected by package
admission.

This private TypeScript package is the typed client for Magician's
supported-public Apps contract. It exposes exactly eight operations:

- negotiate contract capabilities;
- query one installation's admitted records;
- commit an optimistic, idempotent mutation;
- launch one admitted action;
- read or wait for its opaque logical run;
- request generation-bound cancellation with an exact replay key;
- compose one canonical action result into another reviewed action without
  disclosing the source bytes to the client; and
- read or iterate bounded identifier-only entity changes.

Authoring also exports generated `defineInteractiveCapabilityRequest` and
`defineInteractiveGrantSelection` builders. They canonicalize logical
origin/target selectors and admit exactly one Browser, macOS or Android action
class (`observe`, `navigate_or_launch`, `interact`, `outward_commit`, plus
Android-only `capture_pixels` with reviewed pixel capture) with direct-owner,
denied-transfer posture and an invocation- or run-bound session; every other
request uses structured evidence only. Multi-class, background, transfer and
inconsistent owner/target selections fail builder admission. Session, observation, receipt, stop and status references are
opaque branded strings; no raw CDP, host, device, window/tab, MCP or control
token type is exposed.

Every non-capability request first negotiates supported-public contract
`1.5.0`, data protocol `1`, JSON Schema Draft 2020-12, the exact operation
inventory and its BLAKE3 digest, exact deprecation rows, compatibility posture,
and server limits. Advertised limits can narrow the client but cannot raise its
local 1 MiB document, depth-32, node-20,000, value-256-KiB/value-node-8,000,
predicate-depth-16/predicate-node-128, collection-256, or page ceilings.
Requests use fixed routes and methods, same-origin credentials, no redirects,
closed request validation, bounded JSON input, one end-to-end handshake plus
operation deadline/abort fence, and streamed response ceilings
before parsing. There is intentionally no generic request, arbitrary path,
header, SQL, MCP, provider, filesystem, task-id, device-id, or raw argument API.
HTTP failures must carry the strict canonical `AppErrorEnvelope`. SDK errors
expose its stable code, disposition, bounded details, optional retry delay,
status, operation, and optional operation-inventoried reason while retaining a
stable SDK message. Unknown fields/enums, invalid uncertainty pairing,
cross-operation details, unlisted reasons, and illegal retry delays fail
decoding. Recovery follows only the typed disposition; HTTP status and display
text never imply retry safety. Inventoried diagnostics use
`SupportedPublicErrorReason`; `SupportedPublicHttpErrorCode` is retained only
as a deprecated generated alias for source compatibility.
The wire's optional `execution_id` is exposed only as a bounded diagnostic for
the current attempt. It is never required for correlation, retry, polling, or
authority; clients use the opaque logical `run_handle.run_ref` exclusively.
Likewise, the required `origin.execution_id` inside a workflow mutation receipt
is server-returned provenance for that committed mutation. It is never a
caller input, bearer, correlation key, or authority handle.

JSON integers are admitted only inside JavaScript's exact safe-integer range;
larger wire integers fail decoding instead of being silently rounded. Query
records are correlated to the requested entity and may expose only selected
fields plus declared relation-expansion keys. Client-keyed mutation/action/composition POST
failures after dispatch may have committed: transport, timeout, cancellation,
response decoding, and success-envelope validation then return
`outcome_uncertain` with `retry_identical_input`. Callers must reuse the exact
input and idempotency key. A mutation receipt's `batch_digest` remains
server-owned canonical Rust evidence: this SDK validates its shape, exact owner
request provenance, and change-range cardinality but deliberately does not
invent a JavaScript approximation of canonical number/default encoding.

Action results default honestly to bounded `JsonValue`. A caller can supply a
runtime `outputValidator` for one app's known schema, but generated per-app
action/input/output codecs are explicitly deferred until their cross-language
schema generation is reviewed; a local predicate never adds server authority.

`composeActionRun` remains one fixed operation while supporting a linear chain
of at most three destinations. Each hop supplies a server-compiled mapping and
unique idempotency key; waiting, withheld, terminal, or uncertain hops stop
progress until the same request is safely recoverable. Optional subscription
cursors return at most eight monotonic payload-free active-hop updates, are
bound to the exact scope/run/full-chain request, expire after ten minutes, and
signal reset rather than fabricating missed history. No task/execution IDs or
generic event channel are exposed.

The checked `src/generated/public-contract.ts` projection is owned by the Rust
contract generator; `client.ts`, `errors.ts`, and `types.ts` are the small
reviewed ergonomic/transport layer. This package is `private: true` and its
`prepack` hook fails deliberately. The supported-public artifacts and generated
mirrors are current for `1.5.0`; publication remains blocked until the complete
generator/API parity gate and a real authenticated consumer canary reach the
actual registry, entity, and action owners.

`make test-app-typescript-sdk` performs a lockfile-clean install, builds the
package, and runs the focused SDK suite. `make
test-app-typescript-consumer-canary` additionally copies source into a fresh
temporary tree, builds and packs the private SDK for qualification with npm
publication scripts disabled, installs that tarball into the external Research
Planner project, compiles it, and runs all eight fixed routes against a local HTTP
contract double. That clean packaged-consumer lane has passed, but it is not the
real authenticated `AppPlatformApi` canary and does not complete P4.9. `make
test-app-typescript-real-server-canary` is the complementary non-mock gate: it
starts `configure_app_routes` with a real loopback-authenticated
`AppPlatformApi`, registry, and entity owners, then runs the freshly packed SDK
through capabilities, query, mutation, a verifying re-query, and entity
changes. Successful action launch and opaque polling remain blocked on a
provider-free `ArtifactV2Service`/`AppWorkflowService` fixture constructor; the
production action route requires that exact task owner and this qualification
lane does not install a fake or test-only production backdoor.


`recipeNode.reconcile` declares bounded approved host reads followed by an
atomic app-owned reconciliation. Its field mappings, update allow-list,
create-only seeds and completeness checks are part of package review. It is a
single terminal node with receipt-bound effects, not a general tool executor.
Use the fixed reconciliation result schema from the Town Square recipe example;
Rust admission verifies the exact schema, source identity and mutation targets.

The input schema matches the workflow's reviewed record schema.
`input_parameters` forwards only named scalar inputs; `allow_null` preserves a
present nullable source field. Additional `sources` (at most three) seal exact
primitive/action references and can use `when_input_present` for optional reads.
Targets select them with `source`; `source_document.max_bytes` preserves complete
JSON text under a reviewed byte bound. The builder includes all sources in its
tool-call ceiling. Package admission independently checks schema identity,
locked actions and exact mutation targets.

`recipeNode.contextualRound` declares a reusable context-first round: one exact
reviewed host source, fixed App-store queries, deterministic eligibility and
mutation mappings, one reviewed semantic step per eligible participant, and
independent per-participant and aggregate budgets. `ContextualRoundProgram`
provides typed flat expressions rather than executable scripts. The server
validates dependencies, schemas, authority and effect ownership; constructing a
bundle grants no access or model authority. See Town Square's
`recipes/take-ambient-turn.json` for the complete consumer and
`docs/components/magician/app-behavior-recipes.md` for runtime accounting.


`recipeNode.storeTransaction` authors deterministic App-owned record operations.
Its typed declaration provides fixed host sources, ordinary store queries,
explicit guards and create/update/delete rules. The shared value program supports
canonical JSON payloads and revision-fenced updates. There is no model input or
provider selection; the server validates authority and commits through the same
workflow owner as other App actions. `scan_queries` distinguishes complete cursor
scans from intentionally bounded windows. See the shipped Meetings `listen` and
Learning `approve-candidate` recipes for ledger examples.
## Shared mechanical workflow declarations

`recipeNode.storeTransaction` supports fixed host reads, input-derived scalar
parameter overrides, bounded key lookups, deterministic field mapping and
atomic App-owned writes. `StoreTransactionProgram` exposes optional source
parameters and result summaries. `LinkedTextRowsSchema` describes closed JSON
dictionary links for input validation. The server validates these declarations
against the package's exact source locks, grants and entity schema; the SDK
does not grant authority. The shipped Meetings and Claims sync recipes exercise
these helpers without model orchestration.

## Manifest feature `app_memory_read_v1` (2026-09-25)

The generated public contract lists `app_memory_read_v1`. A package that
declares it may add an `app.memory.read` request (`user_tiers`, `agents`,
`purpose`); it is review material only — the owner grants a subset, split
between interactive and background runs, and reads go through the
`memory_data` tool. See `docs/components/magician/app-memory-access.md`.
