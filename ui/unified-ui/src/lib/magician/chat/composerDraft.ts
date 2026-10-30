/** Per-tab text recovery. No uploads, credentials from prompts, or server writes. */
export function createComposerDraft(storage: () => Storage = () => window.sessionStorage) {
	let activeKey: string | null = null;
	let savedText = '';
	return {
		sync(key: string, text: string, pendingKey?: string): string {
			if (key !== activeKey) {
				const first = activeKey === null;
				const inherited = activeKey === pendingKey && key !== pendingKey;
				activeKey = key;
				try {
					// The composer can be edited while its session is loading.
					savedText = (first || inherited) && text ? text : storage().getItem(key) ?? '';
					if ((first || inherited) && text) storage().setItem(key, text);
					if (inherited && pendingKey) storage().removeItem(pendingKey);
				} catch {
					savedText = first || inherited ? text : '';
				}
				return savedText;
			}
			if (text !== savedText) {
				savedText = text;
				try {
					if (text) storage().setItem(key, text);
					else storage().removeItem(key);
				} catch {
					// Browser storage restrictions must not prevent typing or sending.
				}
			}
			return text;
		}
	};
}

export function composerDraftKey(parts: readonly (string | null)[]): string {
	return `magician:composer-draft:v1:${JSON.stringify(parts)}`;
}
