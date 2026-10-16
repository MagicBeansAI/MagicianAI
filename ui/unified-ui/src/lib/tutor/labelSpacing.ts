/**
 * Places overlay labels so they stay readable without leaving their anchors.
 *
 * The draw overlay used to nudge a label only to the right, and only when it
 * shared a row. A column of formulas then stayed piled on one baseline, and a
 * side label walked away from the segment it named. This pass hides an earlier
 * copy of the same string when the two already sit on top of each other, then
 * separates whatever remains by the smaller overlap — sideways for a row,
 * vertically for a column — and stops once a label has moved a few lines.
 */

export interface OverlayLabelBox {
	index: number;
	/** Ink left edge, before nudging. */
	left: number;
	/** Baseline the renderer adds the offset to. */
	baseline: number;
	width: number;
	height: number;
	/** Ink above the baseline. */
	ascent: number;
	text: string;
}

export interface OverlayLabelPlacement {
	dx: number;
	dy: number;
	hidden: boolean;
}

export interface OverlayLabelBounds {
	width: number;
	height: number;
	padding: number;
}

export interface OverlayLabelObstacle {
	left: number;
	top: number;
	right: number;
	bottom: number;
}

interface InkBox {
	left: number;
	top: number;
	right: number;
	bottom: number;
}

interface Placed extends OverlayLabelBox {
	dx: number;
	dy: number;
	hidden: boolean;
}

function normalizeLabelText(text: string) {
	return text.trim().replace(/\s+/g, ' ').toLowerCase();
}

function ink(item: Placed): InkBox {
	const left = item.left + item.dx;
	const top = item.baseline + item.dy - item.ascent;
	return {
		left,
		top,
		right: left + Math.max(0, item.width),
		bottom: top + Math.max(0, item.height)
	};
}

function overlaps(a: InkBox, b: InkBox, gap: number) {
	return (
		a.left < b.right + gap &&
		a.right + gap > b.left &&
		a.top < b.bottom + gap &&
		a.bottom + gap > b.top
	);
}

function overlapAmount(a: InkBox, b: InkBox, gap: number) {
	return {
		x: Math.min(a.right, b.right) - Math.max(a.left, b.left) + gap,
		y: Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top) + gap
	};
}

export function placeOverlayLabels(
	labels: OverlayLabelBox[],
	bounds: OverlayLabelBounds,
	gap: number,
	obstacles: OverlayLabelObstacle[] = []
): Map<number, OverlayLabelPlacement> {
	const items: Placed[] = labels.map((label) => ({ ...label, dx: 0, dy: 0, hidden: false }));
	const result = new Map<number, OverlayLabelPlacement>();
	if (items.length === 0) return result;

	for (let i = 0; i < items.length; i++) {
		const key = normalizeLabelText(items[i].text);
		if (!key) continue;
		for (let j = 0; j < i; j++) {
			if (items[j].hidden) continue;
			if (normalizeLabelText(items[j].text) !== key) continue;
			if (overlaps(ink(items[j]), ink(items[i]), gap)) items[j].hidden = true;
		}
	}

	const visible = items.filter((item) => !item.hidden);
	const tallest = visible.reduce((max, item) => Math.max(max, item.height), 0);
	const maxShift = Math.max(48, tallest * 6);
	const clamp = (value: number) => Math.max(-maxShift, Math.min(maxShift, value));

	for (let iter = 0; iter < 32; iter++) {
		let moved = false;
		for (let i = 0; i < visible.length; i++) {
			for (let j = i + 1; j < visible.length; j++) {
				const a = ink(visible[i]);
				const b = ink(visible[j]);
				if (!overlaps(a, b, gap)) continue;
				const amount = overlapAmount(a, b, gap);
				const later = visible[j];
				const earlier = visible[i];
				if (amount.x <= amount.y) {
					const dir = b.left + b.right >= a.left + a.right ? 1 : -1;
					const shift = dir * (amount.x + 0.5);
					const next = clamp(later.dx + shift);
					if (next !== later.dx) {
						later.dx = next;
						moved = true;
					} else {
						const back = clamp(earlier.dx - shift);
						if (back !== earlier.dx) {
							earlier.dx = back;
							moved = true;
						}
					}
				} else {
					const dir = b.top + b.bottom >= a.top + a.bottom ? 1 : -1;
					const shift = dir * (amount.y + 0.5);
					const next = clamp(later.dy + shift);
					if (next !== later.dy) {
						later.dy = next;
						moved = true;
					} else {
						const back = clamp(earlier.dy - shift);
						if (back !== earlier.dy) {
							earlier.dy = back;
							moved = true;
						}
					}
				}
			}
			for (const obstacle of obstacles) {
				const box = ink(visible[i]);
				if (!overlaps(box, obstacle, gap)) continue;
				const pushLeft = box.right + gap - obstacle.left + 0.5;
				const next = clamp(visible[i].dx - pushLeft);
				if (next !== visible[i].dx) {
					visible[i].dx = next;
					moved = true;
					continue;
				}
				const pushDown = obstacle.bottom + gap - box.top + 0.5;
				const dropped = clamp(visible[i].dy + pushDown);
				if (dropped !== visible[i].dy) {
					visible[i].dy = dropped;
					moved = true;
				}
			}
		}
		if (!moved) break;
	}

	const minLeft = bounds.padding;
	const maxRight = Math.max(minLeft, bounds.width - bounds.padding);
	const minTop = bounds.padding;
	const maxBottom = Math.max(minTop, bounds.height - bounds.padding);
	for (const item of visible) {
		const box = ink(item);
		if (box.left < minLeft) item.dx += minLeft - box.left;
		if (ink(item).right > maxRight) item.dx -= ink(item).right - maxRight;
		if (ink(item).top < minTop) item.dy += minTop - ink(item).top;
		if (ink(item).bottom > maxBottom) item.dy -= ink(item).bottom - maxBottom;
	}

	for (const item of items) {
		if (!item.hidden && item.dx === 0 && item.dy === 0) continue;
		result.set(item.index, { dx: item.dx, dy: item.dy, hidden: item.hidden });
	}
	return result;
}
