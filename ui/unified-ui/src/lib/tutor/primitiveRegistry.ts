/**
 * Client-side registry of which tutor primitives exist and how each renders.
 * Mirrors `magios/Shared/TutorPrimitiveRegistry.swift`: fetches the merged
 * scoped recipe set from the backend, caches it in memory, and exposes
 * synchronous `recipe(for:)` / `isSupported(_:)` lookups with alias matching.
 *
 * A new primitive is a new recipe file served by the backend — no app rebuild.
 * On any fetch failure the current in-memory set is kept (never downgraded).
 */

import { appendCurrentScopeQuery } from '$lib/stores/scopeIdentityStore';
import { decodeRecipes, matchedTypes, type TutorRecipe } from './recipeTypes';

const MAX_RECIPES = 512; // hostile-input cap (Appendix A)
const ENDPOINT = '/api/magician/v2/tutor/primitives';

export class PrimitiveRegistry {
	private byType = new Map<string, TutorRecipe>();
	private fetchedOnce = false;

	/** Build an empty registry (production) or one seeded with an explicit set (tests). */
	constructor(recipes?: TutorRecipe[]) {
		if (recipes) this.index(recipes);
	}

	/** The recipe for a shape `type`, matching the primary `type` or any alias. */
	recipe(type: string): TutorRecipe | undefined {
		return this.byType.get(type.toLowerCase());
	}

	/** Whether any loaded recipe renders this `type`. */
	isSupported(type: string): boolean {
		return this.byType.has(type.toLowerCase());
	}

	/** Whether an initial fetch has completed (recipes may still be empty). */
	get hasFetched(): boolean {
		return this.fetchedOnce;
	}

	/** All loaded recipes (dedup'd by primary type). */
	get recipes(): TutorRecipe[] {
		const seen = new Set<string>();
		const out: TutorRecipe[] = [];
		for (const recipe of this.byType.values()) {
			const key = recipe.type.toLowerCase();
			if (seen.has(key)) continue;
			seen.add(key);
			out.push(recipe);
		}
		return out;
	}

	/** Replace the in-memory set from a decoded recipe list (alias-indexed). */
	index(recipes: TutorRecipe[]): void {
		const map = new Map<string, TutorRecipe>();
		for (const recipe of recipes.slice(0, MAX_RECIPES)) {
			for (const key of matchedTypes(recipe)) map.set(key, recipe);
		}
		this.byType = map;
	}

	/**
	 * Fetch the merged scoped recipe set from the backend and adopt it. On any
	 * failure (network, non-2xx, malformed, empty) the current set is kept.
	 * Returns true when a non-empty set was adopted. Uses the app's scoped fetch,
	 * which injects bearer auth for `/api/magician/` requests.
	 */
	async refresh(fetchImpl: typeof fetch = fetch): Promise<boolean> {
		const params = appendCurrentScopeQuery();
		const url = `${ENDPOINT}?${params.toString()}`;
		try {
			const response = await fetchImpl(url, {
				method: 'GET',
				headers: { Accept: 'application/json' }
			});
			// A completed round-trip counts as "fetched" (so a future readiness gate
			// on hasFetched isn't stuck forever on an empty/non-2xx first response);
			// only a network exception leaves it false.
			this.fetchedOnce = true;
			if (!response.ok) return false;
			const json: unknown = await response.json();
			const recipes = decodeRecipes(json);
			if (recipes.length === 0) return false;
			this.index(recipes);
			return true;
		} catch {
			return false;
		}
	}
}

/** The shared registry used by the draw-overlay. */
export const tutorPrimitiveRegistry = new PrimitiveRegistry();
