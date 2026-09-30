import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import LandingChrome from './LandingChrome.svelte';

afterEach(cleanup);

const source = readFileSync(join(process.cwd(), 'src/lib/landing/LandingChrome.svelte'), 'utf8');

describe('LandingChrome', () => {
	it('puts an un-underlined Manifesto link in the top right', () => {
		render(LandingChrome);

		const link = screen.getByRole('link', { name: 'Manifesto' });
		expect(link).toHaveAttribute('href', '/manifesto');
		expect(link.className).toContain('lc-manifesto');
		expect(source).toMatch(/\.lc-manifesto[\s\S]*text-decoration:\s*none/);
		expect(source).not.toContain('>Superpowers</');
	});
});
