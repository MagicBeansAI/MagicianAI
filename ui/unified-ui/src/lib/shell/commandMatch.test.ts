import { describe, expect, it } from 'vitest';

import { commandFilter } from './commandMatch';

describe('commandFilter', () => {
	const notes = 'actions notes open your notes';

	it('matches regardless of capitalization', () => {
		expect(commandFilter(notes, 'NOTES')).toBeGreaterThan(0);
		expect(commandFilter(notes, 'Notes')).toBeGreaterThan(0);
		expect(commandFilter('Actions Notes Open your notes', 'notes')).toBeGreaterThan(0);
	});

	it('does not match an unrelated command', () => {
		expect(commandFilter(notes, 'settings')).toBe(0);
	});
});
