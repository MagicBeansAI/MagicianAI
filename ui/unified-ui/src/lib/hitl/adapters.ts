/**
 * Adapters between legacy HITL event shapes and the canonical
 * `HitlRequest` / `HitlResponse` types.
 *
 * Today every UI surface that handles HITL re-derives the same things
 * from `FeedItem.metadata`: pause_state_id, execution_id, input_type,
 * input_schema, options, hint, attention_kind. This module centralizes
 * that derivation so:
 *   1. Each surface stops re-implementing the parsing.
 *   2. Canonical events and persisted projections share one consumer shape.
 *   3. The endpoint resolver lives in one place, so source dispatch cannot
 *      drift across surfaces.
 */
import type { FeedItem } from '$lib/feed/types';
import type {
	HitlChoiceOption,
	HitlDiffApprovalFile,
	HitlIdentifiers,
	HitlInputSchema,
	HitlInputType,
	HitlRequest,
	HitlResolveOutcome,
	HitlResponseValue,
	HitlSensitiveKind,
	HitlSensitiveSpec,
	HitlSource
} from './types';

import { timedFetch } from '$lib/shared/fetch';
// ─── Metadata reader helpers ──────────────────────────────────────────

function asRecord(value: unknown): Record<string, unknown> | null {
	if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
	return value as Record<string, unknown>;
}

function readString(record: Record<string, unknown> | null, key: string): string | null {
	if (!record) return null;
	const value = record[key];
	if (typeof value !== 'string') return null;
	const trimmed = value.trim();
	return trimmed.length > 0 ? trimmed : null;
}

function readBoolean(
	record: Record<string, unknown> | null,
	key: string,
	fallback = false
): boolean {
	if (!record) return fallback;
	const value = record[key];
	return typeof value === 'boolean' ? value : fallback;
}

function readNumber(
	record: Record<string, unknown> | null,
	key: string,
	fallback = 0
): number {
	if (!record) return fallback;
	const value = record[key];
	return typeof value === 'number' && Number.isFinite(value) ? value : fallback;
}

function readStrings(record: Record<string, unknown> | null, key: string): string[] {
	if (!record) return [];
	const value = record[key];
	if (!Array.isArray(value)) return [];
	return value
		.map((entry) => (typeof entry === 'string' ? entry.trim() : ''))
		.filter((entry) => entry.length > 0);
}

function readChoiceOptions(value: unknown): HitlChoiceOption[] {
	if (!Array.isArray(value)) return [];
	const out: HitlChoiceOption[] = [];
	for (const entry of value) {
		const record = asRecord(entry);
		if (!record) continue;
		const id = readString(record, 'id') ?? readString(record, 'value');
		const label = readString(record, 'label');
		if (!id || !label) continue;
		out.push({
			id,
			label,
			description: readString(record, 'description') ?? undefined,
			requires_input: readBoolean(record, 'requires_input', false) || undefined
		});
	}
	return out;
}

function readFormQuestions(
	value: unknown
): NonNullable<HitlInputSchema['questions']> {
	if (!Array.isArray(value)) return [];
	const out: NonNullable<HitlInputSchema['questions']> = [];
	for (const entry of value) {
		const record = asRecord(entry);
		if (!record) continue;
		const id = readString(record, 'id');
		const prompt = readString(record, 'prompt') ?? readString(record, 'question');
		if (!id || !prompt) continue;
		out.push({
			id,
			prompt,
			input_type: readString(record, 'input_type') ?? undefined,
			options: readChoiceOptions(record.options)
		});
	}
	return out;
}

const SENSITIVE_KINDS: HitlSensitiveKind[] = ['login_identifier', 'password', 'otp', 'other'];

function readSensitiveKind(value: unknown): HitlSensitiveKind | null {
	return typeof value === 'string' && (SENSITIVE_KINDS as string[]).includes(value)
		? (value as HitlSensitiveKind)
		: null;
}

/**
 * The value-free spec, or `null` when the backend published none. Unknown
 * kinds are dropped rather than guessed: a field the surface cannot classify
 * is rendered as the backend typed it, never as plain text because a newer
 * kind arrived.
 */
export function readSensitiveSpec(value: unknown): HitlSensitiveSpec | null {
	const record = asRecord(value);
	if (!record) return null;
	const spec: HitlSensitiveSpec = {};
	const kind = readSensitiveKind(record.kind);
	if (kind) spec.kind = kind;
	if (Array.isArray(record.fields)) {
		const fields: NonNullable<HitlSensitiveSpec['fields']> = [];
		for (const entry of record.fields) {
			const field = asRecord(entry);
			const id = field ? readString(field, 'id') : null;
			const fieldKind = field ? readSensitiveKind(field.kind) : null;
			if (id && fieldKind) fields.push({ id, kind: fieldKind });
		}
		if (fields.length > 0) spec.fields = fields;
	}
	const provenance = readString(record, 'provenance');
	if (
		provenance === 'producer' ||
		provenance === 'typed_input' ||
		provenance === 'form_schema' ||
		provenance === 'heuristic'
	) {
		spec.provenance = provenance;
	}
	if (typeof record.one_time === 'boolean') spec.one_time = record.one_time;
	const deadline = readNumber(record, 'collection_deadline_ms', 0);
	if (deadline > 0) spec.collection_deadline_ms = deadline;
	const challengeId = readString(record, 'challenge_id');
	if (challengeId) spec.challenge_id = challengeId;
	if (typeof record.revision === 'number') spec.revision = record.revision;
	if (!spec.kind && !spec.fields) return null;
	return spec;
}

function readDiffApprovalFiles(value: unknown): HitlDiffApprovalFile[] {
	if (!Array.isArray(value)) return [];
	const out: HitlDiffApprovalFile[] = [];
	for (const entry of value) {
		const record = asRecord(entry);
		if (!record) continue;
		const path = readString(record, 'path');
		if (!path) continue;
		out.push({
			path,
			status: readString(record, 'status') ?? 'M',
			additions: readNumber(record, 'additions', 0),
			deletions: readNumber(record, 'deletions', 0),
			unified_diff: readString(record, 'unified_diff') ?? ''
		});
	}
	return out;
}

const KNOWN_INPUT_TYPES: HitlInputType[] = [
	'text',
	'password',
	'otp',
	'choice',
	'multi_choice',
	'confirmation',
	'external_action',
	'file_path',
	'guidance',
	'tool_authorization',
	'sandbox_override',
	'diff_approval',
	'form'
];

/**
 * One of the eleven input types, or `null`.
 *
 * **Exported so no surface keeps a second copy of the list.** `HitlInputType` is
 * a TypeScript union over a field that arrives as a bare JSON string, so the
 * declared type is a claim about the wire rather than a check on it — a payload
 * carrying a twelfth spelling type-checks and then reaches a renderer that has
 * no branch for it. Every boundary that decides how to render an ask has to
 * narrow the string here first.
 */
export function normalizeHitlInputType(value: unknown): HitlInputType | null {
	if (typeof value !== 'string') return null;
	const candidate = value.trim();
	return (KNOWN_INPUT_TYPES as string[]).includes(candidate) ? (candidate as HitlInputType) : null;
}

const KNOWN_HITL_SOURCES: ReadonlySet<HitlSource> = new Set([
	'agentic',
	'user_request',
	'approval',
	'plan_approval',
	'clarification',
	'escalation',
	'diff_approval',
	'bot_auth',
	'service_health'
]);

const LEGACY_HITL_SOURCE_ALIASES: Readonly<Record<string, HitlSource>> = {
	primitive: 'agentic',
	inner_loop: 'agentic'
};

/** Normalize only source identifiers documented by the canonical HITL contract. */
export function normalizeHitlSource(value: unknown): HitlSource | null {
	if (typeof value !== 'string') return null;
	const candidate = value.trim();
	if (!candidate) return null;
	if ((KNOWN_HITL_SOURCES as ReadonlySet<string>).has(candidate)) {
		return candidate as HitlSource;
	}
	return LEGACY_HITL_SOURCE_ALIASES[candidate] ?? null;
}

/** Shared source/input/schema compatibility policy for typed HITL targets. */
export function isHitlSourceInputCompatible(
	source: HitlSource,
	inputType: HitlInputType,
	schema: HitlInputSchema
): boolean {
	if (
		(inputType === 'choice' || inputType === 'multi_choice') &&
		!schema.options?.length
	) {
		return false;
	}
	if (source === 'approval' && inputType !== 'confirmation') return false;
	if (source === 'plan_approval' && inputType !== 'confirmation') return false;
	if ((source === 'bot_auth' || source === 'service_health') && inputType !== 'choice') return false;
	if ((source === 'diff_approval') !== (inputType === 'diff_approval')) return false;
	if (
		(inputType === 'tool_authorization' || inputType === 'sandbox_override') &&
		source !== 'agentic' &&
		source !== 'escalation'
	) {
		return false;
	}
	return true;
}

// ─── FeedItem → HitlRequest ────────────────────────────────────────────

/**
 * Build a canonical `HitlRequest` from a `FeedItem` carrying HITL
 * metadata. Returns `null` if the item isn't a HITL request (no
 * pause_state_id / approval_id, or no usable input_type).
 *
 * Sources covered:
 *   - `attention_kind = 'input.requested'` → `agentic` (pause_state_id)
 *   - `attention_kind = 'user_request.pending'` → `user_request`
 *   - `attention_kind = 'waiting_for_confirmation'` → `agentic` confirmation
 *   - `attention_kind = 'max_iterations_reached'` → `escalation`
 *   - `item_type = 'approval'` → `approval`
 */
export function hitlRequestFromFeedItem(item: FeedItem): HitlRequest | null {
	const metadata = asRecord(item.metadata);
	if (!metadata) return null;

	const attentionKind = readString(metadata, 'attention_kind');
	const isApproval = item.item_type === 'approval';
	if (!attentionKind && !isApproval) return null;

	const pauseStateId = readString(metadata, 'pause_state_id');
	const approvalId = readString(metadata, 'approval_id');
	const requestId = readString(metadata, 'request_id') ?? readString(metadata, 'correlation_id');

	// Prefer the explicit `metadata.source` when the backend stamps it.
	// The V3 attention projection now sets this for every canonical HITL
	// source — clarification, plan_approval, user_request, AND diff_approval
	// (a code-change "apply" card). Falls back to `resolveSource(attention_kind)`
	// only for legacy item shapes (`input.requested` / `waiting_for_confirmation`
	// / `max_iterations_reached` / `user_request.pending`) that pre-date the
	// canonical envelope and carry no `source`.
	//
	// Without this preference a diff_approval card — which the backend
	// surfaces as `attention_kind: "input.requested"` — falls through to
	// `'agentic'`, the POST lands in the `agentic` dispatcher arm instead of
	// the `diff_approval` apply/reject arm, the CodeChangeProposal is never
	// applied, the execution never advances, and the card reappears on the
	// next attention poll. (Same failure mode a V3 clarification hits when
	// rendered as `attention_kind: "hitl.requested"`.)
	const explicitSource = readString(metadata, 'source');
	const normalizedExplicitSource = normalizeHitlSource(explicitSource);
	if (explicitSource && !normalizedExplicitSource) return null;
	const source: HitlSource =
		isApproval
			? 'approval'
			: normalizedExplicitSource
				? normalizedExplicitSource
				: resolveSource(attentionKind, false);

	// Approvals carry a fixed input shape: confirmation with approve/reject
	// labels. We don't read input_schema from approval metadata.
	if (source === 'approval') {
		if (!approvalId) return null;
		return {
			id: approvalId,
			source: 'approval',
			input_type: 'confirmation',
			schema: {
				confirm_label: 'Approve',
				deny_label: 'Reject'
			},
			prompt: item.title,
			hint: item.summary ?? undefined,
			scope: scopeFromItem(item, metadata),
			identifiers: { approval_id: approvalId, correlation_id: approvalId },
			at: item.updated_at,
			raw: { ...metadata, item_type: item.item_type }
		};
	}

	const schemaRecord = asRecord(metadata['input_schema']);
	const inputType =
		normalizeHitlInputType(schemaRecord?.type) ?? normalizeHitlInputType(metadata['input_type']);

	// Without an input_type we can't build a render request. Surfaces fall
	// back to their legacy dispatch (escalation paths often don't carry
	// input_type — they're driven by escalation_type instead).
	if (!inputType) return null;

	const id = pauseStateId ?? requestId ?? `${item.id}-hitl`;
	const schema = buildSchemaFromMetadata(metadata, schemaRecord);
	if (!isHitlSourceInputCompatible(source, inputType, schema)) return null;

	return {
		id,
		source,
		input_type: inputType,
		schema,
		prompt: buildPromptText(item, metadata, schemaRecord),
		hint: readString(metadata, 'hint') ?? undefined,
		scope: scopeFromItem(item, metadata),
		identifiers: {
			pause_state_id: pauseStateId ?? undefined,
			correlation_id: requestId ?? undefined,
			request_id: requestId ?? undefined
		},
		at: item.updated_at,
		raw: { ...metadata, item_type: item.item_type, attention_kind: attentionKind ?? undefined }
	};
}

/**
 * Build a canonical `HitlRequest` from a raw `HitlRequested` event as
 * it lands on the bus. Used by surfaces that subscribe to the canonical
 * event stream directly (chat typing-bubble pill, pendingHitlStore
 * click-to-respond) rather than going through the FeedItem detour.
 *
 * Mirrors the shape of `RuntimeTransportEvent::HitlRequested` post-H7:
 * top-level `{event_type: "HitlRequested", data: {correlation_id, source,
 * input_type, prompt, hint, input_schema, scope: {execution_id, task_id,
 * agent_id, thread_id, chat_turn_id}, pause_state_id?, approval_id?}}`.
 *
 * Returns `null` when the event isn't a recognized HITL request or when
 * required fields are missing — caller falls back to whatever surface
 * it had before (typically a navigation route or a no-op).
 */
export function hitlRequestFromCanonicalEvent(raw: Record<string, unknown>): HitlRequest | null {
	const data = asRecord(raw['data']);
	const payload = data ?? raw;
	const eventType = readString(raw, 'event_type');
	if (eventType !== 'HitlRequested' && readString(payload, 'event_type') !== 'HitlRequested') {
		return null;
	}

	const correlationId = readString(payload, 'correlation_id');
	if (!correlationId) return null;

	const sourceValue = readString(payload, 'source');
	const source = normalizeHitlSource(sourceValue);
	if (!source) return null;
	const inputType = normalizeHitlInputType(payload['input_type']);
	if (!inputType) return null;

	if (
		payload['input_schema'] !== undefined &&
		payload['input_schema'] !== null &&
		!asRecord(payload['input_schema'])
	) {
		return null;
	}
	const schemaRecord = asRecord(payload['input_schema']);
	const schema = buildSchemaFromMetadata(payload, schemaRecord);
	if (!isHitlSourceInputCompatible(source, inputType, schema)) return null;

	const scopeRecord = asRecord(payload['scope']) ?? {};
	const taskId = readString(scopeRecord, 'task_id') ?? readString(payload, 'task_id') ?? undefined;
	const principal = readString(scopeRecord, 'principal') ?? readString(payload, 'principal') ?? undefined;
	const workspace = readString(scopeRecord, 'workspace') ?? readString(payload, 'workspace') ?? undefined;
	const scope: HitlRequest['scope'] = {
		principal,
		workspace,
		workflow_id:
			readString(scopeRecord, 'workflow_id') ??
			readString(payload, 'workflow_id') ??
			(source === 'clarification' || source === 'plan_approval' ? taskId : undefined),
		task_id: taskId,
		execution_id:
			readString(scopeRecord, 'execution_id') ?? readString(payload, 'execution_id') ?? undefined,
		agent_id: readString(scopeRecord, 'agent_id') ?? readString(payload, 'agent_id') ?? undefined,
		thread_id:
			readString(scopeRecord, 'thread_id') ??
			readString(scopeRecord, 'ui_thread_id') ??
			readString(payload, 'thread_id') ??
			undefined
	};
	if (!principal || !workspace) return null;
	if (
		(source === 'agentic' || source === 'escalation' || source === 'diff_approval') &&
		!scope.execution_id
	) {
		return null;
	}
	if (
		(source === 'clarification' || source === 'plan_approval') &&
		!scope.workflow_id &&
		!scope.task_id
	) {
		return null;
	}

	const rawPauseStateId = readString(payload, 'pause_state_id') ?? undefined;
	const rawApprovalId = readString(payload, 'approval_id') ?? undefined;
	const rawRequestId = readString(payload, 'request_id') ?? undefined;
	if (rawApprovalId && (source !== 'approval' || rawApprovalId !== correlationId)) return null;
	if (rawRequestId && (source !== 'user_request' || rawRequestId !== correlationId)) return null;
	if (rawPauseStateId) {
		const pauseMatches =
			source === 'diff_approval' ||
			((source === 'agentic' || source === 'escalation' || source === 'user_request') &&
				rawPauseStateId === correlationId);
		if (!pauseMatches) return null;
	}
	const identifiers: HitlIdentifiers = {
		correlation_id: correlationId,
		pause_state_id:
			rawPauseStateId ??
			(source === 'agentic' || source === 'escalation' ? correlationId : undefined),
		approval_id:
			rawApprovalId ?? (source === 'approval' ? correlationId : undefined),
		request_id:
			rawRequestId ?? (source === 'user_request' ? correlationId : undefined)
	};

	const prompt = readString(payload, 'prompt');
	if (!prompt) return null;
	const hint = readString(payload, 'hint') ?? undefined;
	if (source === 'diff_approval') {
		const proposalId = schema.proposal_id?.trim();
		const transactionId = schema.transaction_id?.trim();
		if (proposalId && transactionId && proposalId !== transactionId) return null;
		const diffId = proposalId || transactionId || correlationId;
		if (diffId !== correlationId) return null;
	}
	if (source === 'bot_auth') {
		const parts = correlationId.split(':');
		if (
			parts.length !== 4 ||
			parts[0] !== 'bot_auth' ||
			parts[1] !== principal ||
			parts[2] !== workspace ||
			!parts[3]?.trim()
		) {
			return null;
		}
	}

	return {
		id: correlationId,
		source,
		input_type: inputType,
		schema,
		prompt,
		hint,
		scope,
		identifiers,
		at: readNumber(payload, 'timestamp_ms') || readNumber(raw, 'timestamp_ms') || Date.now(),
		raw: payload as Record<string, unknown>
	};
}

function resolveSource(attentionKind: string | null, isApproval: boolean): HitlSource {
	if (isApproval) return 'approval';
	switch (attentionKind) {
		case 'user_request.pending':
			return 'user_request';
		case 'max_iterations_reached':
			return 'escalation';
		// Self-identifying kinds — kept as a back-compat safety net for cards
		// that name their source in `attention_kind` but pre-date the explicit
		// `metadata.source` field. `hitlRequestFromFeedItem` prefers
		// `metadata.source` when present, so these only fire for legacy shapes.
		case 'diff_approval':
			return 'diff_approval';
		case 'clarification':
			return 'clarification';
		case 'plan_approval':
			return 'plan_approval';
		case 'input.requested':
		case 'waiting_for_confirmation':
		default:
			return 'agentic';
	}
}

function scopeFromItem(
	item: FeedItem,
	metadata: Record<string, unknown> | null
): HitlRequest['scope'] {
	return {
		principal: item.principal,
		workspace: item.workspace,
		workflow_id:
			readString(metadata, 'workflow_id') ??
			(['clarification', 'plan_approval'].includes(readString(metadata, 'source') ?? '')
				? item.task_id ?? readString(metadata, 'task_id') ?? undefined
				: undefined),
		task_id: item.task_id ?? readString(metadata, 'task_id') ?? undefined,
		execution_id: readString(metadata, 'execution_id') ?? undefined,
		agent_id: item.agent_id ?? readString(metadata, 'agent_id') ?? undefined,
		thread_id: item.ui_thread_id ?? readString(metadata, 'thread_id') ?? undefined
	};
}

function buildSchemaFromMetadata(
	metadata: Record<string, unknown>,
	schemaRecord: Record<string, unknown> | null
): HitlInputSchema {
	const schema: HitlInputSchema = {};
	if (schemaRecord) {
		schema.request_type = readString(schemaRecord, 'request_type') ?? undefined;
		if (typeof schemaRecord.multiline === 'boolean') schema.multiline = schemaRecord.multiline;
		const placeholder = readString(schemaRecord, 'placeholder');
		if (placeholder) schema.placeholder = placeholder;
		if (typeof schemaRecord.max_length === 'number') schema.max_length = schemaRecord.max_length;
		if (typeof schemaRecord.allow_other === 'boolean') schema.allow_other = schemaRecord.allow_other;
		if (typeof schemaRecord.min_selections === 'number') {
			schema.min_selections = schemaRecord.min_selections;
		}
		if (typeof schemaRecord.max_selections === 'number') {
			schema.max_selections = schemaRecord.max_selections;
		}
		if (typeof schemaRecord.multiple === 'boolean') schema.multiple = schemaRecord.multiple;
		const filter = readString(schemaRecord, 'filter');
		if (filter) schema.filter = filter;
		const instructions = readString(schemaRecord, 'instructions');
		if (instructions) schema.instructions = instructions;
		const doneLabel = readString(schemaRecord, 'done_label');
		if (doneLabel) schema.done_label = doneLabel;
		const confirmLabel = readString(schemaRecord, 'confirm_label');
		if (confirmLabel) schema.confirm_label = confirmLabel;
		const denyLabel = readString(schemaRecord, 'deny_label');
		if (denyLabel) schema.deny_label = denyLabel;
		if (typeof schemaRecord.destructive === 'boolean') schema.destructive = schemaRecord.destructive;
		// The two authorization shapes carry the thing being authorized as data
		// rather than only inside the composed prompt sentence. Read here so the
		// prompt surface can show it verbatim; see `HitlInputSchema`.
		const toolName = readString(schemaRecord, 'tool_name');
		if (toolName) schema.tool_name = toolName;
		const paramsSummary = readString(schemaRecord, 'params_summary');
		if (paramsSummary) schema.params_summary = paramsSummary;
		const command = readString(schemaRecord, 'command');
		if (command) schema.command = command;
		const violation = readString(schemaRecord, 'violation');
		if (violation) schema.violation = violation;
		const allowedRoots = readStrings(schemaRecord, 'allowed_roots');
		if (allowedRoots.length > 0) schema.allowed_roots = allowedRoots;
		const context = readString(schemaRecord, 'context');
		if (context) schema.context = context;
		const suggestions = readStrings(schemaRecord, 'suggestions');
		if (suggestions.length > 0) schema.suggestions = suggestions;
		const optionsFromSchema = readChoiceOptions(schemaRecord.options);
		if (optionsFromSchema.length > 0) schema.options = optionsFromSchema;
		// Phase H4 — clarification ride-along fields.
		const sourceSlotId = readString(schemaRecord, 'source_slot_id');
		if (sourceSlotId) schema.source_slot_id = sourceSlotId;
		const stage = readString(schemaRecord, 'stage');
		if (stage) {
			const allowed = [
				'planning_bootstrap',
				'planning_iteration',
				'execution_cycle',
				'follow_up',
				'unknown'
			] as const;
			if ((allowed as readonly string[]).includes(stage)) {
				schema.stage = stage as (typeof allowed)[number];
			}
		}
		// Multi-stage MVP — chain identity when this question is one
		// of N in a batched clarification. The modal renders
		// "STEP X OF N" when all three fields are present.
		const chainId = readString(schemaRecord, 'chain_id');
		if (chainId) {
			schema.chain_id = chainId;
			const position = readNumber(schemaRecord, 'chain_position', 0);
			const total = readNumber(schemaRecord, 'chain_total', 0);
			if (position > 0) schema.chain_position = position;
			if (total > 0) schema.chain_total = total;
		}
		const formQuestions = readFormQuestions(schemaRecord.questions);
		if (formQuestions.length > 0) schema.questions = formQuestions;
		const sensitive = readSensitiveSpec(schemaRecord.sensitive);
		if (sensitive) schema.sensitive = sensitive;
	}
	const transactionId =
		readString(schemaRecord, 'transaction_id') ?? readString(metadata, 'transaction_id');
	if (transactionId) schema.transaction_id = transactionId;
	const proposalId =
		readString(schemaRecord, 'proposal_id') ?? readString(metadata, 'proposal_id');
	if (proposalId) schema.proposal_id = proposalId;
	const approvalSource =
		readString(schemaRecord, 'approval_source') ?? readString(metadata, 'approval_source');
	if (approvalSource) schema.approval_source = approvalSource;
	const rationale =
		readString(schemaRecord, 'rationale') ?? readString(metadata, 'rationale');
	if (rationale) schema.rationale = rationale;
	const files = readDiffApprovalFiles(schemaRecord?.files ?? metadata['files']);
	if (files.length > 0) schema.files = files;
	if (!schema.options) {
		const optionsFromMetadata = readChoiceOptions(metadata['options']);
		if (optionsFromMetadata.length > 0) schema.options = optionsFromMetadata;
	}
	return schema;
}

function buildPromptText(
	item: FeedItem,
	metadata: Record<string, unknown>,
	schemaRecord: Record<string, unknown> | null
): string {
	const questions = readStrings(metadata, 'questions');
	const primary = questions[0] ?? (metadata.source === 'service_health' ? item.title : item.summary ?? item.title);
	const context = readString(schemaRecord, 'context');
	const suggestions = readStrings(schemaRecord, 'suggestions');
	const parts = [
		primary,
		context ? `Context: ${context}` : null,
		suggestions.length > 0 ? `Suggestions:\n- ${suggestions.join('\n- ')}` : null
	].filter((part): part is string => !!part && part.trim().length > 0);
	return parts.join('\n\n');
}

// ─── Endpoint resolver ────────────────────────────────────────────────

/**
 * Pick the canonical (Phase H3) resolve URL + body shape for a request.
 *
 * Phase H3 (magician v0.6.475) added `POST /api/magician/v2/hitl/{id}/respond`
 * that dispatches internally based on `source`. This function builds
 * the call against that single endpoint instead of routing per-source
 * URLs.
 *
 * Retired per-source mutation URLs are not used by frontend surfaces.
 */
export interface HitlResolveCall {
	url: string;
	method: 'POST';
	body: Record<string, unknown>;
	/**
	 * 404 / 409 / 410 mean "request already resolved by someone else" — UIs
	 * should treat these as soft success (drop the local pending state)
	 * rather than failures.
	 */
	soft_success_statuses: number[];
}

export interface HitlResolveOptions {
	selectedPaths?: string[];
}

/**
 * Pick the canonical id used as the URL path component. For agentic
 * pauses the canonical id is `pause_state_id`; for user_request it's
 * the request id (also stored as `pause_state_id` in the FeedItem
 * adapter); for approvals it's `approval_id`.
 */
function correlationIdForRequest(request: HitlRequest): string | null {
	if (request.input_type === 'diff_approval' || request.source === 'diff_approval') {
		return (
			request.schema.proposal_id ??
			request.schema.transaction_id ??
			request.identifiers.correlation_id ??
			request.id ??
			null
		);
	}
	const ids = request.identifiers;
	switch (request.source) {
		case 'approval':
			return ids.approval_id ?? null;
		case 'user_request':
			return ids.request_id ?? null;
		case 'agentic':
		case 'escalation':
			return ids.pause_state_id ?? null;
		case 'clarification':
		case 'plan_approval':
		case 'bot_auth':
		case 'service_health':
			return ids.correlation_id ?? null;
		default:
			return null;
	}
}

/**
 * Build the canonical Phase H3 resolve call. Single endpoint, single
 * body shape — backend dispatches by `source`.
 *
 * Planning dispatch carries the task/workflow responder as `task_id` while
 * preserving the durable runtime identity as `execution_id`. Legacy targets
 * may use the same value for both fields.
 */
export function resolveHitlCall(
	request: HitlRequest,
	value: HitlResponseValue,
	options: HitlResolveOptions = {}
): HitlResolveCall | null {
	const correlationId = correlationIdForRequest(request);
	if (!correlationId) return null;
	const body: Record<string, unknown> = {
		source: request.source,
		input_type: request.input_type,
		value,
		channel: 'web'
	};
	const selectedPaths = (options.selectedPaths ?? [])
		.map((path) => path.trim())
		.filter((path) => path.length > 0);
	if (selectedPaths.length > 0) {
		body.selected_paths = selectedPaths;
	}
	const responseTaskId = request.scope.task_id ?? request.scope.workflow_id;
	if (
		responseTaskId &&
		(request.source === 'clarification' || request.source === 'plan_approval')
	) {
		body.task_id = responseTaskId;
	}
	if (request.scope.execution_id) {
		body.execution_id = request.scope.execution_id;
	}
	return {
		url: `/api/magician/v2/hitl/${encodeURIComponent(correlationId)}/respond`,
		method: 'POST',
		body,
		// A missing agentic/escalation pause can mean the runtime already
		// drained it. Older backends returned 404; current backends return
		// 410 `pause_state_gone`. Both are soft successes only for those
		// sources.
		// Clarifications use 409 `already_resolved`; treating their 404 as
		// success would hide a routing/task mismatch while leaving planning
		// blocked.
		soft_success_statuses:
			request.source === 'agentic' || request.source === 'escalation'
				? [404, 409, 410]
				: [409]
	};
}

// ─── Resolve outcome helpers ──────────────────────────────────────────

/**
 * POST a canonical response to the right legacy endpoint. Returns a
 * structured outcome the caller can render appropriately.
 */
export async function postHitlResponse(
	request: HitlRequest,
	value: HitlResponseValue,
	headers: HeadersInit = {},
	options: HitlResolveOptions = {}
): Promise<HitlResolveOutcome> {
	const call = resolveHitlCall(request, value, options);
	if (!call) {
		return {
			ok: false,
			status: 0,
			message: 'No resolver available for this request shape.'
		};
	}
	try {
		const response = await timedFetch(call.url, {
			method: call.method,
			headers: {
				'Content-Type': 'application/json',
				...headers
			},
			body: JSON.stringify(call.body)
		});
		const text = await response.text().catch(() => '');
		let payload: Record<string, unknown> | null = null;
		if (text) {
			try {
				const parsed = JSON.parse(text) as unknown;
				if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
					payload = parsed as Record<string, unknown>;
				}
			} catch {
				// Non-JSON error bodies are reported verbatim below.
			}
		}
		if (response.ok) {
			// #30 — validation-reject / reask responses come back HTTP 200
			// with `{resumed:false, status:"reask_required", question, hint,
			// previous_answer, ...}`. Surface these as a distinct `reask`
			// outcome so the caller can re-open the input modal with the
			// clarified question instead of rendering a terminal error.
			if (payload?.status === 'reask_required') {
				return {
					ok: false,
					reask: true,
					status: response.status,
					message: String(payload.message ?? payload.reason ?? 'Please revise your answer.'),
					question:
						typeof payload.question === 'string' ? payload.question : undefined,
					hint: typeof payload.hint === 'string' ? payload.hint : undefined,
					previousAnswer:
						typeof payload.previous_answer === 'string' ? payload.previous_answer : undefined
				};
			}
			if (payload?.accepted === false || payload?.resumed === false) {
				return {
					ok: false,
					status: response.status,
					message: String(payload.reason ?? payload.message ?? 'Response was not accepted')
				};
			}
			return { ok: true };
		}
		// #5/#6 — an already-resolved HITL (answered elsewhere, double-submit,
		// post-restart id-drift, or a drained pause) is not a failure: the
		// backend already broadcasts the canonical `HitlResolved`, so the card
		// drops correctly and we should NOT show a spurious submit error.
		//   - Older agentic/preplan resume paths returned HTTP 404. Current
		//     backends return HTTP 410 `pause_state_gone` after also finalizing
		//     any orphaned waiting state. Both mean this card is no longer
		//     actionable and must disappear locally.
		//   - The clarification path returns HTTP 409 `{reason:"already_resolved"}`
		//     — accept it on the reason match.
		if (call.soft_success_statuses.includes(response.status)) {
			if (
				(response.status === 404 || response.status === 410)
				&& (request.source === 'agentic' || request.source === 'escalation')
			) {
				return { ok: true };
			}
			if (payload?.reason === 'already_resolved') {
				return { ok: true };
			}
		}
		return {
			ok: false,
			status: response.status,
			message: text || `${response.status} ${response.statusText}`
		};
	} catch (err) {
		return {
			ok: false,
			status: 0,
			message: err instanceof Error ? err.message : String(err)
		};
	}
}
