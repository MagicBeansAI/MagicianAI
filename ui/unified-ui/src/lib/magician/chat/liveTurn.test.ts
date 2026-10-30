import { describe, expect, it } from 'vitest';
import type { ChatMessage } from '$lib/stores/chatStore';
import {
	isTaskWaitingOnUser,
	latestActiveTaskTurn,
	latestActiveTaskTurnId,
	latestOpenUserTurnId
} from './liveTurn';
import { derivePresence } from './next/presence';

function taskStatus(
	id: string,
	taskId: string,
	status: string,
	createdAt: number,
	chatTurnId?: string
): ChatMessage {
	return {
		id,
		session_id: 'session-1',
		direction: 'system',
		content: {
			type: 'task_status_update',
			task_id: taskId,
			status
		},
		created_at: createdAt,
		chat_turn_id: chatTurnId
	};
}

describe('latestActiveTaskTurnId', () => {
	it('ignores uncorrelated historical tasks instead of pinning the newest chat turn', () => {
		const messages = [
			taskStatus('old-planning', 'task-old', 'planning', 10),
			taskStatus('old-paused', 'task-paused', 'paused', 20)
		];

		expect(latestActiveTaskTurnId(messages)).toBeNull();
	});

	it('drops a task the task store already reads as finished', () => {
		// The run's terminal status row never reached the chat.
		const messages = [taskStatus('running', 'task-1', 'running', 10, 'turn-1')];
		expect(latestActiveTaskTurnId(messages)).toBe('turn-1');
		expect(
			latestActiveTaskTurnId(messages, undefined, (message) => message.content.task_id === 'task-1')
		).toBeNull();
	});

	it('uses the latest status per task so an old running row cannot survive completion', () => {
		const messages = [
			taskStatus('running', 'task-1', 'running', 10, 'turn-1'),
			taskStatus('completed', 'task-1', 'completed', 20, 'turn-1')
		];

		expect(latestActiveTaskTurnId(messages)).toBeNull();
	});

	it('keeps the actual spawning turn live while its correlated task is active', () => {
		const messages = [
			taskStatus('old', 'task-old', 'planning', 10),
			taskStatus('active-1', 'task-1', 'running', 30, 'turn-1'),
			taskStatus('active-2', 'task-2', 'paused', 40, 'turn-2')
		];

		expect(latestActiveTaskTurnId(messages)).toBe('turn-2');
	});

	it('retains task-local turn provenance when a later running update omits correlation', () => {
		const messages = [
			taskStatus('correlated-start', 'task-1', 'running', 10, 'turn-1'),
			taskStatus('uncorrelated-progress', 'task-1', 'running', 20)
		];

		expect(latestActiveTaskTurnId(messages)).toBe('turn-1');
	});

	it('lets an uncorrelated terminal update settle a previously correlated task', () => {
		const messages = [
			taskStatus('correlated-start', 'task-1', 'running', 10, 'turn-1'),
			taskStatus('uncorrelated-terminal', 'task-1', 'completed', 20)
		];

		expect(latestActiveTaskTurnId(messages)).toBeNull();
	});

	it('accepts the chat turn side-map resolver used for optimistic messages', () => {
		const message = taskStatus('active', 'task-1', 'running', 10);

		expect(
			latestActiveTaskTurnId([message], (candidate) =>
				candidate.id === 'active' ? 'turn-from-side-map' : null
			)
		).toBe('turn-from-side-map');
	});
});

describe('latestActiveTaskTurn', () => {
    it('settles the spawning turn after cancellation returns the reusable task to ready', () => {
        const messages = [
            taskStatus('run', 'task-1', 'running', 10, 'turn-1'),
            taskStatus('pause', 'task-1', 'paused', 20, 'turn-1'),
            taskStatus('cancel', 'task-1', 'cancelled', 30, 'turn-1'),
            taskStatus('reset', 'task-1', 'ready', 40, 'turn-1')
        ];
        expect(latestActiveTaskTurn(messages)).toBeNull();
    });

    it.each(['ready', 'pending', 'rejected', 'archived'])('does not keep an inactive %s task live', (status) => {
        expect(latestActiveTaskTurn([taskStatus('inactive', 'task-1', status, 10, 'turn-1')])).toBeNull();
    });

	it('reports the latest status of the active task', () => {
		const messages = [
			taskStatus('run', 'task-1', 'running', 10, 'turn-1'),
			taskStatus('pause', 'task-1', 'paused', 20, 'turn-1')
		];

		expect(latestActiveTaskTurn(messages)).toEqual({ turnId: 'turn-1', status: 'paused' });
	});

	it('is null once the task settles', () => {
		const messages = [
			taskStatus('pause', 'task-1', 'paused', 10, 'turn-1'),
			taskStatus('done', 'task-1', 'cancelled', 20, 'turn-1')
		];

		expect(latestActiveTaskTurn(messages)).toBeNull();
	});
});

describe('isTaskWaitingOnUser', () => {
	it('reads paused runs and plans awaiting review as waiting on the user', () => {
		expect(isTaskWaitingOnUser('paused')).toBe(true);
		expect(isTaskWaitingOnUser('waiting_for_user')).toBe(true);
		expect(isTaskWaitingOnUser('draft')).toBe(true);
		expect(isTaskWaitingOnUser(' Ready ')).toBe(false);
	});

	it('reads running work and unknown statuses as not waiting', () => {
		expect(isTaskWaitingOnUser('running')).toBe(false);
		expect(isTaskWaitingOnUser('planning')).toBe(false);
		expect(isTaskWaitingOnUser(undefined)).toBe(false);
	});
});

function row(
	id: string,
	direction: ChatMessage['direction'],
	createdAt: number,
	chatTurnId?: string,
	sourceSurface?: string
): ChatMessage {
	return {
		id,
		session_id: 'session-1',
		direction,
		content: { type: 'text', text: id },
		created_at: createdAt,
		chat_turn_id: chatTurnId,
		source_surface: sourceSurface
	};
}

const MIN = 60 * 1000;

describe('latestOpenUserTurnId', () => {
	it('does not hold the orb on "thinking" for a split realtime-voice utterance', () => {
		// The tail of a real web session (2026-10-01): the Live transcript
		// split one utterance into two user rows, and the reply hung off the
		// second fragment, so the first fragment never got an assistant row.
		const t0 = 1_000_000;
		const messages = [
			row('hi', 'user', t0, 'ios-chat-1', 'ios'),
			row('hi-reply', 'assistant', t0 + 6_000, 'ios-chat-1', 'ios'),
			row('start-shell', 'user', t0 + 20 * MIN, 'voice-turn-be4a', 'realtime_voice'),
			row('wait-60', 'user', t0 + 20 * MIN + 1_000, 'voice-turn-0412', 'realtime_voice'),
			row('wait-60-reply', 'assistant', t0 + 20 * MIN + 2_000, 'voice-turn-0412', 'realtime_voice'),
			row('another', 'user', t0 + 20 * MIN + 10_000, 'voice-turn-ab22', 'realtime_voice'),
			row('another-reply', 'assistant', t0 + 20 * MIN + 13_000, 'voice-turn-ab22', 'realtime_voice')
		];
		const now = t0 + 21 * MIN;

		const openTurn = latestOpenUserTurnId(messages, { now });
		expect(openTurn).toBeNull();
		expect(
			derivePresence({
				hasPendingHitl: false,
				liveTurn: !!openTurn,
				streaming: false,
				toolsRunning: false
			})
		).toBe('idle');
	});

	it('ignores voice rows by surface even without a voice turn-id prefix', () => {
		const messages = [row('fragment', 'user', 100, 'turn-x', 'authenticated_realtime_voice')];
		expect(latestOpenUserTurnId(messages, { now: 200 })).toBeNull();
	});

	it('an older unanswered typed row is superseded by a later answered one', () => {
		const messages = [
			row('orphan', 'user', 100, 'turn-1'),
			row('next', 'user', 200, 'turn-2'),
			row('next-reply', 'assistant', 300, 'turn-2')
		];
		expect(latestOpenUserTurnId(messages, { now: 400 })).toBeNull();
	});

	it('keeps a just-sent typed turn open until its reply lands', () => {
		const sent = [row('ask', 'user', 100, 'turn-1'), row('streaming-1', 'assistant', 101, 'turn-1')];
		expect(latestOpenUserTurnId(sent, { now: 200 })).toBe('turn-1');
		const replied = [...sent, row('ask-reply', 'assistant', 300, 'turn-1')];
		expect(latestOpenUserTurnId(replied, { now: 400 })).toBeNull();
	});

	it('a typed turn sent after voice chatter is still open', () => {
		const messages = [
			row('ask', 'user', 100, 'turn-1'),
			row('fragment', 'user', 200, 'voice-turn-1', 'realtime_voice')
		];
		expect(latestOpenUserTurnId(messages, { now: 300 })).toBe('turn-1');
	});

	it('expires once the clock passes the age cutoff, with no new messages', () => {
		const messages = [row('ask', 'user', 0, 'turn-1')];
		expect(latestOpenUserTurnId(messages, { now: 9 * MIN })).toBe('turn-1');
		expect(latestOpenUserTurnId(messages, { now: 11 * MIN })).toBeNull();
	});

	it('closes a turn whose spawned task reached a terminal status', () => {
		const messages = [row('ask', 'user', 100, 'turn-1')];
		expect(
			latestOpenUserTurnId(messages, { now: 200, turnsWithTerminalTask: new Set(['turn-1']) })
		).toBeNull();
	});

	it('resolves turn ids through the side-map resolver', () => {
		const messages = [row('temp-user-1', 'user', 100)];
		expect(
			latestOpenUserTurnId(messages, {
				now: 200,
				resolveTurnId: (m) => (m.id === 'temp-user-1' ? 'turn-side' : null)
			})
		).toBe('turn-side');
	});
});
