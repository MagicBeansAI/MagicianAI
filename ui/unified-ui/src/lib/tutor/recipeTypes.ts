/**
 * TypeScript recipe types mirroring `magios/Shared/TutorRecipe.swift`.
 *
 * A recipe is declarative geometry data: which shape `type` (plus `aliases`) it
 * renders, optional `defaults` for absent fields, and an ordered `draw` list of
 * ops. `decodeRecipes` accepts either a bare `[...]` array or the endpoint's
 * `{ primitives: [...] }` envelope, and is deliberately lenient so a novel or
 * malformed recipe never throws (it is skipped instead).
 */

/**
 * A recipe value in an op's params bag. A param may be a bare number, a string
 * (a field-ref / coalesce-chain / expression, or a plain literal like `text`),
 * a boolean (`fill: true`), or a (possibly nested) array — nested arrays hold
 * `[x, y]` point pairs or a `points` list.
 */
export type RecipeValue = number | string | boolean | RecipeValue[];

/** One draw-op: an `op` name plus a flexible params bag. */
export interface RecipeOp {
	op: string;
	params: Record<string, RecipeValue>;
}

/** A declarative primitive recipe. */
export interface TutorRecipe {
	type: string;
	aliases: string[];
	version?: number;
	defaults: Record<string, number>;
	draw: RecipeOp[];
}

/**
 * All `type` strings this recipe answers to (primary + aliases), lowercased.
 * Mirrors Swift `TutorRecipe.matchedTypes`.
 */
export function matchedTypes(recipe: TutorRecipe): string[] {
	return [recipe.type, ...recipe.aliases].map((t) => t.toLowerCase());
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function decodeRecipeValue(raw: unknown): RecipeValue | undefined {
	// A JSON boolean decodes as-is (the interpreter's `bool` accessor reads it),
	// matching Swift where bool is checked before number.
	if (typeof raw === 'boolean') return raw;
	if (typeof raw === 'number' && Number.isFinite(raw)) return raw;
	if (typeof raw === 'string') return raw;
	if (Array.isArray(raw)) {
		const out: RecipeValue[] = [];
		for (const item of raw) {
			const decoded = decodeRecipeValue(item);
			if (decoded === undefined) return undefined; // best-effort: drop the whole malformed value
			out.push(decoded);
		}
		return out;
	}
	return undefined;
}

/** Decode one op object into a `RecipeOp`, stripping the reserved `op` key. */
function decodeOp(raw: unknown): RecipeOp | null {
	if (!isPlainObject(raw)) return null;
	let op = '';
	const params: Record<string, RecipeValue> = {};
	for (const [key, value] of Object.entries(raw)) {
		if (key === 'op') {
			op = typeof value === 'string' ? value : '';
			continue;
		}
		const decoded = decodeRecipeValue(value);
		if (decoded !== undefined) params[key] = decoded;
	}
	return { op, params };
}

/** Decode one recipe object; returns null if `type` is missing/invalid. */
export function decodeRecipe(raw: unknown): TutorRecipe | null {
	if (!isPlainObject(raw)) return null;
	if (typeof raw.type !== 'string' || raw.type.length === 0) return null;

	const aliases = Array.isArray(raw.aliases)
		? raw.aliases.filter((a): a is string => typeof a === 'string')
		: [];

	const defaults: Record<string, number> = {};
	if (isPlainObject(raw.defaults)) {
		for (const [key, value] of Object.entries(raw.defaults)) {
			if (typeof value === 'number' && Number.isFinite(value)) defaults[key] = value;
		}
	}

	const draw: RecipeOp[] = [];
	if (Array.isArray(raw.draw)) {
		for (const opRaw of raw.draw) {
			const op = decodeOp(opRaw);
			if (op) draw.push(op);
		}
	}

	return {
		type: raw.type,
		aliases,
		version: typeof raw.version === 'number' ? raw.version : undefined,
		defaults,
		draw
	};
}

/**
 * Decode a recipe set from parsed JSON — accepts a bare array or the
 * `{ primitives: [...] }` envelope. Malformed recipes are skipped, never thrown.
 * Mirrors Swift `TutorPrimitiveRegistry.decode`.
 */
export function decodeRecipes(json: unknown): TutorRecipe[] {
	let list: unknown[] | null = null;
	if (Array.isArray(json)) {
		list = json;
	} else if (isPlainObject(json) && Array.isArray(json.primitives)) {
		list = json.primitives;
	}
	if (!list) return [];
	const out: TutorRecipe[] = [];
	for (const raw of list) {
		const recipe = decodeRecipe(raw);
		if (recipe) out.push(recipe);
	}
	return out;
}
