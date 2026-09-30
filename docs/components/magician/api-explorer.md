# API Explorer

## Purpose

API Explorer is the task-centric operator surface for browserless recipes and
mined API capabilities: inspect recipes, check captured-auth status, replay,
browse generated OpenAPI, review hidden noise, watch routing telemetry, and
manage recipe-linked cached rows. The surface is `/api-mining` (command
palette **API Mining**, chord `G I`). There is no `/presto/forge` route.

Native-route UX lives in
[API Mining Native Route](../unified-ui/api-mining-native-route.md). This file
is the operator-surface contract: route, tabs, endpoints, builder.

## Route And Tabs

`ui/unified-ui/src/routes/(app)/api-mining/+page.svelte` owns fetching and
native rendering. Tabs (0-based):

| Index | Tab | Operator contract |
| --- | --- | --- |
| 0 | Recipes | Task shapes, inputs, versions, run rail, grants, direct replay, and recipe-linked cached rows |
| 1 | Learned APIs | Site-grouped relevance-filtered capabilities, used-by recipe chips, Replay / Edit & Replay, and OpenAPI |
| 2 | Activity | Process-local router, passive-validation, recipe, registry-health, and projection counters (poll 5s while visible) |
| 3 | Auth | Non-secret origin auth status and Capture/Refresh Auth |

`origin_key` is the filesystem-safe origin (for example `https___api_github_com`).

## Data Sources

The explorer sits on `CapabilityRegistry`, the captured-auth `SecretStore`
partition, the replay pipeline (`ApiRunner`), server-side OpenAPI generation,
`OriginPolicyStore` (allow/block and replay-mode), the projection pipeline, and
process-local metrics (reset on backend restart).

## UX Model

Learned APIs groups capabilities by parent site and exposes:

- capability rows with method, URL template, confidence, relevance, side effects, sample count, and used-by recipe chips
- Replay (empty JSON body) and Edit & Replay (loads the capability, then POSTs `headers_overrides` / `body_override`)
- origin-level OpenAPI viewing (`SwaggerModal` fetches OpenAPI; Try-it-out is disabled)
- a telemetry-hidden counter that opens bounded noisy-origin Allow/Block review

The Recipes tab owns versioned DAG detail, run history, grant revocation,
direct replay, and cached-row approve/purge. Auth status and Refresh Auth live
in the dedicated Auth tab. Learned-resource projections are shown under the
recipe or capability that produced them, not as a peer tab.
Overview/list payloads expose input names, scalar schemas, and sources but omit
captured example values. The explicit recipe detail endpoint remains the
operator drill-down surface.

The explorer is operational: it validates and uses learned APIs, not just
browsing raw mining output. Replay responses carry `status`, `headers`, `body`,
`elapsed_ms`, `auth_was_stale`, `confidence_after`, and `error` so the UI can
render the result inline.

## Backend Contract

Handlers live in `magician-api/src/api_mining_api.rs`. `magician-bin/src/main.rs`
mounts them under `/api/magician/v2`. Paths below are the mounted form.

Optional replay body: `{ parameter_overrides, headers_overrides, body_override }`.
`POST …/refresh-auth` returns `202` for a new session or `200` if one is already
active (`400` if the origin is not `http`/`https`). Origin policy POSTs may send
`{ "origin_url": "https://…" }` for trace-only origins. `allow-replay` is a
boolean shim (`true` → `replay_reads`, `false` → `validate_only`);
`replay-mode` is the live takeover policy:
`observe_only` / `validate_only` / `replay_reads` / `replay_writes_with_hitl` /
`replay_trusted_writes`. The native page does not POST replay-mode; Routing
Activity shows `replay_mode` on registry-health origin readiness.

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/api/magician/v2/api-mining/overview` | One task-centric page payload: recipes, site groups, relevance, telemetry-hidden count, auth, grants, and counters |
| `GET` | `/api/magician/v2/api-mining/recipes` | Value-free recipe list metadata; no captured input examples |
| `GET` | `/api/magician/v2/api-mining/recipes/{recipe_id}` | Complete versioned recipe without credential values |
| `GET` | `/api/magician/v2/api-mining/recipes/{recipe_id}/runs` | Newest-first durable run ledger |
| `POST` | `/api/magician/v2/api-mining/recipes/{recipe_id}/replay` | Direct read/granted replay; client approval markers are ignored, 409 precedes any ungranted write, and unverified sent writes return terminal 409. Generated skills bind `published_only=true&expected_version=N` |
| `GET` | `/api/magician/v2/api-mining/replay-grants` | Grant audit list |
| `DELETE` | `/api/magician/v2/api-mining/replay-grants/{grant_id}` | Revoke one active durable grant |
| `GET` | `/api/magician/v2/api-mining/registry` | Full `RegistryIndex` (`version`, `origins`, `last_rebuilt`) |
| `GET` | `/api/magician/v2/api-mining/capabilities/{origin_key}/{capability_id}` | Full `ApiCapability` |
| `POST` | `/api/magician/v2/api-mining/capabilities/{origin_key}/{capability_id}/bless` | Force-promote to Validated (idempotent; not wired on this page) |
| `POST` | `/api/magician/v2/api-mining/replay/{origin_key}/{capability_id}` | Replay with optional overrides |
| `GET` | `/api/magician/v2/api-mining/auth-statuses` | Batch auth status keyed by `origin_key` |
| `GET` | `/api/magician/v2/api-mining/auth-status/{origin_key}` | `{ has_auth, is_stale, has_cookies, has_headers, has_storage }` |
| `POST` | `/api/magician/v2/api-mining/origins/{origin_key}/refresh-auth` | Start a scoped Magicutor CDP capture (no LLM) |
| `GET` | `/api/magician/v2/api-mining/origins/{origin_key}/refresh-auth/{refresh_id}` | Poll non-secret refresh phase |
| `GET` | `/api/magician/v2/api-mining/openapi/{origin_key}` | OpenAPI 3.0 JSON for the origin |
| `GET` | `/api/magician/v2/api-mining/noisy-origins` | `{ threshold: 25, origins: [{ origin_key, origin_url, trace_count, capability_count, decision }] }` |
| `POST` | `/api/magician/v2/api-mining/origins/{origin_key}/allow` | Mark origin allowed |
| `POST` | `/api/magician/v2/api-mining/origins/{origin_key}/block` | Block future capture/mining |
| `POST` | `/api/magician/v2/api-mining/origins/{origin_key}/purge` | Delete mined artifacts, generated recipe tools/skills, grants, and that origin's projection metadata/rows |
| `POST` | `/api/magician/v2/api-mining/origins/{origin_key}/block-and-purge` | Block and purge |
| `POST` | `/api/magician/v2/api-mining/origins/{origin_key}/allow-replay` | Boolean replay shim |
| `POST` | `/api/magician/v2/api-mining/origins/{origin_key}/replay-mode` | Set `OriginReplayMode` |
| `GET` | `/api/magician/v2/api-mining/router-metrics` | Router outcome counters |
| `GET` | `/api/magician/v2/api-mining/passive-validation-metrics` | Background XHR/Fetch validation counters |
| `GET` | `/api/magician/v2/api-mining/registry-health` | Index integrity and takeover-readiness |
| `GET` | `/api/magician/v2/api-mining/projection-metrics` | Projection pipeline counters |
| `GET` | `/api/magician/v2/api-mining/recipe-metrics` | Task-start lookup, replay, fallback, approval, grant, and auth-heal counters |
| `GET` | `/api/magician/v2/api-mining/settings` | Effective process/scope master-switch state |
| `PUT` | `/api/magician/v2/api-mining/settings` | Live scope override or owner-only process ceiling update |
| `POST` | `/api/magician/v2/api-mining/settings/disable-and-purge` | Typed-confirmation off-then-delete learned data |
| `GET` | `/api/magician/v2/api-mining/projections` | List projections (lifecycle, `row_count`) |
| `POST` | `/api/magician/v2/api-mining/projections/{id}/approve` | Pending → approved |
| `POST` | `/api/magician/v2/api-mining/projections/{id}/purge-rows` | Drop rows; keep schema |
| `POST` | `/api/magician/v2/api-mining/projections/query` | `{ origin, resource, where_clause?, params }` (HTTP only; not this page) |
| `GET` | `/api/magician/v2/api-mining/sequences/{origin_key}` | Sequence metadata list (not this page) |
| `GET` | `/api/magician/v2/api-mining/sequences/{origin_key}/{sequence_id}` | Full sequence (not this page) |
| `GET` | `/api/magician/v2/api-mining/sequence-metrics` | Sequence capture counters (not this page) |
| `GET` | `/api/magician/v2/api-mining/workflows/{origin_key}` | Workflow metadata list (not this page) |
| `GET` | `/api/magician/v2/api-mining/workflows/{origin_key}/{workflow_id}` | Full `WorkflowGraph` (not this page) |
| `POST` | `/api/magician/v2/api-mining/workflows/{origin_key}/{workflow_id}/replay` | Workflow replay (not this page) |
| `GET` | `/api/magician/v2/api-mining/workflow-metrics` | Compile counters (not this page) |
| `GET` | `/api/magician/v2/api-mining/replay-metrics` | Workflow-replay counters (not this page) |

## Frontend Composition

The live page is native Svelte: origin panels, HTML tables, Edit & Replay
modal, and `SwaggerModal`. The Presto builder is
`ui/unified-ui/src/lib/magician/presto/surfaces/LearnedApisSurface.ts`
(`buildLearnedApisSurface`, `parseLearnedApisAction`). It encodes the same
Learned APIs contract as MUIJ: collapsible origin panels, `EntityGrid` tables
(`LEARNED_APIS_GRID_PREFIX`), OpenAPI (`LEARNED_APIS_OPENAPI_PREFIX`), and
refresh-auth (`LEARNED_APIS_REFRESH_AUTH_PREFIX`). The live `/api-mining` route
does not render through that builder.

The page component owns async fetching. Surface builders stay declarative.

## Related Docs

- [API Mining Native Route](../unified-ui/api-mining-native-route.md)
- [API Mining Pipeline](api-mining-pipeline.md)
- [API Mining Projections](api-mining-projections.md)
- [Captured Auth For API Replay](auth-capture-and-replay.md)
