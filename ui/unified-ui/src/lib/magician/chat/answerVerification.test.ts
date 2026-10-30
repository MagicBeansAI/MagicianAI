import { describe, expect, it } from 'vitest';
import { splitAnswerVerification, withoutAnswerVerification } from './answerVerification';
import type { StructuredResponseV1 } from '$lib/magician/structuredResponse/types';

const reason = 'figure 93217, 45330 not found in any tool result';
const caveat = `> ⚠️ **Not fully verified.** Magician's evidence check could not confirm part of this answer: ${reason}`;

describe('answer verification display', () => {
    it('keeps useful partial findings and coverage notes in the answer, with diagnostics separate', () => {
        const body = 'Found community rules and account history.\n\n| Community | Subscribers |\n| --- | ---: |\n| Example | 878000 |\n\nCoverage and limitations: account eligibility is still unresolved.';
        const display = splitAnswerVerification(`${caveat}\n\n${body}`);
        expect(display.body).toBe(body);
        expect(display.body.startsWith('Found community rules')).toBe(true);
        expect(display.body).not.toContain('93217');
        expect(display.verification?.details).toBe(reason);
    });
	it.each([caveat, caveat.replace('> ', '').replaceAll('**', '')])('separates the generated caveat while preserving the answer', prefix => {
		const body = 'Revenue is 93,217.\n\n| Current | Previous |\n| --- | --- |\n| 93217 | 45330 |';
		expect(splitAnswerVerification(`${prefix}\n\n${body}`)).toEqual({ body, verification: { details: reason } });
	});
	it('recognises the short task-summary warning without removing its first sentence or figures', () => {
		expect(splitAnswerVerification('⚠️ Not fully verified. Total: 93217, previously 45330.')).toEqual({ body: 'Total: 93217, previously 45330.', verification: { details: null } });
	});
	it.each(['Not fully verified is a status.', '```\n' + caveat + '\n```', 'The source says:\n\n' + caveat, 'Figure 93217; record 45330.', '> A quoted warning.'])('leaves ordinary prose, code, quotations and references intact: %s', body => {
		expect(splitAnswerVerification(body)).toEqual({ body, verification: null });
	});
	it('handles CRLF, standalone diagnostic blocks and escaped punctuation', () => {
		expect(splitAnswerVerification(caveat + '\r\n\r\nAnswer.').body).toBe('Answer.');
		expect(splitAnswerVerification(caveat).body).toBe('');
		expect(splitAnswerVerification(caveat.replace(reason, 'figure 93\\.217 \\[old\\]')).verification?.details).toBe('figure 93.217 [old]');
	});
	it('preserves structured tables, actions and the original response', () => {
		const response: StructuredResponseV1 = {
			schema: 'magician.structured_response', version: 1, plain_text: caveat + '\n\nTotal: 93217.',
			blocks: [{ kind: 'markdown', text: caveat }, { kind: 'markdown', text: 'Total: 93217.' }],
		};
		const original = structuredClone(response);
		expect(withoutAnswerVerification(response).blocks).toEqual([{ kind: 'markdown', text: 'Total: 93217.' }]);
		expect(response).toEqual(original);
	});
});


describe('marked HTML verification', () => {
	const marked = `<aside data-magician-caveat="unverified" role="note"><p>⚠️ <strong>Not fully verified.</strong> Magician's evidence check could not confirm part of this answer: &lt;img&gt; &amp; literal &amp;gt;</p></aside>`;
	it('separates the generated element and decodes the diagnostic exactly once', () => {
		const body = '<!doctype html><html><body>' + marked + '<h1>Useful report</h1><p>Result.</p></body></html>';
		const result = splitAnswerVerification(body);
		expect(result.body).toBe('<!doctype html><html><body><h1>Useful report</h1><p>Result.</p></body></html>');
		expect(result.verification?.details).toBe('<img> & literal &gt;');
	});
	it.each(['Example source:\n'+marked,'```html\n'+marked+'\n```','<p>An ordinary note</p><aside>Not fully verified.</aside>'])('preserves unmarked notes and source examples', text => {
		expect(splitAnswerVerification(text)).toEqual({body:text,verification:null});
	});
});
