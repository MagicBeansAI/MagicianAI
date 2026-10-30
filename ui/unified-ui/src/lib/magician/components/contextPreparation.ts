/** One context phase, shared by live, saved-chat and task steps. */
export function contextPreparationRow(eventType: string, payload?: Record<string, unknown>) {
	if (eventType !== 'context.retrieval.started' && eventType !== 'context.retrieval.completed') return null;
	const id = typeof payload?.phase_id === 'string' ? payload.phase_id.trim() : '';
	if (!id) return null;
	const running = eventType.endsWith('.started');
	const duration = payload?.duration_ms;
	// A phase may name itself (e.g. "Compacting conversation") and report exact
	// progress as step of steps; without them it is generic context preparation.
	const named = typeof payload?.label === 'string' && payload.label.trim() ? payload.label.trim() : null;
	const step = payload?.step, steps = payload?.steps;
	const counted = typeof step === 'number' && typeof steps === 'number' && steps > 0 && step >= 0 && step <= steps
		? ` · ${step}/${steps}` : '';
	const doneLabel = typeof payload?.done_label === 'string' && payload.done_label.trim() ? payload.done_label.trim() : null;
	return {
		key: `context:${id}`,
		label: running ? `${named ?? 'Preparing context'}${counted}` : doneLabel ?? (payload?.status === 'cancelled' ? 'Context preparation cancelled' : payload?.status === 'partial' ? 'Context prepared (partial)' : 'Context prepared'),
		detail: typeof payload?.summary === 'string' ? payload.summary : null,
		status: running ? 'running' as const : payload?.status === 'failed' ? 'failed' as const : 'done' as const,
		durationMs: typeof duration === 'number' && Number.isFinite(duration) && duration >= 0 ? duration : null
	};
}
