import { describe, expect, it } from 'vitest';

import {
	isInteractiveDescendantEvent,
	showTodayPrimaryAction,
	todayRowClass
} from './rowInteractions';

describe('Today row interactions', () => {
	it('distinguishes nested controls from the clickable row body', () => {
		const row = document.createElement('article');
		row.setAttribute('role', 'button');
		const content = document.createElement('div');
		const actions = document.createElement('div');
		const dismiss = document.createElement('button');
		const icon = document.createElement('svg');
		dismiss.append(icon);
		actions.append(dismiss);
		row.append(content, actions);

		expect(isInteractiveDescendantEvent(row, row)).toBe(false);
		expect(isInteractiveDescendantEvent(content, row)).toBe(false);
		expect(isInteractiveDescendantEvent(actions, row)).toBe(false);
		expect(isInteractiveDescendantEvent(dismiss, row)).toBe(true);
		expect(isInteractiveDescendantEvent(icon, row)).toBe(true);
	});

	it('omits the redundant primary action only for Follow-ups', () => {
		expect(showTodayPrimaryAction('followups')).toBe(false);
		expect(showTodayPrimaryAction('needs_you')).toBe(true);
		expect(showTodayPrimaryAction('active_work')).toBe(true);
		expect(showTodayPrimaryAction('delivered')).toBe(true);
		expect(showTodayPrimaryAction('changed')).toBe(true);
	});

	it('opts clickable rows out of active press scaling', () => {
		expect(todayRowClass('followups', false).split(' ')).toContain('ui-no-press');
		expect(todayRowClass('active_work', true)).toBe(
			'today-row ui-no-press today-row--active-work today-row--emphasis'
		);
	});
});
