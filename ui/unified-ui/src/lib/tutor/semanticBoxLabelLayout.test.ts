import { describe, expect, it } from 'vitest';

import { isSemanticBoxType, semanticBoxLabelLayout } from './semanticBoxLabelLayout';

describe('semanticBoxLabelLayout', () => {
	it('centers and preserves every label in the derivative application storyboard', () => {
		const shapes = [
			{ type: 'flow_node', x: 150, y: 260, w: 250, h: 120, label: 'motion' },
			{ type: 'flow_node', x: 520, y: 260, w: 250, h: 120, label: 'growth' },
			{ type: 'flow_node', x: 890, y: 260, w: 250, h: 120, label: 'best choice' },
			{ type: 'state_box', x: 125, y: 570, w: 300, h: 100, label: 'Physics: velocity, acceleration' },
			{ type: 'state_box', x: 495, y: 570, w: 300, h: 100, label: 'Finance & biology: trends' },
			{
				type: 'state_box',
				x: 865,
				y: 570,
				w: 300,
				h: 100,
				label: 'Engineering & economics: optimize'
			}
		];

		const layouts = shapes.map((shape) => semanticBoxLabelLayout(shape, shape.label));

		expect(layouts.every(Boolean)).toBe(true);
		expect(layouts.map((layout) => layout?.lines.join(' '))).toEqual(shapes.map((shape) => shape.label));
		expect(layouts.map((layout) => layout?.x)).toEqual([275, 645, 1015, 275, 645, 1015]);
		expect(layouts.map((layout) => layout?.centerY)).toEqual([320, 320, 320, 620, 620, 620]);
		expect(layouts[5]?.lines.length).toBeGreaterThan(1);
	});

	it('keeps generic outlines on their existing external-label path', () => {
		expect(isSemanticBoxType('rect')).toBe(false);
		expect(isSemanticBoxType('highlight')).toBe(false);
		expect(semanticBoxLabelLayout({ type: 'rect', x: 10, y: 20, w: 100, h: 40 }, 'outside')).toBeNull();
	});

	it('does not invent a label position for malformed boxes', () => {
		expect(semanticBoxLabelLayout({ type: 'flow_node', x: 10, y: 20 }, 'missing size')).toBeNull();
		expect(
			semanticBoxLabelLayout({ type: 'state_box', x: 10, y: 20, w: 100, h: 40 }, '   ')
		).toBeNull();
	});
});
