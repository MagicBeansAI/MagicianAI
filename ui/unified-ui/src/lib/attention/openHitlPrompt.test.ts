import { describe, expect, it, vi } from 'vitest';

import {
	hitlOpenTargetFromFeedItem,
	hitlOpenTargetFromPendingEntry,
	hitlOpenTargetFromUnknown,
	hitlRequestFromOpenTarget,
	openHitlPrompt,
	type HitlOpenTarget
} from './openHitlPrompt';

function target(overrides: Partial<HitlOpenTarget> = {}): HitlOpenTarget {
	return {
		id: 'question-7',
		source: 'clarification',
		input_type: 'choice',
		prompt: 'Which release channel?',
		input_schema: {
			options: [
				{ id: 'stable', label: 'Stable' },
				{ id: 'preview', label: 'Preview' }
			]
		},
		identifiers: { correlation_id: 'question-7' },
		scope: {
			principal: 'owner',
			workspace: 'default',
			workflow_id: 'task-4',
			task_id: 'task-4',
			execution_id: 'planexec-4'
		},
		...overrides
	};
}

const scope = () => ({
	principal: 'owner',
	workspace: 'default',
	key: 'owner:default'
});

describe('direct HITL prompt opening', () => {
	it('normalizes a complete target into the canonical request contract', () => {
		expect(hitlRequestFromOpenTarget(target())).toEqual({
			id: 'question-7',
			source: 'clarification',
			input_type: 'choice',
			schema: {
				options: [
					{ id: 'stable', label: 'Stable' },
					{ id: 'preview', label: 'Preview' }
				]
			},
			prompt: 'Which release channel?',
			hint: undefined,
			scope: {
				principal: 'owner',
				workspace: 'default',
				workflow_id: 'task-4',
				task_id: 'task-4',
				execution_id: 'planexec-4'
			},
			identifiers: {
				pause_state_id: undefined,
				approval_id: undefined,
				correlation_id: 'question-7',
				request_id: undefined
			},
			at: undefined
		});
	});

	it('normalizes persisted legacy choice values to canonical option ids', () => {
		const normalized = hitlOpenTargetFromUnknown({
			...target(),
			input_schema: {
				options: [{ value: 'stable', label: 'Stable' }]
			}
		});

		expect(normalized?.input_schema?.options).toEqual([
			{ id: 'stable', label: 'Stable', description: undefined, requires_input: undefined }
		]);
	});

	it('opens directly, preserves scope headers, and drops only the authoritative canonical id', async () => {
		const respond = vi.fn().mockResolvedValue({ ok: true });
		const dropPending = vi.fn();
		const dropAttention = vi.fn();

		const result = await openHitlPrompt(
			target({
				id: 'txn-7',
				source: 'diff_approval',
				input_type: 'diff_approval',
				input_schema: { transaction_id: 'txn-7' },
				identifiers: {
					correlation_id: 'txn-7',
					pause_state_id: 'pause-7'
				},
				scope: {
					principal: 'owner',
					workspace: 'default',
					task_id: 'task-4',
					execution_id: 'exec-4'
				}
			}),
			{ getScope: scope, respond, dropPending, dropAttention }
		);

		expect(result.status).toBe('resolved');
		expect(respond).toHaveBeenCalledWith(
			expect.objectContaining({
				id: 'txn-7',
				input_type: 'diff_approval',
				schema: expect.objectContaining({ transaction_id: 'txn-7' })
			}),
			expect.any(Headers),
			undefined
		);
		const sentHeaders = new Headers(respond.mock.calls[0]?.[1]);
		expect(sentHeaders.get('X-Principal')).toBeNull();
		expect(sentHeaders.get('X-Workspace')).toBeNull();
		expect(dropPending.mock.calls.map(([id]) => id)).toEqual(['txn-7']);
		expect(dropAttention.mock.calls).toEqual(dropPending.mock.calls);
	});

	it('keeps validation reasks in the same direct flow with the previous answer', async () => {
		const respond = vi
			.fn()
			.mockResolvedValueOnce({
				ok: false,
				reask: true,
				status: 200,
				message: 'Please be more specific.',
				question: 'Which exact channel?',
				previousAnswer: 'Stable'
			})
			.mockResolvedValueOnce({ ok: true });

		const result = await openHitlPrompt(
			target({ input_type: 'text', input_schema: {} }),
			{ getScope: scope, respond, dropPending: vi.fn(), dropAttention: vi.fn() }
		);

		expect(result.status).toBe('resolved');
		expect(respond).toHaveBeenNthCalledWith(
			2,
			expect.objectContaining({ prompt: 'Which exact channel?' }),
			expect.any(Object),
			{ defaultValue: 'Stable' }
		);
	});

	it('rejects an incomplete target without opening a prompt', async () => {
		const respond = vi.fn();
		const result = await openHitlPrompt(target({ id: '', identifiers: {} }), {
			getScope: scope,
			respond
		});

		expect(result).toEqual({
			status: 'error',
			error: 'HITL target is missing a resolvable identifier.'
		});
		expect(respond).not.toHaveBeenCalled();
	});

	it('rejects non-object schema fields from an untrusted target', () => {
		expect(
			hitlOpenTargetFromUnknown({
				...target(),
				input_schema: []
			})
		).toBeNull();
	});

	it('rejects conflicting source-specific identifiers', () => {
		expect(
			hitlRequestFromOpenTarget(
				target({
					id: 'approval-7',
					source: 'approval',
					input_type: 'confirmation',
					identifiers: {
						approval_id: 'approval-other',
						correlation_id: 'approval-7'
					}
				})
			)
		).toBeNull();
		expect(
			hitlRequestFromOpenTarget(
				target({
					id: 'approval-7',
					source: 'approval',
					input_type: 'confirmation',
					identifiers: { correlation_id: 'approval-7' }
				})
			)
		).toBeNull();
		expect(
			hitlRequestFromOpenTarget(
				target({
					id: 'approval-7',
					source: 'approval',
					input_type: 'confirmation',
					identifiers: {
						approval_id: 'approval-7',
						correlation_id: 'approval-7',
						pause_state_id: 'pause-unrelated'
					}
				})
			)
		).toBeNull();
	});

	it('rejects bot authorization ids that do not match their origin scope', () => {
		expect(
			hitlRequestFromOpenTarget(
				target({
					id: 'bot_auth:other:default:gmail',
					source: 'bot_auth',
					input_type: 'choice',
					identifiers: { correlation_id: 'bot_auth:other:default:gmail' },
					scope: { principal: 'owner', workspace: 'default' }
				})
			)
		).toBeNull();
	});

	it('rejects bot authorization ids with an empty bot segment', () => {
		expect(
			hitlOpenTargetFromUnknown({
				id: 'bot_auth:owner:default:',
				source: 'bot_auth',
				input_type: 'choice',
				prompt: 'Reconnect bot',
				input_schema: { options: [{ id: 'recheck', label: 'Recheck' }] },
				identifiers: { correlation_id: 'bot_auth:owner:default:' },
				scope: { principal: 'owner', workspace: 'default' }
			})
		).toBeNull();
	});

	it('requires nonempty choice options and exact specialized source/input pairings', () => {
		expect(hitlRequestFromOpenTarget(target({ input_schema: {} }))).toBeNull();
		expect(
			hitlOpenTargetFromUnknown({
				id: 'approval-7',
				source: 'approval',
				input_type: 'choice',
				prompt: 'Approve?',
				input_schema: { options: [{ id: 'approve', label: 'Approve' }] },
				identifiers: { approval_id: 'approval-7', correlation_id: 'approval-7' },
				scope: { principal: 'owner', workspace: 'default' }
			})
		).toBeNull();
		expect(
			hitlOpenTargetFromUnknown({
				id: 'bot_auth:owner:default:gmail',
				source: 'bot_auth',
				input_type: 'external_action',
				prompt: 'Reconnect Gmail',
				identifiers: { correlation_id: 'bot_auth:owner:default:gmail' },
				scope: { principal: 'owner', workspace: 'default' }
			})
		).toBeNull();
		expect(
			hitlOpenTargetFromUnknown({
				id: 'request-7',
				source: 'user_request',
				input_type: 'tool_authorization',
				prompt: 'Allow tool?',
				identifiers: { request_id: 'request-7', correlation_id: 'request-7' },
				scope: { principal: 'owner', workspace: 'default' }
			})
		).toBeNull();
	});

	it('normalizes documented legacy sources and rejects unknown source identifiers', () => {
		const normalized = hitlOpenTargetFromUnknown({
			id: 'pause-legacy',
			source: 'inner_loop',
			input_type: 'text',
			prompt: 'Continue?',
			input_schema: {},
			identifiers: { pause_state_id: 'pause-legacy', correlation_id: 'pause-legacy' },
			scope: {
				principal: 'owner',
				workspace: 'default',
				execution_id: 'exec-legacy'
			}
		});

		expect(normalized?.source).toBe('agentic');
		expect(hitlOpenTargetFromUnknown({ ...target(), source: 'legacy_guess' })).toBeNull();
	});

	it('allows a diff target to carry its distinct owning pause id', () => {
		expect(
			hitlRequestFromOpenTarget(
				target({
					id: 'txn-7',
					source: 'diff_approval',
					input_type: 'diff_approval',
					input_schema: { transaction_id: 'txn-7' },
					identifiers: {
						correlation_id: 'txn-7',
						pause_state_id: 'pause-7'
					},
					scope: {
						principal: 'owner',
						workspace: 'default',
						task_id: 'task-4',
						execution_id: 'exec-4'
					}
				})
			)
		).toEqual(
			expect.objectContaining({
				id: 'txn-7',
				identifiers: expect.objectContaining({ pause_state_id: 'pause-7' })
			})
		);
	});

	it('refuses to answer a target from a different origin scope', async () => {
		const respond = vi.fn();
		const result = await openHitlPrompt(
			target({
				scope: {
					principal: 'other-owner',
					workspace: 'default',
					workflow_id: 'task-4',
					task_id: 'task-4',
					execution_id: 'planexec-4'
				}
			}),
			{ getScope: scope, respond }
		);

		expect(result.status).toBe('error');
		expect(result).toEqual(
			expect.objectContaining({
				error: expect.stringContaining('other-owner/default')
			})
		);
		expect(respond).not.toHaveBeenCalled();
	});

	it('prefers an embedded backend target over lossy feed-id inference', () => {
		const embedded = target({
			id: 'real-correlation',
			identifiers: { correlation_id: 'real-correlation' },
			at: 2
		});
		const result = hitlOpenTargetFromFeedItem({
			id: 'synthetic-feed-row',
			principal: 'owner',
			workspace: 'default',
			item_type: 'task',
			title: 'Synthetic row',
			status: 'needs_action',
			created_at: 1,
			updated_at: 2,
			actions: [],
			metadata: { hitl_request: embedded }
		});

		expect(result).toEqual(
			expect.objectContaining({
				...embedded,
				hint: undefined
			})
		);
	});

	it('rejects malformed embedded targets instead of opening an unroutable prompt', () => {
		const result = hitlOpenTargetFromFeedItem({
			id: 'synthetic-feed-row',
			principal: 'owner',
			workspace: 'default',
			item_type: 'task',
			title: 'Synthetic row',
			status: 'needs_action',
			created_at: 1,
			updated_at: 2,
			actions: [],
			metadata: {
				hitl_request: {
					...target(),
					source: 'unroutable_projection'
				}
			}
		});

		expect(result).toBeNull();
	});

	it('scope-binds a metadata-less legacy approval fallback', () => {
		const target = hitlOpenTargetFromFeedItem({
			id: 'approval-legacy',
			principal: 'owner',
			workspace: 'default',
			item_type: 'approval',
			title: 'Approve deployment',
			status: 'needs_action',
			created_at: 1,
			updated_at: 2,
			actions: [],
			metadata: null
		});

		expect(target && hitlRequestFromOpenTarget(target)).toEqual(
			expect.objectContaining({
				id: 'approval-legacy',
				scope: expect.objectContaining({
					principal: 'owner',
					workspace: 'default'
				})
			})
		);
	});

	it('rebuilds a direct target from a canonical event-stream entry', () => {
		const result = hitlOpenTargetFromPendingEntry({
			correlation_id: 'question-11',
			at: 11,
			raw: {
				event_type: 'HitlRequested',
				data: {
					correlation_id: 'question-11',
					source: 'clarification',
					input_type: 'text',
					prompt: 'Which account?',
					input_schema: { multiline: false },
					principal: 'owner',
					workspace: 'default',
					task_id: 'task-11',
					execution_id: 'planexec-11'
				}
			}
		});

		expect(result).toEqual(
			expect.objectContaining({
				id: 'question-11',
				prompt: 'Which account?',
				scope: expect.objectContaining({
					principal: 'owner',
					workspace: 'default',
					workflow_id: 'task-11',
					execution_id: 'planexec-11'
				})
			})
		);
	});

	it('keeps source-less or malformed canonical events on the ID-only fallback', () => {
		const base = {
			event_type: 'HitlRequested',
			data: {
				correlation_id: 'question-12',
				input_type: 'choice',
				prompt: 'Which account?',
				input_schema: { options: [{ id: 'one', label: 'One' }] },
				principal: 'owner',
				workspace: 'default',
				task_id: 'task-12',
				execution_id: 'exec-12'
			}
		};
		expect(
			hitlOpenTargetFromPendingEntry({ correlation_id: 'question-12', at: 12, raw: base })
		).toBeNull();
		expect(
			hitlOpenTargetFromPendingEntry({
				correlation_id: 'question-12',
				at: 12,
				raw: {
					...base,
					data: { ...base.data, source: 'clarification', input_schema: { options: [] } }
				}
			})
		).toBeNull();
		expect(
			hitlOpenTargetFromPendingEntry({
				correlation_id: 'question-12',
				at: 12,
				raw: {
					...base,
					data: {
						...base.data,
						source: 'clarification',
						pause_state_id: 'pause-from-another-source'
					}
				}
			})
		).toBeNull();
	});
});

/**
 * **The backstop, and the reason it is not redundant.**
 *
 * Ordinarily this loop cannot spin: every iteration awaits `respond`, which
 * opens the modal, so it advances only on a fresh human answer. That stopped
 * being the whole story when `answerTaskAsk` began substituting the answer the
 * reader typed in the task panel for the first modal — a `respond` that does not
 * prompt. The limit is what keeps a backend re-asking forever from becoming an
 * unbounded POST loop instead of an error the reader can see.
 */
describe('openHitlPrompt — the re-ask limit', () => {
	it('stops re-opening after a bounded number of revisions and reports the last reason', async () => {
		let attempts = 0;
		const respond = async () => {
			attempts += 1;
			return {
				ok: false as const,
				reask: true as const,
				status: 200,
				message: 'Still not a quarter I have data for.',
				question: `Try again (${attempts})`
			};
		};

		const result = await openHitlPrompt(
			{
				id: 'sq_7c11',
				source: 'clarification',
				input_type: 'text',
				prompt: 'Which quarter?',
				identifiers: { correlation_id: 'sq_7c11' },
				scope: {
					principal: 'anonymous',
					workspace: 'default',
					workflow_id: 'task_alpha',
					task_id: 'task_alpha'
				}
			},
			{
				respond,
				getScope: () => ({ principal: 'anonymous', workspace: 'default', key: 'anonymous:default' }),
				dropPending: () => {},
				dropAttention: () => {}
			}
		);

		// Finite, and the reader is told why rather than being left in a dialog
		// that keeps reappearing.
		expect(attempts).toBe(6);
		expect(result.status).toBe('error');
		expect(result).toMatchObject({ error: 'Still not a quarter I have data for.' });
	});
});
