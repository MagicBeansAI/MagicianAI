/** Normalize wake transcripts and configured names through one boundary. */
export function normalizeWakeText(value: string): string {
	return value
		.normalize('NFKC')
		.toLowerCase()
		.replace(/[^\p{L}\p{N}]+/gu, ' ')
		.trim()
		.replace(/\s+/g, ' ');
}

/**
 * The UI promises an explicit "Hey <assistant>" invocation. Agent aliases are
 * normally stored without "Hey"; manual overrides may already include it.
 */
export function explicitWakePhrase(configuredPhrase: string): string {
	const normalized = normalizeWakeText(configuredPhrase);
	if (!normalized) return '';
	return normalized === 'hey' || normalized.startsWith('hey ')
		? normalized
		: `hey ${normalized}`;
}

/**
 * Wake is command admission, not keyword spotting inside arbitrary speech.
 * Require a finalized utterance led by the complete explicit invocation while
 * still allowing the request to follow in the same utterance.
 */
export function finalizedWakePhraseMatches(
	finalized: boolean,
	heard: string,
	configuredPhrase: string
): boolean {
	if (!finalized) return false;
	const normalizedHeard = normalizeWakeText(heard);
	const invocation = explicitWakePhrase(configuredPhrase);
	if (!normalizedHeard || !invocation) return false;
	return normalizedHeard === invocation || normalizedHeard.startsWith(`${invocation} `);
}
