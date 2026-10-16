/**
 * Live Thinking Map — canonical wire contract (hand-written TypeScript).
 *
 * Every type here mirrors the REAL backend serde wire contract exactly:
 *   - Rust: `magician-surfaces/src/thinking_map/{models,operations,store}.rs`
 *     and `magician/src/magician_v2/api/thinking_maps_api.rs`.
 *   - Swift (a proven mirror of the same contract):
 *     `magios/Magios/ThinkingMapCanonical/LTM*.swift`.
 *
 * All JSON is snake_case; field names below match the wire byte-for-byte (no
 * key-conversion strategy is applied on either side). Do NOT invent fields —
 * this is a faithful mirror.
 *
 * Skip-if-none serde fields (`skip_serializing_if = "Option::is_none"`) are
 * modeled as optional (`?:`). `#[serde(default)]`-only collection/bool/enum
 * fields are ALWAYS present on the wire, so they are required (non-optional).
 * The `BTreeMap` collections (`nodes`/`edges`/`clarifications`/`proposals`) are
 * JSON objects keyed by id → `Record<string, T>`. `applied_envelopes`
 * (a `VecDeque`) is a JSON array.
 */

// ── Enums (serde snake_case → string-literal unions) ─────────────────────────

/** `NodeKind` — first-class kinds of thinking node (11). */
export type NodeKind =
	| 'idea'
	| 'fact'
	| 'question'
	| 'decision'
	| 'option'
	| 'risk'
	| 'action'
	| 'metric'
	| 'assumption'
	| 'evidence'
	| 'group';

/** `EpistemicState` — how settled/true a node currently is (7). */
export type EpistemicState =
	| 'provisional'
	| 'asserted'
	| 'confirmed'
	| 'contradicted'
	| 'rejected'
	| 'resolved'
	| 'superseded';

/** `AssertionOrigin` — where an assertion (node/edge) originated (6). */
export type AssertionOrigin =
	| 'owner_spoken'
	| 'participant_spoken'
	| 'owner_edited'
	| 'imported_source'
	| 'model_inferred'
	| 'system_derived';

/** `EdgeKind` — directed relationship kinds between nodes (9). */
export type EdgeKind =
	| 'related_to'
	| 'supports'
	| 'contradicts'
	| 'answers'
	| 'depends_on'
	| 'leads_to'
	| 'alternative_to'
	| 'measures'
	| 'grouped_under';

/** `MapLifecycle` — map lifecycle status. Rust default = `active`. */
export type MapLifecycle = 'active' | 'paused' | 'archived' | 'deleted';

/** `ViewLens` — which lens the shared view renders through. Default `graph`. */
export type ViewLens = 'graph' | 'mind_map' | 'outline' | 'decision' | 'metrics';

/** `PromotionKind` — destination for a node promoted to another surface. */
export type PromotionKind = 'task' | 'today' | 'memory';

/** `ClarificationState` — lifecycle of a clarification. Default `open`. */
export type ClarificationState = 'open' | 'answered' | 'deferred' | 'dismissed';

/** `ProposalState` — lifecycle of a restructure proposal. Default `proposed`. */
export type ProposalState = 'proposed' | 'confirmed' | 'rejected' | 'deferred';

/**
 * `InterpretIntent` — steering intent for `/interpret`. Default
 * `continue_thinking`; unknown/absent falls back to it server-side.
 */
export type InterpretIntent = 'continue_thinking' | 'break_open';

// ── Internally-tagged unions ─────────────────────────────────────────────────

/**
 * `ThinkingMapSource` — internally tagged on `"kind"`. `solo` carries no
 * payload (`{"kind":"solo"}`); the rest carry a single string field.
 */
export type ThinkingMapSource =
	| { kind: 'solo' }
	| { kind: 'meeting'; thread_id: string }
	| { kind: 'observe'; session_id: string }
	| { kind: 'chat'; thread_id: string }
	| { kind: 'imported'; source_kind: string }
	| { kind: 'tutor'; lesson_id: string };

/**
 * `OperationActor` — internally tagged on `"actor"`. The `model` variant's
 * `trace_id` is `Option<String>` with `skip_serializing_if` → absent when null.
 */
export type OperationActor =
	| { actor: 'owner'; principal: string }
	| { actor: 'participant'; speaker_id: string }
	| { actor: 'model'; trace_id?: string }
	| { actor: 'trusted_system'; component: string }
	| { actor: 'imported'; source_kind: string };

// ── Supporting structs ───────────────────────────────────────────────────────

/** `SharedViewState`. `active_node` skips-if-none; `lens` always on the wire. */
export interface SharedViewState {
	active_node?: string;
	lens: ViewLens;
}

/** `SpeakerRef`. `display_name` skips-if-none. */
export interface SpeakerRef {
	speaker_id: string;
	display_name?: string;
}

/** `SourceRef`. Every field skips-if-none. */
export interface SourceRef {
	utterance_id?: string;
	thread_id?: string;
	quote?: string;
	timestamp?: string;
}

/** `Position`. `x`/`y` are `f64`. */
export interface Position {
	x: number;
	y: number;
}

/** `PromotedRef` — record of a node promoted into another product surface. */
export interface PromotedRef {
	destination_kind: PromotionKind;
	object_id: string;
	linked_at: string;
}

/** `Clarification` — a question raised against a node. */
export interface Clarification {
	clarification_id: string;
	node_id: string;
	question: string;
	/** `#[serde(default)]` → always on the wire. */
	state: ClarificationState;
	answer?: string;
	created_at: string;
	resolved_at?: string;
}

/** `RestructureProposal` — a proposed structural rewrite (a bundle of ops). */
export interface RestructureProposal {
	proposal_id: string;
	/** Server-authoritative proposer (serde default; always emitted). */
	proposed_by: OperationActor;
	rationale: string;
	operations: MapOperation[];
	state: ProposalState;
	affected_node_ids: string[];
	created_at: string;
	resolved_at?: string;
}

// ── Core graph elements ──────────────────────────────────────────────────────

/** `ThinkingNode` — a first-class node in the map. `confidence` is `f32`. */
export interface ThinkingNode {
	node_id: string;
	kind: NodeKind;
	label: string;
	detail_markdown?: string;
	epistemic_state: EpistemicState;
	assertion_origin: AssertionOrigin;
	confidence: number;
	speaker?: SpeakerRef;
	source_refs: SourceRef[];
	parent_id?: string;
	position?: Position;
	position_locked: boolean;
	promoted_refs: PromotedRef[];
	tombstoned: boolean;
	created_at: string;
	updated_at: string;
}

/** `ThinkingEdge` — a directed edge between two nodes. */
export interface ThinkingEdge {
	edge_id: string;
	from_node: string;
	to_node: string;
	kind: EdgeKind;
	assertion_origin: AssertionOrigin;
	tombstoned: boolean;
	created_at: string;
	updated_at: string;
}

/**
 * `AppliedEnvelopeRecord` — idempotency-ledger bookkeeping (EXCLUDED from the
 * semantic hash).
 */
export interface AppliedEnvelopeRecord {
	envelope_id: string;
	idempotency_key: string;
	resulting_revision: number;
}

// ── The full map document ────────────────────────────────────────────────────

/** `ThinkingMap` — the full Live Thinking Map document. */
export interface ThinkingMap {
	schema_version: number;
	map_id: string;
	principal: string;
	workspace: string;
	title: string;
	source: ThinkingMapSource;
	lifecycle: MapLifecycle;
	revision: number;
	view_state: SharedViewState;
	nodes: Record<string, ThinkingNode>;
	edges: Record<string, ThinkingEdge>;
	clarifications: Record<string, Clarification>;
	proposals: Record<string, RestructureProposal>;
	applied_envelopes: AppliedEnvelopeRecord[];
	created_at: string;
	updated_at: string;
}

// ── Operations (internally tagged on `"op"`) ─────────────────────────────────

/**
 * `MapOperation` — a single bounded change to a `ThinkingMap`, internally
 * tagged on `"op"`. All 22 Rust variants are represented.
 *
 * `update_node.detail_markdown` mirrors Rust `Option<Option<String>>`:
 *   - key absent           → leave the field unchanged
 *   - key present == null   → clear to null
 *   - key present == string → set
 * Modeled as `detail_markdown?: string | null` (omit / `null` / value).
 */
export type MapOperation =
	| { op: 'add_node'; node: ThinkingNode }
	| {
			op: 'update_node';
			node_id: string;
			label?: string;
			detail_markdown?: string | null;
			confidence?: number;
	  }
	| { op: 'set_node_kind'; node_id: string; kind: NodeKind }
	| { op: 'set_epistemic_state'; node_id: string; state: EpistemicState }
	| { op: 'tombstone_node'; node_id: string }
	| { op: 'restore_node'; node_id: string }
	| { op: 'connect'; edge: ThinkingEdge }
	| { op: 'disconnect'; edge_id: string }
	| { op: 'move_to_parent'; node_id: string; parent_id?: string }
	| { op: 'move_node'; node_id: string; position: Position }
	| { op: 'set_position_lock'; node_id: string; locked: boolean }
	| { op: 'create_clarification'; clarification: Clarification }
	| {
			op: 'resolve_clarification';
			clarification_id: string;
			state: ClarificationState;
			answer?: string;
	  }
	| { op: 'propose_restructure'; proposal: RestructureProposal }
	| { op: 'confirm_restructure'; proposal_id: string }
	| { op: 'reject_restructure'; proposal_id: string }
	| { op: 'set_shared_view'; view_state: SharedViewState }
	| { op: 'link_promoted_object'; node_id: string; promoted: PromotedRef }
	| {
			op: 'unlink_promoted_object';
			node_id: string;
			destination_kind: PromotionKind;
			object_id: string;
	  }
	| { op: 'rename_speaker'; old_speaker_id: string; new_display_name: string }
	| { op: 'set_title'; title: string }
	| { op: 'set_lifecycle'; lifecycle: MapLifecycle };

/** `ModelTraceRef`. `model_profile` skips-if-none. */
export interface ModelTraceRef {
	trace_id: string;
	model_profile?: string;
}

/** `MapOperationEnvelope` — a batch of ops applied atomically at `base_revision`. */
export interface MapOperationEnvelope {
	schema_version: number;
	envelope_id: string;
	map_id: string;
	base_revision: number;
	utterance_id?: string;
	actor: OperationActor;
	idempotency_key: string;
	operations: MapOperation[];
	model_trace?: ModelTraceRef;
	created_at: string;
}

// ── Store / list / event-log types ───────────────────────────────────────────

/** `MapSummary` — lightweight list item returned by `list_maps`. */
export interface MapSummary {
	map_id: string;
	title: string;
	lifecycle: MapLifecycle;
	latest_revision: number;
	updated_at: string;
}

/** `MapEvent` — one applied envelope in the append-only event log. */
export interface MapEvent {
	sequence: number;
	envelope: MapOperationEnvelope;
	resulting_revision: number;
	semantic_hash: string;
	applied_at: string;
}

/** `MapManifest` — the per-map head record. `branched_from_*` skip-if-none. */
export interface MapManifest {
	schema_version: number;
	map_id: string;
	principal: string;
	workspace: string;
	title: string;
	source: ThinkingMapSource;
	lifecycle: MapLifecycle;
	latest_revision: number;
	latest_sequence: number;
	latest_semantic_hash: string;
	created_at: string;
	updated_at: string;
	branched_from_map_id?: string;
	branched_from_sequence?: number;
}

// ── Response shapes ──────────────────────────────────────────────────────────

/**
 * The apply/interpret/patch/consolidate/decision response body, discriminated
 * by `"outcome"`:
 *   - `{"outcome":"applied","resulting_revision":N,"semantic_hash":"...","map":{...}}`
 *   - `{"outcome":"idempotent_replay","resulting_revision":N}`
 *   - `{"outcome":"no_operations"}`
 */
export type ApplyOutcome =
	| {
			outcome: 'applied';
			resulting_revision: number;
			semantic_hash: string;
			map: ThinkingMap;
	  }
	| { outcome: 'idempotent_replay'; resulting_revision: number }
	| { outcome: 'no_operations' };
