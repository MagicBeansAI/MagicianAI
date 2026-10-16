import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';

describe('runtime Clarity opt-in', () => {
	let scripts: Record<string, unknown>[];
	let idle: () => void;
	let load: () => void;
	let target: Record<string, unknown>;
	let doc: Record<string, unknown>;
	beforeEach(() => {
		vi.resetModules();
		scripts = [];
		idle = () => {};
		load = () => {};
		target = {
			location: { hostname: 'app.example.com' },
			requestIdleCallback: vi.fn((callback: () => void) => { idle = callback; return 1; }),
			addEventListener: vi.fn((_event: string, callback: () => void) => { load = callback; })
		};
		doc = { readyState: 'complete', createElement: () => ({}), head: { appendChild: (script: Record<string, unknown>) => scripts.push(script) } };
		vi.stubGlobal('window', target);
		vi.stubGlobal('document', doc);
		vi.stubGlobal('fetch', vi.fn(async () => ({ ok: true, json: async () => ({ clarity_project_id: null }) })));
	});
	 afterEach(() => { vi.unstubAllGlobals(); });

	it('does not record an unconfigured runtime', async () => {
		const { startRuntimeClarity } = await import('./runtimeClarity');
		const done = startRuntimeClarity();
		expect(fetch).not.toHaveBeenCalled();
		idle(); await done;
		expect(scripts).toHaveLength(0);
		expect(target.clarity).toBeUndefined();
	});

	it('loads only the opted-in project, once, after load and idle', async () => {
		doc.readyState = 'loading';
		vi.mocked(fetch).mockResolvedValue({ ok: true, json: async () => ({ clarity_project_id: 'synthetic123' }) } as Response);
		const { startRuntimeClarity } = await import('./runtimeClarity');
		const done = startRuntimeClarity();
		expect(startRuntimeClarity()).toBe(done);
		expect(fetch).not.toHaveBeenCalled();
		load(); expect(fetch).not.toHaveBeenCalled();
		idle(); await done;
		expect(fetch).toHaveBeenCalledTimes(1);
		expect(scripts).toEqual([{ async: true, src: 'https://www.clarity.ms/tag/synthetic123' }]);
	});

	it.each(['localhost', 'tauri.localhost', '127.0.0.1', '[::1]'])('never records loopback %s', async (hostname) => {
		target.location = { hostname };
		const { startRuntimeClarity } = await import('./runtimeClarity');
		await startRuntimeClarity();
		expect(fetch).not.toHaveBeenCalled();
		expect(scripts).toHaveLength(0);
	});

	it.each(['', '../other', 'export.jwt.token', null])('ignores invalid project configuration %s', async (project) => {
		vi.mocked(fetch).mockResolvedValue({ ok: true, json: async () => ({ clarity_project_id: project }) } as Response);
		const { startRuntimeClarity } = await import('./runtimeClarity');
		const done = startRuntimeClarity(); idle(); await done;
		expect(scripts).toHaveLength(0);
	});

	it('does not turn authentication or network errors into startup failures', async () => {
		vi.mocked(fetch).mockRejectedValue(new Error('offline'));
		const { startRuntimeClarity } = await import('./runtimeClarity');
		const done = startRuntimeClarity(); idle(); await expect(done).resolves.toBeUndefined();
		expect(scripts).toHaveLength(0);
	});

	it('reloads consent boundaries for active or pending recording, preserving ordinary app navigation', async () => {
		const { requiresAnalyticsNavigationReload: reload } = await import('./runtimeClarity');
		expect(reload('/', '/work', true, false)).toBe(true);
		expect(reload('/work', '/login', true, false)).toBe(true);
		expect(reload('/work', '/', false, true)).toBe(true);
		expect(reload('/', '/login', true, false)).toBe(true);
		expect(reload('/work', '/settings', true, false)).toBe(false);
		expect(reload('/privacy/', '/terms', true, false)).toBe(false);
		expect(reload('/', '/work', false, false)).toBe(false);
	});
});
