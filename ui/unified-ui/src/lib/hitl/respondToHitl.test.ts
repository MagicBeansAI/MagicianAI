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
import { describe, it, expect, vi, beforeEach } from 'vitest';

// Capture the timedFetch invocation and control the Response it resolves to.
const timedFetch = vi.fn();
vi.mock('$lib/shared/fetch', () => ({
	timedFetch: (input: RequestInfo | URL, init?: RequestInit & { timeoutMs?: number }) =>
		timedFetch(input, init),
	DEFAULT_FETCH_TIMEOUT_MS: 30_000,
	LONG_FETCH_TIMEOUT_MS: 600_000
}));

const requestAttentionInput = vi.fn();
vi.mock('$lib/stores/attentionPromptStore', () => ({
	requestAttentionInput: (request: unknown) => requestAttentionInput(request)
}));

import { postHitlResponse, hitlRequestFromCanonicalEvent, hitlRequestFromFeedItem, resolveHitlCall } from './adapters';
import { respondToHitl } from './respondToHitl';
import type { HitlInputType, HitlRequest, HitlResponseValue } from './types';

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
	});

	it('(a) treats a 404 "Pause state not found" as a soft success (dismiss)', async () => {
		timedFetch.mockResolvedValue(jsonResponse(404, { reason: 'Pause state not found' }));

		const outcome = await postHitlResponse(agenticTextRequest(), textValue);

		expect(outcome).toEqual({ ok: true });
	});

	it('treats 410 pause_state_gone as a soft success (dismiss)', async () => {
		timedFetch.mockResolvedValue(
			jsonResponse(410, {
				code: 'pause_state_gone',
				message: 'Pause state was already resolved or expired'
			})
		);

		const outcome = await postHitlResponse(agenticTextRequest(), textValue);

		expect(outcome).toEqual({ ok: true });
	});

	it('(b) treats a 409 already_resolved as a soft success (dismiss)', async () => {
		timedFetch.mockResolvedValue(jsonResponse(409, { reason: 'already_resolved' }));

		const outcome = await postHitlResponse(agenticTextRequest(), textValue);

		expect(outcome).toEqual({ ok: true });
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

		expect(outcome).toEqual({ ok: true });
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
