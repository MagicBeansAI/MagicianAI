import { describe, expect, it } from 'vitest';

import {
	explicitWakePhrase,
	finalizedWakePhraseMatches,
	normalizeWakeText
} from './wakePhrase';

describe('explicit browser wake admission', () => {
	it('normalizes punctuation and does not duplicate an explicit prefix', () => {
		expect(normalizeWakeText('  Hey,  Nóva! ')).toBe('hey nóva');
		expect(explicitWakePhrase('Nova')).toBe('hey nova');
		expect(explicitWakePhrase('Hey Nova')).toBe('hey nova');
	});

	it('rejects partials, bare names, and incidental background mentions', () => {
		expect(finalizedWakePhraseMatches(false, 'hey nova', 'Nova')).toBe(false);
		expect(finalizedWakePhraseMatches(true, 'nova', 'Nova')).toBe(false);
		expect(finalizedWakePhraseMatches(true, 'I spoke to Nova yesterday', 'Nova')).toBe(false);
		expect(finalizedWakePhraseMatches(true, 'please say hey nova now', 'Nova')).toBe(false);
	});

	it('accepts a phrase-led final utterance with an optional request', () => {
		expect(finalizedWakePhraseMatches(true, 'Hey, Nova!', 'Nova')).toBe(true);
		expect(finalizedWakePhraseMatches(true, 'Hey Nova, open Today', 'Nova')).toBe(true);
		expect(finalizedWakePhraseMatches(true, 'hey novaed', 'Nova')).toBe(false);
	});
});
