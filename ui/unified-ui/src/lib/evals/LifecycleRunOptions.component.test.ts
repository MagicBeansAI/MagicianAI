import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, expect, it, vi } from 'vitest';
import LifecycleRunOptions from './LifecycleRunOptions.svelte';

const fetchRoutingOverview = vi.hoisted(() => vi.fn());
vi.mock('$lib/stores/modelRoutingStore', () => ({ fetchRoutingOverview }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

it('uses current profile metadata, permits comparisons and bounds their size', async () => {
	fetchRoutingOverview.mockResolvedValue({ profiles: ['a','b','c','new-profile'].map(name => ({ name, model: `model-${name}`, provider: 'provider', class: 'api' })) });
	render(LifecycleRunOptions);
	const select = screen.getByLabelText('Profiles to compare (optional, up to 3)') as HTMLSelectElement;
	await waitFor(() => expect(select.options).toHaveLength(4));
	expect(screen.getByRole('option', { name: 'new-profile — provider / model-new-profile' })).toBeInTheDocument();
	for (const option of select.options) option.selected = true;
	await fireEvent.change(select);
	expect(screen.getByRole('alert')).toHaveTextContent('Select at most three profiles');
	await fireEvent.click(screen.getByRole('button', { name: 'Use configured routes' }));
	expect(select.selectedOptions).toHaveLength(0);
	expect(screen.queryByRole('alert')).not.toBeInTheDocument();
	expect(screen.getByLabelText('Repeats')).toHaveValue('3');
});

it('keeps configured routing available if profile discovery fails', async () => {
	fetchRoutingOverview.mockRejectedValue(new Error('unavailable'));
	render(LifecycleRunOptions);
	expect(await screen.findByRole('status')).toHaveTextContent('configured routes');
	expect(screen.getByRole('button', { name: 'Use configured routes' })).toBeEnabled();
});
