import { describe, expect, it } from 'vitest';

import type { ContentBlockRecord } from '$lib/stores/chatStore';
import {
	isAgentFacingTaskFileName,
	isTaskSummaryFileName,
	orderWithinSlots,
	presentTaskOutputBlocks
} from './deliverableOrder';

const source = { type: 'task_output' as const, task_id: 'task_1' };

function file(relativePath: string, extra: Partial<ContentBlockRecord> = {}): ContentBlockRecord {
	return {
		type: 'file',
		source,
		relative_path: relativePath,
		display_name: relativePath.split('/').pop(),
		mime_type: 'application/octet-stream',
		...extra
	};
}

describe('task file names', () => {
	it('recognises the summary and the agent-facing state by their generated names', () => {
		expect(isTaskSummaryFileName('outputs/out_task_user_task_1_exec_1.md')).toBe(true);
		expect(isTaskSummaryFileName('outputs/delivered/exec_1/out_task_user_notes.md')).toBe(true);
		expect(isTaskSummaryFileName('outputs/summary.md')).toBe(false);
		expect(isAgentFacingTaskFileName('outputs/out_task_continuation_task_1_exec_1.json')).toBe(true);
		expect(isAgentFacingTaskFileName('outputs/out_task_agent_task_1.md')).toBe(true);
		expect(isAgentFacingTaskFileName('outputs/out_task_user_task_1_exec_1.md')).toBe(false);
	});
});

describe('orderWithinSlots', () => {
	it('moves only ranked items, stably, inside the slots they held', () => {
		const ordered = orderWithinSlots(['s', 'x', 'a', 'p', 'b'], (item) =>
			item === 'x' ? null : item === 'p' ? 0 : item === 's' ? 2 : 1
		);
		expect(ordered).toEqual(['p', 'x', 'a', 'b', 's']);
	});
});

describe('presentTaskOutputBlocks — the chat task card\'s Outputs', () => {
	it('leads with the primary deliverable and ends with the summary, labelled Summary', () => {
		const blocks = presentTaskOutputBlocks([
			file('outputs/out_task_user_task_1_exec_1.md'),
			file('outputs/out_task_continuation_task_1_exec_1.json'),
			file('outputs/delivered/exec_1/notes.md'),
			file('outputs/delivered/exec_1/report.pdf', { primary: true })
		]);

		expect(blocks.map((block) => [block.display_name, block.label ?? null, block.primary === true])).toEqual([
			['report.pdf', null, true],
			['notes.md', null, false],
			// The label changes; the name a download is saved as does not.
			['out_task_user_task_1_exec_1.md', 'Summary', false]
		]);
	});

	it('keeps an old task\'s order but still drops the summary behind the delivered files', () => {
		const blocks = presentTaskOutputBlocks([
			file('outputs/out_task_user_task_1_exec_1.md'),
			file('outputs/chart.png', { mime_type: 'image/png' }),
			{ type: 'text', text: 'note' },
			file('outputs/data.csv')
		]);
		expect(blocks.map((block) => block.label ?? block.display_name ?? block.text)).toEqual([
			'chart.png',
			'note',
			'data.csv',
			'Summary'
		]);
	});

	it('leaves a list without task files untouched', () => {
		const blocks = [file('outputs/a.md'), file('outputs/b.md')];
		expect(presentTaskOutputBlocks(blocks)).toEqual(blocks);
	});
});
