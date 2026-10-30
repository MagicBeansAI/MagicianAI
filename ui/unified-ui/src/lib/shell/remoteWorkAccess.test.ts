import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { WORK_SPECIALISTS } from '$lib/work/specialistRoutes';

const { hasBearer } = vi.hoisted(() => ({ hasBearer: vi.fn() }));
vi.mock('$app/environment', () => ({ browser: true }));
vi.mock('$lib/stores/scopeIdentityStore', () => ({ hasHydratedScopeBearer: hasBearer }));

beforeEach(() => {
    hasBearer.mockReset();
    const browserLocation = new URL('https://paired.example');
    vi.stubGlobal('window', { location: browserLocation });
    vi.stubGlobal('location', browserLocation);
});
afterEach(() => vi.unstubAllGlobals());

it.each([true, false])('canonicalizes retired browser entries before a 404 without bypassing auth (signed in: %s)', async (signedIn) => {
    const { load } = await import('../../routes/+layout');
    hasBearer.mockResolvedValue(signedIn);
    for (const [entered, destination] of [
        ['/relentless?run=sealed&campaign=local&tag=a&tag=b#artifact', '/work?run=sealed&campaign=local&tag=a&tag=b&view=quality#artifact'],
        ['/tasks?selected=task-one&execution_id=run-two', '/work?selected=task-one&execution_id=run-two'],
        ['/tasks/task-one?execution_id=run-two&plan=open#output', '/work?execution_id=run-two&plan=open&selected=task-one#output'],
        ['/t/Launch/tasks/task-one', '/work?selected=task-one&thread=launch'],
        ['/crew/forge/rules', '/work/crew/forge/rules'],
        ['/harness', '/work?view=operations']
    ]) {
        // SvelteKit load may not read url.hash; the browser location supplies
        // the actual fragment carried across a retired-route redirect.
        vi.stubGlobal('location', new URL(entered, 'https://paired.example'));
        await expect(load({ url: new URL(entered, 'https://paired.example') })).rejects.toMatchObject({ status: 307, location: destination });
        const routed = new URL(destination, 'https://paired.example');
        if (signedIn) await expect(load({ url: routed })).resolves.toBeUndefined();
        else await expect(load({ url: routed })).rejects.toMatchObject({ status: 307, location: `/login?redirectTo=${encodeURIComponent(routed.pathname + routed.search)}` });
    }
});

it('requires a hydrated browser session for every remote Work and specialist entry', async () => {
    const { load } = await import('../../routes/+layout');
    hasBearer.mockResolvedValue(false);
    for (const path of ['/work?selected=task%2Fone&execution_id=run%2Btwo', '/work/crew/forge/edit', ...WORK_SPECIALISTS.map(path => `${path}?from=%2Fwork%3Fview%3Dcrew%26agent%3Dforge`)]) {
        const url = new URL(path, 'https://paired.example');
        await expect(load({ url })).rejects.toMatchObject({ status: 307, location: `/login?redirectTo=${encodeURIComponent(url.pathname + url.search)}` });
    }
});

it('allows an authenticated remote Work return while preserving unrelated hostname and missing-runtime gates', async () => {
    const root = await import('../../routes/+layout');
    const app = await import('../../routes/(app)/+layout');
    hasBearer.mockResolvedValue(true);
    for (const path of ['/work', '/work/crew/forge/memory/shared', ...WORK_SPECIALISTS]) {
        const url = new URL(path, 'https://paired.example');
        await expect(root.load({ url })).resolves.toBeUndefined();
        await expect(app.load({ url })).resolves.toBeUndefined();
    }
    for (const path of ['/today', '/chat', '/work/unknown', '/crew/forge/delete', '/budget/unknown', '/tasks', '/crew', '/relentless', '/internal-tasks', '/debug/tasks', '/t/launch/tasks', '/harness']) {
        await expect(app.load({ url: new URL(path, 'https://paired.example') })).rejects.toMatchObject({ status: 302, location: '/' });
    }
    vi.stubGlobal('window', { location: { hostname: 'paired.example' }, __MAGICIAN_MISSING__: true });
    await expect(app.load({ url: new URL('https://paired.example/work') })).rejects.toMatchObject({ status: 302, location: '/' });
});
