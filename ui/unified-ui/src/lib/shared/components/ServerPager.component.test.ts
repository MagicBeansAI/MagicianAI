import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import ServerPagerHarness from '../../../test/fixtures/ServerPagerHarness.svelte';

describe('ServerPager interactions', () => {
	it('moves between pages and exposes the server-backed item range', async () => {
		const user = userEvent.setup();
		render(ServerPagerHarness, {
			currentPage: 2,
			pageCount: 4,
			startItem: 6,
			endItem: 10,
			totalItems: 18
		});

		expect(screen.getByText('Page 2 of 4')).toHaveTextContent('6-10 of 18');
		await user.click(screen.getByRole('button', { name: 'Next page' }));
		expect(screen.getByTestId('pager-page')).toHaveTextContent('3');
		await user.click(screen.getByRole('button', { name: 'First page' }));
		expect(screen.getByTestId('pager-page')).toHaveTextContent('1');
	});

	it('disables navigation at the first and last page boundaries', async () => {
		const { rerender } = render(ServerPagerHarness, {
			currentPage: 1,
			pageCount: 3
		});

		expect(screen.getByRole('button', { name: 'First page' })).toBeDisabled();
		expect(screen.getByRole('button', { name: 'Previous page' })).toBeDisabled();

		await rerender({ currentPage: 3 });
		expect(screen.getByRole('button', { name: 'Next page' })).toBeDisabled();
		expect(screen.getByRole('button', { name: 'Last page' })).toBeDisabled();
	});

	it('blocks every navigation command while loading', () => {
		render(ServerPagerHarness, { currentPage: 2, pageCount: 3, loading: true });

		for (const button of screen.getAllByRole('button')) expect(button).toBeDisabled();
	});

	it('marks cursor-backed totals as lower bounds and disables only jump-to-last', () => {
		render(ServerPagerHarness, {
			currentPage: 2,
			pageCount: 3,
			startItem: 26,
			endItem: 50,
			totalItems: 50,
			pageCountExact: false,
			totalItemsExact: false
		});

		expect(screen.getByText('Page 2 of 3+')).toHaveTextContent('26-50 of 50+');
		expect(screen.getByRole('button', { name: 'Next page' })).toBeEnabled();
		expect(screen.getByRole('button', { name: 'Last page' })).toBeDisabled();
	});
});
