import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { afterEach, expect, it, vi } from 'vitest';
import LifecycleRunOptionsHarness from '../../test/fixtures/LifecycleRunOptionsHarness.svelte';
import type { ModelChoice } from '$lib/llm/modelCatalog';

const fetchRoutingOverview = vi.hoisted(() => vi.fn());
vi.mock('$lib/stores/modelRoutingStore', () => ({ fetchRoutingOverview }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

const models: ModelChoice[] = Array.from({ length: 4 }, (_, index) => ({
	id: `model-${index}`, label: `Model ${index}`, provider: 'openai', connection: 'openai', model: `physical-${index}`,
	limits: null, installed: true,
	capabilities: { tools: true, vision: false, streaming: true, computer_use: false, structured_output: true, web_search: false },
	thinking: { mode: 'effort', levels: ['low', 'high'], default: 'low' },
	choices: Object.fromEntries(['auto', 'low', 'high'].map(level => [level, `chat/${index}/${level}`])),
	operation_choices: Object.fromEntries(['auto', 'low', 'high'].map(level => [level, `worker/${index}/${level}`]))
}));
const overview = {
	models: [...models, { ...models[0], id: 'native', label: 'Native CLI', provider: 'harness-codex' }],
	profiles: models.flatMap(model => Object.entries(model.operation_choices!).map(([thinking, name]) => ({
		name, class: 'api', selectable: true, model_selection: { model_id: model.id, thinking }
	})))
};
const options = () => JSON.parse(screen.getByTestId('run-options').textContent!);
async function add(index: number, model: number) {
	await fireEvent.click(screen.getByRole('button', { name: 'Add model' }));
	await fireEvent.click(screen.getByRole('button', { name: `Comparison ${index}` }));
	expect(screen.queryByRole('button', { name: 'Native CLI' })).not.toBeInTheDocument();
	await fireEvent.click(screen.getByRole('button', { name: `Model ${model}` }));
}

it('compares catalog models and native effort using backed operation routes, bounded to three', async () => {
	fetchRoutingOverview.mockResolvedValue(overview);
	render(LifecycleRunOptionsHarness);
	await waitFor(() => expect(screen.getByRole('button', { name: 'Add model' })).toBeEnabled());
	await add(1, 0);
	await fireEvent.click(within(screen.getByRole('group', { name: 'Model comparison 1' })).getByRole('button', { name: 'Set thinking to High' }));
	expect(options()).toEqual({ value: { profiles: ['worker/0/high'], repeats: 3, partition: 'all' }, valid: true });
	await add(2, 1);
	await add(3, 2);
	expect(screen.getByRole('button', { name: 'Add model' })).toBeDisabled();
	expect(options().value.profiles).toEqual(['worker/0/high', 'worker/1/auto', 'worker/2/auto']);
	await fireEvent.click(screen.getByRole('button', { name: 'Use configured routes' }));
	expect(options()).toEqual({ value: { profiles: [], repeats: 3, partition: 'all' }, valid: true });
});

it('refuses duplicate or unavailable comparisons without silently replacing saved choices', async () => {
	fetchRoutingOverview.mockResolvedValue(overview);
	render(LifecycleRunOptionsHarness, { initial: { profiles: ['missing-old-profile'], repeats: 1, partition: 'validation' } });
	await waitFor(() => expect(screen.getByRole('button', { name: 'Comparison 1' })).toBeEnabled());
	expect(options().valid).toBe(false);
	expect(options().value.profiles).toEqual(['missing-old-profile']);
	await fireEvent.click(screen.getByRole('button', { name: 'Comparison 1' }));
	await fireEvent.click(screen.getByRole('button', { name: 'Model 0' }));
	await add(2, 0);
	expect(options().valid).toBe(false);
	expect(screen.getByRole('alert')).toHaveTextContent('Each comparison must be different');
});

it('keeps configured routes available when catalog discovery fails, requiring explicit clearing of a saved comparison', async () => {
	fetchRoutingOverview.mockRejectedValue(new Error('unavailable'));
	render(LifecycleRunOptionsHarness, { initial: { profiles: ['worker/0/high'] } });
	expect(await screen.findByRole('status')).toHaveTextContent('configured routes');
	expect(options().valid).toBe(false);
	expect(options().value.profiles).toEqual(['worker/0/high']);
	await fireEvent.click(screen.getByRole('button', { name: 'Use configured routes' }));
	expect(options().valid).toBe(true);
	expect(screen.getByRole('button', { name: 'Add model' })).toBeDisabled();
});

it('does not submit a saved comparison for an uninstalled model', async () => {
	fetchRoutingOverview.mockResolvedValue({ ...overview, models: overview.models.map(model => ({ ...model, installed: false })) });
	render(LifecycleRunOptionsHarness, { initial: { profiles: ['worker/0/high'] } });
	await waitFor(() => expect(screen.getByRole('button', { name: 'Comparison 1' })).toBeEnabled());
	expect(options().valid).toBe(false);
	expect(options().value.profiles).toEqual(['worker/0/high']);
	await fireEvent.click(screen.getByRole('button', { name: 'Comparison 1' }));
	expect(screen.getByRole('button', { name: 'Model 0 · not installed' })).toBeDisabled();
});
