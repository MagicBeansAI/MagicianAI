import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	bridgeEventSourceIsAdmitted,
	frameBridgeRequestFromEvent,
	fetchScriptedSurfaceHost,
	parseScriptedSurfaceHostPlan,
	postScriptedSurfaceReloadNote,
	scriptedSurfaceReloadNoteRoute,
	SCRIPTED_SURFACE_FAILED_NOTICE,
	SCRIPTED_SURFACE_MAX_RELOADS,
	SCRIPTED_SURFACE_SANDBOX,
	SCRIPTED_SURFACE_UNSUPPORTED_NOTICE,
	ScriptedSurfaceBridgeSubmissionFifo,
	ScriptedSurfaceReloadBudget,
	surfaceFrameSourceIsAdmitted
} from './appScriptedSurface';
import { AppSurfaceClientError } from './appSurfaceRuntime';

describe('scripted host error transport', () => {
	afterEach(() => vi.unstubAllGlobals());

	it.each([
		{ error: 'app_custom_surface_unavailable', message: 'Capability disabled' },
		{ code: 'app_custom_surface_unavailable', message: 'Capability disabled' },
		{ error: { code: 'app_custom_surface_unavailable', message: 'Capability disabled' } }
	])('preserves the unavailable code from %j', async (body) => {
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(body), { status: 404 })));
		await expect(fetchScriptedSurfaceHost('install_1', { route: '/canvas' })).rejects.toMatchObject({
			status: 404, code: 'app_custom_surface_unavailable', message: 'Capability disabled'
		});
	});
});

function plan(overrides: Record<string, unknown> = {}) {
	return {
		sandbox: 'allow-scripts',
		csp: "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'none'; frame-src 'none'; frame-ancestors https://home.example; form-action 'none'; base-uri 'none'",
		session_ref: 'bridge-scripted:install_1:1',
		nonce: 'nonce:1',
		installation_id: 'install_1',
		package_revision_ref: 'package-revision:reading-list',
		surface_revision: 1,
		grant_revision: 1,
		entry_route: '/canvas',
		entry_document: 'surfaces/canvas.html',
		entry_document_digest: 'blake3:abc',
		entry_url:
			'/api/magician/v2/apps/installations/install_1/custom-surface-v1/assets/bridge-scripted:install_1:1/blake3:abc/surfaces/canvas.html',
		methods: [
			'query_data',
			'mutate_data',
			'launch_action',
			'get_action_run',
			'compose_action_run',
			'cancel_action_run',
			'read_entity_changes',
			'contract_capabilities'
		],
		...overrides
	};
}

describe('parseScriptedSurfaceHostPlan', () => {
	it('accepts a kernel-constant plan for the requested installation', () => {
		const parsed = parseScriptedSurfaceHostPlan(plan(), 'install_1');
		expect(parsed.sandbox).toBe('allow-scripts');
		expect(parsed.entry_route).toBe('/canvas');
		expect(parsed.methods).toHaveLength(8);
	});

	it('refuses every sandbox widening and non-kernel CSP', () => {
		expect(() =>
			parseScriptedSurfaceHostPlan(plan({ sandbox: 'allow-scripts allow-same-origin' }), 'install_1')
		).toThrow();
		expect(() => parseScriptedSurfaceHostPlan(plan({ sandbox: '' }), 'install_1')).toThrow();
		expect(() =>
			parseScriptedSurfaceHostPlan(
				plan({ csp: "default-src 'none'; connect-src https://evil.example" }),
				'install_1'
			)
		).toThrow();
	});

	it('refuses cross-installation substitution and non-digest entry URLs', () => {
		expect(() => parseScriptedSurfaceHostPlan(plan(), 'install_2')).toThrow();
		expect(() =>
			parseScriptedSurfaceHostPlan(
				plan({
					entry_url: '/api/magician/v2/apps/installations/install_1/custom-surface/assets/x'
				}),
				'install_1'
			)
		).toThrow();
		expect(() =>
			parseScriptedSurfaceHostPlan(plan({ methods: ['query_data'] }), 'install_1')
		).toThrow();
	});

	it('requires the entry URL to carry exactly the minted session path segment', () => {
		// The frame's opaque origin can present no headers: the `session`
		// path segment embedded in the entry URL is the asset route's credential,
		// and the plan parse pins it to the plan's own session binding.
		const parsed = parseScriptedSurfaceHostPlan(plan(), 'install_1');
		expect(parsed.entry_url).toBe(
			'/api/magician/v2/apps/installations/install_1/custom-surface-v1/assets/bridge-scripted:install_1:1/blake3:abc/surfaces/canvas.html'
		);
		// A digest-only entry URL (the pre-fix mint) could never load.
		expect(() =>
			parseScriptedSurfaceHostPlan(
				plan({
					entry_url:
						'/api/magician/v2/apps/installations/install_1/custom-surface-v1/assets/blake3:abc/surfaces/canvas.html'
				}),
				'install_1'
			)
		).toThrow();
		// Any other session than the plan binding is a substitution.
		expect(() =>
			parseScriptedSurfaceHostPlan(
				plan({
					entry_url:
						'/api/magician/v2/apps/installations/install_1/custom-surface-v1/assets/bridge-scripted:install_1:other/blake3:abc/surfaces/canvas.html'
				}),
				'install_1'
			)
		).toThrow();
		// A query is never an alternate credential channel.
		expect(() =>
			parseScriptedSurfaceHostPlan(
				plan({
					entry_url:
						'/api/magician/v2/apps/installations/install_1/custom-surface-v1/assets/bridge-scripted:install_1:1/blake3:abc/surfaces/canvas.html?x=1'
				}),
				'install_1'
			)
		).toThrow();
	});
});

describe('bridgeEventSourceIsAdmitted (per-frame keying)', () => {
	it('admits only the exact frame the host created at the opaque origin', () => {
		const frame = { contentWindow: { postMessage: () => {} } } as unknown as HTMLIFrameElement;
		const other = { contentWindow: { postMessage: () => {} } } as unknown as HTMLIFrameElement;
		const parsed = parseScriptedSurfaceHostPlan(plan(), 'install_1');
		const event = {
			source: frame.contentWindow,
			origin: 'null',
			data: { channel: 'magician-surface-bridge' }
		} as unknown as MessageEvent;
		expect(bridgeEventSourceIsAdmitted(event, frame, parsed)).toBe(true);
		// A different sandboxed frame shares origin "null" on web; only the
		// exact frame object discriminates (design T4).
		expect(bridgeEventSourceIsAdmitted(event, other, parsed)).toBe(false);
		expect(
			bridgeEventSourceIsAdmitted(
				{ ...event, origin: 'https://evil.example' } as unknown as MessageEvent,
				frame,
				parsed
			)
		).toBe(false);
		expect(bridgeEventSourceIsAdmitted(event, null, parsed)).toBe(false);
	});
});

describe('frameBridgeRequestFromEvent', () => {
	it('binds every authority field to the minted plan', () => {
		const parsed = parseScriptedSurfaceHostPlan(plan(), 'install_1');
		const event = {
			data: {
				channel: 'magician-surface-bridge',
				request: {
					request_id: 'req:1',
					sequence: 1,
					method: 'query_data',
					payload: { select: ['title'] }
				}
			}
		} as unknown as MessageEvent;
		const request = frameBridgeRequestFromEvent(event, parsed, 0);
		expect(request).not.toBeNull();
		expect(request?.session_ref).toBe(parsed.session_ref);
		expect(request?.installation_id).toBe('install_1');
		expect(request?.origin).toBe('null');
		// Sequence gaps, replays, and methods outside the closed set are
		// dropped.
		expect(frameBridgeRequestFromEvent(event, parsed, 1)).toBeNull();
		const hostile = {
			data: {
				channel: 'magician-surface-bridge',
				request: {
					request_id: 'req:2',
					sequence: 2,
					method: 'subscribe',
					payload: {}
				}
			}
		} as unknown as MessageEvent;
		expect(frameBridgeRequestFromEvent(hostile, parsed, 1)).toBeNull();
	});
});

describe('ScriptedSurfaceBridgeSubmissionFifo', () => {
	it('starts concurrent frame submissions in FIFO order and survives one failure', async () => {
		const fifo = new ScriptedSurfaceBridgeSubmissionFifo();
		const started: number[] = [];
		const release: Array<() => void> = [];
		const submit = (sequence: number, fail = false) =>
			fifo.enqueue(
				() =>
					new Promise<number>((resolve, reject) => {
						started.push(sequence);
						release.push(() => (fail ? reject(new Error(`failed:${sequence}`)) : resolve(sequence)));
					})
			);

		const first = submit(1);
		const second = submit(2, true);
		const third = submit(3);
		await Promise.resolve();
		expect(started).toEqual([1]);
		release.shift()?.();
		await expect(first).resolves.toBe(1);
		await Promise.resolve();
		expect(started).toEqual([1, 2]);
		release.shift()?.();
		await expect(second).rejects.toThrow('failed:2');
		await Promise.resolve();
		expect(started).toEqual([1, 2, 3]);
		release.shift()?.();
		await expect(third).resolves.toBe(3);
	});
});

describe('ScriptedSurfaceReloadBudget', () => {
	it('tears the surface down on the reload after the budget', () => {
		const budget = new ScriptedSurfaceReloadBudget();
		for (let index = 0; index < SCRIPTED_SURFACE_MAX_RELOADS; index += 1) {
			expect(budget.noteReload().exceeded).toBe(false);
		}
		const verdict = budget.noteReload();
		expect(verdict.exceeded).toBe(true);
		expect(verdict.notice).toBe(SCRIPTED_SURFACE_FAILED_NOTICE);
	});
});

describe('postScriptedSurfaceReloadNote', () => {
	afterEach(() => vi.unstubAllGlobals());

	it('builds the exact reload-note route for the minted session', () => {
		expect(scriptedSurfaceReloadNoteRoute('install_1', 'bridge-scripted:install_1:1')).toBe(
			'/api/magician/v2/apps/installations/install_1/custom-surface-v1/sessions/bridge-scripted:install_1:1/reload-note'
		);
	});

	it('keeps non-canonical path separators encoded for fail-closed refusal', () => {
		expect(scriptedSurfaceReloadNoteRoute('install_1', 'bridge-scripted:install_1:../other')).toBe(
			'/api/magician/v2/apps/installations/install_1/custom-surface-v1/sessions/bridge-scripted:install_1:..%2Fother/reload-note'
		);
	});

	it('records a reload under the host page credentials, never the frame', async () => {
		const fetchMock = vi
			.fn()
			.mockResolvedValue(new Response(JSON.stringify({ reload_note: 'recorded' }), { status: 200 }));
		vi.stubGlobal('fetch', fetchMock);
		await expect(
			postScriptedSurfaceReloadNote('install_1', 'bridge-scripted:install_1:1')
		).resolves.toBeUndefined();
		expect(fetchMock).toHaveBeenCalledTimes(1);
		const [url, init] = fetchMock.mock.calls[0];
		expect(url).toBe(
			'/api/magician/v2/apps/installations/install_1/custom-surface-v1/sessions/bridge-scripted:install_1:1/reload-note'
		);
		expect(init.method).toBe('POST');
		expect(init.headers).toBeDefined();
	});

	it('surfaces a budget quarantine as the same 409 error shape a bridge refusal throws', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue(
				new Response(JSON.stringify({ error: { code: 'app_custom_surface_denied' } }), {
					status: 409
				})
			)
		);
		await expect(
			postScriptedSurfaceReloadNote('install_1', 'bridge-scripted:install_1:1')
		).rejects.toMatchObject({ status: 409, code: 'app_custom_surface_denied' });
	});
});

describe('surfaceFrameSourceIsAdmitted (desktop constraint)', () => {
	it('admits only kernel-issued relative digest paths', () => {
		expect(
			surfaceFrameSourceIsAdmitted(
				'/api/magician/v2/apps/installations/install_1/custom-surface-v1/assets/blake3:abc/surfaces/canvas.html'
			)
		).toBe(true);
		// The minted entry address carries its session path segment; the
		// admission keeps admitting it (no encoding, escapes, or origin).
		expect(
			surfaceFrameSourceIsAdmitted(
				'/api/magician/v2/apps/installations/install_1/custom-surface-v1/assets/bridge-scripted:install_1:1/blake3:abc/surfaces/canvas.html'
			)
		).toBe(true);
	});

	it('refuses the Tauri host origin and dev-server origins', () => {
		for (const url of [
			'tauri://localhost/index.html',
			'https://tauri.localhost/index.html',
			'http://tauri.localhost/index.html',
			'http://localhost:5173/surfaces/canvas.html',
			'http://localhost:3002/surfaces/canvas.html',
			'https://evil.example/canvas.html',
			'/api/../../etc/passwd',
			'/api/%2e%2e/secret'
		]) {
			expect(surfaceFrameSourceIsAdmitted(url), url).toBe(false);
		}
		// The forbidden origins are matched before the relative-path
		// requirement, so an absolute forbidden origin is refused by name —
		// in any letter case.
		for (const url of ['TAURI://localhost/index.html', 'https://TAURI.LOCALHOST/index.html']) {
			expect(surfaceFrameSourceIsAdmitted(url), url).toBe(false);
		}
	});

	it('refuses protocol-relative sources outright', () => {
		// A `//host/path` source resolves against the host page's own
		// scheme to a cross-origin absolute URL: it must never pass the
		// relative-path requirement a bare `/` prefix alone satisfies
		// (same invariant the Rust desktop policy refuses — keep the two
		// sets in step).
		for (const url of [
			'//evil.example/canvas.html',
			'//evil.example/custom-surface-v1/assets/blake3:abc/surfaces/canvas.html'
		]) {
			expect(surfaceFrameSourceIsAdmitted(url), url).toBe(false);
		}
	});

	it('keeps admitting the kernel-issued relative API paths the Rust policy admits', () => {
		// The same admitted set the desktop Rust test pins
		// (`only_kernel_issued_relative_digest_paths_are_admitted`):
		// `/api/...` routes and the custom-surface asset prefix stay
		// admitted, so the `//` refusal widened nothing.
		expect(
			surfaceFrameSourceIsAdmitted('/api/magician/v2/apps/installations/install_1/surfaces/canvas')
		).toBe(true);
		expect(
			surfaceFrameSourceIsAdmitted(
				'/api/magician/v2/apps/installations/install_1/custom-surface-v1/assets/blake3:abc/surfaces/canvas.html'
			)
		).toBe(true);
	});

	it('exports the closed notices as kernel constants', () => {
		expect(SCRIPTED_SURFACE_SANDBOX).toBe('allow-scripts');
		expect(SCRIPTED_SURFACE_UNSUPPORTED_NOTICE).toBe(
			'Custom surfaces are not supported on this client.'
		);
	});
});
