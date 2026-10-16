export type RecipeLifecycleEvent = {
	kind: string;
	recipe_id?: string;
	template?: string;
	duration_ms?: number;
	step_id?: string;
	class?: string;
	replayed_steps?: number;
	origin?: string;
	to?: string;
	version?: number;
	decision?: string;
};

function failureWords(value?: string): string {
	return (value || 'could not continue').replaceAll('_', ' ');
}

export function narrate(events: RecipeLifecycleEvent[]): string[] {
	const lines: string[] = [];
	const completed = [...events].reverse().find((event) => event.kind === 'recipe.replay.completed');
	const started = events.find((event) => event.kind === 'recipe.replay.started');
	const healed = events.find((event) => event.kind === 'recipe.replay.auth.healed');
	const downgrade = events.find((event) => event.kind === 'recipe.replay.transport.downgraded');
	const fallback = events.find((event) => event.kind === 'recipe.replay.fallback.handoff');
	const recompiled = [...events].reverse().find((event) => event.kind === 'recipe.replay.recompiled');
	const approval = events.find((event) => event.kind === 'recipe.replay.approval.resolved');

	if (completed) {
		const recipe = started?.template
			? `the learned API recipe “${started.template}”`
			: 'the learned API recipe';
		const duration = completed.duration_ms == null ? '' : ` in ${completed.duration_ms}ms`;
		lines.push(`Answered from ${recipe}${duration}; no browser was opened.`);
	}
	if (healed) {
		lines.push('The saved session had expired; it was refreshed automatically and the request retried.');
	}
	if (downgrade) {
		const host = downgrade.origin || 'The site';
		lines.push(`${host} rejected direct requests; this step will use ${downgrade.to || 'in-page fetch'} next time.`);
	}
	if (fallback) {
		const done = fallback.replayed_steps ?? 0;
		lines.push(`The API completed ${done} step${done === 1 ? '' : 's'}; ${fallback.step_id || 'the next step'} ${failureWords(fallback.class)}, so the browser continued from there.`);
	}
	if (recompiled) {
		lines.push(`The browser run taught the recipe what changed; version ${recompiled.version ?? '?'} will be used next time.`);
	}
	if (approval && ['deny', 'denied'].includes(approval.decision || '')) {
		lines.push('A write step needed approval and was denied; nothing was changed on the site.');
	}
	return lines;
}

export function recipeCue(events: RecipeLifecycleEvent[]): string | null {
	const approval = [...events]
		.reverse()
		.find((event) => event.kind === 'recipe.replay.approval.resolved');
	if (approval && ['deny', 'denied'].includes(approval.decision || '')) {
		return 'Stopped · write not approved';
	}
	const completed = [...events].reverse().find((event) => event.kind === 'recipe.replay.completed');
	if (completed) {
		return `Learned API · no browser${completed.duration_ms == null ? '' : ` · ${completed.duration_ms}ms`}`;
	}
	const recompiled = [...events].reverse().find((event) => event.kind === 'recipe.replay.recompiled');
	if (recompiled) return `Browser · recipe v${recompiled.version ?? '?'} learned`;
	const fallback = [...events]
		.reverse()
		.find((event) => event.kind === 'recipe.replay.fallback.handoff');
	if (fallback) {
		const replayed = fallback.replayed_steps ?? 0;
		return `Learned API · ${replayed} API step${replayed === 1 ? '' : 's'} · browser from ${fallback.step_id || 'next step'}`;
	}
	return events.some((event) => event.kind === 'recipe.replay.started')
		? 'Learned API · replaying'
		: null;
}
