import { describe, expect, it } from 'vitest';

import { commandFilter, sectionKeyword } from './commandMatch';

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

	it('ranks Jump to, then Manage, ahead of better matches elsewhere', () => {
		const jump = commandFilter('Storage Jump to sizes', 'stor', [sectionKeyword('Jump to')]);
		const manage = commandFilter('Settings Manage storage', 'stor', [sectionKeyword('Manage')]);
		const action = commandFilter('Storage Actions', 'storage', [sectionKeyword('Actions')]);
		expect(jump).toBeGreaterThan(manage);
		expect(manage).toBeGreaterThan(action);
		expect(commandFilter('Storage Jump to', 'zzz', [sectionKeyword('Jump to')])).toBe(0);
	});
});
