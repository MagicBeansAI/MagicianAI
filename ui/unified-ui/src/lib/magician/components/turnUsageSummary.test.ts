import { describe, expect, it } from 'vitest';
import {
	ownCallIds,
	summarizeOwnCalls,
	withServerTurnUsage,
	type TurnCallRecord,
	type TurnUsageSummary
} from './turnUsageSummary';

const summary = (over: Partial<TurnUsageSummary> = {}): TurnUsageSummary => ({
	inputTokens: 100,
	cachedTokens: 40,
	cacheCreationTokens: null,
	outputTokens: 10,
	callCount: 4,
	hitPct: 40,
	costUsd: 0.1,
	...over
});

describe('withServerTurnUsage', () => {
	it('keeps the summed rows when the server sent no usage (older servers)', () => {
		const rows = summary();
		expect(withServerTurnUsage(rows, null, undefined)).toBe(rows);
		expect(withServerTurnUsage(rows, null, null)).toBe(rows);
	});

	it('shows the server whole-turn totals instead of a truncated page sum', () => {
		const result = withServerTurnUsage(summary(), null, {
			calls: 25,
			inputTokens: 5000,
			outputTokens: 700,
			costUsd: 1.5
		});
		expect(result).toMatchObject({
			callCount: 25,
			inputTokens: 5000,
			outputTokens: 700,
			costUsd: 1.5,
			// Cache figures still come from the rows.
			cachedTokens: 40,
			hitPct: 40
		});
	});

	it('adds only live rows that arrived after the read, never the covered ones', () => {
		const uncovered = summary({ callCount: 2, inputTokens: 30, outputTokens: 5, costUsd: 0.02 });
		const result = withServerTurnUsage(summary(), uncovered, {
			calls: 25,
			inputTokens: 5000,
			outputTokens: 700,
			costUsd: 1.5
		});
		expect(result?.callCount).toBe(27);
		expect(result?.inputTokens).toBe(5030);
		expect(result?.outputTokens).toBe(705);
		expect(result?.costUsd).toBeCloseTo(1.52);
	});

	it('keeps cost unpriced when neither side priced a call', () => {
		const result = withServerTurnUsage(null, summary({ costUsd: null }), {
			calls: 3,
			inputTokens: 1,
			outputTokens: 1,
			costUsd: null
		});
		expect(result?.costUsd).toBeNull();
		expect(result?.callCount).toBe(7);
	});

	it('falls back to rows for a turn the server reports no calls for', () => {
		expect(withServerTurnUsage(null, null, { calls: 0, inputTokens: 0, outputTokens: 0, costUsd: null })).toBeNull();
	});
});

describe('summarizeOwnCalls', () => {
	const TURN = 'chat-turn-1';
	const rec = (over: Partial<TurnCallRecord>): TurnCallRecord => ({
		input: null,
		output: null,
		cacheRead: null,
		cacheCreation: null,
		costUsd: null,
		...over
	});
	// The chat call lands twice (llm.succeeded + LLMResponseReceived, one call
	// id); the delegated task's calls carry the task's execution id.
	const MIXED: TurnCallRecord[] = [
		rec({ callId: 'call-own', executionId: TURN, input: 1000, output: 100, cacheRead: 600, costUsd: 2 }),
		rec({ callId: 'call-own', input: 1000, output: 100, cacheRead: 600, costUsd: 2 }),
		rec({ callId: 'task-call-1', executionId: 'exec_task', input: 500, output: 50, costUsd: 3 }),
		rec({ callId: 'task-call-2', executionId: 'exec_task', input: 500, output: 50, costUsd: 3.63 })
	];

	it('counts only the turn own calls, one per call id for tokens and cost', () => {
		const summary = summarizeOwnCalls(MIXED, TURN);
		expect(summary?.callCount).toBe(1);
		expect(summary?.inputTokens).toBe(1000);
		expect(summary?.outputTokens).toBe(100);
		expect(summary?.costUsd).toBe(2);
		expect(summary?.cachedTokens).toBe(600);
	});

	it('counts rows without a call id individually', () => {
		const summary = summarizeOwnCalls(
			[rec({ input: 10, costUsd: 0.1 }), rec({ input: 20, costUsd: 0.2 })],
			TURN
		);
		expect(summary?.callCount).toBe(2);
		expect(summary?.costUsd).toBeCloseTo(0.3);
	});

	it('skips calls already counted elsewhere', () => {
		const covered = ownCallIds(MIXED.slice(0, 1), TURN);
		expect([...covered]).toEqual(['call-own']);
		expect(summarizeOwnCalls(MIXED.slice(1), TURN, covered)).toBeNull();
	});

	it('returns null when the turn made no own calls', () => {
		expect(summarizeOwnCalls(MIXED.slice(2), TURN)).toBeNull();
	});
});

describe('withServerTurnUsage cache figures', () => {
	it('uses the server cache figures when present', () => {
		const result = withServerTurnUsage(summary(), null, {
			calls: 1,
			inputTokens: 1000,
			outputTokens: 100,
			costUsd: 2,
			cacheReadTokens: 600,
			cacheCreationTokens: 50
		});
		expect(result?.cachedTokens).toBe(600);
		expect(result?.cacheCreationTokens).toBe(50);
		expect(result?.hitPct).toBe(60);
	});

	it('falls back to row cache figures when the server sent none', () => {
		const result = withServerTurnUsage(summary(), null, {
			calls: 1,
			inputTokens: 1000,
			outputTokens: 100,
			costUsd: 2,
			cacheReadTokens: null,
			cacheCreationTokens: null
		});
		expect(result?.cachedTokens).toBe(40);
		expect(result?.hitPct).toBe(40);
	});
});
