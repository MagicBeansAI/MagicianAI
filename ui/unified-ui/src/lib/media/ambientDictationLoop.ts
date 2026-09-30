/**
 * Transport-agnostic orchestration for one Ambient Dictation conversation.
 *
 * The UI owns capture/STT/chat/TTS implementations and phase presentation;
 * this function owns the ordering and cancellation fences. Keeping one
 * explicit `while` loop here makes stack safety and late-result behavior
 * deterministic and independently testable.
 */
export interface AmbientDictationLoopPorts<Audio, Reply> {
	sessionId: string;
	isActive(): boolean;
	/**
	 * Optional admission boundary before each bounded capture. Ambient Orb uses
	 * this to require a fresh wake phrase (or explicit tap) for every turn;
	 * legacy callers that omit it retain immediate iterative capture.
	 */
	waitForActivation?(): Promise<boolean>;
	capture(): Promise<Audio | null>;
	transcribe(audio: Audio): Promise<string | null>;
	send(sessionId: string, text: string): Promise<Reply | null>;
	speak(reply: Reply): Promise<void>;
}

export type AmbientDictationLoopOutcome =
	| 'cancelled_or_no_input'
	| 'reply_unavailable';

export async function runAmbientDictationTurns<Audio, Reply>(
	ports: AmbientDictationLoopPorts<Audio, Reply>
): Promise<AmbientDictationLoopOutcome> {
	while (ports.isActive()) {
		const admitted = ports.waitForActivation
			? await ports.waitForActivation()
			: true;
		if (!admitted || !ports.isActive()) return 'cancelled_or_no_input';

		const audio = await ports.capture();
		if (!audio || !ports.isActive()) return 'cancelled_or_no_input';

		const text = await ports.transcribe(audio);
		if (!text || !ports.isActive()) return 'cancelled_or_no_input';

		const reply = await ports.send(ports.sessionId, text);
		if (!ports.isActive()) return 'cancelled_or_no_input';
		if (!reply) return 'reply_unavailable';

		await ports.speak(reply);
		if (!ports.isActive()) return 'cancelled_or_no_input';
	}
	return 'cancelled_or_no_input';
}
