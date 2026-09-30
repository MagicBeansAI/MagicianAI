import { describe, expect, it } from 'vitest';

import type { ProviderInfo } from '$lib/media/providers';
import { resolveTtsProviderChoice } from './providerChoices';

const backend: ProviderInfo = {
	id: 'configured-tts',
	label: 'Configured TTS',
	model: 'configured-model'
};

describe('resolveTtsProviderChoice', () => {
	it('defers provider and model selection to the backend when TTS is registered', () => {
		expect(resolveTtsProviderChoice({ tts: backend }, true)).toEqual({ mode: 'backend' });
	});

	it('uses browser speech only when backend TTS is unavailable', () => {
		expect(resolveTtsProviderChoice({ tts: null }, true)).toEqual({ mode: 'browser' });
	});

	it('reports no path when neither backend nor browser TTS is available', () => {
		expect(resolveTtsProviderChoice({ tts: null }, false)).toEqual({ mode: 'none' });
	});
});
