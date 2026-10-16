import { describe, expect, it } from 'vitest';
import { placeOverlayLabels, type OverlayLabelBox } from './labelSpacing';

const bounds = { width: 1000, height: 800, padding: 12 };

function label(partial: Partial<OverlayLabelBox> & Pick<OverlayLabelBox, 'index' | 'text'>): OverlayLabelBox {
	return {
		left: 100,
		baseline: 200,
		width: 80,
		height: 24,
		ascent: 18,
		...partial
	};
}

function ink(box: OverlayLabelBox, dx = 0, dy = 0) {
	const left = box.left + dx;
	const top = box.baseline + dy - box.ascent;
	return { left, top, right: left + box.width, bottom: top + box.height };
}

function overlaps(
	a: { left: number; top: number; right: number; bottom: number },
	b: { left: number; top: number; right: number; bottom: number },
	gap: number
) {
	return a.left < b.right + gap && a.right + gap > b.left && a.top < b.bottom + gap && a.bottom + gap > b.top;
}

describe('placeOverlayLabels', () => {
	it('leaves an isolated label on its anchor', () => {
		const placed = placeOverlayLabels([label({ index: 0, text: '6' })], bounds, 8);
		expect(placed.size).toBe(0);
	});

	it('keeps a column of formulas aligned and unstacked', () => {
		const labels = [
			label({ index: 0, text: '6² = 36', left: 120, baseline: 400, width: 180, height: 32, ascent: 24 }),
			label({ index: 1, text: '8² = 64', left: 122, baseline: 412, width: 180, height: 32, ascent: 24 }),
			label({ index: 2, text: 'x² = ?', left: 118, baseline: 406, width: 140, height: 32, ascent: 24 })
		];
		const placed = placeOverlayLabels(labels, bounds, 8);
		const resolved = labels.map((box) => {
			const move = placed.get(box.index);
			return ink(box, move?.dx ?? 0, move?.dy ?? 0);
		});
		for (let i = 0; i < resolved.length; i++) {
			for (let j = i + 1; j < resolved.length; j++) {
				expect(overlaps(resolved[i], resolved[j], 8)).toBe(false);
			}
		}
		for (const box of labels) {
			const move = placed.get(box.index);
			expect(Math.abs(move?.dx ?? 0)).toBeLessThan(24);
		}
	});

	it('hides an earlier copy of the same string instead of shoving it aside', () => {
		const labels = [
			label({ index: 0, text: '6² + 8² = x²', left: 200, baseline: 300, width: 220 }),
			label({ index: 1, text: '  6² + 8² = x² ', left: 206, baseline: 308, width: 220 })
		];
		const placed = placeOverlayLabels(labels, bounds, 8);
		expect(placed.get(0)?.hidden).toBe(true);
		expect(placed.get(1)?.hidden ?? false).toBe(false);
		expect(placed.get(1)?.dx ?? 0).toBe(0);
		expect(placed.get(1)?.dy ?? 0).toBe(0);
	});

	it('nudges a side-by-side pair along the shorter overlap and stays near the anchor', () => {
		const labels = [
			label({ index: 0, text: 'a = 6', left: 100, baseline: 240, width: 70, height: 22, ascent: 16 }),
			label({ index: 1, text: 'b = 8', left: 150, baseline: 242, width: 70, height: 22, ascent: 16 })
		];
		const placed = placeOverlayLabels(labels, bounds, 8);
		const second = placed.get(1);
		expect(second?.hidden ?? false).toBe(false);
		const resolved = labels.map((box) => {
			const move = placed.get(box.index);
			return ink(box, move?.dx ?? 0, move?.dy ?? 0);
		});
		expect(overlaps(resolved[0], resolved[1], 8)).toBe(false);
		expect(Math.abs(second?.dx ?? 0)).toBeLessThan(80);
		expect(Math.abs(second?.dy ?? 0)).toBeLessThan(80);
	});

	it('pushes a label out of the top-right control column', () => {
		const box = label({
			index: 0,
			text: 'note',
			left: 800,
			baseline: 70,
			width: 80,
			height: 24,
			ascent: 18
		});
		const placed = placeOverlayLabels( [box], bounds, 8, [
			{ left: 820, top: 30, right: 1000, bottom: 220 }
		]);
		const move = placed.get(0);
		const resolved = ink(box, move?.dx ?? 0, move?.dy ?? 0);
		expect(resolved.right).toBeLessThanOrEqual(820);
	});
});
