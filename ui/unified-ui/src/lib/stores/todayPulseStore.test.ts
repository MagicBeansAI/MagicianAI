// @vitest-environment jsdom
import { get, readable } from 'svelte/store';
import { afterEach, describe, expect, it, vi } from 'vitest';

vi.mock('$app/environment', () => ({ browser: true }));
vi.mock('$lib/stores/taskStore', () => ({ taskStore: readable({ tasks: [] }) }));
vi.mock('$lib/stores/scopeIdentityStore', () => ({ scopeIdentityStore: readable({ principal: 'owner', workspace: 'home' }) }));

describe('Pulse spend availability', () => {
    let stop: (() => void) | undefined;
    afterEach(() => { stop?.(); vi.unstubAllGlobals(); vi.restoreAllMocks(); vi.resetModules(); });

    it('does not present failed first-load spend as a valid zero', async () => {
        vi.spyOn(console, 'warn').mockImplementation(() => {});
        vi.stubGlobal('fetch', vi.fn(async (url: string) => {
            if (url.includes('/llm_calls/')) throw new Error('unavailable');
            return new Response(JSON.stringify({ columns: [], rows: [] }));
        }));
        const { todayPulseStore } = await import('./todayPulseStore');
        stop = () => todayPulseStore.stop(); todayPulseStore.start();
        await vi.waitFor(() => expect(get(todayPulseStore).loading).toBe(false));
        expect(get(todayPulseStore).snapshot).not.toBeNull();
        expect(get(todayPulseStore).readySlices).not.toContain('llm');
        expect(get(todayPulseStore).staleSlices).toContain('llm');
    });

    it('treats a successfully returned empty spend window as real zero spend', async () => {
        vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify({ columns: [], rows: [] }))));
        const { todayPulseStore } = await import('./todayPulseStore');
        stop = () => todayPulseStore.stop(); todayPulseStore.start();
        await vi.waitFor(() => expect(get(todayPulseStore).loading).toBe(false));
        expect(get(todayPulseStore).readySlices).toContain('llm');
        expect(get(todayPulseStore).snapshot?.llm.spendToday).toBe(0);
    });
    it('publishes coding and model slices while memory is still pending', async () => {
        let finishMemory!: () => void;
        vi.stubGlobal('fetch', vi.fn(async (url: string) => {
            if (url.includes('/memory_events/')) return new Promise<Response>(resolve => {
                finishMemory = () => resolve(new Response(JSON.stringify({ columns: [], rows: [] })));
            });
            return new Response(JSON.stringify(url.includes('/llm_calls/') ? { columns: [], rows: [] }
                : { columns: ['section', 'n'], rows: [['coding_runs_today', 3]] }));
        }));
        const { todayPulseStore } = await import('./todayPulseStore');
        stop = () => todayPulseStore.stop(); todayPulseStore.start();
        try {
            await vi.waitFor(() => expect(get(todayPulseStore).readySlices).toContain('coding'));
            expect(get(todayPulseStore).snapshot?.codingRunsToday).toBe(3);
            expect(get(todayPulseStore).readySlices).toContain('llm');
            expect(get(todayPulseStore).readySlices).not.toContain('memory');
        } finally { finishMemory?.(); }
    });

});
