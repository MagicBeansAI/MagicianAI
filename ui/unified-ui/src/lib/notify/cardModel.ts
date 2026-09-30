/**
 * Notify-overlay card model — pure, no I/O.
 *
 * Maps a canonical V3 HITL event (the same `{ event_type, data }` line shape
 * the `/api/magician/v3/events` NDJSON tail emits — see
 * `lib/stores/pendingHitlStore.ts` `ingestLine` and
 * `lib/hitl/adapters.ts` `hitlRequestFromCanonicalEvent`) into a `NotifyCard`
 * the overlay route renders.
 *
 * Fields are read ONLY from the flat `data.*` layer — never the deeper
 * `data.event.payload.*` / `payload.*` / `data.request.*` layers
 * `pendingHitlStore.correlationKey` walks. That is correct, not a gap: a
 * `HitlRequested` on the V3 tail is the serialized `RuntimeTransportEvent`
 * enum variant, whose `#[serde(tag = "event_type", content = "data")]`
 * (magician/src/magician_v2/realtime_events.rs:59-60) puts every field flat
 * under `data.*`. See `notifyStream.ts`'s header for the full
 * flat-vs-enveloped rationale + evidence.
 *
 * Kept strictly side-effect-free so the stream loop (`notifyStream.ts`) and the
 * unit tests share exactly one mapping path. The discriminated `kind` field
 * separates the actionable HITL shape (launcher + correlation lifecycle) from
 * the informational shapes (`info`/`success`/`error`) added in Phase 2/3 —
 * see `infoModel.ts` for the mapping of execution / completion / message
 * events to the informational variant.
 */

import { hitlRequestFromCanonicalEvent } from '$lib/hitl/adapters';

/**
 * Canonical card type the `/notify-overlay` route imports (replacing its
 * Task-2 placeholder `interface NotifyCard { id: string }`).
 *
 * `kind` is the discriminant across two families:
 *
 *   - `'actionable'` — a HITL request awaiting a human response.
 *     Every input type uses the same compact launcher interaction. The actual
 *     response controls live only in the canonical Attention page. Its
 *     `correlationId` is the complete deep-link handoff contract.
 *
 *   - `'info' | 'success' | 'error'` — non-actionable, informational cards
 *     (incoming message, completion, error/failure). These carry a `title`,
 *     an optional `message` body, an optional `deepLink` the view hands to the
 *     Tauri `open_app_at({ path })` command on click, and an optional
 *     `dismissAfterMs` the VIEW uses to schedule an auto-dismiss timer.
 *     `applyEvent` stays pure — it only sets `dismissAfterMs` on the card; it
 *     never starts a timer. All informational kinds (success AND error) carry a
 *     `dismissAfterMs` so they auto-vanish (errors linger longer) and never
 *     bury the persistent approval queue; only actionable cards persist.
 */
export type NotifyCard =
	| {
			id: string;
			kind: 'actionable';
			correlationId: string;
			source: string;
			inputType: string;
			prompt: string;
			hint?: string;
	  }
	| {
			id: string;
			kind: 'info' | 'success' | 'error';
			title: string;
			message?: string;
			deepLink?: string;
			dismissAfterMs?: number;
	  };

/** Read a non-empty string field from a loosely-typed record, else undefined. */
export function readString(data: Record<string, unknown>, key: string): string | undefined {
	const value = data[key];
	return typeof value === 'string' && value.length > 0 ? value : undefined;
}

/**
 * Map a canonical HITL event to a `NotifyCard`.
 *
 * Returns a card only for `event_type === 'HitlRequested'` with a canonical
 * correlation, pause-state, or approval id; resolutions and identifier-free
 * lines yield `null`. Historical and current events share the same ID deep-link
 * path; richer request data is hydrated by the Attention page.
 *
 * Input type is retained for diagnostics; it does not alter notification UI.
 */
export function hitlEventToCard(event: {
	event_type: string;
	data: Record<string, unknown>;
}): NotifyCard | null {
	if (event.event_type !== 'HitlRequested') return null;
	const data = event.data ?? {};
	const legacyId =
		readString(data, 'correlation_id') ??
		readString(data, 'pause_state_id') ??
		readString(data, 'approval_id');
	const request = hitlRequestFromCanonicalEvent({
		event_type: event.event_type,
		data
	});
	if (!request) {
		if (!legacyId) return null;
		const hint = readString(data, 'hint');
		return {
			id: legacyId,
			kind: 'actionable',
			correlationId: legacyId,
			source: readString(data, 'source') ?? 'agentic',
			inputType: readString(data, 'input_type') ?? '',
			prompt: readString(data, 'prompt') ?? '',
			...(hint !== undefined ? { hint } : {})
		};
	}
	return {
		id: request.id,
		kind: 'actionable',
		correlationId: request.id,
		source: request.source,
		inputType: request.input_type,
		prompt: request.prompt,
		...(request.hint !== undefined ? { hint: request.hint } : {})
	};
}
