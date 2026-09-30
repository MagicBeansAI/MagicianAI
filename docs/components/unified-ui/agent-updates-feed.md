# Agent Update Feed (Operator Timeline) — UI Architecture

`/feed` is the operator Insights / Review / Activity surface. The Activity tab
is the live chronological timeline of agent-update events. Pairs with the
backend semantic layer in [`../magician/agent-updates.md`](../magician/agent-updates.md).

- **Plan (archived):** `docs/archive/plans/2026-04-23-agentic-ui-semantic-layer.md`
- **Route:** `/feed` (also `/debug/update-cards` for fixture preview)

## Data flow

```
                  RuntimeTransportBroadcaster
                  (event_type = "agent.update")
                              |
             +----------------+----------------+
             |                                 |
             v                                 v
      WebSocket payload              JSONL journal on disk
             |                                 |
             v                                 v
      v2-websocket.ts                GET /api/magician/v2/updates
   handleAgentUpdateEnvelopeEvent             |
             |                      fetch on /feed mount
             +------------+--------------------+
                          v
                   agentUpdateStore
             (scope-keyed Map, dedup by id,
                    500/scope cap)
                          v
            /feed Activity tab: filters (kind / agent / thread / hide-resolved)
                          v
            groupConsecutive() — kind-level collapsing
                          v
            AgentUpdateCard dispatcher → card components
```

Insights and Review are a separate `createFeedStore({ limit: 200 })` lane over
`GET /api/magician/v2/feed` (and `/feed/counts`), not the agent-update store.

## Files

| Path | Responsibility |
|---|---|
| `src/lib/types/agentUpdate.ts` | Discriminated-union TypeScript mirror of the Rust enum |
| `src/lib/stores/agentUpdateStore.ts` | Scope-keyed event store, ingest + read accessors |
| `src/lib/stores/feedStore.ts` | Learning/feed items for Insights and Review |
| `src/lib/realtime/v2-websocket.ts` | Dispatches envelopes to `handleAgentUpdateEnvelopeEvent` |
| `src/lib/magician/components/agent-updates/registry.ts` | Maps `AgentUpdateKindTag` to Svelte components |
| `src/lib/magician/components/agent-updates/AgentUpdateCard.svelte` | Dispatcher on `event.kind` |
| `src/lib/magician/components/agent-updates/*.svelte` | 13 dedicated cards + `UnknownCard` fallback |
| `src/lib/magician/components/agent-updates/fixtures.ts` | One sample event per variant |
| `src/routes/(app)/feed/+page.svelte` | Insights / Review / Activity surface |
| `src/routes/(app)/debug/update-cards/+page.svelte` | Fixture gallery for visual QA |

## Type system

```ts
export type AgentUpdate = AgentUpdateEnvelope & AgentUpdateKind;

export interface AgentUpdateEnvelope {
  id: string;      // ULID
  ts: number;      // unix millis
  workspace_id: string;
  agent_id: string;
  thread_id?: string;
  cycle_id?: string;
}

export type AgentUpdateKind =
  | { kind: 'cycle_started'; focus_area?: string; trigger: string }
  | { kind: 'cycle_completed'; outcome: CycleOutcome; duration_ms: number }
  | { kind: 'approval_requested'; approval_id: string; tool?: string; action?: string; params?: unknown; pending_action_count?: number }
  // ... 27 more variants
  ;
```

The `Envelope & Kind` intersection lets cards narrow every field on `event.kind`.

## Store

`agentUpdateStore` is a `writable<Map<ScopeKey, ScopeState>>` keyed
`${principal}/${workspace}`. Both ingest paths dedupe by `event.id`:

- `seedScope(principal, workspace, events[])` — after the `/feed` page's initial
  `GET /updates`.
- `handleAgentUpdateEnvelopeEvent(envelope)` — from `v2-websocket.ts`; ignores
  non-`agent.update` types and envelopes missing `principal` or `workspace`.

**Cap:** 500 events per scope, keeping the newest; `seenIds` is rebuilt after
capping so evicted ids can re-enter on replay. Readers (`Readable<AgentUpdate[]>`,
newest first): `updatesForScope(p, w)` and `updatesForThread(p, w, threadId)`
(empty `threadId` → all). Scopes are not auto-GC'd; consumers call
`clearScope(p, w)` on scope switch before seeding.

## Feed page (`/feed`)

`activeTab` is `'insights' | 'review' | 'activity'` (default `insights`). On mount
the page subscribes to `scopeIdentityStore`, starts the feed store, and on
bearer/session change clears the old projection, resubscribes to
`updatesForScope(new)`, fetches `GET /api/magician/v2/updates?limit=200`, and
seeds; WebSocket events append reactively. **Insights** renders
`learning_insight` items (`LearningInsightFeedCard`), **Review**
`learning_candidate` items (`LearningCandidateFeedCard`); `?selected_item=`
focuses a card.

**Activity** filters: Kind and Agent (values present), Thread (only when any
event has `thread_id`; page-side), and **Hide resolved** (default on: hides
`approval_requested` events whose `approval_id` has a later
`approval_resolved`/`approval_expired`, showing the hidden count).

`groupConsecutive` folds consecutive events sharing `kind + agent_id`; groups of
2+ show the newest card plus a `+N more` toggle. Meta strip:
`[kind] [agent_id] [cycle cycle_id] [time ago]  [+2 more | Collapse]`. The group
key is its newest event id, so a newer same-kind event remounts the group and
resets expansion.

## Card registry

`getCardForKind(kind)` returns `registry[kind] ?? UnknownCard`. 13 dedicated
components cover all 30 kinds:

| Component | Kinds it handles |
|---|---|
| `CycleProgressCard` | `cycle_started`, `cycle_completed`, `cycle_failed`, `cycle_paused` |
| `ApprovalRequestCard` | `approval_requested`, `approval_resolved`, `approval_expired` |
| `ArtifactReadyCard` | `artifact_created`, `artifact_create_failed` |
| `TaskCard` | `task_created`, `task_updated`, `task_completed`, `task_failed` |
| `CircuitCard` | `circuit_opened`, `circuit_recovered` |
| `GoalCard` | `goal_completed`, `goal_failed`, `goal_recovered` |
| `DelegationCard` | `delegation_issued`, `delegation_resolved` |
| `AgentLifecycleCard` | `agent_created`, `agent_updated`, `agent_deleted`, `agent_paused`, `agent_resumed` |
| `MemoryReportCard` | `memory_report` |
| `FeedbackCard` | `feedback_generated` |
| `TierConsolidationCard` | `tier_consolidated` |
| `FeedStalledCard` | `feed_stalled` |
| `PublishedSurfaceCard` | `published_surface_changed` |
| `UnknownCard` | fallback for any unregistered kind |

Cards compose [GAUI](../../../ui/unified-ui/src/lib/magician/components/generative/)
`Card` and `Badge`; they are domain-coupled to `AgentUpdateKind`, not GAUI
primitives or MuijRenderer nodes. `/debug/update-cards` renders one fixture per
kind, tagging any that fall back to `UnknownCard`.

Tests: `registry.test.ts` (every kind has a dedicated card, fixtures cover all
variants), `agentUpdateStore.test.ts`.

## Coexistence with existing UI

The Activity timeline is additive. Chat keeps its own `ChatMessage` stream
(different ontology). `/approvals` 301-redirects to `/attention`, mapping legacy
`approval_id` / `correlation_id` onto `attention_item`. Task lists, the
Attention Bar and Published Surface browse keep their own data paths.

## Known limitations

- Rapid scope switches can briefly race the shared `loading`/`error` UI state;
  data stays correct because each `seedScope` captures its own scope.
- Every `emit_task_updated_transport` call emits an event, so same-status task
  progressions duplicate; kind-level collapsing folds them visually.
