import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, expect, it, vi } from 'vitest';
import ChatMarkdown from './ChatMarkdown.svelte';

const request = vi.hoisted(() => vi.fn().mockResolvedValue({ ok: true }));
vi.mock('$lib/shared/fetch', () => ({ timedFetch: request }));

const navigate = vi.hoisted(() => vi.fn());
vi.mock('$app/navigation', () => ({ goto: navigate }));
const openArtifact = vi.hoisted(() => vi.fn().mockResolvedValue(undefined));
vi.mock('$lib/magician/tasks/taskOutputs', () => ({ openAuthenticatedTaskOutput: openArtifact }));
const reportError = vi.hoisted(() => vi.fn());
vi.mock('$lib/shared/stores/notifications', () => ({ showError: reportError }));

afterEach(() => { cleanup(); navigate.mockClear(); openArtifact.mockClear(); reportError.mockClear(); request.mockClear(); });

it.each([
    ['/tasks/task-one', '/work?selected=task-one'],
    ['/tasks/task%2Fone?execution_id=run.two&plan=open#output', '/work?execution_id=run.two&plan=open&selected=task%2Fone#output'],
    ['/t/Launch/tasks/task-one?execution=run-one', '/work?execution=run-one&selected=task-one&thread=launch'],
])('opens a saved task link through Work: %s', async (source, destination) => {
    render(ChatMarkdown, { props: { content: `[Open tracked work](${source})`, sessionId: null } });
    const link = screen.getByRole('link', { name: 'Open tracked work' });
    await waitFor(() => expect(link).toHaveAttribute('href', destination));
    await fireEvent.click(link, { ctrlKey: true });
    expect(navigate).not.toHaveBeenCalled();
    await fireEvent.click(link);
    expect(navigate).toHaveBeenCalledExactlyOnceWith(destination);
});

it('renders a replayed quality report link with a Work destination for modified clicks', async () => {
    render(ChatMarkdown, { props: {
        content: '[Review artifact files](/relentless?run=run-one&campaign=campaign-one)',
        sessionId: null,
    } });
    const link = screen.getByRole('link', { name: 'Review artifact files' });
    await waitFor(() => expect(link).toHaveAttribute('href', '/work?run=run-one&campaign=campaign-one&view=quality'));
    await fireEvent.click(link, { ctrlKey: true });
    expect(navigate).not.toHaveBeenCalled();
});

it.each([{}, { ctrlKey: true }, { metaKey: true }, { shiftKey: true }])(
    'opens artifact links through the authenticated file path with modifiers %j', async (modifiers) => {
        const url = `/api/magician/v3/tasks/task_${'a'.repeat(32)}/outputs/report.png`;
        render(ChatMarkdown, { props: { content: `[Generated file](${url})`, sessionId: null } });
        const link = screen.getByRole('link', { name: 'Generated file' });
        await waitFor(() => expect(link).toHaveAttribute('data-ref-kind', 'taskOutput'));
        expect(await fireEvent.click(link, modifiers)).toBe(false);
        expect(openArtifact).toHaveBeenCalledExactlyOnceWith(url);
        expect(navigate).not.toHaveBeenCalled();
    }
);

it('authenticates middle-clicks and reports file-opening failures', async () => {
    const url = `/api/magician/v3/tasks/task_${'b'.repeat(32)}/outputs/report.pdf`;
    openArtifact.mockRejectedValueOnce(new Error('The file could not be read (HTTP 403)'));
    render(ChatMarkdown, { props: { content: `[Report](${url})`, sessionId: null } });
    const link = screen.getByRole('link', { name: 'Report' });
    await waitFor(() => expect(link).toHaveAttribute('data-ref-kind', 'taskOutput'));
    expect(await fireEvent(link, new MouseEvent('auxclick', { button: 1, bubbles: true, cancelable: true }))).toBe(false);
    expect(openArtifact).toHaveBeenCalledExactlyOnceWith(url);
    await waitFor(() => expect(reportError).toHaveBeenCalledWith('The file could not be read (HTTP 403)'));
});


it.each(['`Pages/Chapter-Map.md`', '[Chapter map](Pages/Chapter-Map.md)'])(
    'opens and reveals a workspace-relative file through its chat scope: %s', async (content) => {
        render(ChatMarkdown, { props: { content, sessionId: 'session-map' } });
        const open = await screen.findByRole('button', { name: 'Open Pages/Chapter-Map.md' });
        await fireEvent.click(open);
        const reveal = screen.getByRole('button', { name: 'Reveal folder containing Pages/Chapter-Map.md' });
        await fireEvent.click(reveal);
        expect(request.mock.calls.map(([url]) => url)).toEqual([
            '/api/magician/v2/chat/sessions/session-map/outputs/open-file',
            '/api/magician/v2/chat/sessions/session-map/outputs/open-folder',
        ]);
        for (const [, options] of request.mock.calls) {
            expect(JSON.parse(options.body)).toEqual({ absolute_path: 'Pages/Chapter-Map.md' });
        }
        expect(navigate).not.toHaveBeenCalled();
    }
);
