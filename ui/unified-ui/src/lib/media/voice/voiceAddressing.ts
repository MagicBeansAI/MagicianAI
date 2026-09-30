export interface VoiceAddressingConfig {
	required: boolean;
	activation_phrases: string[];
	follow_up_window_ms?: number;
}

export const DEFAULT_VOICE_ADDRESSING: VoiceAddressingConfig = {
	required: false,
	activation_phrases: [],
	follow_up_window_ms: 8_000
};

export interface AgentWakeIdentity {
	name?: string;
	aliases?: string[];
	wake_spellings?: string[];
}

function normalizedNames(values: Array<string | undefined>): string[] {
	const seen = new Set<string>();
	const names: string[] = [];
	for (const raw of values) {
		const name = (raw ?? '').trim().replace(/^hey\s+/i, '').trim();
		if (!name) continue;
		const key = name.toLowerCase();
		if (seen.has(key)) continue;
		seen.add(key);
		names.push(name);
	}
	return names;
}

/** Every name the backend voice-address gate admits for this agent. */
export function activationNamesForAgent(identity: AgentWakeIdentity | null): string[] {
	if (!identity) return [];
	return normalizedNames([
		...(identity.aliases ?? []),
		identity.name,
		...(identity.wake_spellings ?? [])
	]);
}

/** Names a constrained local recognizer should arm, matching the native clients. */
export function constrainedWakeNamesForAgent(identity: AgentWakeIdentity | null): string[] {
	if (!identity) return [];
	const spellings = normalizedNames(identity.wake_spellings ?? []);
	return spellings.length > 0
		? spellings
		: normalizedNames([...(identity.aliases ?? []), identity.name]);
}

export interface VoiceAddressingDecision {
	kind: 'admitted' | 'armed' | 'rejected';
	text: string | null;
	armedUntilMs: number;
}

interface WordSpan {
	value: string;
	end: number;
}

function words(text: string): WordSpan[] {
	const result: WordSpan[] = [];
	for (const match of text.matchAll(/[\p{L}\p{N}]+/gu)) {
		const value = match[0];
		const start = match.index ?? 0;
		result.push({ value: value.toLowerCase(), end: start + value.length });
	}
	return result;
}

/**
 * Admit an utterance and remove its assistant-addressing prefix.
 *
 * Matching is token-based so punctuation and casing are harmless, while partial
 * names such as `Sam` in `Samantha` do not activate a call. The prefix must be
 * the first spoken phrase; ambient speech followed by "Hey Sam" is rejected.
 */
export function admitAddressedTranscript(
	transcript: string,
	config: VoiceAddressingConfig
): string | null {
	return decideAddressedTranscript(transcript, config).text;
}

export function decideAddressedTranscript(
	transcript: string,
	config: VoiceAddressingConfig,
	armedUntilMs = 0,
	nowMs = Date.now()
): VoiceAddressingDecision {
	const trimmed = transcript.trim();
	if (!trimmed) return { kind: 'rejected', text: null, armedUntilMs: 0 };
	if (!config.required) return { kind: 'admitted', text: trimmed, armedUntilMs: 0 };

	const transcriptWords = words(trimmed);
	const phrases = config.activation_phrases
		.map((phrase) => words(phrase))
		.filter((phrase) => phrase.length > 1)
		.sort((left, right) => right.length - left.length);

	for (const phrase of phrases) {
		if (phrase.length > transcriptWords.length) continue;
		const matches = phrase.every(
			(word, index) => transcriptWords[index]?.value === word.value
		);
		if (!matches) continue;
		if (phrase.length === transcriptWords.length) {
			const windowMs = Math.max(1_000, config.follow_up_window_ms ?? 8_000);
			return { kind: 'armed', text: null, armedUntilMs: nowMs + windowMs };
		}

		const remainder = trimmed
			.slice(transcriptWords[phrase.length - 1].end)
			.replace(/^[\s,.:;!?"'\-\u2013\u2014\u2018\u2019]+/u, '')
			.trim();
		return remainder
			? { kind: 'admitted', text: remainder, armedUntilMs: 0 }
			: { kind: 'rejected', text: null, armedUntilMs: 0 };
	}

	if (armedUntilMs > 0 && armedUntilMs >= nowMs) {
		return { kind: 'admitted', text: trimmed, armedUntilMs: 0 };
	}
	return { kind: 'rejected', text: null, armedUntilMs: 0 };
}
