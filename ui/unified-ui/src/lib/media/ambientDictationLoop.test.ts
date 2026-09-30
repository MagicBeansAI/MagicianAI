import { describe, expect, it } from 'vitest';

import { runAmbientDictationTurns } from './ambientDictationLoop';

describe('Ambient Dictation turn loop', () => {
	it('pins every follow-up to the session captured at admission', async () => {
		let active = true;
		const captured = ['audio-1', 'audio-2'];
		const sent: Array<{ sessionId: string; text: string }> = [];
		const spoken: string[] = [];

		const outcome = await runAmbientDictationTurns({
			sessionId: 'session-at-start',
			isActive: () => active,
			capture: async () => captured.shift() ?? null,
			transcribe: async (audio) => `text-for-${audio}`,
			send: async (sessionId, text) => {
				sent.push({ sessionId, text });
				return `reply-${sent.length}`;
			},
			speak: async (reply) => {
				spoken.push(reply);
				if (spoken.length === 2) active = false;
			}
		});

		expect(outcome).toBe('cancelled_or_no_input');
		expect(sent).toEqual([
			{ sessionId: 'session-at-start', text: 'text-for-audio-1' },
			{ sessionId: 'session-at-start', text: 'text-for-audio-2' }
		]);
		expect(spoken).toEqual(['reply-1', 'reply-2']);
	});

	it('drops a deliberately late agent reply after cancellation', async () => {
		let active = true;
		let spoke = false;

		const outcome = await runAmbientDictationTurns({
			sessionId: 'fixed-session',
			isActive: () => active,
			capture: async () => 'audio',
			transcribe: async () => 'hello',
			send: async () => {
				active = false;
				return 'late reply';
			},
			speak: async () => {
				spoke = true;
			}
		});

		expect(outcome).toBe('cancelled_or_no_input');
		expect(spoke).toBe(false);
	});

	it('requires a fresh activation before each bounded capture when gated', async () => {
		let active = true;
		let permits = 2;
		let captures = 0;
		const order: string[] = [];

		const outcome = await runAmbientDictationTurns({
			sessionId: 'wake-gated-session',
			isActive: () => active,
			waitForActivation: async () => {
				order.push('activation');
				return permits-- > 0;
			},
			capture: async () => {
				captures += 1;
				order.push('capture');
				return `audio-${captures}`;
			},
			transcribe: async () => 'hello',
			send: async () => 'reply',
			speak: async () => {
				order.push('speak');
			}
		});

		expect(outcome).toBe('cancelled_or_no_input');
		expect(captures).toBe(2);
		expect(order).toEqual([
			'activation', 'capture', 'speak',
			'activation', 'capture', 'speak',
			'activation'
		]);
	});

	it('does not transcribe a capture completed after cancellation', async () => {
		let active = true;
		let transcribed = false;

		const outcome = await runAmbientDictationTurns({
			sessionId: 'fixed-session',
			isActive: () => active,
			capture: async () => {
				active = false;
				return 'late audio';
			},
			transcribe: async () => {
				transcribed = true;
				return 'should not happen';
			},
			send: async () => 'reply',
			speak: async () => {}
		});

		expect(outcome).toBe('cancelled_or_no_input');
		expect(transcribed).toBe(false);
	});

	it('stops explicitly when chat cannot return a reply', async () => {
		let spoke = false;
		const outcome = await runAmbientDictationTurns({
			sessionId: 'fixed-session',
			isActive: () => true,
			capture: async () => 'audio',
			transcribe: async () => 'hello',
			send: async () => null,
			speak: async () => {
				spoke = true;
			}
		});

		expect(outcome).toBe('reply_unavailable');
		expect(spoke).toBe(false);
	});

	it('keeps constant call-stack shape across a long conversation', async () => {
		let turns = 0;
		const outcome = await runAmbientDictationTurns({
			sessionId: 'long-session',
			isActive: () => turns < 10_000,
			capture: async () => 'audio',
			transcribe: async () => 'next',
			send: async () => 'reply',
			speak: async () => {
				turns += 1;
			}
		});

		expect(outcome).toBe('cancelled_or_no_input');
		expect(turns).toBe(10_000);
	});
});
