import { describe, expect, it } from 'vitest';

import viteConfig, { isMagicianBackendRoute, localScriptedSurfaceCsp } from '../../vite.config';

describe('Magician development backend proxy', () => {
	it('proxies the local resource governor namespace to Magician', () => {
		const config = viteConfig as {
			server?: { proxy?: Record<string, { target?: string }> };
		};

		expect(config.server?.proxy?.['/api/local-resource-governor']?.target).toBe(
			'http://127.0.0.1:3002'
		);
	});

	it('guards governor snapshot requests while the backend is starting', () => {
		expect(isMagicianBackendRoute('/api/local-resource-governor/snapshot')).toBe(true);
		expect(isMagicianBackendRoute('/api/local-resource-governor/snapshot?detail=1')).toBe(true);
		expect(isMagicianBackendRoute('/runtime/resources')).toBe(false);
	});

	const csp = "default-src 'none'; script-src 'self'; connect-src 'none'; frame-ancestors https://connect.magican.ai";
	const localFrame = {
		url: '/api/magician/v2/apps/installations/install_1/custom-surface-v1/assets/session/digest/surfaces/review.html',
		host: 'localhost:5173',
		remoteAddress: '::1'
	};

	it('pins local development frames to their own origin while retaining script and network restrictions', () => {
		expect(localScriptedSurfaceCsp(csp, localFrame)).toBe(
			"default-src 'none'; script-src 'self'; connect-src 'none'; frame-ancestors 'self'"
		);
	});

	it('preserves hosted, remote, non-frame and explicitly denied policies', () => {
		for (const request of [
			{ ...localFrame, host: 'ui.example.com' },
			{ ...localFrame, host: 'localhost.attacker.test' },
			{ ...localFrame, remoteAddress: '192.168.1.20' },
			{ ...localFrame, remoteAddress: undefined },
			{ ...localFrame, url: '/api/magician/v2/apps/installations/install_1/custom-surface-v1/host' }
		]) {
			expect(localScriptedSurfaceCsp(csp, request)).toBe(csp);
		}
		const denied = "default-src 'none'; frame-ancestors 'none'";
		expect(localScriptedSurfaceCsp(denied, localFrame)).toBe(denied);
	});
});
