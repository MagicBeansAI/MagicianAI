import type { StructuredResponseBlockV1, StructuredResponseV1 } from '$lib/magician/structuredResponse/types';

export type AnswerVerification = { details: string | null };
export const verificationExplanation = 'Some parts of this answer could not be confirmed against the available sources.';

// Recognise only the generated leading caveat. Never scrub figures, IDs,
// quotations, code or uncertainty elsewhere in the answer.
export function splitAnswerVerification(text: string): { body: string; verification: AnswerVerification | null } {
	// Only the producer's marked HTML disclosure, in a document. Fenced
	// examples and quotations stay untouched; reports unwrap their own fence.
	if (/^\s*<(?:!doctype\s+html|html\b|h[1-6]\b|p\b|table\b|div\b|section\b|article\b|ul\b|ol\b|aside\b)/i.test(text)) {
		const marker = '<aside data-magician-caveat="unverified"';
		const start = text.indexOf(marker);
		const close = start >= 0 ? text.indexOf('</aside>', start) : -1;
		const openEnd = start >= 0 ? text.indexOf('>', start) : -1;
		if (start >= 0 && openEnd >= 0 && close > openEnd) {
			const readable = text.slice(openEnd + 1, close).replace(/<[^>]*>/g, '')
				.replaceAll('&lt;', '<').replaceAll('&gt;', '>').replaceAll('&quot;', '"').replaceAll('&#39;', "'").replaceAll('&amp;', '&');
			const disclosure = splitAnswerVerification(readable);
			return { body: text.slice(0, start) + text.slice(close + '</aside>'.length), verification: disclosure.verification ?? { details: null } };
		}
	}
	const prefix = /^\s*(?:>\s*)?⚠\uFE0F?\s*(?:\*\*)?Not fully verified\.(?:\*\*)?[ \t]*/u.exec(text);
	if (!prefix) return { body: text, verification: null };
	const rest = text.slice(prefix[0].length);
	const diagnostic = /^(?:Magician|Magican)['’]s evidence check could not confirm part of this answer:[ \t]*/.exec(rest);
	if (diagnostic) {
		const end = /\r?\n[ \t]*\r?\n/.exec(rest);
		const paragraph = end ? rest.slice(0, end.index) : rest;
		return {
			body: end ? rest.slice(end.index + end[0].length).trimStart() : '',
			verification: { details: paragraph.slice(diagnostic[0].length).replace(/\\([\\`*{}\[\]()#+.!_>~-])/g, '$1').trim() || null }
		};
	}
	// Task summaries retain a short warning label without the critic's reason.
	return { body: rest.trimStart(), verification: { details: null } };
}

// Keep tables, sources, actions and the stored response intact. Only generated
// caveat text moves to the message's disclosure control.
export function withoutAnswerVerification(response: StructuredResponseV1): StructuredResponseV1 {
	return {
		...response,
		plain_text: splitAnswerVerification(response.plain_text).body,
		summary: response.summary ? splitAnswerVerification(response.summary).body || undefined : response.summary,
		blocks: response.blocks.flatMap<StructuredResponseBlockV1>(block => {
			if (!('text' in block)) return [block];
			const { body, verification } = splitAnswerVerification(block.text);
			return verification ? body ? [{ ...block, text: body }] : [] : [block];
		})
	};
}
