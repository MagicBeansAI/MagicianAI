import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

import { BASE_FONT_HREF, COSTUME_FONT_HREF } from './themeFonts';

/**
 * The shell's font request and this module's costume request are two halves of
 * one set. Split across a `.html` file and a `.ts` file, nothing but a test
 * notices when they drift — and the failure mode is silent: a theme renders in
 * a fallback face and looks merely wrong rather than broken.
 */

const appHtml = readFileSync(join(process.cwd(), 'src/app.html'), 'utf8');

function familiesIn(href: string): string[] {
	return [...href.matchAll(/family=([^&:]+)/g)].map((m) => decodeURIComponent(m[1]).replace(/\+/g, ' ')).sort();
}

/** Every family the shell requested before the split, when one stylesheet
 *  carried all of them. The union of the two halves must still be exactly
 *  this — dropping one silently costs a theme its face. */
const EVERY_FAMILY = [
	'Bagel Fat One',
	'Bricolage Grotesque',
	'Bungee',
	'Bungee Shade',
	'Fira Code',
	'Fredoka',
	'Geist',
	'IBM Plex Mono',
	'Inter',
	'JetBrains Mono',
	'Lilita One',
	'Manrope',
	'Newsreader',
	'Outfit',
	'Permanent Marker',
	'Pixelify Sans',
	'Press Start 2P',
	'Quicksand',
	'Rajdhani',
	'Space Grotesk',
	'Special Elite'
].sort();

describe('theme font split', () => {
	it('ships the base half in the shell, byte for byte', () => {
		const href = appHtml.match(/href="(https:\/\/fonts\.googleapis\.com\/css2\?[^"]+)"/)?.[1];
		expect(href, 'app.html must request a Google Fonts stylesheet').toBeTruthy();
		expect(href).toBe(BASE_FONT_HREF);
	});

	it('covers every family between the two halves, with no overlap', () => {
		const base = familiesIn(BASE_FONT_HREF);
		const costume = familiesIn(COSTUME_FONT_HREF);
		expect(base.filter((f) => costume.includes(f))).toEqual([]);
		expect([...base, ...costume].sort()).toEqual(EVERY_FAMILY);
	});

	// The families the default themes resolve to, read off app.css: `:root`
	// gives Quicksand, Fredoka, Outfit, JetBrains Mono and Fira Code; longhand
	// and longhand-dark give Outfit, Manrope and self-hosted Geist Mono (not a
	// Google family); the landing's own --lp-font is Outfit. A visitor on the
	// default theme must never need the costume sheet.
	it('keeps the default themes whole without the costume sheet', () => {
		const base = familiesIn(BASE_FONT_HREF);
		for (const family of [
			'Fira Code',
			'Fredoka',
			'JetBrains Mono',
			'Manrope',
			'Outfit',
			'Quicksand'
		]) {
			expect(base, `${family} is reachable from :root or longhand`).toContain(family);
		}
	});

	it('defers only what a costume theme asks for', () => {
		expect(familiesIn(COSTUME_FONT_HREF)).toContain('Press Start 2P');
		expect(familiesIn(COSTUME_FONT_HREF)).toContain('Special Elite');
	});

	// Asserted against code, not prose: the shell's comment explains why the
	// retirement script left and names the call it used to make, so a bare
	// substring match on the call name matches the explanation instead.
	it('no longer ships the marketing service-worker retirement script', () => {
		expect(appHtml).not.toContain('navigator.serviceWorker');
		expect(appHtml).not.toContain('magican-marketing-worker-retired-v1');
		expect(appHtml).not.toContain('window.location.reload');
	});
});
