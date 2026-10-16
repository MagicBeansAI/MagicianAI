export interface GaplessAudioSlot {
	startAt: number;
	endAt: number;
}

/**
 * Build one contiguous AudioContext timeline. `AudioBufferSourceNode.start`
 * accepts absolute context time, so sources scheduled from these slots do not
 * cross the JavaScript event loop between clips.
 */
export function planGaplessAudio(
	durations: readonly number[],
	startAt: number
): GaplessAudioSlot[] {
	let cursor = startAt;
	return durations.map((duration) => {
		const safeDuration = Number.isFinite(duration) && duration > 0 ? duration : 0;
		const slot = { startAt: cursor, endAt: cursor + safeDuration };
		cursor = slot.endAt;
		return slot;
	});
}
