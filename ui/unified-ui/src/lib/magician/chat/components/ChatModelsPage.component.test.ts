import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import ChatModelsPage from './ChatModelsPage.svelte';
import ThreadSessionBar from '$lib/threads/ThreadSessionBar.svelte';
import { readUsageModels, readUsageTotals, type UsageModelTotals } from '$lib/llm/usageTotals';

vi.mock('$lib/llm/usageTotals', () => ({ readUsageModels: vi.fn(), readUsageTotals: vi.fn() }));
const rows: UsageModelTotals[] = [
    { provider: 'decision:typesafe', model: 'jev-1.13', calls: 2, input: 1000, output: 50, cacheRead: null, cacheWrite: null, cost: 0.001, unpricedCalls: 0 },
    { provider: 'openai', model: 'realtime', calls: 3, input: 400, output: 100, cacheRead: 200, cacheWrite: 0, cost: 0.02, unpricedCalls: 1 }
];
beforeEach(() => {
    vi.mocked(readUsageModels).mockResolvedValue(rows);
    vi.mocked(readUsageTotals).mockResolvedValue(rows[0]);
});
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it('places Models directly after Files and preserves the viewed chat in its URL', () => {
    render(ThreadSessionBar, { props: { threadId: 'research', sessionId: 'archived chat', activeTab: 'models' } });
    const link = screen.getByRole('link', { name: 'Models' });
    expect(link).toHaveAttribute('aria-current', 'page');
    expect(link).toHaveAttribute('href', '/t/research/models?session=archived%20chat');
    expect(screen.getByRole('link', { name: 'Files' }).nextElementSibling).toBe(link);
});
it('shows each model, calls, token buckets, cache and partial cost without double-counting cache', async () => {
    render(ChatModelsPage, { props: { sessionId: 'chat-1' } });
    const table = await screen.findByRole('table');
    const decision = within(table).getByRole('row', { name: /jev-1.13/ });
    expect(decision).toHaveTextContent('1,050');
    expect(within(decision).getAllByText('Not reported')).toHaveLength(2);
    const voice = within(table).getByRole('row', { name: /realtime/ });
    expect(within(voice).getByText('500')).toBeInTheDocument();
    expect(voice).toHaveTextContent('≥ $0.0200');
    expect(voice).toHaveTextContent('1 unpriced');
    await fireEvent.input(screen.getByRole('searchbox'), { target: { value: 'typesafe' } });
    expect(screen.queryByRole('row', { name: /realtime/ })).not.toBeInTheDocument();
    expect(screen.getByRole('row', { name: /jev-1.13/ })).toBeInTheDocument();
});
it('does not invent token counts or cost for a model with unreported usage', async () => {
    vi.mocked(readUsageModels).mockResolvedValue([{ ...rows[0], input: null, output: null, cost: null }]);
    render(ChatModelsPage, { props: { sessionId: 'chat-1' } });
    const row = await screen.findByRole('row', { name: /jev-1.13/ });
    expect(within(row).getAllByText('Not reported')).toHaveLength(6);
});
it('ignores a late model breakdown from the previously viewed chat', async () => {
    let finish!: (rows: UsageModelTotals[]) => void;
    vi.mocked(readUsageModels).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }))
        .mockResolvedValueOnce([{ ...rows[1], model: 'new-chat-model' }]);
    const view = render(ChatModelsPage, { props: { sessionId: 'old-chat' } });
    await waitFor(() => expect(readUsageModels).toHaveBeenCalledWith('chat', 'old-chat'));
    await view.rerender({ sessionId: 'new-chat' });
    await screen.findByRole('row', { name: /new-chat-model/ });
    finish(rows);
    await new Promise(resolve => setTimeout(resolve, 0));
    expect(screen.queryByRole('row', { name: /jev-1.13/ })).not.toBeInTheDocument();
});
it('distinguishes no chat, empty usage and unavailable usage', async () => {
    const view = render(ChatModelsPage, { props: { sessionId: null } });
    expect(screen.getByRole('heading', { name: 'Choose a chat to see model usage' })).toBeInTheDocument();
    expect(readUsageModels).not.toHaveBeenCalled();
    vi.mocked(readUsageModels).mockResolvedValueOnce([]);
    await view.rerender({ sessionId: 'empty-chat' });
    await screen.findByRole('heading', { name: 'No model usage recorded yet' });
    vi.mocked(readUsageModels).mockRejectedValueOnce(new Error('Usage unavailable (503)'));
    await view.rerender({ sessionId: 'unavailable-chat' });
    await screen.findByRole('alert');
    expect(screen.queryByRole('heading', { name: 'No model usage recorded yet' })).not.toBeInTheDocument();
});
it('refreshes every minute by default, shows the countdown and keeps the chosen interval', async () => {
    localStorage.removeItem('magician.chat.models.refreshIntervalMs');
    render(ChatModelsPage, { props: { sessionId: 'chat-1' } });
    await screen.findByRole('table');
    const interval = screen.getByRole('combobox', { name: 'Refresh interval' }) as HTMLSelectElement;
    expect(interval.value).toBe('60000');
    expect(await screen.findByRole('timer')).toHaveTextContent(/Next refresh in (1:00|\d+ s)/);
    await fireEvent.change(interval, { target: { value: '300000' } });
    expect(localStorage.getItem('magician.chat.models.refreshIntervalMs')).toBe('300000');
});
