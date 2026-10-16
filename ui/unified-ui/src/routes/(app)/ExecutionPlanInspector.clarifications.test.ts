/**
 * The plan inspector's Clarifications tab answers a question through the HITL
 * respond endpoint. A picked option is a choice, and must post as one: posting
 * the option's value as free text answers a different input type than the one
 * the question asked.
 */
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

const inspector = readFileSync(
	join(process.cwd(), 'src/routes/(app)/ExecutionPlanInspector.svelte'),
	'utf8'
);

describe('ExecutionPlanInspector clarifications', () => {
	it('posts an option button as a choice with the picked id', () => {
		expect(inspector).toContain(
			'on:click={() => void submitPlanQuestion(question.id, option.value)}'
		);
		expect(inspector).toMatch(
			/selectedId\s*\?\s*\{ type: 'choice', selected_id: selectedId \}\s*:\s*\{ type: 'text', value: responseText \}/
		);
	});

	it('no longer copies an option into the free-text answer', () => {
		expect(inspector).not.toMatch(/\[question\.id\]: option\.value/);
	});
});
