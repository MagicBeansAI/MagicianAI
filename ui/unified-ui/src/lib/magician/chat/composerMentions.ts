export type ComposerMentionKind = 'agent' | 'tool' | 'personality' | 'task' | 'feature';

export interface ComposerMentionItem {
	id: string;
	label: string;
	detail?: string;
	kind: ComposerMentionKind;
	/** Text inserted into the user turn. Delegate-owned skills use
	 *  `skill:<name> via agent:<agent_id>` so the model sees the intended
	 *  routing hint without a separate hidden control channel. Task mentions use
	 *  `task:<id>` so the model reads the PRECISE task id. */
	insertText: string;
	searchText?: string;
	/** For task mentions: the human title shown INSIDE the chip while `insertText`
	 *  keeps the id (`task:<id>`). Lets the user see the title but the system stay
	 *  precise. */
	chipLabel?: string;
}

// ─── Shared mention state machine ───────────────────────────────────────────
// Pure logic shared by composer surfaces so the
// `@`-mention behavior lives in one place. Composers keep only the UI event
// wiring (caret read + chip insertion via ChipTextarea, picker rendering).

export type MentionGroup = ComposerMentionKind | 'all';

export function mentionGroupForQuery(query: string): MentionGroup {
	const normalized = query.toLowerCase();
	if (normalized === 'agent' || normalized.startsWith('agent:')) return 'agent';
	if (normalized === 'agents' || normalized.startsWith('agents:')) return 'agent';
	if (normalized === 'skill' || normalized.startsWith('skill:')) return 'tool';
	if (normalized === 'skills' || normalized.startsWith('skills:')) return 'tool';
	if (normalized === 'tool' || normalized.startsWith('tool:')) return 'tool';
	if (normalized === 'tools' || normalized.startsWith('tools:')) return 'tool';
	if (normalized === 'personality' || normalized.startsWith('personality:')) return 'personality';
	if (normalized === 'task' || normalized.startsWith('task:')) return 'task';
	if (normalized === 'tasks' || normalized.startsWith('tasks:')) return 'task';
	if (normalized === 'feature' || normalized.startsWith('feature:')) return 'feature';
	if (normalized === 'features' || normalized.startsWith('features:')) return 'feature';
	return 'all';
}

export function mentionNeedleForQuery(query: string, group: MentionGroup): string {
	if (group === 'all') return query.toLowerCase();
	const idx = query.indexOf(':');
	if (idx >= 0) return query.slice(idx + 1).toLowerCase();
	return '';
}

export function filterMentionItems(
	items: ComposerMentionItem[],
	group: MentionGroup,
	needle: string
): ComposerMentionItem[] {
	return items.filter((item) => {
		if (group !== 'all' && item.kind !== group) return false;
		if (!needle) return true;
		const haystack = item.searchText || `${item.label} ${item.detail ?? ''} ${item.id}`;
		return haystack.toLowerCase().includes(needle);
	});
}

/** group → needle → filter → cap. The single entry point composers use to turn
 *  a raw `@…` query into ranked matches. */
export function mentionMatchesFor(
	items: ComposerMentionItem[],
	query: string,
	limit = 9
): ComposerMentionItem[] {
	const group = mentionGroupForQuery(query);
	const needle = mentionNeedleForQuery(query, group);
	return filterMentionItems(items, group, needle).slice(0, limit);
}

/** Detect a trailing `@…` mention trigger in the text before the caret. Returns
 *  the number of chars to consume on commit + the query, or null when the caret
 *  isn't in a mention. Serialized chips emit as `agent:foo` tokens, so the
 *  pattern only ever fires on user-typed `@…`, never on a previously inserted
 *  chip's serialized text. */
export function detectMentionTrigger(
	beforeCursor: string,
	options?: { allowSpaces?: boolean }
): { consume: number; query: string } | null {
	const commandMatch = beforeCursor.match(
		/(^|\s)@(agent|agents|skill|skills|tool|tools|personality|task|tasks|feature|features)\s+([A-Za-z0-9_:-]*)$/i
	);
	if (commandMatch) {
		const matchStart = (commandMatch.index ?? 0) + commandMatch[1].length;
		return {
			consume: beforeCursor.length - matchStart,
			query: `${commandMatch[2].toLowerCase()}:${commandMatch[3] ?? ''}`,
		};
	}
	// With `allowSpaces` (surfaces whose mention labels are human + multi-word — the
	// tasks page, the vibe cockpit), the bare `@…` query may span spaces so the user
	// can narrow on a multi-word title (`@Q3 rev` → "Q3 rev"). Chat leaves it OFF so
	// prose after a mention (`@agent do the thing`) closes the picker. Either way the
	// `@` must be at line-start or after whitespace, and the query stops at a newline
	// (or another `@`, which begins a fresh trigger).
	const bare = options?.allowSpaces
		? beforeCursor.match(/(^|\s)@([^\n@]*)$/)
		: beforeCursor.match(/(^|\s)@([A-Za-z0-9_:-]*)$/);
	if (!bare) return null;
	return { consume: (bare[2]?.length ?? 0) + 1, query: bare[2] ?? '' };
}

export function mentionKindLabel(kind: ComposerMentionKind): string {
	if (kind === 'agent') return 'Agent';
	if (kind === 'personality') return 'Personality';
	if (kind === 'task') return 'Task';
	if (kind === 'feature') return 'Feature';
	return 'Tool';
}

/** Build the chat `feature` mention items. The chip DISPLAYS one label (chipLabel,
 *  e.g. `@tutor_quick`) but SERIALIZES the literal command the backend recognises
 *  inline — e.g. `@tutor #quick` (chipMarkup `featureCommandForSlug`; backend
 *  tutor.rs). `insertText` keeps the `feature:<slug>` routing form so the picker's
 *  colon-split commit path is unchanged, and the clean slug drives serialization.
 *  Every lane here is always offered except copilot, which needs an image to ground
 *  on (HUD + a staged screenshot). */
export function buildFeatureMentionItems(opts: { includeCopilot: boolean }): ComposerMentionItem[] {
	const items: ComposerMentionItem[] = [
		{
			id: 'feature:tutor',
			kind: 'feature',
			label: '@tutor',
			detail: 'Tutor — explains on a blackboard',
			insertText: 'feature:tutor',
			chipLabel: '@tutor',
			searchText: 'tutor @tutor blackboard explain feature'
		},
		{
			id: 'feature:tutor_quick',
			kind: 'feature',
			label: '@tutor_quick',
			detail: 'Tutor, quick — fastest first overlay',
			insertText: 'feature:tutor_quick',
			chipLabel: '@tutor_quick',
			searchText: 'tutor quick tutor_quick @tutor #quick fast feature'
		},
		{
			// Starts a Live Thinking Map from the composer text instead of a chat
			// turn (ChatPanel's send-time detection creates + opens the map — the
			// message never reaches the chat LLM). Serializes to `@brainstorm` via
			// featureCommandForSlug's `@<slug>` fallback.
			id: 'feature:brainstorm',
			kind: 'feature',
			label: '@brainstorm',
			detail: 'Brainstorm — grow a live thinking map from this thought',
			insertText: 'feature:brainstorm',
			chipLabel: '@brainstorm',
			searchText: 'brainstorm @brainstorm thinking map idea mind map feature'
		},
		{
			// Starts a VibeDev build from the composer text. Unlike @brainstorm above,
			// nothing here intercepts the send: the marker travels in the message and
			// the server recognises it (tutor.rs `parse_vibedev_rail_invocation`), so
			// voice and every other surface that reaches chat get the rail without
			// each growing its own copy of the trigger.
			id: 'feature:vibedev',
			kind: 'feature',
			label: '@vibedev',
			detail: 'VibeDev — starts a build in your project',
			insertText: 'feature:vibedev',
			chipLabel: '@vibedev',
			searchText: 'vibedev @vibedev build implement ship feature'
		},
		{
			// The plan-only lane. `#discuss` is a separate token after the marker, so
			// this serializes to `@vibedev #discuss` — same pairing as
			// tutor/tutor_quick.
			id: 'feature:vibedev_discuss',
			kind: 'feature',
			label: '@vibedev_discuss',
			detail: 'VibeDev, discuss — plans the build without writing it',
			insertText: 'feature:vibedev_discuss',
			chipLabel: '@vibedev_discuss',
			searchText: 'vibedev discuss vibedev_discuss @vibedev #discuss plan feature'
		}
	];
	if (opts.includeCopilot) {
		items.push({
			id: 'feature:copilot',
			kind: 'feature',
			label: '@copilot',
			detail: 'Copilot — guides the app on screen (needs a screenshot)',
			insertText: 'feature:copilot',
			chipLabel: '@copilot',
			searchText: 'copilot @copilot app guide operate feature'
		});
	}
	return items;
}

/** Build `task` mention items for the composer picker. ONE definition shared by
 *  every surface (chat, the tasks-page composer, the vibe cockpit) so a task chip
 *  is identical everywhere: it DISPLAYS the title (`chipLabel`/`label`) while
 *  `insertText`/`detail` keep the precise `task:<id>` (so the model — and any
 *  structured readout via `chipTokens()` — gets the exact id), and search matches
 *  title OR id. Callers pass only the tasks they want offered (e.g. filtered to
 *  `status === 'completed'` and the persistent feed). */
export function buildTaskMentionItems(
	tasks: ReadonlyArray<{ id?: string | null; title?: string | null }>
): ComposerMentionItem[] {
	const items: ComposerMentionItem[] = [];
	for (const task of tasks) {
		const id = task?.id?.trim();
		const title = task?.title?.trim();
		if (!id || !title) continue;
		items.push({
			id: `task:${id}`,
			kind: 'task',
			label: title,
			detail: id,
			insertText: `task:${id}`,
			chipLabel: title,
			searchText: `${title} ${id}`
		});
	}
	return items;
}
