/**
 * "Review what I've learned": the preferences Magician inferred about the
 * owner, newest first, with what the owner can do with each
 * (docs/components/unified-ui/README.md, Memory → Preferences).
 *
 * Confirm makes an inferred preference the owner's own, so it can make an
 * outward act wait; Dismiss retires it, so checks stop judging it. Both ride
 * the existing `PATCH /preference-alignment/intentions/{id}` actions.
 */
import type { AssertionStatus, Preference } from './preferencesApi';

/** Statuses Magician arrived at without the owner saying so. */
export const LEARNED_STATUSES: ReadonlySet<AssertionStatus> = new Set<AssertionStatus>([
	'inferred',
	'inferred_untrusted',
	'unknown_provenance'
]);

/** Rows rendered per page; the owner may have hundreds. */
export const REVIEW_PAGE_SIZE = 25;

export const ALL_SOURCES = '';

export const isLearned = (preference: Preference): boolean => LEARNED_STATUSES.has(preference.status);

/** Inferred preferences from `source` (all when empty), newest learned first. */
export function reviewQueue(preferences: Preference[], source: string = ALL_SOURCES): Preference[] {
	return preferences
		.filter((preference) => isLearned(preference) && (!source || preference.source === source))
		.sort((a, b) => {
			const byLearned = (b.learnedAtMs ?? -Infinity) - (a.learnedAtMs ?? -Infinity);
			if (byLearned !== 0 && !Number.isNaN(byLearned)) return byLearned;
			return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
		});
}

/** Each source with how many inferred preferences it holds, largest first. */
export function reviewSources(preferences: Preference[]): Array<{ source: string; count: number }> {
	const counts = new Map<string, number>();
	for (const preference of preferences) {
		if (isLearned(preference)) counts.set(preference.source, (counts.get(preference.source) ?? 0) + 1);
	}
	return [...counts.entries()]
		.map(([source, count]) => ({ source, count }))
		.sort((a, b) => b.count - a.count || (a.source < b.source ? -1 : 1));
}

export interface ReviewActions {
	canConfirm: boolean;
	canDismiss: boolean;
	/** Why Confirm (or both actions) is missing, for the owner to read. */
	note: string | null;
}

/**
 * Confirm only for a plain inference in the owner's native memory: one from a
 * page, screen or meeting stays advisory (the owner can add it in their own
 * words instead), and profile or app preferences change where they live.
 */
export function reviewActions(preference: Preference): ReviewActions {
	if (!preference.editable) {
		return {
			canConfirm: false,
			canDismiss: false,
			note: `Change this in your ${preference.source}.`
		};
	}
	if (preference.status === 'inferred') return { canConfirm: true, canDismiss: true, note: null };
	return {
		canConfirm: false,
		canDismiss: true,
		note:
			preference.status === 'inferred_untrusted'
				? 'Picked up from a page, screen or meeting, so it can only be mentioned. Add it in your own words to make it a rule.'
				: 'Its source is unknown, so it can only be mentioned.'
	};
}
