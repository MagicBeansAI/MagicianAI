import { describe, expect, it } from 'vitest';

import {
	amplitudeToDisplay,
	organicRadialWaveform,
	deriveVoiceStage,
	followEnvelope,
	pulseRings,
	pushHistory,
	radialWaveform,
	rmsFromTimeDomain,
	stageIsLive
} from './voiceViz';

/** A time-domain buffer of a sine at the given peak (0..1 about the 128 centre). */
function sine(peak: number, n = 256): Uint8Array {
	const b = new Uint8Array(n);
	for (let i = 0; i < n; i += 1) b[i] = 128 + Math.round(Math.sin((i / n) * Math.PI * 2) * 127 * peak);
	return b;
}

describe('rmsFromTimeDomain', () => {
	it('reads silence as zero', () => {
		// getByteTimeDomainData centres on 128; a flat 128 buffer IS silence.
		expect(rmsFromTimeDomain(new Uint8Array(256).fill(128))).toBe(0);
	});

	it('scales with real signal level', () => {
		const quiet = rmsFromTimeDomain(sine(0.2));
		const loud = rmsFromTimeDomain(sine(0.9));
		expect(loud).toBeGreaterThan(quiet);
		// A full-scale sine has RMS = peak/sqrt(2) ~= 0.707.
		expect(rmsFromTimeDomain(sine(1))).toBeCloseTo(0.707, 1);
	});

	it('survives an empty buffer', () => {
		expect(rmsFromTimeDomain(new Uint8Array(0))).toBe(0);
	});
});

describe('followEnvelope', () => {
	it('rises faster than it falls', () => {
		// Fast attack / slow release is why this reads as a voice rather than a
		// throb -- the onset of speech is the part that carries meaning.
		const up = followEnvelope(0, 1);
		const down = 1 - followEnvelope(1, 0);
		expect(up).toBeGreaterThan(down);
	});

	it('comes fully to rest instead of twitching forever', () => {
		let v = 1;
		for (let i = 0; i < 200; i += 1) v = followEnvelope(v, 0);
		expect(v).toBe(0);
	});

	it('never exceeds one and ignores garbage targets', () => {
		expect(followEnvelope(0.5, 10)).toBeLessThanOrEqual(1);
		expect(followEnvelope(0.5, Number.NaN)).toBe(0.5);
	});
});

describe('amplitudeToDisplay', () => {
	it('lifts conversational levels into a visible range but never clips flat', () => {
		// Speech RMS sits ~0.05-0.2; raw, that is visually nothing.
		expect(amplitudeToDisplay(0.12)).toBeGreaterThan(0.3);
		// Shouting must still outrank talking -- no saturating to a constant.
		expect(amplitudeToDisplay(0.5)).toBeGreaterThan(amplitudeToDisplay(0.25));
		expect(amplitudeToDisplay(0)).toBe(0);
		expect(amplitudeToDisplay(5)).toBe(1);
	});
});

describe('deriveVoiceStage', () => {
	it('maps the real call states', () => {
		const q = { userSpeaking: false, assistantSpeaking: false };
		expect(deriveVoiceStage({ callState: 'idle', ...q })).toBe('offline');
		expect(deriveVoiceStage({ callState: 'connecting', ...q })).toBe('connecting');
		expect(deriveVoiceStage({ callState: 'reconnecting', ...q })).toBe('connecting');
		expect(deriveVoiceStage({ callState: 'rotating', ...q })).toBe('connecting');
		expect(deriveVoiceStage({ callState: 'connected', ...q })).toBe('listening');
	});

	it('an error outranks everything', () => {
		expect(
			deriveVoiceStage({ callState: 'connected', error: 'mic denied', userSpeaking: true, assistantSpeaking: false })
		).toBe('error');
	});

	it('during barge-in the agent outranks the user', () => {
		// Both flags set at once is exactly the barge-in case; showing the agent
		// is what explains to the operator why they are being talked over.
		expect(
			deriveVoiceStage({ callState: 'connected', userSpeaking: true, assistantSpeaking: true })
		).toBe('agent');
	});

	it('only live stages drive amplitude', () => {
		expect(stageIsLive('listening')).toBe(true);
		expect(stageIsLive('you')).toBe(true);
		expect(stageIsLive('agent')).toBe(true);
		expect(stageIsLive('offline')).toBe(false);
		expect(stageIsLive('connecting')).toBe(false);
		expect(stageIsLive('error')).toBe(false);
	});
});

describe('radialWaveform', () => {
	it('returns a flat ring for silence and a varying one for signal', () => {
		const flat = radialWaveform(new Uint8Array(256).fill(128), 64, 100, 30);
		expect(flat).toHaveLength(64);
		expect(new Set(flat.map((r) => Math.round(r)))).toEqual(new Set([100]));

		const live = radialWaveform(sine(0.9), 64, 100, 30);
		expect(Math.max(...live)).toBeGreaterThan(100);
		expect(Math.min(...live)).toBeLessThan(100);
	});

	it('averages buckets so a large buffer cannot alias into a fake standing wave', () => {
		const wave = radialWaveform(sine(0.9, 2048), 180, 100, 30);
		expect(wave).toHaveLength(180);
		expect(wave.every((r) => Number.isFinite(r))).toBe(true);
	});

	it('degrades to the base ring when there is no buffer at all', () => {
		expect(radialWaveform(new Uint8Array(0), 4, 90, 20)).toEqual([90, 90, 90, 90]);
	});
});

describe('pulseRings + history', () => {
	it('rings grow outward and fade', () => {
		const rings = pulseRings([1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1], 4, 100, 20);
		expect(rings).toHaveLength(4);
		for (let i = 1; i < rings.length; i += 1) {
			expect(rings[i].radius).toBeGreaterThan(rings[i - 1].radius);
			expect(rings[i].alpha).toBeLessThan(rings[i - 1].alpha);
		}
	});

	it('silence leaves the rings dim rather than invisible-but-moving', () => {
		const rings = pulseRings([], 3, 100, 20);
		expect(rings.every((r) => r.alpha <= 0.25)).toBe(true);
	});

	it('history is newest-first and bounded', () => {
		let h: number[] = [];
		for (let i = 0; i < 100; i += 1) h = pushHistory(h, i / 100, 48);
		expect(h).toHaveLength(48);
		expect(h[0]).toBeCloseTo(0.99);
		expect(h[1]).toBeCloseTo(0.98);
	});
});

describe('organicRadialWaveform', () => {
	it('is deterministic in time and changes as time advances', () => {
		const a = organicRadialWaveform(null, 96, 100, 20, 3.2, 0.4);
		const b = organicRadialWaveform(null, 96, 100, 20, 3.2, 0.4);
		const c = organicRadialWaveform(null, 96, 100, 20, 4.9, 0.4);
		expect(a).toEqual(b);
		expect(a).not.toEqual(c);
	});

	it('closes at the 2π seam — integer angular frequencies only', () => {
		// The first and last points are ADJACENT on the circle; a non-integer
		// frequency in the noise stack shows up as a crack between them.
		const r = organicRadialWaveform(null, 512, 100, 24, 7.7, 1);
		expect(Math.abs(r[0] - r[511])).toBeLessThan(2);
	});

	it('speech roils the surface more than silence', () => {
		const spread = (r: number[]) => Math.max(...r) - Math.min(...r);
		const quiet = organicRadialWaveform(null, 128, 100, 20, 2.1, 0);
		const loud = organicRadialWaveform(null, 128, 100, 20, 2.1, 1);
		expect(spread(loud)).toBeGreaterThan(spread(quiet) * 2);
		// But silence still drifts faintly -- the channel being open IS a reading.
		expect(spread(quiet)).toBeGreaterThan(0);
	});

	it('stays bounded by the swing budget', () => {
		const r = organicRadialWaveform(null, 128, 100, 20, 9.3, 1);
		for (const v of r) {
			expect(v).toBeGreaterThan(100 - 21);
			expect(v).toBeLessThan(100 + 21);
		}
	});
});
