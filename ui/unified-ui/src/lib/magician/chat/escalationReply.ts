/**
 * Answering an agentic pause from the chat it was raised in.
 *
 * A delegated run that calls `need_user_input` puts an escalation card in the
 * chat that started it. Two shortcuts answer it without the modal:
 *
 * - **One click.** A `choice` card shows the ask's own options (they ride in
 *   `input_schema.options`; the card's `options` hold one generic `Respond`),
 *   and a plain `confirmation` card's Approve / Deny post at once.
 * - **Typed.** While the newest open card in the session is such an ask, the
 *   composer's text answers it instead of starting a new turn.
 *
 * Both produce an `AttentionPromptResult` — what the modal would have
 * returned — so the post, the re-ask loop and the resolution are the modal's
 * own (`answerTaskAsk` → `openHitlPrompt`). Nothing here posts.
 *
 * See `docs/components/unified-ui/chat-next.md`, *Answering an agent's pause in chat*.
 */

import { isFreeTextChoiceOption } from '$lib/hitl/freeTextChoice';
import type { AttentionPromptResult } from '$lib/stores/attentionPromptStore';
import type { HitlOpenTarget } from '$lib/hitl/types';
import type { ChatMessage, ChatMessageContent, EscalationOption } from '$lib/stores/chatStore';

/** The pause input types an owner can answer by typing in chat. */
const TYPED_ANSWER_INPUT_TYPES = new Set(['text', 'guidance', 'choice', 'confirmation', 'multi_choice']);

/**
 * Escalations with an answer path of their own: planning clarifications reply
 * through the planner composer, Keep Trying has its own endpoint, grants and
 * preference holds must be read in full, and guidance-or-done asks are
 * external actions.
 */
const SPECIAL_ESCALATION_TYPES = new Set([
	'clarification',
	'max_iterations',
	'tool_authorization',
	'sandbox_override',
	'preference_alignment',
	'cannot_proceed',
	'loop_detected'
]);

const YES_WORDS = new Set(['yes', 'y', 'yep', 'yeah', 'ok', 'okay', 'approve', 'approved', 'confirm', 'go ahead', 'do it', 'sure']);
const NO_WORDS = new Set(['no', 'n', 'nope', 'deny', 'denied', 'reject', 'cancel', 'stop', "don't", 'do not']);

function record(value: unknown): Record<string, unknown> | null {
	return value && typeof value === 'object' && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function schemaString(schema: Record<string, unknown> | undefined, key: string): string | null {
	const value = schema?.[key];
	return typeof value === 'string' && value.trim() ? value.trim() : null;
}

/** The ask's own options, off its public `input_schema`. */
export function escalationSchemaChoices(
	inputSchema: Record<string, unknown> | undefined
): EscalationOption[] {
	const options = inputSchema?.options;
	if (!Array.isArray(options)) return [];
	const choices: EscalationOption[] = [];
	for (const entry of options) {
		const option = record(entry);
		const id = typeof option?.id === 'string' ? option.id.trim() : '';
		// A repeated id is one answer; a keyed list must not see it twice.
		if (!id || choices.some((choice) => choice.id === id)) continue;
		const label = typeof option?.label === 'string' && option.label.trim() ? option.label.trim() : id;
		choices.push(option?.requires_input === true ? { id, label, requires_input: true } : { id, label });
	}
	return choices;
}

/** Options as a card shows them: one entry per id, first one wins. */
function uniqueOptions(options: EscalationOption[]): EscalationOption[] {
	const seen = new Set<string>();
	return options.filter((option) => {
		if (seen.has(option.id)) return false;
		seen.add(option.id);
		return true;
	});
}

/**
 * An agentic pause the owner answers directly: a live execution pause, not a
 * UserRequestService ask and not one of the escalations with a path of their
 * own, never one that collects a secret, and never a protected app's ask,
 * whose details generic surfaces do not show.
 */
function isOwnerAnswerablePause(content: ChatMessageContent): boolean {
	if (content.type !== 'escalation') return false;
	if (!content.execution_id?.trim() || !content.pause_state_id?.trim()) return false;
	if (content.request_id?.trim()) return false;
	if (content.escalation_type && SPECIAL_ESCALATION_TYPES.has(content.escalation_type)) return false;
	if (record(content.input_schema?.sensitive)) return false;
	if (content.input_schema?.protected === true) return false;
	return true;
}

/**
 * A confirmation that one click (or a typed yes/no) may answer — an
 * allow-list, not a deny-list. Only the agent's own `need_user_input`
 * confirmation qualifies: its schema is the typed widget (`type:
 * "confirmation"`) with the producer's explicit `destructive: false`. An
 * agentic tool-action approval (`action_type`, no destructive flag), a
 * protected app's ask, a previewed approval and a preference hold all keep the
 * modal, where the full question is in front of the reader — the same reason
 * the push lane never offers one-tap confirm for agentic asks.
 */
function isPlainConfirmation(content: ChatMessageContent): boolean {
	const schema = content.input_schema;
	return (
		content.input_type === 'confirmation' &&
		content.escalation_type === 'confirmation' &&
		schema?.type === 'confirmation' &&
		schema.destructive === false &&
		!schema.action_type &&
		schema.protected !== true &&
		!record(schema.approval_preview) &&
		!record(schema.alignment)
	);
}

/**
 * The options an escalation card shows. A pending agentic `choice` shows the
 * ask's own options; every other card shows the ones the backend put on it.
 */
export function escalationCardOptions(content: ChatMessageContent): EscalationOption[] {
	if (content.type === 'escalation' && content.input_type === 'choice' && isOwnerAnswerablePause(content)) {
		const choices = escalationSchemaChoices(content.input_schema);
		if (choices.length > 0) return choices;
	}
	return uniqueOptions(content.options ?? []);
}

/**
 * What a click on one card option answers, or `null` when the click should
 * open the prompt instead (a change/other option, whose words the prompt
 * collects, or an ask this shortcut does not cover).
 */
export function escalationOneClickResult(
	content: ChatMessageContent,
	option: EscalationOption
): AttentionPromptResult | null {
	if (!isOwnerAnswerablePause(content)) return null;
	if (content.input_type === 'choice') {
		const match = escalationSchemaChoices(content.input_schema).find(
			(choice) => choice.id === option.id
		);
		return match && !isFreeTextChoiceOption(match) ? { kind: 'choice', choiceId: match.id } : null;
	}
	if (option.requires_input) return null;
	if (isPlainConfirmation(content) && (option.id === 'approve' || option.id === 'deny')) {
		return { kind: 'choice', choiceId: option.id };
	}
	return null;
}

/** Whether typed composer text can answer this card. */
export function isTypedAnswerableEscalation(content: ChatMessageContent): boolean {
	if (!isOwnerAnswerablePause(content)) return false;
	const inputType = content.input_type ?? '';
	if (!TYPED_ANSWER_INPUT_TYPES.has(inputType)) return false;
	if (inputType === 'choice' || inputType === 'multi_choice') {
		return escalationSchemaChoices(content.input_schema).length > 0;
	}
	if (inputType === 'confirmation') return isPlainConfirmation(content);
	return true;
}

/**
 * The card typed composer text would answer, or `null`.
 *
 * Only the newest open card decides: when it is not an ask chat can answer by
 * typing, the text is a normal message rather than an answer to an older
 * card. A card the transcript shows as answered, superseded or no longer
 * active (`isActionable` is the card's own test) is never a target, and
 * neither is one the owner chose to talk past (`dismissedPauseId`).
 */
export function pendingEscalationReplyTarget(
	messages: ChatMessage[],
	sessionId: string | null | undefined,
	isActionable: (content: ChatMessageContent) => boolean,
	dismissedPauseId: string | null = null
): ChatMessage | null {
	if (!sessionId) return null;
	for (let index = messages.length - 1; index >= 0; index -= 1) {
		const message = messages[index];
		if (message.session_id !== sessionId || message.content.type !== 'escalation') continue;
		const content = message.content;
		if (content.resolved || content.stale || !isActionable(content)) continue;
		if (dismissedPauseId && content.pause_state_id?.trim() === dismissedPauseId) return null;
		return isTypedAnswerableEscalation(content) ? message : null;
	}
	return null;
}

/** Lower-case, collapse whitespace, drop wrapping quotes and trailing punctuation or an ellipsis. */
function normalizeAnswer(value: string): string {
	return value
		.trim()
		.toLowerCase()
		.replace(/\s+/g, ' ')
		.replace(/^["'“‘]+|["'”’]+$/g, '')
		.replace(/(\.{3}|…|[.!?,;:])+$/u, '')
		.trim();
}

function matchChoice(text: string, choices: EscalationOption[]): EscalationOption | null {
	const wanted = normalizeAnswer(text);
	if (!wanted) return null;
	return (
		choices.find((choice) => normalizeAnswer(choice.id) === wanted) ??
		choices.find((choice) => normalizeAnswer(choice.label) === wanted) ??
		null
	);
}

/** The option that takes free text: one that asks for input, else one named change/other. */
function freeTextChoice(choices: EscalationOption[]): EscalationOption | null {
	return (
		choices.find((choice) => choice.requires_input) ??
		choices.find((choice) => isFreeTextChoiceOption(choice)) ??
		null
	);
}

/**
 * Typed composer text as the answer the card's ask expects, or `null` when the
 * text does not fit it — the caller then keeps it in the composer and says
 * what the ask takes (`escalationReplyChoiceLabels`).
 *
 * A `choice` matches an option id or label, case-insensitively. Text matching
 * none goes to the ask's change/other option with the words as its free-text
 * value (`other_value` on the wire), when the ask has one.
 */
export function typedEscalationAnswer(
	content: ChatMessageContent,
	text: string
): AttentionPromptResult | null {
	const value = text.trim();
	if (!value || !isTypedAnswerableEscalation(content)) return null;
	switch (content.input_type) {
		case 'text':
			return { kind: 'text', value };
		case 'guidance':
			return { kind: 'guidance', value };
		case 'choice': {
			const choices = escalationSchemaChoices(content.input_schema);
			const match = matchChoice(value, choices);
			// Naming the change/other option itself carries no words to send
			// with it; the agent reads the choice and asks what to change.
			if (match) return { kind: 'choice', choiceId: match.id };
			const other = freeTextChoice(choices);
			return other ? { kind: 'choice', choiceId: other.id, input: value } : null;
		}
		case 'multi_choice': {
			const choices = escalationSchemaChoices(content.input_schema);
			// A label may itself hold a comma or an "and" ("Notes and slides").
			const whole = matchChoice(value, choices);
			if (whole) return { kind: 'multi_choice', choiceIds: [whole.id] };
			const parts = value
				.split(/,|;|\band\b/i)
				.map((part) => part.trim())
				.filter(Boolean);
			const ids: string[] = [];
			for (const part of parts) {
				const match = matchChoice(part, choices);
				if (!match) return null;
				if (!ids.includes(match.id)) ids.push(match.id);
			}
			return ids.length > 0 ? { kind: 'multi_choice', choiceIds: ids } : null;
		}
		case 'confirmation': {
			const wanted = normalizeAnswer(value);
			const approveLabel =
				content.options?.find((option) => option.id === 'approve')?.label ??
				schemaString(content.input_schema, 'confirm_label');
			const denyLabel =
				content.options?.find((option) => option.id === 'deny')?.label ??
				schemaString(content.input_schema, 'deny_label');
			if (YES_WORDS.has(wanted) || (approveLabel && normalizeAnswer(approveLabel) === wanted)) {
				return { kind: 'choice', choiceId: 'approve' };
			}
			if (NO_WORDS.has(wanted) || (denyLabel && normalizeAnswer(denyLabel) === wanted)) {
				return { kind: 'choice', choiceId: 'deny' };
			}
			return null;
		}
		default:
			return null;
	}
}

/** The answers the composer hint lists, for an ask with fixed options. */
export function escalationReplyChoiceLabels(content: ChatMessageContent): string[] {
	if (content.input_type === 'choice' || content.input_type === 'multi_choice') {
		return escalationSchemaChoices(content.input_schema).map((choice) => choice.label);
	}
	if (content.input_type === 'confirmation') {
		return uniqueOptions(content.options ?? []).map((option) => option.label);
	}
	return [];
}

/**
 * A composer-dock ask as the card shape the typed-reply rules read, or `null`.
 *
 * An agent's pause seldom reaches the transcript as a card: chat leaves
 * `hitl.requested` to the dock above the composer (`ComposerAskDock`), which
 * reads the pending-asks store. Typed composer text answers the dock's
 * active ask by the same rules as a card's: `isTypedAnswerableEscalation`
 * decides, and the answer posts through the dock's own target. Only an agentic
 * pause qualifies; a grant, preference hold or guidance-or-done ask keeps its
 * own surface (its `escalation_trigger` is one of the special types).
 */
export function dockAskReplyContent(target: HitlOpenTarget | null | undefined): ChatMessageContent | null {
	if (!target || target.source !== 'agentic') return null;
	const executionId = target.scope?.execution_id?.trim() ?? '';
	const pauseStateId = target.identifiers?.pause_state_id?.trim() || target.id.trim();
	if (!executionId || !pauseStateId) return null;
	const schema = (target.input_schema ?? undefined) as Record<string, unknown> | undefined;
	const trigger = typeof schema?.escalation_trigger === 'string' ? schema.escalation_trigger : null;
	const content: ChatMessageContent = {
		type: 'escalation',
		execution_id: executionId,
		pause_state_id: pauseStateId,
		escalation_type: trigger ?? (target.input_type === 'confirmation' ? 'confirmation' : 'user_input'),
		input_type: target.input_type,
		input_schema: schema,
		question: target.prompt,
		hint: target.hint ?? undefined,
		options: []
	} as ChatMessageContent;
	return isTypedAnswerableEscalation(content) ? content : null;
}

