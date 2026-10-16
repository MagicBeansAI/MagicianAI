# API Mining — Capability Evolution + Projection Layer

Related: [`api-mining-pipeline.md`](./api-mining-pipeline.md),
`projection-layer plan`,
`capability-evolution plan`.

API Mining observes HTTP traffic from browser-agent sessions, clusters it into
URL templates ("capabilities"), and gates replay behind a confidence threshold.
**Capability Evolution** turns validated capabilities into agent-callable
skills. **The Projection Layer** materializes JSON response bodies into typed
SQLite rows so recurring queries can be answered offline.

## Lifecycle and on-disk layout

A *projection* is the typed-row materialization of a recurring JSON response.
Records live at `<scope>/api_mining/projections/<projection_id>.json`; rows live
in `<scope>/api_mining/projections/_rows.db` (one table per projection).

| State | Meaning | Advance |
|---|---|---|
| `pending` | First sample; schema inferred; no rows. Awaits review. | `POST …/projections/{id}/approve` |
| `approved` | Subsequent matching responses ingest rows. | Auto → `live` on first ingest |
| `live` | At least one batch materialized; query serves SQLite. | — |
| `invalidated` | Declared variant. No production setter currently assigns it. | — |

Keyed by `(origin, resource_label)`. `origin` is scheme + host.
`resource_label` comes from [`derive_resource_label`](../../../magician/src/magician_v2/api_mining/projection.rs)
(strip scheme/host/query/fragment, `{placeholder}` → `*`, lowercase). Unique
within a scope. Examples: `/api/card/12116/query` → `card-12116-query`;
`/api/v1/users/{id}/sessions` → `users-*-sessions`; `/graphql` → `graphql`.

## Schema, TTL, query

`ProjectionStore::migrate_table` is additive-only (`ALTER TABLE ADD COLUMN`,
`SchemaConverged`). Missing columns in a later sample are a no-op. Column drops
and type changes are **rejected** (`purge_rows required`).

`ttl_seconds` defaults to 3600. `ProjectionPipelineState::query_known_resource`
compares `last_ingested_at`:

- within TTL → `(rows, ServedFrom::Projection)` (`RowsServedFromStore`)
- past TTL → still serves, tagged `ServedFrom::StaleProjection` (`StaleServed`)

The query path never blocks on a refresh. `stale_projection` is a success shape.

There is **no** `query_known_resource` agent tool (the name remains in
`is_parallelizable_read_only_pack` but no tool is built). The live surface is `POST /api/magician/v2/api-mining/projections/query`.

| Error | Meaning |
|---|---|
| `no_projection` | Nothing stored for `(origin, resource)`. Fall back to replay/browser. |
| `pending_approval` | Record exists; not approved. |
| `store_error` | SQLite / I/O. Treat as no projection. |

Backfill is forward-going only. Seed historical data with
`POST /api/magician/v2/api-mining/replay/{origin_key}/{capability_id}`, then
approve the Pending projection.

`find_array_of_objects` is BFS-largest over arrays of homogeneous objects (edit
`array_path` before approval if it picks wrong). Nested objects store as TEXT
JSON. Inference picks `id` as primary key when present; rename via purge + edit
+ re-approve.

## Privacy and operator UI

Per-scope, not cross-scope. `POST …/projections/{id}/purge-rows` drops rows and
keeps the schema. In contrast, origin purge drops every matching projection's
SQLite table and JSON record so neither values nor field-name schema remains;
other origins are untouched. Pending means no rows without a human flip.

The API Mining recipe and capability detail drawers
(`ui/unified-ui/src/routes/(app)/api-mining/+page.svelte`) list linked resource
label, lifecycle, and row count. Approve and Purge rows call the endpoints
below. Projections are not a peer tab.

## HTTP surface

Under `/api/magician/v2`. Workspace-bound bearer required; principal/workspace
are not caller-selected headers or query parameters.

| Method + path | Purpose |
|---|---|
| `GET /api-mining/router-metrics` | Per-scope router counters. |
| `GET /api-mining/projection-metrics` | Per-scope projection counters. |
| `GET /capability-evolution/summary` | Catalog pack counts by lifecycle status. |
| `GET /api-mining/projections` | List projections with row counts. |
| `POST /api-mining/projections/query` | Query (`origin`, `resource`, optional `where_clause` / `params`). |
| `POST /api-mining/projections/{id}/approve` | Pending → Approved. |
| `POST /api-mining/projections/{id}/purge-rows` | Drop rows, keep schema. |
| `POST /api-mining/capabilities/{origin_key}/{capability_id}/bless` | Promote to Validated; seed Capability Evolution. |

## Configuration

Live YAML key is `api_mining:` (`pub api_mining: ApiMiningConfig`, no rename).
`api_mining_config:` is ignored.

```yaml
api_mining:
  enable_trace_capture: true
  enable_mining: true
  enable_replay: true
  enable_xhr_validation: true
  takeover_min_samples_per_method: { GET: 1, HEAD: 1, POST: 1, PUT: 1, PATCH: 1, DELETE: 1, OPTIONS: 1 }
  takeover_default_min_samples: 1

capability_evolution:
  enabled: true
  enable_api_mined_bridge: true
  trial_to_validated_min_attempts: 5
  trial_to_validated_min_success_rate: 0.8
  validated_to_trusted_min_attempts: 20
  validated_to_trusted_min_success_rate: 0.95
```

`min_replay_confidence_for` is the replay-tier policy; Write methods replay at
Candidate so visit-2 holds for POST/PUT/PATCH/DELETE as well as GET/HEAD.
Safety floors (URL denylist, body fingerprint, auth posture) still gate
dangerous paths. Mined data is per-(principal, workspace); there is no public
CLI or MCP server per origin.
