/**
 * Pure maths for the deck's VOICE CORE.
 *
 * The deck's law applies here more than anywhere else: **no motion without
 * information.** Every number below is derived from a real `AnalyserNode`
 * reading of real audio. Nothing synthesises an envelope, and when there is no
 * audio the core rests — a still ring is honest, a writhing one is a lie about
 * a microphone that is not open.
 *
 * Kept framework-free so vitest covers it without a browser or Web Audio.
 */

/** What the core is doing. Drives colour, label and ring behaviour. */
export type VoiceStage =
	| 'offline' // no call
	| 'connecting' // dialling / reconnecting / rotating
	| 'listening' // live, nobody talking
	| 'you' // user speaking
	| 'agent' // assistant speaking
	| 'error';

/**
 * Root-mean-square of a time-domain buffer, normalised to 0..1.
 *
 * Time domain, NOT frequency: `getByteTimeDomainData` centres on 128, and its
 * RMS about that centre is the waveform's actual energy — which is what a
 * listener perceives as loudness. Averaging frequency bins (what the small
 * `VoiceOrb` does) biases toward whichever bins happen to be occupied and
 * reads high on hiss, so a silent-but-noisy room would show as speech.
 */
export function rmsFromTimeDomain(buffer: Uint8Array | number[]): number {
	if (!buffer || buffer.length === 0) return 0;
	let sum = 0;
	for (let i = 0; i < buffer.length; i += 1) {
		const centred = (buffer[i] - 128) / 128;
		sum += centred * centred;
	}
	return Math.sqrt(sum / buffer.length);
}

/**
 * Asymmetric envelope follower: fast attack, slow release.
 *
 * Symmetric smoothing makes speech look like a sine wave — it rounds off the
 * onset that actually carries the sense of someone starting to talk. Rising
 * fast and falling slow is what every VU meter does, and it is why this reads
 * as a voice rather than a throb.
 *
 * `attack`/`release` are per-frame lerp coefficients in 0..1.
 */
export function followEnvelope(
	previous: number,
	target: number,
	attack = 0.45,
	release = 0.08
): number {
	if (!Number.isFinite(target) || target < 0) return previous;
	const k = target > previous ? attack : release;
	const next = previous + (target - previous) * k;
	// Clamp tiny residue to zero so the core actually comes to rest instead of
	// asymptotically twitching forever.
	return next < 0.001 ? 0 : Math.min(1, next);
}

/**
 * Perceptual gain curve. Raw RMS for speech at normal levels sits around
 * 0.05–0.2, which is visually nothing. This maps the useful band across the
 * full range without ever clipping to a constant (which would flatten shouting
 * and quiet speech into the same picture).
 */
export function amplitudeToDisplay(rms: number, gain = 3.2): number {
	if (!Number.isFinite(rms) || rms <= 0) return 0;
	return Math.min(1, Math.pow(rms * gain, 0.72));
}

/**
 * The core's stage, derived from the real call status and transcript flags.
 *
 * Precedence matters: an error outranks everything, and the ASSISTANT
 * outranks the user — during barge-in both flags can be set at once, and
 * showing the agent is what tells the operator why they are being talked over.
 */
export function deriveVoiceStage(input: {
	callState: string;
	error?: string | null;
	userSpeaking: boolean;
	assistantSpeaking: boolean;
}): VoiceStage {
	if (input.callState === 'error' || input.error) return 'error';
	if (input.callState === 'idle' || input.callState === 'closing') return 'offline';
	if (
		input.callState === 'connecting' ||
		input.callState === 'reconnecting' ||
		input.callState === 'rotating'
	) {
		return 'connecting';
	}
	if (input.assistantSpeaking) return 'agent';
	if (input.userSpeaking) return 'you';
	return 'listening';
}

/** Is the core live enough that amplitude should drive it at all? */
export function stageIsLive(stage: VoiceStage): boolean {
	return stage === 'listening' || stage === 'you' || stage === 'agent';
}

/**
 * Radial waveform: map a time-domain buffer onto `points` radii around a
 * circle. Returns the radius for each point, already amplitude-scaled.
 *
 * Buckets are averaged rather than sampled so nothing aliases into a
 * fake-looking standing wave when the buffer is much larger than the point
 * count (1024-sample FFT → ~180 points is a 5:1 reduction).
 */
export function radialWaveform(
	buffer: Uint8Array | number[],
	points: number,
	baseRadius: number,
	swing: number
): number[] {
	const out: number[] = [];
	if (points <= 0) return out;
	if (!buffer || buffer.length === 0) {
		for (let i = 0; i < points; i += 1) out.push(baseRadius);
		return out;
	}
	const bucket = buffer.length / points;
	for (let i = 0; i < points; i += 1) {
		const start = Math.floor(i * bucket);
		const end = Math.max(start + 1, Math.floor((i + 1) * bucket));
		let sum = 0;
		let n = 0;
		for (let j = start; j < end && j < buffer.length; j += 1) {
			sum += (buffer[j] - 128) / 128;
			n += 1;
		}
		const mean = n > 0 ? sum / n : 0;
		out.push(baseRadius + mean * swing);
	}
	return out;
}

/**
 * Concentric pulse rings. Each ring lags the one inside it, so a rising
 * envelope visibly travels outward — the ring positions ARE the recent
 * amplitude history, not a decorative offset.
 *
 * `history` is newest-first.
 */
export function pulseRings(
	history: number[],
	count: number,
	baseRadius: number,
	spacing: number
): Array<{ radius: number; alpha: number }> {
	const rings: Array<{ radius: number; alpha: number }> = [];
	for (let i = 0; i < count; i += 1) {
		const sample = history[Math.min(history.length - 1, i * 3)] ?? 0;
		rings.push({
			radius: baseRadius + i * spacing + sample * spacing * 1.6,
			// Outer rings fade; louder history holds them visible longer.
			alpha: Math.max(0, (1 - i / count) * (0.25 + sample * 0.75))
		});
	}
	return rings;
}

/**
 * Organic waveform: the ribbon's radii when the channel is ALIVE.
 *
 * Layered sinusoids over the circle plus the real audio term. The angular
 * frequencies are INTEGERS on purpose — sin(k·θ) for integer k closes cleanly
 * at the 2π seam, so the ribbon never shows a discontinuity where the circle
 * joins. Non-integer frequencies produce a visible crack at angle 0.
 *
 * The organic term is a READOUT, not decoration: it only runs while the mic
 * is open (the caller gates on a live stage), and its gain scales with the
 * measured envelope — silence gives a faint drift that says "channel open,
 * listening", speech makes the surface roil. Deterministic in `timeSeconds`
 * so it is testable.
 */
export function organicRadialWaveform(
	buffer: Uint8Array | number[] | null,
	points: number,
	baseRadius: number,
	swing: number,
	timeSeconds: number,
	envelope: number
): number[] {
	const out: number[] = [];
	if (points <= 0) return out;
	const env = Number.isFinite(envelope) ? Math.max(0, Math.min(1, envelope)) : 0;
	const t = Number.isFinite(timeSeconds) ? timeSeconds : 0;
	// Weights sum to 1 so the noise term stays inside [-1, 1].
	const organicGain = swing * (0.22 + 0.78 * env);
	const audio = buffer && buffer.length > 0 ? radialWaveform(buffer, points, 0, swing * (0.6 + env)) : null;
	for (let i = 0; i < points; i += 1) {
		const a = (i / points) * Math.PI * 2;
		const n =
			0.5 * Math.sin(a * 3 + t * 0.7) +
			0.3 * Math.sin(a * 5 - t * 1.13 + 1.7) +
			0.2 * Math.sin(a * 8 + t * 1.71 + 4.2);
		out.push(baseRadius + n * organicGain + (audio ? audio[i] : 0));
	}
	return out;
}

/** Fixed-length newest-first history buffer. Returns a new array. */
export function pushHistory(history: number[], value: number, max = 48): number[] {
	const next = [value, ...history];
	return next.length > max ? next.slice(0, max) : next;
}

/** Human label for the stage — the core always says what it is doing. */
export function stageLabel(stage: VoiceStage): string {
	switch (stage) {
		case 'offline':
			return 'VOICE OFFLINE';
		case 'connecting':
			return 'OPENING CHANNEL';
		case 'listening':
			return 'LISTENING';
		case 'you':
			return 'YOU';
		case 'agent':
			return 'AGENT SPEAKING';
		case 'error':
			return 'VOICE FAULT';
	}
}
