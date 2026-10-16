import { cleanup, render, screen, within } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import type { StructuredMetricsBlockV1 } from '$lib/magician/structuredResponse/types';
import MetricsHero from './MetricsHero.svelte';

afterEach(() => cleanup());

describe('MetricsHero', () => {
	it('puts the first metric in the hero and the rest in tinted pills', () => {
		const block: StructuredMetricsBlockV1 = {
			kind: 'metrics',
			title: 'Performance',
			items: [
				{ label: 'Latency', value: '102', unit: 'ms', trend: 'down' },
				{ label: 'Throughput', value: '4.2k', unit: 'rps', trend: 'up' },
				{ label: 'Errors', value: '0', trend: 'flat' }
			]
		};
		render(MetricsHero, { block });

		const hero = screen.getByTestId('cn-metric-hero');
		expect(within(hero).getByText('Latency')).toBeInTheDocument();
		expect(within(hero).getByText('102')).toBeInTheDocument();
		expect(within(hero).getByText('ms')).toBeInTheDocument();
		expect(within(hero).getByLabelText('Trending down')).toHaveTextContent('↓');

		const pills = screen.getAllByTestId('cn-metric-pill');
		expect(pills).toHaveLength(2);
		expect(within(pills[0]).getByText('Throughput')).toBeInTheDocument();
		expect(within(pills[0]).getByLabelText('Trending up')).toHaveTextContent('↑');
		expect(within(pills[1]).getByLabelText('Flat')).toHaveTextContent('→');
		expect(pills[0].getAttribute('style')).toContain('--cn-tint');
		expect(screen.getByText('Performance')).toBeInTheDocument();
	});

	it('renders only the hero for a single metric', () => {
		render(MetricsHero, {
			block: { kind: 'metrics', items: [{ label: 'Score', value: '98' }] }
		});

		expect(screen.getByTestId('cn-metric-hero')).toHaveTextContent('98');
		expect(screen.queryAllByTestId('cn-metric-pill')).toHaveLength(0);
		expect(screen.queryByRole('list')).not.toBeInTheDocument();
	});
});
