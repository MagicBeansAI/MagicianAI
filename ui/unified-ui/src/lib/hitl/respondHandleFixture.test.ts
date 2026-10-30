/**
 * Web side of the HITL respond-handle contract.
 *
 * `magician/tests/fixtures/hitl/attention_rows.json` holds one Attention row
 * per HITL kind exactly as `/api/magician/v2/feed/attention` serves it (built
 * and pinned by the Rust test `feed::hitl_respond_fixtures`; iOS and Android
 * read the same file). Every row must resolve to the call the server names in
 * `expect`: through its `metadata.respond` handle, through the direct-open
 * target path, and — for servers that predate the handle — through the
 * per-source fallback derivation with the handle stripped.
 */
import { describe, expect, it, vi } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const timedFetch = vi.fn();
vi.mock('$lib/shared/fetch', () => ({
	timedFetch: (input: RequestInfo | URL, init?: RequestInit & { timeoutMs?: number }) =>
		timedFetch(input, init),
	DEFAULT_FETCH_TIMEOUT_MS: 30_000,
	LONG_FETCH_TIMEOUT_MS: 600_000
}));

import type { FeedItem } from '$lib/feed/types';
import {
	hitlRequestFromCanonicalEvent,
	hitlRequestFromFeedItem,
	readHitlRespondHandle,
	resolveHitlCall
} from './adapters';
import { hitlOpenTargetFromFeedItem, hitlRequestFromOpenTarget } from '$lib/attention/openHitlPrompt';
import type { HitlResponseValue } from './types';
import { acknowledgeLiveRunNotice, parseLiveRun } from '$lib/stores/vibeDevLiveRunsStore';

interface FixtureRow {
	kind: string;
	answer: HitlResponseValue;
	expect: { url: string; body: Record<string, unknown> };
	row: FeedItem;
}

const FIXTURES = fileURLToPath(new URL('../../../../../magician/tests/fixtures/', import.meta.url));

function load<T>(name: string): T {
	return JSON.parse(readFileSync(`${FIXTURES}${name}`, 'utf8')) as T;
}

const rows = load<{ rows: FixtureRow[] }>('hitl/attention_rows.json').rows;

function expectCall(
	kind: string,
	call: ReturnType<typeof resolveHitlCall>,
	fixture: FixtureRow
): void {
	expect(call, `${kind}: resolves`).not.toBeNull();
	expect(call!.url, `${kind}: url`).toBe(fixture.expect.url);
	expect(call!.body, `${kind}: body`).toEqual({
		...fixture.expect.body,
		value: fixture.answer,
		channel: 'web'
	});
}

function withoutHandle(item: FeedItem): FeedItem {
	const metadata = structuredClone(item.metadata) as Record<string, unknown>;
	delete metadata.respond;
	const embedded = metadata.hitl_request as Record<string, unknown> | null | undefined;
	if (embedded && typeof embedded === 'object') delete embedded.respond;
	return { ...item, metadata };
}

describe('HITL respond-handle fixture (shared with Rust, iOS and Android)', () => {
	it('covers every HITL kind the clients answer', () => {
		expect(rows.map((row) => row.kind).sort()).toEqual(
			[
				'agentic_pause',
				'approval',
				'bot_auth',
				'clarification',
				'diff_approval',
				'mcp_oauth',
				'plan_approval',
				'service_health',
				'user_request_acknowledge',
				'user_request_choice'
			].sort()
		);
	});

	it.each(rows.map((row) => [row.kind, row] as const))(
		'%s: the served row posts the call its handle names',
		(kind, fixture) => {
			const handle = readHitlRespondHandle(
				(fixture.row.metadata as Record<string, unknown>).respond
			);
			expect(handle, `${kind}: handle parses`).not.toBeNull();
			const request = hitlRequestFromFeedItem(fixture.row);
			expect(request, `${kind}: request`).not.toBeNull();
			expect(request!.respond).toEqual(handle);
			expectCall(kind, resolveHitlCall(request!, fixture.answer), fixture);
		}
	);

	it.each(rows.map((row) => [row.kind, row] as const))(
		'%s: the direct-open target keeps the handle',
		(kind, fixture) => {
			const target = hitlOpenTargetFromFeedItem(fixture.row);
			expect(target, `${kind}: open target`).not.toBeNull();
			const request = hitlRequestFromOpenTarget(target!);
			expect(request, `${kind}: open request`).not.toBeNull();
			expectCall(kind, resolveHitlCall(request!, fixture.answer), fixture);
		}
	);

	it.each(rows.map((row) => [row.kind, row] as const))(
		'%s: an older server without the handle still resolves the same call',
		(kind, fixture) => {
			const request = hitlRequestFromFeedItem(withoutHandle(fixture.row));
			expect(request, `${kind}: fallback request`).not.toBeNull();
			expect(request!.respond).toBeUndefined();
			expectCall(kind, resolveHitlCall(request!, fixture.answer), fixture);
		}
	);

	it('a HitlRequested frame posts its own data.respond', () => {
		const request = hitlRequestFromCanonicalEvent({
			event_type: 'HitlRequested',
			data: {
				correlation_id: 'plan-7',
				source: 'plan_approval',
				input_type: 'confirmation',
				prompt: 'Approve the plan?',
				input_schema: {},
				task_id: 'task-4',
				execution_id: 'planexec-4',
				principal: 'owner',
				workspace: 'home',
				respond: {
					url: '/api/magician/v2/hitl/plan-7/respond',
					method: 'POST',
					correlation_id: 'plan-7',
					source: 'plan_approval',
					input_type: 'confirmation',
					execution_id: 'planexec-4',
					task_id: 'task-4',
					soft_success_statuses: [409]
				}
			}
		});
		expect(request?.respond?.task_id).toBe('task-4');
		const call = resolveHitlCall(request!, { type: 'confirmation', confirmed: true });
		expect(call).toEqual({
			url: '/api/magician/v2/hitl/plan-7/respond',
			method: 'POST',
			body: {
				source: 'plan_approval',
				input_type: 'confirmation',
				value: { type: 'confirmation', confirmed: true },
				channel: 'web',
				execution_id: 'planexec-4',
				task_id: 'task-4'
			},
			soft_success_statuses: [409]
		});
	});

	it('refuses a handle that points outside the respond route', () => {
		const base = {
			method: 'POST',
			correlation_id: 'x',
			source: 'user_request',
			input_type: 'choice',
			soft_success_statuses: [409]
		};
		expect(readHitlRespondHandle({ ...base, url: 'https://evil.example/hitl/x/respond' })).toBeNull();
		expect(readHitlRespondHandle({ ...base, url: '/api/magician/v2/other/x/respond' })).toBeNull();
		expect(readHitlRespondHandle({ ...base, url: '/api/magician/v2/hitl/x/respond', source: 'nope' })).toBeNull();
		expect(readHitlRespondHandle({ ...base, url: '/api/magician/v2/hitl/x/respond' })?.url).toBe(
			'/api/magician/v2/hitl/x/respond'
		);
	});

	it('a VibeDev live-run notice acknowledges through its handle', async () => {
		const live = load<{ runs: Record<string, unknown>[] }>('vibedev/live_runs.json');
		const briefed = live.runs.map(parseLiveRun).find((run) => run?.notice);
		const notice = briefed!.notice!;
		expect(notice.respond?.source).toBe('user_request');
		timedFetch.mockResolvedValueOnce(new Response('{}', { status: 200 }));
		const result = await acknowledgeLiveRunNotice(notice);
		expect(result.ok).toBe(true);
		const [url, init] = timedFetch.mock.calls.at(-1)!;
		expect(url).toBe(notice.respond!.url);
		expect(JSON.parse(String((init as RequestInit).body))).toEqual({
			source: 'user_request',
			input_type: 'choice',
			value: { type: 'choice', selected_id: 'acknowledge' },
			channel: 'web',
			execution_id: 'exec_d278'
		});
	});
});
