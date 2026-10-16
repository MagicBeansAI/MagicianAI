import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import PlanGlyph from './PlanGlyph.svelte';

afterEach(cleanup);

describe('PlanGlyph', () => {
	it('renders an svg labelled "Planned task" with a tooltip', () => {
		render(PlanGlyph);
		const glyph = screen.getByRole('img', { name: 'Planned task' });
		expect(glyph.getAttribute('title')).toBe('Planned task');
		const svg = glyph.querySelector('svg');
		expect(svg).not.toBeNull();
		expect(svg?.getAttribute('aria-hidden')).toBe('true');
		expect(svg?.getAttribute('fill')).toBe('currentColor');
	});

	it('defaults to 13px with no layout shift', () => {
		render(PlanGlyph);
		const glyph = screen.getByTestId('plan-glyph');
		expect(glyph.style.width).toBe('13px');
		expect(glyph.style.height).toBe('13px');
		const svg = glyph.querySelector('svg');
		expect(svg?.getAttribute('width')).toBe('13');
		expect(svg?.getAttribute('height')).toBe('13');
	});

	it('honours size and title props', () => {
		render(PlanGlyph, { props: { size: 16, title: 'Plan approved' } });
		const glyph = screen.getByRole('img', { name: 'Plan approved' });
		expect(glyph.style.width).toBe('16px');
		expect(glyph.querySelector('svg')?.getAttribute('width')).toBe('16');
	});

	it('takes its colour from --accent-secondary', () => {
		render(PlanGlyph);
		const glyph = screen.getByTestId('plan-glyph');
		expect(glyph.style.getPropertyValue('color')).toBe('var(--accent-secondary)');
	});
});
