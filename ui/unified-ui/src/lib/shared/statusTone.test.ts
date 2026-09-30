import { describe, expect, it } from 'vitest';
import { statusBadgeTone, statusTone, statusToneVar } from './statusTone';

describe('statusTone', () => {
	it('maps run-family statuses to the five semantic tones', () => {
		expect(statusTone('running').tone).toBe('running');
		expect(statusTone('in_progress').tone).toBe('running');
		expect(statusTone('paused').tone).toBe('paused');
		expect(statusTone('failed').tone).toBe('failed');
		expect(statusTone('error').tone).toBe('failed');
		expect(statusTone('rejected').tone).toBe('failed');
		expect(statusTone('needs_attention').tone).toBe('attention');
		expect(statusTone('pending_approval').tone).toBe('attention');
		expect(statusTone('completed').tone).toBe('completed');
		expect(statusTone('done').tone).toBe('completed');
	});

	it('maps the extended real-world statuses found in the UI', () => {
		// Activity-flavored → running
		expect(statusTone('planning').tone).toBe('running');
		expect(statusTone('synthesizing').tone).toBe('running');
		expect(statusTone('building').tone).toBe('running');
		// On hold / retry → paused
		expect(statusTone('deferred').tone).toBe('paused');
		expect(statusTone('snoozed').tone).toBe('paused');
		// Terminal-bad → failed
		expect(statusTone('cancelled').tone).toBe('failed');
		// Needs a human → attention
		expect(statusTone('needs_action').tone).toBe('attention');
		expect(statusTone('eliciting').tone).toBe('attention');
		expect(statusTone('blocked').tone).toBe('attention');
		// Terminal-good → completed
		expect(statusTone('approved').tone).toBe('completed');
		expect(statusTone('succeeded').tone).toBe('completed');
	});

	it('falls back to neutral for unknown/idle statuses', () => {
		expect(statusTone('pending').tone).toBe('neutral');
		expect(statusTone('whatever').tone).toBe('neutral');
		// Idle states are explicitly neutral — no color shout
		// (unifies today's green-vs-blue split for ready).
		expect(statusTone('ready').tone).toBe('neutral');
		expect(statusTone('draft').tone).toBe('neutral');
		expect(statusTone('idle').tone).toBe('neutral');
		expect(statusTone('queued').tone).toBe('neutral');
		expect(statusTone('skipped').tone).toBe('neutral');
		expect(statusTone('info').tone).toBe('neutral'); // FeedItemStatus — informational, no shout
		expect(statusTone(null).tone).toBe('neutral');
		expect(statusTone(undefined).tone).toBe('neutral');
	});

	it('does not leak prototype-chain keys — unknown built-in names stay neutral', () => {
		expect(statusTone('constructor').tone).toBe('neutral');
		expect(statusTone('toString').tone).toBe('neutral');
		expect(statusTone('hasOwnProperty').tone).toBe('neutral');
	});

	it('normalizes case and whitespace', () => {
		expect(statusTone('  RUNNING ').tone).toBe('running');
		expect(statusTone('Completed').tone).toBe('completed');
	});

	it('emits CSS var references, never hexes', () => {
		expect(statusToneVar('running')).toBe('var(--status-running)');
		expect(statusToneVar('pending')).toBe('var(--text-muted)');
	});

	it('emits matching -soft background vars for toned statuses', () => {
		expect(statusTone('running').softVar).toBe('var(--status-running-soft)');
		expect(statusTone('failed').softVar).toBe('var(--status-failed-soft)');
		expect(statusTone('needs_action').softVar).toBe('var(--status-attention-soft)');
		expect(statusTone('pending').softVar).toBe('var(--bg-soft)');
	});

	it('statusBadgeTone returns the tone for toned statuses and null for neutral', () => {
		expect(statusBadgeTone('running')).toBe('running');
		expect(statusBadgeTone('planning')).toBe('running');
		expect(statusBadgeTone('paused')).toBe('paused');
		expect(statusBadgeTone('failed')).toBe('failed');
		expect(statusBadgeTone('cancelled')).toBe('failed');
		expect(statusBadgeTone('completed')).toBe('completed');
		// Neutral → null so Badge renders its default gray, never a tone class
		expect(statusBadgeTone('ready')).toBeNull();
		expect(statusBadgeTone('pending')).toBeNull();
		expect(statusBadgeTone('skipped')).toBeNull();
		expect(statusBadgeTone('unknown-status')).toBeNull();
		expect(statusBadgeTone(null)).toBeNull();
		expect(statusBadgeTone(undefined)).toBeNull();
	});
});
