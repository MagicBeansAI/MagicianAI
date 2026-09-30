import { get } from 'svelte/store';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { voiceCallStore } from './realtimeVoiceClient';
import {
	setWakeEnabled,
	stopWakeWord,
	wakeStatusStore
} from './wakeWord';

vi.mock('vosk-browser', () => ({
	createModel: vi.fn(async () => ({
		KaldiRecognizer: class {
			on(): void {}
			acceptWaveform(): boolean { return true; }
			remove(): void {}
		}
	}))
}));

const originalCreateObjectURL = URL.createObjectURL;
const originalRevokeObjectURL = URL.revokeObjectURL;

function callState(state: 'idle' | 'connecting') {
	return {
		state,
		error: null,
		model: null,
		voice: null,
		connectedAt: null,
		activationPhrase: null,
		activationPhrases: [] as string[],
		addressingRequired: false,
		boundary: null
	} as const;
}

describe('wake-word microphone ownership', () => {
	afterEach(async () => {
		voiceCallStore.set(callState('idle'));
		setWakeEnabled(false);
		await stopWakeWord();
		vi.useRealTimers();
		vi.unstubAllGlobals();
		Object.defineProperty(URL, 'createObjectURL', {
			configurable: true,
			value: originalCreateObjectURL
		});
		Object.defineProperty(URL, 'revokeObjectURL', {
			configurable: true,
			value: originalRevokeObjectURL
		});
	});

	it('cancels a late wake stream and resumes listening after the call', async () => {
		vi.useFakeTimers();
		localStorage.setItem('magician.voice.wakeFsVersion', '2');

		class TestWorker {
			onmessage: (() => void) | null = null;
			onerror: (() => void) | null = null;
			constructor() {
				queueMicrotask(() => this.onmessage?.());
			}
			terminate(): void {}
		}
		vi.stubGlobal('Worker', TestWorker);
		Object.defineProperty(URL, 'createObjectURL', {
			configurable: true,
			value: vi.fn(() => 'blob:wake-probe')
		});
		Object.defineProperty(URL, 'revokeObjectURL', {
			configurable: true,
			value: vi.fn()
		});

		let resolveFirstMic!: (stream: MediaStream) => void;
		const firstMic = new Promise<MediaStream>((resolve) => { resolveFirstMic = resolve; });
		const firstTrack = { stop: vi.fn() };
		const secondTrack = { stop: vi.fn() };
		const firstStream = { getTracks: () => [firstTrack] } as unknown as MediaStream;
		const secondStream = { getTracks: () => [secondTrack] } as unknown as MediaStream;
		const getUserMedia = vi
			.fn<() => Promise<MediaStream>>()
			.mockReturnValueOnce(firstMic)
			.mockResolvedValueOnce(secondStream);
		Object.defineProperty(navigator, 'mediaDevices', {
			configurable: true,
			value: { getUserMedia }
		});

		const node = { connect: vi.fn(), disconnect: vi.fn() };
		class TestAudioContext {
			sampleRate = 48_000;
			destination = {} as AudioDestinationNode;
			createMediaStreamSource(): MediaStreamAudioSourceNode { return node as unknown as MediaStreamAudioSourceNode; }
			createScriptProcessor(): ScriptProcessorNode {
				return { ...node, onaudioprocess: null } as unknown as ScriptProcessorNode;
			}
			createGain(): GainNode {
				return { ...node, gain: { value: 1 } } as unknown as GainNode;
			}
			close = vi.fn(async () => undefined);
		}
		vi.stubGlobal('AudioContext', TestAudioContext);

		setWakeEnabled(true);
		await vi.waitFor(() => expect(getUserMedia).toHaveBeenCalledOnce());

		voiceCallStore.set(callState('connecting'));
		resolveFirstMic(firstStream);
		await vi.runAllTicks();

		expect(firstTrack.stop).toHaveBeenCalledOnce();
		await vi.waitFor(() => expect(get(wakeStatusStore)).toBe('paused'));

		voiceCallStore.set(callState('idle'));
		await vi.advanceTimersByTimeAsync(2_500);
		await vi.waitFor(() => expect(getUserMedia).toHaveBeenCalledTimes(2));
		await vi.runAllTicks();

		expect(get(wakeStatusStore)).toBe('listening');
		expect(secondTrack.stop).not.toHaveBeenCalled();
	});
});
