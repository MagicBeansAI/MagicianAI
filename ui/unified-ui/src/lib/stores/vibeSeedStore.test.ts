import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// vibeSeedStore guards every sessionStorage access behind `browser`; force it on
// so the one-shot store logic actually runs in the node test env. (vi.mock is
// hoisted above the import below.)
vi.mock('$app/environment', () => ({ browser: true }));

import {
	buildChatSeedContent,
	buildMeetingSeedContent,
	peekSeed,
	putSeed,
	takeSeed,
	type SeedTurn
} from './vibeSeedStore';

/** Minimal Map-backed sessionStorage (the node test env has no DOM). */
function makeSessionStorageMock(): Storage {
	const map = new Map<string, string>();
	return {
		getItem: (k: string) => (map.has(k) ? (map.get(k) as string) : null),
		setItem: (k: string, v: string) => {
			map.set(k, String(v));
		},
		removeItem: (k: string) => {
			map.delete(k);
		},
		clear: () => map.clear(),
		key: (i: number) => Array.from(map.keys())[i] ?? null,
		get length() {
			return map.size;
		}
	} as Storage;
}

beforeEach(() => {
	vi.stubGlobal('sessionStorage', makeSessionStorageMock());
});

afterEach(() => {
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

describe('vibeSeedStore — putSeed / peekSeed / takeSeed', () => {
	it('roundtrips a seed draft and returns a seed-* id', () => {
		const id = putSeed({ source: 'chat', label: 'Chat thread', content: 'build a todo app' });
		expect(id).toMatch(/^seed-/);
		const peeked = peekSeed(id);
		expect(peeked?.content).toBe('build a todo app');
		expect(peeked?.source).toBe('chat');
		expect(peeked?.label).toBe('Chat thread');
		expect(typeof peeked?.createdAt).toBe('number');
		expect(peeked?.id).toBe(id);
	});

	it('takeSeed is ONE-SHOT — a second take returns null (a /vibe refresh cannot re-inject)', () => {
		const id = putSeed({ source: 'meeting', label: 'design-sync', content: 'what we agreed' });
		expect(takeSeed(id)?.content).toBe('what we agreed');
		expect(takeSeed(id)).toBeNull();
		expect(peekSeed(id)).toBeNull();
	});

	it('peekSeed is non-consuming (still takeable after repeated peeks)', () => {
		const id = putSeed({ source: 'chat', label: 'x', content: 'payload' });
		expect(peekSeed(id)?.content).toBe('payload');
		expect(peekSeed(id)?.content).toBe('payload');
		expect(takeSeed(id)?.content).toBe('payload');
	});

	it('returns null for an unknown or empty id', () => {
		expect(takeSeed('seed-missing')).toBeNull();
		expect(peekSeed('')).toBeNull();
		expect(takeSeed('')).toBeNull();
	});

	it('takeSeed returns null (and clears) on unparseable stored JSON', () => {
		const id = putSeed({ source: 'chat', label: 'x', content: 'y' });
		sessionStorage.setItem(`vibe.seed.${id}`, '{ not valid json');
		expect(takeSeed(id)).toBeNull();
		expect(peekSeed(id)).toBeNull();
	});
});

describe('vibeSeedStore — buildChatSeedContent', () => {
	it('formats turns as "- role: text" lines', () => {
		const turns: SeedTurn[] = [
			{ role: 'user', text: 'build a todo app' },
			{ role: 'assistant', text: 'on it' }
		];
		expect(buildChatSeedContent(turns)).toBe('- user: build a todo app\n- assistant: on it');
	});

	it('drops empty turns and keeps only the most-recent N', () => {
		const turns: SeedTurn[] = [
			{ role: 'user', text: 'first' },
			{ role: 'user', text: '   ' },
			{ role: 'user', text: 'second' },
			{ role: 'user', text: 'third' }
		];
		expect(buildChatSeedContent(turns, 2)).toBe('- user: second\n- user: third');
	});

	it('collapses internal whitespace within a turn', () => {
		expect(buildChatSeedContent([{ role: 'user', text: 'a\n\n  b' }])).toBe('- user: a b');
	});

	it('returns a placeholder when there is no usable content', () => {
		expect(buildChatSeedContent([])).toBe('(no chat content)');
		expect(buildChatSeedContent([{ role: 'user', text: '   ' }])).toBe('(no chat content)');
	});

	it('truncates an over-long turn with an ellipsis', () => {
		const out = buildChatSeedContent([{ role: 'user', text: 'x'.repeat(5000) }]);
		expect(out.endsWith('…')).toBe(true);
		expect(out.length).toBeLessThan(1300); // clamped to MAX_TURN_CHARS (1200) + "- user: " + …
	});
});

describe('vibeSeedStore — buildMeetingSeedContent', () => {
	it('prepends a Meeting/Date/Summary header before the transcript', () => {
		const out = buildMeetingSeedContent([{ role: 'Alice', text: 'ship it' }], {
			title: 'Sync',
			date: '2026-06-14',
			summary: 'we agreed to ship'
		});
		expect(out).toContain('Meeting: Sync');
		expect(out).toContain('Date: 2026-06-14');
		expect(out).toContain('Summary: we agreed to ship');
		expect(out).toContain('Transcript:');
		expect(out).toContain('- Alice: ship it');
	});

	it('omits absent header lines and still includes the transcript', () => {
		expect(buildMeetingSeedContent([{ role: 'Bob', text: 'hi' }], {})).toBe('Transcript:\n- Bob: hi');
	});

	it('preserves a multi-line summary structure (Decisions / Action items survive, not flattened)', () => {
		const summary = '## Decisions\n- ship v2 on Friday\n\n## Action items\n- Alice: write the migration';
		const out = buildMeetingSeedContent([{ role: 'Alice', text: 'ship it' }], {
			title: 'Sync',
			summary
		});
		// Rendered as a labeled block, newlines intact (NOT collapsed into one line).
		expect(out).toContain('Summary:\n## Decisions');
		expect(out).toContain('- ship v2 on Friday');
		expect(out).toContain('## Action items');
		expect(out).toContain('- Alice: write the migration');
		expect(out).toContain('Transcript:');
		// Guard against regression to the whitespace-collapsing clampTurn path.
		expect(out).not.toContain('## Decisions - ship v2 on Friday');
	});
});
