import { cleanup, fireEvent, render, screen, within } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import ModelSelectionControl from './ModelSelectionControl.svelte';
import type { ModelChoice } from './modelCatalog';

afterEach(cleanup);
const models: ModelChoice[] = Array.from({ length: 7 }, (_, index) => ({
	id: `model-${index}`, label: `Model ${index}`, model: `api-${index}`, provider: 'openai', connection: 'OpenAI', limits: null,
	capabilities: { tools: true, vision: false, streaming: true, computer_use: false, web_search: false, structured_output: false },
	thinking: { mode: 'effort', levels: ['low', 'high'], default: 'low' },
	choices: { auto: `auto-${index}`, low: `low-${index}`, high: `high-${index}` }, installed: index !== 6
}));

describe('custom model selection panels', () => {
	it('searches provider rows and preserves a compatible thinking level', async () => {
		const change = vi.fn();
		render(ModelSelectionControl, { props: { inline: true, models, selection: { model_id: 'model-0', thinking: 'high' } }, events: { change } });
		expect(screen.queryByRole('combobox')).toBeNull();
		expect(screen.queryByText('api-0')).toBeNull();
		expect(screen.getByRole('button', { name: 'Model 6 · not installed' })).toBeDisabled();
		await fireEvent.input(screen.getByRole('searchbox'), { target: { value: 'api-2' } });
		const choices = screen.getByRole('region', { name: 'Model choices' });
		expect(within(choices).getAllByRole('button')).toHaveLength(1);
		await fireEvent.click(within(choices).getByRole('button', { name: 'Model 2' }));
		expect(change.mock.calls[0][0].detail).toEqual({ model_id: 'model-2', thinking: 'high' });
	});
	it('supports keyboard selection and Escape returns focus to the trigger', async () => {
		render(ModelSelectionControl, { props: { models, selection: { model_id: 'model-0', thinking: 'auto' } } });
		const trigger = screen.getByRole('button', { name: 'Model' });
		await fireEvent.click(trigger);
		const first = screen.getByRole('button', { name: 'Model 0' });
		first.focus();
		await fireEvent.keyDown(first, { key: 'End' });
		expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Model 5' }));
		await fireEvent.keyDown(document.activeElement!, { key: 'Escape' });
		expect(screen.queryByRole('region', { name: 'Model choices' })).toBeNull();
		expect(document.activeElement).toBe(trigger);
	});
});
