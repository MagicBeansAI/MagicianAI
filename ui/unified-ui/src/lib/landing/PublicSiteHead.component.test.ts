import { cleanup, render } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import PublicSiteHead from './PublicSiteHead.svelte';
import site from './publicSite.json';

beforeEach(() => vi.stubEnv('VITE_MARKETING_BUILD', 'true'));
afterEach(() => { cleanup(); vi.unstubAllEnvs(); });

const meta = (key: string) => document.head.querySelector(`meta[name="${key}"],meta[property="${key}"]`)?.getAttribute('content');

describe('public site metadata', () => {
	it('provides one complete, absolute social preview and parseable structured data', () => {
		render(PublicSiteHead, { path: '/' });
		expect(document.title).toBe(site.pages['/'].title);
		expect(document.head.querySelectorAll('meta[name="description"]')).toHaveLength(1);
		expect(document.head.querySelector('link[rel="canonical"]')).toHaveAttribute('href', site.origin + '/');
		expect(meta('og:url')).toBe(site.origin + '/');
		expect(meta('og:image')).toBe(site.origin + '/og-image.png');
		expect(meta('twitter:image')).toBe(meta('og:image'));
		expect(meta('og:image:width')).toBe('1200');
		expect(meta('og:image:height')).toBe('630');
		expect(meta('robots')).toMatch(/^index,/);
		const schema = JSON.parse(document.head.querySelector('script[type="application/ld+json"]')!.textContent!);
		expect(schema['@graph'].map((entry: Record<string, unknown>) => entry['@type'])).toEqual(['WebSite', 'WebPage']);
	});

	it('updates legal-page metadata without leaving duplicate or stale tags', async () => {
		const { rerender } = render(PublicSiteHead, { path: '/' });
		await rerender({ path: '/privacy' });
		expect(document.title).toBe('Privacy — Magican');
		expect(meta('og:description')).toBe(site.pages['/privacy'].description);
		expect(meta('og:url')).toBe(site.origin + '/privacy');
		expect(document.head.querySelectorAll('meta[property="og:title"]')).toHaveLength(1);
		expect(document.head.querySelectorAll('link[rel="canonical"]')).toHaveLength(1);
		await rerender({ path: '/work' });
		expect(meta('robots')).toBe('noindex,nofollow');
		expect(document.head.querySelector('link[rel="canonical"]')).toBeNull();
		expect(meta('og:description')).toBeUndefined();
	});

	it('keeps personal-app and development builds out of search indexes', () => {
		vi.stubEnv('VITE_MARKETING_BUILD', 'false');
		render(PublicSiteHead, { path: '/' });
		expect(meta('robots')).toBe('noindex,nofollow');
	});
});
