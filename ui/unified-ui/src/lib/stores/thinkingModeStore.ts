/**
 * Per-chat-turn thinking-mode tracker.
 *
 * Derives "is this turn currently running in the thinking variant of an
 * adaptive profile?" from the per-turn event log
 * (`chatTurnEventsStore`). A turn is in thinking mode after the backend
 * emits `ThinkingModeActivated` and before `ThinkingModeCompleted` for
 * the same `chat_turn_id`.
 *
 * Used by the floating composer to show a "Thinking mode" chip while
 * the in-flight turn is escalated. The chip clears automatically on
 * `ThinkingModeCompleted` (emitted at turn end regardless of outcome —
 * success, error, or loop-exhausted) so it never sticks.
 */
import { derived, type Readable } from 'svelte/store';
import { chatTurnEventsStore, type RawTurnEvent } from './chatTurnEventsStore';

export interface ThinkingModeState {
    /** True iff `ThinkingModeActivated` has fired without a matching
     *  `ThinkingModeCompleted` for this turn id. */
    active: boolean;
    /** Optional reason string the LLM passed when escalating. */
    reason: string | null;
    /** Adaptive composite name (e.g. `chat-openai-adaptive`). */
    adaptive_profile: string | null;
}

const EMPTY: ThinkingModeState = { active: false, reason: null, adaptive_profile: null };

function readThinkingMode(events: RawTurnEvent[] | undefined): ThinkingModeState {
    if (!events || events.length === 0) return EMPTY;
    let state: ThinkingModeState = EMPTY;
    for (const event of events) {
        const eventType = (event as Record<string, unknown>).event_type;
        if (eventType !== 'ThinkingModeActivated' && eventType !== 'ThinkingModeCompleted') {
            continue;
        }
        const data = ((event as Record<string, unknown>).data ?? {}) as Record<string, unknown>;
        if (eventType === 'ThinkingModeActivated') {
            const reasonRaw = data.reason;
            const profileRaw = data.adaptive_profile;
            state = {
                active: true,
                reason: typeof reasonRaw === 'string' && reasonRaw.length > 0 ? reasonRaw : null,
                adaptive_profile:
                    typeof profileRaw === 'string' && profileRaw.length > 0 ? profileRaw : null,
            };
        } else {
            // ThinkingModeCompleted — pair clear. Preserve last-known
            // reason/profile so consumers that want to render a "this
            // turn used thinking mode" footnote can still see it.
            state = { ...state, active: false };
        }
    }
    return state;
}

/**
 * Read the thinking-mode state for a specific chat turn. Reactive —
 * updates when new events land in `chatTurnEventsStore`. Returns
 * `EMPTY` for unknown / no-events turn ids.
 */
export function thinkingModeForTurn(
    chatTurnId: string | null | undefined
): Readable<ThinkingModeState> {
    return derived(chatTurnEventsStore, ($map) => {
        if (!chatTurnId) return EMPTY;
        return readThinkingMode($map.get(chatTurnId));
    });
}
