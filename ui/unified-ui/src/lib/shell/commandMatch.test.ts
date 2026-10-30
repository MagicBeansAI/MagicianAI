import { describe, expect, it } from 'vitest';

import { commandFilter, commandItemScore } from './commandMatch';

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

	it('ranks exact labels before label prefixes and supporting-text matches across sections', () => {
		const exact = commandItemScore({ label: 'Settings', section: 'Manage' }, 'settings');
		const prefix = commandItemScore({ label: 'Settings helper', section: 'Apps' }, 'settings');
		const hint = commandItemScore({ label: 'Work', section: 'Jump to', hint: 'task settings' }, 'settings');
		expect(exact).toBeGreaterThan(prefix);
		expect(prefix).toBeGreaterThan(hint);
		expect(commandItemScore({ label: 'Calendar', section: 'Apps' }, 'settings')).toBe(0);
	});

	it('keeps multiword and ID searches without unrelated fuzzy matches', () => {
		expect(commandItemScore({ label: 'Work', section: 'Jump to', hint: 'internal tasks' }, 'tasks internal')).toBeGreaterThan(0);
		expect(commandItemScore({ label: 'Write a draft', section: 'Recent tasks', entityId: 'task_8223' }, '8223')).toBeGreaterThan(0);
		expect(commandFilter('Recent sessions', 'settings')).toBe(0);
	});
});
