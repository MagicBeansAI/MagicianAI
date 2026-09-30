import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { runInNewContext } from 'node:vm';
import { describe, expect, it } from 'vitest';

const appHtml = readFileSync(join(process.cwd(), 'src/app.html'), 'utf8');
const rootPage = readFileSync(join(process.cwd(), 'src/routes/+page.svelte'), 'utf8');
const lifeDevice = readFileSync(join(process.cwd(), 'src/lib/landing/LifeDevice.svelte'), 'utf8');

describe('landing cold-load contract', () => {
	it('does not make every theme font stylesheet render-blocking', () => {
		const googleFontCatalogs = appHtml.match(/href="https:\/\/fonts\.googleapis\.com\/css2[^\"]+"/g) ?? [];
		expect(googleFontCatalogs).toHaveLength(1);
		const catalogHref = googleFontCatalogs[0];
		if (!catalogHref) throw new Error('missing Google Fonts catalog');
		const catalogStart = appHtml.lastIndexOf('<link', appHtml.indexOf(catalogHref));
		const catalogEnd = appHtml.indexOf('/>', appHtml.indexOf(catalogHref)) + 2;
		const catalog = appHtml.slice(catalogStart, catalogEnd);
		expect(catalog).toContain('media="print"');
		expect(catalog).toContain("onload=\"this.media='all'\"");
	});

	// The hero face is Outfit, served from the shared Google Fonts stylesheet
	// app.html already preconnects to — there is no self-hosted file to
	// preload. `GeistVariable.woff2` still sits in static/ but nothing on this
	// route asks for non-mono Geist, so preloading it spent the cold-load
	// budget on bytes no glyph needed. Geist Mono is self-hosted and used for
	// --lp-mono, deliberately without a preload: its labels are small and none
	// are above the fold at first paint.
	it('preloads no font this route does not paint with', () => {
		expect(rootPage).not.toContain('href="/fonts/geist/GeistVariable.woff2"');
		expect(rootPage).toContain("url('/fonts/geist/GeistMonoVariable.woff2')");
		expect(rootPage).toContain('font-display: swap');
	});

	it('defers third-party analytics until idle', () => {
		expect(appHtml).toContain("c.addEventListener('load', start, { once: true })");
		expect(appHtml).toContain('c.requestIdleCallback(inject, { timeout: 2500 })');
	});

	it('skips analytics for loopback sessions and retains deferred loading elsewhere', () => {
		const tag = appHtml.match(/<script type="text\/javascript">([\s\S]*?clarity[\s\S]*?)<\/script>/)?.[1];
		if (!tag) throw new Error('missing analytics initializer');
		for (const hostname of ['localhost', '127.0.0.1', '127.0.1.2', '[::1]', 'tauri.localhost', 'app.example.com']) {
			let scheduled = false;
			const window = { location: { hostname }, addEventListener: () => { scheduled = true; } };
			runInNewContext(tag, { window, document: { readyState: 'loading' } });
			expect(scheduled, hostname).toBe(hostname === 'app.example.com');
		}
	});

	it('keeps the late laptop composite lazy and low priority', () => {
		expect(lifeDevice).toContain('loading="lazy"');
		expect(lifeDevice).toContain('decoding="async"');
		expect(lifeDevice).toContain('fetchpriority="low"');
	});
});
