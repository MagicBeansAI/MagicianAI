import { describe, expect, it } from 'vitest';

import {
	deriveTaskBrief,
	deriveTaskTitle,
	labelOrShortIdTitle,
	looksLikeId,
	shortTaskId,
	titleFromPrompt
} from './taskPanelTitle';

/** The prompt observed live, whose drawer title rendered as `executio…7bba`. */
const ROUTED_PROMPT =
	'web-researcher / Do not delegate further. Use browser with default cdp to search MakeMyTrip for the cheapest one-way economy flight BLR to DEL departing 10 October 2026. Report the airline and fare.';

describe('looksLikeId', () => {
	it('recognises the task id itself, and any string carrying it', () => {
		expect(looksLikeId('execution_1f2e3d4c7bba', 'execution_1f2e3d4c7bba')).toBe(true);
		expect(looksLikeId('Run execution_1f2e3d4c7bba', 'execution_1f2e3d4c7bba')).toBe(true);
	});

	it('recognises id-shaped tokens without being told the id', () => {
		expect(looksLikeId('execution_1f2e3d4c7bba', null)).toBe(true);
		expect(looksLikeId('task-20261001-abc123', null)).toBe(true);
		expect(looksLikeId('3f2504e0-4f89-11d3-9a0c-0305e82c3301', null)).toBe(true);
		// The drawer's own elided rendering of one.
		expect(looksLikeId('executio…7bba', null)).toBe(true);
		expect(looksLikeId('Run exec_9a8b7c6d5e', null)).toBe(true);
	});

	it('leaves prose alone, including short titles with numbers in them', () => {
		expect(looksLikeId('Reindex the corpus', 'task_alpha')).toBe(false);
		expect(looksLikeId('Q3 report', null)).toBe(false);
		expect(looksLikeId('Weekly digest', null)).toBe(false);
		expect(looksLikeId('GPT-5', null)).toBe(false);
	});
});

describe('titleFromPrompt', () => {
	it('strips the routing slug and the directive sentences, keeping the goal', () => {
		expect(titleFromPrompt(ROUTED_PROMPT)).toBe(
			// 95 characters, so the ~90 cap cuts it at the last word boundary.
			'Search MakeMyTrip for the cheapest one-way economy flight BLR to DEL departing 10 October…'
		);
	});

	it('drops leading "Don\'t" / "Do not" meta sentences', () => {
		expect(titleFromPrompt("Don't ask questions. Summarise the attached PDF.")).toBe(
			'Summarise the attached PDF'
		);
		expect(titleFromPrompt('Do not use tools. Do not delegate. Write a haiku about rain.')).toBe(
			'Write a haiku about rain'
		);
	});

	it('drops a bare "Use the browser." sentence', () => {
		expect(titleFromPrompt('Use the browser. Find the opening hours of the city library.')).toBe(
			'Find the opening hours of the city library'
		);
	});

	it('takes only the first line', () => {
		expect(titleFromPrompt('Plan the offsite\n\n- venue\n- budget')).toBe('Plan the offsite');
	});

	it('caps at ~90 characters on a word boundary', () => {
		const long =
			'Compare the pricing pages of every major vector database vendor and summarise the differences in storage, egress and query costs';
		const title = titleFromPrompt(long);
		expect(title).not.toBeNull();
		expect(title!.length).toBeLessThanOrEqual(91);
		expect(title!.endsWith('…')).toBe(true);
		// The cut lands between words, never inside one.
		expect(long.startsWith(title!.slice(0, -1))).toBe(true);
		expect(long.charAt(title!.length - 1)).toBe(' ');
	});

	it('keeps a slug-looking first word that is not followed by a slash', () => {
		expect(titleFromPrompt('web-researcher findings for Q3')).toBe('web-researcher findings for Q3');
	});

	it('answers null when there is no prose', () => {
		expect(titleFromPrompt(null)).toBeNull();
		expect(titleFromPrompt('   ')).toBeNull();
		expect(titleFromPrompt('Do not delegate further.')).toBeNull();
		expect(titleFromPrompt('execution_1f2e3d4c7bba')).toBeNull();
	});
});

describe('deriveTaskTitle', () => {
	const id = 'execution_1f2e3d4c7bba';

	it('never overrides a real title a surface passed', () => {
		expect(
			deriveTaskTitle({ label: null, title: 'Reindex the corpus', description: ROUTED_PROMPT, id })
		).toBe('Reindex the corpus');
	});

	it('prefers a display label when one exists', () => {
		expect(
			deriveTaskTitle({ label: 'Flight search', title: id, description: ROUTED_PROMPT, id })
		).toBe('Flight search');
	});

	it('replaces an id-shaped title with the prompt’s goal', () => {
		expect(deriveTaskTitle({ label: null, title: id, description: ROUTED_PROMPT, id })).toBe(
			// 95 characters, so the ~90 cap cuts it at the last word boundary.
			'Search MakeMyTrip for the cheapest one-way economy flight BLR to DEL departing 10 October…'
		);
		expect(
			deriveTaskTitle({ label: null, title: `Run ${id}`, description: ROUTED_PROMPT, id })
		).toMatch(/^Search MakeMyTrip/);
	});

	it('fills a missing or blank title from the prompt', () => {
		expect(deriveTaskTitle({ label: null, title: null, description: ROUTED_PROMPT, id })).toMatch(
			/^Search MakeMyTrip/
		);
		expect(deriveTaskTitle({ label: null, title: '  ', description: ROUTED_PROMPT, id })).toMatch(
			/^Search MakeMyTrip/
		);
	});

	it('treats a routing-prefixed title as prompt text, not a human title', () => {
		// Live: the surface passed the routed prompt itself as the title.
		expect(deriveTaskTitle({ label: null, title: ROUTED_PROMPT, description: null, id })).toBe(
			'Search MakeMyTrip for the cheapest one-way economy flight BLR to DEL departing 10 October…'
		);
		// The same, already cut short by the surface.
		expect(
			deriveTaskTitle({
				label: null,
				title:
					'web-researcher / Do not delegate further. Use browser with default cdp to search MakeMyTrip for the cheapest one-way economy flight BLR to DEL…',
				description: ROUTED_PROMPT,
				id
			})
		).toBe('Search MakeMyTrip for the cheapest one-way economy flight BLR to DEL');
		// A routed label is prompt text too.
		expect(deriveTaskTitle({ label: ROUTED_PROMPT, title: null, description: null, id })).toMatch(
			/^Search MakeMyTrip/
		);
	});

	it('falls back to the description when a routed title has no prose left', () => {
		expect(
			deriveTaskTitle({
				label: null,
				title: 'web-researcher / Do not delegate further.',
				description: 'Write a haiku about rain.',
				id
			})
		).toBe('Write a haiku about rain');
	});

	it('keeps real titles that merely contain a slash', () => {
		expect(
			deriveTaskTitle({ label: null, title: 'Q3 / Q4 budget review', description: ROUTED_PROMPT, id })
		).toBe('Q3 / Q4 budget review');
		expect(
			deriveTaskTitle({ label: null, title: 'Web-researcher findings', description: ROUTED_PROMPT, id })
		).toBe('Web-researcher findings');
	});

	it('falls back to what it was given when there is no prose to use', () => {
		expect(deriveTaskTitle({ label: null, title: id, description: null, id })).toBe(id);
		expect(deriveTaskTitle({ label: null, title: null, description: null, id })).toBeNull();
		// An id-shaped label is not a label.
		expect(deriveTaskTitle({ label: id, title: null, description: 'Write a haiku', id })).toBe(
			'Write a haiku'
		);
	});
});

describe('deriveTaskBrief', () => {
	it('strips a routing directive from the description', () => {
		expect(deriveTaskBrief(ROUTED_PROMPT, 'Something else')).toBe(
			'Use browser with default cdp to search MakeMyTrip for the cheapest one-way economy flight BLR to DEL departing 10 October 2026. Report the airline and fare.'
		);
	});

	it('leaves a description with no routing directive exactly as written', () => {
		const description = "Don't panic. Reindex every shard.";
		expect(deriveTaskBrief(description, 'Reindex')).toBe(description);
	});

	it('hides a brief that only repeats the title', () => {
		expect(deriveTaskBrief('Write a haiku about rain.', 'Write a haiku about rain')).toBeNull();
		expect(deriveTaskBrief('researcher / Write a haiku about rain', 'Write a haiku about rain')).toBeNull();
	});

	it('answers null for nothing', () => {
		expect(deriveTaskBrief(null, 'x')).toBeNull();
		expect(deriveTaskBrief('  ', 'x')).toBeNull();
	});
});

describe('shortTaskId / labelOrShortIdTitle', () => {
	it('drops the word prefix and keeps six characters', () => {
		expect(shortTaskId('task_2679cefd6f664bf7bd4a3b57fae4ce75')).toBe('2679ce');
		expect(shortTaskId('execution-1f2e3d4c7bba')).toBe('1f2e3d');
		expect(shortTaskId('3f2504e0-4f89-11d3')).toBe('3f2504');
		expect(shortTaskId('  ')).toBeNull();
		expect(shortTaskId(null)).toBeNull();
	});

	it('titles from a real label, else a short id, never the raw id', () => {
		expect(labelOrShortIdTitle('Flight search', 'task_2679cefd6f')).toBe('Flight search');
		expect(labelOrShortIdTitle(null, 'task_2679cefd6f664bf7bd4a3b57fae4ce75')).toBe('Task · 2679ce');
		expect(labelOrShortIdTitle('task_2679cefd6f664bf7', 'task_2679cefd6f664bf7')).toBe('Task · 2679ce');
		expect(labelOrShortIdTitle(ROUTED_PROMPT, 'task_1')).toMatch(/^Search MakeMyTrip/);
		expect(labelOrShortIdTitle(null, null)).toBe('Task');
	});
});
