import { describe, expect, it } from 'vitest';
import {
    type ChatSession,
    collapseTaskProgressMessages,
    getMessageText,
    mergeThreadSessionsAuthoritatively,
    normalizeMessages,
    type ChatMessage
} from './chatStore';

function systemMessage(
    id: string,
    createdAt: number,
    content: ChatMessage['content']
): ChatMessage {
    return {
        id,
        session_id: 'session-1',
        direction: 'system',
        content,
        created_at: createdAt
    };
}

function assistantTextMessage(id: string, createdAt: number, text: string): ChatMessage {
    return {
        id,
        session_id: 'session-1',
        direction: 'assistant',
        content: {
            type: 'text',
            text
        },
        created_at: createdAt
    };
}

describe('normalizeMessages', () => {
    it('marks escalations resolved when a matching resolution notice exists in history', () => {
        const messages = normalizeMessages([
            systemMessage('escalation-1', 1, {
                type: 'escalation',
                execution_id: 'exec-1',
                pause_state_id: 'pause-1',
                question: 'Proceed with the risky action?',
                options: [{ id: 'approve', label: 'Approve' }]
            }),
            systemMessage('resolved-1', 2, {
                type: 'escalation_resolved',
                execution_id: 'exec-1',
                summary: 'Execution completed: approved'
            })
        ]);

        expect(messages).toHaveLength(2);
        expect(messages[0]?.content.type).toBe('escalation');
        expect(messages[0]?.content.resolved).toBe(true);
        expect(messages[1]?.content.type).toBe('escalation_resolved');
    });

    it('marks request-backed escalations resolved without an execution id', () => {
        const messages = normalizeMessages([
            systemMessage('escalation-1', 1, {
                type: 'escalation',
                request_id: 'request-1',
                pause_state_id: 'pause-1',
                question: 'Provide guidance before I continue.',
                options: [{ id: 'respond', label: 'Provide guidance', requires_input: true }]
            }),
            systemMessage('resolved-1', 2, {
                type: 'escalation_resolved',
                request_id: 'request-1',
                summary: 'Guidance received'
            })
        ]);

        expect(messages).toHaveLength(2);
        expect(messages[0]?.content.type).toBe('escalation');
        expect(messages[0]?.content.resolved).toBe(true);
        expect(messages[1]?.content.type).toBe('escalation_resolved');
    });

    it('marks pause-backed escalations resolved without an execution id', () => {
        const messages = normalizeMessages([
            systemMessage('escalation-1', 1, {
                type: 'escalation',
                pause_state_id: 'pause-1',
                question: 'Provide guidance before I continue.',
                options: [{ id: 'respond', label: 'Provide guidance', requires_input: true }]
            }),
            systemMessage('resolved-1', 2, {
                type: 'escalation_resolved',
                pause_state_id: 'pause-1',
                summary: 'Guidance received'
            })
        ]);

        expect(messages).toHaveLength(2);
        expect(messages[0]?.content.type).toBe('escalation');
        expect(messages[0]?.content.resolved).toBe(true);
        expect(messages[1]?.content.type).toBe('escalation_resolved');
    });

    it('keeps the resolution reason on the resolved escalation card', () => {
        const messages = normalizeMessages([
            systemMessage('escalation-1', 1, {
                type: 'escalation',
                execution_id: 'exec-1',
                pause_state_id: 'pause-1',
                question: 'Continue?',
                options: [{ id: 'continue', label: 'Keep Trying' }]
            }),
            systemMessage('resolved-1', 2, {
                type: 'escalation_resolved',
                execution_id: 'exec-1',
                pause_state_id: 'pause-1',
                summary: 'Task deleted'
            })
        ]);

        expect(messages).toHaveLength(2);
        expect(messages[0]?.content.type).toBe('escalation');
        expect(messages[0]?.content.resolved).toBe(true);
        expect(messages[0]?.content.inactive_reason).toBe('Task deleted');
    });

    it('marks older escalations stale when a newer escalation exists for the same execution', () => {
        const messages = normalizeMessages([
            systemMessage('escalation-1', 1, {
                type: 'escalation',
                execution_id: 'exec-1',
                pause_state_id: 'pause-1',
                question: 'First question',
                options: [{ id: 'respond', label: 'Respond', requires_input: true }]
            }),
            systemMessage('escalation-2', 2, {
                type: 'escalation',
                execution_id: 'exec-1',
                pause_state_id: 'pause-2',
                question: 'Second question',
                options: [{ id: 'respond', label: 'Respond', requires_input: true }]
            })
        ]);

        expect(messages).toHaveLength(2);
        expect(messages[0]?.content.type).toBe('escalation');
        expect(messages[0]?.content.stale).toBe(true);
        expect(messages[0]?.content.resolved).not.toBe(true);
        expect(messages[1]?.content.type).toBe('escalation');
        expect(messages[1]?.content.stale).toBe(false);
    });

    it('marks older unresolved escalations stale even across different executions', () => {
        const messages = normalizeMessages([
            systemMessage('escalation-1', 1, {
                type: 'escalation',
                execution_id: 'exec-1',
                pause_state_id: 'pause-1',
                question: 'Older prompt',
                options: [{ id: 'respond', label: 'Respond', requires_input: true }]
            }),
            systemMessage('escalation-2', 2, {
                type: 'escalation',
                execution_id: 'exec-2',
                pause_state_id: 'pause-2',
                question: 'Latest prompt',
                options: [{ id: 'respond', label: 'Respond', requires_input: true }]
            })
        ]);

        expect(messages).toHaveLength(2);
        expect(messages[0]?.content.type).toBe('escalation');
        expect(messages[0]?.content.stale).toBe(false);
        expect(messages[1]?.content.type).toBe('escalation');
        expect(messages[1]?.content.stale).toBe(false);
    });

    it('keeps an unresolved escalation active when later unrelated chat messages arrive', () => {
        const messages = normalizeMessages([
            systemMessage('escalation-1', 1, {
                type: 'escalation',
                execution_id: 'exec-1',
                pause_state_id: 'pause-1',
                question: 'Authenticate Gmail before I continue',
                options: [{ id: 'respond', label: 'Respond', requires_input: true }]
            }),
            assistantTextMessage('assistant-1', 2, 'I will wait for your next instruction.')
        ]);

        expect(messages).toHaveLength(2);
        expect(messages[0]?.content.type).toBe('escalation');
        expect(messages[0]?.content.stale).toBe(false);
        expect(messages[1]?.content.type).toBe('text');
    });

    it('marks older guidance escalations stale when later chat activity exists', () => {
        const messages = normalizeMessages([
            systemMessage('escalation-1', 1, {
                type: 'escalation',
                execution_id: 'exec-1',
                pause_state_id: 'pause-1',
                escalation_type: 'loop_detected',
                question: 'I am stuck. Provide guidance or mark done?',
                options: [
                    { id: 'guidance', label: 'Provide Guidance', requires_input: true },
                    { id: 'done', label: 'Mark Done' }
                ]
            }),
            assistantTextMessage('assistant-1', 2, 'I will wait for your next instruction.')
        ]);

        expect(messages).toHaveLength(2);
        expect(messages[0]?.content.type).toBe('escalation');
        expect(messages[0]?.content.stale).toBe(true);
        expect(messages[0]?.content.resolved).not.toBe(true);
        expect(messages[1]?.content.type).toBe('text');
    });

    it('keeps the latest guidance escalation actionable', () => {
        const messages = normalizeMessages([
            assistantTextMessage('assistant-1', 1, 'Working on it.'),
            systemMessage('escalation-1', 2, {
                type: 'escalation',
                execution_id: 'exec-1',
                pause_state_id: 'pause-1',
                escalation_type: 'cannot_proceed',
                question: 'I need help. Provide guidance or mark done?',
                options: [
                    { id: 'guidance', label: 'Provide Guidance', requires_input: true },
                    { id: 'done', label: 'Mark Done' }
                ]
            })
        ]);

        expect(messages).toHaveLength(2);
        expect(messages[1]?.content.type).toBe('escalation');
        expect(messages[1]?.content.stale).toBe(false);
    });

    it('does not let an earlier resolution auto-resolve a later pause on the same execution', () => {
        const messages = normalizeMessages([
            systemMessage('escalation-1', 1, {
                type: 'escalation',
                execution_id: 'exec-1',
                pause_state_id: 'pause-1',
                question: 'First question',
                options: [{ id: 'respond', label: 'Respond', requires_input: true }]
            }),
            systemMessage('resolved-1', 2, {
                type: 'escalation_resolved',
                execution_id: 'exec-1',
                summary: 'Execution completed: approved'
            }),
            systemMessage('escalation-2', 3, {
                type: 'escalation',
                execution_id: 'exec-1',
                pause_state_id: 'pause-2',
                question: 'Second question',
                options: [{ id: 'respond', label: 'Respond', requires_input: true }]
            })
        ]);

        expect(messages).toHaveLength(3);
        expect(messages[0]?.content.type).toBe('escalation');
        expect(messages[0]?.content.resolved).toBe(true);
        expect(messages[0]?.content.stale).toBe(false);
        expect(messages[2]?.content.type).toBe('escalation');
        expect(messages[2]?.content.resolved).toBe(false);
        expect(messages[2]?.content.stale).toBe(false);
    });

    it('uses pause ids before execution ids when resolving a resumed escalation', () => {
        const messages = normalizeMessages([
            systemMessage('escalation-1', 1, {
                type: 'escalation',
                execution_id: 'exec-1',
                pause_state_id: 'pause-1',
                escalation_type: 'loop_detected',
                question: 'I am stuck. Provide guidance or mark done?',
                options: [
                    { id: 'guidance', label: 'Provide Guidance', requires_input: true },
                    { id: 'done', label: 'Mark Done' }
                ]
            }),
            systemMessage('escalation-2', 2, {
                type: 'escalation',
                execution_id: 'exec-1',
                pause_state_id: 'pause-2',
                question: 'New question after resume',
                options: [{ id: 'respond', label: 'Respond', requires_input: true }]
            }),
            systemMessage('resolved-1', 3, {
                type: 'escalation_resolved',
                execution_id: 'exec-1',
                pause_state_id: 'pause-1',
                summary: 'Agent resumed (external_action)'
            })
        ]);

        expect(messages).toHaveLength(3);
        expect(messages[0]?.content.type).toBe('escalation');
        expect(messages[0]?.content.resolved).toBe(true);
        expect(messages[0]?.content.stale).toBe(false);
        expect(messages[1]?.content.type).toBe('escalation');
        expect(messages[1]?.content.resolved).toBe(false);
        expect(messages[1]?.content.stale).toBe(false);
    });

    it('compacts delegated agent terminal boilerplate text', () => {
        expect(getMessageText({
            type: 'text',
            text: "Delegated agent 'executive-assistant' completed.\n\nExecution completed."
        })).toBe("Delegated agent 'executive-assistant' completed.");
    });

    it('keeps only the useful delegated agent summary details', () => {
        expect(getMessageText({
            type: 'text',
            text: "Delegated agent 'executive-assistant' completed.\n\nExecution completed: goal_achieved — Located the latest Swiggy and Zomato receipts in Gmail and prepared a short summary. Test results: omitted"
        })).toBe("Delegated agent 'executive-assistant' completed. goal_achieved — Located the latest Swiggy and Zomato receipts in Gmail and prepared a short summary.");
    });

    it('compacts handover messages that include the full context prompt', () => {
        expect(getMessageText({
            type: 'text',
            text: "Handed over execution ownership from 'personal-assistant' to 'gmail-specialist'. Context: Search Gmail for the latest Swiggy and Zomato receipts, cross-check them against recent messages, and summarize the order confirmations with dates, merchant names, and totals."
        })).toBe("Handed over execution ownership from 'personal-assistant' to 'gmail-specialist'. Search Gmail for the latest Swiggy and Zomato receipts, cross-check them against recent messages, and summarize the order confirmations with dates, merchant names, and totals.");
    });

    it('compacts yield-back evidence into a short summary', () => {
        expect(getMessageText({
            type: 'text',
            text: "Yielded back from 'gmail-specialist' to 'personal-assistant' after specialist success. Evidence: Goal achieved: Located the latest Swiggy and Zomato order confirmations in Gmail and extracted the order dates and totals. Test results: omitted"
        })).toBe("Yielded back from 'gmail-specialist' to 'personal-assistant' after specialist success. Located the latest Swiggy and Zomato order confirmations in Gmail and extracted the order dates and totals.");
    });
});

describe('mergeThreadSessionsAuthoritatively', () => {
    it('drops local-only sessions for the reloaded thread while preserving other threads', () => {
        const existingSessions: ChatSession[] = [
            {
                id: 'thread-a-stale',
                principal: 'anonymous',
                workspace: 'default',
                ui_thread_id: 'thread-a',
                agent_id: 'agent-1',
                origin_channel: { channel_type: 'web' },
                status: 'active',
                created_at: 100,
                updated_at: 100,
                title: null
            },
            {
                id: 'thread-b-active',
                principal: 'anonymous',
                workspace: 'default',
                ui_thread_id: 'thread-b',
                agent_id: 'agent-2',
                origin_channel: { channel_type: 'web' },
                status: 'active',
                created_at: 200,
                updated_at: 200,
                title: null
            }
        ];

        const fetchedSessions: ChatSession[] = [
            {
                id: 'thread-a-restored',
                principal: 'anonymous',
                workspace: 'default',
                ui_thread_id: 'thread-a',
                agent_id: 'agent-1',
                origin_channel: { channel_type: 'web' },
                status: 'active',
                created_at: 300,
                updated_at: 300,
                title: null
            }
        ];

        const merged = mergeThreadSessionsAuthoritatively(
            existingSessions,
            fetchedSessions,
            'thread-a'
        );

        expect(merged.map(session => session.id)).toEqual([
            'thread-a-restored',
            'thread-b-active'
        ]);
    });
});

describe('normalizeMessages task card ordering', () => {
    function turnMessage(
        id: string,
        direction: ChatMessage['direction'],
        createdAt: number,
        content: ChatMessage['content'],
        chatTurnId?: string
    ): ChatMessage {
        return {
            id,
            session_id: 'session-1',
            direction,
            content,
            created_at: createdAt,
            ...(chatTurnId ? { chat_turn_id: chatTurnId } : {})
        };
    }
    const card = (id: string, at: number, status: string, turn?: string) =>
        turnMessage(id, 'system', at, { type: 'task_status_update', task_id: 'task-1', status }, turn);
    const noSideMap = (m: ChatMessage) => m.chat_turn_id ?? null;

    it('renders a mid-turn task card after the reply of the turn that started it', () => {
        const ordered = normalizeMessages([
            turnMessage('u1', 'user', 1, { type: 'text', text: 'do it' }, 'turn-1'),
            card('c1', 2, 'planning', 'turn-1'),
            turnMessage('a1', 'assistant', 5, { type: 'text', text: 'On it.' }, 'turn-1'),
            turnMessage('u2', 'user', 10, { type: 'text', text: 'next' }, 'turn-2')
        ], noSideMap);
        expect(ordered.map((m) => m.id)).toEqual(['u1', 'a1', 'c1', 'u2']);
    });

    it('keeps later card updates in time order after the next user turn', () => {
        const ordered = normalizeMessages([
            turnMessage('u1', 'user', 1, { type: 'text', text: 'do it' }, 'turn-1'),
            card('c1', 2, 'running', 'turn-1'),
            turnMessage('a1', 'assistant', 5, { type: 'text', text: 'On it.' }, 'turn-1'),
            turnMessage('u2', 'user', 10, { type: 'text', text: 'next' }, 'turn-2'),
            card('c2', 12, 'failed', 'turn-1')
        ], noSideMap);
        expect(ordered.map((m) => m.id)).toEqual(['u1', 'a1', 'c1', 'u2', 'c2']);
        const collapsed = collapseTaskProgressMessages(ordered);
        expect(collapsed.map((entry) => entry.message.id)).toEqual(['u1', 'a1', 'u2', 'c2']);
    });

    it('collapses a card after the reply even when every update is mid-turn', () => {
        const ordered = normalizeMessages([
            turnMessage('u1', 'user', 1, { type: 'text', text: 'do it' }, 'turn-1'),
            card('c1', 2, 'planning', 'turn-1'),
            card('c2', 3, 'running', 'turn-1'),
            turnMessage('a1', 'assistant', 5, { type: 'text', text: 'On it.' }, 'turn-1')
        ], noSideMap);
        expect(ordered.map((m) => m.id)).toEqual(['u1', 'a1', 'c1', 'c2']);
        const collapsed = collapseTaskProgressMessages(ordered);
        expect(collapsed.map((entry) => entry.message.id)).toEqual(['u1', 'a1', 'c2']);
    });

    it('keeps time order for cards without a chat_turn_id', () => {
        const ordered = normalizeMessages([
            turnMessage('u1', 'user', 1, { type: 'text', text: 'do it' }, 'turn-1'),
            card('c1', 2, 'running'),
            turnMessage('a1', 'assistant', 5, { type: 'text', text: 'On it.' }, 'turn-1')
        ], noSideMap);
        expect(ordered.map((m) => m.id)).toEqual(['u1', 'c1', 'a1']);
    });

    it('keeps time order while the reply is still a streaming placeholder', () => {
        const ordered = normalizeMessages([
            turnMessage('streaming-1', 'assistant', 1.5, { type: 'text', text: '' }, 'turn-1'),
            turnMessage('u1', 'user', 1, { type: 'text', text: 'do it' }, 'turn-1'),
            card('c1', 2, 'planning', 'turn-1')
        ], noSideMap);
        expect(ordered.map((m) => m.id)).toEqual(['u1', 'c1', 'streaming-1']);
    });

    it('resolves the reply turn through the caller resolver (sending-tab side-map)', () => {
        const sideMap = new Map([['a1', 'turn-1']]);
        const ordered = normalizeMessages([
            turnMessage('u1', 'user', 1, { type: 'text', text: 'do it' }, 'turn-1'),
            card('c1', 2, 'planning', 'turn-1'),
            turnMessage('a1', 'assistant', 5, { type: 'text', text: 'On it.' })
        ], (m) => m.chat_turn_id ?? sideMap.get(m.id) ?? null);
        expect(ordered.map((m) => m.id)).toEqual(['u1', 'a1', 'c1']);
    });
});
