import { afterEach, expect, it, vi } from 'vitest';
import { installFetchMock } from '../../../test/browser';
import { openAuthenticatedTaskOutput } from './taskOutputs';

vi.mock('$lib/stores/scopeIdentityStore', () => ({
	scopedRequestHeaders: () => new Headers({ Authorization: 'Bearer synthetic-output-session' })
}));

afterEach(() => { vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

function viewer() {
	vi.useFakeTimers();
	const blobs = new Map<string, Blob>();
	const create = vi.fn((blob: Blob) => {
		const url = `blob:${window.location.origin}/output-${blobs.size}`;
		blobs.set(url, blob);
		return url;
	});
	const revoke = vi.fn();
	vi.stubGlobal('URL', class extends URL {
		static createObjectURL = create;
		static revokeObjectURL = revoke;
	});
	const tab = { opener: {}, location: { href: '' }, closed: false, close: vi.fn() };
	vi.spyOn(window, 'open').mockReturnValue(tab as unknown as Window);
	return { blobs, revoke, tab };
}

it('embeds protected attachments once without authority and retains an independent static preview until close', async () => {
	const { blobs, revoke, tab } = viewer();
	const base = '/api/magician/v3/tasks/task_owner/outputs/report.html';
	const media = '/api/magician/v3/tasks/task_owner/outputs/scene.png';
	const execution = '/api/magician/v3/executions/exec_owner/artifacts/outputs/clip.mp4';
	const external = 'https://example.com/api/magician/v3/tasks/foreign/outputs/photo.png';
	const { calls } = installFetchMock([{ match: () => true, handle: ({ url }) => new Response(
		url === base ? `<h1>Report</h1><img src="scene.png"><a href="${media}">Image</a><video src="${execution}"></video><img src="${external}"><script>window.localStorage.getItem('session')</script>` : 'media bytes',
		{ headers: { 'Content-Type': url === base ? 'text/html' : 'image/png' } }
	) }]);
	await openAuthenticatedTaskOutput(base);
	expect(calls.map(call => call.url)).toEqual([base, window.location.origin + media, window.location.origin + execution]);
	expect(calls.every(call => new Headers(call.init?.headers).get('Authorization') === 'Bearer synthetic-output-session')).toBe(true);
	const wrapper = new DOMParser().parseFromString(await blobs.get(tab.location.href)!.text(), 'text/html');
	const frame = wrapper.querySelector('iframe')!;
	expect(frame.getAttribute('sandbox')).toBe('allow-same-origin allow-popups allow-downloads');
	expect(frame.getAttribute('sandbox')).not.toContain('allow-scripts');
	const html = new DOMParser().parseFromString(frame.getAttribute('srcdoc')!, 'text/html');
	expect(html.querySelector('img')!.getAttribute('src')).toBe(html.querySelector('a')!.getAttribute('href'));
	expect(html.querySelector('video')!.getAttribute('src')).toMatch(/^data:image\/png;base64,/);
	expect(html.querySelector('a')!.getAttribute('download')).toBe('scene.png');
	expect(atob(html.querySelector('img')!.getAttribute('src')!.split(',')[1])).toBe('media bytes');
	// Only the viewer itself depends on an object URL. Attachments remain in
	// its loaded document if the originating Work tab closes or reloads.
	expect(blobs.size).toBe(1);
	expect(html.querySelectorAll('img')[1].getAttribute('src')).toBe(external);
	expect(frame.getAttribute('srcdoc')).not.toContain('synthetic-output-session');
	vi.advanceTimersByTime(120_000);
	expect(revoke).not.toHaveBeenCalled();
	tab.closed = true;
	vi.advanceTimersByTime(60_000);
	expect(revoke.mock.calls.map(([url]) => url).sort()).toEqual([...blobs.keys()].sort());
});

it('closes a failed HTML viewer without leaving attachment object URLs', async () => {
	const { blobs, revoke, tab } = viewer();
	const base = '/api/magician/v3/tasks/task_owner/outputs/report.html';
	installFetchMock([{ match: () => true, handle: async ({ url }) => {
		if (url === base) return new Response('<img src="missing.png"><img src="valid.png">', { headers: { 'Content-Type': 'text/html' } });
		if (url.endsWith('missing.png')) return new Response('missing', { status: 404 });
		await Promise.resolve();
		return new Response('image bytes', { headers: { 'Content-Type': 'image/png' } });
	} }]);
	await expect(openAuthenticatedTaskOutput(base)).rejects.toThrow('HTTP 404');
	expect(tab.close).toHaveBeenCalledOnce();
	expect(tab.location.href).toBe('');
	expect(blobs.size).toBe(0);
	expect(revoke).not.toHaveBeenCalled();
});
