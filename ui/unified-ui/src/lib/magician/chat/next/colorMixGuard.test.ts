import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join, relative } from 'node:path';
import { describe, expect, it } from 'vitest';

// `--wash-*` tokens are gradients/images in the pastel themes. `color-mix()`
// only accepts colours, so mixing a wash makes the whole declaration invalid
// and the browser drops it (transparent background). Mix `--bg-card` /
// `--bg-elevated` instead.
const ROOT = join(process.cwd(), 'src/lib/magician/chat/next');

function svelteFiles(dir: string): string[] {
	return readdirSync(dir).flatMap((name) => {
		const path = join(dir, name);
		if (statSync(path).isDirectory()) return svelteFiles(path);
		return name.endsWith('.svelte') ? [path] : [];
	});
}

/** Return the balanced argument text of every `color-mix(` call. */
function colorMixArgs(source: string): string[] {
	const out: string[] = [];
	let from = 0;
	for (;;) {
		const at = source.indexOf('color-mix(', from);
		if (at < 0) return out;
		let depth = 1;
		let i = at + 'color-mix('.length;
		for (; i < source.length && depth > 0; i++) {
			if (source[i] === '(') depth++;
			else if (source[i] === ')') depth--;
		}
		out.push(source.slice(at + 'color-mix('.length, i - 1));
		from = i;
	}
}

describe('chatNext color-mix guard', () => {
	it('parses nested color-mix arguments', () => {
		expect(colorMixArgs('a: color-mix(in srgb, var(--wash-card) 5%, red);')).toEqual([
			'in srgb, var(--wash-card) 5%, red'
		]);
	});

	it('never mixes a --wash-* (gradient/image) token', () => {
		const files = svelteFiles(ROOT);
		expect(files.length).toBeGreaterThan(0);
		const offenders = files.flatMap((file) =>
			colorMixArgs(readFileSync(file, 'utf8'))
				.filter((args) => args.includes('--wash-'))
				.map((args) => `${relative(ROOT, file)}: color-mix(${args})`)
		);
		expect(offenders).toEqual([]);
	});
});

