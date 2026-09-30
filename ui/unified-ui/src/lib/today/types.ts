import type { FeedAction, FeedItemStatus } from '$lib/feed/types';

export type TodaySectionId =
	| 'needs_you'
	| 'delivered'
	| 'changed'
	| 'active_work'
	| 'followups';

export interface TodayItem {
	id: string;
	principal: string;
	workspace: string;
	section: TodaySectionId;
	priority: number;
	title: string;
	summary?: string | null;
	reason: string;
	source_kind: string;
	source_id: string;
	source_url?: string | null;
	space_ids: string[];
	thread_id?: string | null;
	task_id?: string | null;
	agent_id?: string | null;
	status: FeedItemStatus;
	actions: FeedAction[];
	evidence_refs: unknown[];
	created_at: number;
	updated_at: number;
	expires_at?: number | null;
	seen_at?: number | null;
	dismissed_at?: number | null;
	snoozed_until?: number | null;
	metadata: unknown;
}

export interface TodaySections {
	needs_you: TodayItem[];
	delivered: TodayItem[];
	changed: TodayItem[];
	active_work: TodayItem[];
	followups: TodayItem[];
}

export interface TodayCounts {
	needs_you: number;
	delivered: number;
	changed: number;
	active_work: number;
	followups: number;
	total: number;
}

export interface TodayFreshness {
	source: string;
	generated_at: number;
}

export interface TodayDigestBullet {
	id: string;
	text: string;
	source_kind: string;
	source_id: string;
	source_url?: string | null;
	space_ids: string[];
	updated_at: number;
}

export interface TodayChangedDigest {
	generated_at: number;
	since?: number | null;
	total: number;
	limit: number;
	offset: number;
	bullets: TodayDigestBullet[];
}

export interface TodaySectionPage {
	section: TodaySectionId;
	total: number;
	limit: number;
	cursor?: string | null;
	next_cursor?: string | null;
	has_more: boolean;
}

export interface TodayResponse {
	principal: string;
	workspace: string;
	generated_at: number;
	freshness: TodayFreshness;
	headline: string;
	digest: TodayChangedDigest;
	sections: TodaySections;
	section_page?: TodaySectionPage | null;
	counts: TodayCounts;
}

export interface TodayVisibilitySnapshot {
	title: string;
	summary?: string | null;
	reason: string;
	section: TodaySectionId | string;
	source_kind: string;
	source_id: string;
	source_url?: string | null;
	space_ids: string[];
	item_updated_at: number;
}

export interface TodayVisibilityRecord {
	seen_at?: number | null;
	dismissed_at?: number | null;
	snoozed_until?: number | null;
	snapshot?: TodayVisibilitySnapshot | null;
	updated_at: number;
}

export interface TodayVisibilityListItem {
	item_id: string;
	hidden_kind: 'dismissed' | 'snoozed' | string;
	record: TodayVisibilityRecord;
}

export interface TodayVisibilityListResponse {
	items: TodayVisibilityListItem[];
}
