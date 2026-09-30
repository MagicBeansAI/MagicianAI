import { describe, expect, it } from 'vitest';

import { linkCandidates, rewriteWikiLinks, targetFromHref } from './noteLinks';

describe('note links', () => {
	it('turns a wiki link into an openable note link and leaves code fences alone', () => {
		const rewritten = rewriteWikiLinks('See [[Inbox]] and [[Programs/harness|Harness]].\n\n```\n[[not a link]]\n```');
		expect(rewritten).toContain('[Inbox](#note/Inbox)');
		expect(rewritten).toContain('[Harness](#note/Programs%2Fharness)');
		expect(rewritten).toContain('[[not a link]]');
		expect(targetFromHref('#note/Inbox')).toBe('Inbox');
		expect(linkCandidates('index.md', 'Inbox')).toEqual(['Inbox.md', 'Inbox/index.md']);
	});
});