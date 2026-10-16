import { describe, expect, it } from 'vitest';
import { capabilityCategory, capabilityLabel } from './capabilityLabels';

describe('capabilityLabels', () => {
	it('turns a raw tool id into a readable label', () => {
		expect(capabilityLabel('mcp__browser__web_search')).toBe('Web Search');
		expect(capabilityLabel('git-status')).toBe('Git Status');
	});

	it('groups tools into the Armory categories', () => {
		expect(capabilityCategory('web_search')).toBe('Research and web');
		expect(capabilityCategory('git_status')).toBe('Engineering');
		expect(capabilityCategory('slack_post')).toBe('Communication');
		expect(capabilityCategory('analytics_query')).toBe('Knowledge and data');
		expect(capabilityCategory('calendar')).toBe('General operations');
	});
});
