import { describe, expect, it } from 'vitest';

import {
	activationNamesForAgent,
	admitAddressedTranscript,
	constrainedWakeNamesForAgent,
	decideAddressedTranscript,
	type VoiceAddressingConfig
} from './voiceAddressing';

const required: VoiceAddressingConfig = {
	required: true,
	activation_phrases: ['Hey Nova', 'Hey Sam']
};

describe('voice addressing', () => {
	it('strips an addressed prefix without leaking it into the command', () => {
		expect(admitAddressedTranscript('HEY, Nova: send the update.', required)).toBe(
			'send the update.'
		);
	});

	it('accepts aliases as complete tokens', () => {
		expect(admitAddressedTranscript('Hey Sam, what is next?', required)).toBe('what is next?');
		expect(admitAddressedTranscript('Hey Samantha, what is next?', required)).toBeNull();
	});

	it('rejects ambient and mid-sentence address phrases', () => {
		expect(admitAddressedTranscript('Please send the update.', required)).toBeNull();
		expect(admitAddressedTranscript('I said hey Sam, send it.', required)).toBeNull();
	});

	it('arms one follow-up utterance when the address phrase is finalized alone', () => {
		const armed = decideAddressedTranscript('Hey Nova!', required, 0, 1_000);
		expect(armed).toEqual({ kind: 'armed', text: null, armedUntilMs: 9_000 });
		expect(decideAddressedTranscript('send the update', required, armed.armedUntilMs, 2_000)).toEqual({
			kind: 'admitted',
			text: 'send the update',
			armedUntilMs: 0
		});
		expect(decideAddressedTranscript('too late', required, armed.armedUntilMs, 9_001).kind).toBe(
			'rejected'
		);
	});

	it('uses shipped wake spellings for constrained recognition without hiding server names', () => {
		const identity = {
			name: 'Magican',
			aliases: [] as string[],
			wake_spellings: [' magical ', 'magician', 'MAGICAL']
		};

		expect(constrainedWakeNamesForAgent(identity)).toEqual(['magical', 'magician']);
		expect(activationNamesForAgent(identity)).toEqual(['Magican', 'magical', 'magician']);
	});

	it('falls back to advertised names only when no wake spelling is configured', () => {
		const identity = { name: 'Atlas', aliases: ['Nova', 'HEY nova'] };
		expect(constrainedWakeNamesForAgent(identity)).toEqual(['Nova', 'Atlas']);
	});

	it('passes trimmed speech through when addressing is disabled', () => {
		expect(
			admitAddressedTranscript('  Talk to everyone in the room.  ', {
				required: false,
				activation_phrases: []
			})
		).toBe('Talk to everyone in the room.');
	});
});
