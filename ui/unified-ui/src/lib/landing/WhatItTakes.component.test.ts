import { cleanup, render, screen, within } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';
import WhatItTakes from './WhatItTakes.svelte';

describe('WhatItTakes', () => {
	afterEach(cleanup);

	it('presents the restored requirements as one accessible checklist', () => {
		render(WhatItTakes);

		expect(screen.getByRole('heading', { name: 'What it takes.' })).toBeInTheDocument();
		expect(screen.getByText('Three things. Nothing more.')).toBeInTheDocument();

		const checklist = screen.getByRole('list');
		const requirements = within(checklist).getAllByRole('listitem');
		expect(requirements).toHaveLength(3);
		expect(within(requirements[0]).getByText('A Mac')).toBeInTheDocument();
		expect(within(requirements[1]).getByText('A browser it can drive')).toBeInTheDocument();
		expect(within(requirements[2]).getByText('A little trust')).toBeInTheDocument();
	});

	it('keeps each practical status attached to its requirement', () => {
		render(WhatItTakes);

		const requirements = screen.getAllByRole('listitem');
		const firstStatus = within(requirements[0]).getByText('macOS 13+');
		expect(firstStatus).toBeInTheDocument();
		// DaisyUI's global `.status` is an 8×8 presence dot. Reusing that class
		// collapses the badge and lets its text spill into the clipped card edge.
		expect(firstStatus).toHaveClass('requirement-status');
		expect(firstStatus).not.toHaveClass('status');
		expect(within(requirements[1]).getByText('Connected')).toBeInTheDocument();
		expect(within(requirements[2]).getByText('Earned over time')).toBeInTheDocument();
	});
});
