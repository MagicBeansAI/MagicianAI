/**
 * Promise-based "respond to a HITL request" helper.
 *
 * Reuses `attentionPromptStore` for the actual input UI (so all the
 * keyboard ergonomics, themed chrome, choice-list rendering land for
 * free) and `adapters.postHitlResponse` for the source-aware POST.
 *
 * Surfaces call this once per HITL item:
 *
 *   const outcome = await respondToHitl(request, scopedHeaders);
 *   if (outcome.ok) { ... } else if (!outcome.cancelled) showError(...)
 *
 * Phase H1 keeps the legacy resolve URLs; Phase H2 swaps the body of
 * `postHitlResponse` to hit the canonical `/api/.../hitl/{id}/respond`
 * endpoint. The surface code stays unchanged across that switch.
 */
import {
	requestAttentionInput,
	type AttentionPromptKind,
	type AttentionPromptRequest,
	type AttentionPromptResult
} from '$lib/stores/attentionPromptStore';
import { postHitlResponse } from './adapters';
import type {
	HitlInputType,
	HitlRequest,
	HitlResolveOutcome,
	HitlResponseValue
} from './types';

export interface RespondToHitlOptions {
	/** Initial value for free-form response inputs, such as a validation reask. */
	defaultValue?: string;
}

/**
 * Drive a HITL request from prompt to resolved response. Returns the
 * outcome of the POST (or a `cancelled: true` outcome if the operator
 * dismissed the modal without responding).
 */
export async function respondToHitl(
	request: HitlRequest,
	headers: HeadersInit = {},
	options: RespondToHitlOptions = {}
): Promise<HitlResolveOutcome> {
	const result = await requestAttentionInput(promptFor(request, options));
	if (!result) {
		// Dismissing a secret ask is an answer: the pending operation must
		// cancel (and its custody retire) rather than wait out its window.
		// The backend's spec decides, with the legacy secure browser request
		// types kept for entries announced before the spec existed.
		if (isSensitiveRequest(request)) {
			return await postHitlResponse(request, { type: 'aborted' }, headers);
		}
		return { ok: false, cancelled: true };
	}
	const value = mapPromptResultToHitlValue(request, result);
	if (!value) {
		return { ok: false, cancelled: true };
	}

	const selectedPaths = result.kind === 'choice' ? result.selectedPaths : undefined;
	return await postHitlResponse(request, value, headers, { selectedPaths });
}

// ─── Mapping helpers ──────────────────────────────────────────────────

/**
 * Whether the ask collects a secret: the backend's value-free spec, or the
 * two built-in secure browser request types that predate it.
 */
export function isSensitiveRequest(request: HitlRequest): boolean {
	if (request.schema.sensitive) return true;
	return (
		request.source === 'user_request' &&
		['secure_browser_input', 'secure_browser_confirm'].includes(request.schema.request_type ?? '')
	);
}

/**
 * How a single-value ask renders once the spec is applied: the typed widget
 * wins, then a `text`/`guidance` ask the backend classified as a secret is
 * masked by its kind — a code as a one-time-code field, anything else as a
 * password field. An identifier stays readable; only its handling changes.
 */
function sensitiveKindFor(request: HitlRequest): AttentionPromptKind | null {
	const kind = request.schema.sensitive?.kind;
	if (!kind) return null;
	if (request.input_type !== 'text' && request.input_type !== 'guidance') return null;
	if (kind === 'otp') return 'otp';
	if (kind === 'password' || kind === 'other') return 'password';
	return null;
}

function promptDefaultValue(
	request: HitlRequest,
	options: RespondToHitlOptions
): string | undefined {
	// A secret is never pre-filled — not from a re-ask's previous answer, not
	// from a draft. The field starts empty every time it is shown.
	if (isSensitiveRequest(request)) return undefined;
	switch (request.input_type) {
		case 'text':
		case 'guidance':
		case 'file_path':
		case 'external_action':
			return options.defaultValue;
		default:
			return undefined;
	}
}

/**
 * The shape each input type is rendered as — **the one place the two
 * vocabularies meet**.
 *
 * A `Record<HitlInputType, AttentionPromptKind>` rather than the `switch` this
 * replaces, and that is the point: a twelfth input type is now a build failure
 * here instead of a `switch` falling out of its last case with `undefined` and a
 * surface rendering a blank field for an ask nobody can answer.
 *
 * **Five of these rows used to be coercions and are now translations.**
 * `confirmation`, `tool_authorization` and `sandbox_override` were all mapped to
 * `choice` — a radio group plus a Submit, indistinguishable at a glance from
 * "which quarter?", with a sandbox escape reading exactly like a preference.
 * `external_action` was mapped to `multiline`, which dropped `instructions`, the
 * one field that says what the reader has to go and do. `file_path` was mapped
 * to `text`, which dropped `multiple` and `filter`, so a prompt wanting three
 * CSVs looked like one wanting a config file. Each now has a shape of its own in
 * `HitlPromptFields`.
 *
 * `text` is the only row that reads the schema, because `multiline` is a
 * rendering fact the wire states as a flag rather than as a type.
 */
const PROMPT_KIND: Record<HitlInputType, AttentionPromptKind> = {
	text: 'text',
	password: 'password',
	otp: 'otp',
	guidance: 'guidance',
	choice: 'choice',
	multi_choice: 'multi_choice',
	confirmation: 'confirmation',
	external_action: 'external_action',
	file_path: 'file_path',
	tool_authorization: 'authorization',
	sandbox_override: 'authorization',
	diff_approval: 'diff_approval',
	form: 'form'
};

function mapToPromptKind(request: HitlRequest): AttentionPromptKind {
	const masked = sensitiveKindFor(request);
	if (masked) return masked;
	if (request.input_type === 'text' && request.schema.multiline) return 'multiline';
	return PROMPT_KIND[request.input_type];
}

/**
 * The whole prompt for one request.
 *
 * Exported because a second surface builds the same prompt from the same
 * request and renders it in place rather than in a modal — see
 * `magician/tasks/taskAsk.ts`. A second construction of this would be a second
 * answer to what an ask looks like, and the two would diverge the first time a
 * schema field was added to either.
 */
export function promptFor(
	request: HitlRequest,
	options: RespondToHitlOptions = {}
): Omit<AttentionPromptRequest, 'id'> {
	const kind = mapToPromptKind(request);
	const isAuthorization =
		request.input_type === 'tool_authorization' || request.input_type === 'sandbox_override';
	const schemaChoices = request.schema.options?.map((option) => ({
		id: option.id,
		label: option.label,
		description: option.description,
		requiresInput: option.requires_input
	}));

	return {
		title: titleFor(request),
		body: request.prompt,
		// Carried for the first time. On a re-ask this is the reason the question
		// came back — a validation message, the previous answer, the retry count —
		// and every surface used to drop it, so a re-ask arrived looking identical
		// to the question it was re-asking.
		hint: request.hint,
		kind,
		placeholder: request.schema.placeholder ?? defaultPlaceholder(kind),
		defaultValue: promptDefaultValue(request, options),
		choices: kind === 'choice' || kind === 'multi_choice' ? schemaChoices : undefined,
		minSelections:
			request.input_type === 'multi_choice' ? request.schema.min_selections : undefined,
		maxSelections:
			request.input_type === 'multi_choice' ? request.schema.max_selections : undefined,
		// Multi-stage MVP — forward chain identity onto the prompt so
		// the modal can render "STEP X OF N" for batched clarifications.
		chainId: request.schema.chain_id,
		chainPosition: request.schema.chain_position,
		chainTotal: request.schema.chain_total,
		confirmation:
			request.input_type === 'confirmation'
				? {
						confirmLabel: request.schema.confirm_label ?? 'Confirm',
						denyLabel: request.schema.deny_label ?? 'Deny',
						destructive: request.schema.destructive === true
					}
				: undefined,
		externalAction:
			request.input_type === 'external_action'
				? {
						instructions: request.schema.instructions ?? null,
						doneLabel: request.schema.done_label ?? "I've completed this"
					}
				: undefined,
		filePath:
			request.input_type === 'file_path'
				? {
						multiple: request.schema.multiple === true,
						filter: request.schema.filter ?? null
					}
				: undefined,
		authorization: isAuthorization
			? {
					grant: request.input_type === 'sandbox_override' ? 'sandbox' : 'tool',
					// The thing being authorized, verbatim off the schema. The prompt
					// sentence is composed around these; a grant made against the
					// sentence is a grant made against something never shown.
					subject:
						(request.input_type === 'sandbox_override'
							? request.schema.command
							: request.schema.tool_name) ?? '',
					detail:
						(request.input_type === 'sandbox_override'
							? request.schema.violation
							: request.schema.params_summary) ?? null,
					roots: request.schema.allowed_roots ?? [],
					options: schemaChoices ?? [],
					// The id the runtime's resume dispatcher matches on, and the id
					// `UserInputType::options_json` emits for both grants.
					denyId: schemaChoices?.some((option) => option.id === 'deny') ? 'deny' : null
				}
			: undefined,
		diffApproval:
			request.input_type === 'diff_approval'
				? {
						transactionId:
							request.schema.proposal_id ?? request.schema.transaction_id ?? request.id,
						rationale: request.schema.rationale,
						files: request.schema.files ?? []
					}
				: undefined,
		sensitive: request.schema.sensitive
			? {
					kind: request.schema.sensitive.kind,
					oneTime: request.schema.sensitive.one_time === true,
					deadlineMs: request.schema.sensitive.collection_deadline_ms,
					// The ask the retrieval status is read for (secure HITL P6).
					correlationId: request.identifiers.correlation_id ?? request.id
				}
			: undefined,
		formQuestions:
			request.input_type === 'form'
				? (request.schema.questions ?? []).map((question) => ({
						id: question.id,
						prompt: question.prompt,
						inputType: question.input_type ?? 'text',
						// The backend's per-field classification wins; a typed
						// `password`/`otp` question masks even when the spec was
						// published without it (an entry announced before P3).
						sensitive:
							request.schema.sensitive?.fields?.find((field) => field.id === question.id)?.kind ??
							(question.input_type === 'password' || question.input_type === 'otp'
								? question.input_type
								: undefined),
						options: question.options?.map((option) => ({
							id: option.id,
							label: option.label,
							description: option.description
						}))
					}))
				: undefined
	};
}

function defaultPlaceholder(kind: AttentionPromptKind): string {
	switch (kind) {
		case 'multiline':
		case 'guidance':
			return 'Type your response… (⌘/Ctrl+Enter to submit)';
		case 'password':
			return 'Enter your password…';
		case 'otp':
			return 'Enter the code…';
		case 'choice':
			return 'Pick an option';
		case 'diff_approval':
			return 'Review the staged diff';
		case 'text':
		default:
			return 'Type your response…';
	}
}

function titleFor(request: HitlRequest): string {
	switch (request.input_type) {
		case 'choice':
		case 'multi_choice':
			return 'Choose an option';
		case 'confirmation':
			return 'Confirm action';
		// Not `Confirm action`: these two grant a capability rather than let a
		// step continue, and the title is the first thing that says so.
		case 'tool_authorization':
			return 'Authorize a tool';
		case 'sandbox_override':
			return 'Authorize a sandbox override';
		case 'diff_approval':
			return 'Review file changes';
		case 'external_action':
			return 'External action';
		case 'file_path':
			return request.schema.multiple ? 'Provide file paths' : 'Provide a file path';
		case 'guidance':
			return 'Provide guidance';
		case 'password':
			return 'Password required';
		case 'otp':
			return 'Verification code required';
		case 'form':
			return 'Answer these questions';
		case 'text':
		default:
			return 'Provide input';
	}
}

/**
 * One prompt result, as the response value its input type posts.
 *
 * Exported because a second surface collects the same result from the same
 * renderer and posts it without opening a modal — see `magician/tasks/taskAsk.ts`.
 * A second mapping would be a second answer to what a click on `Allow for This
 * Run` means on the wire.
 */
export function mapPromptResultToHitlValue(
	request: HitlRequest,
	result: AttentionPromptResult | null
): HitlResponseValue | null {
	if (!result) return null;

	switch (request.input_type) {
		case 'text': {
			// A masked rendering of a text-typed ask (classified by the spec)
			// still posts the type the pause expects; the backend routes it to
			// custody by the spec, not by the value's type.
			if (
				result.kind !== 'text' &&
				result.kind !== 'multiline' &&
				result.kind !== 'password' &&
				result.kind !== 'otp'
			) {
				return null;
			}
			return { type: 'text', value: result.value };
		}
		case 'password': {
			if (result.kind !== 'password') return null;
			return { type: 'password', value: result.value };
		}
		case 'otp': {
			// The wire value of a one-time code rides the password shape (exact
			// string, masked everywhere); the ask's type is what says "code".
			if (result.kind !== 'otp') return null;
			return { type: 'password', value: result.value };
		}
		case 'guidance': {
			if (
				result.kind !== 'guidance' &&
				result.kind !== 'text' &&
				result.kind !== 'multiline' &&
				result.kind !== 'password' &&
				result.kind !== 'otp'
			) {
				return null;
			}
			return { type: 'guidance', advice: (result as { value: string }).value };
		}
		case 'choice':
		case 'tool_authorization':
		case 'sandbox_override': {
			if (result.kind !== 'choice') return null;
			return {
				type: 'choice',
				selected_id: result.choiceId,
				other_value: result.input
			};
		}
		case 'diff_approval': {
			if (result.kind !== 'choice') return null;
			return {
				type: 'choice',
				selected_id: result.choiceId,
				other_value: result.input
			};
		}
		case 'form': {
			if (result.kind !== 'form') return null;
			return { type: 'form', answers: result.answers };
		}
		case 'multi_choice': {
			if (result.kind === 'multi_choice') {
				return { type: 'multi_choice', selected_ids: result.choiceIds };
			}
			// Defensive: the modal may still emit a single-choice result if
			// a future caller wires `multi_choice` through the radio surface
			// directly. Promote to a one-element multi-select rather than
			// dropping the response.
			if (result.kind === 'choice') {
				return { type: 'multi_choice', selected_ids: [result.choiceId] };
			}
			return null;
		}
		case 'confirmation': {
			if (result.kind !== 'choice') return null;
			const confirmed = result.choiceId === 'confirm' || result.choiceId === 'approve' || result.choiceId === 'yes';
			return { type: 'confirmation', confirmed };
		}
		case 'external_action': {
			if (result.kind === 'choice') {
				return {
					type: 'external_action_completed',
					guidance: result.input
				};
			}
			if (result.kind === 'text' || result.kind === 'multiline' || result.kind === 'guidance') {
				const value = (result as { value: string }).value;
				return {
					type: 'external_action_completed',
					guidance: value.trim().length > 0 ? value : undefined
				};
			}
			return null;
		}
		case 'file_path': {
			if (result.kind !== 'text' && result.kind !== 'multiline') return null;
			const paths = result.value
				.split(',')
				.map((entry) => entry.trim())
				.filter((entry) => entry.length > 0);
			return { type: 'file_path', paths };
		}
	}
}
