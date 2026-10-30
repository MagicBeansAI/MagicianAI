import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/svelte';
import UsageTotals from './UsageTotals.svelte';
import { readUsageModels, readUsageTotals } from '$lib/llm/usageTotals';
vi.mock('$lib/llm/usageTotals', () => ({ readUsageTotals: vi.fn(), readUsageModels: vi.fn() }));
beforeEach(() => {
    vi.mocked(readUsageTotals).mockResolvedValue({ calls: 7, input: 900, output: 100, cacheRead: 400, cacheWrite: 20, cost: 0.15, unpricedCalls: 0 });
    vi.mocked(readUsageModels).mockResolvedValue([
        { provider: 'decision:typesafe', model: 'jev-1.13', calls: 2, input: 100, output: 0, cacheRead: null, cacheWrite: null, cost: 0.001, unpricedCalls: 0 },
        { provider: 'openai', model: 'realtime', calls: 5, input: 800, output: 100, cacheRead: 400, cacheWrite: 20, cost: 0.149, unpricedCalls: 0 }
    ]);
});
afterEach(() => { cleanup(); vi.clearAllMocks(); vi.useRealTimers(); vi.restoreAllMocks(); });
it('recovers an idle header after a transient initial failure without reopening the chat', async () => {
    vi.useFakeTimers();
    vi.mocked(readUsageTotals).mockRejectedValueOnce(new Error('Usage unavailable (503)'));
    const { unmount } = render(UsageTotals, { props: { kind: 'chat', id: 'chat-1', live: false } });
    const summary = screen.getByLabelText('Total chat usage');
    await vi.waitFor(() => expect(summary).toHaveTextContent('Usage unavailable'));
    await vi.advanceTimersByTimeAsync(15000);
    await vi.waitFor(() => expect(summary).toHaveTextContent('1K tokens'));
    expect(readUsageTotals).toHaveBeenCalledTimes(2);
    unmount();
    await vi.advanceTimersByTimeAsync(60000);
    expect(readUsageTotals).toHaveBeenCalledTimes(2);
});
it('pauses idle polling while hidden and refreshes when the page becomes visible', async () => {
    vi.useFakeTimers();
    const hidden = vi.spyOn(document, 'hidden', 'get').mockReturnValue(false);
    render(UsageTotals, { props: { kind: 'chat', id: 'chat-1', live: false } });
    await vi.waitFor(() => expect(screen.getByLabelText('Total chat usage')).toHaveTextContent('1K tokens'));
    hidden.mockReturnValue(true);
    document.dispatchEvent(new Event('visibilitychange'));
    await vi.advanceTimersByTimeAsync(60000);
    expect(readUsageTotals).toHaveBeenCalledTimes(1);
    hidden.mockReturnValue(false);
    window.dispatchEvent(new Event('focus'));
    await vi.waitFor(() => expect(readUsageTotals).toHaveBeenCalledTimes(2));
});
it('keeps only tokens, cached tokens and cost in the chat header and loads models on demand', async () => {
    render(UsageTotals, { props: { kind: 'chat', id: 'chat-1' } });
    const summary = screen.getByLabelText('Total chat usage');
    await within(summary).findByText('1K tokens');
    expect(summary).toHaveTextContent('400 cached');
    expect(summary).toHaveTextContent('$0.15');
    expect(summary).not.toHaveTextContent('calls');
    expect(readUsageModels).not.toHaveBeenCalled();
    await fireEvent.click(summary);
    const details = await screen.findByRole('dialog', { name: 'Total chat usage details' });
    await within(details).findByText('jev-1.13');
    expect(details).toHaveTextContent('realtime');
    expect(details).toHaveTextContent('Cache unreported');
    expect(summary).not.toHaveTextContent('jev');
});
it('preserves unknown cache and cost explicitly in the header', async () => {
    vi.mocked(readUsageTotals).mockResolvedValue({ calls: 1, input: 100, output: 0, cacheRead: null, cacheWrite: null, cost: null, unpricedCalls: 1 });
    render(UsageTotals, { props: { kind: 'chat', id: 'chat-2' } });
    const summary = screen.getByLabelText('Total chat usage');
    await within(summary).findByText('100 tokens');
    expect(summary).toHaveTextContent('Cache unreported');
    expect(summary).toHaveTextContent('Cost unreported');
});
