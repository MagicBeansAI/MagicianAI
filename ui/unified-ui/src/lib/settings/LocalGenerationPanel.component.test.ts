import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { installFetchMock, jsonResponse } from '../../test/browser';
import LocalGenerationPanel from './LocalGenerationPanel.svelte';

const envelope = {
	settings_path: '/notes/magician-config.yaml',
	selected: 'gemma4:12b',
	recommended: 'gemma4:12b',
	processing_mode: 'local',
	host: { memory_gb: 32, free_memory_gb: 4, arch: 'arm64', os: 'macos' },
	min_memory_gb: 16,
	requires_arch: 'arm64',
	requires_os: 'macos',
	catalog_path: 'data/magician_v2/local_generation_catalog.yaml',
	warnings: [],
	models: [
		{
			id: 'qwen3.8-ud2-mtp',
			label: 'Qwen 3.8 27B',
			ollama: 'qwen3.8-ud2-mtp',
			install: 'create-gguf',
			min_memory_gb: 36,
			resident_gb: 11,
			disk_gb: 9.8,
			classify_agree_pct: 68,
			distill_recall_pct: 82.7,
			browser_effect: '4/4',
			browser_protocol: '3/4',
			tok_s_channel: 70,
			notes: null,
			selected: false,
			recommended: false,
			rule_ok: false,
			installed: true,
			warnings: ['Qwen 3.8 27B is auto-picked at 36 GB+; this machine has 32 GB']
		},
		{
			id: 'gemma4:12b',
			label: 'Gemma 4 12B',
			ollama: 'gemma4:12b',
			install: 'ollama-pull',
			min_memory_gb: 24,
			resident_gb: 8,
			disk_gb: 7.6,
			classify_agree_pct: 55,
			distill_recall_pct: null,
			browser_effect: '4/4',
			browser_protocol: '2/4',
			tok_s_channel: null,
			notes: null,
			selected: true,
			recommended: true,
			rule_ok: true,
			installed: true,
			warnings: []
		},
		{
			id: 'woof-4b',
			label: 'Underdog Woof 4B',
			ollama: 'woof-4b',
			install: 'mlx-safetensors',
			min_memory_gb: 16,
			resident_gb: 3,
			disk_gb: 2.4,
			classify_agree_pct: 39,
			distill_recall_pct: 82.7,
			browser_effect: '3/4',
			browser_protocol: '2/4',
			tok_s_channel: 86,
			notes: null,
			selected: false,
			recommended: false,
			rule_ok: true,
			installed: true,
			warnings: ['auto-setup would pick gemma4:12b on this 32 GB machine; this is an override']
		}
	]
};

vi.mock('$lib/stores/confirmationStore', () => ({
	requestConfirmation: vi.fn(async () => true)
}));

afterEach(() => cleanup());

describe('LocalGenerationPanel', () => {
	it('renders the kitty, host RAM, and the recommended pin', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/settings/local-generation',
				handle: () => jsonResponse(envelope)
			}
		]);
		render(LocalGenerationPanel);
		expect(await screen.findByText(/32 GB arm64 macos/)).toBeTruthy();
		expect(screen.getByRole('heading', { name: 'On-device generation' })).toBeTruthy();
		expect(screen.getByText(/Gemma 4 12B \(gemma4:12b\)/)).toBeTruthy();
		expect(screen.getByText('Qwen 3.8 27B')).toBeTruthy();
		expect(screen.getByText('Outside RAM rule')).toBeTruthy();
	});

	it('warns then still switches to a model that violates the RAM rule', async () => {
		const { requestConfirmation } = await import('$lib/stores/confirmationStore');
		installFetchMock([
			{
				method: 'GET',
				match: '/settings/local-generation',
				handle: () => jsonResponse(envelope)
			},
			{
				method: 'PUT',
				match: '/settings/local-generation',
				handle: (call) => {
					const body = JSON.parse(String(call.init?.body)) as { selected: string; reload_ollama: boolean };
					expect(body.selected).toBe('qwen3.8-ud2-mtp');
					expect(body.reload_ollama).toBe(true);
					return jsonResponse({
						...envelope,
						selected: 'qwen3.8-ud2-mtp',
						previous: 'gemma4:12b',
						rule_violated: true,
						config_reloaded: true,
						ollama_reloaded: true,
						models: envelope.models.map((model) => ({
							...model,
							selected: model.id === 'qwen3.8-ud2-mtp'
						}))
					});
				}
			}
		]);
		const user = userEvent.setup();
		render(LocalGenerationPanel);
		const qwen = await screen.findByRole('radio', { name: /Qwen 3.8 27B/ });
		await user.click(qwen);
		await user.click(screen.getByRole('button', { name: 'Switch anyway and reload' }));
		await waitFor(() => expect(requestConfirmation).toHaveBeenCalled());
		expect(requestConfirmation).toHaveBeenCalledWith(
			expect.objectContaining({
				confirmLabel: 'Switch anyway',
				title: expect.stringMatching(/outside the RAM rule/i)
			})
		);
	});
});
