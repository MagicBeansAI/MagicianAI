import { describe, expect, it, vi } from 'vitest';

import {
	captureVoiceGuidedFlowScreen,
	discardVoiceGuidedFlowCapture,
	lockedVoiceGuidedFlowMessage,
	parseVoiceGuidedFlowInvocation,
	voiceGuidedFlowCaptureStillCurrent
} from './guidedFlow';

describe('voice guided-flow commands', () => {
	it('uses feature-specific locked-screen speech', () => {
		expect(lockedVoiceGuidedFlowMessage('tutor'))
			.toBe('Please unlock your screen to use Tutor.');
		expect(lockedVoiceGuidedFlowMessage('app_copilot'))
			.toBe('Please unlock your screen to use App Copilot.');
	});

	it.each([
		['Tutor explain recursion', '@tutor explain recursion', 'blackboard', false, false],
		['Hey Tutor blackboard explain recursion', '@tutor blackboard explain recursion', 'blackboard', false, false],
		['Tutor Quick blackboard explain recursion', '@tutor #quick blackboard explain recursion', 'blackboard', true, false],
		['Quick Tutor explain recursion', '@tutor #quick explain recursion', 'blackboard', true, false],
		['Tutor screen explain this graph', '@tutor screen explain this graph', 'screen_overlay', false, true],
		['Start Tutor Quick screen explain this graph', '@tutor #quick screen explain this graph', 'screen_overlay', true, true],
		['Hey Tutor take a screenshot and explain the error', '@tutor take a screenshot and explain the error', 'screen_overlay', false, true],
		['App Copilot show me how to create a note', '@copilot show me how to create a note', 'screen_overlay', false, true]
	] as const)('normalizes %s', (input, normalizedText, canvas, quick, capture) => {
		expect(parseVoiceGuidedFlowInvocation(input)).toEqual({
			feature: normalizedText.startsWith('@copilot') ? 'app_copilot' : 'tutor',
			canvas,
			quick,
			normalizedText,
			requiresScreenCapture: capture
		});
	});

	it('lets an explicit blackboard request override incidental screen wording', () => {
		expect(parseVoiceGuidedFlowInvocation('Tutor blackboard explain what a screen reader does'))
			.toMatchObject({ canvas: 'blackboard', requiresScreenCapture: false });
		expect(parseVoiceGuidedFlowInvocation('App Copilot blackboard show me how to create a note'))
			.toMatchObject({ canvas: 'screen_overlay', requiresScreenCapture: true });
	});

	it.each([
		'Tutor explain my app',
		'Tutor explain the current window',
		'Tutor explain why the screenshot is blurry'
	])('does not infer screen capture from question wording: %s', (text) => {
		expect(parseVoiceGuidedFlowInvocation(text))
			.toMatchObject({ canvas: 'blackboard', requiresScreenCapture: false });
	});

	it('keeps the command-prefix source authoritative over later topic words', () => {
		expect(parseVoiceGuidedFlowInvocation('Tutor screen explain the blackboard controls'))
			.toMatchObject({ canvas: 'screen_overlay', requiresScreenCapture: true });
		expect(parseVoiceGuidedFlowInvocation('Tutor blackboard explain how screenshots work'))
			.toMatchObject({ canvas: 'blackboard', requiresScreenCapture: false });
	});

	it.each([
		'Can you compare tutor products?',
		'I mentioned app copilot later in this sentence',
		'Please ask hey tutor to explain this'
	])('does not authorize incidental text: %s', (text) => {
		expect(parseVoiceGuidedFlowInvocation(text)).toBeNull();
	});
});

describe('voice guided-flow screen capture', () => {
	const scope = { principal: 'owner-1', workspace: 'workspace-1' };

	it('rejects capture completion after the session or draft changes', () => {
		expect(voiceGuidedFlowCaptureStillCurrent(
			'chat-1',
			'Tutor screen explain this',
			'chat-1',
			'Tutor screen explain this'
		)).toBe(true);
		expect(voiceGuidedFlowCaptureStillCurrent(
			'chat-1',
			'Tutor screen explain this',
			'chat-2',
			'Tutor screen explain this'
		)).toBe(false);
		expect(voiceGuidedFlowCaptureStillCurrent(
			'chat-1',
			'Tutor screen explain this',
			'chat-1',
			'Tutor screen explain that'
		)).toBe(false);
	});

	it('stages a server-attested screenshot into the active chat session', async () => {
		const fetcher = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) => new Response(JSON.stringify({
			attachments: [{
				attachment_id: 'attachment-1',
				stored_name: 'screen.png',
				mime_type: 'image/png',
				size_bytes: 42
			}]
		}), { status: 200 }));

		await expect(captureVoiceGuidedFlowScreen('chat-1', scope, fetcher as typeof fetch)).resolves.toEqual([
			{
				attachment_id: 'attachment-1',
				filename: 'screen.png',
				mime_type: 'image/png',
				size: 42,
				label: 'voice screen capture',
				server_registered_capture: true
			}
		]);
		expect(fetcher).toHaveBeenCalledWith('/api/magician/v2/screen/capture', expect.objectContaining({
			method: 'POST',
			body: JSON.stringify({ mode: 'screenshot', session_id: 'chat-1' })
		}));
		const captureHeaders = new Headers(fetcher.mock.calls[0]?.[1]?.headers);
		expect(captureHeaders.get('X-Principal')).toBeNull();
		expect(captureHeaders.get('X-Workspace')).toBeNull();
	});

	it.each([
		[new Response(JSON.stringify({ cancelled: true }), { status: 200 }), 'cancelled'],
		[new Response(JSON.stringify({ attachments: [] }), { status: 200 }), 'usable image'],
		[new Response('permission denied', { status: 500 }), 'permission denied']
	] as const)('fails closed when capture cannot be staged', async (response, message) => {
		const fetcher = vi.fn(async () => response);
		await expect(captureVoiceGuidedFlowScreen('chat-1', scope, fetcher as typeof fetch))
			.rejects.toThrow(message);
	});

	it('discards a server-staged capture when its session ownership changes', async () => {
		const fetcher = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) => new Response(JSON.stringify({
			discarded_attachment_ids: ['attachment-1'],
			retained_attachment_ids: []
		}), { status: 200 }));
		const attachment = {
			attachment_id: 'attachment-1',
			filename: 'screen.png',
			mime_type: 'image/png',
			size: 42,
			label: 'voice screen capture',
			server_registered_capture: true as const
		};

		await discardVoiceGuidedFlowCapture('chat-1', [attachment], scope, fetcher as typeof fetch);
		expect(fetcher).toHaveBeenCalledWith('/api/magician/v2/screen/capture/discard', expect.objectContaining({
			method: 'POST',
			keepalive: true,
			body: JSON.stringify({ session_id: 'chat-1', attachment_ids: ['attachment-1'] })
		}));
		const discardHeaders = new Headers(fetcher.mock.calls[0]?.[1]?.headers);
		expect(discardHeaders.get('X-Principal')).toBeNull();
		expect(discardHeaders.get('X-Workspace')).toBeNull();
	});

	it('fails closed when the backend retains a capture that is already in use', async () => {
		const fetcher = vi.fn(async () => new Response(JSON.stringify({
			discarded_attachment_ids: [],
			retained_attachment_ids: ['attachment-1']
		}), { status: 200 }));

		await expect(discardVoiceGuidedFlowCapture('chat-1', [{
			attachment_id: 'attachment-1',
			filename: 'screen.png',
			mime_type: 'image/png',
			size: 42,
			label: 'voice screen capture',
			server_registered_capture: true
		}], scope, fetcher as typeof fetch)).rejects.toThrow('already in use');
	});
});
