import { render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import EntityGridServerHarness from '../../../../test/fixtures/EntityGridServerHarness.svelte';
import EntityGrid from './EntityGrid.svelte';

describe('EntityGrid server pagination', () => {
	it('renders the supplied page without slicing it again and requests the next server page', async () => {
		const user = userEvent.setup();
		render(EntityGridServerHarness, {
			rows: [{ title: 'First server row' }, { title: 'Second server row' }],
			currentPage: 2,
			pageCount: 4,
			totalItems: 7,
			startItem: 3,
			endItem: 4
		});

		expect(screen.getByText('First server row')).toBeInTheDocument();
		expect(screen.getByText('Second server row')).toBeInTheDocument();
		expect(screen.getAllByText('Page 2 of 4')[0]).toHaveTextContent('3-4 of 7');

		await user.click(screen.getAllByRole('button', { name: 'Next page' })[0]);
		expect(screen.getByTestId('requested-page')).toHaveTextContent('3');
		await user.click(screen.getByRole('button', { name: 'Sort by Title' }));
		expect(screen.getByTestId('requested-sort')).toHaveTextContent('title:asc');
	});

	it('filters cyclic nested values without recursive traversal', async () => {
		const user = userEvent.setup();
		const cyclic: unknown[] = [];
		cyclic.push(cyclic);
		render(EntityGrid, {
			columns: [{ key: 'title', label: 'Title' }],
			rows: [{ title: 'Safe row', metadata: cyclic }],
			filterKeys: ['metadata']
		});

		await user.type(screen.getByRole('searchbox', { name: 'Filter rows' }), 'missing');
		expect(screen.queryByText('Safe row')).not.toBeInTheDocument();
	});
});
