import { describe, expect, it } from 'vitest';

import {
	absoluteNoteLink,
	linkCandidates,
	noteHref,
	rewriteWikiLinks,
	targetFromHref
} from './noteLinks';

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
describe('canonical note links', () => {
	it('builds the relative /notes link with path then workspace', () => {
		expect(noteHref('Workspaces/acme/Goals/Raise a seed.md', 'acme-co')).toBe(
			'/notes?path=Workspaces%2Facme%2FGoals%2FRaise%20a%20seed.md&workspace=acme-co'
		);
	});

	it('omits an empty workspace', () => {
		expect(noteHref('Inbox/a.md')).toBe('/notes?path=Inbox%2Fa.md');
		expect(noteHref('Inbox/a.md', '  ')).toBe('/notes?path=Inbox%2Fa.md');
	});

	it('encodes reserved characters in both values', () => {
		expect(noteHref('Q&A/50% done?.md', 'a b')).toBe(
			'/notes?path=Q%26A%2F50%25%20done%3F.md&workspace=a%20b'
		);
	});

	it('makes the absolute form from an origin', () => {
		expect(absoluteNoteLink('Inbox/a.md', 'home', 'https://mac.example.com/')).toBe(
			'https://mac.example.com/notes?path=Inbox%2Fa.md&workspace=home'
		);
	});
});
