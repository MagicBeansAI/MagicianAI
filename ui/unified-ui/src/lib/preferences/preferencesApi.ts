/**
 * The owner's preferences, exactly as preference checks see them, and their
 * history (docs/components/magician/preference-alignment.md, "Owner view and
 * edits", "Preference history").
 *
 * - `GET /preference-alignment/intentions?surface=web` — the view.
 * - `POST` / `PATCH` / `DELETE /preference-alignment/intentions[/{id}]` — add,
 *   edit, confirm, retire, forget. Every edit but add names the revision this
 *   screen showed; a changed preference answers `stale` with its current one.
 * - `GET /preference-alignment/history` and `/intentions/{id}/history`.
 *
 * Only the owner's web and iOS apps may call these; the server refuses any
 * other surface.
 */
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

const BASE = '/api/magician/v2/preference-alignment';
const SURFACE = 'surface=web';

export type IntentionKind = 'taste' | 'goal' | 'standing_instruction';
export type AssertionStatus =
	| 'direct_owner_statement'
	| 'confirmed'
	| 'inferred'
	| 'inferred_untrusted'
	| 'unknown_provenance';

export interface Preference {
	id: string;
	revision: string;
	kind: IntentionKind;
	status: AssertionStatus;
	statement: string;
	conditions: string | null;
	lifecycle: string;
	/** The memory tier, or `taste profile` / another source name. */
	source: string;
	editable: boolean;
	lastCheckedAtMs: number | null;
	/** When memory first saved it; `null` for profile and app preferences. */
	learnedAtMs: number | null;
	grounds: Array<{ role: string; sourceId: string }>;
}

export interface RetiredPreference {
	id: string;
	statement: string;
	lifecycle: string;
	supersededBy: string | null;
	revision: string;
}

export interface OmittedPreference {
	id: string;
	reason: string;
}

export interface PreferenceView {
	preferences: Preference[];
	retired: RetiredPreference[];
	omitted: OmittedPreference[];
	coverage: string;
	policyRevision: number | null;
}

export interface HistoryEntry {
	atMs: number;
	recordId: string;
	op: string;
	via: string;
	surface: string | null;
	wordingBefore: string | null;
	wordingAfter: string | null;
	wordingErased: boolean;
}

export type EditOutcome =
	| { status: 'applied'; recordId: string }
	| { status: 'stale'; currentRevision: string; message: string };

const str = (value: unknown): string | null => (typeof value === 'string' ? value : null);

function sourceName(source: unknown): string {
	if (source && typeof source === 'object') {
		const [variant, detail] = Object.entries(source as Record<string, unknown>)[0] ?? [];
		const tier = detail && typeof detail === 'object' ? str((detail as Record<string, unknown>).tier) : null;
		if (tier) return tier;
		if (variant) return variant.replace(/_/g, ' ');
	}
	return typeof source === 'string' ? source.replace(/_/g, ' ') : 'unknown';
}

export function parseView(body: unknown): PreferenceView {
	const value = (body ?? {}) as Record<string, unknown>;
	const list = (key: string): Array<Record<string, unknown>> =>
		Array.isArray(value[key]) ? (value[key] as Array<Record<string, unknown>>) : [];
	return {
		preferences: list('intentions').map((row) => ({
			id: str(row.id) ?? '',
			revision: str(row.revision) ?? '',
			kind: (str(row.kind) ?? 'taste') as IntentionKind,
			status: (str(row.assertion_status) ?? 'inferred') as AssertionStatus,
			statement: str(row.statement) ?? '',
			conditions: str(row.conditions),
			lifecycle: str(row.lifecycle) ?? 'active',
			source: sourceName(row.source),
			editable: row.editable === true,
			lastCheckedAtMs: typeof row.last_checked_at_ms === 'number' ? row.last_checked_at_ms : null,
			learnedAtMs: typeof row.learned_at_ms === 'number' ? row.learned_at_ms : null,
			grounds: (Array.isArray(row.grounds) ? row.grounds : []).map((ground) => ({
				role: str((ground as Record<string, unknown>).role) ?? 'unknown',
				sourceId: str((ground as Record<string, unknown>).source_id) ?? ''
			}))
		})),
		retired: list('retired').map((row) => ({
			id: str(row.id) ?? '',
			statement: str(row.statement) ?? '',
			lifecycle: str(row.lifecycle) ?? 'retired',
			supersededBy: str(row.superseded_by),
			revision: str(row.revision) ?? ''
		})),
		omitted: list('omitted').map((row) => ({
			id: str(row.id) ?? '',
			reason: str(row.reason) ?? 'unreadable'
		})),
		coverage: str((value.coverage as Record<string, unknown> | undefined)?.status) ?? 'complete',
		policyRevision: typeof value.policy_revision === 'number' ? value.policy_revision : null
	};
}

/** A stored statement shaped for reading: a title and its fields. */
export interface PresentedStatement {
	title: string;
	details: Array<{ label: string; value: string }>;
}

const ACRONYMS = new Set(['ui', 'api', 'url', 'llm', 'id', 'pdf', 'sms', 'ai']);

const humanize = (key: string): string => {
	const words = key
		.replace(/[._]+/g, ' ')
		.trim()
		.split(/\s+/)
		.map((word) => (ACRONYMS.has(word.toLowerCase()) ? word.toUpperCase() : word));
	const text = words.join(' ');
	return text.charAt(0).toUpperCase() + text.slice(1);
};

/**
 * Memory sometimes stores a preference as `key: {json}`. Shown raw it is a
 * blob; this renders the key as a title and each field as a line. Display
 * only: editing, checks and history keep the stored wording. `null` when the
 * statement is ordinary prose.
 */
export function presentStatement(statement: string): PresentedStatement | null {
	const match = /^\s*([A-Za-z][\w.]*)\s*:\s*(\{[\s\S]*\})\s*$/.exec(statement);
	if (!match) return null;
	let fields: unknown;
	try {
		fields = JSON.parse(match[2]);
	} catch {
		return null;
	}
	if (!fields || typeof fields !== 'object' || Array.isArray(fields)) return null;
	// Sorted by key on every client: iOS's JSON decoding keeps no order.
	const details = Object.entries(fields as Record<string, unknown>)
		.sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
		.map(([key, value]) => ({
		label: humanize(key),
		value: typeof value === 'string' ? value : JSON.stringify(value)
	}));
	return { title: humanize(match[1]), details };
}

export function parseHistory(body: unknown): HistoryEntry[] {
	const entries = (body as { entries?: unknown } | null)?.entries;
	return (Array.isArray(entries) ? entries : []).map((raw) => {
		const entry = raw as Record<string, unknown>;
		const actor = (entry.actor ?? {}) as Record<string, unknown>;
		return {
			atMs: typeof entry.at_ms === 'number' ? entry.at_ms : 0,
			recordId: str(entry.record_id) ?? '',
			op: str(entry.op) ?? 'changed',
			via: str(actor.via) ?? 'unknown',
			surface: str(actor.surface),
			wordingBefore: str(entry.wording_before),
			wordingAfter: str(entry.wording_after),
			wordingErased: entry.wording_erased === true
		};
	});
}

async function readJson(response: Response): Promise<Record<string, unknown> | null> {
	return (await response.json().catch(() => null)) as Record<string, unknown> | null;
}

export async function fetchPreferenceView(): Promise<PreferenceView> {
	const response = await fetch(`${BASE}/intentions?${SURFACE}`, {
		headers: scopedRequestHeaders({ Accept: 'application/json' }),
		cache: 'no-store'
	});
	const body = await readJson(response);
	if (!response.ok) throw new Error(str(body?.message) ?? 'Your preferences are unavailable.');
	return parseView(body);
}

export async function fetchPreferenceHistory(id?: string): Promise<HistoryEntry[]> {
	const path = id
		? `${BASE}/intentions/${encodeURIComponent(id)}/history?${SURFACE}`
		: `${BASE}/history?${SURFACE}`;
	const response = await fetch(path, {
		headers: scopedRequestHeaders({ Accept: 'application/json' }),
		cache: 'no-store'
	});
	const body = await readJson(response);
	if (!response.ok) throw new Error(str(body?.message) ?? 'The history is unavailable.');
	return parseHistory(body);
}

async function edit(path: string, method: string, body?: unknown): Promise<EditOutcome> {
	const response = await fetch(path, {
		method,
		headers: scopedRequestHeaders({ 'Content-Type': 'application/json', Accept: 'application/json' }),
		body: body === undefined ? undefined : JSON.stringify(body)
	});
	const reply = await readJson(response);
	if (response.status === 409 && reply?.error === 'stale_preference') {
		return {
			status: 'stale',
			currentRevision: str(reply.current_revision) ?? '',
			message: str(reply.message) ?? 'This preference changed since you opened it.'
		};
	}
	if (!response.ok) throw new Error(str(reply?.message) ?? 'The change was not saved.');
	return { status: 'applied', recordId: str(reply?.record_id) ?? '' };
}

export function addPreference(input: {
	kind: IntentionKind;
	wording: string;
	label?: string;
	conditions?: string;
}): Promise<EditOutcome> {
	return edit(`${BASE}/intentions?${SURFACE}`, 'POST', {
		kind: input.kind,
		wording: input.wording,
		label: input.label || null,
		conditions: input.conditions || null
	});
}

function patch(id: string, body: Record<string, unknown>): Promise<EditOutcome> {
	return edit(`${BASE}/intentions/${encodeURIComponent(id)}?${SURFACE}`, 'PATCH', body);
}

export const editPreference = (id: string, expectedRevision: string, wording: string) =>
	patch(id, { action: 'edit', expected_revision: expectedRevision, wording });
export const confirmPreference = (id: string, expectedRevision: string) =>
	patch(id, { action: 'confirm', expected_revision: expectedRevision });
export const retirePreference = (id: string, expectedRevision: string) =>
	patch(id, { action: 'retire', expected_revision: expectedRevision });

/** Forget: the preference is deleted and its wording erased from the history. */
export function forgetPreference(id: string, expectedRevision: string): Promise<EditOutcome> {
	return edit(
		`${BASE}/intentions/${encodeURIComponent(id)}?${SURFACE}&expected_revision=${encodeURIComponent(expectedRevision)}`,
		'DELETE'
	);
}
