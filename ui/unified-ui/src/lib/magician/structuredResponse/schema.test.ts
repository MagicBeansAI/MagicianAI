import { describe, expect, it } from 'vitest';

import { validateStructuredResponse, isStructuredResponseV1 } from './schema';

describe('structured response schema validation', () => {
	it('accepts a bounded complete variant', () => {
		const result = validateStructuredResponse({
			schema: 'magician.structured_response',
			version: 1,
			plain_text: 'Summary',
			title: 'Execution',
			summary: 'Done',
			tone: 'success',
			blocks: [
				{
					kind: 'text',
					text: 'Result is ready'
				},
				{
					kind: 'callout',
					tone: 'success',
					text: 'All checks passed',
					title: 'Checks'
				},
				{
					kind: 'table',
					columns: [{ key: 'k', label: 'Name' }],
					rows: [{ k: 'alpha' }]
				},
				{
					kind: 'list',
					style: 'steps',
					items: [{ text: 'Run', detail: 'start now' }]
				},
				{
					kind: 'artifacts',
					title: 'Outputs',
					items: [{ label: 'report.pdf', href: 'https://example.com/report.pdf' }]
				},
				{
					kind: 'sources',
					items: [{ href: 'https://example.com', label: 'Source' }]
				},
				{
					kind: 'metrics',
					items: [{ label: 'Latency', value: '120', unit: 'ms', trend: 'up' }]
				},
				{
					kind: 'key_values',
					items: [{ label: 'Mode', value: 'Safe' }]
				}
			],
			actions: [
				{ kind: 'copy_text', label: 'Copy', text: 'hello' },
				{ kind: 'open_url', label: 'Open', url: 'https://docs' }
			]
		});
		expect(result.ok).toBe(true);
	});

	it('rejects wrong schema names', () => {
		expect(
			validateStructuredResponse({
				schema: 'legacy.response',
				version: 1,
				plain_text: 'x',
				blocks: []
			}).ok
		).toBe(false);
	});

	it('rejects unsupported block kinds', () => {
		const response = validateStructuredResponse({
			schema: 'magician.structured_response',
			version: 1,
			plain_text: 'x',
			blocks: [{ kind: 'widget', title: 'x' }]
		});
		expect(response.ok).toBe(false);
		expect(response.reason).toContain('unsupported');
	});

	it('rejects malformed block shape', () => {
		const response = validateStructuredResponse({
			schema: 'magician.structured_response',
			version: 1,
			plain_text: 'x',
			blocks: [{ kind: 'list', items: [{ value: 'missing-text' }] }]
		});
		expect(response.ok).toBe(false);
		expect(response.reason).toContain('must be a string');
	});

	it('supports bounded action validation', () => {
		const response = validateStructuredResponse({
			schema: 'magician.structured_response',
			version: 1,
			plain_text: 'x',
			blocks: [{ kind: 'text', text: 'x' }],
			actions: [{ kind: 'open_url', label: 'Docs', url: 123 }]
		});
		expect(response.ok).toBe(false);
		expect(response.reason).toContain('url');
	});

	it('type guard aligns with strict variants', () => {
		const unknown = {
			schema: 'magician.structured_response',
			version: 1,
			plain_text: 'x',
			blocks: [{ kind: 'text', text: 'x' }]
		};
		expect(isStructuredResponseV1(unknown)).toBe(true);
	});

	it('rejects missing canonical text, empty blocks, unsafe URLs, and server actions', () => {
		const base = {
			schema: 'magician.structured_response',
			version: 1,
			plain_text: 'x',
			blocks: [{ kind: 'text', text: 'x' }]
		};
		expect(validateStructuredResponse({ ...base, plain_text: '' }).ok).toBe(false);
		expect(validateStructuredResponse({ ...base, blocks: [] }).ok).toBe(false);
		expect(
			validateStructuredResponse({
				...base,
				actions: [{ kind: 'open_url', label: 'Open', url: 'javascript:alert(1)' }]
			}).ok
		).toBe(false);
		expect(
			validateStructuredResponse({
				...base,
				actions: [{ kind: 'invoke_server_action', label: 'Run', action_ref: 'legacy' }]
			}).ok
		).toBe(false);
	});

	it('rejects a serialized presentation larger than 64 KiB', () => {
		const maximumField = 'x'.repeat(32 * 1024);
		expect(
			validateStructuredResponse({
				schema: 'magician.structured_response',
				version: 1,
				plain_text: maximumField,
				blocks: [{ kind: 'markdown', text: maximumField }]
			}).ok
		).toBe(false);
	});

	it('rejects unbounded title and metadata fields', () => {
		const base = {
			schema: 'magician.structured_response',
			version: 1,
			plain_text: 'x',
			blocks: [{ kind: 'text', text: 'x' }]
		};
		expect(validateStructuredResponse({ ...base, title: 'x'.repeat(161) }).ok).toBe(false);
		expect(
			validateStructuredResponse({
				...base,
				meta: { provenance: [{ id: 'source', ref: 'x'.repeat(2049) }] }
			}).ok
		).toBe(false);
	});

	it('rejects artifact values that the backend will not admit', () => {
		const base = {
			schema: 'magician.structured_response',
			version: 1,
			plain_text: 'x',
			blocks: [{ kind: 'text', text: 'x' }]
		};
		const withArtifact = (item: Record<string, unknown>) =>
			validateStructuredResponse({
				...base,
				blocks: [{ kind: 'artifacts', items: [item] }]
			}).ok;

		expect(withArtifact({ label: 'report', href: 'javascript:alert(1)' })).toBe(false);
		expect(withArtifact({ label: 'report', artifact_id: 'a'.repeat(161) })).toBe(false);
		expect(withArtifact({ label: 'report', size: -1 })).toBe(false);
		expect(withArtifact({ label: 'report', size: 2_147_483_648 })).toBe(false);
		expect(withArtifact({ label: 'report', source: 'legacy-path' })).toBe(false);
	});

	it('applies the 160-byte artifact action identifier limit', () => {
		expect(
			validateStructuredResponse({
				schema: 'magician.structured_response',
				version: 1,
				plain_text: 'x',
				blocks: [{ kind: 'text', text: 'x' }],
				actions: [{ kind: 'open_artifact', label: 'Open', artifact_id: 'a'.repeat(161) }]
			}).ok
		).toBe(false);
	});

	it('accepts only valid model context and numeric metadata', () => {
		const base = {
			schema: 'magician.structured_response',
			version: 1,
			plain_text: 'x',
			blocks: [{ kind: 'text', text: 'x' }]
		};
		expect(
			validateStructuredResponse({
				...base,
				model_context: { summary: 'shown context', privacy: 'model_visible' },
				meta: { confidence: 0.75, cost: { input_tokens: 1, cost_usd: 0.01, model: 'test' } }
			}).ok
		).toBe(true);
		expect(
			validateStructuredResponse({
				...base,
				model_context: { summary: '', privacy: 'public' }
			}).ok
		).toBe(false);
		expect(validateStructuredResponse({ ...base, meta: { confidence: 1.1 } }).ok).toBe(false);
	});
});
