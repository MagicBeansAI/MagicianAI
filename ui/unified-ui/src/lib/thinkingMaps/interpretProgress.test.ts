import { describe, expect, it } from 'vitest';
import {
	interpretProgressApplies,
	interpretStageLine,
	parseInterpretStage
} from './interpretProgress';

/**
 * Interpretation progress — the two pure decisions the page makes about
 * narrated stages. The vocabulary is a wire contract with the backend's
 * `InterpretStage::wire_name`; the routing gate is what keeps a shared bus
 * from driving the wrong strip.
 */
describe('parseInterpretStage', () => {
	it('recognises exactly the six narrated stages', () => {
		for (const stage of [
			'preparing',
			'loading_context',
			'facilitating',
			'parsing',
			'shaping',
			'idle'
		]) {
			expect(parseInterpretStage(stage)).toBe(stage);
		}
	});

	it('returns null for a stage this build predates, never a guess', () => {
		// The two labels iOS once declared for work no server step backs are
		// strangers now, exactly like any future stage.
		expect(parseInterpretStage('grounding')).toBeNull();
		expect(parseInterpretStage('opening_thread')).toBeNull();
		expect(parseInterpretStage('')).toBeNull();
		expect(parseInterpretStage('PREPARING')).toBeNull();
	});
});

describe('interpretStageLine', () => {
	it('gives every stage a label and a sentence', () => {
		for (const stage of [
			'preparing',
			'loading_context',
			'facilitating',
			'parsing',
			'shaping',
			'idle'
		] as const) {
			const line = interpretStageLine(stage);
			expect(line.label.length).toBeGreaterThan(0);
			expect(line.detail.length).toBeGreaterThan(0);
		}
	});

	it('preparing folds the count in — the number is what makes the wait feel like motion', () => {
		expect(interpretStageLine('preparing', 34).detail).toBe(
			'Reading 34 thoughts and the active branch…'
		);
		expect(interpretStageLine('preparing', 1).detail).toBe(
			'Reading 1 thought and the active branch…'
		);
	});

	it('preparing without a count reads generically — "Reading 0 thoughts" reads as a bug, not a board', () => {
		expect(interpretStageLine('preparing').detail).toBe('Reading the board and the active branch…');
		expect(interpretStageLine('preparing', 0).detail).toBe(
			'Reading the board and the active branch…'
		);
		expect(interpretStageLine('preparing', null).detail).toBe(
			'Reading the board and the active branch…'
		);
	});

	it('the count belongs to preparing alone', () => {
		expect(interpretStageLine('shaping', 34)).toEqual(interpretStageLine('shaping'));
	});
});

describe('interpretProgressApplies', () => {
	const event = { map_id: 'm1', utterance_id: 'u1', stage: 'facilitating' };

	it('applies only to this page’s own in-flight run', () => {
		expect(interpretProgressApplies(event, 'm1', 'u1')).toBe(true);

		// Another map's run, another run on this map, or no run at all.
		expect(interpretProgressApplies(event, 'm2', 'u1')).toBe(false);
		expect(interpretProgressApplies(event, 'm1', 'u2')).toBe(false);
		expect(interpretProgressApplies(event, 'm1', null)).toBe(false);
		expect(interpretProgressApplies(event, null, 'u1')).toBe(false);
	});

	it('keeps the current line for a stage this build has never heard of', () => {
		expect(interpretProgressApplies({ ...event, stage: 'grounding' }, 'm1', 'u1')).toBe(false);
	});

	it('never applies the terminal idle — settling is the HTTP response’s job', () => {
		// A realtime idle can outrun the response body; clearing early would
		// show "Ready" over a run still being applied.
		expect(interpretProgressApplies({ ...event, stage: 'idle' }, 'm1', 'u1')).toBe(false);
	});
});
