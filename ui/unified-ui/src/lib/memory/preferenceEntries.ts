/**
 * `/memory` Preferences pager — confirmable user-knowledge entries.
 *
 * The User Memory tab must not load the whole store. This helper is the
 * request/parse contract for `GET /memory/entries` so the page and tests
 * share offset, limit, and the owner-facing tier allowlist.
 */

import type { MemoryEntry } from './memoryEntry';

export const PREFERENCE_PAGE_SIZE = 20;
export const PREFERENCE_ENTRY_TIERS = ['preferences', 'research_findings'] as const;
export const MEMORY_ENTRIES_ENDPOINT = '/api/magician/v2/memory/entries';
export const MEMORY_EFFECT_REVIEW_ENDPOINT = '/api/magician/v2/memory/effect-review';
export const MEMORY_TABS = ['overview', 'user-memory', 'observability'] as const;
export type MemoryTabKey = (typeof MEMORY_TABS)[number];

export function memoryTabFromQuery(raw: string | null | undefined): MemoryTabKey {
	const value = (raw || '').trim();
	return MEMORY_TABS.includes(value as MemoryTabKey) ? (value as MemoryTabKey) : 'overview';
}

export interface PreferenceEntriesPage {
	entries: MemoryEntry[];
	total: number;
	page: number;
}

export class PreferenceEntriesApiError extends Error {
	status: number;

	constructor(message: string, status: number) {
		super(message);
		this.name = 'PreferenceEntriesApiError';
		this.status = status;
	}
}

export function preferencePageCount(total: number, pageSize = PREFERENCE_PAGE_SIZE): number {
	const size = Math.max(1, Math.floor(pageSize));
	return Math.max(1, Math.ceil(Math.max(0, total) / size));
}

export function clampPreferencePageIndex(
	page: number,
	total: number,
	pageSize = PREFERENCE_PAGE_SIZE
): number {
	const safePage = Number.isFinite(page) ? Math.floor(page) : 0;
	return Math.max(0, Math.min(safePage, preferencePageCount(total, pageSize) - 1));
}

export function preferenceEntriesRequest(
	page: number,
	pageSize = PREFERENCE_PAGE_SIZE
): { url: string; offset: number; limit: number; tiers: string } {
	const limit = Math.max(1, Math.floor(pageSize));
	const safePage = Math.max(0, Number.isFinite(page) ? Math.floor(page) : 0);
	const offset = safePage * limit;
	const tiers = PREFERENCE_ENTRY_TIERS.join(',');
	const params = new URLSearchParams({
		offset: String(offset),
		limit: String(limit),
		tiers
	});
	return {
		url: `${MEMORY_ENTRIES_ENDPOINT}?${params.toString()}`,
		offset,
		limit,
		tiers
	};
}

export function preferenceConfirmPath(tier: string, key: string): string {
	return `${MEMORY_ENTRIES_ENDPOINT}/${encodeURIComponent(tier)}/${encodeURIComponent(key)}/confirm`;
}

export function preferenceKeepConflictPath(tier: string, key: string): string {
	return `${MEMORY_ENTRIES_ENDPOINT}/${encodeURIComponent(tier)}/${encodeURIComponent(key)}/keep-conflict`;
}

export function preferenceScopePath(tier: string, key: string): string {
	return `${MEMORY_ENTRIES_ENDPOINT}/${encodeURIComponent(tier)}/${encodeURIComponent(key)}/scope`;
}

export type MemoryEffectAdvice =
	| 'collect_shadow_evidence'
	| 'investigate_attachment'
	| 'advance_to_canary'
	| 'stay_in_canary_collect_labels'
	| 'advance_to_enforced'
	| 'stay_enforced';

export interface MemoryEffectReview {
	compiled_mode: string;
	effective_mode: string;
	pending_hitl: boolean;
	advice: MemoryEffectAdvice | string;
	reason: string;
	next_step: string;
	observation?: {
		mode?: string;
		judgement_count?: number;
		would_suppress_count?: number;
		explain_count?: number;
		conflict_count?: number;
		unique_memory_keys?: number;
	};
}

export function memoryEffectReviewShowsBanner(review: MemoryEffectReview | null | undefined): boolean {
	if (!review) return false;
	return review.advice !== 'collect_shadow_evidence' && review.advice !== 'stay_enforced';
}

export function memoryEffectReviewCanAdvance(advice: string | undefined): boolean {
	return advice === 'advance_to_canary' || advice === 'advance_to_enforced';
}

export function memoryEffectAdvanceLabel(advice: string | undefined): string {
	if (advice === 'advance_to_enforced') return 'Switch to Enforced';
	if (advice === 'advance_to_canary') return 'Switch to Canary';
	return 'Advance';
}

export function preferencePagerView(
	page: number,
	total: number,
	loadedCount: number,
	pageSize = PREFERENCE_PAGE_SIZE
): { page: number; pageCount: number; startItem: number; endItem: number } {
	const clamped = clampPreferencePageIndex(page, total, pageSize);
	const size = Math.max(1, Math.floor(pageSize));
	const pageCount = preferencePageCount(total, size);
	const startItem = total === 0 ? 0 : clamped * size + 1;
	const endItem = Math.min(total, clamped * size + Math.max(0, loadedCount));
	return { page: clamped, pageCount, startItem, endItem };
}

function asRecord(value: unknown): Record<string, unknown> | null {
	return typeof value === 'object' && value !== null && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function asString(value: unknown): string {
	return typeof value === 'string' ? value : '';
}

function parseScope(value: unknown): MemoryEntry['scope'] {
	const rec = asRecord(value);
	if (!rec) return null;
	const list = (field: string): string[] =>
		Array.isArray(rec[field])
			? rec[field].filter((item): item is string => typeof item === 'string' && item.trim().length > 0)
			: [];
	return {
		topics: list('topics'),
		entities: list('entities'),
		applies_to: list('applies_to')
	};
}

export function parsePreferenceEntry(value: unknown): MemoryEntry | null {
	const rec = asRecord(value);
	if (!rec) return null;
	const tier = asString(rec.tier).trim();
	const key = asString(rec.key).trim();
	const source_type = asString(rec.source_type).trim();
	const trust = asString(rec.trust).trim();
	const kind = asString(rec.kind).trim();
	if (!tier || !key || !source_type || !trust || !kind) return null;
	const entry: MemoryEntry = { tier, key, source_type, trust, kind };
	if ('value' in rec) entry.value = rec.value;
	if ('scope' in rec) entry.scope = parseScope(rec.scope);
	if (typeof rec.updated_at === 'string' || rec.updated_at === null) {
		entry.updated_at = rec.updated_at;
	}
	if (typeof rec.confirmed_from === 'string' || rec.confirmed_from === null) {
		entry.confirmed_from = rec.confirmed_from;
	}
	if (typeof rec.may_explain === 'boolean') entry.may_explain = rec.may_explain;
	if (typeof rec.conflict === 'string' && rec.conflict.trim()) {
		entry.conflict = rec.conflict;
	}
	if (typeof rec.conflict_agree === 'number' && Number.isFinite(rec.conflict_agree)) {
		entry.conflict_agree = rec.conflict_agree;
	}
	if (typeof rec.conflict_disagree === 'number' && Number.isFinite(rec.conflict_disagree)) {
		entry.conflict_disagree = rec.conflict_disagree;
	}
	return entry;
}

export function parsePreferenceEntriesPage(payload: unknown): { entries: MemoryEntry[]; total: number } {
	const rec = asRecord(payload);
	const rawEntries = Array.isArray(rec?.entries) ? rec.entries : [];
	const entries = rawEntries
		.map(parsePreferenceEntry)
		.filter((entry): entry is MemoryEntry => entry !== null);
	const total =
		typeof rec?.total === 'number' && Number.isFinite(rec.total)
			? Math.max(0, Math.floor(rec.total))
			: entries.length;
	return { entries, total };
}

export async function fetchPreferenceEntriesPage(
	page: number,
	options: {
		fetchImpl?: typeof fetch;
		pageSize?: number;
		readError?: (response: Response) => Promise<string>;
	} = {}
): Promise<PreferenceEntriesPage> {
	const fetchImpl = options.fetchImpl ?? fetch;
	const pageSize = options.pageSize ?? PREFERENCE_PAGE_SIZE;

	const load = async (targetPage: number): Promise<{ entries: MemoryEntry[]; total: number }> => {
		const { url } = preferenceEntriesRequest(targetPage, pageSize);
		const response = await fetchImpl(url);
		if (!response.ok) {
			const message = options.readError
				? await options.readError(response)
				: `Request failed (${response.status})`;
			throw new PreferenceEntriesApiError(message, response.status);
		}
		return parsePreferenceEntriesPage(await response.json());
	};

	const first = await load(page);
	const clamped = clampPreferencePageIndex(page, first.total, pageSize);
	if (clamped === page) return { ...first, page: clamped };
	const second = await load(clamped);
	return { ...second, page: clamped };
}
