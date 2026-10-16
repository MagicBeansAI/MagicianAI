// Stable tint slots for chat-next cards, chips and avatars.
//
// A key (source host, artifact name, agent id) always lands on the same
// `--tint-N` slot from app.css, so a source keeps its colour across turns
// and reloads without anyone storing an assignment.

export type TintSlot = 1 | 2 | 3 | 4 | 5;

/** FNV-1a 32-bit hash of `key` (UTF-16 code units), mapped onto slots 1..5. */
export function tintFor(key: string): TintSlot {
	let hash = 0x811c9dc5;
	for (let i = 0; i < key.length; i++) {
		hash ^= key.charCodeAt(i);
		hash = Math.imul(hash, 0x01000193) >>> 0;
	}
	return ((hash % 5) + 1) as TintSlot;
}

/**
 * Inline style string that points `--cn-tint` / `--cn-tint-ink` at the key's
 * slot, for a Svelte `style=` attribute. Components paint with
 * `background: var(--cn-tint); color: var(--cn-tint-ink)`.
 */
export function tintVars(key: string): string {
	const n = tintFor(key);
	return `--cn-tint: var(--tint-${n}); --cn-tint-ink: var(--tint-${n}-ink);`;
}
