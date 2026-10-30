import { timedFetch } from '$lib/shared/fetch';
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

/** Goal notes: markdown under Workspaces/<name>/Goals/ with YAML frontmatter.
 * Only the flat fields the goal template writes are read; anything else is ignored. */
export type GoalKind = 'main' | 'sub' | 'peer';
export type GoalVisibility = 'private' | 'internal' | 'public';
export interface Goal {
	name: string; title: string; path: string; workspace: string | null;
	kind: GoalKind; parent: string | null; peers: string[]; owner: string | null;
	status: string; target: string | null; visibility: GoalVisibility;
	program: string | null; goalId: string | null; metrics: string[];
	/** `closed:` — the date (`YYYY-MM-DD`) the goal was marked done or dropped, when recorded. */
	closed: string | null;
	/** Content revision of the note as read (`GET /notes/file`). An edit sends it back so a
	 * change made since the read (by an agent or another device) is refused, not overwritten. */
	revision?: string;
}
/** `closedParent` names the closed goal an open goal was moved up from (`openGoalTree`). */
export interface GoalNode { goal: Goal; children: GoalNode[]; closedParent?: string }
/** Why a goal heads a "Not linked" tree instead of hanging under a main goal. */
export type OrphanReason = 'missing-parent' | 'self-parent' | 'parent-loop';
export interface OrphanNode extends GoalNode { reason: OrphanReason }
/** Every goal appears exactly once: under a main goal or under one orphan root.
 * `byKey` resolves links by `goalKey`; the first goal listed wins a shared key. */
export interface GoalTree { roots: GoalNode[]; orphans: OrphanNode[]; byKey: Map<string, Goal> }

export function linkTarget(value: string): string {
	const match = value.trim().match(/^\[\[([^\]|]+)(?:\|[^\]]*)?\]\]$/);
	return (match ? match[1] : value).trim();
}

/** The note a link names: `[[Goals/Name#part|alias]]`, `Name.md` and `name` all name the note `Name`. */
export function goalKey(link: string): string {
	return linkTarget(link).split('#')[0].split('/').pop()!.replace(/\.md$/i, '').trim().toLowerCase();
}

function unquote(value: string): string {
	const v = value.trim();
	if (v.startsWith('"') && v.endsWith('"')) {
		try { return JSON.parse(v); } catch { /* Accept existing YAML quoting below. */ }
	}
	if (v.startsWith("'") && v.endsWith("'")) return v.slice(1, -1).replace(/''/g, "'");
	return v.length > 1 && ((v.startsWith('"') && v.endsWith('"')) || (v.startsWith("'") && v.endsWith("'"))) ? v.slice(1, -1) : v;
}

/** Splits at `separator` outside quotes and brackets and drops a ` #` comment.
 * A quote only opens at the start of a value, so `Founder's goal` stays plain. */
function scan(text: string, separator = ''): string[] {
	const parts: string[] = [];
	let quote = '', depth = 0, start = 0, fresh = true;
	for (let i = 0; i < text.length; i++) {
		const c = text[i];
		if (quote) { if (c === quote) quote = ''; continue; }
		if ((c === '"' || c === "'") && fresh) quote = c;
		else if (c === '#' && (i === 0 || /\s/.test(text[i - 1]))) return [...parts, text.slice(start, i)];
		else if (c === '[') depth++;
		else if (c === ']') depth = Math.max(0, depth - 1);
		else if (separator && c === separator && depth === 0) { parts.push(text.slice(start, i)); start = i + 1; fresh = true; continue; }
		if (!/\s/.test(c)) fresh = c === '[' || (c === ',' && depth > 0);
	}
	return [...parts, text.slice(start)];
}

function frontmatter(markdown: string): { fields: Record<string, string | string[]>; body: string } | null {
	const source = markdown.replace(/^\uFEFF/, '');
	const match = source.match(/^---[ \t]*\r?\n([\s\S]*?)\r?\n---[ \t]*(?:\r?\n|$)/);
	if (!match) return null;
	const fields: Record<string, string | string[]> = {};
	let listKey: string | null = null;
	for (const line of match[1].split(/\r?\n/)) {
		const item = line.match(/^\s*-\s+(.*)$/);
		if (item) {
			const value = unquote(scan(item[1])[0]);
			if (listKey && value) (fields[listKey] as string[]).push(value);
			continue;
		}
		const pair = line.match(/^([A-Za-z_][\w-]*):\s*(.*)$/);
		if (!pair) continue;
		const [, key, raw] = pair;
		const value = scan(raw)[0].trim();
		listKey = null;
		if (value === '') { fields[key] = []; listKey = key; }
		// A flow list, unless the whole value is one bare `[[link]]`.
		else if (value.startsWith('[') && value.endsWith(']') && !/^\[\[[^[\]]*\]\]$/.test(value)) {
			fields[key] = scan(value.slice(1, -1), ',').map(unquote).filter(Boolean);
		} else fields[key] = unquote(value);
	}
	return { fields, body: source.slice(match[0].length) };
}

const text = (v: string | string[] | undefined) => typeof v === 'string' && v && v !== '~' && v !== 'null' ? v : null;
const list = (v: string | string[] | undefined) => Array.isArray(v) ? v : text(v) ? [v as string] : [];

export function parseGoalNote(markdown: string, path: string): Goal | null {
	const note = frontmatter(markdown);
	if (!note || note.fields.magician_kind !== 'goal') return null;
	const { fields, body } = note;
	const name = path.split('/').pop()!.replace(/\.md$/i, '');
	// Only an H1 on the first non-blank line is the title; otherwise the file name is.
	const heading = body.split(/\r?\n/).find(line => line.trim())?.match(/^#[ \t]+(.+)$/)?.[1]?.trim();
	const parent = text(fields.parent);
	const kind = (['main', 'sub', 'peer'] as const).find(k => k === text(fields.kind)?.toLowerCase()) ?? (parent ? 'sub' : 'main');
	const visibility = (['private', 'internal', 'public'] as const).find(v => v === text(fields.visibility)?.toLowerCase()) ?? 'private';
	return {
		name, title: heading || name, path, workspace: text(fields.workspace), kind,
		parent: parent ? linkTarget(parent) : null,
		peers: list(fields.peers).map(linkTarget), owner: text(fields.owner),
		status: text(fields.status)?.toLowerCase() ?? 'proposed', target: text(fields.target), visibility,
		program: text(fields.program), goalId: text(fields.goal_id), metrics: list(fields.metrics),
		closed: text(fields.closed)
	};
}

/** `done` and `dropped` goals are closed: the goal screens list them only under Closed. */
export const CLOSED_GOAL_STATUSES: readonly string[] = ['done', 'dropped'];
export const isClosedGoal = (goal: Pick<Goal, 'status'>): boolean => CLOSED_GOAL_STATUSES.includes(goal.status);

/** The tree without closed goals. An open goal under a closed one moves up to its nearest open
 * ancestor, or heads a tree of its own, and names the closed goal it sat under. */
export function openGoalTree(tree: GoalTree): GoalTree {
	const prune = (nodes: GoalNode[], closedParent?: string): GoalNode[] => nodes.flatMap(node => isClosedGoal(node.goal)
		? prune(node.children, node.goal.title)
		: [{ goal: node.goal, children: prune(node.children), ...(closedParent ? { closedParent } : {}) }]);
	return {
		roots: prune(tree.roots),
		orphans: tree.orphans.flatMap(orphan => prune([orphan]).map(node => ({ ...node, reason: orphan.reason }))),
		byKey: tree.byKey
	};
}

/** Closed goals, most recently closed first; those without a `closed:` date last, by title. */
export function closedGoals(goals: Goal[]): Goal[] {
	return goals.filter(isClosedGoal).sort((a, b) => (b.closed ?? '').localeCompare(a.closed ?? '') || a.title.localeCompare(b.title));
}

/** A workspace read only in part, or with notes that could not be read, may hold goals the
 * screen did not see, so both lists keep it to show why. */
const readInPart = (space: GoalWorkspace): boolean => space.problems.length > 0 || space.notShown > 0 || space.foldersNotRead > 0;
/** A workspace whose goals are all closed is listed only under Closed. One with no goals yet stays
 * in the open list, so it can be filled. */
export const showsInOpenList = (space: GoalWorkspace): boolean =>
	readInPart(space) || !space.goals.length || space.goals.some(goal => !isClosedGoal(goal));
export const showsInClosedList = (space: GoalWorkspace): boolean => readInPart(space) || space.goals.some(isClosedGoal);

export function buildGoalTree(goals: Goal[]): GoalTree {
	const nodes: GoalNode[] = goals.map(goal => ({ goal, children: [] }));
	const order = new Map<Goal, number>(goals.map((goal, index) => [goal, index]));
	const byKey = new Map<string, GoalNode>();
	for (const node of nodes) if (!byKey.has(goalKey(node.goal.name))) byKey.set(goalKey(node.goal.name), node);
	const parentOf = new Map<GoalNode, GoalNode>();
	const mains: GoalNode[] = [];
	const heads: Array<{ node: GoalNode; reason: OrphanReason }> = [];
	for (const node of nodes) {
		if (!node.goal.parent) { mains.push(node); continue; }
		const parent = byKey.get(goalKey(node.goal.parent));
		if (!parent) heads.push({ node, reason: 'missing-parent' });
		else if (parent === node) heads.push({ node, reason: 'self-parent' });
		else { parent.children.push(node); parentOf.set(node, parent); }
	}
	// Copies each subtree once; a link back into a placed goal (a closed loop) is dropped.
	const placed = new Set<GoalNode>();
	function place(node: GoalNode): GoalNode {
		const copy: GoalNode = { goal: node.goal, children: [] };
		placed.add(node);
		const stack: Array<[GoalNode, GoalNode]> = [[node, copy]];
		while (stack.length) {
			const [from, to] = stack.pop()!;
			for (const child of from.children) {
				if (placed.has(child)) continue;
				placed.add(child);
				const next: GoalNode = { goal: child.goal, children: [] };
				to.children.push(next);
				stack.push([child, next]);
			}
		}
		return copy;
	}
	const roots = mains.map(place);
	const orphans: OrphanNode[] = heads.map(({ node, reason }) => ({ ...place(node), reason }));
	// What is left hangs off a parent loop. Each loop is headed by its first member in goalKey order.
	for (const node of nodes) {
		if (placed.has(node)) continue;
		const walked = new Set<GoalNode>();
		let at = node;
		while (!walked.has(at)) { walked.add(at); at = parentOf.get(at)!; }
		const loop = [at];
		for (let next = parentOf.get(at)!; next !== at; next = parentOf.get(next)!) loop.push(next);
		const head = loop.reduce((best, member) => {
			const a = goalKey(member.goal.name), b = goalKey(best.goal.name);
			return a < b || (a === b && member.goal.path < best.goal.path) ? member : best;
		});
		orphans.push({ ...place(head), reason: 'parent-loop' });
	}
	orphans.sort((a, b) => order.get(a.goal)! - order.get(b.goal)!);
	return { roots, orphans, byKey: new Map<string, Goal>([...byKey].map(([key, node]) => [key, node.goal])) };
}

export function orphanNote(orphan: OrphanNode): string {
	if (orphan.reason === 'missing-parent') return `Parent goal not found: ${orphan.goal.parent}`;
	if (orphan.reason === 'self-parent') return 'Names itself as its parent.';
	return 'Part of a parent loop: its parent goals lead back to it.';
}

/** Program documents are bare `*.md` names; a goal may name one without the extension. */
export function goalProgramName(program: string): string {
	return /\.md$/i.test(program) ? program : `${program}.md`;
}

export function goalProgramHref(program: string): string {
	return `/work?view=programs&program=${encodeURIComponent(goalProgramName(program))}`;
}

const NOTES = '/api/magician/v2/notes';
const WORKSPACES = 'Workspaces';

export function goalNoteName(value: string): string {
	const name = value.trim();
	if (!name || name.length > 160 || /[\\/\[\]#\x00-\x1f\x7f]/.test(name) || name === '.' || name === '..') {
		throw new Error('Use a name of up to 160 characters without slashes, brackets, # or control characters.');
	}
	return name;
}

/** Each creation is one scoped request. The server publishes complete content
 * without replacing an existing file; creating notes never schedules work. */
export async function createGoalFolder(name: string, signal?: AbortSignal): Promise<void> {
	const response = await timedFetch(`${NOTES}/tree`, {
		method: 'POST', headers: scopedRequestHeaders({ 'content-type': 'application/json' }), signal,
		body: JSON.stringify({ folder: WORKSPACES, name: goalNoteName(name) })
	});
	if (!response.ok) throw await failure(response, 'create folder');
}

export interface NewGoal {
	workspace: string; title: string; parent?: Goal; target?: string; description?: string; program?: string;
}

export async function createGoalNote(input: NewGoal, signal?: AbortSignal): Promise<void> {
	const workspace = goalNoteName(input.workspace), name = goalNoteName(input.title);
	const program = input.program?.trim() || '';
	if (program && (!program.endsWith('.md') || program.startsWith('.') || program.includes('..') || /[\\/\x00-\x1f\x7f]/.test(program))) {
		throw new Error('Choose a playbook document from this workspace.');
	}
	const root = `${WORKSPACES}/${workspace}/Goals/`;
	if (input.parent && !input.parent.path.startsWith(root)) throw new Error('Choose a parent from this folder.');
	const folder = input.parent ? input.parent.path.slice(0, input.parent.path.lastIndexOf('/')) : `${root}${name}`;
	const markdown = [
		'---', 'magician_kind: goal', `workspace: ${JSON.stringify(workspace)}`,
		`kind: ${input.parent ? 'sub' : 'main'}`,
		...(input.parent ? [`parent: ${JSON.stringify(`[[${input.parent.name}]]`)}`] : []),
		'owner: user', 'status: proposed', 'visibility: private',
		...(program ? [`program: ${JSON.stringify(program)}`] : []),
		...(input.target ? [`target: ${JSON.stringify(input.target)}`] : []),
		'---', `# ${name}`, '', input.description?.trim() || '', ''
	].join('\n');
	// A distinct endpoint also fails safely against an older backend: it must
	// never silently ignore the initial content and create a non-goal note.
	const response = await timedFetch(`${NOTES}/file/content`, {
		method: 'POST', headers: scopedRequestHeaders({ 'content-type': 'application/json' }), signal,
		body: JSON.stringify({ folder, name, markdown })
	});
	if (!response.ok) throw await failure(response, 'create goal');
}

type NoteEntry = { name: string; relative_path: string; kind: 'dir' | 'file'; has_children?: boolean };

/** How much of one workspace's `Goals/` the screen reads. Each main goal has its own folder
 * (`Goals/<Main goal>/`), so a folder is one listing request and a note one read request.
 * `notesPerFolder` caps the notes read from any one folder (the folder's own main-goal note first). */
export interface GoalReadLimits { folderDepth: number; folders: number; notes: number; notesPerFolder?: number }
export const GOAL_READ_LIMITS: GoalReadLimits = { folderDepth: 3, folders: 1000, notes: 2000 };
/** The All work landing card shows 3 open main goals with up to 2 sub-goals each. A main goal's
 * sub-goals sit flat in its folder, so 6 folders one level deep (closed main goals still take a
 * folder), 3 notes from each (the main goal and 2 sub-goals) and 3 older notes directly in
 * `Goals/`: at most 7 listings and 21 reads per workspace instead of the full screen's 1000
 * folders and 2000 notes. */
export const COMPACT_GOAL_READ_LIMITS: GoalReadLimits = { folderDepth: 1, folders: 6, notes: 21, notesPerFolder: 3 };

/** `problems` are notes or folders in this workspace that could not be read; the rest still show.
 * `notShown` counts notes past the note limits; `foldersNotRead` counts folders past the depth or folder limit.
 * `notReadIn` counts, per folder path, the notes the per-folder limit left unread. */
export interface GoalWorkspace {
	name: string; path: string; goals: Goal[]; problems: string[];
	notShown: number; foldersNotRead: number; notReadIn?: Record<string, number>;
}

const message = (error: unknown) => error instanceof Error ? error.message : String(error);

async function failure(response: Response, what: string): Promise<Error> {
	const body = await response.json().catch(() => null) as { message?: string } | null;
	return new Error(body?.message || `Could not ${what} (${response.status}).`);
}

/** `null` when the folder does not exist. */
async function noteTree(path: string, signal?: AbortSignal): Promise<NoteEntry[] | null> {
	const response = await timedFetch(`${NOTES}/tree?path=${encodeURIComponent(path)}`, { signal });
	if (response.status === 404) return null;
	if (!response.ok) throw await failure(response, `list ${path}`);
	return ((await response.json()) as { entries?: NoteEntry[] }).entries ?? [];
}

/** A folder that is missing (removed since it was listed) is empty. */
async function listFolder(path: string, signal?: AbortSignal): Promise<{ entries: NoteEntry[]; problem: string | null }> {
	try {
		return { entries: (await noteTree(path, signal)) ?? [], problem: null };
	} catch (error) {
		if (signal?.aborted) throw error;
		return { entries: [], problem: message(error) };
	}
}

async function readGoalNote(path: string, signal?: AbortSignal): Promise<{ goal: Goal | null; problem: string | null }> {
	try {
		const response = await timedFetch(`${NOTES}/file?path=${encodeURIComponent(path)}`, { signal });
		// Removed between the listing and the read.
		if (response.status === 404) return { goal: null, problem: null };
		if (!response.ok) throw await failure(response, `read ${path}`);
		const note = await response.json() as { markdown?: unknown; revision?: unknown };
		if (typeof note.markdown !== 'string') throw new Error(`Could not read ${path}.`);
		const goal = parseGoalNote(note.markdown, path);
		if (goal && typeof note.revision === 'string') goal.revision = note.revision;
		return { goal, problem: null };
	} catch (error) {
		if (signal?.aborted) throw error;
		return { goal: null, problem: message(error) };
	}
}

/** At most four notes requests at once: long-lived streams already hold some of the browser's connections. */
async function inPool<T, R>(items: T[], run: (item: T) => Promise<R>): Promise<R[]> {
	const results = new Array<R>(items.length);
	let next = 0;
	await Promise.all(Array.from({ length: Math.min(4, items.length) }, async () => {
		while (next < items.length) { const index = next++; results[index] = await run(items[index]); }
	}));
	return results;
}

interface GoalFolders { root: string; listed: Map<string, NoteEntry[]>; problems: string[]; folders: number; foldersNotRead: number }

/** Lists every workspace's `Goals/` folder tree, one depth level at a time across all workspaces,
 * so a level's listings share one request pool. A folder the API reports as empty is not listed. */
async function listGoalFolders(spaces: NoteEntry[], limits: GoalReadLimits, signal?: AbortSignal): Promise<GoalFolders[]> {
	const walks: GoalFolders[] = spaces.map(space => ({ root: `${space.relative_path}/Goals`, listed: new Map(), problems: [], folders: 0, foldersNotRead: 0 }));
	let level = walks.map((walk, space) => ({ space, path: walk.root, depth: 0 }));
	while (level.length) {
		const results = await inPool(level, folder => listFolder(folder.path, signal));
		const next: typeof level = [];
		level.forEach((folder, i) => {
			const walk = walks[folder.space];
			const { entries, problem } = results[i];
			if (problem) walk.problems.push(problem);
			walk.listed.set(folder.path, entries);
			for (const entry of entries) {
				if (entry.kind !== 'dir' || entry.has_children === false) continue;
				if (folder.depth + 1 > limits.folderDepth || walk.folders >= limits.folders) { walk.foldersNotRead++; continue; }
				walk.folders++;
				next.push({ space: folder.space, path: entry.relative_path, depth: folder.depth + 1 });
			}
		});
		level = next;
	}
	return walks;
}

/** Markdown notes in reading order: the main-goal folders in `Goals/` first, then legacy notes
 * directly in `Goals/`. Inside a folder its notes come before its subfolders, and the note named
 * after the folder (the main goal) leads. */
function goalNotePaths(walk: GoalFolders, perFolder = Infinity): { paths: string[]; notReadIn: Record<string, number> } {
	const paths: string[] = [];
	const notReadIn: Record<string, number> = {};
	const visit = (path: string) => {
		const entries = walk.listed.get(path) ?? [];
		const folder = path.split('/').pop()!.toLowerCase();
		const own = (entry: NoteEntry) => entry.name.replace(/\.md$/i, '').toLowerCase() === folder;
		const notes = entries.filter(entry => entry.kind === 'file' && /\.md$/i.test(entry.name));
		const subfolders = () => { for (const entry of entries) if (entry.kind === 'dir' && walk.listed.has(entry.relative_path)) visit(entry.relative_path); };
		if (path === walk.root) subfolders();
		const ordered = [...notes.filter(own), ...notes.filter(entry => !own(entry))];
		if (ordered.length > perFolder) notReadIn[path] = ordered.length - perFolder;
		paths.push(...ordered.slice(0, perFolder).map(entry => entry.relative_path));
		if (path !== walk.root) subfolders();
	};
	visit(walk.root);
	return { paths, notReadIn };
}

/** Every folder under `Workspaces/` with the goal notes under its `Goals/` folder: one folder per
 * main goal (`Goals/<Main goal>/`, its sub-goals flat beside it) and legacy notes directly in `Goals/`.
 * The `parent:` field, not the folder, places a goal in the tree. No `Workspaces/` folder is no workspaces. */
export async function loadGoalWorkspaces(signal?: AbortSignal, limits: GoalReadLimits = GOAL_READ_LIMITS): Promise<GoalWorkspace[]> {
	const spaces = ((await noteTree(WORKSPACES, signal)) ?? []).filter(entry => entry.kind === 'dir');
	const walks = await listGoalFolders(spaces, limits, signal);
	const listed = walks.map(walk => goalNotePaths(walk, limits.notesPerFolder));
	const files = listed.flatMap(({ paths }, space) => paths.slice(0, limits.notes).map(path => ({ space, path })));
	const notes = await inPool(files, file => readGoalNote(file.path, signal));
	return spaces.map((space, index) => {
		const own = notes.filter((_, i) => files[i].space === index);
		const { paths, notReadIn } = listed[index];
		const perFolderSkipped = Object.values(notReadIn).reduce((sum, count) => sum + count, 0);
		return {
			name: space.name, path: space.relative_path,
			goals: own.flatMap(note => note.goal ? [note.goal] : []),
			problems: [...walks[index].problems, ...own.map(note => note.problem)].filter((p): p is string => !!p),
			notShown: Math.max(0, paths.length - limits.notes) + perFolderSkipped,
			foldersNotRead: walks[index].foldersNotRead,
			...(perFolderSkipped ? { notReadIn } : {})
		};
	});
}

/** Sub-goals the per-folder limit left unread beside a main goal: the notes not read in the
 * folder the goal is named after (`Goals/<Main goal>/<Main goal>.md`). */
export function subGoalsNotRead(workspace: GoalWorkspace, goal: Goal): number {
	const folder = goal.path.slice(0, goal.path.lastIndexOf('/'));
	const own = folder.split('/').pop()!.toLowerCase() === goal.name.toLowerCase();
	return own ? workspace.notReadIn?.[folder] ?? 0 : 0;
}

/** Recently read goal workspaces per scope and limits, for the All work landing card, which
 * remounts whenever the page around it changes. Explicit Refresh and every edit bypass it. */
const GOAL_CACHE_MS = 60_000;
let goalCache: { key: string; at: number; workspaces: GoalWorkspace[] } | null = null;
const cacheKey = (scope: string, limits: GoalReadLimits) => JSON.stringify([scope, limits]);

export function cachedGoalWorkspaces(scope: string, limits: GoalReadLimits, now = Date.now()): GoalWorkspace[] | null {
	return goalCache && goalCache.key === cacheKey(scope, limits) && now - goalCache.at < GOAL_CACHE_MS ? goalCache.workspaces : null;
}
export function rememberGoalWorkspaces(scope: string, limits: GoalReadLimits, workspaces: GoalWorkspace[], now = Date.now()): void {
	goalCache = { key: cacheKey(scope, limits), at: now, workspaces };
}
export function forgetGoalWorkspaces(): void { goalCache = null; }

/** A real calendar date written `YYYY-MM-DD`. */
export function isGoalTargetDate(value: string): boolean {
	const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
	if (!match) return false;
	const [year, month, day] = match.slice(1).map(Number);
	const date = new Date(Date.UTC(year, month - 1, day));
	return date.getUTCFullYear() === year && date.getUTCMonth() === month - 1 && date.getUTCDate() === day;
}

export type GoalTargetResult =
	| { ok: true; revision: string | null }
	| { ok: false; reason: 'invalid' | 'conflict' | 'failed'; message: string };

/** Changes only the named front-matter keys through `PATCH /notes/front-matter`. The server keeps
 * every other line and the body as they are, and refuses with 409 when the note changed since
 * `goal.revision` was read. */
async function patchGoal(goal: Pick<Goal, 'path' | 'revision'>, update: { set?: Record<string, string>; unset?: string[] }, what: string, signal?: AbortSignal): Promise<GoalTargetResult> {
	if (!goal.revision) return { ok: false, reason: 'conflict', message: 'This goal has no saved revision yet. Reload the goals and try again.' };
	try {
		const response = await timedFetch(`${NOTES}/front-matter`, {
			method: 'PATCH',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ path: goal.path, expected_revision: goal.revision, ...update }),
			signal
		});
		if (response.status === 409) return { ok: false, reason: 'conflict', message: 'This goal changed since it was loaded.' };
		if (!response.ok) return { ok: false, reason: 'failed', message: (await failure(response, what)).message };
		const saved = await response.json().catch(() => null) as { revision?: unknown } | null;
		forgetGoalWorkspaces();
		return { ok: true, revision: typeof saved?.revision === 'string' ? saved.revision : null };
	} catch (error) {
		if (signal?.aborted) throw error;
		return { ok: false, reason: 'failed', message: message(error) };
	}
}

/** Set (`YYYY-MM-DD`) or clear a goal's `target:`. */
export async function updateGoalTarget(goal: Pick<Goal, 'path' | 'revision'>, target: string | null, signal?: AbortSignal): Promise<GoalTargetResult> {
	const value = target?.trim() || null;
	if (value && !isGoalTargetDate(value)) return { ok: false, reason: 'invalid', message: 'Enter a real date as YYYY-MM-DD.' };
	return patchGoal(goal, value ? { set: { target: value } } : { unset: ['target'] }, 'save the date', signal);
}

/** Today in the browser's time zone, written `YYYY-MM-DD`. */
export function localDate(now = new Date()): string {
	return `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}-${String(now.getDate()).padStart(2, '0')}`;
}

/** Mark a goal done, stamping `closed:` with today's date for the Closed list, or reopen it as
 * `active` and remove `closed:`. */
export async function updateGoalStatus(goal: Pick<Goal, 'path' | 'revision'>, status: 'done' | 'active', signal?: AbortSignal, today = localDate()): Promise<GoalTargetResult> {
	return status === 'done'
		? patchGoal(goal, { set: { status: 'done', closed: today } }, 'mark the goal done', signal)
		: patchGoal(goal, { set: { status: 'active' }, unset: ['closed'] }, 'reopen the goal', signal);
}
