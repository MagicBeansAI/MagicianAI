import { afterEach, describe, expect, it, vi } from 'vitest';
import { parseAppInteractiveRunState, requestAppInteractiveStop } from './appInteractiveState';

const RUN = 'run:app-action:fixture';
const SESSION = `interactive-session:${'a'.repeat(64)}`;
const TARGET = `interactive-target:${'b'.repeat(64)}`;
const DIGEST = `blake3:${'c'.repeat(64)}`;

function fixture(): Record<string, unknown> {
	return {
		run_ref: RUN,
		installation_id: 'install_fixture',
		installation_generation: 4,
		owner_availability: [
			{ profile: 'browser_session', state: 'active' },
			{ profile: 'macos_host', state: 'unavailable', unavailable_reason: 'not_declared' },
			{ profile: 'android_device', state: 'conditional' }
		],
		current_session: {
			session_ref: SESSION,
			profile: 'browser_session',
			target_ref: TARGET,
			target_summary: { target_ref: TARGET, kind: 'isolated_browser' },
			activity: 'observe',
			phase: 'active',
			started_at: '2026-08-23T10:00:00Z',
			expires_at: '2026-08-23T10:01:00Z',
			resource_claim: { evidence_bytes: 128, evidence_nodes: 4, pixels: 0, artifact_bytes: 0, output_bytes: 256 }
		},
		recent_audit_receipts: [],
		declared_stop_state: { phase: 'available' },
		stop_available: true,
		captured_content_included: false
	};
}

describe('app interactive run state', () => {
	afterEach(() => vi.unstubAllGlobals());

	it('accepts only the correlated payload-free current session', () => {
		const state = parseAppInteractiveRunState(fixture(), RUN);
		expect(state.currentSession?.profile).toBe('browser_session');
		expect(state.currentSession?.targetSummary.kind).toBe('isolated_browser');
		expect(state.currentSession?.resourceClaim.evidenceNodes).toBe(4);
		expect(state.ownerAvailability.map(({ profile, state }) => [profile, state])).toEqual([
			['browser_session', 'active'],
			['macos_host', 'unavailable'],
			['android_device', 'conditional']
		]);
		expect(state.declaredStopState.phase).toBe('available');
		expect(state.capturedContentIncluded).toBe(false);
	});

	it('rejects run substitution, disclosure flags, and terminal-active contradictions', () => {
		const substituted = fixture();
		substituted.run_ref = 'run:app-action:other';
		expect(() => parseAppInteractiveRunState(substituted, RUN)).toThrow(/correlation/i);

		const captured = fixture();
		captured.captured_content_included = true;
		expect(() => parseAppInteractiveRunState(captured, RUN)).toThrow(/disclosure/i);

		const terminal = fixture();
		terminal.recent_audit_receipts = [{
			receipt_ref: `interactive-audit:${'d'.repeat(64)}`, session_ref: SESSION,
			activity: 'observe', sequence: 1, terminal: 'completed', result_bytes: 10, evidence_bytes: 20
		}];
		expect(() => parseAppInteractiveRunState(terminal, RUN)).toThrow(/terminal receipt/i);

		const targetSubstitution = fixture();
		(targetSubstitution.current_session as Record<string, unknown>).target_summary = {
			target_ref: `interactive-target:${'f'.repeat(64)}`,
			kind: 'isolated_browser'
		};
		expect(() => parseAppInteractiveRunState(targetSubstitution, RUN)).toThrow(/target summary correlation/i);

		const falseStop = fixture();
		falseStop.declared_stop_state = { phase: 'requested' };
		falseStop.stop_available = false;
		expect(() => parseAppInteractiveRunState(falseStop, RUN)).toThrow(/declared stop state/i);

		const stopSubstitution = fixture();
		stopSubstitution.declared_stop_state = {
			phase: 'requested',
			session_ref: `interactive-session:${'f'.repeat(64)}`,
			stop_ref: `interactive-stop:${'e'.repeat(64)}`,
			requested_at: '2026-08-23T10:00:01Z'
		};
		stopSubstitution.stop_available = false;
		expect(() => parseAppInteractiveRunState(stopSubstitution, RUN)).toThrow(/declared stop state/i);
	});

	it('requires the exact durable stop receipt correlation', async () => {
		const key = 'interactive-stop-request:fixture';
		const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({
			schema: 'magician.app-interactive-stop.v1', run_ref: RUN, session_ref: SESSION,
			stop_ref: `interactive-stop:${'e'.repeat(64)}`, idempotency_key: key,
			requested_at: '2026-08-23T10:00:01Z', phase: 'stop_requested', receipt_digest: DIGEST
		}), { status: 200, headers: { 'content-type': 'application/json' } }));
		vi.stubGlobal('fetch', fetchMock);

		const receipt = await requestAppInteractiveStop(RUN, SESSION, key);
		expect(receipt.phase).toBe('stop_requested');
		const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
		expect(JSON.parse(String(init.body))).toEqual({ expected_session_ref: SESSION, idempotency_key: key });
	});
});
