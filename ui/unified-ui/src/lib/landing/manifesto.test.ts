import { describe, expect, it } from 'vitest';
import {
	MANIFESTO_DATE,
	MANIFESTO_TITLE,
	MANIFESTO_SIGN,
	MANIFESTO_TAGLINE,
	MANIFESTO_EXCERPT,
	MANIFESTO_BODY,
	manifestoInline,
	manifestoText
} from './manifesto';

describe('manifesto copy', () => {
	it('dates and titles the document', () => {
		expect(MANIFESTO_DATE).toBe('Sunday, 13 September 2026');
		expect(MANIFESTO_TITLE).toBe('Personal Intelligence is your asset');
		expect(MANIFESTO_SIGN).toBe('Magican');
		expect(MANIFESTO_TAGLINE).toBe('More You');
	});

	it('excerpts the opening and timing sequence before the full argument', () => {
		const excerpt = manifestoText(MANIFESTO_EXCERPT);
		expect(excerpt).toContain('The work finished while everyone else was still discussing it.');
		expect(excerpt).toContain('Your company will adapt');
		expect(excerpt).not.toContain('Life outside work is no different');
		expect(manifestoText(MANIFESTO_BODY)).toContain('Life outside work is no different');
		expect(manifestoText(MANIFESTO_BODY)).toContain('It is you who must.');
		expect(manifestoText(MANIFESTO_BODY)).not.toContain('**');
	});

	it('preserves the personal opening and Magican signature', () => {
		const body = manifestoText(MANIFESTO_BODY);
		expect(manifestoText(MANIFESTO_EXCERPT)).toContain('build the life you desire');
		expect(body).toContain('I developed Magican for myself');
		expect(body).toContain("You don't have to keep briefing another chatbot.");
		expect(body.match(/\bMagican\b/g)).toHaveLength(1);
	});

	it('marks It is yours as emphasis', () => {
		const yours = MANIFESTO_BODY.find(
			(block) => block.kind === 'verse' && block.lines.some((line) => line.includes('It is yours'))
		);
		expect(yours?.kind === 'verse' ? yours.lines : []).toContain('**It is yours.**');
		expect(manifestoInline('**It is yours.**')).toEqual([{ strong: true, text: 'It is yours.' }]);
	});

	it('marks It is you who must as emphasis', () => {
		const company = MANIFESTO_BODY.find(
			(block) => block.kind === 'prose' && block.text.includes('Your company will adapt')
		);
		expect(company?.kind === 'prose' ? manifestoInline(company.text) : []).toEqual([
			{
				strong: false,
				text: 'You have to find your own way, hold onto your self-confidence, and create your own opportunities. Your company will adapt, automate, reorganise, and move on. It is under no obligation to keep you relevant. '
			},
			{ strong: true, text: 'It is you who must.' }
		]);
	});

	it('keeps the timing stanza as verse', () => {
		const timing = MANIFESTO_EXCERPT.find((block) => block.kind === 'verse');
		expect(timing?.kind === 'verse' && timing.lines).toEqual([
			'The insight delivered at the right moment.',
			"The detail that wasn't overlooked.",
			'The preparation no one else thought to make.',
			'The work finished while everyone else was still discussing it.'
		]);
	});
});
