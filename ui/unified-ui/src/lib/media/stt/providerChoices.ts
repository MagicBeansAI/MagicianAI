import type { ProviderInfo, ProviderSnapshot } from '$lib/media/providers';

export function recordingSttProviderChoices(
	providers: Pick<ProviderSnapshot, 'stt' | 'stt_fallbacks'>
): ProviderInfo[] {
	return uniqueProviders([
		...(providers.stt ? [providers.stt] : []),
		...(providers.stt_fallbacks ?? [])
	]);
}

export function uniqueProviders<T extends { id: string }>(providers: T[]): T[] {
	const seen = new Set<string>();
	return providers.filter((provider) => {
		const key = provider.id.toLowerCase();
		if (seen.has(key)) return false;
		seen.add(key);
		return true;
	});
}

export function isProviderVisibleInBrowser(_provider: ProviderInfo): boolean {
	return true;
}

export function hasHiddenMacosSttProvider(_providers: ProviderInfo[]): boolean {
	return false;
}

export function normalizeRecordingSttProvider(
	provider: string,
	visibleProviders: ProviderInfo[],
	hiddenMacosSttProvider: boolean
): string {
	const current = provider || 'auto';
	if (
		current !== 'auto'
		&& visibleProviders.length > 0
		&& !visibleProviders.some((candidate) => candidate.id === current)
	) {
		return 'auto';
	}
	if (hiddenMacosSttProvider && current === 'auto' && visibleProviders[0]) {
		return visibleProviders[0].id;
	}
	return current;
}

export function providerDisplayName(provider: ProviderInfo): string {
	if (provider.label?.trim()) return provider.label.trim();
	if (provider.id === 'macos_speech') return 'macOS Speech';
	if (provider.id === 'openai') {
		return provider.model ? `OpenAI · ${provider.model}` : 'OpenAI';
	}
	if (provider.model && provider.model !== provider.id) {
		return `${provider.id} · ${provider.model}`;
	}
	return provider.id;
}
