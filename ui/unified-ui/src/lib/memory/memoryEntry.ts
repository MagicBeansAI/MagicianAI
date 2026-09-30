/**
 * Owner-facing memory entry contract.
 *
 * Types live here (not on the Svelte card) so Svelte 5 instance scripts
 * do not have to re-export them. The card, `/memory` page, and pager
 * share this shape.
 */

export type MemoryTrust = 'stated' | 'inferred' | 'untrusted';

export interface MemoryScopeDraft {
	topics: string[];
	entities: string[];
	applies_to: string[];
}

export interface MemoryEntry {
	tier: string;
	key: string;
	source_type: string;
	trust: MemoryTrust | string;
	kind: string;
	value?: unknown;
	scope?: MemoryScopeDraft | null;
	updated_at?: string | null;
	confirmed_from?: string | null;
	may_explain?: boolean;
	conflict?: string | null;
	/** Times behaviour agreed with the written memory. Shown only with `conflict_disagree`. */
	conflict_agree?: number;
	/** Times behaviour disagreed (opened anyway). Shown only with `conflict_agree`. */
	conflict_disagree?: number;
}
