import { describe, expect, it } from 'vitest';
import { tutorOverlayTransitionForEvent } from './tutorOverlayLifecycle';

describe('Tutor overlay lifecycle', () => {
	it.each(['tutor.run.failed', 'tutor.run.completed'])(
		'closes the matching overlay on %s even after the chat card stops being live',
		(eventType) => {
			const terminal = new Set<string>();
			expect(tutorOverlayTransitionForEvent(eventType, 'run-1', false, terminal)).toBe('idle');
			expect(terminal).toContain('run-1');
		}
	);

	it('never reopens an overlay while replaying historical activity', () => {
		const terminal = new Set<string>();
		expect(
			tutorOverlayTransitionForEvent('tutor.run.started', 'historical-run', false, terminal)
		).toBeNull();
		expect(
			tutorOverlayTransitionForEvent('tutor.step.recovering', 'historical-run', false, terminal)
		).toBeNull();
	});

	it('opens and advances a live Tutor run', () => {
		const terminal = new Set<string>();
		expect(tutorOverlayTransitionForEvent('tutor.run.started', 'run-1', true, terminal)).toBe(
			'working'
		);
		expect(tutorOverlayTransitionForEvent('tutor.step.recovering', 'run-1', true, terminal)).toBe(
			'working'
		);
	});

	it('does not let a late step event reopen a terminal run', () => {
		const terminal = new Set<string>();
		expect(tutorOverlayTransitionForEvent('tutor.run.failed', 'run-1', true, terminal)).toBe(
			'idle'
		);
		expect(tutorOverlayTransitionForEvent('tutor.step.recovering', 'run-1', true, terminal)).toBeNull();
	});

	it('allows an explicit new start to clear a prior terminal marker', () => {
		const terminal = new Set(['run-1']);
		expect(tutorOverlayTransitionForEvent('tutor.run.started', 'run-1', true, terminal)).toBe(
			'working'
		);
		expect(terminal).not.toContain('run-1');
	});
});
