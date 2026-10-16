/**
 * Generic interpreter that renders any `TutorRecipe` against the tutor canvas,
 * mirroring `magios/Magios/RecipeInterpreter.swift` (the reference
 * implementation) EXACTLY so the same JSON recipes drive web + iOS.
 *
 * Geometry is resolved in the shape's coordinate space, then projected to
 * screen space via a supplied `project` function, then emitted as
 * framework-agnostic draw descriptors (the Svelte layer renders them as SVG).
 *
 * The `geometry(for:)` seam returns the pre-projection (space-coordinate)
 * geometry each op produces — the shared cross-platform golden fixtures assert
 * against it, guaranteeing web/iOS parity. See
 * `docs/plans/2026-07-13-data-driven-tutor-primitives.md` Appendix A (normative).
 */

import { evaluateExpression } from './recipeExpression';
import type { RecipeOp, RecipeValue, TutorRecipe } from './recipeTypes';

// Hostile-input bounds (Appendix A).
const MAX_OPS = 64;
const MAX_POINTS_PER_OP = 256;

export interface Point {
	x: number;
	y: number;
}

/**
 * The shape fields the interpreter reads. A superset-tolerant subset of the
 * backend `screen-draw` shape contract; unknown fields are ignored. Field names
 * match the JSON contract (snake_case) so `numericEnvironment` keys line up with
 * Swift `TutorShape.numericEnvironment()`.
 */
export interface RecipeShape {
	type: string;
	x?: number;
	y?: number;
	w?: number;
	h?: number;
	width?: number;
	height?: number;
	x1?: number;
	y1?: number;
	x2?: number;
	y2?: number;
	from_x?: number;
	from_y?: number;
	to_x?: number;
	to_y?: number;
	cx?: number;
	cy?: number;
	r?: number;
	/** Elliptical radii. Absent means circular — see `numericEnvironment`. */
	rx?: number;
	ry?: number;
	size?: number;
	start_angle?: number;
	end_angle?: number;
	stroke_width?: number;
	opacity?: number;
	font_size?: number;
	c1x?: number;
	c1y?: number;
	c2x?: number;
	c2y?: number;
	points?: Array<[number, number] | { x?: number; y?: number }> | string;
	d?: string;
	text?: string;
	color?: string;
	fill?: string;
	side?: string;
	orientation?: string;
	coordinate_space?: { width?: number; height?: number };
	capture_image_size?: { width?: number; height?: number };
}

/** Space-coordinate geometry an op produces (pre-projection) — the test seam. */
export interface OpGeometry {
	op: string;
	points: Point[]; // space coords
	closed: boolean;
}

// A default project function: identity (space == screen). The web overlay uses
// an SVG viewBox in space coordinates, so identity projection is correct there.
export type ProjectFn = (p: Point) => Point;
export const identityProject: ProjectFn = (p) => ({ x: p.x, y: p.y });

/** Resolved per-op styling (mirrors `RecipeInterpreter.render`). */
export interface DrawStyle {
	strokeColor: string;
	fillColor: string;
	width: number;
	dashed: boolean;
	doStroke: boolean;
	fill: boolean;
	opacityStroke: number;
	opacityFill: number;
}

/** A single framework-agnostic draw command produced by `render`. */
export type DrawCommand =
	| { kind: 'path'; op: string; points: Point[]; closed: boolean; style: DrawStyle }
	| {
			kind: 'rect';
			op: string;
			x: number;
			y: number;
			width: number;
			height: number;
			radius: number;
			style: DrawStyle;
	  }
	| { kind: 'circle'; op: string; cx: number; cy: number; r: number; style: DrawStyle }
	| { kind: 'arrowhead'; op: string; from: Point; to: Point; style: DrawStyle }
	| {
			kind: 'label';
			op: string;
			at: Point;
			text: string;
			anchor: TextAnchor;
			cursive: boolean;
			fontSize: number;
			color: string;
	  };

export type TextAnchor = 'leading' | 'center' | 'trailing';

const DEFAULT_STROKE = '#ffcc00';

/** The size non-cursive labels render at; see the `label` op. */
export const LABEL_FONT_SIZE = 15;

export interface TextMetrics {
	width: number;
	/** Glyph-box height (ascent + descent), not line height. */
	height: number;
	/**
	 * Height above the baseline. Recipe labels render as SVG `<text>` with no
	 * `dominant-baseline`, so their `y` IS the baseline — a box drawn around
	 * one has to know how much of the glyph sits above that line, or it rides
	 * low and clips the ascenders.
	 */
	ascent: number;
}

/**
 * Measures text so a recipe can size a box around glyphs instead of guessing
 * from a character count. Supplied by the layer that owns a font — the browser
 * overlay passes a canvas-backed measurer.
 */
export type MeasureTextFn = (text: string, fontSize: number) => TextMetrics;

/**
 * The fallback when no measurer is supplied (SSR, tests that do not care about
 * exact glyph widths).
 *
 * **The width deliberately reproduces the old estimate**: `callout` sized
 * itself as `text_len * 9 + 16` against the hardcoded 15px label, and
 * `15 * 0.6 === 9`. So an unmeasured caller keeps exactly the horizontal
 * geometry it had, and only a caller that CAN measure gets a box that fits.
 * Height does change — it becomes derived from the font rather than the
 * constant 36 the recipe hardcoded, which is the point.
 */
export const estimateTextMetrics: MeasureTextFn = (text, fontSize) => ({
	width: Array.from(text).length * fontSize * 0.6,
	height: fontSize,
	ascent: fontSize * 0.78
});

// ---------------------------------------------------------------------------
// Environment — field resolution + derived fields (Appendix A)
// ---------------------------------------------------------------------------

class Environment {
	private readonly base: Record<string, number>;
	private readonly derived: Record<string, number>;
	private readonly defaults: Record<string, number>;

	constructor(
		shape: RecipeShape,
		defaults: Record<string, number>,
		measureText: MeasureTextFn = estimateTextMetrics
	) {
		this.defaults = defaults;
		this.base = numericEnvironment(shape);

		const derived: Record<string, number> = {};
		const sx = firstDefined(shape.from_x, shape.x1);
		const sy = firstDefined(shape.from_y, shape.y1);
		const ex = firstDefined(shape.to_x, shape.x2);
		const ey = firstDefined(shape.to_y, shape.y2);
		if (sx !== undefined) derived.sx = sx;
		if (sy !== undefined) derived.sy = sy;
		if (ex !== undefined) derived.ex = ex;
		if (ey !== undefined) derived.ey = ey;
		// cx/cy prefer a raw cx/cy, else x/y.
		const cx = firstDefined(shape.cx, shape.x);
		const cy = firstDefined(shape.cy, shape.y);
		if (cx !== undefined) derived.cx = cx;
		if (cy !== undefined) derived.cy = cy;
		const w = firstDefined(shape.w, shape.width);
		const h = firstDefined(shape.h, shape.height);
		if (w !== undefined) derived.w = w;
		if (h !== undefined) derived.h = h;
		if (sx !== undefined && ex !== undefined) derived.mx = (sx + ex) / 2;
		if (sy !== undefined && ey !== undefined) derived.my = (sy + ey) / 2;
		const sideRight =
			shape.side === 'right' || shape.side === '-1' || shape.orientation === 'right';
		derived.side_sign = sideRight ? -1 : 1;
		const textForLen = firstDefinedString(shape.text, shape.d);
		if (textForLen !== undefined) {
			derived.text_len = Array.from(textForLen).length;
			// `text_w`/`text_h` are the measured box a recipe should wrap around
			// its text. `text_len` stays for recipes that genuinely want a count,
			// but sizing from it is what let a long label run out of its own
			// background: a character count cannot see glyph width, and the
			// label op does not wrap, so the overflow had nowhere to go.
			const fontSize = this.base.font_size ?? defaults.font_size ?? LABEL_FONT_SIZE;
			const metrics = measureText(textForLen, fontSize);
			derived.text_w = metrics.width;
			derived.text_h = metrics.height;
			// `text_rise` is how far the ink extends ABOVE the label's own
			// anchor point, so a recipe can write `y-text_rise-pad` and be
			// correct on every renderer. It is deliberately anchor-relative
			// rather than a font ascent: this renderer anchors labels on the
			// BASELINE (SVG `<text>` with no `dominant-baseline`), so the rise
			// is the ascent — but iOS draws through `GraphicsContext.draw(at:
			// anchor: .leading)`, which is vertically CENTRED, and there the
			// rise is half the glyph box. Same recipe, same variable, two
			// correct boxes.
			derived.text_rise = metrics.ascent;
		}

		this.derived = derived;
	}

	/** Resolve a bare identifier: raw field -> derived -> defaults. */
	resolve(name: string): number | undefined {
		if (name in this.base) return this.base[name];
		if (name in this.derived) return this.derived[name];
		if (name in this.defaults) return this.defaults[name];
		return undefined;
	}

	/** A numeric op-param value (number literal or an expression/field ref). */
	number(value: RecipeValue | undefined): number | undefined {
		if (value === undefined) return undefined;
		if (typeof value === 'number') return value;
		if (typeof value === 'boolean') return undefined;
		if (typeof value === 'string') {
			const v = evaluateExpression(value, (n) => this.resolve(n));
			if (v !== undefined && Number.isNaN(v)) return undefined;
			return v;
		}
		return undefined; // array
	}

	/** A `[x, y]` point param — each component resolved independently. */
	point(value: RecipeValue | undefined): Point | undefined {
		if (!Array.isArray(value) || value.length < 2) return undefined;
		const x = this.number(value[0]);
		const y = this.number(value[1]);
		if (x === undefined || y === undefined) return undefined;
		return { x, y };
	}

	/**
	 * A `points` param: either a literal `[[x,y]…]` array, or a field ref
	 * (`"points|d"`) pulling the shape's `points` array / parsing its `d` string.
	 */
	points(value: RecipeValue | undefined, shape: RecipeShape): Point[] {
		if (Array.isArray(value)) {
			const out: Point[] = [];
			for (const item of value) {
				const p = this.point(item);
				if (p !== undefined) out.push(p);
			}
			return out;
		}
		if (typeof value === 'string') {
			return recipeShapePoints(shape);
		}
		return [];
	}

	/** The raw string of a param (a field name / literal), not evaluated. */
	stringValue(value: RecipeValue | undefined): string | undefined {
		return typeof value === 'string' ? value : undefined;
	}

	/** Resolve a text param — a literal or a field-ref (`"text"`, `"text|d"`). */
	text(value: RecipeValue | undefined, shape: RecipeShape): string | undefined {
		if (typeof value !== 'string') return undefined;
		for (const token of value.split('|').map((t) => t.trim())) {
			switch (token) {
				case 'text':
					if (shape.text !== undefined) return shape.text;
					break;
				case 'd':
					if (shape.d !== undefined) return shape.d;
					break;
				default:
					return token; // literal
			}
		}
		return undefined;
	}

	bool(value: RecipeValue | undefined): boolean | undefined {
		if (typeof value === 'boolean') return value;
		if (typeof value === 'number') return value !== 0;
		if (typeof value === 'string') {
			switch (value.toLowerCase()) {
				case 'true':
					return true;
				case 'false':
					return false;
				default:
					return undefined;
			}
		}
		return undefined;
	}
}

function firstDefined(...values: Array<number | undefined>): number | undefined {
	for (const v of values) {
		if (v !== undefined && Number.isFinite(v)) return v;
	}
	return undefined;
}

function firstDefinedString(...values: Array<string | undefined>): string | undefined {
	for (const v of values) {
		if (v !== undefined) return v;
	}
	return undefined;
}

/**
 * Every present raw numeric field keyed by its JSON name — the base environment.
 * Mirrors Swift `TutorShape.numericEnvironment()`.
 */
function numericEnvironment(shape: RecipeShape): Record<string, number> {
	const env: Record<string, number> = {};
	const put = (key: string, value: number | undefined) => {
		if (value !== undefined && Number.isFinite(value)) env[key] = value;
	};
	put('x', shape.x);
	put('y', shape.y);
	put('w', shape.w);
	put('h', shape.h);
	put('width', shape.width);
	put('height', shape.height);
	put('x1', shape.x1);
	put('y1', shape.y1);
	put('x2', shape.x2);
	put('y2', shape.y2);
	put('from_x', shape.from_x);
	put('from_y', shape.from_y);
	put('to_x', shape.to_x);
	put('to_y', shape.to_y);
	put('cx', shape.cx);
	put('cy', shape.cy);
	put('r', shape.r);
	// An ellipse is a first-class request, not a circle that lost precision. A
	// shape may carry `rx`/`ry` instead of `r`; recipes coalesce (`rx|r|size`)
	// so a circular caller is unaffected. Before these were addressable the
	// interpreter could not see them at all, so an `arc` asking for 150x38 fell
	// through to the `angle_marker` default of `size: 36` and drew a small
	// circle — the shape was right and the plumbing substituted its own.
	put('rx', shape.rx);
	put('ry', shape.ry);
	put('stroke_width', shape.stroke_width);
	put('opacity', shape.opacity);
	put('size', shape.size);
	put('start_angle', shape.start_angle);
	put('end_angle', shape.end_angle);
	put('font_size', shape.font_size);
	put('c1x', shape.c1x);
	put('c1y', shape.c1y);
	put('c2x', shape.c2x);
	put('c2y', shape.c2y);
	return env;
}

/**
 * Points list from a shape (its `points` array, else parsed from `d`). Mirrors
 * the Swift `recipeShapePoints` / native `points(for:)` helper.
 */
function recipeShapePoints(shape: RecipeShape): Point[] {
	const raw = shape.points;
	if (Array.isArray(raw)) {
		const out: Point[] = [];
		for (const point of raw) {
			if (Array.isArray(point)) {
				const x = point[0];
				const y = point[1];
				if (Number.isFinite(x) && Number.isFinite(y)) out.push({ x, y });
			} else if (point && typeof point === 'object') {
				const x = point.x;
				const y = point.y;
				if (Number.isFinite(x) && Number.isFinite(y)) out.push({ x: x as number, y: y as number });
			}
		}
		return out;
	}
	if (typeof raw === 'string') {
		return parsePointTokens(raw);
	}
	if (typeof shape.d === 'string') {
		return parsePointTokens(shape.d);
	}
	return [];
}

// Extract [x,y] pairs from a `d`/points string: every run of digits/./-.
function parsePointTokens(d: string): Point[] {
	const tokens: number[] = [];
	for (const m of d.match(/[0-9.-]+/g) ?? []) {
		const n = Number(m);
		if (Number.isFinite(n)) tokens.push(n);
	}
	if (tokens.length < 2) return [];
	const out: Point[] = [];
	for (let i = 0; i + 1 < tokens.length; i += 2) {
		out.push({ x: tokens[i], y: tokens[i + 1] });
	}
	return out;
}

// ---------------------------------------------------------------------------
// Arc sampling (24 segments in space coords) — mirrors Swift arcPoints
// ---------------------------------------------------------------------------

/**
 * Arc points, circular or elliptical.
 *
 * `rx`/`ry` are optional and each falls back to `r`, so every existing arc
 * recipe keeps its exact geometry. They exist because a solid drawn in
 * projection needs an ellipse — the base of a cone or cylinder is a circle seen
 * at an angle, and approximating it with a circle is the difference between a
 * cone and a party hat.
 */
function arcPoints(op: RecipeOp, env: Environment): Point[] {
	const cx = env.number(op.params.cx);
	const cy = env.number(op.params.cy);
	const r = env.number(op.params.r);
	const rx = env.number(op.params.rx) ?? r;
	const ry = env.number(op.params.ry) ?? r;
	if (cx === undefined || cy === undefined || rx === undefined || ry === undefined) return [];
	const a1 = ((env.number(op.params.from) ?? 0) * Math.PI) / 180;
	const a2 = ((env.number(op.params.to) ?? 90) * Math.PI) / 180;
	// A full ellipse needs more than the 24 steps a 90° arc was tuned for, or
	// the seam shows as a visible polygon edge.
	const sweep = Math.abs(a2 - a1);
	const steps = Math.max(24, Math.min(96, Math.ceil((sweep / (Math.PI / 2)) * 24)));
	const pts: Point[] = [];
	for (let i = 0; i <= steps; i += 1) {
		const t = a1 + ((a2 - a1) * i) / steps;
		pts.push({ x: cx + Math.cos(t) * rx, y: cy + Math.sin(t) * ry });
	}
	return pts;
}

// ---------------------------------------------------------------------------
// geometry(for:) — the pre-projection test seam (parity fixtures)
// ---------------------------------------------------------------------------

/**
 * Space-coordinate geometry each op produces for a shape — matches Swift
 * `RecipeInterpreter.geometry(for:)`. Label/text ops carry no stroked geometry
 * and are omitted; ops skipped for a missing coord produce no entry.
 */
export function geometryFor(
	recipe: TutorRecipe,
	shape: RecipeShape,
	measureText?: MeasureTextFn
): OpGeometry[] {
	const env = new Environment(shape, recipe.defaults, measureText);
	const out: OpGeometry[] = [];
	for (const op of recipe.draw.slice(0, MAX_OPS)) {
		const name = op.op.toLowerCase();
		switch (name) {
			case 'line': {
				const from = env.point(op.params.from);
				const to = env.point(op.params.to);
				if (from && to) out.push({ op: 'line', points: [from, to], closed: false });
				break;
			}
			case 'polyline':
			case 'polygon': {
				const pts = env.points(op.params.points, shape);
				if (pts.length > 0) out.push({ op: name, points: pts, closed: name === 'polygon' });
				break;
			}
			case 'rect': {
				const x = env.number(op.params.x);
				const y = env.number(op.params.y);
				const w = env.number(op.params.w);
				const h = env.number(op.params.h);
				if (x !== undefined && y !== undefined && w !== undefined && h !== undefined) {
					out.push({
						op: 'rect',
						points: [
							{ x, y },
							{ x: x + w, y: y + h }
						],
						closed: true
					});
				}
				break;
			}
			case 'circle': {
				const cx = env.number(op.params.cx);
				const cy = env.number(op.params.cy);
				const r = env.number(op.params.r);
				if (cx !== undefined && cy !== undefined && r !== undefined) {
					out.push({
						op: 'circle',
						points: [
							{ x: cx, y: cy },
							{ x: cx + r, y: cy }
						],
						closed: true
					});
				}
				break;
			}
			case 'arc': {
				const pts = arcPoints(op, env);
				if (pts.length > 0) out.push({ op: 'arc', points: pts, closed: false });
				break;
			}
			case 'bezier': {
				const from = env.point(op.params.from);
				const to = env.point(op.params.to);
				const c1 = env.point(op.params.c1);
				if (from && to && c1) {
					const pts: Point[] = [from, c1];
					const c2 = env.point(op.params.c2);
					if (c2) pts.push(c2);
					pts.push(to);
					out.push({ op: 'bezier', points: pts, closed: false });
				}
				break;
			}
			case 'arrowhead': {
				const from = env.point(op.params.from);
				const to = env.point(op.params.to);
				if (from && to) out.push({ op: 'arrowhead', points: [from, to], closed: false });
				break;
			}
			default:
				break; // label / cursive_label carry no stroked geometry
		}
	}
	return out;
}

// ---------------------------------------------------------------------------
// render — produce projected draw commands (styling + progress gating)
// ---------------------------------------------------------------------------

export interface StylingContext {
	/** Map a color name / hex to the CSS color the overlay strokes with. */
	mapColor: (raw: string) => string;
	/** Optional alpha applied to a resolved color (returns a CSS color string). */
	applyOpacity?: (color: string, opacity: number) => string;
}

export interface RenderInput {
	recipe: TutorRecipe;
	shape: RecipeShape;
	project: ProjectFn;
	progress: number;
	styling: StylingContext;
	/**
	 * Measures label text so text-sized recipes fit their glyphs. Omit it and
	 * the interpreter falls back to the character-count estimate, which is what
	 * it always did — see `estimateTextMetrics`.
	 */
	measureText?: MeasureTextFn;
}

/**
 * Render a recipe into projected, framework-agnostic draw commands. Mirrors
 * `RecipeInterpreter.render` — resolves per-op styling, projects geometry, and
 * gates labels/arrowheads on progress thresholds. The Svelte layer converts
 * each command to an SVG element.
 */
export function renderRecipe(input: RenderInput): DrawCommand[] {
	const { recipe, shape, project, styling } = input;
	const env = new Environment(shape, recipe.defaults, input.measureText);
	const progress = Math.min(Math.max(input.progress, 0), 1);

	const shapeOpacity = numOr(shape.opacity, 1);
	const shapeStrokeColor = styling.mapColor(shape.color ?? DEFAULT_STROKE);
	const shapeWidth = Math.max(1, numOr(shape.stroke_width, 4));

	const commands: DrawCommand[] = [];
	for (const op of recipe.draw.slice(0, MAX_OPS)) {
		const command = renderOp(op, env, shape, project, progress, {
			shapeStrokeColor,
			shapeWidth,
			shapeOpacity,
			styling
		});
		for (const c of command) commands.push(c);
	}
	return commands;
}

interface RenderCtx {
	shapeStrokeColor: string;
	shapeWidth: number;
	shapeOpacity: number;
	styling: StylingContext;
}

function renderOp(
	op: RecipeOp,
	env: Environment,
	shape: RecipeShape,
	project: ProjectFn,
	progress: number,
	ctx: RenderCtx
): DrawCommand[] {
	const name = op.op.toLowerCase();

	const width = Math.max(1, env.number(op.params.width) ?? ctx.shapeWidth);
	const opColorName = env.stringValue(op.params.color);
	const opOpacity = env.number(op.params.opacity);

	const strokeColor = resolveStroke(opColorName, opOpacity, shape, ctx);
	const fillColor = resolveFill(opColorName, opOpacity, shape, ctx);
	const dashed = env.bool(op.params.dashed) ?? false;
	const doStroke = env.bool(op.params.stroke) ?? true;

	const style: DrawStyle = {
		strokeColor,
		fillColor,
		width,
		dashed,
		doStroke,
		fill: false,
		opacityStroke: opOpacity ?? numOr(shape.opacity, 1),
		opacityFill: opOpacity ?? 0.22
	};

	switch (name) {
		case 'line': {
			const from = env.point(op.params.from);
			const to = env.point(op.params.to);
			if (!from || !to) return [];
			return [{ kind: 'path', op: 'line', points: [project(from), project(to)], closed: false, style }];
		}
		case 'polyline':
		case 'polygon': {
			const isPolygon = name === 'polygon';
			const pts = env
				.points(op.params.points, shape)
				.slice(0, MAX_POINTS_PER_OP)
				.map((p) => project(p));
			if (pts.length === 0) return [];
			style.fill = shouldFill(op, env, isPolygon, shape);
			return [{ kind: 'path', op: name, points: pts, closed: isPolygon, style }];
		}
		case 'rect': {
			const x = env.number(op.params.x);
			const y = env.number(op.params.y);
			const w = env.number(op.params.w);
			const h = env.number(op.params.h);
			if (x === undefined || y === undefined || w === undefined || h === undefined) return [];
			const start = project({ x, y });
			const end = project({ x: x + w, y: y + h });
			const radius = env.number(op.params.radius) ?? 6;
			style.fill = shouldFill(op, env, true, shape);
			return [
				{
					kind: 'rect',
					op: 'rect',
					x: start.x,
					y: start.y,
					width: end.x - start.x,
					height: end.y - start.y,
					radius,
					style
				}
			];
		}
		case 'circle': {
			const cx = env.number(op.params.cx);
			const cy = env.number(op.params.cy);
			const r = env.number(op.params.r);
			if (cx === undefined || cy === undefined || r === undefined) return [];
			const center = project({ x: cx, y: cy });
			const edge = project({ x: cx + r, y: cy });
			const screenRadius = Math.abs(edge.x - center.x);
			style.fill = shouldFill(op, env, true, shape);
			return [{ kind: 'circle', op: 'circle', cx: center.x, cy: center.y, r: screenRadius, style }];
		}
		case 'arc': {
			const pts = arcPoints(op, env)
				.slice(0, MAX_POINTS_PER_OP)
				.map((p) => project(p));
			if (pts.length === 0) return [];
			return [{ kind: 'path', op: 'arc', points: pts, closed: false, style }];
		}
		case 'bezier': {
			const from = env.point(op.params.from);
			const to = env.point(op.params.to);
			const c1 = env.point(op.params.c1);
			if (!from || !to || !c1) return [];
			const pts: Point[] = [project(from), project(c1)];
			const c2 = env.point(op.params.c2);
			if (c2) pts.push(project(c2));
			pts.push(project(to));
			// Encode control flag on the path: 2 controls => cubic, 1 => quad.
			// The Svelte layer reads points as [from, c1, (c2?), to].
			return [{ kind: 'path', op: 'bezier', points: pts, closed: false, style }];
		}
		case 'arrowhead': {
			if (progress <= 0.85) return [];
			const from = env.point(op.params.from);
			const to = env.point(op.params.to);
			if (!from || !to) return [];
			return [{ kind: 'arrowhead', op: 'arrowhead', from: project(from), to: project(to), style }];
		}
		case 'label':
		case 'cursive_label': {
			const text = env.text(op.params.text, shape);
			const at = env.point(op.params.at);
			if (text === undefined || !at) return [];
			const cursive = name === 'cursive_label';
			const threshold = env.number(op.params.threshold) ?? 0.15;
			if (progress <= threshold) return [];
			const anchor = textAnchor(env.stringValue(op.params.anchor));
			const fontSize = cursive
				? Math.min(180, Math.max(34, env.number(op.params.size) ?? 92))
				: 15;
			return [
				{
					kind: 'label',
					op: name,
					at: project(at),
					text,
					anchor,
					cursive,
					fontSize,
					color: strokeColor
				}
			];
		}
		default:
			return []; // unknown op -> no-op (rest of the recipe still draws)
	}
}

/**
 * `rect`/`circle`/`polygon` fill iff `op.fill == true` OR
 * (`op.fill != false` AND the shape carries a `fill`). Non-fillable ops never fill.
 * Mirrors Swift `shouldFill`.
 */
function shouldFill(op: RecipeOp, env: Environment, isFillable: boolean, shape: RecipeShape): boolean {
	if (!isFillable) return false;
	const explicit = env.bool(op.params.fill);
	if (explicit !== undefined) return explicit;
	return shape.fill !== undefined && shape.fill !== null;
}

function resolveStroke(
	opColorName: string | undefined,
	opOpacity: number | undefined,
	shape: RecipeShape,
	ctx: RenderCtx
): string {
	if (opColorName === undefined) return ctx.shapeStrokeColor;
	const base = ctx.styling.mapColor(opColorName);
	const opacity = opOpacity ?? numOr(shape.opacity, 1);
	return applyOpacity(ctx.styling, base, opacity);
}

function resolveFill(
	opColorName: string | undefined,
	opOpacity: number | undefined,
	shape: RecipeShape,
	ctx: RenderCtx
): string {
	const base =
		opColorName !== undefined
			? ctx.styling.mapColor(opColorName)
			: ctx.styling.mapColor(shape.fill ?? shape.color ?? DEFAULT_STROKE);
	const opacity = opOpacity ?? 0.22;
	return applyOpacity(ctx.styling, base, opacity);
}

function applyOpacity(styling: StylingContext, color: string, opacity: number): string {
	if (styling.applyOpacity) return styling.applyOpacity(color, opacity);
	return color;
}

function textAnchor(raw: string | undefined): TextAnchor {
	switch ((raw ?? 'center').toLowerCase()) {
		case 'leading':
			return 'leading';
		case 'trailing':
			return 'trailing';
		default:
			return 'center';
	}
}

function numOr(value: number | undefined | null, fallback: number): number {
	return typeof value === 'number' && Number.isFinite(value) ? value : fallback;
}
