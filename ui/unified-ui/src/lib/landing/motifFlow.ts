/**
 * Shared geometry and surface grammar for the landing page's living motif.
 *
 * MovieTrack and Greeting use separate canvases because they have independent
 * scroll geometries. Their seam is nevertheless one physical handoff: the
 * movie reaches the bottom edge at the same normalized point and tangent from
 * which the greeting enters at its top edge. Keeping those values here stops
 * either section from quietly redesigning the protagonist at the boundary.
 */

export interface MotifPoint {
	x: number;
	y: number;
}

export interface ScreenTrailPoint extends MotifPoint {
	progress: number;
}

export interface MotifStrokePass {
	width: number;
	alpha: number;
	tint: number;
}

export interface MotifStrokeRecipe {
	outer: MotifStrokePass;
	glow: MotifStrokePass;
	filament: MotifStrokePass;
	core: MotifStrokePass;
}

export const MOTIF_HANDOFF = {
	/** Last in-frame control point in MovieTrack, in viewport coordinates. */
	moviePenultimate: { x: 0.85, y: 0.82 },
	/** MovieTrack's bottom-edge exit. */
	movieExit: { x: 0.5, y: 1 },
	/** Greeting's top-edge entry: the same document boundary. */
	greetingEntry: { x: 0.5, y: 0 },
	/** First Bézier control point; preserves the movie exit tangent. */
	greetingControl: { x: 0.15, y: 0.18 }
} as const;

/** A body length that remains legible without taking over the composition. */
export function motifTrailLengthPx(viewportWidth: number): number {
	return Math.min(360, Math.max(210, viewportWidth * 0.24));
}

/** Mild tail taper: most of the body remains present instead of vanishing. */
export function motifBodyAlpha(position: number): number {
	const g = Math.min(1, Math.max(0, position));
	return 0.18 + 0.82 * Math.sin((Math.PI / 2) * g) ** 0.72;
}

/**
 * One travelling wave along the path normal. Both endpoints remain anchored,
 * which is essential at the cross-canvas handoff.
 */
export function motifWaveOffset(position: number, timeSeconds: number, bloom = 1): number {
	const g = Math.min(1, Math.max(0, position));
	const envelope = Math.sin(Math.PI * g);
	const wave = Math.sin(timeSeconds * 0.95 - g * Math.PI * 2.4);
	return wave * envelope * (4.2 + 0.8 * Math.min(1, Math.max(0, bloom)));
}

/** The leading light is a detail of the ribbon, never a separate orb. */
export function motifHeadRadius(bloom = 1): number {
	return 2.5 + 1.5 * Math.min(1, Math.max(0, bloom));
}

/** Identical four-pass body treatment on both sides of the canvas seam. */
export function motifStrokeRecipe(onLight: boolean, bloom = 1): MotifStrokeRecipe {
	const b = Math.min(1, Math.max(0, bloom));
	return onLight
		? {
				outer: { width: 8 + 2 * b, alpha: 0.07 + 0.03 * b, tint: 0 },
				glow: { width: 3.8 + b, alpha: 0.16 + 0.04 * b, tint: 0.06 },
				filament: { width: 1.55 + 0.55 * b, alpha: 0.72 + 0.1 * b, tint: 0.12 },
				core: { width: 0.62 + 0.18 * b, alpha: 0.9, tint: 0.5 }
			}
		: {
				outer: { width: 9 + 2 * b, alpha: 0.09 + 0.03 * b, tint: 0 },
				glow: { width: 4.2 + b, alpha: 0.19 + 0.04 * b, tint: 0.03 },
				filament: { width: 1.65 + 0.55 * b, alpha: 0.76 + 0.1 * b, tint: 0.14 },
				core: { width: 0.68 + 0.18 * b, alpha: 0.92, tint: 0.58 }
			};
}

/**
 * Sample a moving path by screen distance and resample it uniformly. Both
 * landing canvases use this exact routine, so unequal section timelines never
 * change the protagonist's physical length or segment density at the seam.
 */
export function sampleMotifTrail(
	startProgress: number,
	headProgress: number,
	targetLengthPx: number,
	sample: (progress: number) => MotifPoint,
	candidateCount = 112,
	outputCount = 65
): ScreenTrailPoint[] {
	const lo = Math.min(startProgress, headProgress);
	const hi = Math.max(startProgress, headProgress);
	const candidates = Math.max(2, Math.floor(candidateCount));
	const outputs = Math.max(2, Math.floor(outputCount));
	const backwards: ScreenTrailPoint[] = [];
	let walked = 0;
	let previous: ScreenTrailPoint | null = null;

	for (let i = 0; i <= candidates; i++) {
		const progress = hi - (hi - lo) * (i / candidates);
		const point = sample(progress);
		const current = { x: point.x, y: point.y, progress };
		if (previous) {
			const segmentLength = Math.hypot(current.x - previous.x, current.y - previous.y);
			const remaining = Math.max(0, targetLengthPx - walked);
			if (segmentLength > 0 && remaining < segmentLength) {
				const t = remaining / segmentLength;
				backwards.push({
					x: previous.x + (current.x - previous.x) * t,
					y: previous.y + (current.y - previous.y) * t,
					progress: previous.progress + (current.progress - previous.progress) * t
				});
				break;
			}
			walked += segmentLength;
		}
		backwards.push(current);
		previous = current;
		if (walked >= Math.max(0, targetLengthPx)) break;
	}

	const arc = backwards.reverse();
	if (arc.length < 2) return arc;
	const cumulative = [0];
	for (let i = 1; i < arc.length; i++) {
		cumulative.push(
			cumulative[i - 1] + Math.hypot(arc[i].x - arc[i - 1].x, arc[i].y - arc[i - 1].y)
		);
	}
	const total = cumulative[cumulative.length - 1];
	if (total <= 1e-6) return [arc[0], arc[arc.length - 1]];

	const result: ScreenTrailPoint[] = [];
	let segment = 1;
	for (let i = 0; i < outputs; i++) {
		const distance = total * (i / (outputs - 1));
		while (segment < cumulative.length - 1 && cumulative[segment] < distance) segment += 1;
		const a = arc[segment - 1];
		const b = arc[segment];
		const span = cumulative[segment] - cumulative[segment - 1];
		const t = span > 0 ? (distance - cumulative[segment - 1]) / span : 1;
		result.push({
			x: a.x + (b.x - a.x) * t,
			y: a.y + (b.y - a.y) * t,
			progress: a.progress + (b.progress - a.progress) * t
		});
	}
	return result;
}

/**
 * Greeting's single cubic pass. P0→P1 exactly matches the normalized
 * MovieTrack penultimate→exit vector, so position and first derivative remain
 * continuous as the document boundary crosses the viewport.
 */
export function greetingMotifPoint(
	progress: number,
	width: number,
	height: number,
	sourceViewportHeight = height
): MotifPoint {
	const t = Math.min(1, Math.max(0, progress));
	const v = 1 - t;
	const p0 = MOTIF_HANDOFF.greetingEntry;
	// Normalized y must be corrected for the canvases' different heights.
	// MovieTrack is viewport-high; Greeting is compact. Without this ratio the
	// same normalized vector becomes a visibly different physical tangent.
	const p1 = {
		x: MOTIF_HANDOFF.greetingControl.x,
		y:
			MOTIF_HANDOFF.greetingEntry.y +
			(MOTIF_HANDOFF.greetingControl.y - MOTIF_HANDOFF.greetingEntry.y) *
				(sourceViewportHeight / Math.max(1, height))
	};
	const p2 = { x: 0.95, y: 0.5 };
	const p3 = { x: 0.5, y: 1.3 };
	const b0 = v * v * v;
	const b1 = 3 * v * v * t;
	const b2 = 3 * v * t * t;
	const b3 = t * t * t;
	return {
		x: (b0 * p0.x + b1 * p1.x + b2 * p2.x + b3 * p3.x) * width,
		y: (b0 * p0.y + b1 * p1.y + b2 * p2.y + b3 * p3.y) * height
	};
}
