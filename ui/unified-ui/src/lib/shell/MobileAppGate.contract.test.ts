import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

const source = readFileSync(join(process.cwd(), 'src/lib/shell/MobileAppGate.svelte'), 'utf8');
const appLayoutGate = readFileSync(join(process.cwd(), 'src/routes/(app)/+layout.ts'), 'utf8');

describe('mobile app gate shell', () => {
	it('drives the marketing shell class from isMarketingPath, not root-only', () => {
		expect(source).toContain('isMarketingPath');
		expect(source).toContain('class:is-root={isMarketingPath($page.url.pathname)}');
		expect(source).not.toContain("class:is-root={$page.url.pathname === '/'}");
	});

	// The gate is enforced twice — the media query and the {#if} — and the two
	// must ask the same question. Keyed off the marketing class, the CSS kept
	// showing "install the app" over a route the script had already allowed.
	it('keys the mobile media query off the route decision, not the marketing class', () => {
		expect(source).toContain('class:is-mobile-open={isMobileOpenPath($page.url.pathname)}');
		expect(source).toContain('.mobile-route-shell:not(.is-mobile-open) .mobile-app-gate');
		expect(source).toContain('.mobile-route-shell:not(.is-mobile-open) .mobile-route-content');
		expect(source).not.toContain('.mobile-route-shell:not(.is-root) .mobile-app-gate');
		expect(source).not.toContain('.mobile-route-shell:not(.is-root) .mobile-route-content');
	});

	// The app shell stays local-only; the alert's attention page is the single
	// named exception, because the message linking it is read off this device.
	it('keeps the app shell local-only apart from off-device surfaces', () => {
		expect(appLayoutGate).toContain('isOffDeviceSurface');
		expect(appLayoutGate).toContain("redirect(302, '/')");
		for (const hostname of ['localhost', '127.0.0.1', 'tauri.localhost']) {
			expect(appLayoutGate).toContain(hostname);
		}
	});
});
