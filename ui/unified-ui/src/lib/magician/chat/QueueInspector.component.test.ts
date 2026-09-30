import { render, screen, fireEvent, waitFor } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import QueueInspector from './QueueInspector.svelte';
import { chatStore } from '$lib/stores/chatStore';

vi.mock('$lib/stores/chatStore', async () => {
    const { writable } = await import('svelte/store');
    const state = writable({ queueRevision: 0, activeRunSessionId: null as string | null });
    return { chatStore: {
        subscribe: state.subscribe,
        listQueuedMessages: vi.fn(async () => {
            state.update(value => ({ ...value, activeRunSessionId: 'parent' }));
            return [{ id: 'queued-1', session_id: 'parent', text: 'Follow up', queued_at: Date.now() }];
        }),
        actOnQueuedMessage: vi.fn(async () => {}),
    } };
});

vi.mock('$lib/shared/stores/notifications', () => ({ showError: vi.fn(), showInfo: vi.fn(), showSuccess: vi.fn() }));

describe('QueueInspector', () => {
    it('does not refetch recursively when a read publishes run liveness, and offers explicit actions', async () => {
        render(QueueInspector, { sessionId: 'parent', pollIntervalMs: 0 });
        await waitFor(() => expect(screen.getByRole('button', { name: 'Expand queued messages' })).toBeEnabled());
        expect(vi.mocked(chatStore.listQueuedMessages).mock.calls.length).toBeLessThanOrEqual(2);
        await fireEvent.click(screen.getByRole('button', { name: 'Expand queued messages' }));
        await fireEvent.click(screen.getByRole('button', { name: 'Run in parallel' }));
        expect(chatStore.actOnQueuedMessage).toHaveBeenCalledWith('parent', 'queued-1', 'parallel');
        expect(screen.getByRole('button', { name: 'Stop & send' })).toBeInTheDocument();
    });
});
