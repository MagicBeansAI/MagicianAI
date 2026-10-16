import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';

import Tree from './Tree.svelte';

describe('Tree hostile-input bounds', () => {
	it('normalizes iteratively, skips malformed members, and never renders past the depth cap', () => {
		const cyclic: Record<string, unknown> = { id: 'cycle', label: 'Cycle' };
		cyclic.children = [cyclic];
		const nodes = [
			{ id: 'root', label: 'Root', children: [
				{ id: 'child', label: 'Child', children: [
					{ id: 'grandchild', label: 'Grandchild', children: [
						{ id: 'too-deep', label: 'Must not render' }
					] }
				] }
			] },
			null,
			{ id: 'after-invalid', label: 'After invalid' },
			cyclic
		];

		render(Tree, { nodes, expandAll: true, maxDepth: 999 });

		expect(screen.getByText('Root')).toBeInTheDocument();
		expect(screen.getByText('Grandchild')).toBeInTheDocument();
		expect(screen.queryByText('Must not render')).not.toBeInTheDocument();
		expect(screen.getByText('After invalid')).toBeInTheDocument();
		expect(screen.getByText('Cycle')).toBeInTheDocument();
	});

	it('emits a bounded node identity only when selection is enabled', async () => {
		const selected = vi.fn();
		const disabled = render(Tree, {
			props: {
				nodes: [{ id: 'record_1', label: 'One' }],
				selectable: false
			},
			events: { select: selected }
		});
		await fireEvent.click(screen.getByText('One'));
		expect(selected).not.toHaveBeenCalled();
		disabled.unmount();

		render(Tree, {
			props: {
				nodes: [{ id: 'record_1', label: 'One' }],
				selectable: true
			},
			events: { select: selected }
		});
		await fireEvent.click(screen.getByText('One'));
		expect(selected).toHaveBeenCalledTimes(1);
		expect(selected.mock.calls[0][0].detail).toEqual({ nodeId: 'record_1' });
	});
});
