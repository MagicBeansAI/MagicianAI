import { fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

/**
 * Push-to-talk gesture wiring on the composer mic.
 *
 * The regression this pins: starting a capture inserts the elapsed-time readout
 * beside the button and swaps its glyph for the orb, which re-lays-out the
 * composer row and slides the button out from under a perfectly still finger.
 * That fired `pointerleave`, which used to cancel the hold — so a deliberate
 * long press produced a near-empty take and the "Recording is too short to
 * transcribe." error. A hold must survive the pointer leaving the button; only
 * `pointerup` (or a real `pointercancel`) may end it.
 */

const mic = vi.hoisted(() => ({
	stop: vi.fn(async () => ({
		blob: new Blob(['x'.repeat(4096)], { type: 'audio/webm' }),
		durationMs: 1200,
		mimeType: 'audio/webm'
	})),
	cancel: vi.fn(),
	startMicRecording: vi.fn()
}));

vi.mock('$lib/media/capture/mic', () => ({
	isMicCaptureSupported: () => true,
	makeRecordedAudioFile: () => new File(['x'], 'take.webm', { type: 'audio/webm' }),
	startMicRecording: mic.startMicRecording
}));

vi.mock('$lib/media/session', () => ({
	publishMediaEvent: vi.fn(async () => undefined),
	updateMediaSessionPermissions: vi.fn(async () => undefined)
}));

vi.mock('$lib/media/stt/sttClient', () => ({
	transcribeAudioStreaming: vi.fn(async () => ({ transcript: 'hello there' }))
}));

import MicCaptureButton from './MicCaptureButton.svelte';

const HOLD_MS = 300; // comfortably past the 250ms hold threshold

function micButton(): HTMLElement {
	return screen.getByRole('button', { name: /Record a voice note/i });
}

describe('MicCaptureButton push-to-talk', () => {
	beforeEach(() => {
		vi.useFakeTimers();
		mic.stop.mockClear();
		mic.cancel.mockClear();
		mic.startMicRecording.mockReset();
		mic.startMicRecording.mockImplementation(async () => ({
			stop: mic.stop,
			cancel: mic.cancel,
			analyser: null
		}));
	});

	afterEach(() => {
		vi.useRealTimers();
	});

	it('starts capturing once the press passes the hold threshold', async () => {
		render(MicCaptureButton, { props: { compact: true } });
		const btn = micButton();

		await fireEvent.pointerDown(btn, { pointerId: 1, button: 0 });
		expect(mic.startMicRecording).not.toHaveBeenCalled(); // not yet a hold

		await vi.advanceTimersByTimeAsync(HOLD_MS);
		expect(mic.startMicRecording).toHaveBeenCalledOnce();
	});

	it('keeps capturing when the pointer leaves the button mid-hold', async () => {
		render(MicCaptureButton, { props: { compact: true } });
		const btn = micButton();

		await fireEvent.pointerDown(btn, { pointerId: 1, button: 0 });
		await vi.advanceTimersByTimeAsync(HOLD_MS);
		expect(mic.startMicRecording).toHaveBeenCalledOnce();

		// The row reflows under the finger — this must NOT end the take.
		await fireEvent.pointerLeave(btn, { pointerId: 1 });
		await vi.advanceTimersByTimeAsync(50);
		expect(mic.stop).not.toHaveBeenCalled();

		// Releasing still ends it, even though the pointer had left.
		await fireEvent.pointerUp(btn, { pointerId: 1 });
		await vi.advanceTimersByTimeAsync(50);
		expect(mic.stop).toHaveBeenCalledOnce();
	});

	it('does not start a capture for a quick tap below the threshold', async () => {
		render(MicCaptureButton, { props: { compact: true } });
		const btn = micButton();

		await fireEvent.pointerDown(btn, { pointerId: 1, button: 0 });
		await vi.advanceTimersByTimeAsync(80);
		await fireEvent.pointerUp(btn, { pointerId: 1 });
		await vi.advanceTimersByTimeAsync(20);

		// The tap path runs through the native click handler, not the hold.
		expect(mic.startMicRecording).not.toHaveBeenCalled();
	});
});
