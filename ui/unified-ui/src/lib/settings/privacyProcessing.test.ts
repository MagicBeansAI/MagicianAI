import { describe, expect, it } from 'vitest';
import { parseProcessingSettings } from './privacyProcessing';

describe('parseProcessingSettings', () => {
	it('reads the mode, routing rows and warnings', () => {
		const parsed = parseProcessingSettings({
			mode: 'cloud',
			routing: [
				{ operation: 'summarize', profile: 'operation/summarize_cloud', provider: 'openai', switched: true },
				{ operation: 42 },
				{ operation: 'classify', profile: 'local/qwen', provider: 'ollama', switched: false }
			],
			warnings: ['x', 7]
		});
		expect(parsed.mode).toBe('cloud');
		expect(parsed.routing.map((row) => row.operation)).toEqual(['summarize', 'classify']);
		expect(parsed.routing[0].switched).toBe(true);
		expect(parsed.warnings).toEqual(['x']);
	});

	it('falls back to local for a missing or unknown mode', () => {
		expect(parseProcessingSettings({}).mode).toBe('local');
		expect(parseProcessingSettings({ mode: 'hybrid' }).mode).toBe('local');
		expect(parseProcessingSettings(null).routing).toEqual([]);
	});
});
