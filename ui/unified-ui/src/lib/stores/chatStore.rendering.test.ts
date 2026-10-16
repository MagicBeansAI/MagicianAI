import { describe, expect, it } from 'vitest';
import type { ChatMessage, ChatMessageContent, ChatRenderMessage } from './chatStore';
import {
	attachInTurnActivityToAssistantMessages,
	attachedActivityMessageIds,
	collapseTaskProgressMessages,
	convertMessage,
	getEscalationResolvedSummary,
	getMessageContentBlocks,
	getMessageText,
	getStructuredResponsePlainText,
	isHiddenTranscriptMessage,
	renderMessageContainsId
} from './chatStore';

function message(
	id: string,
	direction: ChatMessage['direction'],
	content: ChatMessageContent,
	createdAt = 1
): ChatMessage {
	return {
		id,
		session_id: 'session-1',
		direction,
		content,
		created_at: createdAt
	};
}

function textMessage(
	id: string,
	direction: ChatMessage['direction'],
	text: string,
	createdAt = 1
): ChatMessage {
	return message(id, direction, { type: 'text', text }, createdAt);
}

function taskMessage(
	id: string,
	status: string,
	executionId: string | undefined,
	createdAt = 1,
	taskId = 'task-1'
): ChatMessage {
	return message(id, 'system', {
		type: 'task_status_update',
		task_id: taskId,
		execution_id: executionId,
		status,
		summary: `${status} summary`
	}, createdAt);
}

function render(messages: ChatMessage[]): ChatRenderMessage[] {
	return collapseTaskProgressMessages(messages);
}

describe('structured response canonical projection', () => {
	it('matches the backend projection instead of display-only prose', () => {
		expect(getStructuredResponsePlainText({
			type: 'tool_call_executed', tool_name: 'search', summary: 'Found a result'
		} as ChatMessageContent)).toBe('Found a result');
		expect(getStructuredResponsePlainText({
			type: 'rich_tool_result', tool_name: 'search', summary: '',
			content_blocks: [{ type: 'text', text: 'First result' }]
		} as ChatMessageContent)).toBe('First result');
		expect(getStructuredResponsePlainText({
			type: 'attachment', filename: 'report.pdf', label: 'Report'
		} as ChatMessageContent)).toBe('report.pdf');
		expect(getStructuredResponsePlainText({
			type: 'task_status_update', task_id: 'task-1', status: 'completed', summary: ''
		} as ChatMessageContent)).toBe('completed');
	});
});

describe('chat task progress coalescing', () => {
	it('keeps ordinary messages as independent render rows', () => {
		const rows = render([
			textMessage('user', 'user', 'hello'),
			textMessage('assistant', 'assistant', 'hi', 2)
		]);
		expect(rows.map((row) => row.id)).toEqual(['user', 'assistant']);
		expect(rows.every((row) => row.taskExecutionGroups.length === 0)).toBe(true);
	});

	it('coalesces updates for one task execution and promotes the latest message', () => {
		const rows = render([
			taskMessage('running', 'running', 'exec-1'),
			taskMessage('completed', 'completed', 'exec-1', 2)
		]);
		expect(rows).toHaveLength(1);
		expect(rows[0].message.id).toBe('completed');
		expect(rows[0].messageIds).toEqual(['running', 'completed']);
		expect(rows[0].taskExecutionGroups[0].updates.map((entry) => entry.id)).toEqual([
			'running', 'completed'
		]);
	});

	it('promotes a no-execution creation placeholder into the first real run', () => {
		const rows = render([
			taskMessage('created', 'created', undefined),
			taskMessage('running', 'running', 'exec-1', 2)
		]);
		expect(rows).toHaveLength(1);
		expect(rows[0].taskExecutionGroups).toHaveLength(1);
		expect(rows[0].taskExecutionGroups[0].id).toBe('task:task-1::execution:exec-1');
		expect(rows[0].taskExecutionGroups[0].messageIds).toEqual(['created', 'running']);
	});

	it('groups concurrent executions for the same still-open task', () => {
		const rows = render([
			taskMessage('run-1', 'running', 'exec-1'),
			taskMessage('run-2', 'running', 'exec-2', 2)
		]);
		expect(rows).toHaveLength(1);
		expect(rows[0].taskExecutionGroups.map((group) => group.id)).toEqual([
			'task:task-1::execution:exec-1',
			'task:task-1::execution:exec-2'
		]);
	});

	it('starts a new task container after every run in the prior container is terminal', () => {
		const rows = render([
			taskMessage('done-1', 'completed', 'exec-1'),
			taskMessage('run-2', 'running', 'exec-2', 2)
		]);
		expect(rows).toHaveLength(2);
		expect(rows.map((row) => row.message.id)).toEqual(['done-1', 'run-2']);
	});

	it('never combines different task ids', () => {
		const rows = render([
			taskMessage('one', 'running', 'exec-1', 1, 'task-1'),
			taskMessage('two', 'running', 'exec-1', 2, 'task-2')
		]);
		expect(rows).toHaveLength(2);
	});

	it('leaves malformed task updates as ordinary rows', () => {
		const malformed = message('bad', 'system', { type: 'task_status_update', status: 'running' });
		const rows = render([malformed]);
		expect(rows[0]).toMatchObject({
			id: 'bad',
			messageIds: ['bad'],
			taskExecutionGroups: []
		});
	});
});

describe('chat in-turn activity attachment', () => {
	it('folds non-terminal task activity between a user turn and assistant reply', () => {
		const rows = attachInTurnActivityToAssistantMessages(render([
			textMessage('user', 'user', 'do it'),
			taskMessage('activity', 'running', 'exec-1', 2),
			textMessage('assistant', 'assistant', 'done', 3)
		]));
		const assistant = rows.find((row) => row.message.id === 'assistant');
		expect(assistant?.attachedActivityMessages.map((entry) => entry.id)).toEqual(['activity']);
		expect(attachedActivityMessageIds(rows)).toEqual(new Set(['activity']));
	});

	it('keeps terminal activity standalone so its result remains inspectable', () => {
		const rows = attachInTurnActivityToAssistantMessages(render([
			textMessage('user', 'user', 'do it'),
			taskMessage('activity', 'completed', 'exec-1', 2),
			textMessage('assistant', 'assistant', 'done', 3)
		]));
		expect(rows.find((row) => row.message.id === 'assistant')?.attachedActivityMessages).toEqual([]);
		expect(attachedActivityMessageIds(rows)).toEqual(new Set());
	});

	it('keeps task activity after an assistant reply as background work', () => {
		const rows = attachInTurnActivityToAssistantMessages(render([
			textMessage('user', 'user', 'start it'),
			textMessage('assistant', 'assistant', 'started', 2),
			taskMessage('background', 'running', 'exec-1', 3)
		]));
		expect(attachedActivityMessageIds(rows)).toEqual(new Set());
	});

	it('hides in-flight activity at the end of the visible transcript window', () => {
		const rows = attachInTurnActivityToAssistantMessages(render([
			textMessage('user', 'user', 'start it'),
			taskMessage('activity', 'running', 'exec-1', 2)
		]));
		expect(rows.find((row) => row.messageIds.includes('activity'))?.inTurnActivityHidden).toBe(true);
		expect(attachedActivityMessageIds(rows)).toEqual(new Set(['activity']));
	});

	it('unhides orphaned activity when a new user turn begins', () => {
		const rows = attachInTurnActivityToAssistantMessages(render([
			textMessage('user-1', 'user', 'first'),
			taskMessage('orphan', 'running', 'exec-1', 2),
			textMessage('user-2', 'user', 'second', 3)
		]));
		expect(rows.find((row) => row.messageIds.includes('orphan'))?.inTurnActivityHidden).toBe(false);
		expect(attachedActivityMessageIds(rows)).toEqual(new Set());
	});

	it('attaches activity when pagination begins in the middle of a turn', () => {
		const rows = attachInTurnActivityToAssistantMessages(render([
			taskMessage('activity', 'running', 'exec-1'),
			textMessage('assistant', 'assistant', 'done', 2)
		]));
		expect(rows.find((row) => row.message.id === 'assistant')?.attachedActivityMessages)
			.toHaveLength(1);
	});
});

describe('chat transcript visibility and identity', () => {
	it.each([
		[message('legacy', 'system', { type: 'text', text: 'Agent event: StepStarted' }), true],
		[message('normal', 'system', { type: 'text', text: 'A useful system note' }), false],
		[message('assistant', 'assistant', { type: 'text', text: 'Agent event: visible text' }), false],
		[message('resolved', 'system', { type: 'escalation', resolved: true }), true],
		[message('stale', 'system', { type: 'escalation', stale: true }), true],
		[message('active', 'system', { type: 'escalation', question: 'Continue?' }), false],
		[message('pip', 'system', { type: 'escalation_resolved' }), true]
	])('applies transcript visibility policy to %s', (entry, expected) => {
		expect(isHiddenTranscriptMessage(entry)).toBe(expected);
	});

	it('finds ids coalesced into a render row', () => {
		const row = render([
			taskMessage('first', 'running', 'exec-1'),
			taskMessage('second', 'completed', 'exec-1', 2)
		])[0];
		expect(renderMessageContainsId(row, 'first')).toBe(true);
		expect(renderMessageContainsId(row, 'second')).toBe(true);
		expect(renderMessageContainsId(row, 'missing')).toBe(false);
		expect(renderMessageContainsId(row, null)).toBe(false);
	});
});

describe('chat content rendering helpers', () => {
	it('renders every structured content type into a concise text fallback', () => {
		expect(getMessageText({ type: 'tool_call_executed', tool_name: 'search', summary: 'Found 3' }))
			.toBe('Action completed: search\nFound 3');
		expect(getMessageText({
			type: 'rich_tool_result',
			content_blocks: [
				{ type: 'text', text: 'Summary' },
				{ type: 'url', label: 'Open source', url: 'https://example.com' },
				{ type: 'file', display_name: 'report.csv' }
			]
		})).toBe('Summary\nOpen source\nFile: report.csv');
		expect(getMessageText({ type: 'attachment', label: 'Invoice' })).toBe('Attachment: Invoice');
		expect(getMessageText({ type: 'task_status_update', task_id: 't1', status: 'running', summary: 'Step 1' }))
			.toBe('Task t1: running\nStep 1');
		expect(getMessageText({ type: 'escalation', question: 'Approve?' })).toBe('Action Required: Approve?');
	});

	it('compacts escalation completion details and removes diagnostic tails', () => {
		expect(getEscalationResolvedSummary(
			'Execution completed: goal_achieved — Goal achieved: Initial boilerplate. Delivered the report. Shared it with finance. Test results: omitted'
		)).toBe('Execution completed: goal_achieved — Delivered the report. Shared it with finance.');
		expect(getEscalationResolvedSummary('   ')).toBe('Escalation resolved');
	});

	it('extracts rich, task-output, and attachment content blocks', () => {
		const rich = [{ type: 'text' as const, text: 'hello' }];
		expect(getMessageContentBlocks({ type: 'rich_tool_result', content_blocks: rich })).toEqual(rich);
		const outputs = [{ type: 'file' as const, relative_path: 'out.csv' }];
		expect(getMessageContentBlocks({ type: 'task_status_update', output_files: outputs })).toEqual(outputs);
		expect(getMessageContentBlocks({
			type: 'attachment', filename: 'invoice.pdf', label: 'Invoice', mime_type: 'application/pdf', size: 12
		})).toEqual([{
			type: 'file',
			source: { type: 'session_output' },
			relative_path: 'invoice.pdf',
			display_name: 'Invoice',
			mime_type: 'application/pdf',
			absolute_path: undefined,
			label: 'Invoice',
			size: 12
		}]);
		expect(getMessageContentBlocks({ type: 'text', text: 'none' })).toEqual([]);
	});
});

describe('chat API message conversion', () => {
	it('unwraps Rust Text content and normalizes direction', () => {
		expect(convertMessage({
			id: 'm1', session_id: 's1', direction: 'Assistant',
			content: { Text: 'Hello' }, created_at: 10
		})).toMatchObject({
			id: 'm1', session_id: 's1', direction: 'assistant',
			content: { type: 'text', text: 'Hello' }, created_at: 10
		});
	});

	it('falls back to system direction and empty text for malformed fields', () => {
		expect(convertMessage({
			id: 'm1', session_id: 's1', direction: 'sideways', created_at: 10
		})).toMatchObject({ direction: 'system', content: { type: 'text', text: '' } });
	});

	it('preserves and trims turn, surface, presence, and voice metadata', () => {
		const converted = convertMessage({
			id: 'm1', session_id: 's1', direction: 'assistant', content: 'Hello', created_at: 10,
			chat_turn_id: ' turn-1 ',
			source_surface: ' voice ',
			presence_session_id: ' presence-1 ',
			voice_origin: true,
			speech_segments: [{ text: 'Hello', pace: 'fast' }]
		});
		expect(converted).toMatchObject({
			chat_turn_id: 'turn-1',
			source_surface: 'voice',
			presence_session_id: 'presence-1',
			voice_origin: true,
			speech_segments: [{ text: 'Hello', pace: 'fast' }]
		});
	});

	it('drops empty optional metadata instead of storing whitespace', () => {
		const converted = convertMessage({
			id: 'm1', session_id: 's1', direction: 'user', content: 'Hello', created_at: 10,
			chat_turn_id: '', source_surface: '   ', presence_session_id: ' '
		});
		expect(converted.chat_turn_id).toBeUndefined();
		expect(converted.source_surface).toBeUndefined();
		expect(converted.presence_session_id).toBeUndefined();
	});
});
