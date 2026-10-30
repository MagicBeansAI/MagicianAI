import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { ModelChoice } from '$lib/llm/modelCatalog';
import type { CodingProfile } from '$lib/stores/codingProfileStore';
import CodingEnginePicker from './CodingEnginePicker.svelte';

afterEach(cleanup);
const model: ModelChoice = {
	id: 'sol', label: 'Sol', model: 'sol', provider: 'openai', connection: 'openai', limits: null,
	capabilities: { vision: true, tools: true, streaming: true, computer_use: false, structured_output: false, web_search: false },
	thinking: { mode: 'effort', levels: ['low', 'high'], default: 'low' },
	choices: { auto: 'coding-auto', low: 'coding-low', high: 'coding-high' }
};
const profiles: CodingProfile[] = [
	{ id: 'coding-auto', label: 'Sol', engine: 'pi', model_choice: model, model_selection: { model_id: 'sol', thinking: 'auto' }, supports_user_image_inputs: true, is_default: true },
	{ id: 'coding-low', label: 'Sol', engine: 'pi', model_selection: { model_id: 'sol', thinking: 'low' }, supports_user_image_inputs: true, is_default: false },
	{ id: 'coding-high', label: 'Sol', engine: 'pi', model_selection: { model_id: 'sol', thinking: 'high' }, supports_user_image_inputs: true, is_default: false },
	{ id: 'codex-default', label: 'Codex', engine: 'codex_app_server', supports_user_image_inputs: false, is_default: false }
];

describe('coding composer catalog controls', () => {
	it('shares the visible lightning feedback when switching to a fast coding model', async () => {
		const fast: CodingProfile = { ...profiles[0], id: 'fast-auto', model_choice: { ...model, id: 'custom', provider: 'sarvam', label: 'Custom provider model', model: 'unfamiliar-v1', feedback: 'fast' }, model_selection: { model_id: 'custom', thinking: 'auto' } };
		const available = [...profiles, fast];
		const { container, rerender } = render(CodingEnginePicker, { props: { profiles: available, selectedProfile: profiles[0], selectedProfileId: profiles[0].id } });
		await rerender({ profiles: available, selectedProfile: fast, selectedProfileId: fast.id });
		expect(container.querySelector('.profile-selection-symbol')).toBeNull();
		expect(document.body.querySelector('.profile-selection-symbol--instant')).not.toBeNull();
	});
	it('offers the selected CLI engine’s catalog models and dispatches its native thinking choice', async () => {
		const select = vi.fn();
		const native = { ...model, id: 'codex-sol', label: 'Codex Sol', provider: 'harness-codex', choices: { auto: 'codex-auto', low: 'codex-low', high: 'codex-high' } };
		const profile: CodingProfile = { ...profiles[3], id: 'codex-auto', model_choice: native, model_selection: { model_id: native.id, thinking: 'auto' } };
		render(CodingEnginePicker, { props: { profiles: [...profiles.slice(0, 3), profile], selectedProfile: profile, selectedProfileId: profile.id }, events: { select } });
		await fireEvent.click(screen.getByRole('button', { name: 'Coding model and engine' }));
		await fireEvent.click(screen.getByRole('button', { name: 'Model' }));
		expect(screen.queryByRole('button', { name: 'Sol' })).toBeNull();
		expect(screen.getByRole('button', { name: 'Codex Sol' })).toBeTruthy();
		await fireEvent.keyDown(screen.getByRole('button', { name: 'Codex Sol' }), { key: 'Escape' });
		await fireEvent.keyDown(screen.getByRole('slider', { name: 'Thinking level' }), { key: 'End' });
		expect(select.mock.calls[0][0].detail).toEqual({ id: 'codex-high' });
	});
	it('shows one model and sends the coding handle for the native thinking step', async () => {
		const select = vi.fn();
		render(CodingEnginePicker, { props: { profiles, selectedProfile: profiles[0], selectedProfileId: profiles[0].id }, events: { select } });
		await fireEvent.click(screen.getByRole('button', { name: 'Coding model and engine' }));
		await fireEvent.click(screen.getByRole('button', { name: 'Model' }));
		expect(screen.getAllByRole('button', { name: 'Sol' })).toHaveLength(1);
		expect(screen.queryByRole('combobox')).toBeNull();
		await fireEvent.keyDown(screen.getByRole('button', { name: 'Sol' }), { key: 'Escape' });
		await fireEvent.keyDown(screen.getByRole('slider', { name: 'Thinking level' }), { key: 'End' });
		expect(select.mock.calls[0][0].detail).toEqual({ id: 'coding-high' });
	});
	it('retains distinct engines and displays blocked reasons', async () => {
		const select = vi.fn();
		render(CodingEnginePicker, { props: { profiles, selectedProfile: profiles[0], blockedProfiles: [{ id: 'claude', label: 'Claude Code', engine: 'claude_code', selectable: false, reason: 'Sign in required', supports_user_image_inputs: false, is_default: false }] }, events: { select } });
		await fireEvent.click(screen.getByRole('button', { name: 'Coding model and engine' }));
		await fireEvent.click(screen.getByRole('button', { name: 'Engine' }));
		expect(screen.getByRole('button', { name: /Claude Code.*Sign in required/ })).toBeDisabled();
		await fireEvent.click(screen.getByRole('button', { name: 'Codex' }));
		expect(select.mock.calls[0][0].detail).toEqual({ id: 'codex-default' });
	});
	it('filters the model dropdown to the selected engine and keeps the outer picker open', async () => {
		const select = vi.fn();
		const codex = { ...profiles[3], model: 'gpt-sol' };
		const { rerender } = render(CodingEnginePicker, { props: { profiles: [...profiles.slice(0, 3), codex], selectedProfile: codex, selectedProfileId: codex.id }, events: { select } });
		await fireEvent.click(screen.getByRole('button', { name: 'Coding model and engine' }));
		await fireEvent.click(screen.getByRole('button', { name: 'Model' }));
		expect(screen.queryByRole('button', { name: 'Sol' })).toBeNull();
		await fireEvent.click(screen.getByRole('button', { name: 'gpt-sol' }));
		expect(select.mock.calls[0][0].detail).toEqual({ id: 'codex-default' });
		expect(screen.getByRole('dialog', { name: 'Coding model and engine' })).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: 'Engine' }));
		await fireEvent.click(screen.getByRole('button', { name: 'Pi' }));
		expect(select.mock.calls[1][0].detail).toEqual({ id: 'coding-auto' });
		await rerender({ profiles, selectedProfile: profiles[0], selectedProfileId: profiles[0].id });
		expect(screen.getByRole('slider', { name: 'Thinking level' })).toBeTruthy();
	});
});
