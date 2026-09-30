// Numeric parity with the iOS truth. Every value here is copied from
// `magios/Shared/AmbientOrbAppearance.swift`'s `AuroraPalette` presets;
// if the Swift file retunes a stop, this test is where the website learns
// it drifted — a failing number, not a slowly wrong page.
import { describe, expect, it } from 'vitest';

import {
	AURORA,
	CONTROL_ACCENT,
	VOICE_ORB_LANDING_PHASE,
	VOICE_ORB_SEQUENCE,
	mixStops,
	rgb,
	rgba
} from './auroraPalette';

describe('aurora palette parity with AmbientOrbAppearance.swift', () => {
	it('pins armedEmber', () => {
		expect(AURORA.armedEmber.stops).toEqual([
			{ r: 0.36, g: 0.3, b: 0.52 },
			{ r: 0.2, g: 0.17, b: 0.3 }
		]);
		expect(AURORA.armedEmber.halo).toEqual({ r: 0.55, g: 0.42, b: 0.95 });
		expect(AURORA.armedEmber.haloStrength).toBe(0.16);
		expect(AURORA.armedEmber.blobSeed).toBe(0.9);
	});

	it('pins violetSurge, including the wrap-around third stop', () => {
		expect(AURORA.violetSurge.stops).toEqual([
			{ r: 0.62, g: 0.35, b: 1 },
			{ r: 1, g: 0.42, b: 0.78 },
			{ r: 0.62, g: 0.35, b: 1 }
		]);
		expect(AURORA.violetSurge.halo).toEqual({ r: 0.78, g: 0.45, b: 1 });
		expect(AURORA.violetSurge.haloStrength).toBe(0.9);
		expect(AURORA.violetSurge.blobSeed).toBe(2.1);
	});

	it('pins calmAurora', () => {
		expect(AURORA.calmAurora.stops).toEqual([
			{ r: 0.55, g: 0.38, b: 0.98 },
			{ r: 0.36, g: 0.48, b: 1 }
		]);
		expect(AURORA.calmAurora.halo).toEqual({ r: 0.6, g: 0.5, b: 1 });
		expect(AURORA.calmAurora.haloStrength).toBe(0.55);
		expect(AURORA.calmAurora.blobSeed).toBe(3.3);
	});

	it('pins amberThinking', () => {
		expect(AURORA.amberThinking.stops).toEqual([
			{ r: 1, g: 0.62, b: 0.26 },
			{ r: 0.94, g: 0.44, b: 0.18 }
		]);
		expect(AURORA.amberThinking.halo).toEqual({ r: 1, g: 0.65, b: 0.3 });
		expect(AURORA.amberThinking.haloStrength).toBe(0.5);
		expect(AURORA.amberThinking.blobSeed).toBe(4.6);
	});

	it('pins tealSpeaking', () => {
		expect(AURORA.tealSpeaking.stops).toEqual([
			{ r: 0.2, g: 0.85, b: 0.72 },
			{ r: 0.28, g: 0.78, b: 0.4 }
		]);
		expect(AURORA.tealSpeaking.halo).toEqual({ r: 0.3, g: 0.9, b: 0.6 });
		expect(AURORA.tealSpeaking.haloStrength).toBe(0.6);
		expect(AURORA.tealSpeaking.blobSeed).toBe(5.8);
	});

	it('pins graphite: white 0.45 → 0.28, and genuinely no glow', () => {
		expect(AURORA.graphite.stops).toEqual([
			{ r: 0.45, g: 0.45, b: 0.45 },
			{ r: 0.28, g: 0.28, b: 0.28 }
		]);
		expect(AURORA.graphite.haloStrength).toBe(0);
		expect(AURORA.graphite.blobSeed).toBe(0);
	});

	it('pins controlAccent as the surge’s leading stop, like the Swift file', () => {
		expect(CONTROL_ACCENT).toEqual(AURORA.violetSurge.stops[0]);
	});

	it('pins the native voice lifecycle and its notification handoff to the same final phase', () => {
		expect(VOICE_ORB_SEQUENCE).toEqual([
			AURORA.armedEmber,
			AURORA.violetSurge,
			AURORA.calmAurora,
			AURORA.amberThinking,
			AURORA.tealSpeaking
		]);
		expect(VOICE_ORB_SEQUENCE.at(-1)).toBe(VOICE_ORB_LANDING_PHASE);
		expect(VOICE_ORB_LANDING_PHASE).toBe(AURORA.tealSpeaking);
	});
});

describe('css rendering', () => {
	it('renders 0..1 components as 8-bit rgb()', () => {
		expect(rgb({ r: 0.62, g: 0.35, b: 1 })).toBe('rgb(158, 89, 255)');
		expect(rgba({ r: 1, g: 0, b: 0 }, 0.5)).toBe('rgba(255, 0, 0, 0.5)');
	});

	it('mixStops interpolates linearly and clamps t', () => {
		const mid = mixStops({ r: 0, g: 0, b: 0 }, { r: 1, g: 1, b: 1 }, 0.5);
		expect(mid).toEqual({ r: 0.5, g: 0.5, b: 0.5 });
		expect(mixStops({ r: 0, g: 0, b: 0 }, { r: 1, g: 1, b: 1 }, 2)).toEqual({ r: 1, g: 1, b: 1 });
	});
});
