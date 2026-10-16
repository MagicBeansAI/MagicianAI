import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { runInNewContext } from 'node:vm';
import { describe, expect, it } from 'vitest';

import {
	DEFAULT_THEME,
	RETIRED_THEME_ALIASES,
	VALID_THEMES,
	normalizeTheme,
	resolveThemeId
} from './themeStore';

const RETIRED: Array<[string, string]> = [
	['bubbly', 'sorbet'],
	['bubbly-dark', 'sorbet-dark'],
	['soft-machine', 'sorbet'],
	['soft-machine-dark', 'sorbet-dark']
];

describe('retired theme ids', () => {
	it.each(RETIRED)('%s resolves to %s', (retired, live) => {
		expect(VALID_THEMES).not.toContain(retired);
		expect(resolveThemeId(retired)).toBe(live);
		// The server's ui_preferences path goes through normalizeTheme.
		expect(normalizeTheme(retired)).toBe(live);
	});

	it('every alias targets a live theme', () => {
		for (const target of Object.values(RETIRED_THEME_ALIASES)) {
			expect(VALID_THEMES).toContain(target);
		}
	});

	it('live ids resolve to themselves; unknown ids do not resolve', () => {
		for (const id of VALID_THEMES) expect(resolveThemeId(id)).toBe(id);
		expect(resolveThemeId('no-such-theme')).toBeNull();
		expect(resolveThemeId('toString')).toBeNull();
		expect(resolveThemeId(null)).toBeNull();
		expect(normalizeTheme('no-such-theme')).toBe(DEFAULT_THEME);
		expect(normalizeTheme(42)).toBe(DEFAULT_THEME);
	});
});

/** Run app.html's pre-hydration theme script, sandboxed, against a fake localStorage. */
function runPreHydration(stored: Record<string, string>): { theme: string | null; storage: Map<string, string> } {
	const html = readFileSync(join(process.cwd(), 'src/app.html'), 'utf8');
	const marker = '<!-- Synchronous theme initialization to prevent flash of wrong theme -->';
	const start = html.indexOf('<script>', html.indexOf(marker)) + '<script>'.length;
	const source = html.slice(start, html.indexOf('</script>', start));
	const storage = new Map(Object.entries(stored));
	const result: { theme: string | null } = { theme: null };
	runInNewContext(source, {
		localStorage: {
			getItem: (k: string) => storage.get(k) ?? null,
			setItem: (k: string, v: string) => void storage.set(k, v),
			removeItem: (k: string) => void storage.delete(k)
		},
		document: { documentElement: { setAttribute: (_: string, v: string) => (result.theme = v) } }
	});
	return { theme: result.theme, storage };
}

describe('app.html pre-hydration script', () => {
	it.each(RETIRED)('a stored %s opens as %s and is rewritten', (retired, live) => {
		const { theme, storage } = runPreHydration({ 'magican-theme': retired });
		expect(theme).toBe(live);
		expect(storage.get('magican-theme')).toBe(live);
	});

	it('a retired legacy-key id also migrates', () => {
		const { theme, storage } = runPreHydration({ 'magician-theme': 'bubbly-dark' });
		expect(theme).toBe('sorbet-dark');
		expect(storage.get('magican-theme')).toBe('sorbet-dark');
		expect(storage.has('magician-theme')).toBe(false);
	});

	it('a live id is kept and an unknown id falls back to sorbet', () => {
		expect(runPreHydration({ 'magican-theme': 'jarvis' }).theme).toBe('jarvis');
		expect(runPreHydration({ 'magican-theme': 'nope' }).theme).toBe('sorbet');
		expect(runPreHydration({}).theme).toBe('sorbet');
	});
});
