# TypeScript Apps SDK

`sdk/typescript/` is the prepublication `@magician/apps` client for the
supported-public Apps contract. It is a small client, not an alternate
Magician runtime or a generic HTTP wrapper.

## Public operation ceiling

The generated inventory admits exactly these calls:

| SDK method | HTTP operation |
|---|---|
| `connect` | `GET /api/magician/v2/apps/contract-capabilities` |
| `queryData` | `POST /installations/{installation_id}/data/query` |
| `mutateData` | `POST /installations/{installation_id}/data/mutations` |
| `launchAction` | `POST /installations/{installation_id}/actions/{action_id}/runs` |
| `getActionRun` / `waitForRun` | `GET /action-runs/{run_ref}` |
| `cancelActionRun` | `POST /action-runs/{run_ref}/cancel` |
| `composeActionRun` | `POST /action-runs/{run_ref}/compositions` |
| `readEntityChanges` / `iterateEntityChanges` | `GET /installations/{installation_id}/entity-changes` |

There is no generic `request`, arbitrary path/header, legacy action launch,
generic task/tree cancellation, installation review, custom-surface bridge, raw MCP, SQL,
filesystem, credential, provider, task-id, device-id, or argv API. An injected
`fetch` is only the authenticated transport seam. The SDK fixes the HTTP
method, route, media types, same-origin credential posture, no-redirect policy,
no-referrer policy, one end-to-end handshake-plus-operation deadline, and
response parser for each operation.

## Paginated live collections

`AppLiveCollection` is a client primitive over the existing query/change owners;
it adds no endpoint, manifest permission, workflow or LLM step. Apps supply an
installation/surface revision, entity query, indexed record-identity field and
comparator matching the server order (including its ascending `record_id` tie
break). Native pages use the same primitive through
`ui/unified-ui/src/lib/apps/appLiveCollection.ts`.

```ts
const list = new AppLiveCollection(client, {
  request: messageQuery, // entity, selected fields, predicate, order and purpose
  surfaceRevision,
  recordIdField: "message_id",
  pageSize: 25,
  compare: compareMessages,
});
const unsubscribe = list.subscribe(render);
await list.start();
await list.loadMore(); // one explicit older page
await list.synchronize(); // call on a change wake-up or reconnect
// Schedule another bounded turn while list.state.moreChanges is true.
// On navigation/scope change: unsubscribe(); list.dispose();
```

Only the requested page loads on open; the helper imposes no total row cap.
Loaded pages stay visible while incoming rows merge at their ordered positions.
Memory grows with opened pages and live rows, not stored history; hosts should
use rendering containment or virtualization for long sessions (Town Square uses
keyed rows and browser content visibility).

The initial durable head is captured before querying, closing the hydration
race. Identifier-only change pages re-query affected identities in batches no
larger than the page size; unchanged catch-up re-queries nothing, and changes
outside the opened window do not pull history into memory. The helper uses
platform keyset pagination: load more seeks from the last sort value/record ID
and reads one page. Queries require indexed equality/in filters (and all/any
combinations) and at most one indexed sort field; collections above 10,000 rows
remain pageable. Concurrent calls coalesce, mutations apply serially, and failed
reads never acknowledge a sequence.

The web driver uses scoped `app.entity.changed` wake-ups on the shared
websocket, then reads the durable sequence, and also catches up on reconnect,
visibility and a 15-second fallback poll. Hidden pages pause, requests never
overlap, and failures back off. The package bridge can implement the same
`AppCollectionTransport` with granted query/change methods; the helper does not
widen its session or capability budgets.

Keyset cursors expire after 24 idle hours; continuations renew that window.
Cursor cache pressure may reclaim an abandoned or older traversal earlier,
within its installation, while protecting the chain being continued. An expired
or reclaimed cursor, or pruned change history, causes an explicit reset to the
newest page with `resetReason`, which the host must show; the helper never walks
the whole history to rebuild a cursor. Installation, grant, schema and scope
binding changes must reopen the collection under fresh authority; rows never
move silently between bindings.

## Negotiation and bounds

`connect()` is explicit and every other method safely performs the handshake
first. Only a successfully validated capability document is cached;
concurrent pre-success callers retain independent cancellation and deadline
fences. Negotiation requires:

- supported-public contract `1.5.0` and capability schema `1`;
- data-plane protocol `1` and exact equality with the fifteen-feature
  manifest inventory (`APP_SUPPORTED_MANIFEST_FEATURES`), including widgets,
  schedules, event behaviors, one-way owner notifications and app memory reads;
- JSON Schema Draft 2020-12;
- the current-contract-only SDK compatibility window;
- the exact generated deprecation inventory;
- exact structural equality with the generated eight-operation inventory; and
- an advertised BLAKE3 inventory digest that recomputes over those exact wire
operations.

Advertised limits may narrow the SDK but cannot raise its local 1 MiB document,
depth-32, node-20,000, value-256-KiB/value-node-8,000,
predicate-depth-16/predicate-node-128, collection-256, query-page-200, or
change-page-128 ceilings. Entity-change iteration also stops after at most 200
requested pages under one caller deadline. Query and mutation collection,
predicate-arena, relation, and aggregate value limits are enforced before the
operation fetch; they are not merely advertised.

Requests get node/depth validation and an exact JSON UTF-8 byte preflight
before `JSON.stringify` or `TextEncoder` allocates the body. Responses are
streamed under the lower of the advertised document ceiling and the SDK's 1 MiB
hard ceiling before decoding or parsing. Every success model is then
closed-validated: envelopes, handling labels, source references, records,
mutation receipts/origins, run handles, lifecycle enums, typed action errors,
result semantics and entity-change sequences. Application-owned payloads remain
bounded JSON. Every integer must be a JavaScript safe integer on request and
response, so the client never silently rounds a revision, sequence or
identifier.

Query responses are correlated to the exact request: every record uses the
requested entity, and its field map holds only `select` entries plus declared
relation-expansion keys. Mutation receipts must carry the exact owner-API
idempotency reference, with change-range cardinality equal to the
committed-revision rows. `batch_digest` is server-owned canonical Rust evidence;
until a shared cross-language canonical JSON codec exists, the SDK validates its
shape only, and identical-key server idempotency plus exact origin/range
validation is the boundary.

The client defaults action output to `JsonValue`. A narrower type requires
`outputValidator`, which runs on every returned output. Separately,
`magician app check --write-generated` emits per-app exact workflow/action input
and result types plus `defineAppValueCodec` codecs from the bounded Rust
workflow schema graph. These are content-addressed, byte-current package
evidence and runtime validation, not server authority. The same module exposes
schema-derived form metadata, a correlated custom-surface bridge, opaque logical
handles and the closed nine-builder `AppRecipe` set (`query`, `get`, `map`,
`validate`, `emitValue`, `sequence`, `parallel`, `switch`, `reconcile`; the
SDK's `recipeNode` also has `storeTransaction` and `contextualRound`, which the
generated module does not expose).

`composeActionRun` accepts one initial destination plus at most two `chain`
continuations (three destination hops total, fanout `1`). Every hop has its own
mapping and unique idempotency key. The SDK rejects excess depth, duplicate
keys, unbounded mappings, and a response whose active hop, source run,
destination installation/action, or hop count differs from the exact request.
The server stops on waiting, withheld, terminal, failed, or uncertain outcomes;
a later byte-identical retry resumes from durable receipts without duplicating
already-launched effects.

An optional `subscription` object adds cursor polling to that same operation.
Its page limit is `1..=8`; response validation requires monotonic sequences,
exact active run/action/installation correlation, bounded payload-free updates,
an opaque next cursor, and an expiry. `reset_required` means the caller must
accept the supplied current canonical state because the cursor expired or an
intermediate transition is no longer retained. Subscription objects never
contain internal task or execution IDs.

For `mutateData`, `launchAction` and `composeActionRun`, any transport,
deadline, cancellation, decode or validation failure after dispatch is reported
as `outcome_uncertain` with `retry_identical_input`: retry only the
byte-identical request with the same idempotency key. Local preflight rejection
is conclusive. A non-success response must be a strict `AppErrorEnvelope`
(unknown fields/enums, invalid uncertainty pairing, cross-operation diagnostics,
unlisted route reasons and invalid retry delays are rejected). Its `disposition`
is the only server-owned recovery instruction; `retry_same_input` becomes
`retry_identical_input`, and `outcome_uncertain` stays uncertainty, not an
ordinary HTTP failure.
`MagicianAppsError` exposes the canonical `errorCode`, `errorDisposition`,
bounded `errorDetails`, optional `retryAfterMs`, and optional inventoried
`reasonCode` typed as `SupportedPublicErrorReason`. The generated
`SupportedPublicHttpErrorCode` name remains only as a deprecated source alias.
HTTP status and display text never imply retry safety.

Optional launch/run `execution_id` is bounded diagnostic information for the
current attempt. Correlation and polling use only the stable opaque
`run:app-action:` handle. The required `origin.execution_id` in a workflow
mutation receipt is similarly server-returned provenance, never an accepted
authority or caller correlation value.

## Generated versus reviewed source

The Rust contract descriptors are canonical. `make app-contract-codegen`
regenerates `src/generated/public-contract.ts` and
`src/generated/interactive-contract.ts` from the same operation
inventory used by route registration and OpenAPI generation; it must stay
byte-coherent with the OpenAPI bundle and the Unified UI and Swift fixture
mirrors. The other `src/` files (`client.ts`, `validators.ts`,
`bounded-json.ts`, `errors.ts`, `types.ts`, `public-contract.ts`,
`live-collection.ts`, `recipe.ts`, `value-schema.ts`, `contextual-round.ts`,
`index.ts`) are the handwritten layer.

## Release gate

The package is `private: true`, uses exact dependency/tool versions, publishes
only `dist` plus its README, and has a deliberately failing `prepack` hook.
Publication stays blocked until all of the following are complete:

1. a clean repeat of atomic supported-public code generation;
2. generator/OpenAPI/fixture/SDK parity review;
3. the focused SDK tests and Rust JSON-envelope tests;
4. an independent static security review; and
5. an authenticated consumer canary against a local server.

`make test-app-typescript-real-server-canary` is the provider-free real-server
canary: it mounts the production `configure_app_routes`, the real
`AppPlatformApi`, loopback auth and seeded registry/entity-store owners, and
drives a packed consumer through capabilities, query, mutation and entity
changes. Action launch/polling is not covered: those handlers need a real
`AgentResources.artifact_v2_service`, which requires the full production
orchestrator.
