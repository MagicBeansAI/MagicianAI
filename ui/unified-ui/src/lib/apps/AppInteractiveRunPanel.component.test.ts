import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import AppInteractiveRunPanel from './AppInteractiveRunPanel.svelte';

const RUN = 'run:app-action:fixture';
const SESSION = `interactive-session:${'a'.repeat(64)}`;
const TARGET = `interactive-target:${'b'.repeat(64)}`;
const AUDIT_SESSION = `interactive-session:${'e'.repeat(64)}`;

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

describe('AppInteractiveRunPanel', () => {
	it('shows unified owner availability, sanitized target, audit, and declared stop truth', async () => {
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
			run_ref: RUN,
			installation_id: 'install_fixture',
			installation_generation: 4,
			owner_availability: [
				{ profile: 'browser_session', state: 'stopping' },
				{ profile: 'macos_host', state: 'unavailable', unavailable_reason: 'not_declared' },
				{ profile: 'android_device', state: 'conditional' }
			],
			current_session: {
				session_ref: SESSION,
				profile: 'browser_session',
				target_ref: TARGET,
				target_summary: { target_ref: TARGET, kind: 'isolated_browser' },
				activity: 'observe',
				phase: 'stop_requested',
				started_at: '2026-08-23T10:00:00Z',
				expires_at: '2026-08-23T10:01:00Z',
				resource_claim: { evidence_bytes: 128, evidence_nodes: 4, pixels: 0, artifact_bytes: 0, output_bytes: 256 }
			},
			recent_audit_receipts: [{
				receipt_ref: `interactive-audit:${'c'.repeat(64)}`,
				session_ref: AUDIT_SESSION,
				activity: 'observe',
				sequence: 1,
				terminal: 'outcome_uncertain',
				result_bytes: 10,
				evidence_bytes: 20
			}],
			declared_stop_state: {
				phase: 'requested',
				session_ref: SESSION,
				stop_ref: `interactive-stop:${'d'.repeat(64)}`,
				requested_at: '2026-08-23T10:00:01Z'
			},
			stop_available: false,
			captured_content_included: false
		}), { status: 200, headers: { 'content-type': 'application/json' } })));

		const { container } = render(AppInteractiveRunPanel, { runRef: RUN, runTerminal: false });
		expect(await screen.findByLabelText('Interactive owner availability')).toBeInTheDocument();
		expect(screen.getByText('Browser session')).toBeInTheDocument();
		expect(screen.getByText('Android application')).toBeInTheDocument();
		expect(screen.getByText(/Isolated browser/)).toBeInTheDocument();
		expect(screen.getByText(/Declared stop:/)).toBeInTheDocument();
		expect(screen.getByText(/Recent settlement audit/)).toBeInTheDocument();
		expect(container.textContent).not.toMatch(/bundle_id|device_id|cdp|argv|permission evidence/i);
		expect(screen.queryByRole('button', { name: 'Request stop' })).not.toBeInTheDocument();
	});
});
