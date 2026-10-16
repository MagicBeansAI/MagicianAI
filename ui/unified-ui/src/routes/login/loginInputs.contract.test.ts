import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

const source = readFileSync(join(process.cwd(), 'src/routes/login/+page.svelte'), 'utf8');

describe('login form on a phone', () => {
	// The server compares identity names exactly, and a phone keyboard
	// capitalises the first letter of a text input by default — so without
	// these a mobile sign-in sends `Owner` for `owner` and the owner reads
	// it as a wrong password. This mattered the moment the critical-request
	// alert's link started being opened on a phone.
	it('turns off autocapitalize and autocorrect on both credential fields', () => {
		const inputs = source.match(/<input[\s\S]*?\/>/g) ?? [];
		const credentialInputs = inputs.filter(
			(input) => input.includes('bind:value={username}') || input.includes('bind:value={password}')
		);
		expect(credentialInputs).toHaveLength(2);
		for (const input of credentialInputs) {
			// `off`, not `none`: the spec makes them the same state and WebKit
			// before iOS 10 understood only `off`, so `off` is the portable half
			// of an equivalent pair.
			expect(input).toContain('autocapitalize="off"');
			expect(input).toContain('autocorrect="off"');
		}
	});
});
