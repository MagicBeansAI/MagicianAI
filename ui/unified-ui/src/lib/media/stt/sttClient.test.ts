import { describe, expect, it } from 'vitest';

import { providerRequiresWavUpload } from './sttClient';

describe('recording STT upload format', () => {
	it('normalizes auto and local provider uploads before backend profile resolution', () => {
		expect(providerRequiresWavUpload(undefined)).toBe(true);
		expect(providerRequiresWavUpload('auto')).toBe(true);
		expect(providerRequiresWavUpload('default')).toBe(true);
		expect(providerRequiresWavUpload('macos_speech')).toBe(true);
		expect(providerRequiresWavUpload('fluid-qwen3-asr-f32')).toBe(true);
	});

	it('leaves explicit online provider uploads in their captured format', () => {
		expect(providerRequiresWavUpload('openai')).toBe(false);
		expect(providerRequiresWavUpload('gemini-flash-stt')).toBe(false);
	});
});
