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
	it('moves from proposition to proof to freedom to trust to manifesto to action', () => {
		const order = [
			position('<HeroSplit'),
			position('<HowTrack'),
			position('<DayTrack />'),
			position('<LifeTrack />'),
			position('<TrustReveal />'),
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
		expect(route).toContain('<title>Magican — Superpowers for Work, Play and all your side quests</title>');
	});

	it('renders no visitor-facing theme control', () => {
		expect(route).not.toContain('.theme-switcher');
		expect(route).not.toContain('.lp-chrome');
	});

	it('closes on the superpowers drop line and links the manifesto', () => {
		expect(route).toContain('Superpowers for work, play, and all your side quests.');
		expect(route).toContain('href="/manifesto"');
		expect(route).not.toContain('For everyone you are.');
		expect(route).not.toContain('Software that learns your ways');
	});
});
