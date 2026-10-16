// Agent presence for the chat-next orb (see docs/components/unified-ui/chat-next.md).

export type PresenceState = 'idle' | 'thinking' | 'working' | 'needs-you';

export interface PresenceSignals {
	/** A HITL request is pending for a turn in the current session. */
	hasPendingHitl: boolean;
	/** A chat turn is in flight (sent, or a spawned task still emitting). */
	liveTurn: boolean;
	/** An assistant reply is streaming in. */
	streaming: boolean;
	/** Tools or a spawned task are running for the live turn. */
	toolsRunning: boolean;
}

/** Precedence: needs-you > working > thinking > idle. */
export function derivePresence(signals: PresenceSignals): PresenceState {
	if (signals.hasPendingHitl) return 'needs-you';
	if (signals.streaming || signals.toolsRunning) return 'working';
	if (signals.liveTurn) return 'thinking';
	return 'idle';
}

const LABELS: Record<PresenceState, string> = {
	idle: 'Magician is idle',
	thinking: 'Magician is thinking',
	working: 'Magician is working',
	'needs-you': 'Magician needs you'
};

export function presenceLabel(state: PresenceState): string {
	return LABELS[state];
}
