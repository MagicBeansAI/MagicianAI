import { describe, expect, it } from 'vitest';

import { reviewActions, reviewQueue, reviewSources } from './preferenceReview';
import type { AssertionStatus, Preference } from './preferencesApi';

const pref = (
	id: string,
	status: AssertionStatus,
	extra: Partial<Preference> = {}
): Preference => ({
	id,
	revision: 'r1',
	kind: 'taste',
	status,
	statement: `${id}: something`,
	conditions: null,
	lifecycle: 'active',
	source: 'preferences',
	editable: true,
	lastCheckedAtMs: null,
	learnedAtMs: null,
	grounds: [],
	...extra
});

describe('review what I have learned', () => {
	it('lists only what Magician inferred, newest learned first, undated last', () => {
		const queue = reviewQueue([
			pref('said', 'direct_owner_statement', { learnedAtMs: 50 }),
			pref('confirmed', 'confirmed', { learnedAtMs: 40 }),
			pref('old', 'inferred', { learnedAtMs: 10 }),
			pref('undated-b', 'inferred'),
			pref('new', 'inferred_untrusted', { learnedAtMs: 30 }),
			pref('undated-a', 'unknown_provenance', { editable: false, source: 'taste profile' })
		]);
		expect(queue.map((p) => p.id)).toEqual(['new', 'old', 'undated-a', 'undated-b']);
	});

	it('narrows to one source and counts each source, largest first', () => {
		const prefs = [
			pref('a', 'inferred', { source: 'insights' }),
			pref('b', 'inferred', { source: 'insights' }),
			pref('c', 'inferred', { source: 'preferences' }),
			pref('d', 'confirmed', { source: 'preferences' })
		];
		expect(reviewQueue(prefs, 'insights').map((p) => p.id)).toEqual(['a', 'b']);
		expect(reviewSources(prefs)).toEqual([
			{ source: 'insights', count: 2 },
			{ source: 'preferences', count: 1 }
		]);
	});

	it('offers Confirm only for a plain inference the owner can edit', () => {
		expect(reviewActions(pref('a', 'inferred'))).toEqual({ canConfirm: true, canDismiss: true, note: null });
		const untrusted = reviewActions(pref('b', 'inferred_untrusted'));
		expect(untrusted).toMatchObject({ canConfirm: false, canDismiss: true });
		expect(untrusted.note).toContain('own words');
		expect(
			reviewActions(pref('c', 'unknown_provenance', { editable: false, source: 'taste profile' }))
		).toEqual({ canConfirm: false, canDismiss: false, note: 'Change this in your taste profile.' });
	});
});
