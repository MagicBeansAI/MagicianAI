/**
 * Preference alignment holds on the web (P6.1).
 *
 * A hold is an ordinary `confirmation` ask whose `input_schema` carries
 * `escalation_trigger: "preference_alignment"` and an `alignment` block —
 * the public projection: choice ids, non-revealing labels, the deadline and
 * the stage. The shared event never says *why*; this surface is one of the
 * owner-private ones (P0 §8.6), so it reads the reason, the cited
 * preferences and any wording readback from
 * `GET /api/magician/v2/hitl/{id}/alignment?surface=web`.
 *
 * Answers go through the one canonical respond route like every other ask:
 *
 * - a choice posts `{type: 'choice', selected_id}`;
 * - "change my preference" carries the new wording in `other_value`;
 * - "save this wording" carries the readback's token in `other_value` —
 *   only the private read shows it, so nothing else can confirm a lasting
 *   change;
 * - a typed reply posts `{type: 'text', value}`; it is the owner's
 *   instruction, never consent.
 */
import type { HitlResponseValue } from './types';

export type AlignmentStage = 'decide' | 'confirm_wording';

export interface AlignmentChoice {
	id: string;
	label: string;
}

/** `input_schema.alignment` — safe on any surface. */
export interface AlignmentProjection {
	askId: string;
	purpose: string;
	choices: AlignmentChoice[];
	freeText: boolean;
	deadlineAtMs: number | null;
	stage: AlignmentStage;
	/**
	 * Other gates this one ask also settles (a combined ask): gate names
	 * only, e.g. `approval_rule`. Allowing the act approves it for them too.
	 */
	alsoCovers: string[];
}

/** One other requirement a combined ask presents, with its reason. */
export interface AlignmentCoveredRequirement {
	gate: string;
	reason: string;
}

export interface AlignmentDetailChoice extends AlignmentChoice {
	/** Changes what Magician remembers rather than deciding the act. */
	lasting: boolean;
	/** The act's one consent ("Allow this once"). */
	consent: boolean;
	/** Needs the owner's new wording. */
	needsWording: boolean;
}

export interface AlignmentIntention {
	id: string;
	statement: string;
	assertionStatus: string;
	editable: boolean;
}

export interface AlignmentReadback {
	kind: 'remember' | 'replace';
	wording: string;
	superseded: string | null;
	token: string;
}

/** What the owner-private read returns. */
export interface AlignmentDetail {
	private: boolean;
	stage: AlignmentStage;
	explanation: string | null;
	actSummary: string | null;
	intentions: AlignmentIntention[];
	remember: { id: string; statement: string } | null;
	choices: AlignmentDetailChoice[];
	readback: AlignmentReadback | null;
	retryReason: string | null;
	deadlineAtMs: number | null;
	alsoCovers: AlignmentCoveredRequirement[];
}

/** The choice a typed reply rides on when a client only has choices. */
export const ALIGNMENT_REPLY_CHOICE = 'respond';

function record(value: unknown): Record<string, unknown> | null {
	return typeof value === 'object' && value !== null && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function str(value: unknown): string | null {
	return typeof value === 'string' && value.length > 0 ? value : null;
}

function stageOf(value: unknown): AlignmentStage {
	return value === 'confirm_wording' ? 'confirm_wording' : 'decide';
}

function choicesOf(value: unknown): AlignmentChoice[] {
	if (!Array.isArray(value)) return [];
	return value.flatMap((entry) => {
		const choice = record(entry);
		const id = str(choice?.id);
		const label = str(choice?.label);
		return id && label ? [{ id, label }] : [];
	});
}

/**
 * Read `input_schema.alignment`; `null` when it is absent or malformed.
 *
 * Accepts the wire block and this module's own parsed shape: an open target
 * built from a parsed request carries the parsed projection, and reading it
 * back must not drop the hold (which would leave a plain approve that posts
 * consent without the reason).
 */
export function readAlignmentProjection(value: unknown): AlignmentProjection | null {
	const block = record(value);
	if (!block) return null;
	if (block.kind === undefined && typeof block.askId === 'string') {
		const askId = str(block.askId);
		const choices = choicesOf(block.choices);
		if (!askId || choices.length === 0) return null;
		return {
			askId,
			purpose: str(block.purpose) ?? 'allow_once',
			choices,
			freeText: block.freeText !== false,
			deadlineAtMs: typeof block.deadlineAtMs === 'number' ? block.deadlineAtMs : null,
			stage: stageOf(block.stage),
			alsoCovers: Array.isArray(block.alsoCovers)
				? block.alsoCovers.flatMap((gate) => (str(gate) ? [gate as string] : []))
				: []
		};
	}
	if (block.kind !== 'preference_alignment') return null;
	const askId = str(block.ask_id);
	const choices = choicesOf(block.choices);
	if (!askId || choices.length === 0) return null;
	return {
		askId,
		purpose: str(block.purpose) ?? 'allow_once',
		choices,
		freeText: block.free_text !== false,
		deadlineAtMs: typeof block.deadline_at_ms === 'number' ? block.deadline_at_ms : null,
		stage: stageOf(block.stage),
		alsoCovers: Array.isArray(block.also_covers)
			? block.also_covers.flatMap((gate) => (str(gate) ? [gate as string] : []))
			: []
	};
}

/** Parse the private read. Unknown or missing fields stay absent. */
export function parseAlignmentDetail(raw: unknown): AlignmentDetail | null {
	const body = record(raw);
	const alignment = record(body?.alignment);
	if (!body || !alignment) return null;
	const choices: AlignmentDetailChoice[] = Array.isArray(alignment.choices)
		? alignment.choices.flatMap((entry) => {
				const choice = record(entry);
				const id = str(choice?.id);
				const label = str(choice?.label);
				if (!id || !label) return [];
				return [
					{
						id,
						label,
						lasting: choice?.lasting === true,
						consent: choice?.consent === true,
						needsWording: choice?.needs_wording === true
					}
				];
			})
		: [];
	const intentions: AlignmentIntention[] = Array.isArray(alignment.intentions)
		? alignment.intentions.flatMap((entry) => {
				const intention = record(entry);
				const id = str(intention?.id);
				const statement = str(intention?.statement);
				if (!id || !statement) return [];
				return [
					{
						id,
						statement,
						assertionStatus: str(intention?.assertion_status) ?? 'unknown_provenance',
						editable: intention?.editable === true
					}
				];
			})
		: [];
	const remember = record(alignment.remember);
	const readback = record(alignment.readback);
	const readbackToken = str(readback?.token);
	const readbackWording = str(readback?.wording);
	return {
		private: body.private === true,
		stage: stageOf(alignment.stage),
		explanation: str(alignment.explanation),
		actSummary: str(alignment.act_summary),
		intentions,
		remember:
			remember && str(remember.id) && str(remember.statement)
				? { id: str(remember.id)!, statement: str(remember.statement)! }
				: null,
		choices,
		readback:
			readback && readbackToken && readbackWording
				? {
						kind: readback.kind === 'replace' ? 'replace' : 'remember',
						wording: readbackWording,
						superseded: str(readback.superseded),
						token: readbackToken
					}
				: null,
		retryReason: str(body.retry_reason),
		deadlineAtMs: typeof alignment.deadline_at_ms === 'number' ? alignment.deadline_at_ms : null,
		alsoCovers: Array.isArray(alignment.also_covers)
			? alignment.also_covers.flatMap((entry) => {
					const covered = record(entry);
					const gate = str(covered?.gate);
					return gate ? [{ gate, reason: str(covered?.reason) ?? '' }] : [];
				})
			: []
	};
}

/**
 * An answer to a hold, as the respond route's value.
 *
 * `choiceId` is an offered choice id, or `respond` for a typed reply. Every
 * choice, the act's consent included, is posted as a typed choice: the
 * server resolves it against the ask, never by label.
 */
export function alignmentAnswerValue(choiceId: string, input?: string): HitlResponseValue | null {
	const words = input?.trim() ?? '';
	if (choiceId === ALIGNMENT_REPLY_CHOICE) {
		return words.length > 0 ? { type: 'text', value: words } : null;
	}
	return words.length > 0
		? { type: 'choice', selected_id: choiceId, other_value: words }
		: { type: 'choice', selected_id: choiceId };
}

/** How long the owner has to answer. `null` when the deadline is unknown. */
export function deadlineLine(deadlineAtMs: number | null, nowMs: number = Date.now()): string | null {
	if (deadlineAtMs === null) return null;
	const left = deadlineAtMs - nowMs;
	if (left <= 0) return 'The answer window has closed; a late answer runs nothing.';
	const minutes = Math.round(left / 60_000);
	if (minutes < 60) return `Answer within ${Math.max(minutes, 1)} min; after that it won't run.`;
	const hours = Math.round(minutes / 60);
	return `Answer within ${hours} h; after that it won't run.`;
}
