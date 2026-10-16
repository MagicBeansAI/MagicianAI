import { describe, expect, it } from 'vitest';
import { distillProcessedTotal, distillQueueView, type ChannelAssistStats } from './channelStatsStore';

function snapshot(pending: number, expired: number, done = 100, skipped = 20): ChannelAssistStats {
	return { distill: {
		pending, done, skipped, suppressed: 0,
		by_state: { pending, expired, done, skipped },
		queue: { history_floor_ms: 1000, pending: 0, retryable: 0, outside_history: pending, expired, retry_exhausted: 2 }
	} } as unknown as ChannelAssistStats;
}

describe('distillation queue history', () => {
	it('keeps history outside the eligible queue before and after retirement', () => {
		expect(distillQueueView(snapshot(5050, 0), null)).toEqual({ queued: 0, outsideHistory: 5050, expired: 0, retryExhausted: 2 });
		expect(distillQueueView(snapshot(0, 5050), null)).toEqual({ queued: 0, outsideHistory: 0, expired: 5050, retryExhausted: 2 });
	});
	it('does not report expiration or history changes as model processing', () => {
		expect(distillProcessedTotal(snapshot(0, 5050)) - distillProcessedTotal(snapshot(5050, 0))).toBe(0);
		expect(distillProcessedTotal(snapshot(0, 5050, 101, 22)) - distillProcessedTotal(snapshot(0, 5050))).toBe(3);
	});
	it('includes eligible retries without counting exhausted failures', () => {
		const s = snapshot(2, 10);
		s.distill.queue = { history_floor_ms: 1000, pending: 2, retryable: 3, outside_history: 0, expired: 10, retry_exhausted: 782 };
		expect(distillQueueView(s, null).queued).toBe(5);
	});
	it('keeps older servers usable when queue partition fields are absent', () => {
		const s = snapshot(5, 0); delete s.distill.queue;
		expect(distillQueueView(s, null).queued).toBe(5);
		expect(distillQueueView(null, null).queued).toBe(0);
	});
});
