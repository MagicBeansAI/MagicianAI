import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

import { describe, expect, it } from 'vitest';

import {
	estimateTextMetrics,
	geometryFor,
	renderRecipe,
	LABEL_FONT_SIZE,
	type MeasureTextFn,
	type RecipeShape
} from './recipeInterpreter';
import { decodeRecipe } from './recipeTypes';
import type { TutorRecipe } from './recipeTypes';

const HERE = dirname(fileURLToPath(import.meta.url));
// ui/unified-ui/src/lib/tutor -> repo docs/components/magician/tutor-primitive-fixtures
const FIXTURE_DIR = join(
	HERE,
	'..',
	'..',
	'..',
	'..',
	'..',
	'docs',
	'components',
	'magician',
	'tutor-primitive-fixtures'
);

function recipe(json: object): TutorRecipe {
	const decoded = decodeRecipe(json);
	if (!decoded) throw new Error('recipe failed to decode');
	return decoded;
}

function closeTo(actual: number, expected: number, accuracy = 1e-6) {
	expect(Math.abs(actual - expected)).toBeLessThanOrEqual(accuracy);
}

function assertPoints(actual: { x: number; y: number }[], expected: [number, number][], accuracy = 1e-6) {
	expect(actual.length).toBe(expected.length);
	for (let i = 0; i < expected.length; i += 1) {
		closeTo(actual[i].x, expected[i][0], accuracy);
		closeTo(actual[i].y, expected[i][1], accuracy);
	}
}

// ---------------------------------------------------------------------------
// Inline parity cases (mirror the Swift RecipeInterpreterTests)
// ---------------------------------------------------------------------------

describe('recipeInterpreter — right_angle_marker', () => {
	it('matches the native L geometry', () => {
		const r = recipe({
			type: 'right_angle_marker',
			defaults: { size: 28 },
			draw: [
				{
					op: 'polyline',
					points: [
						['(x|cx)+size', 'y|cy'],
						['(x|cx)+size', '(y|cy)+size'],
						['x|cx', '(y|cy)+size']
					]
				}
			]
		});
		const shape: RecipeShape = { type: 'right_angle_marker', x: 100, y: 200 };
		const geo = geometryFor(r, shape);
		expect(geo).toHaveLength(1);
		expect(geo[0].op).toBe('polyline');
		assertPoints(geo[0].points, [
			[128, 200],
			[128, 228],
			[100, 228]
		]);
	});
});

describe('recipeInterpreter — angle_marker', () => {
	const arcRecipe = recipe({
		type: 'angle_marker',
		aliases: ['arc'],
		defaults: { size: 36, start_angle: 0, end_angle: 90 },
		draw: [{ op: 'arc', cx: 'cx', cy: 'cy', r: 'r|size', from: 'start_angle', to: 'end_angle' }]
	});

	it('samples 24 segments matching the native arc', () => {
		const geo = geometryFor(arcRecipe, { type: 'angle_marker', cx: 50, cy: 60 });
		expect(geo).toHaveLength(1);
		expect(geo[0].op).toBe('arc');
		const cx = 50,
			cy = 60,
			rr = 36,
			a1 = 0,
			a2 = Math.PI / 2;
		const expected: [number, number][] = [];
		for (let i = 0; i <= 24; i += 1) {
			const t = a1 + ((a2 - a1) * i) / 24;
			expected.push([cx + Math.cos(t) * rr, cy + Math.sin(t) * rr]);
		}
		assertPoints(geo[0].points, expected);
	});

	it('uses radius over size when both present', () => {
		const geo = geometryFor(arcRecipe, { type: 'angle_marker', cx: 0, cy: 0, r: 10 });
		closeTo(geo[0].points[0].x, 10); // angle 0 -> (r,0)
		closeTo(geo[0].points[0].y, 0);
		closeTo(geo[0].points[geo[0].points.length - 1].x, 0); // angle 90 -> (0,r)
		closeTo(geo[0].points[geo[0].points.length - 1].y, 10);
	});
});

describe('recipeInterpreter — square_on_segment', () => {
	const squareRecipe = recipe({
		type: 'square_on_segment',
		draw: [
			{
				op: 'polygon',
				points: [
					['sx', 'sy'],
					['ex', 'ey'],
					['ex-(ey-sy)*side_sign', 'ey+(ex-sx)*side_sign'],
					['sx-(ey-sy)*side_sign', 'sy+(ex-sx)*side_sign']
				]
			}
		]
	});

	it('left side (default sign +1)', () => {
		const geo = geometryFor(squareRecipe, { type: 'square_on_segment', x1: 0, y1: 0, x2: 10, y2: 0 });
		expect(geo).toHaveLength(1);
		expect(geo[0].op).toBe('polygon');
		expect(geo[0].closed).toBe(true);
		assertPoints(geo[0].points, [
			[0, 0],
			[10, 0],
			[10, 10],
			[0, 10]
		]);
	});

	it('right side flips the sign', () => {
		const geo = geometryFor(squareRecipe, {
			type: 'square_on_segment',
			x1: 0,
			y1: 0,
			x2: 10,
			y2: 0,
			side: 'right'
		});
		assertPoints(geo[0].points, [
			[0, 0],
			[10, 0],
			[10, -10],
			[0, -10]
		]);
	});
});

describe('recipeInterpreter — arrow', () => {
	it('emits line + arrowhead geometry, omits the label', () => {
		const r = recipe({
			type: 'arrow',
			draw: [
				{ op: 'line', from: ['sx', 'sy'], to: ['ex', 'ey'] },
				{ op: 'arrowhead', from: ['sx', 'sy'], to: ['ex', 'ey'] },
				{ op: 'label', at: ['mx', 'my-14'], text: 'text', anchor: 'center', threshold: 0.75 }
			]
		});
		const geo = geometryFor(r, { type: 'arrow', from_x: 20, from_y: 40, to_x: 120, to_y: 90, text: 'F' });
		expect(geo.map((g) => g.op)).toEqual(['line', 'arrowhead']);
		assertPoints(geo[0].points, [
			[20, 40],
			[120, 90]
		]);
		assertPoints(geo[1].points, [
			[20, 40],
			[120, 90]
		]);
	});
});

describe('recipeInterpreter — rect + callout', () => {
	it('rect uses two-corner geometry and w|width coalesce', () => {
		const r = recipe({
			type: 'rect',
			draw: [{ op: 'rect', x: 'x', y: 'y', w: 'w|width', h: 'h|height', radius: 6 }]
		});
		const geo = geometryFor(r, { type: 'rect', x: 40, y: 60, width: 200, height: 120 });
		expect(geo[0].op).toBe('rect');
		assertPoints(geo[0].points, [
			[40, 60],
			[240, 180]
		]);
	});

	it('callout background width uses derived text_len', () => {
		const r = recipe({
			type: 'callout',
			draw: [
				{ op: 'rect', x: 'x-8', y: 'y-18', w: 'min(260,text_len*9+16)', h: 36, radius: 9, fill: true },
				{ op: 'label', at: ['x', 'y'], text: 'text', anchor: 'leading' }
			]
		});
		// 4-char text -> width min(260, 4*9+16) = 52; corners (x-8,y-18) -> (+52,+36).
		const geo = geometryFor(r, { type: 'callout', x: 100, y: 50, text: 'Next' });
		expect(geo).toHaveLength(1);
		expect(geo[0].op).toBe('rect');
		assertPoints(geo[0].points, [
			[92, 32],
			[144, 68]
		]);
	});
});

describe('recipeInterpreter — op skipped when a required coord is missing', () => {
	it('produces nothing when endpoints are absent', () => {
		const r = recipe({ type: 'line', draw: [{ op: 'line', from: ['sx', 'sy'], to: ['ex', 'ey'] }] });
		expect(geometryFor(r, { type: 'line' })).toHaveLength(0);
	});
});

// ---------------------------------------------------------------------------
// render — styling + progress gating
// ---------------------------------------------------------------------------

describe('recipeInterpreter — render styling + progress gating', () => {
	const styling = { mapColor: (raw: string) => raw };

	it('arrowhead only appears past progress 0.85', () => {
		const r = recipe({
			type: 'arrow',
			draw: [
				{ op: 'line', from: ['sx', 'sy'], to: ['ex', 'ey'] },
				{ op: 'arrowhead', from: ['sx', 'sy'], to: ['ex', 'ey'] }
			]
		});
		const shape: RecipeShape = { type: 'arrow', from_x: 0, from_y: 0, to_x: 10, to_y: 0 };
		const early = renderRecipe({ recipe: r, shape, project: (p) => p, progress: 0.5, styling });
		expect(early.map((c) => c.op)).toEqual(['line']);
		const late = renderRecipe({ recipe: r, shape, project: (p) => p, progress: 0.9, styling });
		expect(late.map((c) => c.op)).toEqual(['line', 'arrowhead']);
	});

	it('labels appear past their threshold and inherit the shape color', () => {
		const r = recipe({
			type: 'label',
			draw: [{ op: 'label', at: ['x', 'y'], text: 'text', anchor: 'leading' }]
		});
		const shape: RecipeShape = { type: 'label', x: 10, y: 20, text: 'Hi', color: 'red' };
		expect(renderRecipe({ recipe: r, shape, project: (p) => p, progress: 0.1, styling })).toHaveLength(0);
		const shown = renderRecipe({ recipe: r, shape, project: (p) => p, progress: 0.5, styling });
		expect(shown).toHaveLength(1);
		const cmd = shown[0];
		expect(cmd.kind).toBe('label');
		if (cmd.kind === 'label') {
			expect(cmd.text).toBe('Hi');
			expect(cmd.anchor).toBe('leading');
			expect(cmd.color).toBe('red');
			expect(cmd.at).toEqual({ x: 10, y: 20 });
		}
	});

	it('a fillable op fills when the shape carries a fill', () => {
		const r = recipe({
			type: 'highlight',
			draw: [{ op: 'rect', x: 'x', y: 'y', w: 'w', h: 'h', fill: true }]
		});
		const cmds = renderRecipe({
			recipe: r,
			shape: { type: 'highlight', x: 0, y: 0, w: 10, h: 10, fill: '#123456' },
			project: (p) => p,
			progress: 1,
			styling
		});
		const cmd = cmds[0];
		expect(cmd.kind).toBe('rect');
		if (cmd.kind === 'rect') expect(cmd.style.fill).toBe(true);
	});

	it('projects points through the supplied project function', () => {
		const r = recipe({ type: 'line', draw: [{ op: 'line', from: ['sx', 'sy'], to: ['ex', 'ey'] }] });
		const cmds = renderRecipe({
			recipe: r,
			shape: { type: 'line', x1: 1, y1: 2, x2: 3, y2: 4 },
			project: (p) => ({ x: p.x * 2, y: p.y * 2 }),
			progress: 1,
			styling
		});
		expect(cmds[0].kind).toBe('path');
		if (cmds[0].kind === 'path') {
			expect(cmds[0].points).toEqual([
				{ x: 2, y: 4 },
				{ x: 6, y: 8 }
			]);
		}
	});
});

// ---------------------------------------------------------------------------
// SHARED GOLDEN FIXTURES — cross-platform parity with the Swift suite
// ---------------------------------------------------------------------------

interface Fixture {
	name?: string;
	recipe: object;
	shape: RecipeShape;
	space?: { width: number; height: number };
	expected: Array<{ op: string; points: [number, number][]; closed?: boolean }>;
}

describe('recipeInterpreter — shared golden fixtures', () => {
	const files = readdirSync(FIXTURE_DIR).filter((f) => f.endsWith('.json'));

	it('fixture directory is non-empty', () => {
		expect(files.length).toBeGreaterThan(0);
	});

	for (const file of files) {
		it(`matches fixture ${file}`, () => {
			const fixture = JSON.parse(readFileSync(join(FIXTURE_DIR, file), 'utf8')) as Fixture;
			const r = recipe(fixture.recipe);
			const geo = geometryFor(r, fixture.shape);
			expect(geo.map((g) => g.op)).toEqual(fixture.expected.map((e) => e.op));
			for (let i = 0; i < fixture.expected.length; i += 1) {
				assertPoints(geo[i].points, fixture.expected[i].points);
				if (fixture.expected[i].closed !== undefined) {
					expect(geo[i].closed).toBe(fixture.expected[i].closed);
				}
			}
		});
	}
});

// ui/unified-ui/src/lib/tutor -> repo magician_data_v3/system/tutor_primitives
const PRIMITIVE_DIR = join(
	HERE,
	'..',
	'..',
	'..',
	'..',
	'..',
	'magician_data_v3',
	'system',
	'tutor_primitives'
);

/** The recipe as SHIPPED, so these pin the real file and not a copy of it. */
function shippedRecipe(name: string): TutorRecipe {
	return recipe(JSON.parse(readFileSync(join(PRIMITIVE_DIR, `${name}.json`), 'utf8')));
}

describe('callout sizes its background from measured text', () => {
	const styling = { mapColor: (raw: string) => raw };
	// A deliberately WIDE font: 20px per character at size 15. A box sized from
	// a character count cannot tell this apart from a narrow one, which is the
	// whole defect.
	const wideFont: MeasureTextFn = (text, fontSize) => ({
		width: Array.from(text).length * 20,
		height: fontSize,
		ascent: fontSize * 0.8
	});

	function calloutRect(text: string, measureText?: MeasureTextFn) {
		const shape: RecipeShape = { type: 'callout', x: 100, y: 200, text };
		const commands = renderRecipe({
			recipe: shippedRecipe('callout'),
			shape,
			project: (p) => p,
			progress: 1,
			styling,
			measureText
		});
		const rect = commands.find((c) => c.kind === 'rect');
		if (rect?.kind !== 'rect') throw new Error('callout drew no background rect');
		return rect;
	}

	it('a long label gets a box wide enough to hold it', () => {
		// Under the old recipe this was `min(260, text_len*9+16)` — the cap made
		// every label past ~27 characters overflow its own background, and the
		// label op does not wrap, so the text simply ran out of the bubble.
		const text = 'A callout long enough that a 260px cap would clip it outright';
		const rect = calloutRect(text, estimateTextMetrics);
		const textWidth = estimateTextMetrics(text, LABEL_FONT_SIZE).width;

		expect(rect.width).toBeGreaterThan(260);
		closeTo(rect.width, textWidth + 16);
	});

	it('the box follows the measurer, not the character count', () => {
		const text = 'Click here';
		const narrow = calloutRect(text, estimateTextMetrics);
		const wide = calloutRect(text, wideFont);

		expect(wide.width).toBeGreaterThan(narrow.width);
		closeTo(wide.width, Array.from(text).length * 20 + 16);
	});

	it('the box sits above the anchor by the ink that rises above it', () => {
		// The label renders as SVG `<text>` with no `dominant-baseline`, so its
		// `y` is the BASELINE. A box centred on `y` would clip the ascenders.
		const rect = calloutRect('Hg', estimateTextMetrics);
		const metrics = estimateTextMetrics('Hg', LABEL_FONT_SIZE);

		closeTo(rect.y, 200 - metrics.ascent - 6);
		closeTo(rect.height, metrics.height + 12);
		expect(rect.y + rect.height).toBeGreaterThan(200);
	});

	it('without a measurer the horizontal geometry is exactly what it was', () => {
		// The fallback reproduces `text_len * 9` at the 15px label, so an
		// unmeasured caller (SSR, a test that does not care) keeps the old box
		// rather than silently getting a different one.
		const text = 'Short label';
		const rect = calloutRect(text);
		closeTo(rect.width, Array.from(text).length * 9 + 16);
	});
});

describe('parametric solids derive their own proportions', () => {
	const styling = { mapColor: (raw: string) => raw };

	function points(type: string, shape: RecipeShape) {
		return renderRecipe({
			recipe: shippedRecipe(type),
			shape,
			project: (p) => p,
			progress: 1,
			styling
		});
	}

	it('a cone base spans the radius it was given', () => {
		// The defect this exists for: a hand-assembled cone let the model pick
		// the base width and the slant independently, so the base came out too
		// small for the cone it belonged to. Here the model supplies r and h and
		// the recipe derives every point, so the two cannot disagree.
		const cmds = points('cone', { type: 'cone', cx: 200, cy: 200, r: 60, h: 140 });
		const arc = cmds.find((c) => c.op === 'arc');
		if (arc?.kind !== 'path') throw new Error('cone drew no base arc');

		const xs = arc.points.map((p) => p.x);
		const ys = arc.points.map((p) => p.y);
		closeTo(Math.min(...xs), 140, 1e-6); // cx - r
		closeTo(Math.max(...xs), 260, 1e-6); // cx + r
		// Squashed into an ellipse, because a cone base is a circle seen at an
		// angle — but only vertically. The horizontal span IS the radius.
		closeTo(Math.min(...ys), 270 - 18, 1e-6);
		closeTo(Math.max(...ys), 270 + 18, 1e-6);
	});

	it('both cone slants meet at one apex', () => {
		const cmds = points('cone', { type: 'cone', cx: 200, cy: 200, r: 60, h: 140 });
		const lines = cmds.filter((c) => c.op === 'line');
		expect(lines).toHaveLength(2);
		for (const line of lines) {
			if (line.kind !== 'path') throw new Error('slant is not a path');
			const apex = line.points[line.points.length - 1];
			closeTo(apex.x, 200, 1e-6);
			closeTo(apex.y, 130, 1e-6); // cy - h/2
		}
	});

	it('a sector unrolls to exactly the circumference it came from', () => {
		// THE relation the cone lesson turns on: the unrolled surface is a
		// sector whose arc equals the base circumference, theta*l == 2*pi*r.
		// Deriving the angle from `size` (base radius) and `r` (slant) makes
		// that true by construction rather than by asking the model nicely.
		const slant = Math.hypot(60, 140);
		const cmds = points('sector', { type: 'sector', cx: 400, cy: 200, r: slant, size: 60 });
		const arc = cmds.find((c) => c.op === 'arc');
		if (arc?.kind !== 'path') throw new Error('sector drew no arc');

		let arcLength = 0;
		for (let i = 1; i < arc.points.length; i += 1) {
			arcLength += Math.hypot(
				arc.points[i].x - arc.points[i - 1].x,
				arc.points[i].y - arc.points[i - 1].y
			);
		}
		// Sampled as a polyline, so it reads slightly under the true arc.
		expect(arcLength).toBeGreaterThan(2 * Math.PI * 60 * 0.999);
		expect(arcLength).toBeLessThanOrEqual(2 * Math.PI * 60);
	});

	it('an explicit end_angle still overrides the derived one', () => {
		const cmds = points('sector', {
			type: 'sector',
			cx: 400,
			cy: 200,
			r: 100,
			size: 60,
			end_angle: 90
		});
		const arc = cmds.find((c) => c.op === 'arc');
		if (arc?.kind !== 'path') throw new Error('sector drew no arc');
		const last = arc.points[arc.points.length - 1];
		closeTo(last.x, 400, 1e-6); // cos(90deg) == 0
		closeTo(last.y, 300, 1e-6); // cy + sin(90deg)*100
	});

	it('an arc without rx/ry is still exactly circular', () => {
		// rx/ry fall back to r, so every recipe written before ellipses existed
		// keeps its geometry.
		const r = recipe({
			type: 'ring',
			draw: [{ op: 'arc', cx: 'cx', cy: 'cy', r: 'r', from: 0, to: 360 }]
		});
		const cmds = renderRecipe({
			recipe: r,
			shape: { type: 'ring', cx: 0, cy: 0, r: 50 },
			project: (p) => p,
			progress: 1,
			styling
		});
		const arc = cmds[0];
		if (arc?.kind !== 'path') throw new Error('no arc');
		for (const p of arc.points) closeTo(Math.hypot(p.x, p.y), 50, 1e-9);
	});
});

describe('an arc shape may ask for an ellipse', () => {
	const styling = { mapColor: (raw: string) => raw };

	/** The exact shape the model emitted for a cone's base. */
	const CONE_BASE: RecipeShape = {
		type: 'arc',
		cx: 330,
		cy: 430,
		rx: 150,
		ry: 38,
		start_angle: 0,
		end_angle: 360
	};

	function arcOf(shape: RecipeShape) {
		const cmds = renderRecipe({
			recipe: shippedRecipe('angle_marker'),
			shape,
			project: (p) => p,
			progress: 1,
			styling
		});
		const arc = cmds.find((c) => c.op === 'arc');
		if (arc?.kind !== 'path') throw new Error('no arc drawn');
		return arc;
	}

	it('honours rx/ry instead of falling back to the default size', () => {
		// The defect: `rx`/`ry` were addressable by NO layer, and the recipe read
		// only `r|size`. A request for 150x38 silently became a circle of radius
		// 36 — the `angle_marker` default — which is why a cone's base kept
		// coming out far too small for the cone above it.
		const arc = arcOf(CONE_BASE);
		const xs = arc.points.map((p) => p.x);
		const ys = arc.points.map((p) => p.y);

		closeTo(Math.max(...xs) - Math.min(...xs), 300, 1e-6); // 2*rx
		closeTo(Math.max(...ys) - Math.min(...ys), 76, 1e-6); // 2*ry
		expect(Math.max(...xs) - Math.min(...xs)).not.toBeCloseTo(72, 6); // NOT 2*36
	});

	it('still draws a circle when only r is given', () => {
		const arc = arcOf({ type: 'arc', cx: 0, cy: 0, r: 50, start_angle: 0, end_angle: 360 });
		for (const p of arc.points) closeTo(Math.hypot(p.x, p.y), 50, 1e-9);
	});

	it('still falls back to size when neither r nor rx/ry is given', () => {
		const arc = arcOf({ type: 'angle_marker', cx: 0, cy: 0 });
		for (const p of arc.points) closeTo(Math.hypot(p.x, p.y), 36, 1e-9);
	});
});
