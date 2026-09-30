export type Relevance =
	| 'answer_bearing'
	| 'dependency'
	| 'first_party_api'
	| 'third_party_api'
	| 'telemetry'
	| 'unclassified';

export type CapabilityOverview = {
	id: string;
	name: string;
	origin?: string;
	parent_origin?: string | null;
	relevance: Relevance;
	used_by_recipe_ids?: string[];
};

export type RecipeOverview = {
	id: string;
	template: string;
	last_replayed_at_ms?: number | null;
};

export type SiteOverview = {
	site: string;
	capabilities: CapabilityOverview[];
	telemetry_hidden: number;
};

export const DEFAULT_RELEVANCE = new Set<Relevance>([
	'answer_bearing',
	'dependency',
	'first_party_api'
]);

export function applyRelevanceFilter<T extends CapabilityOverview>(
	capabilities: T[],
	shown: ReadonlySet<Relevance>
): T[] {
	return capabilities.filter((capability) => shown.has(capability.relevance));
}

export function groupBySite<T extends CapabilityOverview>(capabilities: T[]): SiteOverview[] {
	const groups = new Map<string, SiteOverview>();
	for (const capability of capabilities) {
		const origin = capability.parent_origin || capability.origin || 'unknown';
		let site = origin;
		try {
			site = new URL(origin).hostname || origin;
		} catch {
			// Preserve a stable, human-readable group for legacy origin values.
		}
		const group = groups.get(site) ?? { site, capabilities: [], telemetry_hidden: 0 };
		if (capability.relevance === 'telemetry') group.telemetry_hidden += 1;
		else group.capabilities.push(capability);
		groups.set(site, group);
	}
	return [...groups.values()].sort((left, right) => left.site.localeCompare(right.site));
}

export function recipeRows<T extends RecipeOverview>(recipes: T[]): T[] {
	return [...recipes].sort(
		(left, right) =>
			(right.last_replayed_at_ms ?? 0) - (left.last_replayed_at_ms ?? 0) ||
			left.template.localeCompare(right.template) ||
			left.id.localeCompare(right.id)
	);
}

/** Replay the version the user inspected, never an unseen recompiled graph. */
export function recipeReplayUrl(recipeId: string, version: number): string {
	if (!Number.isSafeInteger(version) || version < 1) throw new Error('Load recipe details before replaying');
	return `/api/magician/v2/api-mining/recipes/${encodeURIComponent(recipeId)}/replay?expected_version=${version}`;
}

export function recipeReplayUncertainty(body: unknown): string | null {
	if (body && typeof body === 'object' && 'effect_uncertain' in body && body.effect_uncertain === true) {
		return 'Do not retry this write through the API or browser. Check the destination to determine whether it completed.';
	}
	return null;
}

export function staleRecipeDetailIds(
	recipes: Array<{ id: string; version: number }>,
	details: Record<string, { current_version: number }>
): string[] {
	const versions = new Map(recipes.map((recipe) => [recipe.id, recipe.version]));
	return Object.entries(details)
		.filter(([id, detail]) => versions.get(id) !== detail.current_version)
		.map(([id]) => id);
}

export function recipeReplayTone(status: number, body: unknown): 'success' | 'warning' | 'error' {
	if (status === 409 || recipeReplayUncertainty(body)) return 'warning';
	return status >= 200 && status < 300 && body !== null && typeof body === 'object'
		&& 'success' in body && body.success === true ? 'success' : 'error';
}

export function recipeRunTone(run: { rail_ended: string; failure_class?: string | null }): 'success' | 'warning' | 'error' {
	if (run.rail_ended !== 'api') return 'warning';
	return run.failure_class ? 'error' : 'success';
}

/** One submission only. A missing write response is not evidence of failure. */
export async function submitRecipeReplay(
	request: { id: string; version: number; inputs: Record<string, string>; hasWriteSteps: boolean },
	send: (url: string, init: RequestInit) => Promise<Pick<Response, 'status' | 'json'>>
): Promise<{ status: number; body: unknown }> {
	const url = recipeReplayUrl(request.id, request.version);
	try {
		const response = await send(url, {
			method: 'POST', headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ inputs: request.inputs })
		});
		const body: unknown = await response.json();
		if (!body || typeof body !== 'object') throw new Error('Replay response was unreadable');
		// A gateway can return valid JSON after losing the backend response;
		// that error envelope still says nothing about the write's outcome.
		if (request.hasWriteSteps && ((response.status >= 200 && response.status < 300) || response.status >= 500)
			&& (!('success' in body) || typeof body.success !== 'boolean')) {
			throw new Error('Replay response omitted the execution outcome');
		}
		return { status: response.status, body };
	} catch (error) {
		if (!request.hasWriteSteps) throw error;
		return { status: 0, body: {
			effect_uncertain: true, retryable: false, browser_retry_allowed: false,
			message: 'The write response was lost. Check the destination before taking further action.'
		} };
	}
}
