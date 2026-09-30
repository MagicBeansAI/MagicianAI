import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

const source = readFileSync(
	join(process.cwd(), 'src/lib/magician/chat/ChatPanel.svelte'),
	'utf8'
);

describe('ChatPanel voice guided-flow lifecycle contract', () => {
	it('engages an admission guard before awaiting lock permission', () => {
		const guard = source.indexOf('voiceGuidedAdmissionInFlight = true;');
		const permission = source.indexOf('await startScreenLockMonitoring(true);', guard);
		const upload = source.indexOf('voiceGuidedCaptureInFlight = true;', permission);

		expect(guard).toBeGreaterThan(-1);
		expect(permission).toBeGreaterThan(guard);
		expect(upload).toBeGreaterThan(permission);
		expect(source).toContain('voiceGuidedAdmissionGeneration !== admissionGeneration');
	});

	it('invalidates admission and voice provenance on every chat-scope reset', () => {
		const reset = source.slice(
			source.indexOf('function resetChatPageLocalState()'),
			source.indexOf('function beginComposerReferenceRequest()')
		);

		expect(reset).toContain('voiceComposerTurnOrigin = false;');
		expect(reset).toContain('voiceGuidedCaptureInFlight = false;');
		expect(reset).toContain('voiceGuidedCaptureGeneration += 1;');
		expect(reset).toContain('voiceGuidedAdmissionGeneration += 1;');
		expect(reset).toContain('voiceGuidedAdmissionInFlight = false;');
	});

	it('lets only the owning capture generation clear upload/send guards', () => {
		const capture = source.indexOf('const captureGeneration =');
		const finalizer = source.indexOf(
			'voiceGuidedCaptureGeneration === captureGeneration',
			capture
		);
		const sessionFence = source.indexOf('activeSessionId === sendSessionId', finalizer);

		expect(capture).toBeGreaterThan(-1);
		expect(finalizer).toBeGreaterThan(capture);
		expect(sessionFence).toBeGreaterThan(finalizer);
	});

	it('rechecks exact lock state after capture and discards a rejected capture', () => {
		const capture = source.indexOf('await captureVoiceGuidedFlowScreen');
		const refresh = source.indexOf('await startScreenLockMonitoring(false);', capture);
		const rejection = source.indexOf(
			'rejectLockedGuidedFlow(lockedGuidedFlowFeature, voiceOrigin);',
			refresh
		);
		const discard = source.indexOf('await discardVoiceGuidedFlowCapture', rejection);

		expect(capture).toBeGreaterThan(-1);
		expect(refresh).toBeGreaterThan(capture);
		expect(rejection).toBeGreaterThan(refresh);
		expect(discard).toBeGreaterThan(rejection);
	});

	it('reuses a staged server-attested image instead of stacking retry captures', () => {
		expect(source).toContain('const hasReusableGuidedCapture = pendingAttachments.some');
		expect(source).toContain(
			'guidedVoiceFlow?.requiresScreenCapture && !hasReusableGuidedCapture'
		);
	});
});
