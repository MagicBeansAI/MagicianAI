// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import ChatFilesPage from './ChatFilesPage.svelte';
import ThreadSessionBar from '$lib/threads/ThreadSessionBar.svelte';

afterEach(() => { cleanup(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });
function inventory(files: unknown[] = [], extra = {}) {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({ session_id: 'chat-1', chat_title: 'Research', files, ...extra }))));
}
describe('chat Files page', () => {
    it('shows readable per-file sizes and the overall total independently of filtering', async () => {
        inventory([
            { id: 'session:upload', name: 'brief.pdf', path: 'brief.pdf', source: 'upload', size: 1024 },
            { id: 'task:t:o', name: 'report.md', path: 'report.md', source: 'task', size: 2048 }
        ]);
        render(ChatFilesPage, { props: { sessionId: 'chat-1' } });
        await screen.findByRole('button', { name: 'report.md' });
        expect(screen.getByLabelText('Total file size')).toHaveTextContent('2 files · Total: 3 KiB');
        expect(screen.getByText('1 KiB')).toHaveAttribute('title', '1,024 bytes');
        expect(screen.getByText('2 KiB')).toBeInTheDocument();
        await fireEvent.click(screen.getByRole('button', { name: 'Uploads' }));
        expect(screen.getByLabelText('Total file size')).toHaveTextContent('Total: 3 KiB');
    });
    it('marks unknown sizes without presenting a complete total', async () => {
        inventory([
            { id: 'session:known', name: 'empty.txt', path: 'empty.txt', source: 'upload', size: 0 },
            { id: 'task:t:o', name: 'unknown.md', path: 'unknown.md', source: 'task' }
        ]);
        render(ChatFilesPage, { props: { sessionId: 'chat-1' } });
        await screen.findByRole('button', { name: 'unknown.md' });
        expect(screen.getByLabelText('Total file size')).toHaveTextContent('Known size: 0 B · 1 size unavailable');
        expect(screen.getByText('Size unavailable')).toBeInTheDocument();
    });
    it('confirms deletion of exactly one file and updates the total', async () => {
        const fetch = vi.fn().mockResolvedValueOnce(Response.json({ files: [
            { id: 'session:one', name: 'one.pdf', path: 'one.pdf', source: 'upload', size: 1024 },
            { id: 'task:t:two', name: 'two.md', path: 'two.md', source: 'task', task_id: 't', size: 2048 }
        ] })).mockResolvedValue(new Response(null, { status: 204 }));
        vi.stubGlobal('fetch', fetch);
        render(ChatFilesPage, { props: { sessionId: 'chat-1' } });
        await fireEvent.click(await screen.findByRole('button', { name: 'Delete one.pdf' }));
        expect(fetch).toHaveBeenCalledTimes(1);
        await fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Cancel' }));
        expect(fetch).toHaveBeenCalledTimes(1);
        await fireEvent.click(screen.getByRole('button', { name: 'Delete one.pdf' }));
        await fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Delete file' }));
        await waitFor(() => expect(screen.queryByRole('button', { name: 'one.pdf' })).not.toBeInTheDocument());
        expect(fetch.mock.calls[1][0]).toBe('/api/magician/v2/chat/sessions/chat-1/assets/session%3Aone');
        expect(fetch.mock.calls[1][1].method).toBe('DELETE');
        expect(screen.getByRole('button', { name: 'two.md' })).toBeInTheDocument();
        expect(screen.getByLabelText('Total file size')).toHaveTextContent('1 file · Total: 2 KiB');
        await fireEvent.click(screen.getByRole('button', { name: 'Delete two.md' }));
        expect(screen.getByRole('dialog')).toHaveTextContent('original stays available under Work');
    });
    it('retains a file when the server refuses deletion', async () => {
        vi.stubGlobal('fetch', vi.fn().mockResolvedValueOnce(Response.json({ files: [
            { id: 'session:one', name: 'one.pdf', path: 'one.pdf', source: 'upload', size: 1024 }
        ] })).mockResolvedValue(Response.json({ message: 'Wait for this chat to finish.' }, { status: 409 })));
        render(ChatFilesPage, { props: { sessionId: 'chat-1' } });
        await fireEvent.click(await screen.findByRole('button', { name: 'Delete one.pdf' }));
        await fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Delete file' }));
        await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('Wait for this chat to finish.'));
        expect(screen.getByRole('button', { name: 'one.pdf' })).toBeInTheDocument();
        expect(screen.getByLabelText('Total file size')).toHaveTextContent('Total: 1 KiB');
    });
    it('dismisses a pending confirmation when switching chats', async () => {
        inventory([{ id: 'session:one', name: 'one.pdf', path: 'one.pdf', source: 'upload', size: 1 }]);
        const view = render(ChatFilesPage, { props: { sessionId: 'old-chat' } });
        await fireEvent.click(await screen.findByRole('button', { name: 'Delete one.pdf' }));
        expect(screen.getByRole('dialog')).toBeInTheDocument();
        await view.rerender({ sessionId: 'new-chat' });
        expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
        expect(vi.mocked(fetch).mock.calls.every(([, options]) => options?.method !== 'DELETE')).toBe(true);
    });
    it('ignores an old chat deletion result after switching chats', async () => {
        let finish!: (response: Response) => void;
        vi.stubGlobal('fetch', vi.fn().mockImplementation((url, options) => {
            if (options?.method === 'DELETE') return new Promise<Response>(resolve => { finish = resolve; });
            const name = String(url).includes('old-chat') ? 'old.pdf' : 'new.pdf';
            return Promise.resolve(Response.json({ files: [{ id: 'session:same', name, path: name, source: 'upload', size: 1024 }] }));
        }));
        const view = render(ChatFilesPage, { props: { sessionId: 'old-chat' } });
        await fireEvent.click(await screen.findByRole('button', { name: 'Delete old.pdf' }));
        await fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Delete file' }));
        await view.rerender({ sessionId: 'new-chat' });
        await screen.findByRole('button', { name: 'new.pdf' });
        finish(new Response(null, { status: 204 }));
        await new Promise(resolve => setTimeout(resolve, 0));
        expect(screen.getByRole('button', { name: 'new.pdf' })).toBeInTheDocument();
        expect(screen.getByLabelText('Total file size')).toHaveTextContent('Total: 1 KiB');
    });
    it('makes Files a selected peer tab with the exact viewed session', () => {
        render(ThreadSessionBar, { props: { threadId: 'research', sessionId: 'archived chat', activeTab: 'files' } });
        expect(screen.getByRole('link', { name: 'Files' })).toHaveAttribute('aria-current', 'page');
        expect(screen.getByRole('link', { name: 'Files' })).toHaveAttribute('href', '/t/research/files?session=archived%20chat');
        expect(screen.queryByRole('button', { name: 'Files' })).not.toBeInTheDocument();
    });
    it('has a useful empty state with a return link and no file popup', async () => {
        inventory(); render(ChatFilesPage, { props: { sessionId: 'chat-1', threadId: 'research' } });
        await screen.findByRole('heading', { name: 'No files in this chat yet' });
        expect(screen.getByText(/Files you upload and artifacts created/)).toBeInTheDocument();
        expect(screen.getByRole('link', { name: 'Back to chat' })).toHaveAttribute('href', '/t/research/chat?session=chat-1');
        expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    });
    it('shows uploaded files and provenance, and filters uploads separately', async () => {
        inventory([
            { id: 'upload', name: 'brief.pdf', path: 'brief.pdf', source: 'upload', session_id: 'chat-1', created_at: 1792720000000 },
            { id: 'artifact', name: 'Chapter-Map.md', path: 'Pages/Chapter-Map.md', source: 'task', task_id: 'task-1', execution_id: 'run-2', session_id: 'chat-1', created_at: '2026-10-23T02:00:00Z' }
        ]);
        render(ChatFilesPage, { props: { sessionId: 'chat-1' } });
        await screen.findByRole('button', { name: 'Chapter-Map.md' });
        expect(screen.getByText('Uploaded')).toBeInTheDocument();
        expect(screen.getByText('Execution: run-2')).toBeInTheDocument();
        expect(screen.getByRole('link', { name: 'Task: task-1' })).toHaveAttribute('href', '/work?selected=task-1');
        expect(screen.getAllByText('Chat: Research')).toHaveLength(2);
        expect(document.querySelectorAll('time')).toHaveLength(2);
        await fireEvent.click(screen.getByRole('button', { name: 'Uploads' }));
        expect(screen.getByRole('button', { name: 'brief.pdf' })).toBeInTheDocument();
        expect(screen.queryByRole('button', { name: 'Chapter-Map.md' })).not.toBeInTheDocument();
    });
    it('discards a late file inventory after switching conversations', async () => {
        let finishOld!: (value: Response) => void;
        vi.stubGlobal('fetch', vi.fn()
            .mockImplementationOnce(() => new Promise<Response>(resolve => { finishOld = resolve; }))
            .mockResolvedValue(new Response(JSON.stringify({ files: [{ id: 'new', name: 'new.md', path: 'new.md', source: 'chat' }] }))));
        const view = render(ChatFilesPage, { props: { sessionId: 'old-chat' } });
        await waitFor(() => expect(fetch).toHaveBeenCalledTimes(1));
        await view.rerender({ sessionId: 'new-chat' });
        await screen.findByRole('button', { name: 'new.md' });
        finishOld(new Response(JSON.stringify({ files: [{ id: 'old', name: 'private-old.md', path: 'old.md', source: 'chat' }] })));
        await new Promise(resolve => setTimeout(resolve, 0));
        expect(screen.queryByRole('button', { name: 'private-old.md' })).not.toBeInTheDocument();
        expect(screen.getByRole('button', { name: 'new.md' })).toBeInTheDocument();
    });
    it('does not query a session when none is selected', () => {
        inventory(); render(ChatFilesPage, { props: { sessionId: null } });
        expect(screen.getByRole('heading', { name: 'Choose a chat to see its files' })).toBeInTheDocument();
        expect(fetch).not.toHaveBeenCalled();
    });
    it('does not turn unavailable inventory into a successful empty state', async () => {
        vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('', { status: 503 })));
        render(ChatFilesPage, { props: { sessionId: 'chat-1' } });
        await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('Files could not be loaded'));
        expect(screen.queryByRole('heading', { name: 'No files in this chat yet' })).not.toBeInTheDocument();
        expect(screen.queryByLabelText('Total file size')).not.toBeInTheDocument();
    });
});
