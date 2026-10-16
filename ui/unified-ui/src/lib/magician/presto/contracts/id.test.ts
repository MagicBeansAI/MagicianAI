import { describe, expect, it } from 'vitest';
import { buildPrestoComponentId, routePrefixFromPath } from './id';

describe('presto id route prefixes', () => {
	it('maps route paths to deterministic prefixes without duplicate separators', () => {
		expect(routePrefixFromPath('/crew/[id]/memory/[tier]')).toBe(
			'presto.crew.id.memory.tier'
		);
		expect(routePrefixFromPath('/crew/[id]/memory/[tier]')).not.toContain('..');
	});

	it('builds deterministic component ids for presto routes', () => {
		expect(buildPrestoComponentId('/today', 'hero')).toBe('presto.today.hero');
	});
});
