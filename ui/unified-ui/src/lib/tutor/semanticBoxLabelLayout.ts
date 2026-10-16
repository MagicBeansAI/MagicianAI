export interface SemanticBoxShape {
	type: string;
	x?: number;
	y?: number;
	w?: number;
	h?: number;
}

export interface SemanticBoxLabelLayout {
	x: number;
	centerY: number;
	lineHeight: number;
	lines: string[];
}

const SEMANTIC_BOX_TYPES = new Set([
	'free_body_body',
	'stack_frame',
	'heap_object',
	'state_box',
	'flow_node',
	'memory_cell'
]);

const LABEL_FONT_SIZE = 16;
const LABEL_LINE_HEIGHT = 22;
const HORIZONTAL_PADDING = 20;

/**
 * Measures a label so it can be wrapped to the box that holds it.
 *
 * The caller passes the real font metrics; the fallback is the old
 * `fontSize * 0.6` per character. A character count decides the wrap POINT
 * here, so an underestimate does not merely mis-size a background — it packs
 * too many words onto a line and the text runs out of the box.
 */
export type MeasureLabelFn = (text: string, fontSize: number) => number;

const estimateLabelWidth: MeasureLabelFn = (text, fontSize) =>
	Array.from(text).length * fontSize * 0.6;

export function isSemanticBoxType(type: string) {
	return SEMANTIC_BOX_TYPES.has(type);
}

export function semanticBoxLabelLayout(
	shape: SemanticBoxShape,
	text: string,
	measure: MeasureLabelFn = estimateLabelWidth
): SemanticBoxLabelLayout | null {
	const label = text.trim();
	if (!label || !isSemanticBoxType(shape.type)) return null;

	const x = finiteNumber(shape.x);
	const y = finiteNumber(shape.y);
	const width = positiveNumber(shape.w);
	const height = positiveNumber(shape.h);
	if (x === null || y === null || width === null || height === null) return null;

	const usableWidth = Math.max(LABEL_FONT_SIZE * 2, width - HORIZONTAL_PADDING * 2);

	return {
		x: x + width / 2,
		centerY: y + height / 2,
		lineHeight: LABEL_LINE_HEIGHT,
		lines: wrapLabel(label, usableWidth, measure)
	};
}

/** Greedy wrap against MEASURED width rather than a character count. */
function wrapLabel(text: string, usableWidth: number, measure: MeasureLabelFn) {
	const words = text.split(/\s+/).filter(Boolean);
	if (words.length === 0) return [];

	const lines: string[] = [];
	let current = '';
	for (const word of words) {
		if (!current) {
			current = word;
			continue;
		}

		const candidate = `${current} ${word}`;
		if (measure(candidate, LABEL_FONT_SIZE) <= usableWidth) {
			current = candidate;
		} else {
			lines.push(current);
			current = word;
		}
	}
	if (current) lines.push(current);
	return lines;
}

function finiteNumber(value: number | undefined) {
	return Number.isFinite(value) ? Number(value) : null;
}

function positiveNumber(value: number | undefined) {
	const number = finiteNumber(value);
	return number !== null && number > 0 ? number : null;
}
