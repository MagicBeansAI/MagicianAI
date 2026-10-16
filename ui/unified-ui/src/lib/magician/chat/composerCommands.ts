// ─── Composer slash commands ────────────────────────────────────────────────
// Pure logic for the `/` command picker (docs/plans/2026-10-03-slash-commands-
// and-run-budgets.md). The composer owns only the UI wiring — the trigger, the
// level the picker is on, filtering and the option rows all live here so web
// behaviour is testable without a DOM.
//
// The picker is text-driven: the composer value is always
// `/<token> <token> … <query>`, where each token is a level the user already
// chose (see `slashPrefix`). Deleting into a token (Backspace) therefore steps
// back a level on its own — `resolveSlashInput` pops every step whose prefix
// the text no longer carries. Commands never reach the model: choosing an
// option yields a `SlashAction` the host runs client-side.

export type SlashCommandId = 'budget' | 'budget-default' | 'engine' | 'run-engine';

/** A level the user chose. `token` is what the composer text shows for it. */
export interface SlashStep {
	id: string;
	token: string;
	label: string;
	command?: SlashCommandId;
	/** `/budget` → the chosen task. */
	taskId?: string;
	/** `/engine` → "make default everywhere" sub-level. */
	scope?: 'everywhere';
}

export type BudgetCap = number | 'unlimited';

export interface BudgetDefaults {
	native_max_cost_usd: BudgetCap;
	harness_max_cost_usd: BudgetCap;
}

export interface TaskBudgetGrant {
	add_cost_usd?: number;
	add_tokens?: number;
	add_iterations?: number;
	add_duration_secs?: number;
	unlimited?: boolean;
}

export type SlashAction =
	| { type: 'chat-engine'; engine: string; model: string; scope: 'device' | 'everywhere' }
	| { type: 'run-engine'; engine: string; model: string }
	| { type: 'task-budget'; taskId: string; taskLabel: string; grant: TaskBudgetGrant; grantLabel: string }
	| { type: 'budget-default'; field: keyof BudgetDefaults; value: BudgetCap };

export interface SlashItem {
	id: string;
	label: string;
	detail?: string;
	/** Short uppercase badge, like the `@` picker's kind badge. */
	badge: string;
	/** The value in force right now (marked in the list). */
	current?: boolean;
	searchText?: string;
	/** Choosing it descends a level … */
	next?: SlashStep;
	/** … or runs it. Exactly one of `next` / `action` is set. */
	action?: SlashAction;
}

export interface SlashEngineRow {
	name: string;
	installed: boolean;
	models?: string[];
}

export interface SlashEngineChoice {
	engine: string;
	model: string;
}

/** A task `/budget` can extend: running or paused, in this chat. */
export interface SlashBudgetTask {
	id: string;
	title: string;
	status: string;
}

/** Everything the option lists read. `null` fields are still loading. */
export interface SlashCommandContext {
	engines: SlashEngineRow[] | null;
	/** Why the roster could not be read (the list then offers Magician only). */
	enginesError?: string | null;
	/** This browser's chat engine (`chatHarnessPreferenceStore`). */
	chatEngine: SlashEngineChoice | null;
	/** The server's chat default (roster `chat_current` / `chat_model`). */
	serverChatEngine: SlashEngineChoice | null;
	/** The run engine (roster `current` / `run_model`). */
	runEngine: SlashEngineChoice | null;
	tasks: SlashBudgetTask[];
	budgetDefaults: BudgetDefaults | null;
	budgetDefaultsError?: string | null;
}

export const EMPTY_SLASH_CONTEXT: SlashCommandContext = {
	engines: null,
	chatEngine: null,
	serverChatEngine: null,
	runEngine: null,
	tasks: [],
	budgetDefaults: null
};

export interface SlashCommandDef {
	id: SlashCommandId;
	token: string;
	label: string;
	detail: string;
}

export const SLASH_COMMANDS: readonly SlashCommandDef[] = [
	{
		id: 'budget',
		token: 'budget',
		label: '/budget',
		detail: 'Add budget to a running or paused task in this chat'
	},
	{
		id: 'budget-default',
		token: 'budget default',
		label: '/budget default',
		detail: 'Default cost cap for new runs'
	},
	{
		id: 'engine',
		token: 'engine',
		label: '/engine',
		detail: 'Engine that answers this chat'
	},
	{
		id: 'run-engine',
		token: 'run-engine',
		label: '/run-engine',
		detail: 'Engine that drives task runs'
	}
];

export const HARNESS_DEFAULT_CAPS: readonly BudgetCap[] = [50, 100, 250, 'unlimited'];
export const NATIVE_DEFAULT_CAPS: readonly BudgetCap[] = [5, 15, 50, 'unlimited'];

export const TASK_BUDGET_GRANTS: ReadonlyArray<{ id: string; label: string; detail: string; grant: TaskBudgetGrant }> = [
	{ id: 'cost-25', label: '+$25', detail: 'Raise the cost cap by $25', grant: { add_cost_usd: 25 } },
	{ id: 'cost-100', label: '+$100', detail: 'Raise the cost cap by $100', grant: { add_cost_usd: 100 } },
	{ id: 'tokens-1m', label: '+1M tokens', detail: 'Raise the token limit by 1,000,000', grant: { add_tokens: 1_000_000 } },
	{ id: 'duration-24h', label: '+24 hours', detail: 'Add 24 hours of active time on the next resume', grant: { add_duration_secs: 86400 } },
	{ id: 'iterations-50', label: '+50 iterations', detail: 'Allow 50 more iterations', grant: { add_iterations: 50 } },
	{ id: 'unlimited', label: 'Unlimited', detail: 'Remove the cost cap and token meter for this run', grant: { unlimited: true } }
];

// ─── Trigger + level resolution ─────────────────────────────────────────────

/** `/` + every chosen token, each followed by one space. Root is `/`. */
export function slashPrefix(path: readonly SlashStep[]): string {
	return '/' + path.map((step) => `${step.token} `).join('');
}

/** contenteditable may hand back NBSPs for typed spaces. */
function normalizeSpaces(value: string): string {
	return value.replace(/ /g, ' ');
}

/** Is this composer value a slash command (a `/` first, a single line)? */
export function isSlashCommandInput(value: string): boolean {
	return value.startsWith('/') && !value.includes('\n');
}

/** Read the composer value against the levels chosen so far: drop every level
 *  whose prefix the text lost (the user deleted into it), and return the
 *  remaining text as the query for the current level. */
export function resolveSlashInput(
	value: string,
	path: readonly SlashStep[]
): { open: boolean; path: SlashStep[]; query: string } {
	const text = normalizeSpaces(value);
	if (!isSlashCommandInput(text)) return { open: false, path: [], query: '' };
	let kept = path.slice();
	while (kept.length > 0 && !text.startsWith(slashPrefix(kept))) kept = kept.slice(0, -1);
	return { open: true, path: kept, query: text.slice(slashPrefix(kept).length) };
}

// ─── Filtering ──────────────────────────────────────────────────────────────

/** Every whitespace word of the query must appear in the row's text; rows whose
 *  label starts with the query rank first, otherwise list order holds. */
export function filterSlashItems(items: readonly SlashItem[], query: string): SlashItem[] {
	const needle = normalizeSpaces(query).trim().toLowerCase();
	if (!needle) return items.slice();
	const words = needle.split(/\s+/);
	const matches = items.filter((item) => {
		const haystack = `${item.label} ${item.detail ?? ''} ${item.badge} ${item.searchText ?? ''}`.toLowerCase();
		return words.every((word) => haystack.includes(word));
	});
	const startsWith = (item: SlashItem) => item.label.toLowerCase().replace(/^\//, '').startsWith(needle.replace(/^\//, ''));
	return [...matches.filter(startsWith), ...matches.filter((item) => !startsWith(item))];
}

// ─── Option lists ───────────────────────────────────────────────────────────

const ENGINE_NAMES: Record<string, string> = {
	magician: 'Magician',
	pi: 'Coding agent',
	claude_code: 'Claude Code',
	codex: 'Codex',
	codex_app_server: 'Codex App Server',
	grok: 'Grok',
	agy: 'Antigravity'
};

export function engineDisplayName(name: string): string {
	return (
		ENGINE_NAMES[name] ??
		name
			.split(/[_\s]+/)
			.filter(Boolean)
			.map((word) => word.charAt(0).toUpperCase() + word.slice(1))
			.join(' ')
	);
}

function modelDisplay(model: string): string {
	return model === 'default' ? 'default model' : model;
}

export function formatBudgetCap(cap: BudgetCap): string {
	if (cap === 'unlimited') return 'Unlimited';
	return Number.isInteger(cap) ? `$${cap}` : `$${cap.toFixed(2)}`;
}

function sameChoice(a: SlashEngineChoice | null, b: SlashEngineChoice): boolean {
	return !!a && a.engine === b.engine && (a.model || 'default') === b.model;
}

/** One row per installed engine × model (`default` first). */
export function engineChoiceRows(engines: readonly SlashEngineRow[]): SlashEngineChoice[] {
	const rows: SlashEngineChoice[] = [];
	for (const engine of engines) {
		if (!engine.installed) continue;
		const models = ['default', ...(engine.models ?? []).filter((model) => model && model !== 'default')];
		for (const model of models) rows.push({ engine: engine.name, model });
	}
	return rows;
}

function engineItems(
	ctx: SlashCommandContext,
	current: SlashEngineChoice | null,
	toAction: (row: SlashEngineChoice) => SlashAction,
	idPrefix: string
): SlashItem[] {
	return engineChoiceRows(ctx.engines ?? []).map((row) => ({
		id: `${idPrefix}:${row.engine}:${row.model}`,
		label: `${engineDisplayName(row.engine)} · ${modelDisplay(row.model)}`,
		detail: sameChoice(current, row) ? 'Current' : undefined,
		badge: 'Engine',
		current: sameChoice(current, row),
		searchText: `${row.engine} ${row.model}`,
		action: toAction(row)
	}));
}

function budgetDefaultItems(ctx: SlashCommandContext): SlashItem[] {
	const items: SlashItem[] = [];
	const add = (field: keyof BudgetDefaults, caps: readonly BudgetCap[], badge: string, what: string) => {
		for (const cap of caps) {
			const current = ctx.budgetDefaults ? ctx.budgetDefaults[field] === cap : false;
			items.push({
				id: `${field}:${cap}`,
				label: `${what} · ${formatBudgetCap(cap)}`,
				detail: current ? 'Current' : undefined,
				badge,
				current,
				searchText: `${field} ${cap}`,
				action: { type: 'budget-default', field, value: cap }
			});
		}
	};
	add('harness_max_cost_usd', HARNESS_DEFAULT_CAPS, 'Harness', 'Harness runs');
	add('native_max_cost_usd', NATIVE_DEFAULT_CAPS, 'Native', 'Native runs');
	return items;
}

export interface SlashView {
	/** Breadcrumb of the current level, e.g. `/budget › Research ACME`. */
	title: string;
	items: SlashItem[];
	/** Shown when `items` is empty (loading, nothing to pick, or no match). */
	emptyText: string;
	/** A note above the rows (e.g. the current defaults could not be read). */
	note?: string;
}

/** The rows for the level `path` points at, filtered by `query`. */
export function slashViewFor(
	path: readonly SlashStep[],
	query: string,
	ctx: SlashCommandContext
): SlashView {
	const title = ['/', ...path.map((step) => step.label)].join(' › ').replace('/ › ', '');
	if (path.length === 0) {
		const items: SlashItem[] = SLASH_COMMANDS.map((command) => ({
			id: `command:${command.id}`,
			label: command.label,
			detail: command.detail,
			badge: 'Command',
			next: { id: command.id, token: command.token, label: command.label, command: command.id }
		}));
		return { title: 'Commands', items: filterSlashItems(items, query), emptyText: 'No matching command' };
	}
	const [root, ...rest] = path;
	const noMatch = 'No matching option';
	switch (root.command) {
		case 'engine':
		case 'run-engine': {
			if (ctx.engines === null) return { title, items: [], emptyText: 'Loading engines…' };
			const note = ctx.enginesError ? `Engine list unavailable: ${ctx.enginesError}` : undefined;
			if (root.command === 'run-engine') {
				const items = engineItems(ctx, ctx.runEngine, (row) => ({ type: 'run-engine', ...row }), 'run');
				return { title, items: filterSlashItems(items, query), emptyText: noMatch, note };
			}
			if (rest[0]?.scope === 'everywhere') {
				const items = engineItems(
					ctx,
					ctx.serverChatEngine,
					(row) => ({ type: 'chat-engine', ...row, scope: 'everywhere' }),
					'everywhere'
				);
				return { title, items: filterSlashItems(items, query), emptyText: noMatch, note };
			}
			const items: SlashItem[] = [
				...engineItems(ctx, ctx.chatEngine, (row) => ({ type: 'chat-engine', ...row, scope: 'device' }), 'device'),
				{
					id: 'engine:everywhere',
					label: 'Make default everywhere…',
					detail: 'Set the chat engine for every client, not just this browser',
					badge: 'All clients',
					searchText: 'everywhere default all clients server',
					next: { id: 'everywhere', token: 'everywhere', label: 'Default everywhere', scope: 'everywhere' }
				}
			];
			return { title: `${title} · this browser`, items: filterSlashItems(items, query), emptyText: noMatch, note };
		}
		case 'budget': {
			const taskStep = rest[0];
			if (!taskStep?.taskId) {
				const items: SlashItem[] = ctx.tasks.map((task) => ({
					id: `task:${task.id}`,
					label: task.title,
					detail: `${task.status} · ${task.id.slice(0, 8)}`,
					badge: 'Task',
					searchText: task.id,
					next: { id: task.id, token: task.id.slice(0, 8), label: task.title, taskId: task.id }
				}));
				return {
					title,
					items: filterSlashItems(items, query),
					emptyText: ctx.tasks.length === 0 ? 'No running or paused task in this chat' : noMatch
				};
			}
			const items: SlashItem[] = TASK_BUDGET_GRANTS.map((option) => ({
				id: `grant:${option.id}`,
				label: option.label,
				detail: option.detail,
				badge: 'Budget',
				action: {
					type: 'task-budget',
					taskId: taskStep.taskId as string,
					taskLabel: taskStep.label,
					grant: option.grant,
					grantLabel: option.label
				}
			}));
			return { title, items: filterSlashItems(items, query), emptyText: noMatch };
		}
		case 'budget-default': {
			if (ctx.budgetDefaults === null && !ctx.budgetDefaultsError) {
				return { title, items: [], emptyText: 'Loading current defaults…' };
			}
			const note = ctx.budgetDefaultsError
				? `Current defaults unavailable: ${ctx.budgetDefaultsError}`
				: undefined;
			return { title, items: filterSlashItems(budgetDefaultItems(ctx), query), emptyText: noMatch, note };
		}
		default:
			return { title, items: [], emptyText: noMatch };
	}
}

// ─── Task candidates for /budget ────────────────────────────────────────────

const BUDGETABLE_STATUSES = new Set(['running', 'in_progress', 'paused']);

/** The chat's running/paused tasks, newest first: the union of the transcript's
 *  task cards (latest card per task wins) and this thread's `$taskStore` rows.
 *  The store's status is fresher than an old card, so it wins when both know a
 *  task. */
export function budgetTaskCandidates(
	cards: Iterable<{
		content: { type: string; task_id?: string; status?: string; display_label?: string; label?: string };
		created_at?: number;
	}>,
	tasks: ReadonlyArray<{ id: string; title?: string; status: string; createdAt?: string; updatedAt?: string }>
): SlashBudgetTask[] {
	const byId = new Map<string, { id: string; title: string; status: string; at: number }>();
	for (const card of cards) {
		if (card.content.type !== 'task_status_update') continue;
		const id = card.content.task_id?.trim();
		const status = card.content.status?.trim();
		if (!id || !status) continue;
		const prior = byId.get(id);
		byId.set(id, {
			id,
			title: card.content.display_label?.trim() || card.content.label?.trim() || prior?.title || '',
			status,
			at: Math.max(prior?.at ?? 0, card.created_at ?? 0)
		});
	}
	for (const task of tasks) {
		const prior = byId.get(task.id);
		const at = Date.parse(task.createdAt ?? '') || 0;
		byId.set(task.id, {
			id: task.id,
			title: task.title?.trim() || prior?.title || '',
			status: task.status,
			at: Math.max(prior?.at ?? 0, at)
		});
	}
	return [...byId.values()]
		.filter((task) => BUDGETABLE_STATUSES.has(task.status))
		.sort((a, b) => b.at - a.at)
		.map((task) => ({ id: task.id, title: task.title || `Task ${task.id.slice(0, 8)}`, status: task.status }));
}
