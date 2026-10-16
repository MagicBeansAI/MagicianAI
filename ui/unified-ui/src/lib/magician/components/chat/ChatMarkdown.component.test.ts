import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, expect, it, vi } from 'vitest';
import ChatMarkdown from './ChatMarkdown.svelte';

const navigate = vi.hoisted(() => vi.fn());
vi.mock('$app/navigation', () => ({ goto: navigate }));
const openArtifact = vi.hoisted(() => vi.fn().mockResolvedValue(undefined));
vi.mock('$lib/magician/tasks/taskOutputs', () => ({ openAuthenticatedTaskOutput: openArtifact }));
const reportError = vi.hoisted(() => vi.fn());
vi.mock('$lib/shared/stores/notifications', () => ({ showError: reportError }));

afterEach(() => { cleanup(); navigate.mockClear(); openArtifact.mockClear(); reportError.mockClear(); });

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
