/**
 * Side-notch inbox — the Attention questions that used to be bottom-right
 * launcher cards. Pure except for the request memory the overlay uses when
 * it posts an answer. The notch renders the question and its options; this
 * module decides which asks can be answered there.
 */

import { hitlRequestFromCanonicalEvent, postHitlResponse } from '$lib/hitl/adapters';
import { mapPromptResultToHitlValue } from '$lib/hitl/respondToHitl';
import type { HitlRequest, HitlResponseValue } from '$lib/hitl/types';
import { hitlEventToCard, type NotifyCard } from './cardModel';

export interface NotchOption {
	id: string;
	label: string;
	description?: string;
}

export type NotchInline = 'choice' | 'multi_choice' | 'confirmation' | 'text' | 'open';

export interface NotchInboxItem {
	id: string;
	correlationId: string;
	source: string;
	inputType: string;
	prompt: string;
	hint?: string;
	inline: NotchInline;
	options: NotchOption[];
	confirmLabel?: string;
	denyLabel?: string;
	executionId?: string;
	taskId?: string;
}

export interface NotchAnswer {
	id: string;
	correlationId: string;
	kind: NotchInline | 'open' | 'dismiss';
	choiceId?: string;
	choiceIds?: string[];
	text?: string;
	/** The host already posted this answer. The overlay only drops the card. */
	posted?: boolean;
}

const requests = new Map<string, HitlRequest>();
const items = new Map<string, NotchInboxItem>();

export function resetNotchMemory(): void {
	requests.clear();
	items.clear();
}

function readString(record: Record<string, unknown>, key: string): string | undefined {
	const value = record[key];
	return typeof value === 'string' && value.trim().length > 0 ? value.trim() : undefined;
}

function readOptions(value: unknown): NotchOption[] {
	if (!Array.isArray(value)) return [];
	const out: NotchOption[] = [];
	for (const entry of value) {
		if (!entry || typeof entry !== 'object') continue;
		const record = entry as Record<string, unknown>;
		const id = readString(record, 'id') ?? readString(record, 'value');
		const label = readString(record, 'label');
		if (!id || !label) continue;
		const description = readString(record, 'description');
		out.push(description ? { id, label, description } : { id, label });
	}
	return out;
}

function schemaOf(data: Record<string, unknown>): Record<string, unknown> | null {
	const schema = data.input_schema;
	return schema && typeof schema === 'object' && !Array.isArray(schema)
		? (schema as Record<string, unknown>)
		: null;
}

function isSensitive(schema: Record<string, unknown> | null): boolean {
	const sensitive = schema?.sensitive;
	return Boolean(sensitive && typeof sensitive === 'object');
}

/**
 * What the notch can answer without opening Attention. Secrets, forms, diffs
 * and file picks stay a deep link — the notch shows the question either way.
 */
export function inlineKindFor(
	inputType: string,
	options: NotchOption[],
	sensitive: boolean
): NotchInline {
	if (sensitive) return 'open';
	if (inputType === 'confirmation') return 'confirmation';
	if (inputType === 'multi_choice' && options.length > 0) return 'multi_choice';
	if (
		(inputType === 'choice' ||
			inputType === 'tool_authorization' ||
			inputType === 'sandbox_override') &&
		options.length > 0
	) {
		return 'choice';
	}
	if (inputType === 'text' || inputType === 'guidance') return 'text';
	return 'open';
}

export function notchItemFromEvent(event: {
	event_type: string;
	data: Record<string, unknown>;
}): NotchInboxItem | null {
	const card = hitlEventToCard(event);
	if (!card || card.kind !== 'actionable') return null;
	const schema = schemaOf(event.data ?? {});
	const options = readOptions(schema?.options);
	const inline = inlineKindFor(card.inputType, options, isSensitive(schema));
	const confirmLabel = readString(schema ?? {}, 'confirm_label');
	const denyLabel = readString(schema ?? {}, 'deny_label');
	const executionId = readString(event.data ?? {}, 'execution_id');
	const taskId = readString(event.data ?? {}, 'task_id');
	return {
		id: card.id,
		correlationId: card.correlationId,
		source: card.source,
		inputType: card.inputType,
		prompt: card.prompt,
		...(card.hint ? { hint: card.hint } : {}),
		inline,
		options,
		...(confirmLabel ? { confirmLabel } : {}),
		...(denyLabel ? { denyLabel } : {}),
		...(executionId ? { executionId } : {}),
		...(taskId ? { taskId } : {})
	};
}

/** Remember a live event so a later notch answer can post the same request. */
export function rememberNotchEvent(event: {
	event_type: string;
	data: Record<string, unknown>;
}): void {
	if (event.event_type === 'HitlResolved') {
		const correlationId = readString(event.data ?? {}, 'correlation_id');
		if (!correlationId) return;
		for (const [id, item] of items) {
			if (item.correlationId === correlationId) {
				items.delete(id);
				requests.delete(id);
			}
		}
		return;
	}
	const item = notchItemFromEvent(event);
	if (!item) return;
	items.set(item.id, item);
	const request = hitlRequestFromCanonicalEvent(event);
	if (request) requests.set(item.id, request);
	else requests.delete(item.id);
}

/** Drop memory the card store no longer shows (suppressed, resolved, reset). */
export function syncNotchMemory(cards: NotifyCard[]): void {
	const live = new Set(
		cards.filter((card) => card.kind === 'actionable').map((card) => card.id)
	);
	for (const id of items.keys()) {
		if (!live.has(id)) {
			items.delete(id);
			requests.delete(id);
		}
	}
}

export function notchItemsForCards(cards: NotifyCard[]): NotchInboxItem[] {
	syncNotchMemory(cards);
	const out: NotchInboxItem[] = [];
	for (const card of cards) {
		if (card.kind !== 'actionable') continue;
		const remembered = items.get(card.id);
		if (remembered) {
			out.push(remembered);
			continue;
		}
		out.push({
			id: card.id,
			correlationId: card.correlationId,
			source: card.source,
			inputType: card.inputType,
			prompt: card.prompt,
			...(card.hint ? { hint: card.hint } : {}),
			inline: inlineKindFor(card.inputType, [], false),
			options: []
		});
	}
	return out;
}

export function valueForNotchAnswer(
	request: HitlRequest,
	answer: NotchAnswer
): HitlResponseValue | null {
	if (answer.kind === 'confirmation' || answer.kind === 'choice') {
		if (!answer.choiceId) return null;
		return mapPromptResultToHitlValue(request, {
			kind: 'choice',
			choiceId: answer.choiceId
		});
	}
	if (answer.kind === 'multi_choice') {
		const choiceIds = answer.choiceIds ?? [];
		if (choiceIds.length === 0) return null;
		return mapPromptResultToHitlValue(request, { kind: 'multi_choice', choiceIds });
	}
	if (answer.kind === 'text') {
		const value = answer.text?.trim() ?? '';
		if (!value) return null;
		const kind = request.input_type === 'guidance' ? 'guidance' : 'text';
		return mapPromptResultToHitlValue(request, { kind, value });
	}
	return null;
}

export function requestForNotch(id: string): HitlRequest | undefined {
	return requests.get(id);
}

export async function answerNotchItem(
	answer: NotchAnswer
): Promise<{ ok: boolean; message?: string; drop?: boolean }> {
	const request = requests.get(answer.id);
	if (!request) {
		// The card can outlive the request (legacy stub, already answered,
		// or never remembered). The notch must drop it.
		return { ok: false, drop: true, message: 'That question is no longer waiting.' };
	}
	const value = valueForNotchAnswer(request, answer);
	if (!value) {
		return { ok: false, message: 'Pick an option or write an answer first.' };
	}
	const outcome = await postHitlResponse(request, value);
	if (outcome.ok) return { ok: true };
	const message = 'message' in outcome ? outcome.message : 'The answer did not send.';
	return { ok: false, message };
}
