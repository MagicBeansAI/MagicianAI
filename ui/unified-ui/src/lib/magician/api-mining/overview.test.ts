import { describe, expect, it } from 'vitest';
import { applyRelevanceFilter, groupBySite, recipeRows, recipeReplayUrl, recipeReplayUncertainty, recipeReplayTone, recipeRunTone, staleRecipeDetailIds, submitRecipeReplay, type CapabilityOverview } from './overview';

describe('API mining overview', () => {
	it('makes uncertain writes explicitly non-retryable, independently of persistence warnings', () => {
		expect(recipeReplayUncertainty({ effect_uncertain: true, state_persistence_warning: 'not saved' })).toContain('Do not retry');
		expect(recipeReplayUncertainty({ success: true, state_persistence_warning: 'not saved' })).toBeNull();
		expect(recipeReplayUncertainty(null)).toBeNull();
	});
	it('binds manual replay to the inspected recipe version', () => {
		expect(recipeReplayUrl('recipe/a', 2)).toBe('/api/magician/v2/api-mining/recipes/recipe%2Fa/replay?expected_version=2');
		expect(() => recipeReplayUrl('recipe', 0)).toThrow('Load recipe details');
	});
	it('invalidates changed or removed definitions without discarding unchanged forms', () => {
		const details = { changed: { current_version: 1 }, current: { current_version: 2 }, gone: { current_version: 1 } };
		expect(staleRecipeDetailIds([{ id: 'changed', version: 2 }, { id: 'current', version: 2 }], details)).toEqual(['changed', 'gone']);
		expect(details.changed.current_version).toBe(1);
	});
	it('uses recipe outcome rather than HTTP status alone for success styling', () => {
		expect(recipeReplayTone(200, { success: false, failure: { detail: 'Missing input' } })).toBe('error');
		expect(recipeReplayTone(200, { success: true })).toBe('success');
		expect(recipeReplayTone(409, { pending_approval: {} })).toBe('warning');
		expect(recipeReplayTone(0, { effect_uncertain: true })).toBe('warning');
		expect(recipeReplayTone(200, null)).toBe('error');
	});
	it('does not show a terminal failed API write as a successful run', () => {
		expect(recipeRunTone({ rail_ended: 'api', failure_class: 'network' })).toBe('error');
		expect(recipeRunTone({ rail_ended: 'api' })).toBe('success');
		expect(recipeRunTone({ rail_ended: 'api_then_browser', failure_class: 'auth' })).toBe('warning');
	});
	it('submits exactly the inspected version and current form values once', async () => {
		const calls: Array<{ url: string; init: RequestInit }> = [];
		const result = await submitRecipeReplay({ id: 'recipe', version: 2, inputs: { query: 'red' }, hasWriteSteps: false }, async (url, init) => {
			calls.push({ url, init });
			return { status: 200, json: async () => ({ success: true, answer: { count: 0 } }) };
		});
		expect(calls).toHaveLength(1);
		expect(calls[0].url).toContain('expected_version=2');
		expect(JSON.parse(calls[0].init.body as string)).toEqual({ inputs: { query: 'red' } });
		expect(recipeReplayTone(result.status, result.body)).toBe('success');
	});
	it('preserves uncertainty after a lost or unreadable write response without retrying', async () => {
		for (const failure of ['network', 'json', 'envelope', 'gateway']) {
			let calls = 0;
			const result = await submitRecipeReplay({ id: 'recipe', version: 1, inputs: {}, hasWriteSteps: true }, async () => {
				calls += 1;
				if (failure === 'network') throw new Error('connection lost');
				if (failure === 'gateway') return { status: 502, json: async () => ({ error: 'upstream connection lost' }) };
				return { status: 200, json: async () => {
					if (failure === 'json') throw new Error('truncated JSON');
					return {};
				} };
			});
			expect(calls).toBe(1);
			expect(result.status).toBe(0);
			expect(result.body).toMatchObject({ effect_uncertain: true, retryable: false, browser_retry_allowed: false });
		}
	});
	it('does not label a read failure or a pre-submission error as an uncertain write', async () => {
		const request = { id: 'recipe', version: 1, inputs: {}, hasWriteSteps: false };
		await expect(submitRecipeReplay(request, async () => { throw new Error('read failed'); })).rejects.toThrow('read failed');
		let calls = 0;
		await expect(submitRecipeReplay({ ...request, version: 0, hasWriteSteps: true }, async () => {
			calls += 1;
			return { status: 200, json: async () => ({ success: true }) };
		})).rejects.toThrow('Load recipe details');
		expect(calls).toBe(0);
	});
	it('groups capabilities by parent site and counts telemetry separately', () => {
		const capabilities: CapabilityOverview[] = [
			{ id: 'answer', name: 'Answer', origin: 'https://api.example.com', parent_origin: 'https://example.com', relevance: 'answer_bearing' },
			{ id: 'beacon', name: 'Beacon', origin: 'https://metrics.example.com', parent_origin: 'https://example.com', relevance: 'telemetry' }
		];
		expect(groupBySite(capabilities)).toEqual([
			{ site: 'example.com', capabilities: [capabilities[0]], telemetry_hidden: 1 }
		]);
	});

	it('applies the explicit relevance chip selection', () => {
		const capabilities: CapabilityOverview[] = [
			{ id: 'a', name: 'A', relevance: 'answer_bearing' },
			{ id: 'b', name: 'B', relevance: 'third_party_api' }
		];
		expect(applyRelevanceFilter(capabilities, new Set(['answer_bearing']))).toEqual([
			capabilities[0]
		]);
	});

	it('puts the most recently replayed recipe first', () => {
		const rows = recipeRows([
			{ id: 'old', template: 'old', last_replayed_at_ms: 1 },
			{ id: 'new', template: 'new', last_replayed_at_ms: 2 }
		]);
		expect(rows.map((row) => row.id)).toEqual(['new', 'old']);
	});
});
