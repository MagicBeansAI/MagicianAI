import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
import type { AppMemoryReadGrant, AppMemoryReadSelection } from './installationReview';

export type AppMemoryTierReadability = 'ordinary' | 'sensitive';

export interface AppMemoryAccess {
	installation_id: string;
	/** `null` when the app requested no owner memory. */
	request: { user_tiers: string[]; agents: string[]; purpose: string } | null;
	reviewed: AppMemoryReadGrant | null;
	/** What the app can read right now; `null` means nothing. */
	effective: AppMemoryReadGrant | null;
	edit_revision: number;
	installation_enabled: boolean;
	user_tier_catalog: Array<{ name: string; readability: AppMemoryTierReadability }>;
}

function accessUrl(installationId: string): string {
	return `/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/memory-access`;
}

function errorFrom(body: unknown, fallback: string): Error {
	if (body && typeof body === 'object' && 'message' in body && typeof body.message === 'string') {
		return new Error(body.message);
	}
	return new Error(fallback);
}

function strings(value: unknown): string[] | null {
	return Array.isArray(value) && value.every((item) => typeof item === 'string')
		? [...value]
		: null;
}

function selection(value: unknown): AppMemoryReadSelection | null {
	if (!value || typeof value !== 'object') return null;
	const item = value as Record<string, unknown>;
	const userTiers = strings(item.user_tiers ?? []);
	const agents = strings(item.agents ?? []);
	return userTiers && agents ? { user_tiers: userTiers, agents } : null;
}

function grant(value: unknown): AppMemoryReadGrant | null | undefined {
	if (value === null || value === undefined) return null;
	if (typeof value !== 'object') return undefined;
	const item = value as Record<string, unknown>;
	const interactive = selection(item.interactive);
	const background = selection(item.background);
	if (
		!interactive || !background || typeof item.schema !== 'string' ||
		typeof item.request_digest !== 'string' || item.engagement !== 'owner_only'
	) {
		return undefined;
	}
	return {
		schema: item.schema,
		request_digest: item.request_digest,
		engagement: 'owner_only',
		interactive,
		background
	};
}

/** Strict parse; throws rather than rendering a partially understood grant. */
export function parseAppMemoryAccess(body: unknown): AppMemoryAccess {
	const invalid = () => new Error('The memory access response was not understood.');
	if (!body || typeof body !== 'object') throw invalid();
	const item = body as Record<string, unknown>;
	let request: AppMemoryAccess['request'] = null;
	if (item.request !== null && item.request !== undefined) {
		const raw = item.request as Record<string, unknown>;
		const userTiers = strings(raw.user_tiers ?? []);
		const agents = strings(raw.agents ?? []);
		if (!userTiers || !agents || typeof raw.purpose !== 'string') throw invalid();
		request = { user_tiers: userTiers, agents, purpose: raw.purpose };
	}
	const reviewed = grant(item.reviewed);
	const effective = grant(item.effective);
	const catalog = Array.isArray(item.user_tier_catalog) ? item.user_tier_catalog : null;
	if (
		reviewed === undefined || effective === undefined || !catalog ||
		typeof item.installation_id !== 'string' || typeof item.edit_revision !== 'number' ||
		typeof item.installation_enabled !== 'boolean'
	) {
		throw invalid();
	}
	return {
		installation_id: item.installation_id,
		request,
		reviewed,
		effective,
		edit_revision: item.edit_revision,
		installation_enabled: item.installation_enabled,
		user_tier_catalog: catalog.flatMap((entry) => {
			const tier = entry as Record<string, unknown>;
			return typeof tier.name === 'string' &&
				(tier.readability === 'ordinary' || tier.readability === 'sensitive')
				? [{ name: tier.name, readability: tier.readability }]
				: [];
		})
	};
}

export async function fetchAppMemoryAccess(
	installationId: string,
	signal?: AbortSignal
): Promise<AppMemoryAccess> {
	const response = await fetch(accessUrl(installationId), {
		headers: scopedRequestHeaders({ Accept: 'application/json' }),
		signal
	});
	const body = await response.json().catch(() => null);
	if (!response.ok) throw errorFrom(body, 'Memory access could not be loaded.');
	return parseAppMemoryAccess(body);
}

export async function updateAppMemoryAccess(
	installationId: string,
	expectedEditRevision: number,
	interactive: AppMemoryReadSelection,
	background: AppMemoryReadSelection,
	signal?: AbortSignal
): Promise<AppMemoryAccess> {
	const response = await fetch(accessUrl(installationId), {
		method: 'POST',
		headers: scopedRequestHeaders({ Accept: 'application/json', 'Content-Type': 'application/json' }),
		body: JSON.stringify({
			expected_edit_revision: expectedEditRevision,
			interactive,
			background
		}),
		signal
	});
	const body = await response.json().catch(() => null);
	if (!response.ok) throw errorFrom(body, 'Memory access could not be saved.');
	return parseAppMemoryAccess(body);
}
