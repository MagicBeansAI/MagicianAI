/**
 * The inspected run, live.
 *
 * The old `DeepWorkPanel` streamed because its data layer owned a WebSocket
 * subscription; the unified panel is a pure render of a `TaskPanelModel` prop
 * and must stay one, so the subscription belongs to the **surface** that fetched
 * the state. This is that subscription, lifted out of the surface for the one
 * part of it worth pinning: deciding whether a pushed state is about the run the
 * reader is looking at.
 *
 * `ExecutionPanelDelta` is a server-pushed **full snapshot**, not a patch, so
 * there is no accumulator here and no ordering to reconstruct — the newest
 * matching state in a batch replaces the model outright, which is exactly what
 * the fetch does.
 *
 * **Two surfaces subscribe through this**, and neither is allowed a bridge of
 * its own: chat's *Inspect run →*, and `/crew/<id>`'s agent cycle. The cycle's
 * synthetic `agent-cycle:<agent>:<cycle>` task id is only an endpoint selector
 * and is never read here — what is read is its real execution id, which is what
 * the projector puts on every delta it pushes for that run.
 *
 * See `docs/components/unified-ui/unified-task-panel.md`, *The run drawers
 * stream*.
 */

import {
	getV2EventSequence,
	v2Events,
	type V2WebSocketEvent
} from '$lib/realtime/v2-websocket';
import type { ExecutionPanelState } from '$lib/types/executionPanel';

import type { ExecutionPanelTarget } from './executionPanelModel';
import { executionIdOf } from './taskTimeline';

/** The principal and workspace the drawer is reading under. */
export interface PanelScope {
	principal: string;
	workspace: string;
}

type PanelDelta = Extract<V2WebSocketEvent, { event_type: 'ExecutionPanelDelta' }>;

/**
 * The state this delta carries **if it is about the run on screen**, else
 * `null`.
 *
 * **The identity test is the execution, and it is read off the state rather
 * than off the envelope.** The obvious rule — the one the retired data layer
 * used — accepted a delta whose `task_id` matched *or* whose `execution_id`
 * matched. That is right for a panel pinned to a task and wrong for one pinned
 * to a run: a task with two executions pushes deltas for both, and the
 * task-id arm would drop the newer run's state into a drawer showing the older
 * one. Every value involved is individually valid, the panel would render a
 * complete and plausible run, and nothing downstream could catch it.
 *
 * Reading the id from `state` rather than from the event's own `execution_id`
 * closes the same gap from the other side: what the panel renders is the state,
 * so the state is what has to be about this run. `executionIdOf` is the one
 * function every reader of this payload asks, so this cannot come to a different
 * answer than the model's id or the timeline's filter does.
 *
 * The scope check stays because a delta is a broadcast: another principal's run
 * can carry any execution id at all, including one this workspace also uses.
 */
export function panelDeltaState(
	event: PanelDelta,
	target: ExecutionPanelTarget,
	scope: PanelScope
): ExecutionPanelState | null {
	const data = event.data;
	if (data.principal !== scope.principal || data.workspace !== scope.workspace) return null;
	const state = data.state;
	if (!state?.overview?.status) return null;
	return executionIdOf(state) === target.executionId ? state : null;
}

/**
 * Push this run's state at `onState` as it changes. Returns the unsubscribe.
 *
 * Sequence-gated the same way every other consumer of this store is: the
 * subscription replays its buffer on every emit, so without the gate the first
 * batch after opening the drawer would re-deliver states the fetch already
 * covered — and, more to the point, an older state after a newer one.
 */
export function streamExecutionPanelState(
	target: ExecutionPanelTarget,
	scope: PanelScope,
	onState: (state: ExecutionPanelState) => void
): () => void {
	if (v2Events.getConnectionState() === 'CLOSED') v2Events.connectGlobal();

	let seen = 0;
	return v2Events.subscribe((events: V2WebSocketEvent[]) => {
		let latest: ExecutionPanelState | null = null;
		let next = seen;
		for (const event of events) {
			const sequence = getV2EventSequence(event);
			if (sequence <= seen) continue;
			next = Math.max(next, sequence);
			if (event.event_type !== 'ExecutionPanelDelta') continue;
			// The last match in the batch wins: these are full snapshots, so an
			// earlier one in the same batch is state the later one already includes.
			const state = panelDeltaState(event, target, scope);
			if (state !== null) latest = state;
		}
		if (next <= seen) return;
		seen = next;
		if (latest !== null) onState(latest);
	});
}
