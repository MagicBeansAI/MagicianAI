import { render, screen } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';

import AppDeclarativeSurface from './AppDeclarativeSurface.svelte';

describe('AppDeclarativeSurface', () => {
	it('renders semantic bounded components as text and preserves typed edit actions', () => {
		const { container } = render(AppDeclarativeSurface, {
			components: [
				{
					kind: 'section', id: 'overview', label: 'Overview', children: [
						{ kind: 'detail', id: 'item_detail', label: 'Item Detail', fields: ['title', 'status'] }
					]
				},
				{ kind: 'list', id: 'item_list', label: 'Item List', fields: ['title'] },
				{ kind: 'table', id: 'item_table', label: 'Item Table', columns: ['title', 'status'] },
				{ kind: 'form', id: 'create_item', label: 'Create Item', fields: ['title', 'status'] }
			],
			records: [{
				entity: 'item', record_id: 'record_1', record_revision: 7,
				fields: { title: '<script>alert(1)</script>', status: 'open' }
			}],
			fieldBindings: [
				{ field: 'title', kind: 'text', required: true, nullable: false, sortable: true, allowedValues: [] },
				{ field: 'status', kind: 'enum', required: true, nullable: false, sortable: true, allowedValues: ['open', 'done'] }
			]
		});
		expect(screen.getByRole('heading', { name: 'Overview' })).toBeInTheDocument();
		expect(screen.getByRole('table', { name: 'Item Table' })).toBeInTheDocument();
		expect(container.querySelector('form')).not.toBeNull();
		expect(screen.getAllByText('<script>alert(1)</script>').length).toBeGreaterThan(0);
		expect(container.querySelector('script')).toBeNull();
		expect(screen.getAllByRole('button', { name: 'Edit' }).length).toBeGreaterThan(0);
	});
});
