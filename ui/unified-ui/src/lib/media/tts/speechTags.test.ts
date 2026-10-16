import { describe, expect, it } from 'vitest';
import { extractSpeechBlocks, hasSpeechTags, stripSpeechTags } from './speechTags';

describe('extractSpeechBlocks', () => {
	it('returns a single block with full trimmed text when no tags present', () => {
		const blocks = extractSpeechBlocks('  hello world  ');
		expect(blocks).toEqual([{ text: 'hello world' }]);
	});

	it('returns empty array for empty / whitespace-only input', () => {
		expect(extractSpeechBlocks('')).toEqual([]);
		expect(extractSpeechBlocks(null)).toEqual([]);
		expect(extractSpeechBlocks(undefined)).toEqual([]);
		expect(extractSpeechBlocks('   ')).toEqual([]);
	});

	it('parses one bare speech block', () => {
		const blocks = extractSpeechBlocks(
			'preamble <speech>Done — moved to inbox.</speech> trailing notes'
		);
		expect(blocks).toEqual([{ text: 'Done — moved to inbox.' }]);
	});

	it('parses multiple speech blocks in order', () => {
		const blocks = extractSpeechBlocks(
			'<speech>first part</speech> ignored text <speech>second part</speech>'
		);
		expect(blocks).toEqual([{ text: 'first part' }, { text: 'second part' }]);
	});

	it('drops empty / whitespace-only bodies', () => {
		const blocks = extractSpeechBlocks('<speech>   </speech><speech>real</speech>');
		expect(blocks).toEqual([{ text: 'real' }]);
	});

	it('collapses internal whitespace and newlines', () => {
		const blocks = extractSpeechBlocks('<speech>hello\n\n  world\t  again</speech>');
		expect(blocks).toEqual([{ text: 'hello world again' }]);
	});

	it('parses double-quoted attributes', () => {
		const blocks = extractSpeechBlocks(
			'<speech emotion="apologetic" pace="slow">Sorry, retrying.</speech>'
		);
		expect(blocks).toEqual([
			{ text: 'Sorry, retrying.', emotion: 'apologetic', pace: 'slow' }
		]);
	});

	it('parses single-quoted attributes', () => {
		const blocks = extractSpeechBlocks(
			"<speech emotion='excited' style='casual'>Done!</speech>"
		);
		expect(blocks).toEqual([{ text: 'Done!', emotion: 'excited', style: 'casual' }]);
	});

	it('parses unquoted attributes (relaxed parser)', () => {
		// Models occasionally drop quotes — we should still pick up the hint
		// rather than silently dropping it.
		const blocks = extractSpeechBlocks(
			'<speech emotion=happy pace=fast>Quick win.</speech>'
		);
		expect(blocks).toEqual([{ text: 'Quick win.', emotion: 'happy', pace: 'fast' }]);
	});

	it('maps `voice` and `voice_mode` attributes interchangeably', () => {
		const a = extractSpeechBlocks('<speech voice="whisper">aside</speech>');
		const b = extractSpeechBlocks('<speech voice_mode="whisper">aside</speech>');
		expect(a[0].voice_mode).toBe('whisper');
		expect(b[0].voice_mode).toBe('whisper');
	});

	it('keeps emphasis as a free-form string', () => {
		const blocks = extractSpeechBlocks(
			'<speech emphasis="the deadline is today">Ship it.</speech>'
		);
		expect(blocks[0].emphasis).toBe('the deadline is today');
	});

	it('drops unknown enum values silently', () => {
		const blocks = extractSpeechBlocks(
			'<speech emotion="overjoyed" style="casual">x</speech>'
		);
		// `overjoyed` is not in the enum — dropped, `casual` survives.
		expect(blocks[0]).toEqual({ text: 'x', style: 'casual' });
	});

	it('is case-insensitive on tag name and attribute keys', () => {
		const blocks = extractSpeechBlocks('<SPEECH EMOTION="HAPPY">hi</SPEECH>');
		expect(blocks).toEqual([{ text: 'hi', emotion: 'happy' }]);
	});

	it('supports different attributes per block (mix-and-match)', () => {
		const blocks = extractSpeechBlocks(
			'<speech emotion="apologetic">Sorry.</speech> Meanwhile <speech emotion="confident" pace="fast">retrying now.</speech>'
		);
		expect(blocks).toEqual([
			{ text: 'Sorry.', emotion: 'apologetic' },
			{ text: 'retrying now.', emotion: 'confident', pace: 'fast' }
		]);
	});
});

describe('stripSpeechTags', () => {
	it('removes both bare and attributed opening tags', () => {
		expect(stripSpeechTags('<speech>hello</speech>')).toBe('hello');
		expect(stripSpeechTags('<speech emotion="happy" pace="fast">hi</speech>')).toBe('hi');
	});

	it('preserves inner content verbatim including whitespace', () => {
		expect(stripSpeechTags('<speech>  spaced   text  </speech>')).toBe('  spaced   text  ');
	});
});

describe('hasSpeechTags', () => {
	it('detects bare tags', () => {
		expect(hasSpeechTags('<speech>x</speech>')).toBe(true);
	});

	it('detects attributed tags', () => {
		expect(hasSpeechTags('<speech emotion="happy">x</speech>')).toBe(true);
	});

	it('returns false on plain text', () => {
		expect(hasSpeechTags('just words')).toBe(false);
		expect(hasSpeechTags('')).toBe(false);
		expect(hasSpeechTags(null)).toBe(false);
	});
});
