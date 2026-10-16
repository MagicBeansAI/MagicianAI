import { describe, expect, it } from 'vitest';

import {
	isMarketingPath,
	isMobileAllowedPath,
	isOffDeviceSurface,
	MOBILE_APP_MEDIA_QUERY,
	shouldShowMobileAppGate
} from './mobileAccess';

describe('mobile app access policy', () => {
	it('keeps the root landing page available on mobile', () => {
		expect(shouldShowMobileAppGate('/', true)).toBe(false);
	});

	it('keeps the manifesto available on mobile', () => {
		expect(shouldShowMobileAppGate('/manifesto', true)).toBe(false);
	});

	it('keeps the policy pages available on mobile', () => {
		expect(shouldShowMobileAppGate('/privacy', true)).toBe(false);
		expect(shouldShowMobileAppGate('/terms', true)).toBe(false);
	});

	it('blocks destinations without a mobile web interaction contract', () => {
		for (const pathname of ['/today', '/chat', '/warroom', '/notify-overlay']) {
			expect(shouldShowMobileAppGate(pathname, true)).toBe(true);
		}
	});

	it('allows authenticated Work and rejects retired hosts on remote touch screens', () => {
		expect(shouldShowMobileAppGate('/work', true)).toBe(false);
		expect(shouldShowMobileAppGate('/work/', true)).toBe(false);
		expect(isOffDeviceSurface('/work')).toBe(true);
		for (const path of ['/tasks', '/crew', '/crew/new', '/crew/forge/rules', '/crew/forge/memory/shared', '/relentless', '/internal-tasks', '/debug/tasks', '/t/launch/tasks', '/harness']) {
			expect(shouldShowMobileAppGate(path, true)).toBe(true);
			expect(isOffDeviceSurface(path)).toBe(false);
		}
	});

	// The critical-request alert is delivered to the owner's phone and its link
	// opens this page; an install gate there makes the link useless on the very
	// device the alert arrived on.
	it('keeps the attention page available on mobile, with or without a trailing slash', () => {
		expect(shouldShowMobileAppGate('/attention', true)).toBe(false);
		expect(shouldShowMobileAppGate('/attention/', true)).toBe(false);
	});

	it('does not treat the attention page as a marketing path', () => {
		expect(isMarketingPath('/attention')).toBe(false);
		expect(isMobileAllowedPath('/attention')).toBe(true);
		expect(isMobileAllowedPath('/today')).toBe(false);
	});

	// An unauthenticated phone opening the alert link is sent to /login by the
	// root session gate. Gate the login form and the attention exemption is
	// unreachable: the page shows for a frame, then the bounce lands the owner
	// back on "install the app" — which is exactly what it did.
	it('keeps the login page available on mobile, since the alert link bounces through it', () => {
		expect(shouldShowMobileAppGate('/login', true)).toBe(false);
		expect(isMobileAllowedPath('/login')).toBe(true);
	});

	it('keeps unrelated operator pages local-only', () => {
		expect(isOffDeviceSurface('/attention')).toBe(true);
		expect(isOffDeviceSurface('/attention/')).toBe(true);
		for (const pathname of ['/today', '/chat', '/warroom', '/login']) {
			expect(isOffDeviceSurface(pathname)).toBe(false);
		}
	});

	it('keeps non-root routes available on desktop', () => {
		expect(shouldShowMobileAppGate('/today', false)).toBe(false);
	});

	it('requires both a mobile-sized viewport and coarse pointer', () => {
		expect(MOBILE_APP_MEDIA_QUERY).toBe('(max-width: 1023px) and (pointer: coarse)');
	});
	it('allows Work-owned editors and inspectors without granting arbitrary Work subpaths', () => {
		for (const path of ['/work/crew/new', '/work/crew/forge/edit', '/work/crew/forge/rules', '/work/crew/forge/memory/shared/']) {
			expect(shouldShowMobileAppGate(path, true)).toBe(false);
			expect(isOffDeviceSurface(path)).toBe(true);
		}
		for (const path of ['/work/unknown', '/work/crew/forge/delete', '/work/crew/forge/memory/shared/unknown']) {
			expect(shouldShowMobileAppGate(path, true)).toBe(true);
		}
	});
});


it('allows authenticated remote Work specialists while rejecting arbitrary subpaths', () => {
 for (const path of ['/llm','/history','/memory','/budget','/budget/audit','/evals','/apps','/apps/installed/view','/skills','/runtime','/events','/observe']) {
  expect(shouldShowMobileAppGate(path,true)).toBe(false);
  expect(isOffDeviceSurface(path)).toBe(true);
 }
 expect(shouldShowMobileAppGate('/budget/unknown',true)).toBe(true);
 for (const path of ['/work/unknown','/crew/member/delete','/apps-unknown','/t/thread/chat']) expect(isOffDeviceSurface(path)).toBe(false);
});
