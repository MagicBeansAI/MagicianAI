// The iOS ambient orb's aurora palette, ported numerically.
//
// SOURCE OF TRUTH: `magios/Shared/AmbientOrbAppearance.swift` —
// `AuroraPalette`'s named presets. The landing movie wears one phase's
// palette per chapter, so the page's emotional register IS the product's:
// the violet a visitor sees at the delegation beat is byte-for-byte the
// violet the Dynamic Island answers a wake with. Change the Swift presets
// and this file must follow — `auroraPalette.test.ts` pins every number so
// the drift is a failing test rather than a slowly wrong website.
//
// Values are sRGB components in 0..1 exactly as Swift declares them;
// `rgb()`/`rgba()` render them for CSS at call sites.

export interface AuroraStop {
	r: number;
	g: number;
	b: number;
}

export interface AuroraPhase {
	/** Angular-gradient stops for the orb body, in Swift's order. */
	stops: AuroraStop[];
	/** The glow rendered under the body. */
	halo: AuroraStop;
	/** 0..1 opacity of the halo layer. 0 means no glow at all (graphite). */
	haloStrength: number;
	/** `AuroraBlobShape`'s silhouette seed — each phase owns a still form. */
	blobSeed: number;
}

const stop = (r: number, g: number, b: number): AuroraStop => ({ r, g, b });

export const AURORA = {
	armedEmber: {
		stops: [stop(0.36, 0.3, 0.52), stop(0.2, 0.17, 0.3)],
		halo: stop(0.55, 0.42, 0.95),
		haloStrength: 0.16,
		blobSeed: 0.9
	},
	violetSurge: {
		stops: [stop(0.62, 0.35, 1.0), stop(1.0, 0.42, 0.78), stop(0.62, 0.35, 1.0)],
		halo: stop(0.78, 0.45, 1.0),
		haloStrength: 0.9,
		blobSeed: 2.1
	},
	calmAurora: {
		stops: [stop(0.55, 0.38, 0.98), stop(0.36, 0.48, 1.0)],
		halo: stop(0.6, 0.5, 1.0),
		haloStrength: 0.55,
		blobSeed: 3.3
	},
	amberThinking: {
		stops: [stop(1.0, 0.62, 0.26), stop(0.94, 0.44, 0.18)],
		halo: stop(1.0, 0.65, 0.3),
		haloStrength: 0.5,
		blobSeed: 4.6
	},
	tealSpeaking: {
		stops: [stop(0.2, 0.85, 0.72), stop(0.28, 0.78, 0.4)],
		halo: stop(0.3, 0.9, 0.6),
		haloStrength: 0.6,
		blobSeed: 5.8
	},
	graphite: {
		stops: [stop(0.45, 0.45, 0.45), stop(0.28, 0.28, 0.28)],
		halo: stop(0, 0, 0),
		haloStrength: 0,
		blobSeed: 0
	}
} as const satisfies Record<string, AuroraPhase>;

export type AuroraPhaseName = keyof typeof AURORA;

/**
 * The native voice-orb lifecycle, in the same order used by the product.
 * Landing surfaces must consume `VOICE_ORB_LANDING_PHASE` rather than
 * choosing a nearby theme color: the large orb physically becomes that
 * smaller glyph, so both sides of the handoff share one paint source.
 */
export const VOICE_ORB_LANDING_PHASE: AuroraPhase = AURORA.tealSpeaking;
export const VOICE_ORB_SEQUENCE: readonly AuroraPhase[] = [
	AURORA.armedEmber,
	AURORA.violetSurge,
	AURORA.calmAurora,
	AURORA.amberThinking,
	VOICE_ORB_LANDING_PHASE
];

/** The kit's control accent — the surge's leading stop, like the Swift file. */
export const CONTROL_ACCENT: AuroraStop = AURORA.violetSurge.stops[0];

const to255 = (v: number): number => Math.round(v * 255);

export function rgb(s: AuroraStop): string {
	return `rgb(${to255(s.r)}, ${to255(s.g)}, ${to255(s.b)})`;
}

export function rgba(s: AuroraStop, alpha: number): string {
	return `rgba(${to255(s.r)}, ${to255(s.g)}, ${to255(s.b)}, ${alpha})`;
}

/** Linear blend between two stops — enough for the canvas orb's phase morph. */
export function mixStops(a: AuroraStop, b: AuroraStop, t: number): AuroraStop {
	const u = Math.min(1, Math.max(0, t));
	return {
		r: a.r + (b.r - a.r) * u,
		g: a.g + (b.g - a.g) * u,
		b: a.b + (b.b - a.b) * u
	};
}
