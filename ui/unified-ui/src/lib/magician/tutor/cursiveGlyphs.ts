export type CursiveStrokeSegment = {
	key: string;
	d: string;
	transform: string;
	kind: 'glyph' | 'join';
};

export type CursiveLayout = {
	width: number;
	segments: CursiveStrokeSegment[];
	unsupported: string[];
};

export const CURSIVE_ALPHABET = 'abcdefghijklmnopqrstuvwxyz'.split('');

export function layoutCursiveText(text: string): CursiveLayout {
	const width = Math.max(1, text.trim().length * 0.5);

	return {
		width,
		segments: [],
		unsupported: []
	};
}

export function cursiveLayoutViewBox(layout: CursiveLayout): string {
	return `-0.2 -1.18 ${Math.max(1.4, layout.width + 0.4)} 1.92`;
}
