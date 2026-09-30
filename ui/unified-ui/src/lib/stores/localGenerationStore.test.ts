import { describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse, textResponse } from '../../test/browser';
import {
	fetchLocalGeneration,
	parseLocalGenerationEnvelope,
	switchLocalGeneration
} from './localGenerationStore';

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
		}
	]
};

describe('localGenerationStore', () => {
	it('parses a kitty envelope including a RAM-tier warning', () => {
		const parsed = parseLocalGenerationEnvelope(envelope);
		expect(parsed?.selected).toBe('gemma4:12b');
		expect(parsed?.models).toHaveLength(2);
		expect(parsed?.models[0].rule_ok).toBe(false);
		expect(parsed?.models[0].warnings[0]).toContain('36 GB+');
	});

	it('loads the current pin', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/settings/local-generation',
				handle: () => jsonResponse(envelope)
			}
		]);
		const loaded = await fetchLocalGeneration();
		expect(loaded.selected).toBe('gemma4:12b');
		expect(loaded.host.memory_gb).toBe(32);
	});

	it('switches even when the RAM rule is violated', async () => {
		installFetchMock([
			{
				method: 'PUT',
				match: '/settings/local-generation',
				handle: () =>
					jsonResponse({
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
					})
			}
		]);
		const result = await switchLocalGeneration('qwen3.8-ud2-mtp');
		expect(result.selected).toBe('qwen3.8-ud2-mtp');
		expect(result.rule_violated).toBe(true);
		expect(result.ollama_reloaded).toBe(true);
	});

	it('surfaces a structured backend failure', async () => {
		installFetchMock([
			{
				method: 'PUT',
				match: '/settings/local-generation',
				handle: () =>
					textResponse(JSON.stringify({ error: 'unknown_local_generation_model' }), {
						status: 400
					})
			}
		]);
		await expect(switchLocalGeneration('nope')).rejects.toThrow('unknown_local_generation_model');
	});
});
