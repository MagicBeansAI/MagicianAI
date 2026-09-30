import type { ProviderSnapshot } from '$lib/media/providers';

export interface ResolvedTtsProviderChoice {
	mode: 'backend' | 'browser' | 'none';
}

export function resolveTtsProviderChoice(
	providers: Pick<ProviderSnapshot, 'tts'>,
	browserAvailable: boolean
): ResolvedTtsProviderChoice {
	if (providers.tts) {
		return { mode: 'backend' };
	}
	if (browserAvailable) {
		return { mode: 'browser' };
	}
	return { mode: 'none' };
}
