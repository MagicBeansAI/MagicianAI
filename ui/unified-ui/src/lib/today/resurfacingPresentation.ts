import type {
	ResurfacingActionCapability,
	ResurfacingActionKind,
	ResurfacingBrief,
	ResurfacingCard,
	ResurfacingDetail,
	ResurfacingRecommendation
} from './resurfacingQueries';

export interface ResurfacingRowFact {
	id: string;
	kind: 'change' | 'date' | 'fact';
	label: string;
	value: string;
}

export interface ResurfacingPrimaryAction {
	kind: ResurfacingActionKind;
	label: string;
	rationale: string;
	contentRevision: string | null;
	isRecommendation: boolean;
}

export type ResurfacingDialogActionKind =
	| 'create_task'
	| 'create_reminder'
	| 'share'
	| 'save_to_memory';

export type ResurfacingShareChannel = 'email' | 'whatsapp' | 'telegram' | 'imessage';

export interface ResurfacingActionDraft {
	title: string;
	instruction: string;
	atLocal: string;
	timezone: string;
	recipient: string;
	channel: ResurfacingShareChannel;
	fact: string;
}

export type ResurfacingActionInputResult =
	| { ok: true; input: Record<string, unknown> }
	| { ok: false; error: string };

function clean(value: string | null | undefined): string {
	return value?.trim() ?? '';
}

function dedupeKey(value: string): string {
	return value.toLocaleLowerCase().replace(/\s+/g, ' ').trim();
}

function sentence(value: string): string {
	const trimmed = clean(value);
	if (!trimmed) return '';
	return /[.!?]$/.test(trimmed) ? trimmed : `${trimmed}.`;
}

function titleCase(value: string): string {
	return value
		.replace(/_/g, ' ')
		.replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function changeValue(change: ResurfacingBrief['changes'][number]): string {
	const before = clean(change.before);
	const after = clean(change.after);
	const effective = clean(change.effective_text);
	let value = '';
	if (before && after) value = `${before} to ${after}`;
	else value = after || before;
	if (effective) value = value ? `${value} · ${effective}` : effective;
	return value;
}

/**
 * Highest-value row facts: concrete changes first, dates second, then general
 * facts. Duplicate text is removed and the row is capped to avoid table shift.
 */
export function resurfacingRowFacts(
	brief: ResurfacingBrief | null,
	limit = 3
): ResurfacingRowFact[] {
	if (!brief || limit <= 0) return [];
	const candidates: ResurfacingRowFact[] = [];
	for (const [index, change] of brief.changes.entries()) {
		const label = clean(change.aspect) || 'Change';
		const value = changeValue(change);
		if (value) candidates.push({ id: `change-${index}`, kind: 'change', label, value });
	}
	for (const [index, fact] of brief.temporal_facts.entries()) {
		const value = clean(fact.text);
		if (value) {
			candidates.push({
				id: `date-${index}`,
				kind: 'date',
				label: titleCase(clean(fact.kind) || 'Date'),
				value
			});
		}
	}
	for (const [index, value] of brief.key_facts.entries()) {
		const fact = clean(value);
		if (fact) candidates.push({ id: `fact-${index}`, kind: 'fact', label: 'Fact', value: fact });
	}

	const seen = new Set<string>();
	return candidates
		.filter((fact) => {
			const key = dedupeKey(`${fact.label} ${fact.value}`);
			if (seen.has(key)) return false;
			seen.add(key);
			return true;
		})
		.slice(0, limit);
}

export function resurfacingConcreteSummary(
	card: ResurfacingCard,
	detail: ResurfacingDetail | null = null
): string {
	if (detail) return clean(detail.summary) || 'Current summary unavailable';
	return (
		clean(card.summary) ||
		clean(card.line) ||
		clean(card.source_title) ||
		'Worth reviewing'
	);
}

export function resurfacingDisplayTitle(
	card: ResurfacingCard,
	detail: ResurfacingDetail | null = null
): string {
	if (detail) return clean(detail.title) || 'Worth a look';
	return clean(card.source_title) || clean(card.line) || 'Worth a look';
}

export function resurfacingBrief(
	card: ResurfacingCard,
	detail: ResurfacingDetail | null = null
): ResurfacingBrief | null {
	return detail ? detail.brief : card.brief;
}

export function resurfacingHasMissingDetails(brief: ResurfacingBrief | null): boolean {
	return Boolean(
		brief &&
			(brief.detail_status === 'source_omits_details' || brief.missing_details.length > 0)
	);
}

export function mergeResurfacingCapabilities(
	card: ResurfacingCard,
	detail: ResurfacingDetail | null = null
): ResurfacingActionCapability[] {
	if (detail) return detail.actions;
	const merged = new Map<ResurfacingActionKind, ResurfacingActionCapability>();
	for (const capability of card.actions) merged.set(capability.kind, capability);
	// Details is a first-party read endpoint and is the compatibility fallback
	// for cards emitted before capabilities were added to the list payload.
	if (!merged.has('view_details')) {
		merged.set('view_details', {
			kind: 'view_details',
			label: 'Details',
			requires_input: false,
			side_effect: 'none'
		});
	}
	return [...merged.values()];
}

export function resurfacingRecommendation(
	card: ResurfacingCard,
	detail: ResurfacingDetail | null = null
): ResurfacingRecommendation | null {
	return detail ? detail.recommended_action : card.recommended_action;
}

export function resurfacingPrimaryAction(
	card: ResurfacingCard,
	detail: ResurfacingDetail | null = null
): ResurfacingPrimaryAction {
	const recommendation = resurfacingRecommendation(card, detail);
	const available = new Set(mergeResurfacingCapabilities(card, detail).map((action) => action.kind));
	if (recommendation && available.has(recommendation.kind)) {
		return {
			kind: recommendation.kind,
			label: clean(recommendation.label) || 'Details',
			rationale: clean(recommendation.rationale),
			contentRevision: recommendation.content_revision,
			isRecommendation: true
		};
	}
	return {
		kind: 'view_details',
		label: 'Details',
		rationale: 'Review the safe structured brief.',
		contentRevision: detail ? detail.content_revision : card.content_revision,
		isRecommendation: false
	};
}

export function isResurfacingDialogAction(
	kind: ResurfacingActionKind
): kind is ResurfacingDialogActionKind {
	return (
		kind === 'create_task' ||
		kind === 'create_reminder' ||
		kind === 'share' ||
		kind === 'save_to_memory'
	);
}

function localDateTimeValue(timestampMs: number | null): string {
	if (timestampMs === null || !Number.isFinite(timestampMs)) return '';
	const date = new Date(timestampMs);
	if (!Number.isFinite(date.getTime())) return '';
	const local = new Date(date.getTime() - date.getTimezoneOffset() * 60_000);
	return local.toISOString().slice(0, 16);
}

function preferredReminderTime(card: ResurfacingCard, detail: ResurfacingDetail | null): number | null {
	const temporal = resurfacingBrief(card, detail)?.temporal_facts.find(
		(fact) => fact.at_ms !== null && fact.at_ms > Date.now()
	);
	return temporal?.at_ms ?? detail?.temporal_anchor_at ?? card.temporal_anchor_at;
}

export function defaultResurfacingActionDraft(
	card: ResurfacingCard,
	detail: ResurfacingDetail | null = null,
	timezone = Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC'
): ResurfacingActionDraft {
	const title = resurfacingDisplayTitle(card, detail).slice(0, 200);
	const summary = resurfacingConcreteSummary(card, detail);
	return {
		title,
		instruction: sentence(summary).slice(0, 4_000),
		atLocal: localDateTimeValue(preferredReminderTime(card, detail)),
		timezone,
		recipient: '',
		channel: 'email',
		fact: sentence(summary).slice(0, 1_200)
	};
}

export function buildResurfacingActionInput(
	kind: ResurfacingDialogActionKind,
	draft: ResurfacingActionDraft,
	uiThreadId: string | null
): ResurfacingActionInputResult {
	const title = clean(draft.title);
	const instruction = clean(draft.instruction);
	const thread = clean(uiThreadId);
	const browserTimezone = Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
	switch (kind) {
		case 'create_task':
			if (!title) return { ok: false, error: 'Title is required.' };
			if (!instruction) return { ok: false, error: 'Instruction is required.' };
			return {
				ok: true,
				input: { title, instruction, ...(thread ? { ui_thread_id: thread } : {}) }
			};
		case 'create_reminder': {
			if (!title) return { ok: false, error: 'Title is required.' };
			if (!instruction) return { ok: false, error: 'Reminder note is required.' };
			const date = new Date(draft.atLocal);
			if (!clean(draft.atLocal) || !Number.isFinite(date.getTime())) {
				return { ok: false, error: 'Choose a valid reminder date and time.' };
			}
			if (date.getTime() <= Date.now()) {
				return { ok: false, error: 'Reminder time must be in the future.' };
			}
			return {
				ok: true,
				input: {
					title,
					instruction,
					at: date.toISOString(),
					timezone: browserTimezone,
					delivery: 'host_apple_reminders',
					...(thread ? { ui_thread_id: thread } : {})
				}
			};
		}
		case 'share':
			if (!clean(draft.recipient)) return { ok: false, error: 'Recipient is required.' };
			return {
				ok: true,
				input: {
					recipient: clean(draft.recipient),
					channel: draft.channel,
					...(instruction ? { instruction } : {}),
					...(thread ? { ui_thread_id: thread } : {})
				}
			};
		case 'save_to_memory':
			if (!clean(draft.fact)) return { ok: false, error: 'Fact is required.' };
			return {
				ok: true,
				input: { ...(title ? { title } : {}), fact: clean(draft.fact) }
			};
	}
}
