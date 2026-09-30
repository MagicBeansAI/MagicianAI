import type { FeedItem } from '$lib/feed/types';

export interface LearningEvidenceRef {
	kind?: string;
	id?: string | null;
	path?: string | null;
	uri?: string | null;
	summary?: string | null;
}

export function metadataRecord(item: FeedItem): Record<string, unknown> {
	return typeof item.metadata === 'object' && item.metadata && !Array.isArray(item.metadata)
		? (item.metadata as Record<string, unknown>)
		: {};
}

export function readMetadataString(item: FeedItem, key: string): string | null {
	const value = metadataRecord(item)[key];
	return typeof value === 'string' && value.trim().length > 0 ? value : null;
}

export function readMetadataValue(item: FeedItem, key: string): unknown {
	return metadataRecord(item)[key];
}

export function agentLearningSourceUrl(rawEntry: unknown): string | null {
	if (!rawEntry || typeof rawEntry !== 'object' || Array.isArray(rawEntry)) return null;
	const entry = rawEntry as Record<string, unknown>;
	for (const key of ['sources', 'source_urls', 'source_url', 'url']) {
		const value = entry[key];
		if (typeof value === 'string' && value.trim()) return value.trim();
		if (Array.isArray(value)) {
			const first = value.find((candidate): candidate is string =>
				typeof candidate === 'string' && candidate.trim().length > 0
			);
			if (first) return first.trim();
		}
		if (value && typeof value === 'object') {
			const record = value as Record<string, unknown>;
			for (const field of ['url', 'source_url', 'summary']) {
				const candidate = record[field];
				if (typeof candidate === 'string' && candidate.trim()) return candidate.trim();
			}
		}
	}
	return null;
}

export function metadataValueLabel(value: unknown): string | null {
	if (typeof value === 'string') return value.trim() || null;
	if (typeof value === 'number' || typeof value === 'boolean') return String(value);
	if (value && typeof value === 'object') {
		try {
			return JSON.stringify(value);
		} catch {
			return null;
		}
	}
	return null;
}

export function titleCase(value: string | null | undefined): string {
	if (!value) return 'Unknown';
	return value.replace(/_/g, ' ').replace(/\b\w/g, (match) => match.toUpperCase());
}

export function formatRelative(value: number | null | undefined): string {
	if (!value) return 'just now';
	const diffMs = Date.now() - value;
	const minutes = Math.round(diffMs / 60_000);
	if (Math.abs(minutes) < 1) return 'just now';
	if (Math.abs(minutes) < 60) return `${Math.abs(minutes)}m ago`;
	const hours = Math.round(minutes / 60);
	if (Math.abs(hours) < 48) return `${Math.abs(hours)}h ago`;
	const days = Math.round(hours / 24);
	return `${Math.abs(days)}d ago`;
}

export function learningCandidateId(item: FeedItem): string | null {
	return (
		readMetadataString(item, 'candidate_id')
		|| (item.id.startsWith('learning_candidate:') ? item.id.slice('learning_candidate:'.length) : null)
	);
}

export function learningInsightId(item: FeedItem): string {
	return readMetadataString(item, 'insight_id') || item.id;
}

export function learningTargetLabel(item: FeedItem): string {
	const scope = readMetadataString(item, 'target_scope') || 'user';
	const tier = readMetadataString(item, 'target_tier') || 'memory';
	return `${titleCase(scope)} ${titleCase(tier)}`;
}

export function learningValueLabel(item: FeedItem): string {
	return (
		readMetadataString(item, 'memory_value_label')
		|| metadataValueLabel(readMetadataValue(item, 'memory_value'))
		|| item.summary
		|| item.title
	);
}

export function insightKindLabel(item: FeedItem): string {
	return titleCase(readMetadataString(item, 'insight_kind') || readMetadataString(item, 'source_type') || 'insight');
}

export function insightWhyLabel(item: FeedItem): string | null {
	return readMetadataString(item, 'why_it_matters');
}

export function confidenceLabel(item: FeedItem): string | null {
	const confidence = readMetadataValue(item, 'confidence');
	if (typeof confidence === 'number') return `${Math.round(confidence * 100)}%`;
	if (typeof confidence === 'string' && confidence.trim()) return confidence.trim();
	return null;
}

export function learningEvidenceRefs(item: FeedItem): LearningEvidenceRef[] {
	const refs = readMetadataValue(item, 'evidence_refs');
	if (!Array.isArray(refs)) return [];
	return refs
		.filter((ref): ref is Record<string, unknown> => typeof ref === 'object' && ref !== null)
		.map((ref) => ({
			kind: typeof ref.kind === 'string' ? ref.kind : undefined,
			id: typeof ref.id === 'string' ? ref.id : null,
			path: typeof ref.path === 'string' ? ref.path : null,
			uri: typeof ref.uri === 'string' ? ref.uri : null,
			summary: typeof ref.summary === 'string' ? ref.summary : null
		}));
}

export function learningEvidenceCount(item: FeedItem): number {
	return learningEvidenceRefs(item).length;
}

export function learningSourceLabel(item: FeedItem): string | null {
	const source = readMetadataString(item, 'source_type');
	const sourceId = readMetadataString(item, 'source_id');
	if (source && sourceId) return `${titleCase(source)} ${sourceId}`;
	if (source) return titleCase(source);
	return null;
}

export function learningEvidenceHref(item: FeedItem): string {
	const firstUri = learningEvidenceRefs(item)
		.map((ref) => ref.uri?.trim())
		.find((uri): uri is string => Boolean(uri));
	if (firstUri) return firstUri;
	const sourceTaskId = item.task_id || readMetadataString(item, 'source_task_id');
	if (sourceTaskId) return `/tasks?filter=all&selected=${encodeURIComponent(sourceTaskId)}`;
	const sourceThreadId = item.ui_thread_id || readMetadataString(item, 'source_chat_session_id');
	if (sourceThreadId) {
		return `/t/${encodeURIComponent(sourceThreadId)}?selected_item=${encodeURIComponent(item.id)}`;
	}
	return `/feed?selected_item=${encodeURIComponent(item.id)}`;
}

export function evidenceHrefIsExternal(href: string): boolean {
	return /^[a-z][a-z0-9+.-]*:/i.test(href);
}

export function candidateReviewLabel(item: FeedItem): string {
	const candidateType = readMetadataString(item, 'candidate_type') || 'learning';
	switch (candidateType) {
		case 'memory_fact':
		case 'memory_preference':
			return 'Memory review';
		case 'evaluation_case':
			return 'Evaluation review';
		case 'skill_update':
		case 'workflow_template':
		case 'memory_procedure':
			return 'Procedure review';
		case 'tool_wrapper_fix':
		case 'capability_update':
		case 'tool_schema_update':
			return 'Tooling review';
		default:
			return `${titleCase(candidateType)} review`;
	}
}

export function insightActionLabel(item: FeedItem): string {
	const sourceType = readMetadataString(item, 'source_type');
	const candidateType = readMetadataString(item, 'candidate_type');
	if (candidateType === 'evaluation_case' || sourceType === 'learning_evaluation_backlog') {
		return 'Queue follow-up';
	}
	if (candidateType === 'tool_wrapper_fix' || candidateType === 'tool_schema_update') {
		return 'Create repair task';
	}
	if (candidateType === 'skill_update' || candidateType === 'workflow_template') {
		return 'Create skill task';
	}
	return 'Create task';
}
