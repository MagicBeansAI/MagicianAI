import { get } from 'svelte/store';
import { beforeEach, describe, expect, it } from 'vitest';

import {
	chatTurnStateStore,
	handleChatTurnStateEnvelope,
	parseChatTurnState,
	recordChatTurnStateFromResponse,
	resetChatTurnStates,
	runningTurnIdForSession,
	settledTurnIds,
	turnStatusNotice
} from './turnState';

const running = { chat_turn_id: 'turn-1', status: 'running', started_at: 10 };

describe('chat turn state', () => {
	beforeEach(() => resetChatTurnStates());

	it('rehydrates a running turn from the session response after a reload', () => {
		recordChatTurnStateFromResponse({ messages: [], turn_state: running }, 'sess-1');
		const states = get(chatTurnStateStore);
		expect(runningTurnIdForSession(states, 'sess-1')).toBe('turn-1');
		expect(runningTurnIdForSession(states, 'sess-2')).toBeNull();
	});

	it('settles from the live event, and a stale running poll does not undo it', () => {
		recordChatTurnStateFromResponse({ turn_state: running }, 'sess-1');
		handleChatTurnStateEnvelope({
			event_type: 'chat.turn.state_changed',
			payload: { ...running, chat_session_id: 'sess-1', status: 'stopped', ended_at: 20, reason: 'user_stop' }
		});
		recordChatTurnStateFromResponse({ turn_state: running }, 'sess-1');
		const states = get(chatTurnStateStore);
		expect(runningTurnIdForSession(states, 'sess-1')).toBeNull();
		expect(settledTurnIds(states).has('turn-1')).toBe(true);
		expect(turnStatusNotice(states.get('turn-1'))).toBe('Stopped');
	});

	it('drops a running turn the server no longer reports (restart)', () => {
		recordChatTurnStateFromResponse({ turn_state: running }, 'sess-1');
		recordChatTurnStateFromResponse({ turn_state: null }, 'sess-1');
		expect(runningTurnIdForSession(get(chatTurnStateStore), 'sess-1')).toBeNull();
	});

	it('ignores other events and malformed payloads', () => {
		handleChatTurnStateEnvelope({ event_type: 'chat.turn.stopped', payload: running });
		expect(get(chatTurnStateStore).size).toBe(0);
		expect(parseChatTurnState({ chat_turn_id: 't', status: 'weird' }, 's')).toBeNull();
		expect(parseChatTurnState({ status: 'running' }, 's')).toBeNull();
	});

	it('only a reply-less settled turn gets a status line', () => {
		const base = { chat_turn_id: 't', chat_session_id: 's', started_at: 1 };
		expect(turnStatusNotice({ ...base, status: 'completed' })).toBeNull();
		expect(turnStatusNotice({ ...base, status: 'running' })).toBeNull();
		// The watchdog persisted its own notice message.
		expect(turnStatusNotice({ ...base, status: 'stopped', reason: 'turn_stalled' })).toBeNull();
		expect(turnStatusNotice({ ...base, status: 'failed', reason: 'error' })).toMatch(/failed/);
		expect(turnStatusNotice({ ...base, status: 'failed', reason: 'interrupted' })).toMatch(/Interrupted/);
		// Realtime voice turns render on the call surface.
		expect(turnStatusNotice({ ...base, chat_turn_id: 'voice-turn-1', status: 'stopped' })).toBeNull();
	});
});
