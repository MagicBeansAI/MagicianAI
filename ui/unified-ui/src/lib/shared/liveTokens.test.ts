import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

const css = readFileSync(resolve(__dirname, '../../app.css'), 'utf8');

/** The balanced `{…}` block whose opening brace is the first one at or after `from`. */
function blockFrom(from: number): string {
	let depth = 0;
	for (let i = css.indexOf('{', from); i < css.length; i++) {
		if (css[i] === '{') depth++;
		if (css[i] === '}' && --depth === 0) return css.slice(from, i + 1);
	}
	throw new Error(`unterminated block at ${from}`);
}

/** The first `:root {` block, the fallback every theme inherits. */
function rootBlock(): string {
	const start = css.indexOf(':root {');
	if (start === -1) throw new Error('app.css has no :root block');
	return blockFrom(start);
}

/**
 * The reduced-motion block that carries the duration tokens. `app.css` has
 * more than one `prefers-reduced-motion: reduce` block (the later one neutralises
 * transforms), so find the one this contract lives in rather than the first.
 */
function reducedMotionTokenBlock(): string {
	const marker = '@media (prefers-reduced-motion: reduce)';
	for (let at = css.indexOf(marker); at !== -1; at = css.indexOf(marker, at + 1)) {
		const block = blockFrom(at);
		if (block.includes('--dur-base')) return block;
	}
	throw new Error('no reduced-motion block sets --dur-base');
}

export const LIVE_TOKENS = [
	'--spring-snappy', '--spring-gentle', '--spring-bouncy',
	'--dur-quick', '--dur-base', '--dur-slow',
	'--surface-glass', '--blur-glass', '--shadow-float', '--ring-focus',
	'--tint-1', '--tint-2', '--tint-3', '--tint-4', '--tint-5',
	'--tint-1-ink', '--tint-2-ink', '--tint-3-ink', '--tint-4-ink', '--tint-5-ink',
	'--wash-hero', '--wash-card'
];

describe('liveliness tokens', () => {
	it.each(LIVE_TOKENS)('%s has a :root fallback so every theme resolves it', (token) => {
		expect(rootBlock()).toMatch(new RegExp(`${token}\\s*:`));
	});

	it('reduced motion collapses durations', () => {
		const rm = reducedMotionTokenBlock();
		expect(rm).toMatch(/--dur-quick\s*:\s*0ms/);
		expect(rm).toMatch(/--dur-base\s*:\s*0ms/);
		expect(rm).toMatch(/--dur-slow\s*:\s*0ms/);
	});
});
