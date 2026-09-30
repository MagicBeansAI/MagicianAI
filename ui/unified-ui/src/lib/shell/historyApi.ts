import { get } from 'svelte/store';

import type { ChatSession } from '$lib/stores/chatStore';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import type { UiThreadRecord } from '$lib/threads/types';
import { timedFetch } from '$lib/shared/fetch';

export type HistoryLane = 'personal' | 'automated';

export interface HistoryPage<T> {
	items: T[];
	total: number;
	limit: number;
	offset: number;
}

export type HistorySearchItem =
	| {
			kind: 'session';
			history_lane: HistoryLane;
			session: ChatSession;
	  }
	| {
			kind: 'thread';
			history_lane: HistoryLane;
			thread: UiThreadRecord;
	  };

interface HistoryPageQuery {
	lane?: HistoryLane;
	search: string;
	limit: number;
	offset: number;
	threadId?: string | null;
}

function scopedParams(query: HistoryPageQuery): URLSearchParams {
	const params = new URLSearchParams({
		limit: String(query.limit),
		offset: String(query.offset)
	});
	if (query.lane) params.set('history_lane', query.lane);
	if (query.search.trim()) params.set('q', query.search.trim());
	if (query.threadId?.trim()) params.set('ui_thread_id', query.threadId.trim());
	return params;
}

async function requestJson<T>(url: string): Promise<T> {
	const response = await timedFetch(url);
	if (!response.ok) {
		const detail = await response.text().catch(() => '');
		throw new Error(detail || `History request failed (${response.status})`);
	}
	return response.json() as Promise<T>;
}

export async function fetchSessionHistory(
	query: HistoryPageQuery
): Promise<HistoryPage<ChatSession>> {
	const payload = await requestJson<{
		sessions: ChatSession[];
		total: number;
		limit: number;
		offset: number;
	}>(`/api/magician/v2/chat/sessions?${scopedParams(query)}`);
	return {
		items: payload.sessions,
		total: Number.isFinite(payload.total) ? payload.total : payload.sessions.length,
		limit: Number.isFinite(payload.limit) ? payload.limit : query.limit,
		offset: Number.isFinite(payload.offset) ? payload.offset : query.offset
	};
}

export async function fetchThreadHistory(
	query: HistoryPageQuery
): Promise<HistoryPage<UiThreadRecord>> {
	const payload = await requestJson<{
		threads: UiThreadRecord[];
		total: number;
		limit: number;
		offset: number;
	}>(`/api/magician/v2/ui-threads?${scopedParams(query)}`);
	return {
		items: payload.threads,
		total: Number.isFinite(payload.total) ? payload.total : payload.threads.length,
		limit: Number.isFinite(payload.limit) ? payload.limit : query.limit,
		offset: Number.isFinite(payload.offset) ? payload.offset : query.offset
	};
}

export async function fetchHistorySearch(query: {
	search: string;
	limit: number;
	offset: number;
}): Promise<HistoryPage<HistorySearchItem>> {
	const payload = await requestJson<{
		items: HistorySearchItem[];
		total: number;
		limit: number;
		offset: number;
	}>(`/api/magician/v2/history/search?${scopedParams(query)}`);
	return {
		items: payload.items,
		total: Number.isFinite(payload.total) ? payload.total : payload.items.length,
		limit: Number.isFinite(payload.limit) ? payload.limit : query.limit,
		offset: Number.isFinite(payload.offset) ? payload.offset : query.offset
	};
}
