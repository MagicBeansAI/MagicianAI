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
		expect(MANIFESTO_TAGLINE).toBe('Superpowers for work, play and all your side quests');
	});

	it('excerpts the opening and control principle before the full argument', () => {
		const excerpt = manifestoText(MANIFESTO_EXCERPT);
		expect(excerpt).toContain('The work was finished while everyone else was still discussing it.');
		expect(excerpt).not.toContain('Your company will adapt');
		expect(manifestoText(MANIFESTO_BODY)).toContain('Your company will adapt');
		expect(manifestoText(MANIFESTO_BODY)).toContain('It is you who do.');
		expect(manifestoText(MANIFESTO_BODY)).not.toContain('**');
	});

	it('preserves the personal opening and Magican signature', () => {
		const body = manifestoText(MANIFESTO_BODY);
		expect(manifestoText(MANIFESTO_EXCERPT)).toContain('help yourself in building the life you desire');
		expect(manifestoText(MANIFESTO_EXCERPT)).not.toContain('help you in building');
		expect(body).toContain('I developed Magican for myself');
		expect(body).toContain("You don't have to keep briefing another chatbot.");
		expect(body).not.toContain('constantly briefing');
		expect(body.match(/\bMagican\b/g)).toHaveLength(1);
	});

	it('marks It is yours as emphasis', () => {
		const yours = MANIFESTO_BODY.find(
			(block) => block.kind === 'verse' && block.lines.some((line) => line.includes('It is yours'))
		);
		expect(yours?.kind === 'verse' ? yours.lines : []).toContain('**It is yours.**');
		expect(manifestoInline('**It is yours.**')).toEqual([{ strong: true, text: 'It is yours.' }]);
	});

	it('marks It is you who do as emphasis', () => {
		const company = MANIFESTO_BODY.find(
			(block) => block.kind === 'prose' && block.text.includes('Your company will adapt')
		);
		expect(company?.kind === 'prose' ? manifestoInline(company.text) : []).toEqual([
			{
				strong: false,
				text: 'Your company will adapt, automate, reorganise and move on; it does not have the responsibility of keeping you relevant. '
			},
			{ strong: true, text: 'It is you who do.' }
		]);
	});

	it('keeps the timing stanza as verse, not five loose paragraphs', () => {
		const timing = MANIFESTO_EXCERPT.find((block) => block.kind === 'verse');
		expect(timing?.kind === 'verse' && timing.lines).toEqual([
			'Everything at work depends on timing.',
			'The insight given at the right time.',
			'The subsequent one which was not overlooked.',
			'A preparation that no one else had thought to carry out.',
			'The work was finished while everyone else was still discussing it.'
		]);
	});
});
