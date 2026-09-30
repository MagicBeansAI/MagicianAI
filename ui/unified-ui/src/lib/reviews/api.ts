import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

const EVIDENCE_BASE = '/api/magician/v2/evidence';

export interface ImpactReviewEntry {
	namespace: string;
	name: string;
	agent: string | null;
	last_updated: string | null;
	stale?: boolean;
	cited_count?: number;
}

export interface ImpactReviewVerification {
	grounded: boolean;
	ungrounded_claims: string[];
	citation_coverage: number;
	uncited_bullets: number;
	total_bullets: number;
	cited_ids: string[];
	notes: string;
}

export interface ImpactLabelCount {
	label: string;
	count: number;
}

export interface ImpactTopEntity {
	name: string;
	entity_type: string;
	sources: number;
}

export interface ImpactRecentItem {
	summary: string;
	kind: string;
	facets: string;
	when: string;
}

export interface ImpactDashboardData {
	total_evidence: number;
	active_entities: number;
	facets_tracked: number;
	reviews_generated: number;
	facet_coverage: ImpactLabelCount[];
	entity_types: ImpactLabelCount[];
	top_entities: ImpactTopEntity[];
	weekly_activity: ImpactLabelCount[];
	recent_evidence: ImpactRecentItem[];
	visibility_gaps: string[];
}

export interface ImpactReviewRequest {
	agent: string;
	days: number;
	facet: string;
}

export interface GeneratedImpactReview {
	markdown: string;
	verification: ImpactReviewVerification | null;
	artifact_name: string | null;
	evidence_count: number | null;
}

export type ImpactReviewVerdict = 'accepted' | 'edited' | 'discarded';

async function responseBody(response: Response): Promise<Record<string, unknown> | null> {
	try {
		const value = await response.json();
		return value && typeof value === 'object' && !Array.isArray(value)
			? (value as Record<string, unknown>)
			: null;
	} catch {
		return null;
	}
}

function responseError(
	response: Response,
	body: Record<string, unknown> | null,
	fallback: string
): Error {
	const message =
		typeof body?.message === 'string'
			? body.message
			: typeof body?.error === 'string'
				? body.error
				: `${fallback} (${response.status})`;
	return new Error(message);
}

export async function listImpactReviews(): Promise<ImpactReviewEntry[]> {
	const response = await fetch(`${EVIDENCE_BASE}/reviews`, {
		headers: scopedRequestHeaders()
	});
	const body = await responseBody(response);
	if (!response.ok) throw responseError(response, body, 'Failed to load reviews');
	return Array.isArray(body?.reviews) ? (body.reviews as ImpactReviewEntry[]) : [];
}

export async function generateImpactReview(
	request: ImpactReviewRequest
): Promise<GeneratedImpactReview> {
	const response = await fetch(`${EVIDENCE_BASE}/review`, {
		method: 'POST',
		headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
		body: JSON.stringify(request)
	});
	const body = await responseBody(response);
	if (!response.ok) throw responseError(response, body, 'Failed to generate review');
	return {
		markdown: typeof body?.markdown === 'string' ? body.markdown : '',
		verification:
			body?.verification && typeof body.verification === 'object'
				? (body.verification as unknown as ImpactReviewVerification)
				: null,
		artifact_name: typeof body?.artifact_name === 'string' ? body.artifact_name : null,
		evidence_count: typeof body?.evidence_count === 'number' ? body.evidence_count : null
	};
}

export async function fetchImpactReviewArtifact(name: string): Promise<string> {
	const response = await fetch(
		`/api/magician/v2/artifacts/durable/evidence-reviews/${encodeURIComponent(name)}`,
		{ headers: scopedRequestHeaders() }
	);
	const body = await responseBody(response);
	if (!response.ok) throw responseError(response, body, 'Could not open that review');
	return typeof body?.content === 'string' ? body.content : '';
}

export async function recordImpactReviewFeedback(
	review: string,
	verdict: ImpactReviewVerdict
): Promise<void> {
	const response = await fetch(`${EVIDENCE_BASE}/review/feedback`, {
		method: 'POST',
		headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
		body: JSON.stringify({ review, verdict })
	});
	const body = await responseBody(response);
	if (!response.ok) throw responseError(response, body, 'Could not record feedback');
}

export async function fetchImpactReviewUtility(): Promise<number | null> {
	const response = await fetch(`${EVIDENCE_BASE}/utility`, {
		headers: scopedRequestHeaders()
	});
	const body = await responseBody(response);
	if (!response.ok) throw responseError(response, body, 'Could not load review utility');
	return typeof body?.utility_rate === 'number' ? body.utility_rate : null;
}

export async function fetchImpactDashboard(
	request: ImpactReviewRequest
): Promise<ImpactDashboardData> {
	const params = new URLSearchParams({
		agent: request.agent,
		facet: request.facet,
		days: String(request.days)
	});
	const response = await fetch(`${EVIDENCE_BASE}/dashboard?${params}`, {
		headers: scopedRequestHeaders()
	});
	const body = await responseBody(response);
	if (!response.ok) throw responseError(response, body, 'Failed to load dashboard');
	if (!body) throw new Error('Failed to load dashboard: empty response');
	return body as unknown as ImpactDashboardData;
}

export async function publishImpactDashboard(
	request: ImpactReviewRequest
): Promise<{ surface_id: string; route: string }> {
	const response = await fetch(`${EVIDENCE_BASE}/dashboard/publish`, {
		method: 'POST',
		headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
		body: JSON.stringify(request)
	});
	const body = await responseBody(response);
	if (!response.ok) throw responseError(response, body, 'Publish failed');
	return {
		surface_id: typeof body?.surface_id === 'string' ? body.surface_id : '',
		route: typeof body?.route === 'string' ? body.route : '/briefing'
	};
}

export function reviewRegenerationDefaults(
	entry: Pick<ImpactReviewEntry, 'name' | 'agent'>,
	currentDays: number
): { facet?: string; days: number; agent?: string } {
	const match = entry.name.match(/^(.*)-(\d+)d-/);
	return {
		facet: match?.[1],
		days: match ? Number.parseInt(match[2], 10) || currentDays : currentDays,
		agent: entry.agent ?? undefined
	};
}
