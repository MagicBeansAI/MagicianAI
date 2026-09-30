import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	fetchCustomSurfaceHost,
	parseCustomSurfaceHostEnvelope,
	parseCustomSurfaceRunReply,
	postCustomSurfaceRunBridge
} from './appCustomSurface';

function envelope(overrides: Record<string, unknown> = {}) {
	return {
		srcdoc: '<html><head><style>body{color:navy}</style></head><body>plan</body></html>',
		sandbox: '',
		csp: "default-src 'none'; connect-src 'none'; frame-ancestors 'none'; form-action 'none'; base-uri 'none'; img-src 'none'; media-src 'none'; font-src 'none'; style-src 'unsafe-inline'; script-src 'none'",
		allowed_assets: ['surfaces/theme.css'],
		session_ref: 'bridge:install_1:1',
		nonce: 'nonce:1',
		installation_id: 'install_1',
		package_revision_ref: 'package-revision:reading-list',
		surface_revision: 1,
		grant_revision: 1,
		change_sequence: 12,
		envelope_digest: 'blake3:abc',
		...overrides
	};
}

describe('parseCustomSurfaceHostEnvelope', () => {
	afterEach(() => vi.unstubAllGlobals());
	it('accepts a no-script envelope for the requested installation', () => {
		const parsed = parseCustomSurfaceHostEnvelope(envelope(), 'install_1');
		expect(parsed.sandbox).toBe('');
		expect(parsed.srcdoc).toContain('<style>body{color:navy}</style>');
		expect(parsed.change_sequence).toBe(12);
	});

	it('refuses the general iframe sandbox tokens', () => {
		expect(() =>
			parseCustomSurfaceHostEnvelope(
				envelope({ sandbox: 'allow-scripts allow-same-origin' }),
				'install_1'
			)
		).toThrow('refused');
		expect(() =>
			parseCustomSurfaceHostEnvelope(envelope({ sandbox: 'allow-forms' }), 'install_1')
		).toThrow('refused');
	});

	it('refuses a host bound to another installation', () => {
		expect(() => parseCustomSurfaceHostEnvelope(envelope(), 'install_2')).toThrow('refused');
	});

	it('refuses a host that would enable scripts in the display frame', () => {
		expect(() =>
			parseCustomSurfaceHostEnvelope(envelope({ sandbox: 'allow-scripts' }), 'install_1')
		).toThrow('refused');
		expect(() =>
			parseCustomSurfaceHostEnvelope(
				envelope({ csp: "default-src 'none'; connect-src 'none'" }),
				'install_1'
			)
		).toThrow('refused');
		expect(() =>
			parseCustomSurfaceHostEnvelope(
				envelope({ csp: "default-src 'none'; script-src 'none'; style-src https:" }),
				'install_1'
			)
		).toThrow('refused');
	});

	it('ignores worker metadata so a scripted session still displays as no-script', () => {
		const parsed = parseCustomSurfaceHostEnvelope(
			envelope({ worker_pid: 42, worker_entry: 'surfaces/app.js', last_render: '<p>w</p>' }),
			'install_1'
		);
		expect(parsed.sandbox).toBe('');
		expect(parsed.srcdoc).toContain('plan');
	});

	it('refuses unowned fields, invalid cursors, and package-path escape', () => {
		expect(() => parseCustomSurfaceHostEnvelope(envelope({ transport: 'https://evil.invalid' }), 'install_1')).toThrow('refused');
		expect(() => parseCustomSurfaceHostEnvelope(envelope({ change_sequence: -1 }), 'install_1')).toThrow('refused');
		expect(() => parseCustomSurfaceHostEnvelope(envelope({ allowed_assets: ['surfaces/../secret'] }), 'install_1')).toThrow('refused');
		expect(() => parseCustomSurfaceHostEnvelope(envelope({ allowed_assets: ['surfaces\\secret.css'] }), 'install_1')).toThrow('refused');
		expect(() => parseCustomSurfaceHostEnvelope(envelope({ allowed_assets: ['surfaces//secret.css'] }), 'install_1')).toThrow('refused');
	});

	it('retires the prior live session only through the authenticated host fetch', async () => {
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(envelope()), { status: 200 })));
		await fetchCustomSurfaceHost('install_1', { replaceSession: 'bridge:install_1:old' });
		expect(fetch).toHaveBeenCalledWith(
			expect.stringContaining('replace_session=bridge%3Ainstall_1%3Aold'),
			expect.objectContaining({ headers: expect.any(Object) })
		);
	});
});

describe('custom-surface typed run bridge', () => {
	afterEach(() => vi.unstubAllGlobals());

	it('accepts only the opaque correlated terminal-withheld projection', () => {
		const parsed = parseCustomSurfaceRunReply({
			run_ref: 'run:app-action:abc',
			status: 'completed',
			terminal: true,
			result_withheld: true,
			retry_disposition: 'none'
		}, 'run:app-action:abc');
		expect(parsed.result_withheld).toBe(true);
		expect('result' in parsed).toBe(false);
	});

	it('rejects response substitution, owner coordinates, and unknown fields', () => {
		const base = {
			run_ref: 'run:app-action:abc',
			status: 'running',
			terminal: false,
			result_withheld: false,
			retry_disposition: 'poll_run'
		};
		expect(() => parseCustomSurfaceRunReply(base, 'run:app-action:other')).toThrow('refused');
		expect(() => parseCustomSurfaceRunReply({ ...base, task_id: 'task-secret' }, base.run_ref)).toThrow('refused');
		expect(() => parseCustomSurfaceRunReply({ ...base, execution_id: 'exec-secret' }, base.run_ref)).toThrow('refused');
		expect(() => parseCustomSurfaceRunReply({ ...base, result: {} }, base.run_ref)).toThrow('refused');
	});

	it('accepts a generation-bound cancellation receipt and rejects mismatched replay', () => {
		const reply = {
			run_ref: 'run:app-action:abc',
			status: 'cancelling',
			terminal: false,
			result_withheld: false,
			cancellation_generation: 1,
			receipt: {
				generation: 1,
				idempotency_key: 'cancel:abc',
				status: 'cancelling',
				requested_at: '2026-08-23T01:02:03Z'
			},
			retry_disposition: 'poll_run'
		};
		expect(parseCustomSurfaceRunReply(reply, reply.run_ref).receipt?.generation).toBe(1);
		expect(() => parseCustomSurfaceRunReply({
			...reply,
			receipt: { ...reply.receipt, generation: 2 }
		}, reply.run_ref)).toThrow('refused');
	});

	it('does not translate transport abort into canonical cancellation', async () => {
		const abort = Object.assign(new Error('aborted'), { name: 'AbortError' });
		const fetchMock = vi.fn().mockRejectedValue(abort);
		vi.stubGlobal('fetch', fetchMock);
		const controller = new AbortController();
		const request = postCustomSurfaceRunBridge(
			'install_1',
			{ method: 'get_action_run' },
			'run:app-action:abc',
			controller.signal
		);
		controller.abort();
		await expect(request).rejects.toMatchObject({ name: 'AbortError' });
		expect(fetchMock).toHaveBeenCalledTimes(1);
		expect(JSON.stringify(fetchMock.mock.calls)).not.toContain('cancel_run');
	});
});
