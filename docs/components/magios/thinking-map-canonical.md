# Live Thinking Map — canonical iOS client (`LTM`)

`magios/Magios/ThinkingMapCanonical/`. Native-iOS client for the
**canonical** Live Thinking Map backend (see
[backend component doc](../magician/live-thinking-map.md)). `ThinkingMapModel`
is unconditionally backed by `LTM.SyncStore`; the older E0 prototype's iPhone UX
is kept, but not its local graph or AI bridge. A one-time importer and the old
`UserDefaults` **read** path remain so pre-canonical maps migrate on first launch
instead of being destroyed.

## Namespace

Everything lives under a caseless enum **`LTM`** (`LTM.Map`, `LTM.Node`,
`LTM.Edge`, `LTM.Operation`, `LTM.OperationEnvelope`, `LTM.Actor`,
`LTM.Source`, `LTM.Summary`, `LTM.Event`, `LTM.NodeKind`, …). The E0
prototype (`ThinkingMapPrototypeView.swift`) still defines unnamespaced
`ThinkingNode`/`ThinkingEdge`/`ThinkingMapSource` in the same module.

## Wire contract

Swift `Codable` models byte-match the Rust backend's serde JSON.

- **Fixtures** (`Fixtures/*.json`) from
  `magician/examples/thinking_map_wire_fixtures.rs`
  (`cargo run -p magician --example thinking_map_wire_fixtures`): populated
  `map`, `envelope`, `summary`, `event`, `manifest`, and `applied` /
  `idempotent_replay` / `no_operations` response shapes.
- **Models** use explicit snake_case `CodingKeys`. Internally-tagged enums
  have hand-written `Codable`: `LTM.Operation` on `"op"`, `LTM.Actor` on
  `"actor"`, `LTM.Source` on `"kind"`. `update_node`'s
  `Option<Option<String>>` is `LTM.FieldEdit { unchanged, clear, set(T) }`.
  `LTM.ApplyOutcome` decodes on `"outcome"`.
- **Shared coders**: `LTMResponses.swift` `makeEncoder()`/`makeDecoder()`
  (no `.convertFromSnakeCase` — that would corrupt discriminator keys).

Standalone `swiftc` harnesses live in `magios/ThinkingMapCanonicalTests/`
(outside the `Magios/` source glob). XCTests `LTMWireRoundtripTests`,
`LTMAPIClientTests`, `LTMSyncStoreTests` live under `magios/MagiosTests/`,
driven by generated `LTMWireFixtures.swift`. Raw `Fixtures/` JSON is
excluded from the app bundle via `project.yml`.

## API client

`LTM.APIClient` over an injectable `LTM.Transport`
(`LTM.URLSessionTransport` in production). One `async throws` method per
endpoint: create / list / get / patch / operations / interpret / events /
replay / restore, plus `consolidate`, `decideProposal`,
`attachSession`/`detachSession` (`.coordinatorUnavailable` for
`503 coordinator_unavailable`), and `promoteNode` (`.confirmationRequired`
for 409 `confirmation_required`). Shared workspace-bound bearer auth,
per-segment percent-encoding, typed `LTM.APIError`. Injected `baseURL` +
`LTM.Scope` + transport; no app dependency. Production wires `MagicianAccess`.

`listMaps(limit:offset:)` walks `GET /thinking-maps?limit=&offset=` in
100-summary pages (id-deduped, raw-offset advance, 100-page runaway cap,
partial results on mid-loop error) → `LTM.SummaryPage`.

## Sync store

`LTM.SyncStore` is server-authoritative: persisted last-authoritative map
cache + persisted offline op-queue. `apply` runs a cosmetic optimistic
transform (`LTM.optimisticApply`) then enqueues + `flush`es. `flush` drains
in order using the cached revision; stale `base_revision` returns **409
`revision_conflict`**, which triggers `refresh` + one retry, then escalation
to `conflicts` without blocking the queue; transport errors stop the drain.
`interpret` is online-only. Injectable `LTM.Persistence` (file-backed in
prod). The backend surfaces the 409 in `thinking_maps_api.rs`.

`ThinkingMapModel` reads via a projection (`LTM.Map` → E0 view types —
kinds mapped case-insensitively, tree from `parent_id`, `suggested` from
`model_inferred`/`provisional`) and writes owner operations through
`LTM.SyncStore`. The projection retains exact canonical node and related-edge
string keys alongside UUID view identities; callers must use those retained
keys for writes. A stale or removed interpret focus returns typed
`invalidFocusNode`. `ThinkingMapIntelligenceController.refresh` calls
`SyncStore.interpret` with continue / break-open intent and the selected
canonical node id as request-scoped focus. Offline / errors fall back to the
"AI unavailable" palette.

The map library is projected from `listMaps`. Pin / preferredMode /
lastOpened are client-local in `CanonicalMapLocalPrefs`. New / rename /
archive / delete via `createMap` / `patchMap` (delete = soft
`lifecycle:.deleted`; duplicate = restore-as-branch).
`importLocalMapsIfNeeded` replays existing `UserDefaults` maps as owner
operations. `beginNewMap(with:)` is sequential: create → re-point → seed →
select. `addThought`/`begin` select the new node via `set_shared_view`.
`duplicateMap` delivers the copy's id via a main-actor completion once the
async create lands.

## Clarifications and restructure

The interpreter emits `create_clarification`; `/consolidate` stages an
owner-confirmable board reorganization. `LTM.SyncStore` —
`consolidate()` / `decideProposal(_:decision:)` (online-only, adopt returned
map) and `respondClarification(_:answer:)` (`resolve_clarification` owner op
through apply/queue/flush, works offline). Projection —
`projectClarifications` (open only) + `projectProposals` (pending only);
`resolveNodeUUID` keeps node refs aligned. `ThinkingMapModel` publishes
`clarifications` + `pendingProposals`. SwiftUI: **Reorganize** in the
actions menu; a restructure proposal card above the composer; "?" chip +
answer field in node detail.

## Listen mode

Hands-free realtime voice attaches to the open map; finalized spoken turns
auto-map via the server-side
[ambient coordinator](../magician/live-thinking-map.md).
`LTM.SyncStore` — online-only `attachSession` / `detachSession` (throw
`.transport` offline). `ThinkingMapModel` — `@Published isListening` /
`listeningUnavailable`; `startListening(sessionID:)` attaches then
PUSH-FIRST refresh: `ThinkingMapRealtime` subscribes to
`ThinkingMapUpdated` on `/realtime/ws` and re-fetches; a 20 s safety poll
bounds staleness if the socket drops. `stopListening()` on `onDisappear`.
`RealtimeVoiceClient` exposes `mediaSessionID` + `awaitReadySessionID()`;
the view starts a hands-free call (ui-thread id `thinking-map-<mapId>`),
waits for `.ready`, then hands the **media session id** to the model (what
the coordinator matches against `presence_session_id`). Prototype view:
**Listen** toggle + status banner; auto-stops if the call drops.

## Share ingestion

Shared TEXT and webpage URLs seed a Thinking Map from the iOS share sheet
via Magican Assist. No networking in the extension. The extension persists
shared text/URL into App Group `SharedInbox` with `dest = thinking_map`
(+ `sourceURL`), then best-effort foregrounds (`magican://share`).
`ShareRouter.drainInbox` builds `ShareThinkingMapSeed` (bounded first-line
label; full text in detail behind `Shared from <host> — <url>`) and
presents via `ThinkingMapRouter.present(initialThought:detail:)` →
`beginNewMap(with:detail:)`. Share seed is asserted **`owner_spoken`**;
imported-content provenance lives in the node's **detail markdown**. The
authority matrix in `thinking_map/validation.rs` is not weakened.

**Append:** **Add to current Thinking Map** (`dest = thinking_map_append`)
routes the same seed to `ThinkingMapModel.appendToMostRecentMap`: refresh
library → most-recently-opened non-archived map (`mostRecentAppendTarget`;
a pin is not recency) → open → append as root-level owner thought →
select, sequenced as one task. Empty/all-archived library falls back to
`beginNewMap`. `ThinkingMapRouter.present(…, disposition: .appendToRecent)`.

## Promote

Node detail **SEND ONWARD** calls
`POST /thinking-maps/{map}/nodes/{node}/promote` (`target = "task" |
"memory"`). `LTM.APIClient.promoteNode` → `LTM.PromoteNodeResult`; 409
`confirmation_required` for non-owner-asserted nodes.
`LTM.SyncStore.promoteNode` is online-only; the backend refreshes so the
node re-projects with its new promoted ref. `ThinkingNode.promotedKinds`
defaults to `[]` on decode. Already-promoted kinds render as badges; 409
shows "AI-suggested — promote anyway?" and retries with `confirm: true`.

## UI-test transport

`--ui-test` (offline) wires `LTM.InMemoryTransport` (in-process fake of
create/list/get/patch/operations via `LTM.optimisticApply` +
`set_shared_view`/`disconnect`) and `LTM.EphemeralPersistence`. Request
handling serializes with `NSLock.withLock`. Production is untouched.

## Notes

`brainstorm-facilitator` is retained: `chat/service.rs` still keys live
behavior on that agent id, though the map no longer uses it.
