import { render, screen, fireEvent, waitFor } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';

const api = vi.hoisted(() => ({ followUps: vi.fn(), worth: vi.fn(), dismiss: vi.fn() }));
vi.mock('$app/environment', () => ({ browser: true, dev: false }));
vi.mock('$app/stores', async () => {
    const { readable } = await import('svelte/store');
    return { page: readable({ url: new URL('http://localhost/today') }) };
});
vi.mock('$lib/stores/todayStore', async () => {
    const { readable } = await import('svelte/store');
    return { todayStore: { ...readable({ lastLoadedAt: 1, error: null, sections: { needs_you: [], delivered: [] }, digest: { total: 0, bullets: [] }, hiddenItems: [] }),
        start: vi.fn(), stop: vi.fn(), setQuery: vi.fn() } };
});
vi.mock('$lib/stores/canonicalAttentionProjectionStore', async () => {
    const { readable } = await import('svelte/store');
    return { canonicalAttentionProjectionStore: { ...readable({ projection: null }), refresh: vi.fn().mockResolvedValue(undefined) } };
});
vi.mock('$lib/stores/threadStore', async () => {
    const { readable } = await import('svelte/store');
    return { threadStore: { ...readable({ threads: [] }), start: vi.fn(), stop: vi.fn() } };
});
// Partial: the deck's Act menu and detail view use the pure helpers.
vi.mock('$lib/stores/channelNeedsYouStore', async (importOriginal) => ({
    ...(await importOriginal<Record<string, unknown>>()),
    fetchChannelFollowUpsPage: api.followUps, dismissChannelFollowUp: api.dismiss,
    usefulChannelFollowUp: vi.fn(), acknowledgeChannelFollowUp: vi.fn(), approveChannelFollowUp: vi.fn(),
    snoozeChannelFollowUp: vi.fn(), dismissChannelFollowUpWithReason: vi.fn()
}));
vi.mock('./resurfacingQueries', async (importOriginal) => ({
    ...(await importOriginal<Record<string, unknown>>()),
    fetchResurfacingTodayPage: api.worth, postResurfacingAction: vi.fn()
}));
vi.mock('$lib/magician/presto/surfaces/publishedSurfaces', () => ({ loadPublishedSurfacePage: vi.fn().mockResolvedValue({ records: [] }) }));
vi.mock('./TodayNewspaperLedger.svelte', async () => ({ default: (await import('../../test/fixtures/EmptyComponent.svelte')).default }));
vi.mock('./TodayRealtimeWire.svelte', async () => ({ default: (await import('../../test/fixtures/EmptyComponent.svelte')).default }));
vi.mock('$lib/apps/AppSlotRegion.svelte', async () => ({ default: (await import('../../test/fixtures/EmptyComponent.svelte')).default }));
import MorningEdition from './MorningEdition.svelte';

const message = (id: number) => ({ annotation_id: `f${id}`, provider: 'gmail', subject: `Message ${id}`, summary: 'Review this message', created_at: 1700000000000 });
describe('Morning Edition deck paging', () => {
    beforeEach(() => { api.followUps.mockReset(); api.worth.mockReset(); api.dismiss.mockReset(); });
    it('fetches beyond the first broadsheet page and keeps the advanced cursor on refresh', async () => {
        api.followUps.mockImplementation(async (_limit: number, cursor: string | null) => cursor === 'page-3'
            ? { ok: true, items: [11,12,13,14,15].map(message), total: 15 - api.dismiss.mock.calls.length, has_more: false, next_cursor: null }
            : cursor === 'page-2'
            ? { ok: true, items: [6,7,8,9,10].map(message), total: 15 - api.dismiss.mock.calls.length, has_more: true, next_cursor: 'page-3' }
            : { ok: true, items: [1,2,3,4,5].map(message), total: 15 - api.dismiss.mock.calls.length, has_more: true, next_cursor: 'page-2' });
        api.worth.mockResolvedValue({ cards: [], total: 0, has_more: false, next_cursor: null });
        api.dismiss.mockResolvedValue({ ok: true });
        render(MorningEdition);
        await screen.findByText('5 loaded · 15 total');
        await fireEvent.click(screen.getByRole('button', { name: 'Dismiss' }));
        await screen.findByText('9 loaded · 14 total');
        expect(api.followUps).toHaveBeenCalledWith(5, 'page-2', { includeCanonicalProjection: false });
        await fireEvent.click(screen.getByRole('button', { name: 'Refresh morning edition' }));
        await waitFor(() => expect(api.followUps).toHaveBeenCalledTimes(3));
        await waitFor(() => expect(screen.getByRole('button', { name: 'Refresh morning edition' })).toBeEnabled());
        expect(screen.getByText(/9 loaded/)).toBeInTheDocument();
        // The head refresh must not bring a successfully handled card back.
        expect(screen.queryByRole('heading', { name: 'Message 1' })).not.toBeInTheDocument();
        for (let i = 0; i < 5; i++) {
            await fireEvent.click(screen.getByRole('button', { name: 'Dismiss' }));
            await new Promise(resolve => setTimeout(resolve, 280));
        }
        await waitFor(() => expect(api.followUps).toHaveBeenCalledWith(5, 'page-3', { includeCanonicalProjection: false }));
    });
    it('recovers paging when an initial lane failure is followed by a successful refresh', async () => {
        const errorLog = vi.spyOn(console, 'error').mockImplementation(() => {});
        api.followUps.mockResolvedValueOnce({ ok: false, error: 'offline' }).mockResolvedValue({
            ok: true, items: [1,2,3,4,5].map(message), total: 15, has_more: true, next_cursor: 'page-2'
        });
        api.worth.mockResolvedValue({ cards: [], total: 0, has_more: false, next_cursor: null });
        api.dismiss.mockResolvedValue({ ok: true });
        render(MorningEdition);
        await screen.findByText('Could not load For You. Try again.');
        await waitFor(() => expect(screen.getByRole('button', { name: 'Refresh morning edition' })).toBeEnabled());
        await fireEvent.click(screen.getByRole('button', { name: 'Refresh morning edition' }));
        await screen.findByText('5 loaded · 15 total');
        await fireEvent.click(screen.getByRole('button', { name: 'Dismiss' }));
        await waitFor(() => expect(api.followUps).toHaveBeenCalledWith(5, 'page-2', { includeCanonicalProjection: false }));
        errorLog.mockRestore();
    });

});
