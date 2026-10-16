import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { DEFAULT_THEME, RETIRED_THEME_ALIASES, VALID_THEMES } from '$lib/shared/stores/themeStore';
import { families } from '../../routes/dev/theme-gallery/plates';

const root = resolve(__dirname, '../..');
const read = (p: string) => readFileSync(resolve(root, p), 'utf8');
const PASTEL = ['sorbet', 'sorbet-dark', 'sky', 'sky-dark', 'candy', 'candy-dark'];

describe('theme registry agreement', () => {
	it('app.html inline list equals VALID_THEMES', () => {
		const html = read('app.html');
		const at = html.indexOf('var validThemes = [');
		const list = html.slice(at, html.indexOf('];', at));
		const ids = [...list.matchAll(/'([a-z0-9-]+)'/g)].map((m) => m[1]);
		expect(ids).toEqual([...VALID_THEMES]);
	});

	it('app.html retired-theme map equals RETIRED_THEME_ALIASES', () => {
		const html = read('app.html');
		const at = html.indexOf('var retiredThemes = {');
		const block = html.slice(at, html.indexOf('};', at));
		const pairs = Object.fromEntries(
			[...block.matchAll(/'([a-z0-9-]+)':\s*'([a-z0-9-]+)'/g)].map((m) => [m[1], m[2]])
		);
		expect(pairs).toEqual({ ...RETIRED_THEME_ALIASES });
	});

	it.each(Object.keys(RETIRED_THEME_ALIASES))('retired %s is gone from every theme surface', (id) => {
		expect(VALID_THEMES).not.toContain(id);
		expect(read('app.css')).not.toContain(`[data-theme="${id}"]`);
		expect(read('lib/shared/components/ThemeSwitcher.svelte')).not.toContain(`id: '${id}'`);
		expect(families.flatMap((f) => f.plates.map((p) => p.id))).not.toContain(id);
	});

	it.each(PASTEL)('%s is registered in app.css, switcher, LeftRail pairs and gallery', (id) => {
		expect(VALID_THEMES).toContain(id);
		expect(read('app.css')).toContain(`[data-theme="${id}"]`);
		expect(read('lib/shared/components/ThemeSwitcher.svelte')).toContain(`id: '${id}'`);
		expect(read('lib/shell/LeftRail.svelte')).toContain(`'${id}'`);
		expect(families.flatMap((f) => f.plates.map((p) => p.id))).toContain(id);
	});

	it('sorbet is the default everywhere the shell hardcodes one', () => {
		expect(DEFAULT_THEME).toBe('sorbet');
		const html = read('app.html');
		expect(html).toMatch(/<html[^>]*data-theme="sorbet"/);
		expect(html).toMatch(/(?::|\|\|)\s*'sorbet';/);
	});
});
