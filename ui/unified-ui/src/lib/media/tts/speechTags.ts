/**
 * Speech-tag protocol helpers.
 *
 * The chat backend parses `<speech>` tags into typed `SpeechSegment`s
 * at persist time (see `magician_v2::media_rails::speech_segments`),
 * so for any assistant message that arrived through the normal chat
 * flow the segments come pre-parsed off the envelope. The helpers in
 * this file exist for two reasons:
 *
 *   1. **Pre-field messages** (persisted before `speech_segments`
 *      existed) need a client-side fallback — `resolveSpeechBlocks`
 *      uses `extractSpeechBlocks` for that.
 *   2. **Manual SpeakButton** on typed-turn messages — the LLM
 *      doesn't emit `<speech>` tags there, so the helper returns one
 *      block with the trimmed body and the whole message reads
 *      aloud.
 *
 * Tags are case-insensitive and may span newlines. Inner content is
 * collapsed to single spaces and trimmed so the spoken version sounds
 * natural even when the source had markdown-friendly line breaks.
 *
 * Attribute parsing accepts double-quoted (`emotion="happy"`),
 * single-quoted (`emotion='happy'`), and unquoted bare-word
 * (`emotion=happy`) values — matching the backend parser — so a
 * model that occasionally drops quotes still gets its hints.
 */
// Matches either `<speech>` (no attrs) or `<speech attr=… …>`. The
// attribute string lands in capture group 1, the body in capture
// group 2. Attributes are parsed in a second pass.
const SPEECH_TAG_RE = /<speech\b([^>]*)>([\s\S]*?)<\/speech>/gi;

// Three accepted attribute shapes, in priority order: double-quoted
// (`key="value with spaces"`), single-quoted (`key='value'`), and
// unquoted bare-word (`key=value`). Unquoted values stop at the first
// whitespace so `emotion=happy style=casual` parses cleanly. This
// matches what HTML attribute parsing tolerates and means a quoting
// slip from the model ("emotion=happy" instead of `emotion="happy"`)
// no longer silently drops the hint.
const ATTR_RE =
	/(\w+)\s*=\s*"([^"]*)"|(\w+)\s*=\s*'([^']*)'|(\w+)\s*=\s*([^\s"'>]+)/g;

export type TtsEmotion =
	| 'neutral'
	| 'happy'
	| 'excited'
	| 'concerned'
	| 'apologetic'
	| 'confident'
	| 'playful'
	| 'urgent'
	| 'sad'
	| 'confused';

export type TtsStyle = 'casual' | 'formal' | 'dramatic' | 'deadpan' | 'warm' | 'clinical';

export type TtsPace = 'slow' | 'normal' | 'fast';

export type TtsVoiceMode = 'default' | 'whisper' | 'announcement';

export interface SpeechBlock {
	text: string;
	emotion?: TtsEmotion;
	style?: TtsStyle;
	pace?: TtsPace;
	voice_mode?: TtsVoiceMode;
	emphasis?: string;
}

const EMOTION_VALUES: ReadonlySet<TtsEmotion> = new Set<TtsEmotion>([
	'neutral',
	'happy',
	'excited',
	'concerned',
	'apologetic',
	'confident',
	'playful',
	'urgent',
	'sad',
	'confused'
]);
const STYLE_VALUES: ReadonlySet<TtsStyle> = new Set<TtsStyle>([
	'casual',
	'formal',
	'dramatic',
	'deadpan',
	'warm',
	'clinical'
]);
const PACE_VALUES: ReadonlySet<TtsPace> = new Set<TtsPace>(['slow', 'normal', 'fast']);
const VOICE_MODE_VALUES: ReadonlySet<TtsVoiceMode> = new Set<TtsVoiceMode>([
	'default',
	'whisper',
	'announcement'
]);

function parseAttrs(rawAttrs: string): Partial<SpeechBlock> {
	const out: Partial<SpeechBlock> = {};
	if (!rawAttrs) return out;
	ATTR_RE.lastIndex = 0;
	let m: RegExpExecArray | null;
	while ((m = ATTR_RE.exec(rawAttrs)) !== null) {
		// One of the three alternatives in ATTR_RE matched; pick the
		// non-undefined key/value pair. Double-quoted is groups 1+2,
		// single-quoted is 3+4, unquoted bare-word is 5+6.
		const key = (m[1] ?? m[3] ?? m[5] ?? '').toLowerCase();
		const value = (m[2] ?? m[4] ?? m[6] ?? '').trim();
		if (!key || !value) continue;
		switch (key) {
			case 'emotion': {
				const v = value.toLowerCase() as TtsEmotion;
				if (EMOTION_VALUES.has(v)) out.emotion = v;
				break;
			}
			case 'style': {
				const v = value.toLowerCase() as TtsStyle;
				if (STYLE_VALUES.has(v)) out.style = v;
				break;
			}
			case 'pace': {
				const v = value.toLowerCase() as TtsPace;
				if (PACE_VALUES.has(v)) out.pace = v;
				break;
			}
			case 'voice':
			case 'voice_mode': {
				const v = value.toLowerCase() as TtsVoiceMode;
				if (VOICE_MODE_VALUES.has(v)) out.voice_mode = v;
				break;
			}
			case 'emphasis':
				out.emphasis = value;
				break;
		}
	}
	return out;
}

/**
 * Resolve playback blocks for a message, preferring server-parsed
 * `speech_segments` from the chat envelope when present. Falls back
 * to client-side parsing of the raw body for:
 *
 *  - messages persisted before the backend started parsing,
 *  - manually-triggered SpeakButton clicks on non-voice messages
 *    (where the LLM never emitted `<speech>` tags and we want to
 *    read the whole body).
 *
 * Pass the server-parsed segments alongside the raw body; the helper
 * decides which to use.
 */
export function resolveSpeechBlocks(
	serverSegments: SpeechBlock[] | null | undefined,
	rawMessage: string | null | undefined
): SpeechBlock[] {
	if (serverSegments && serverSegments.length > 0) {
		return serverSegments
			.map((s) => ({
				text: s.text.replace(/\s+/g, ' ').trim(),
				emotion: s.emotion,
				style: s.style,
				pace: s.pace,
				voice_mode: s.voice_mode,
				emphasis: s.emphasis
			}))
			.filter((s) => s.text.length > 0);
	}
	return extractSpeechBlocks(rawMessage);
}

/**
 * Return ordered `SpeechBlock[]`s parsed from `<speech>` tags. When no
 * tags are present, returns a single block with the trimmed full text
 * and no attributes (preserves non-voice behaviour). Empty bodies are
 * dropped so the synth queue doesn't fire silent requests.
 *
 * Prefer `resolveSpeechBlocks(message.speech_segments, raw)` over
 * calling this directly — the backend pre-parses voice-turn replies
 * into typed segments, so re-running the regex on the client is
 * wasted work for the common case.
 */
export function extractSpeechBlocks(rawMessage: string | null | undefined): SpeechBlock[] {
	if (!rawMessage) return [];
	const matches = [...rawMessage.matchAll(SPEECH_TAG_RE)];
	if (matches.length === 0) {
		const trimmed = rawMessage.trim();
		return trimmed.length > 0 ? [{ text: trimmed }] : [];
	}
	const blocks: SpeechBlock[] = [];
	for (const match of matches) {
		const attrs = parseAttrs(match[1] ?? '');
		const body = (match[2] ?? '').replace(/\s+/g, ' ').trim();
		if (!body) continue;
		blocks.push({ text: body, ...attrs });
	}
	return blocks;
}

/**
 * Strip `<speech>` wrappers (with or without attributes) from a
 * message so the chat bubble renders without protocol markers. Inner
 * content is preserved verbatim — tags only are removed.
 */
export function stripSpeechTags(rawMessage: string | null | undefined): string {
	if (!rawMessage) return '';
	return rawMessage.replace(/<speech\b[^>]*>/gi, '').replace(/<\/speech>/gi, '');
}

/**
 * Whether a message contains at least one `<speech>` block. Used to
 * decide whether the message is "voice-shaped" for UI affordances
 * (e.g. a small speaker glyph next to the spoken portion).
 */
export function hasSpeechTags(rawMessage: string | null | undefined): boolean {
	if (!rawMessage) return false;
	SPEECH_TAG_RE.lastIndex = 0;
	return SPEECH_TAG_RE.test(rawMessage);
}
