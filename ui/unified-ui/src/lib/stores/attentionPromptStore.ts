/**
 * Promise-based prompt service for HITL attention requests.
 *
 * Replaces `window.prompt()` in the surfaces that respond to attention items —
 * originally `ExecutionPanel.svelte`, which has since been retired in favour of
 * the unified task panel. A single `<AttentionPromptModal />`
 * mounted at the app root subscribes to this store, renders the request,
 * and resolves the awaiting promise on submit / cancel.
 *
 * See `docs/archive/plans/2026-05-10-execution-panel-canvas-redesign.md` Phase 3
 * (themed input modal) and the Deferred — HITL standardization section.
 */
import { writable, type Readable } from 'svelte/store';

/**
 * The shapes a prompt can be *rendered* as.
 *
 * **Deliberately not `HitlInputType`, and the two must not be collapsed.** This
 * vocabulary is about a form, not about a pause: `multiline` exists because
 * callers that are not HITL at all — editing a task's description, on `/tasks`
 * and on `/t/<thread>/tasks` — need a textarea and have no `input_type` to give,
 * and `HitlInputType` mirrors a Rust enum (`UserInputType`) that has no
 * `multiline` member and should not grow one to suit a frontend dialog. What
 * *was* a defect is that this list was shorter than the one it has to serve:
 * five of the eleven input types had no shape of their own and were coerced onto
 * another's, which is why a sandbox escape used to render as a radio group and
 * an `external_action`'s instructions were dropped on the floor. The mapping now
 * lives in one `Record<HitlInputType, AttentionPromptKind>` in
 * `respondToHitl.ts`, so a twelfth input type is a compile error there rather
 * than a blank field here.
 */
export type AttentionPromptKind =
	| 'text'
	| 'multiline'
	| 'password'
	| 'otp'
	| 'guidance'
	| 'choice'
	| 'multi_choice'
	| 'confirmation'
	| 'external_action'
	| 'file_path'
	| 'authorization'
	| 'diff_approval'
	| 'form';

export interface AttentionPromptChoice {
	id: string;
	label: string;
	description?: string;
	requiresInput?: boolean;
}

export interface AttentionPromptDiffFile {
	path: string;
	status: string;
	additions: number;
	deletions: number;
	unified_diff: string;
}

export interface AttentionPromptDiffApproval {
	transactionId: string;
	rationale?: string;
	files: AttentionPromptDiffFile[];
}

/**
 * `confirmation` — a yes/no decision with the backend's own words on both
 * buttons.
 *
 * It renders its own pair of controls rather than a two-option radio group with
 * a Submit under it, which is what it used to become. That was one extra click
 * on the most common decision in the system, and — worse — it made a
 * confirmation indistinguishable at a glance from "which quarter?".
 *
 * `destructive` is the backend's flag and bands the decision; it never changes
 * what is posted, because a confirmed destructive action and a confirmed benign
 * one are the same response value.
 */
export interface AttentionPromptConfirmation {
	confirmLabel: string;
	denyLabel: string;
	destructive: boolean;
}

/**
 * `external_action` — go and do something outside the system, then say you did.
 *
 * `instructions` is the whole point of the type and was **never rendered
 * anywhere**: the schema field existed, no surface read it, and the reader got a
 * bare textarea under a prompt that assumed they already knew what to do. The
 * note is optional — the acknowledgement is the answer, and the note is
 * whatever the reader wants to add about it.
 */
export interface AttentionPromptExternalAction {
	instructions: string | null;
	doneLabel: string;
}

/**
 * `file_path` — one path, or several when the ask allows it.
 *
 * `multiple` and `filter` were both on the schema and neither reached the
 * reader, so a prompt that wanted three CSVs looked exactly like one that wanted
 * a single config file. The separator is stated in the field rather than assumed,
 * because the response mapping splits on commas and a reader who separated with
 * spaces would have posted one nonsense path.
 */
export interface AttentionPromptFilePath {
	multiple: boolean;
	filter: string | null;
}

/**
 * `tool_authorization` / `sandbox_override` — a grant of permission.
 *
 * **One kind for two input types**, because the decision is the same one and
 * only the nouns differ: a tool the agent is not listed for, or a command that
 * violated sandbox policy. `grant` carries which, so the block can name it
 * without a second renderer that would drift from this one.
 *
 * `subject` and `detail` are shown **verbatim** and never paraphrased. The
 * prompt sentence is composed by the backend *around* these values; a grant made
 * against the sentence is a grant made against something the reader was not
 * shown. `roots` is empty for shell overrides, and an empty list renders no line
 * rather than "no roots".
 */
export interface AttentionPromptAuthorization {
	grant: 'tool' | 'sandbox';
	subject: string;
	detail: string | null;
	roots: string[];
	/**
	 * The grants on offer, in the order the backend listed them.
	 *
	 * **Not two buttons.** `UserInputType::options_json` offers a tool
	 * authorization *three* ids — `allow_once`, `allow_always`, `deny` — and
	 * `allow_always` writes the tool into the session allowlist, which is a
	 * strictly broader grant than the one-off. A two-button control would have
	 * silently dropped it: a control that cannot express the ask, offered as
	 * though it could.
	 */
	options: AttentionPromptChoice[];
	/**
	 * Which of `options` refuses, or `null` when none does.
	 *
	 * `deny` is the id the runtime's own resume dispatcher matches on and the id
	 * `options_json` emits for both grants, so it is a contract rather than a
	 * guess. It is singled out because the refusal is the one control that must
	 * not read like the others.
	 */
	denyId: string | null;
}

/**
 * Whether this shape supplies the controls that **answer** it, or leaves that to
 * whatever chrome is around it.
 *
 * The four that answer themselves are the four whose answer *is* a choice
 * between named actions — apply/reject, confirm/deny, allow/deny/allow-always,
 * done — so a generic `Submit` beside them would be a second control for one
 * decision, and the reader would have to guess which of the two ends the prompt.
 * The rest are fields: something is typed or picked and then submitted, and the
 * submit belongs to the surface, because a modal puts it in a footer and a task
 * panel puts it under the field.
 *
 * **This is also what stops Enter granting a capability**, and it is the *only*
 * thing that does. `authorization` and `diff_approval` both grant something —
 * permission to run an unlisted tool or to leave the sandbox, and permission to
 * write files — and both can arrive back to back in a queue the reader is
 * clearing. Every submit path, the modal's footer button and the Enter chord
 * alike, gates on `canSubmit`, which `HitlPromptFields` holds permanently
 * `false` for the shapes on this list; so a reader holding Enter through that
 * queue cannot grant a sandbox escape without having aimed at a named control.
 * A second `promptSubmitsOnEnter` map used to say this too, and was deleted for
 * being unfalsifiable: mutating it changed nothing, because this map had already
 * prevented what it claimed to prevent. Two guards for one property is one
 * property nothing pins.
 *
 * A `Record<AttentionPromptKind, boolean>`, so a twelfth shape has to be
 * classified rather than defaulting into whichever answer happens to be `false`.
 */
export const PROMPT_HAS_OWN_ACTIONS: Record<AttentionPromptKind, boolean> = {
	text: false,
	multiline: false,
	password: false,
	otp: false,
	guidance: false,
	choice: false,
	multi_choice: false,
	file_path: false,
	confirmation: true,
	external_action: true,
	authorization: true,
	diff_approval: true,
	form: false
};


export interface AttentionPromptRequest {
	id: number;
	title: string;
	body?: string;
	/**
	 * The ask's own supporting text — a retry reason, a previous answer, the
	 * context snippets the backend attached. Carried on `HitlRequest.hint` and
	 * **dropped by every surface until now**, so a re-ask that explained itself
	 * arrived looking identical to the question it was re-asking.
	 */
	hint?: string;
	kind: AttentionPromptKind;
	placeholder?: string;
	choices?: AttentionPromptChoice[];
	confirmLabel?: string;
	cancelLabel?: string;
	/**
	 * Pre-fill value for text/multiline/guidance/password kinds. Used when
	 * editing existing content (e.g., editing a task description) rather
	 * than entering fresh input. Ignored for choice / multi_choice kinds.
	 */
	defaultValue?: string;
	/**
	 * `multi_choice` — selection bounds. `min_selections` defaults to 0
	 * (empty selection allowed); `max_selections` defaults to unlimited.
	 * Used by `AttentionPromptModal` to gate `canSubmit` and rendered as
	 * a footer hint so operators know the constraints up front.
	 */
	minSelections?: number;
	maxSelections?: number;
	diffApproval?: AttentionPromptDiffApproval;
	/**
	 * The four payloads the shapes above describe. Each is present for exactly
	 * its own `kind` and absent otherwise; a shape whose payload did not arrive
	 * renders the ask and no controls rather than controls that cannot express
	 * it, which is the same absent-not-broken rule the rest of this app follows.
	 */
	confirmation?: AttentionPromptConfirmation;
	externalAction?: AttentionPromptExternalAction;
	filePath?: AttentionPromptFilePath;
	authorization?: AttentionPromptAuthorization;
	/**
	 * Multi-stage MVP — set when this prompt is one of N batched
	 * clarifications. Rendered as a "STEP X OF N" eyebrow so the
	 * operator sees the batch as one logical decision. All three
	 * fields arrive together (set by the canonical envelope's
	 * `input_schema.chain_*`) or none arrive (single-question case).
	 */
	chainId?: string;
	chainPosition?: number;
	chainTotal?: number;
	formQuestions?: Array<{
		id: string;
		prompt: string;
		inputType: string;
		options?: AttentionPromptChoice[];
		/**
		 * The backend's classification of this field, when it collects a
		 * secret. `password`/`otp`/`other` render masked; `login_identifier`
		 * renders plain but says it is kept private. Absent for ordinary fields.
		 */
		sensitive?: 'login_identifier' | 'password' | 'otp' | 'other';
	}>;
	/**
	 * Present when the ask collects a secret (the backend's value-free spec).
	 * `deadlineMs` is the collection deadline; past it the surface stops
	 * accepting the value and offers to ask for a fresh one. `oneTime` names
	 * a code that is good for exactly one submission.
	 */
	sensitive?: {
		kind?: 'login_identifier' | 'password' | 'otp' | 'other';
		oneTime: boolean;
		deadlineMs?: number;
		/** The ask's correlation id — what `GET /hitl/{id}/retrieval` is read for. */
		correlationId?: string;
	};
}

export interface AttentionPromptTextResult {
	kind: 'text' | 'multiline' | 'password' | 'otp' | 'guidance';
	value: string;
}

export interface AttentionPromptChoiceResult {
	kind: 'choice';
	choiceId: string;
	input?: string;
	selectedPaths?: string[];
}

export interface AttentionPromptMultiChoiceResult {
	kind: 'multi_choice';
	choiceIds: string[];
}

export interface AttentionPromptFormResult {
	kind: 'form';
	answers: Array<{
		id: string;
		skipped?: boolean;
		value?: string;
		selected_ids?: string[];
	}>;
}

export type AttentionPromptResult =
	| AttentionPromptTextResult
	| AttentionPromptChoiceResult
	| AttentionPromptMultiChoiceResult
	| AttentionPromptFormResult;

interface InternalState {
	request: AttentionPromptRequest | null;
	resolve: ((value: AttentionPromptResult | null) => void) | null;
}

const state = writable<InternalState>({ request: null, resolve: null });

export const attentionPromptStore: Readable<InternalState> = {
	subscribe: state.subscribe
};

let nextRequestId = 1;

/**
 * Prompts that arrived while a SECRET prompt was open. A credential prompt is
 * never displaced: the owner may be halfway through typing the value, and
 * displacing it resolved the promise with `null`, which the HITL path reads as
 * a dismissal — so it posted `aborted`, cancelled the ask and retired its
 * custody because something else wanted the modal. Queued prompts open in turn
 * as each one is answered or dismissed.
 */
let queued: Array<{
	request: AttentionPromptRequest;
	resolve: (value: AttentionPromptResult | null) => void;
}> = [];
const MAX_QUEUED_PROMPTS = 8;

/**
 * Request input from the user via the themed AttentionPromptModal.
 * Resolves with the input on submit, or `null` on cancel.
 *
 * Concurrent requests cancel the previous (the new one wins). Callers
 * should not assume the previous request still resolves; check the return
 * value at every callsite.
 */
export function requestAttentionInput(
	request: Omit<AttentionPromptRequest, 'id'>
): Promise<AttentionPromptResult | null> {
	return new Promise((resolve) => {
		state.update((current) => {
			if (current.request?.sensitive && current.resolve) {
				if (queued.length >= MAX_QUEUED_PROMPTS) {
					// Refuse the NEWEST rather than dropping one already queued:
					// evicting the oldest would resolve it `null`, and a `null`
					// on a sensitive request posts `aborted` — the very
					// cancellation this queue exists to prevent.
					resolve(null);
					return current;
				}
				queued.push({ request: { ...request, id: nextRequestId++ }, resolve });
				return current;
			}
			if (current.resolve) {
				current.resolve(null);
			}
			return {
				request: { ...request, id: nextRequestId++ },
				resolve
			};
		});
	});
}

/**
 * Resolve the active request from inside the modal component.
 * Called on submit or cancel. Safe to call when no request is active.
 */
export function resolveAttentionPrompt(result: AttentionPromptResult | null): void {
	state.update((current) => {
		if (current.resolve) {
			current.resolve(result);
		}
		const next = queued.shift();
		return next ? { request: next.request, resolve: next.resolve } : { request: null, resolve: null };
	});
}
