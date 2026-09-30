/**
 * Canonical Human-In-The-Loop request/response shapes.
 *
 * Today the backend emits at least four distinct event shapes that all
 * mean "the system is waiting for a human" — `AgenticWaitingForUser`,
 * `UserRequestPending`, `approval.requested`, `ClarificationQueued`. Each
 * has its own pause identifier, its own resume endpoint, and its own
 * frontend dispatch path. This module defines a single normalized
 * `HitlRequest` shape that every UI surface can consume, plus a single
 * `HitlResponse` value type that every resolve call can carry.
 *
 * The backend emits canonical `HitlRequested` / `HitlResolved` events and
 * accepts every response through `/api/magician/v2/hitl/{id}/respond`.
 * Adapters in `adapters.ts` normalize the canonical event and remaining
 * persisted feed projections into the same request contract.
 */

/**
 * Where the request originated. The canonical responder uses this to select
 * its backend dispatcher, and the UI uses it to label the source for operators.
 */
export type HitlSource =
	| 'agentic' // ExecutionPauseKind::UserInput / Confirmation / MaxIterations / Escalation / PrimitiveUserInput
	| 'user_request' // /user-requests/{pause_state_id}/respond — the V2 user-request channel
	| 'approval' // approval.requested — distinct resolve endpoint
	| 'plan_approval' // Draft task plan awaiting Approve / Reject (Phase F)
	| 'clarification' // planning-side ClarificationQueued
	| 'escalation' // max_iterations / cannot_proceed / loop_detected
	| 'diff_approval' // Staged file-edit transaction awaiting Apply / Reject
	| 'service_health' // Provider/service outage, authentication, or credit notice
	| 'bot_auth'; // Bot adapter needs sign-in (gmail OAuth / WhatsApp QR / Telegram pair / Kapso) — emitted by bots/auth_hitl_broker.rs on NeedsAuth / AccountMismatch transition

/**
 * Input shapes the backend currently surfaces. Mirrors Rust
 * `UserInputType` (`magician_v2/execution/agentic/types.rs`). Each
 * variant carries the schema needed to render its input UI. `form`
 * is several questions in one pause.
 */
export type HitlInputType =
	| 'text'
	| 'password'
	| 'otp'
	| 'choice'
	| 'multi_choice'
	| 'confirmation'
	| 'external_action'
	| 'file_path'
	| 'guidance'
	| 'tool_authorization'
	| 'sandbox_override'
	| 'diff_approval'
	| 'form';

export interface HitlChoiceOption {
	id: string;
	label: string;
	description?: string;
	requires_input?: boolean;
}

/**
 * What kind of secret an ask (or one of its form fields) collects. Mirrors
 * Rust `SensitiveKind` (`magician_v2/user_requests/sensitive.rs`).
 */
export type HitlSensitiveKind = 'login_identifier' | 'password' | 'otp' | 'other';

/**
 * The server-decided, value-free sensitivity of an ask — mirrors Rust
 * `SensitiveInputSpec`. Published on `hitl.requested` as
 * `input_schema.sensitive` and on the pending-request listings.
 *
 * **This, not the request-type name or the wording, is what a surface masks
 * by.** `kind` describes a single-value ask; `fields` lists the flagged ids of
 * a form, and a field absent from it is ordinary. `one_time` material has a
 * short `collection_deadline_ms`, after which the only honest answer is to
 * ask for a fresh code.
 */
export interface HitlSensitiveSpec {
	kind?: HitlSensitiveKind;
	fields?: Array<{ id: string; kind: HitlSensitiveKind }>;
	provenance?: 'producer' | 'typed_input' | 'form_schema' | 'heuristic';
	one_time?: boolean;
	collection_deadline_ms?: number;
	challenge_id?: string;
	revision?: number;
}

export interface HitlDiffApprovalFile {
	path: string;
	status: string;
	additions: number;
	deletions: number;
	unified_diff: string;
}

/**
 * Per-input-type schema details. Optional fields populate from
 * `input_schema` metadata; absent fields fall back to defaults at render
 * time.
 */
export interface HitlInputSchema {
	/** Built-in secure prompts cancel their pending operation when dismissed. */
	request_type?: string;
	/**
	 * Present when the backend classified the ask as collecting a secret.
	 * Read by `promptFor` to mask fields, band the prompt, show the collection
	 * deadline, and treat dismissal as an explicit cancel.
	 */
	sensitive?: HitlSensitiveSpec;
	/** `text`/`guidance` — free-form input */
	multiline?: boolean;
	placeholder?: string;
	/** `text` — character cap */
	max_length?: number;

	/** `choice`/`multi_choice` */
	options?: HitlChoiceOption[];
	/** `choice` — allow a free-text "other" alongside the listed ids */
	allow_other?: boolean;
	/** `multi_choice` — selection bounds (0 = no bound) */
	min_selections?: number;
	max_selections?: number;

	/** `external_action` — instructions shown above the optional guidance input */
	instructions?: string;
	/** `external_action` — label for the acknowledgement control (Rust `done_label`). */
	done_label?: string;

	/** `file_path` */
	multiple?: boolean;
	filter?: string;

	/** `confirmation` — optional confirm/deny labels */
	confirm_label?: string;
	deny_label?: string;
	/**
	 * `confirmation` — the backend's own `destructive` flag. Read by the prompt
	 * surface to band the decision, never to change what is posted: a confirmed
	 * destructive action and a confirmed benign one are the same response value.
	 */
	destructive?: boolean;

	/**
	 * `tool_authorization` — what the agent wants to call, and with what.
	 *
	 * Both are shown **verbatim**. The prompt text is a sentence the backend
	 * composed around `tool_name`; these are the fact itself, and a grant made
	 * against a paraphrase is a grant made against something the reader did not
	 * see.
	 */
	tool_name?: string;
	params_summary?: string;

	/**
	 * `sandbox_override` — the command that violated policy, the policy it
	 * violated, and the file roots the grant would cover.
	 *
	 * `allowed_roots` is empty for shell overrides and for pause records written
	 * before file-path HITL support, which is why an empty list renders no roots
	 * line rather than "no roots" — the same absence rule the rest of this
	 * contract follows.
	 */
	command?: string;
	violation?: string;
	allowed_roots?: string[];

	/** Hint text shown alongside the prompt */
	context?: string;
	suggestions?: string[];

	/** `form` — several questions in one pause. */
	questions?: Array<{
		id: string;
		prompt: string;
		input_type?: string;
		options?: HitlChoiceOption[];
	}>;

	/** Phase H4 — clarifications carry a slot id that the answer fills.
	 *  Surfaces that render slot-aware UI (Plan Inspector clarification
	 *  panel) read this when present. Empty/undefined for non-slot
	 *  clarifications and for non-clarification sources. */
	source_slot_id?: string;
	/** Phase H4 — clarification stage. Informational only; does not
	 *  affect resume routing (one canonical source value covers all
	 *  stages — the planning orchestrator picks the right resume path
	 *  from the persisted state). Surfaces can use it to label
	 *  "planning clarification" vs "in-execution clarification". */
	stage?: 'planning_bootstrap' | 'planning_iteration' | 'execution_cycle' | 'follow_up' | 'unknown';

	/** Multi-stage MVP — when the planner emits N clarifications in
	 *  one batch, every question in the batch shares this `chain_id`
	 *  and carries its 1-indexed `chain_position` plus the batch's
	 *  `chain_total`. The AttentionPromptModal renders a
	 *  "STEP X OF N" eyebrow when these are present so the operator
	 *  sees the batch as one logical decision rather than N
	 *  independent prompts. Absent on single-question emissions and
	 *  on non-clarification sources. */
	chain_id?: string;
	chain_position?: number;
	chain_total?: number;

	/** `diff_approval` — staged FileEditTransaction or CodeChangeProposal payload. */
	transaction_id?: string;
	proposal_id?: string;
	approval_source?: 'transaction' | 'proposal' | string;
	rationale?: string;
	files?: HitlDiffApprovalFile[];
}

/**
 * Where the request belongs. At least one of `task_id` / `execution_id` /
 * `agent_id` should be present; the UI uses these for `/events` deep-link
 * scoping and for grouping/counting in attention surfaces / Internals
 * drawer.
 */
export interface HitlScope {
	principal?: string;
	workspace?: string;
	workflow_id?: string;
	task_id?: string;
	execution_id?: string;
	agent_id?: string;
	thread_id?: string;
}

/**
 * Identifiers needed to resolve the request. Only one of `pause_state_id`
 * / `approval_id` / `correlation_id` is canonical for any given source —
 * the endpoint resolver in `adapters.ts` knows which to use.
 */
export interface HitlIdentifiers {
	pause_state_id?: string;
	approval_id?: string;
	correlation_id?: string;
	request_id?: string;
}

/**
 * Self-contained payload for opening a HITL prompt from any projection.
 * Unlike a feed-row id, this contract carries everything needed to render and
 * resolve the prompt without hydrating the paginated Attention store.
 */
export interface HitlOpenTarget {
	id: string;
	source: HitlSource;
	input_type: HitlInputType;
	prompt: string;
	hint?: string | null;
	input_schema?: HitlInputSchema | null;
	identifiers?: HitlIdentifiers;
	scope?: HitlScope;
	at?: number;
}

/**
 * Canonical HITL request. Adapters in `adapters.ts` build this from
 * `FeedItem` (attention/feed surfaces), `AgenticPause` events
 * (ExecutionPanel pauses), or escalation messages
 * (chat/thread pages).
 */
export interface HitlRequest {
	/** Stable id used for store keys and deduplication. Prefer
	 *  `pause_state_id` → `approval_id` → `correlation_id` → `request_id`
	 *  → fallback synthetic id. */
	id: string;
	source: HitlSource;
	input_type: HitlInputType;
	schema: HitlInputSchema;
	prompt: string;
	hint?: string;
	scope: HitlScope;
	identifiers: HitlIdentifiers;
	/**
	 * Wall-clock ms when the request was raised. Used for `↗` deep-link
	 * anchors and for sorting/timeout display. Optional because some
	 * legacy adapters don't carry a timestamp.
	 */
	at?: number;
	/** Free-form metadata pass-through for adapters that need to round-trip
	 *  fields the canonical shape doesn't model (e.g., legacy
	 *  `attention_kind`). The component should not read these directly;
	 *  resolve helpers in `adapters.ts` consume them when posting back. */
	raw?: Record<string, unknown>;
}

/**
 * Canonical HITL response. Mirrors backend `AgenticResumeValue`. The
 * adapter layer picks the right endpoint and request body shape based on
 * `request.source`.
 */
export type HitlResponseValue =
	| { type: 'text'; value: string }
	| { type: 'password'; value: string }
	| { type: 'choice'; selected_id: string; other_value?: string }
	| { type: 'multi_choice'; selected_ids: string[] }
	| { type: 'confirmation'; confirmed: boolean }
	| { type: 'external_action_completed'; guidance?: string }
	| { type: 'file_path'; paths: string[] }
	| { type: 'guidance'; advice: string }
	| { type: 'aborted'; reason?: string }
	| {
			type: 'form';
			answers: Array<{
				id: string;
				skipped?: boolean;
				value?: string;
				selected_ids?: string[];
			}>;
	  };

export interface HitlResponse {
	request: HitlRequest;
	value: HitlResponseValue;
	/** When `true`, the UI should not POST — the operator chose to dismiss
	 *  the dialog without responding. */
	cancelled?: boolean;
}

/**
 * Result of an attempted resolve. Surfaces let the caller decide how to
 * present errors (toast, inline, both).
 */
export type HitlResolveOutcome =
	| { ok: true }
	| {
			/**
			 * Validation-reject / re-ask: the backend accepted the request but
			 * asked the operator to revise their answer (HTTP 200 with
			 * `{resumed:false, status:"reask_required", question, hint,
			 * previous_answer}`). Callers should re-open the input modal with
			 * the clarified `question`/`hint` (seeded with `previousAnswer`)
			 * rather than treating it as a terminal error — the pause is still
			 * live and the answer was not lost.
			 */
			ok: false;
			reask: true;
			status: number;
			message: string;
			question?: string;
			hint?: string;
			previousAnswer?: string;
	  }
	| { ok: false; status: number; message: string }
	| { ok: false; cancelled: true };
