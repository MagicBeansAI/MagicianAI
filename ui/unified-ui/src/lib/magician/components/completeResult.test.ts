import { describe, expect, it } from 'vitest';
import {
	parseCompleteResultOwner,
	reconstructCompleteResult,
	resolveCompleteResultReadTarget,
	type CompleteResultEntry,
} from './completeResult';

describe('complete result reconstruction', () => {
	it('preserves the legacy complete-record array representation', () => {
		const entries: CompleteResultEntry[] = [
			{ field_path: '', source_index: 0, value: { id: 1 } },
			{ field_path: '', source_index: 1, value: { id: 2 } },
		];
		expect(reconstructCompleteResult(entries, 0)).toEqual([{ id: 1 }, { id: 2 }]);
	});

	it('reconstructs typed containers, escaped object keys, and fragments across pages', () => {
		const text = 'नमस्ते-🧭-مرحبا';
		const first = text.slice(0, 7);
		const firstBytes = new TextEncoder().encode(first).byteLength;
		const totalBytes = new TextEncoder().encode(text).byteLength;
		const entries: CompleteResultEntry[] = [
			{ field_path: '', reconstruction_path: '', kind: 'container', value: {} },
			{ field_path: '', reconstruction_path: '/nested', kind: 'container', value: {} },
			{
				field_path: '',
				reconstruction_path: '/nested/slash~1key~0',
				kind: 'string_fragment',
				string_fragment: { byte_start: 0, byte_end: firstBytes, total_bytes: totalBytes },
				value: first,
			},
			{
				field_path: '',
				reconstruction_path: '/nested/slash~1key~0',
				kind: 'string_fragment',
				string_fragment: { byte_start: firstBytes, byte_end: totalBytes, total_bytes: totalBytes },
				value: text.slice(7),
			},
			{ field_path: '', reconstruction_path: '/empty', kind: 'complete_value', value: [] },
		];
		expect(reconstructCompleteResult(entries, 1)).toEqual({
			nested: { 'slash/key~': text },
			empty: [],
		});
	});

	it('fails closed when a fragment is missing or out of order', () => {
		expect(() =>
			reconstructCompleteResult(
				[
					{
						field_path: '',
						reconstruction_path: '',
						kind: 'string_fragment',
						string_fragment: { byte_start: 4, byte_end: 8, total_bytes: 8 },
						value: 'tail',
					},
				],
				1
			)
		).toThrow(/missing, duplicated, or out of order/);
	});

	it('preserves v0 and v1 root scalars without wrapping or stringifying them', () => {
		expect(reconstructCompleteResult([{ field_path: '', value: 42 }], 0)).toBe(42);
		expect(
			reconstructCompleteResult(
				[{ field_path: '', reconstruction_path: '', kind: 'complete_value', value: 'नमस्ते 🧭' }],
				1
			)
		).toBe('नमस्ते 🧭');
	});

	it('uses typed v1 containers to preserve numeric object keys', () => {
		expect(
			reconstructCompleteResult(
				[
					{ field_path: '', reconstruction_path: '', kind: 'container', value: {} },
					{ field_path: '', reconstruction_path: '/0', kind: 'complete_value', value: 'object-key' },
				],
				1
			)
		).toEqual({ '0': 'object-key' });
	});

	it('rejects duplicate paths, skipped array indexes, and invalid pointer escapes', () => {
		expect(() =>
			reconstructCompleteResult(
				[
					{ field_path: '', source_index: 0, value: 'first' },
					{ field_path: '', source_index: 0, value: 'duplicate' },
				],
				0
			)
		).toThrow(/duplicate reconstruction path/);
		expect(() =>
			reconstructCompleteResult([{ field_path: '', source_index: 1, value: 'skipped zero' }], 0)
		).toThrow(/missing or out of order/);
		expect(() =>
			reconstructCompleteResult(
				[{ field_path: '', reconstruction_path: '/bad~2key', kind: 'complete_value', value: true }],
				1
			)
		).toThrow(/invalid JSON-pointer escape/);
	});

	it('rejects complete-parent overlap and out-of-order parent containers', () => {
		expect(() =>
			reconstructCompleteResult(
				[
					{ field_path: '', reconstruction_path: '/record', kind: 'complete_value', value: { value: 1 } },
					{ field_path: '', reconstruction_path: '/record/value', kind: 'complete_value', value: 2 },
				],
				1
			)
		).toThrow(/overlapping scalar/);
		expect(() =>
			reconstructCompleteResult(
				[
					{ field_path: '', reconstruction_path: '/record/value', kind: 'complete_value', value: 2 },
					{ field_path: '', reconstruction_path: '/record', kind: 'container', value: {} },
				],
				1
			)
		).toThrow(/out-of-order parent/);
	});

	it('treats prototype-looking JSON pointer tokens as inert own properties', () => {
		const value = reconstructCompleteResult(
			[
				{ field_path: '', reconstruction_path: '', kind: 'container', value: {} },
				{ field_path: '', reconstruction_path: '/__proto__', kind: 'container', value: {} },
				{ field_path: '', reconstruction_path: '/__proto__/polluted', kind: 'complete_value', value: true },
			],
			1
		) as Record<string, unknown>;

		expect(Object.prototype).not.toHaveProperty('polluted');
		expect(Object.prototype.hasOwnProperty.call(value, '__proto__')).toBe(true);
		expect((value.__proto__ as Record<string, unknown>).polluted).toBe(true);
	});
});

describe('complete-result ownership', () => {
	it('keeps a chat-owned delegation result on the chat read API even when the row has task navigation', () => {
		const owner = parseCompleteResultOwner({
			kind: 'chat',
			session_id: 'session parent',
		});
		expect(
			resolveCompleteResultReadTarget({
				owner,
				hostSessionId: 'session-host',
				legacyTaskId: 'delegated-task',
				legacyExecutionId: 'delegated-exec',
			})
		).toEqual({
			path: '/api/magician/v2/chat/sessions/session%20parent/results/read',
			executionId: null,
		});
	});

	it('uses the canonical task owner and its execution binding', () => {
		const owner = parseCompleteResultOwner({
			kind: 'task',
			task_id: 'task 7',
			execution_id: 'exec-7',
		});
		expect(
			resolveCompleteResultReadTarget({ owner, hostSessionId: 'session-host' })
		).toEqual({
			path: '/api/magician/v3/tasks/task%207/results/read',
			executionId: 'exec-7',
		});
	});

	it('retains the legacy task-id fallback only for pre-owner events', () => {
		expect(
			resolveCompleteResultReadTarget({
				owner: null,
				hostSessionId: 'session-host',
				legacyTaskId: 'legacy-task',
				legacyExecutionId: 'legacy-exec',
			})
		).toEqual({
			path: '/api/magician/v3/tasks/legacy-task/results/read',
			executionId: 'legacy-exec',
		});
	});

	it('does not route ephemeral voice ownership through an unrelated chat', () => {
		const owner = parseCompleteResultOwner({
			kind: 'ephemeral_voice',
			voice_session_id: 'voice-7',
		});
		expect(
			resolveCompleteResultReadTarget({ owner, hostSessionId: 'session-host' })
		).toBeNull();
	});
});
