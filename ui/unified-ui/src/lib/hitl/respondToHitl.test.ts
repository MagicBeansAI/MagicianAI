/**
 * Regression guard for the HITL "cards never dismiss" symptom.
 *
 * The owner's most-cited failure was that a stale HITL card would show a
 * terminal submit-error (and stay on screen) when the backend had already
 * drained the pause. `adapters.postHitlResponse` classifies the responses so
 * the card dismisses instead:
 *
 *   - 404 "Pause state not found" (agentic/preplan resume, already drained)
 *     → soft success `{ ok: true }` (dismiss)
 *   - 409 `{reason:"already_resolved"}` (clarification double-submit)
 *     → soft success `{ ok: true }` (dismiss)
 *   - a genuine 500 → `{ ok: false }` (real error, surfaced)
 *   - HTTP 200 `{resumed:false, status:"reask_required", question, ...}`
 *     → a DISTINCT reask outcome (`ok:false, reask:true`) so the caller
 *       re-opens the modal instead of rendering a bare error.
 *
 * `postHitlResponse` issues its request through `timedFetch`
 * (`$lib/shared/fetch`), so we `vi.mock` that module and drive the response
 * per call. Mirrors the mocking approach in `../notify/resolve.test.ts`.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

// Capture the timedFetch invocation and control the Response it resolves to.
const timedFetch = vi.fn();
vi.mock('$lib/shared/fetch', () => ({
	timedFetch: (input: RequestInfo | URL, init?: RequestInit & { timeoutMs?: number }) =>
		timedFetch(input, init),
	DEFAULT_FETCH_TIMEOUT_MS: 30_000,
	LONG_FETCH_TIMEOUT_MS: 600_000
}));

const requestAttentionInput = vi.fn();
const executeTask = vi.hoisted(() => vi.fn());
vi.mock('$lib/stores/taskStore', () => ({ taskStore: { executeTask } }));
vi.mock('$lib/stores/attentionPromptStore', () => ({
	requestAttentionInput: (request: unknown) => requestAttentionInput(request)
}));

import { get } from 'svelte/store';
import { notifications } from '$lib/shared/stores/notifications';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { postHitlResponse, hitlRequestFromCanonicalEvent, hitlRequestFromFeedItem, resolveHitlCall } from './adapters';
import { HITL_ALREADY_CLOSED_NOTICE, resetHitlAlreadyClosedNoticeForTests } from './alreadyClosedNotice';

function alreadyClosedNotices(): number {
	return get(notifications).notifications.filter(
		(notification) => notification.title === HITL_ALREADY_CLOSED_NOTICE
	).length;
}

describe('revision-bound agentic wire identity', () => {
	it('posts the exact ask revision rather than the reusable pause storage key', () => {
		const askId = 'exec:pause~0123456789ab';
		const request = hitlRequestFromCanonicalEvent({ event_type: 'HitlRequested', data: {
			source: 'agentic', input_type: 'text', prompt: 'Which date?',
			correlation_id: askId, pause_state_id: 'exec:pause', input_schema: {},
			principal: 'owner', workspace: 'default', execution_id: 'exec'
		} });
		expect(request).not.toBeNull();
		expect(request!.identifiers.pause_state_id).toBe('exec:pause');
		expect(resolveHitlCall(request!, { type: 'text', value: '2026-10-20' })?.url)
			.toBe(`/api/magician/v2/hitl/${encodeURIComponent(askId)}/respond`);
	});
});
import { respondToHitl, startRunAfterPlanApproval } from './respondToHitl';
import type { HitlInputType, HitlRequest, HitlResponseValue } from './types';

it('does not start a run when the focused Approve & Run was already answered', async () => {
	executeTask.mockClear();
	requestAttentionInput.mockResolvedValueOnce({ kind: 'choice', choiceId: 'approve_run' });
	timedFetch.mockResolvedValueOnce(jsonResponse(409, { reason: 'already_resolved' }));
	const outcome = await respondToHitl({ ...agenticTextRequest(), source: 'plan_approval',
		input_type: 'confirmation', scope: { task_id: 'task-plan' }, identifiers: { correlation_id: 'plan-123' } });
	expect(outcome).toEqual({ ok: true, alreadyClosed: true });
	expect(executeTask).not.toHaveBeenCalled();
});

describe('approval workspace changes', () => {
	beforeEach(() => {
		scopeIdentityStore.observe('owner', 'first');
		executeTask.mockReset();
		requestAttentionInput.mockReset();
		timedFetch.mockReset();
		notifications.clear();
	});
	afterEach(() => scopeIdentityStore.reset());

	function planRequest(): HitlRequest {
		return { ...agenticTextRequest(), source: 'plan_approval', input_type: 'confirmation',
			scope: { task_id: 'task-plan' }, identifiers: { correlation_id: 'plan-123' } };
	}

	it('does not start a task in the newly selected workspace after approval returns', async () => {
		requestAttentionInput.mockResolvedValueOnce({ kind: 'choice', choiceId: 'approve_run' });
		let finish!: (response: Response) => void;
		timedFetch.mockReturnValueOnce(new Promise<Response>((resolve) => { finish = resolve; }));
		const pending = respondToHitl(planRequest());
		await vi.waitFor(() => expect(timedFetch).toHaveBeenCalledOnce());
		scopeIdentityStore.observe('owner', 'second');
		finish(jsonResponse(200, { accepted: true }));
		expect(await pending).toEqual({ ok: true });
		expect(executeTask).not.toHaveBeenCalled();
		expect(get(notifications).notifications.some((notice) =>
			notice.title.includes('workspace changed'))).toBe(true);
	});

	it('does not post an answer if the workspace changes while the prompt is open', async () => {
		let answer!: (result: { kind: 'choice'; choiceId: string }) => void;
		requestAttentionInput.mockReturnValueOnce(new Promise((resolve) => { answer = resolve; }));
		const pending = respondToHitl(planRequest());
		scopeIdentityStore.observe('owner', 'second');
		answer({ kind: 'choice', choiceId: 'approve_run' });
		expect(await pending).toMatchObject({ ok: false, message: expect.stringContaining('workspace changed') });
		expect(timedFetch).not.toHaveBeenCalled();
		expect(executeTask).not.toHaveBeenCalled();
	});

	it('still starts the approved task when the workspace remains selected', async () => {
		requestAttentionInput.mockResolvedValueOnce({ kind: 'choice', choiceId: 'approve_run' });
		timedFetch.mockResolvedValueOnce(jsonResponse(200, { accepted: true }));
		expect(await respondToHitl(planRequest())).toEqual({ ok: true });
		expect(executeTask).toHaveBeenCalledExactlyOnceWith('task-plan');
	});

	it('also checks the request scope for inline Approve & Run', async () => {
		const request = planRequest();
		request.scope = { ...request.scope, principal: 'owner', workspace: 'first' };
		scopeIdentityStore.observe('other-owner', 'first');
		await startRunAfterPlanApproval(request, { kind: 'choice', choiceId: 'approve_run' });
		expect(executeTask).not.toHaveBeenCalled();
	});
});

// A minimal, valid agentic HITL request. `resolveHitlCall` derives its
// correlation id from the source-specific pause id, so this resolves to a real POST call
// carrying `soft_success_statuses: [404, 409, 410]`.
function agenticTextRequest(): HitlRequest {
	return {
		id: 'pause-123',
		source: 'agentic',
		input_type: 'text',
		schema: {},
		prompt: 'What is your name?',
		scope: { execution_id: 'exec-123' },
		identifiers: { pause_state_id: 'pause-123' }
	};
}

function clarificationTextRequest(): HitlRequest {
	return {
		id: 'question-123',
		source: 'clarification',
		input_type: 'text',
		schema: {},
		prompt: 'Which domain?',
		scope: {
			workflow_id: 'task-123',
			task_id: 'task-123',
			execution_id: 'planexec-123'
		},
		identifiers: { correlation_id: 'question-123' }
	};
}

const textValue: HitlResponseValue = { type: 'text', value: 'Ada' };

function jsonResponse(status: number, body: Record<string, unknown>): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'Content-Type': 'application/json' }
	});
}

describe('postHitlResponse — stale/already-resolved cards dismiss, not error', () => {
	beforeEach(() => {
		timedFetch.mockReset();
		notifications.clear();
		resetHitlAlreadyClosedNoticeForTests();
	});

	it('(a) treats a 404 "Pause state not found" as a soft success (dismiss) and says so', async () => {
		timedFetch.mockResolvedValue(jsonResponse(404, { reason: 'Pause state not found' }));

		const outcome = await postHitlResponse(agenticTextRequest(), textValue);

		// The card still drops, but the reader sees their answer did not land.
		expect(outcome).toEqual({ ok: true, alreadyClosed: true });
		expect(alreadyClosedNotices()).toBe(1);
		expect(get(notifications).notifications[0].type).toBe('info');
	});

	it('coalesces a burst of already-closed answers into one notice', async () => {
		timedFetch.mockImplementation(async () => jsonResponse(409, { reason: 'already_resolved' }));

		await postHitlResponse(agenticTextRequest(), textValue);
		await postHitlResponse(agenticTextRequest(), textValue);
		await postHitlResponse(agenticTextRequest(), textValue);

		expect(alreadyClosedNotices()).toBe(1);
	});

	it('shows no already-closed notice for an accepted answer', async () => {
		timedFetch.mockResolvedValue(jsonResponse(200, { accepted: true }));

		const outcome = await postHitlResponse(agenticTextRequest(), textValue);

		expect(outcome).toEqual({ ok: true });
		expect(alreadyClosedNotices()).toBe(0);
	});

	it.each(['{}', 'null', '<html>Sign in</html>', ''])('keeps the question open without an acknowledgement: %s', async (body) => {
		timedFetch.mockResolvedValue(new Response(body, { status: 200 }));
		const outcome = await postHitlResponse(agenticTextRequest(), textValue);
		expect(outcome.ok).toBe(false);
		expect(alreadyClosedNotices()).toBe(0);
	});

	it('treats 410 pause_state_gone as a soft success (dismiss)', async () => {
		timedFetch.mockResolvedValue(
			jsonResponse(410, {
				code: 'pause_state_gone',
				message: 'Pause state was already resolved or expired'
			})
		);

		const outcome = await postHitlResponse(agenticTextRequest(), textValue);

		expect(outcome).toEqual({ ok: true, alreadyClosed: true });
		expect(alreadyClosedNotices()).toBe(1);
	});

	it('(b) treats a 409 already_resolved as a soft success (dismiss)', async () => {
		timedFetch.mockResolvedValue(jsonResponse(409, { reason: 'already_resolved' }));

		const outcome = await postHitlResponse(agenticTextRequest(), textValue);

		expect(outcome).toEqual({ ok: true, alreadyClosed: true });
		expect(alreadyClosedNotices()).toBe(1);
	});

	it('(c) surfaces a genuine 500 as a real error', async () => {
		timedFetch.mockResolvedValue(jsonResponse(500, { reason: 'boom' }));

		const outcome = await postHitlResponse(agenticTextRequest(), textValue);

		expect(outcome.ok).toBe(false);
		// Not a soft success and not a reask — a terminal error carrying the status.
		expect(outcome.ok === false && 'reask' in outcome).toBe(false);
		expect(outcome.ok === false && !('cancelled' in outcome) ? outcome.status : undefined).toBe(
			500
		);
	});

	it('does not hide a missing clarification as an already-drained pause', async () => {
		timedFetch.mockResolvedValue(
			jsonResponse(404, { reason: 'clarification_dispatch_failed' })
		);

		const outcome = await postHitlResponse(clarificationTextRequest(), textValue);

		expect(outcome.ok).toBe(false);
		expect(outcome.ok === false && !('cancelled' in outcome) ? outcome.status : undefined).toBe(
			404
		);
	});

	it('does not apply agentic 410 semantics to a clarification', async () => {
		timedFetch.mockResolvedValue(jsonResponse(410, { code: 'clarification_gone' }));

		const outcome = await postHitlResponse(clarificationTextRequest(), textValue);

		expect(outcome.ok).toBe(false);
		expect(outcome.ok === false && !('cancelled' in outcome) ? outcome.status : undefined).toBe(
			410
		);
	});

	it('still treats a clarification 409 already_resolved as success', async () => {
		timedFetch.mockResolvedValue(jsonResponse(409, { reason: 'already_resolved' }));

		const outcome = await postHitlResponse(clarificationTextRequest(), textValue);

		expect(outcome).toEqual({ ok: true, alreadyClosed: true });
	});

	it('(d) surfaces a 200 reask_required as a distinct reask outcome, not a bare error', async () => {
		timedFetch.mockResolvedValue(
			jsonResponse(200, {
				resumed: false,
				status: 'reask_required',
				question: 'Please give your full name',
				hint: 'first and last',
				previous_answer: 'Ada'
			})
		);

		const outcome = await postHitlResponse(agenticTextRequest(), textValue);

		expect(outcome.ok).toBe(false);
		// The reask branch must fire — otherwise the caller renders a terminal
		// error and the pause (still live) is lost.
		expect(outcome.ok === false && 'reask' in outcome && outcome.reask).toBe(true);
		if (outcome.ok === false && 'reask' in outcome) {
			expect(outcome.reask).toBe(true);
			expect(outcome.question).toBe('Please give your full name');
			expect(outcome.hint).toBe('first and last');
			expect(outcome.previousAnswer).toBe('Ada');
		}
	});

	it('preserves specialized input type and source-specific execution binding', async () => {
		timedFetch.mockResolvedValue(jsonResponse(200, { accepted: true }));
		await postHitlResponse(
			{
				...agenticTextRequest(),
				input_type: 'tool_authorization',
				schema: {
					options: [
						{ id: 'allow_once', label: 'Allow once' },
						{ id: 'deny', label: 'Deny' }
					]
				}
			},
			{ type: 'choice', selected_id: 'allow_once' }
		);

		const [, init] = timedFetch.mock.calls[0];
		expect(JSON.parse(String(init?.body))).toEqual(
			expect.objectContaining({
				source: 'agentic',
				input_type: 'tool_authorization',
				execution_id: 'exec-123'
			})
		);
	});

	it('keeps the planning responder separate from the durable plan execution id', async () => {
		timedFetch.mockResolvedValue(jsonResponse(200, { accepted: true }));
		await postHitlResponse(clarificationTextRequest(), textValue);

		const [, init] = timedFetch.mock.calls[0];
		expect(JSON.parse(String(init?.body))).toEqual(
			expect.objectContaining({
				task_id: 'task-123',
				execution_id: 'planexec-123',
				input_type: 'text'
			})
		);
	});
});

describe('respondToHitl reask defaults', () => {
	beforeEach(() => {
		requestAttentionInput.mockReset();
		requestAttentionInput.mockResolvedValue(null);
	});

	it.each(['text', 'guidance', 'file_path'] satisfies HitlInputType[])(
		'prefills a previous %s answer',
		async (inputType) => {
			await respondToHitl(
				{ ...agenticTextRequest(), input_type: inputType },
				{},
				{ defaultValue: 'Previous answer' }
			);

			expect(requestAttentionInput).toHaveBeenCalledWith(
				expect.objectContaining({ defaultValue: 'Previous answer' })
			);
		}
	);

	it('never prefills a password', async () => {
		await respondToHitl(
			{ ...agenticTextRequest(), input_type: 'password' },
			{},
			{ defaultValue: 'secret' }
		);

		expect(requestAttentionInput).toHaveBeenCalledWith(
			expect.objectContaining({ kind: 'password', defaultValue: undefined })
		);
	});
});


describe('one-time secure credential HITL', () => {
 it('posts cancellation when a private prompt is dismissed', async () => {
  timedFetch.mockReset(); requestAttentionInput.mockResolvedValue(null);
  timedFetch.mockResolvedValue(jsonResponse(200, { accepted: true }));
  const request: HitlRequest = { ...agenticTextRequest(), source:'user_request', input_type:'password',
    schema:{ request_type:'secure_browser_input' }, identifiers:{correlation_id:'secure-1',request_id:'secure-1'},
    scope:{principal:'owner',workspace:'workspace'} };
  await respondToHitl(request);
  const body=JSON.parse(timedFetch.mock.calls[0][1].body);
  expect(body.value).toEqual({type:'aborted'});
  expect(body.source).toBe('user_request');
 });
 it('uses a hidden prompt and sends its value only to the scoped HITL endpoint', async () => {
  timedFetch.mockReset(); requestAttentionInput.mockResolvedValue({kind:'password',value:'one-time-canary'});
  timedFetch.mockResolvedValue(jsonResponse(200, {accepted:true}));
  const request: HitlRequest = { ...agenticTextRequest(), source:'user_request', input_type:'password',
    schema:{request_type:'secure_browser_input'}, identifiers:{correlation_id:'secure-2',request_id:'secure-2'},
    scope:{principal:'owner',workspace:'workspace'} };
  await respondToHitl(request);
  expect(requestAttentionInput.mock.calls.at(-1)?.[0].kind).toBe('password');
  expect(JSON.parse(timedFetch.mock.calls[0][1].body).value).toEqual({type:'password',value:'one-time-canary'});
 });
});

// ─── P3 Task 3.8: the published spec drives masking, cancel, and the wire ──

describe('the sensitive spec published by the backend', () => {
	const otpSpec = {
		kind: 'otp' as const,
		provenance: 'heuristic' as const,
		one_time: true,
		collection_deadline_ms: Date.now() + 180_000
	};

	beforeEach(() => {
		timedFetch.mockReset();
		requestAttentionInput.mockReset();
	});

	it('masks a text-typed ask the backend classified as a code and posts the type the pause expects', async () => {
		requestAttentionInput.mockResolvedValue({ kind: 'otp', value: '007123' });
		timedFetch.mockResolvedValue(jsonResponse(200, { resumed: true }));
		const request: HitlRequest = {
			...agenticTextRequest(),
			prompt: 'Enter the verification code we sent',
			schema: { sensitive: otpSpec }
		};
		await respondToHitl(request, {}, { defaultValue: '123' });
		const prompt = requestAttentionInput.mock.calls.at(-1)?.[0];
		expect(prompt.kind).toBe('otp');
		expect(prompt.defaultValue).toBeUndefined();
		expect(prompt.sensitive).toEqual({
			kind: 'otp',
			oneTime: true,
			deadlineMs: otpSpec.collection_deadline_ms,
			// The ask the modal reads the retrieval status for (P6): the
			// request's own correlation, which names one ask of a step (P7).
			correlationId: 'pause-123'
		});
		const body = JSON.parse(timedFetch.mock.calls[0][1].body);
		expect(body.value).toEqual({ type: 'text', value: '007123' });
	});

	it('renders a typed otp ask as a code field and posts the password value shape', async () => {
		requestAttentionInput.mockResolvedValue({ kind: 'otp', value: '007123' });
		timedFetch.mockResolvedValue(jsonResponse(200, { resumed: true }));
		await respondToHitl({
			...agenticTextRequest(),
			input_type: 'otp',
			schema: { sensitive: { ...otpSpec, provenance: 'typed_input' } }
		});
		expect(requestAttentionInput.mock.calls.at(-1)?.[0].kind).toBe('otp');
		const body = JSON.parse(timedFetch.mock.calls[0][1].body);
		expect(body.value).toEqual({ type: 'password', value: '007123' });
		expect(body.input_type).toBe('otp');
	});

	it('dismissing any spec-classified ask posts an explicit cancel, not a silent close', async () => {
		requestAttentionInput.mockResolvedValue(null);
		timedFetch.mockResolvedValue(jsonResponse(200, { accepted: true }));
		const request: HitlRequest = {
			...agenticTextRequest(),
			source: 'user_request',
			input_type: 'text',
			schema: { request_type: 'need_user_input', sensitive: { kind: 'password', provenance: 'heuristic', one_time: false } },
			identifiers: { correlation_id: 'req-9', request_id: 'req-9' },
			scope: { principal: 'owner', workspace: 'workspace' }
		};
		const outcome = await respondToHitl(request);
		expect(outcome).toEqual({ ok: true });
		expect(JSON.parse(timedFetch.mock.calls[0][1].body).value).toEqual({ type: 'aborted' });
	});

	it('masks only the flagged fields of a mixed form', async () => {
		requestAttentionInput.mockResolvedValue(null);
		await respondToHitl({
			...agenticTextRequest(),
			input_type: 'form',
			schema: {
				questions: [
					{ id: 'user', prompt: 'Username' },
					{ id: 'pw', prompt: 'Password' },
					{ id: 'city', prompt: 'City' },
					{ id: 'code', prompt: 'Code', input_type: 'otp' }
				],
				sensitive: {
					provenance: 'form_schema',
					one_time: false,
					fields: [
						{ id: 'user', kind: 'login_identifier' },
						{ id: 'pw', kind: 'password' }
					]
				}
			}
		});
		const prompt = requestAttentionInput.mock.calls.at(-1)?.[0];
		expect(prompt.formQuestions.map((q: { id: string; sensitive?: string }) => [q.id, q.sensitive])).toEqual([
			['user', 'login_identifier'],
			['pw', 'password'],
			['city', undefined],
			['code', 'otp']
		]);
	});

	it('leaves an ordinary ask exactly as it was', async () => {
		requestAttentionInput.mockResolvedValue(null);
		const outcome = await respondToHitl(agenticTextRequest(), {}, { defaultValue: 'Lisbon' });
		expect(outcome).toEqual({ ok: false, cancelled: true });
		const prompt = requestAttentionInput.mock.calls.at(-1)?.[0];
		expect(prompt.kind).toBe('text');
		expect(prompt.defaultValue).toBe('Lisbon');
		expect(prompt.sensitive).toBeUndefined();
		expect(timedFetch).not.toHaveBeenCalled();
	});
});


describe('service health notices', () => {
    it('keeps the service failure visible when opening a feed notice', () => {
        const request = hitlRequestFromFeedItem({
            id:'runtime:hitl:service_health:test', principal:'alice', workspace:'work',
            item_type:'escalation', status:'needs_action', title:'Pi needs credentials.',
            summary:'Dismiss this notice or retry after signing in.', created_at:1, updated_at:1,
            actions:[], metadata:{attention_kind:'hitl.requested',source:'service_health',
                correlation_id:'service_health:test', input_type:'choice',
                input_schema:{type:'choice',options:[{id:'dismiss',label:'Dismiss'}]}}
        } as Parameters<typeof hitlRequestFromFeedItem>[0]);
        expect(request?.prompt).toBe('Pi needs credentials.');
    });

    it('accepts a scoped taskless notice and routes dismissal through canonical HITL', () => {
        const request = hitlRequestFromCanonicalEvent({event_type: 'HitlRequested', data: {
            source: 'service_health', correlation_id: 'service_health:fixture',
            principal: 'alice', workspace: 'work', input_type: 'choice',
            prompt: 'Pi needs credentials for its selected model.',
            input_schema: {type:'choice', options:[{id:'dismiss',label:'Dismiss'}], allow_other:false}
        }});
        expect(request?.source).toBe('service_health');
        const call = resolveHitlCall(request!, {type:'choice',selected_id:'dismiss'});
        expect(call).not.toBeNull();
        expect(JSON.stringify(call)).toContain('service_health');
        expect(JSON.stringify(call)).toContain('dismiss');
    });
});

describe('pending user-request attention rows', () => {
	// The live shape (2026-10-20): the row names the request only as a flat
	// `pause_state_id` and inside its embedded `hitl_request` envelope.
	// Acknowledging it answered "No resolver available for this request shape."
	it('resolves an owner briefing to its user request', () => {
		const request = hitlRequestFromFeedItem({
			id: 'v3:attention:attention:user_request.pending:task_1:exec_1', principal: 'anonymous',
			workspace: 'default', item_type: 'task', status: 'needs_action', title: '1 pending user request(s).',
			summary: 'Owner briefing', created_at: 1, updated_at: 1, actions: [],
			metadata: {
				attention_kind: 'user_request.pending', source: 'user_request', input_type: 'choice',
				pause_state_id: 'req-1', execution_id: 'exec_1',
				input_schema: { request_type: 'harness.notify_owner.briefing', options: [{ id: 'acknowledge', label: 'Acknowledge' }] },
				hitl_request: { id: 'req-1', source: 'user_request', identifiers: { correlation_id: 'req-1', request_id: 'req-1' } }
			}
		} as Parameters<typeof hitlRequestFromFeedItem>[0]);
		expect(request?.identifiers.request_id).toBe('req-1');
		const call = resolveHitlCall(request!, { type: 'choice', selected_id: 'acknowledge' });
		expect(call?.url).toBe('/api/magician/v2/hitl/req-1/respond');
	});

	it('takes the request id from the pause id when nothing else names it', () => {
		const request = hitlRequestFromFeedItem({
			id: 'row', principal: 'p', workspace: 'w', item_type: 'task', status: 'needs_action',
			title: 'ask', summary: null, created_at: 1, updated_at: 1, actions: [],
			metadata: {
				attention_kind: 'user_request.pending', source: 'user_request', input_type: 'choice',
				pause_state_id: 'req-2', input_schema: { options: [{ id: 'acknowledge', label: 'Acknowledge' }] }
			}
		} as Parameters<typeof hitlRequestFromFeedItem>[0]);
		expect(resolveHitlCall(request!, { type: 'choice', selected_id: 'acknowledge' })?.url).toBe(
			'/api/magician/v2/hitl/req-2/respond'
		);
	});
});

describe('mcp_oauth sign-in asks (C4)', () => {
	function mcpOauthEvent(schema: Record<string, unknown> = {}) {
		return hitlRequestFromCanonicalEvent({
			event_type: 'HitlRequested',
			data: {
				source: 'mcp_oauth',
				correlation_id: 'mcp_oauth:binding:flow:attempt',
				principal: 'alice',
				workspace: 'work',
				input_type: 'external_action',
				prompt: 'Finish connecting this service in your browser.',
				input_schema: {
					instructions: 'Complete sign-in in the browser window.',
					done_label: 'Open sign-in again',
					...schema
				}
			}
		});
	}

	it('is a known source, rendered as an external action with its link', () => {
		const request = mcpOauthEvent({ url: 'https://auth.example.com/authorize?x=1' });
		expect(request?.source).toBe('mcp_oauth');
		expect(request?.schema.url).toBe('https://auth.example.com/authorize?x=1');
	});

	it('drops a non-http link rather than rendering it', () => {
		expect(mcpOauthEvent({ url: 'javascript:alert(1)' })?.schema.url).toBeUndefined();
	});

	it('refuses any input type other than external_action', () => {
		const request = hitlRequestFromCanonicalEvent({
			event_type: 'HitlRequested',
			data: {
				source: 'mcp_oauth',
				correlation_id: 'mcp_oauth:x',
				principal: 'alice',
				workspace: 'work',
				input_type: 'text',
				prompt: 'Sign in'
			}
		});
		expect(request).toBeNull();
	});

	it('answers external_action_completed against the correlation id, with no note field', async () => {
		const request = mcpOauthEvent()!;
		requestAttentionInput.mockResolvedValueOnce({ kind: 'choice', choiceId: 'completed' });
		timedFetch.mockResolvedValueOnce(new Response(JSON.stringify({ accepted: true }), { status: 200 }));
		await expect(respondToHitl(request)).resolves.toEqual({ ok: true });
		const prompt = requestAttentionInput.mock.calls.at(-1)?.[0] as {
			externalAction?: { showNote?: boolean; browserSigninCorrelationId?: string; browserSigninScope?: { principal: string; workspace: string } };
		};
		expect(prompt.externalAction?.showNote).toBe(false);
		expect(prompt.externalAction?.browserSigninCorrelationId).toBe('mcp_oauth:binding:flow:attempt');
		expect(prompt.externalAction?.browserSigninScope).toEqual({ principal: 'alice', workspace: 'work' });
		const [url, init] = timedFetch.mock.calls.at(-1)!;
		expect(url).toBe('/api/magician/v2/hitl/mcp_oauth%3Abinding%3Aflow%3Aattempt/respond');
		expect(JSON.parse(String(init?.body))).toMatchObject({
			source: 'mcp_oauth',
			value: { type: 'external_action_completed' }
		});
	});

	it('declines with "Not now" by posting an abort, which cancels the flow server-side', async () => {
		const request = mcpOauthEvent({ dismiss_label: 'Not now', service_label: 'Swiggy (food)' })!;
		expect(request.schema.service_label).toBe('Swiggy (food)');
		requestAttentionInput.mockResolvedValueOnce({ kind: 'choice', choiceId: 'dismiss' });
		timedFetch.mockResolvedValueOnce(
			new Response(JSON.stringify({ accepted: true, reason: 'dismissed' }), { status: 200 })
		);
		await expect(respondToHitl(request)).resolves.toEqual({ ok: true });
		const prompt = requestAttentionInput.mock.calls.at(-1)?.[0] as {
			externalAction?: { dismissLabel?: string };
		};
		expect(prompt.externalAction?.dismissLabel).toBe('Not now');
		const [url, init] = timedFetch.mock.calls.at(-1)!;
		expect(url).toBe('/api/magician/v2/hitl/mcp_oauth%3Abinding%3Aflow%3Aattempt/respond');
		expect(JSON.parse(String(init?.body))).toMatchObject({
			source: 'mcp_oauth',
			value: { type: 'aborted' }
		});
	});

	it('offers "Not now" on sign-in asks published before the label existed', async () => {
		const request = mcpOauthEvent()!;
		requestAttentionInput.mockResolvedValueOnce(null);
		await respondToHitl(request);
		const prompt = requestAttentionInput.mock.calls.at(-1)?.[0] as {
			externalAction?: { dismissLabel?: string };
		};
		expect(prompt.externalAction?.dismissLabel).toBe('Not now');
	});

	it('treats a flow that already finished as resolved', async () => {
		timedFetch.mockResolvedValueOnce(
			new Response(JSON.stringify({ accepted: false, reason: 'mcp_oauth_flow_not_live' }), { status: 409 })
		);
		await expect(
			postHitlResponse(mcpOauthEvent()!, { type: 'external_action_completed' })
		).resolves.toEqual({ ok: true, alreadyClosed: true });
	});
});

describe('delegated child asks (C2)', () => {
	const childItem = {
		id: 'v3:attention:child-1',
		principal: 'alice',
		workspace: 'work',
		item_type: 'task',
		task_id: 'parent-task',
		status: 'needs_action',
		title: 'Input requested',
		summary: 'Which quarter?',
		created_at: 1,
		updated_at: 2,
		actions: [],
		metadata: {
			attention_kind: 'input.requested',
			source: 'agentic',
			pause_state_id: 'pause-child',
			execution_id: 'exec-child',
			input_type: 'form',
			input_schema: {
				type: 'form',
				questions: [
					{ id: 'q1', prompt: 'Which quarter?', input_type: 'text' },
					{ id: 'q2', prompt: 'Which region?', input_type: 'choice', options: [{ id: 'eu', label: 'EU' }] }
				]
			},
			delegation_child: true,
			delegated_agent_id: 'researcher'
		}
	} as unknown as Parameters<typeof hitlRequestFromFeedItem>[0];

	it('labels the ask with the delegated agent and posts against the child execution', () => {
		const request = hitlRequestFromFeedItem(childItem)!;
		expect(request.delegated_agent_id).toBe('researcher');
		expect(request.scope.task_id).toBe('parent-task');
		const call = resolveHitlCall(request, {
			type: 'form',
			answers: [
				{ id: 'q1', value: 'Q3' },
				{ id: 'q2', selected_ids: ['eu'] }
			]
		})!;
		expect(call.url).toBe('/api/magician/v2/hitl/pause-child/respond');
		expect(call.body).toMatchObject({
			source: 'agentic',
			execution_id: 'exec-child',
			value: { type: 'form' }
		});
	});

	it('prefers the embedded request execution over the row when both are stamped', () => {
		const request = hitlRequestFromFeedItem({
			...childItem,
			metadata: {
				...(childItem.metadata as Record<string, unknown>),
				execution_id: 'exec-parent',
				hitl_request: { scope: { execution_id: 'exec-child' } }
			}
		})!;
		expect(request.scope.execution_id).toBe('exec-child');
	});
});
