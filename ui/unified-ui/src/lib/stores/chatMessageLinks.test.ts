import { describe, expect, it } from 'vitest';
import { matchesMessageTarget, messageTargetFromQuery, originalAnswerHref } from './chatMessageLinks';
import { convertMessage, collapseTaskProgressMessages } from './chatStore';
describe('original answer links', () => {
    const projection = {
        id: 'copy', session_id: 'parent', direction: 'assistant', created_at: 42,
        chat_turn_id: 'request', content: { type: 'text', text: 'Summary' },
        context_origin: { ui_thread_id: 'research/ideas', session_id: 'branch', request_id: 'request', message_id: 'saved' }
    };
    it('preserves live and reloaded provenance and safely encodes the whole destination', () => {
        const href = originalAnswerHref(convertMessage(projection))!;
        expect(href).toBe('/t/research%2Fideas/chat?session=branch&message=saved');
        expect(messageTargetFromQuery(new URL(href, 'https://example.test').searchParams)).toEqual({ messageId: 'saved' });
    });
    it('resolves legacy copies by exact timestamp and turn, never just the latest reply', () => {
        const legacy = { ...projection, context_origin: { ...projection.context_origin, message_id: undefined } };
        const target = messageTargetFromQuery(new URL(originalAnswerHref(legacy)!, 'https://example.test').searchParams)!;
        expect(matchesMessageTarget({ id: 'canonical', created_at: 42, chat_turn_id: 'request', direction: 'assistant' }, target)).toBe(true);
        expect(matchesMessageTarget({ id: 'other', created_at: 43, chat_turn_id: 'request', direction: 'assistant' }, target)).toBe(false);
        expect(matchesMessageTarget({ id: 'user', created_at: 42, chat_turn_id: 'request', direction: 'user' }, target)).toBe(false);
        expect(matchesMessageTarget({ id: 'other', created_at: 42, chat_turn_id: 'request', direction: 'assistant' }, { ...target, messageId: 'removed' })).toBe(false);
    });
    it('keeps an addressed task answer visible after a newer update', () => {
        const task = (id: string, at: number) => convertMessage({ id, session_id: 'branch', direction: 'system', created_at: at,
            content: { type: 'task_status_update', task_id: 'task', execution_id: 'run', status: 'completed', summary: id } });
        const rows = [task('original', 1), task('newer', 2)];
        expect(collapseTaskProgressMessages(rows)).toHaveLength(1);
        const linked = collapseTaskProgressMessages(rows, 'original');
        expect(linked.map(row => row.message.id)).toEqual(['original', 'newer']);
    });
    it('does not label canonical or user messages as forwarded answers', () => {
        expect(originalAnswerHref({ ...projection, session_id: 'branch' })).toBeNull();
        expect(originalAnswerHref({ ...projection, direction: 'user' })).toBeNull();
    });
});
