import { describe, expect, it } from 'vitest';
import { narrate, recipeCue } from './recipeNarrative';

describe('recipe replay narrative', () => {
	it('explains a browserless success and healed session', () => {
		expect(
			narrate([
				{ kind: 'recipe.replay.started', recipe_id: 'recipe_1' },
				{ kind: 'recipe.replay.auth.healed' },
				{ kind: 'recipe.replay.completed', duration_ms: 42 }
			])
		).toEqual([
			'Answered from the learned API recipe in 42ms; no browser was opened.',
			'The saved session had expired; it was refreshed automatically and the request retried.'
		]);
	});

	it('explains exact-step fallback and recovery', () => {
		expect(
			narrate([
				{ kind: 'recipe.replay.fallback.handoff', step_id: 'step_3', class: 'schema_drift', replayed_steps: 2 },
				{ kind: 'recipe.replay.recompiled', version: 4 }
			])
		).toEqual([
			'The API completed 2 steps; step_3 schema drift, so the browser continued from there.',
			'The browser run taught the recipe what changed; version 4 will be used next time.'
		]);
	});

	it('summarises browserless, recovered, and denied rails as task cues', () => {
		expect(
			recipeCue([
				{ kind: 'recipe.replay.started' },
				{ kind: 'recipe.replay.completed', duration_ms: 42 }
			])
		).toBe('Learned API · no browser · 42ms');
		expect(
			recipeCue([
				{ kind: 'recipe.replay.fallback.handoff', step_id: 'step_3', replayed_steps: 2 },
				{ kind: 'recipe.replay.recompiled', version: 4 }
			])
		).toBe('Browser · recipe v4 learned');
		expect(
			recipeCue([{ kind: 'recipe.replay.approval.resolved', decision: 'deny' }])
		).toBe('Stopped · write not approved');
	});
});
