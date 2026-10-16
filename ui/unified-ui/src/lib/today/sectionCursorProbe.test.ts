import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { todaySectionCursorProbeUrl } from './sectionCursorProbe';

function probeParams(cursor: string | null = null): URLSearchParams {
	return new URL(
		todaySectionCursorProbeUrl({
			sectionId: 'followups',
			cursor,
			sectionPageSize: 8,
			digestPageSize: 7
		}),
		'http://localhost'
	).searchParams;
}

describe('the Today section cursor probe', () => {
	beforeEach(() => {
		scopeIdentityStore.observe('alice', 'work');
	});

	afterEach(() => {
		scopeIdentityStore.reset();
	});

	it('asks for the page the store is about to ask for', () => {
		const params = probeParams('780:1700:item-9');
		expect(params.get('section')).toBe('followups');
		expect(params.get('per_section')).toBe('8');
		expect(params.get('limit')).toBe('8');
		expect(params.get('digest_limit')).toBe('7');
		expect(params.get('digest_offset')).toBe('0');
		expect(params.has('principal')).toBe(false);
		expect(params.has('workspace')).toBe(false);
		expect(params.get('cursor')).toBe('780:1700:item-9');
	});

	it('leaves the cursor off the first page', () => {
		expect(probeParams(null).has('cursor')).toBe(false);
	});

	// Pinned in a positive-offset zone at an hour where the reader's calendar
	// date and the UTC date are different days. The probe mints the cursors the
	// store then seeks with, and a Follow-ups cursor's leading priority band is
	// derived from the date — a cursor minted against the server's UTC day and
	// seeked against the reader's day lands in a different projection, so rows
	// are skipped or repeated and the totals disagree with what renders.
	describe('the date the probe carries', () => {
		const ambientTimeZone = process.env.TZ;

		beforeEach(() => {
			process.env.TZ = 'Asia/Kolkata';
			vi.useFakeTimers();
			// 01:30 on the 31st in IST. In UTC it is still the 30th.
			vi.setSystemTime(new Date('2026-07-30T20:00:00.000Z'));
		});

		afterEach(() => {
			vi.useRealTimers();
			if (ambientTimeZone === undefined) delete process.env.TZ;
			else process.env.TZ = ambientTimeZone;
		});

		it('sends the local date the reader is on, never its UTC rendering', () => {
			// The fixture's own guard, asserted before anything else: if this ever
			// stops naming the 30th the pinned instant has stopped straddling the
			// boundary and every assertion below would pass without meaning it.
			expect(new Date().toISOString().slice(0, 10)).toBe('2026-07-30');
			// The value, not the parameter's presence. A probe that derived the
			// date from `toISOString()` — the omission this closes, and the bug
			// before it — would send `2026-07-30` and still satisfy a presence
			// check.
			expect(probeParams().get('today')).toBe('2026-07-31');
			expect(probeParams().get('today')).not.toBe(new Date().toISOString().slice(0, 10));
		});
	});
});
