import { describe, expect, it } from 'vitest';
import { normalizeChipToken, normalizeKind, serializeChipToken } from './chipMarkup';
import {
	buildFeatureMentionItems,
	buildTaskMentionItems,
	detectMentionTrigger,
	filterMentionItems,
	mentionGroupForQuery,
	mentionKindLabel,
	mentionMatchesFor,
	mentionNeedleForQuery,
	type ComposerMentionItem
} from './composerMentions';

const ITEMS: ComposerMentionItem[] = [
	{ id: 'agent:forge', kind: 'agent', label: 'Forge', detail: 'CTO', insertText: 'agent:forge' },
	{ id: 'tool:web-search', kind: 'tool', label: 'Web Search', detail: 'Research', insertText: 'skill:web-search' },
	{ id: 'personality:concise', kind: 'personality', label: 'Concise', insertText: 'personality:concise' },
	{ id: 'task:task-1', kind: 'task', label: 'Quarterly Report', detail: 'task-1', insertText: 'task:task-1' },
	{ id: 'feature:tutor', kind: 'feature', label: '@tutor', insertText: 'feature:tutor', searchText: 'blackboard explain' }
];

describe('composer mention query parsing', () => {
	it.each([
		['agent', 'agent'], ['agents:fo', 'agent'],
		['skill', 'tool'], ['skills:web', 'tool'], ['tool:search', 'tool'], ['tools', 'tool'],
		['personality:con', 'personality'], ['task', 'task'], ['tasks:q', 'task'],
		['feature', 'feature'], ['features:t', 'feature'], ['', 'all'], ['forge', 'all']
	] as const)('maps %j to the %s group', (query, group) => {
		expect(mentionGroupForQuery(query)).toBe(group);
	});

	it('extracts the post-colon needle only for a recognized group', () => {
		expect(mentionNeedleForQuery('agents:FOR', 'agent')).toBe('for');
		expect(mentionNeedleForQuery('task', 'task')).toBe('');
		expect(mentionNeedleForQuery('Quarter', 'all')).toBe('quarter');
	});

	it('filters by group and searches label, detail, and id', () => {
		expect(filterMentionItems(ITEMS, 'agent', '')).toEqual([ITEMS[0]]);
		expect(filterMentionItems(ITEMS, 'all', 'cto')).toEqual([ITEMS[0]]);
		expect(filterMentionItems(ITEMS, 'all', 'task-1')).toEqual([ITEMS[3]]);
		expect(filterMentionItems(ITEMS, 'all', 'blackboard')).toEqual([ITEMS[4]]);
	});

	it('applies group parsing, filtering, and result cap in one operation', () => {
		const repeated = Array.from({ length: 12 }, (_, index): ComposerMentionItem => ({
			id: `agent:a-${index}`,
			kind: 'agent',
			label: `Agent ${index}`,
			insertText: `agent:a-${index}`
		}));
		expect(mentionMatchesFor([...repeated, ITEMS[1]], 'agents:', 4)).toHaveLength(4);
		expect(mentionMatchesFor(ITEMS, 'task:quarter')).toEqual([ITEMS[3]]);
	});
});

describe('composer mention trigger detection', () => {
	it.each([
		['@', { consume: 1, query: '' }],
		['hello @for', { consume: 4, query: 'for' }],
		['line\n@task:abc', { consume: 9, query: 'task:abc' }],
		['@agent forge', { consume: 12, query: 'agent:forge' }],
		['please @skills web', { consume: 11, query: 'skills:web' }]
	])('detects trigger in %j', (text, expected) => {
		expect(detectMentionTrigger(text)).toEqual(expected);
	});

	it('allows multi-word bare searches only when requested', () => {
		expect(detectMentionTrigger('Use @Quarterly rev')).toBeNull();
		expect(detectMentionTrigger('Use @Quarterly rev', { allowSpaces: true })).toEqual({
			consume: 14,
			query: 'Quarterly rev'
		});
	});

	it.each([
		['email@example.com'],
		['prefix@agent'],
		['already agent:forge'],
		['@agent forge more words'],
		['newline @first\nmore']
	])('rejects non-trigger text %j', (text) => {
		expect(detectMentionTrigger(text)).toBeNull();
	});

	it('uses the latest at-sign for spaced searches', () => {
		expect(detectMentionTrigger('Discuss @old and @new title', { allowSpaces: true })).toEqual({
			consume: 10,
			query: 'new title'
		});
	});
});

describe('composer mention catalog builders', () => {
	it.each([
		['agent', 'Agent'], ['tool', 'Tool'], ['personality', 'Personality'],
		['task', 'Task'], ['feature', 'Feature']
	] as const)('labels %s items as %s', (kind, label) => {
		expect(mentionKindLabel(kind)).toBe(label);
	});

	it('always offers tutor, brainstorm, and VibeDev lanes and gates copilot on image availability', () => {
		const withoutCopilot = buildFeatureMentionItems({ includeCopilot: false });
		expect(withoutCopilot.map((item) => item.id)).toEqual([
			'feature:tutor',
			'feature:tutor_quick',
			'feature:brainstorm',
			'feature:vibedev',
			'feature:vibedev_discuss'
		]);
		expect(buildFeatureMentionItems({ includeCopilot: true }).map((item) => item.id)).toEqual([
			'feature:tutor',
			'feature:tutor_quick',
			'feature:brainstorm',
			'feature:vibedev',
			'feature:vibedev_discuss',
			'feature:copilot'
		]);
	});

	it('offers both VibeDev lanes for a bare @vibedev query', () => {
		const items = buildFeatureMentionItems({ includeCopilot: true });
		expect(mentionMatchesFor(items, 'vibedev').map((item) => item.id)).toEqual([
			'feature:vibedev',
			'feature:vibedev_discuss'
		]);
	});

	it.each([
		['feature:vibedev', 'vibedev', '@vibedev'],
		['feature:vibedev_discuss', 'vibedev_discuss', '@vibedev #discuss']
	])('round-trips picker entry %s into feature chip %s sending %s', (id, slug, command) => {
		const item = buildFeatureMentionItems({ includeCopilot: false }).find(
			(candidate) => candidate.id === id
		);
		expect(item?.insertText).toBe(id);
		// Replay MentionTextarea.applyMention's commit path — split `insertText` on
		// the first colon, hand the halves to the chip primitives — so the picker
		// entry stays pinned to the command that actually reaches the backend
		// rather than only to itself.
		const colonIdx = id.indexOf(':');
		const chip = normalizeChipToken(normalizeKind(id.slice(0, colonIdx)), id.slice(colonIdx + 1));
		expect(chip).toEqual({ kind: 'feature', slug, routeAgent: undefined });
		expect(serializeChipToken(chip.kind, chip.slug)).toBe(command);
	});

	it('builds precise task mentions while displaying the human title', () => {
		expect(buildTaskMentionItems([
			{ id: ' task-42 ', title: ' Quarterly Review ' },
			{ id: '', title: 'Missing id' },
			{ id: 'task-43', title: ' ' },
			{}
		])).toEqual([{
			id: 'task:task-42',
			kind: 'task',
			label: 'Quarterly Review',
			detail: 'task-42',
			insertText: 'task:task-42',
			chipLabel: 'Quarterly Review',
			searchText: 'Quarterly Review task-42'
		}]);
	});

	it('preserves caller order and duplicate ids for transparent catalog composition', () => {
		const items = buildTaskMentionItems([
			{ id: 'same', title: 'First' },
			{ id: 'same', title: 'Second' }
		]);
		expect(items.map((item) => item.label)).toEqual(['First', 'Second']);
	});
});
