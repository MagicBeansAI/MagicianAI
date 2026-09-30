import { cleanup, render } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import AppCustomSurfaceHost from './AppCustomSurfaceHost.svelte';
import type { AppCustomSurfaceHostEnvelope } from './appCustomSurface';

function envelope(
	overrides: Partial<AppCustomSurfaceHostEnvelope> = {}
): AppCustomSurfaceHostEnvelope {
	return {
		srcdoc: '<html><head></head><body><p>plan</p><script>window.__escaped=true</script></body></html>',
		sandbox: '',
		csp: "default-src 'none'; connect-src 'none'; frame-ancestors 'none'; script-src 'none'; style-src 'unsafe-inline'",
		allowed_assets: ['surfaces/theme.css'],
		session_ref: 'bridge:install_1:1',
		nonce: 'nonce:1',
		installation_id: 'install_1',
		package_revision_ref: 'package-revision:reading-list',
		surface_revision: 1,
		grant_revision: 1,
		change_sequence: 0,
		envelope_digest: 'blake3:abc',
		...overrides
	};
}

afterEach(() => {
	cleanup();
});

describe('AppCustomSurfaceHost live host qualification', () => {
	it('renders the no-script srcdoc iframe and never the general Iframe host', () => {
		const { container } = render(AppCustomSurfaceHost, { envelope: envelope() });
		const frame = container.querySelector('iframe.custom-surface-frame');
		expect(frame).not.toBeNull();
		expect(frame?.getAttribute('sandbox')).toBe('');
		expect(frame?.getAttribute('referrerpolicy')).toBe('no-referrer');
		expect(frame?.getAttribute('srcdoc')).toContain('<p>plan</p>');
		expect(frame?.getAttribute('srcdoc')).toContain('window.__escaped=true');
		expect(frame?.getAttribute('src')).toBeNull();
		expect(container.querySelectorAll('iframe')).toHaveLength(1);
		expect(frame?.className).not.toMatch(/generative/i);
	});

	it('keeps allow-scripts and allow-same-origin off the live frame', () => {
		const { container } = render(AppCustomSurfaceHost, { envelope: envelope() });
		const sandbox = container.querySelector('iframe')?.getAttribute('sandbox') ?? 'missing';
		expect(sandbox).toBe('');
		expect(sandbox).not.toContain('allow-scripts');
		expect(sandbox).not.toContain('allow-same-origin');
	});
});
