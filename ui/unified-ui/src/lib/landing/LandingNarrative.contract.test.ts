import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

const route = readFileSync(join(process.cwd(), 'src/routes/+page.svelte'), 'utf8');

function position(source: string): number {
	const at = route.indexOf(source);
	expect(at, `${source} must be present in the root narrative`).toBeGreaterThanOrEqual(0);
	return at;
}

describe('root landing narrative', () => {
	it('moves from proposition to trust to day to proof to freedom to how-it-works to manifesto to action', () => {
		const order = [
			position('<HeroSplit'),
			position('<TrustReveal />'),
			position('<DayTrack />'),
			position('<HowTrack'),
			position('<LifeTrack />'),
			position('<HowItWorks'),
			position('<ManifestoExcerpt'),
			position('<WhatItTakes />'),
			position('<section class="lp-cta"')
		];
		expect(order).toEqual([...order].sort((a, b) => a - b));
	});

	it('does not mount the retired BrandReveal lockup', () => {
		expect(route).not.toContain('<BrandReveal');
		expect(route).toContain('<LandingField');
		expect(route).toContain('<HeroSplit');
	});

	it('has one Magican split hero and no longer opens on the role cycle', () => {
		expect(route.match(/<HeroSplit/g)).toHaveLength(1);
		expect(route).not.toContain('<Greeting />');
		expect(route).not.toContain('<LandingHero />');
		expect(route).not.toContain('<Declaration />');
		// Public metadata is owned by the root layout's PublicSiteHead.
	});

	it('scopes landing appearance without writing the app theme', () => {
		expect(route).toContain('data-appearance={dark');
		expect(route).not.toContain('document.documentElement.setAttribute');
		expect(route).not.toContain('themeStore');
		expect(route).not.toContain('.lp-chrome');
	});

	it('closes on More You and links the manifesto', () => {
		expect(route).toContain('<span class="lp-footer-tag">More You</span>');
		expect(route).toContain('href="/manifesto"');
		expect(route).toContain('<a href={EXPLAIN_URL}>How it works</a>');
		expect(route).not.toContain('For everyone you are.');
		expect(route).not.toContain('Software that learns your ways');
	});
});
