import { describe, expect, it } from 'vitest';

import { isVoiceNotePath } from './voiceNotePath';

describe('isVoiceNotePath', () => {
	it('marks the Audio Notes folder and the transcripts inside it', () => {
		expect(isVoiceNotePath('Audio Notes')).toBe(true);
		expect(isVoiceNotePath('Audio Notes/2026-08-01/note.md')).toBe(true);
		expect(isVoiceNotePath('Inbox/note.md')).toBe(false);
	});
});
