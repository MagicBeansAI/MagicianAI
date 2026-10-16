/**
 * The drawer's heading and brief, as functions.
 *
 * A task reaches the drawer with whatever title its surface had. For a run
 * started by an agent that is often the execution id (`Run execution_…`) or
 * nothing, while the prompt carries a routing directive the reader never wrote:
 * `web-researcher / Do not delegate further. Use browser … to search …`. These
 * helpers turn that into a human title without ever overriding a real one.
 *
 * Deliberately conservative and generic — no task-kind input, no agent list.
 * See `docs/components/unified-ui/unified-task-panel.md`, *The header card*.
 */

/** Longest derived title, in characters, before it is cut at a word boundary. */
export const TITLE_MAX_CHARS = 90;

/** `<agent-slug> / ` at the very start of a prompt: the routing prefix. */
const ROUTING_PREFIX = /^[a-z0-9]+(?:[-_][a-z0-9]+)*\s+\/\s+/;

/** A leading meta sentence about *how* to run, not *what* to do. */
const META_SENTENCE = /^(?:do\s+not|don['’]t|never)\b[^.!?\n]*[.!?]?\s*/i;

/** "Use (the) browser … to <goal>" keeps the goal; a bare "Use the browser." goes. */
const BROWSER_LEAD = /^use\s+(?:the\s+)?browser\b[^.!?\n]*?\bto\s+/i;
const BROWSER_SENTENCE = /^use\s+(?:the\s+)?browser\b(?:(?!\bto\s)[^.!?\n])*[.!?]\s*/i;

/** Words a surface prefixes onto an id to make a title out of it. */
const ID_PREFIX = /^(?:run|task|execution|job)\s+/i;

/**
 * Whether `text` is an identifier rather than prose: the task's own id (or a
 * string carrying it), or a single whitespace-free token that has digits in it
 * and is long enough to be generated — `execution_1f2e…`, a UUID, the drawer's
 * own elided `executio…7bba`. Short tokens with digits (`Q3`, `GPT-5`) are prose.
 */
export function looksLikeId(text: string | null | undefined, id: string | null | undefined): boolean {
	const value = (text ?? '').trim();
	if (value === '') return false;
	if (id && id.trim() !== '' && value.includes(id.trim())) return true;
	const token = value.replace(ID_PREFIX, '');
	if (/\s/.test(token)) return false;
	return token.length >= 8 && /\d/.test(token);
}

/** Strip the routing prefix and any directive sentences that follow it. */
function stripDirectives(text: string): string {
	let rest = text.trim().replace(ROUTING_PREFIX, '');
	for (;;) {
		const next = rest.replace(META_SENTENCE, '').replace(BROWSER_SENTENCE, '');
		if (next === rest) break;
		rest = next;
	}
	return rest.trim();
}

function capitalise(text: string): string {
	return text.charAt(0).toUpperCase() + text.slice(1);
}

function capAtWord(text: string, max: number): string {
	if (text.length <= max) return text;
	const cut = text.slice(0, max);
	const space = cut.lastIndexOf(' ');
	return `${(space > 0 ? cut.slice(0, space) : cut).trimEnd()}…`;
}

/**
 * A human title from a prompt: routing prefix and directive sentences dropped,
 * "use the browser … to" folded into the verb it introduces, the first line's
 * first sentence, capped at ~90 characters on a word boundary. `null` when no
 * prose is left.
 */
export function titleFromPrompt(prompt: string | null | undefined): string | null {
	if (!prompt) return null;
	let rest = stripDirectives(prompt);
	// The goal's verb was lower-case inside the directive; it now opens the title.
	if (BROWSER_LEAD.test(rest)) rest = capitalise(rest.replace(BROWSER_LEAD, ''));
	const firstLine = rest.split('\n').find((line) => line.trim() !== '') ?? '';
	const sentence = (firstLine.trim().split(/(?<=[.!?])\s+/)[0] ?? '')
		.replace(/[.!?…]+$/, '')
		.replace(/\s+/g, ' ')
		.trim();
	if (sentence === '' || looksLikeId(sentence, null)) return null;
	return capAtWord(sentence, TITLE_MAX_CHARS);
}

export interface TaskTitleInput {
	/** A display label the model carries, if any — preferred over everything. */
	label?: string | null;
	/** The title the surface passed. A real one is never overridden. */
	title: string | null;
	/** The task's prompt / description. */
	description: string | null;
	/** The task id, so a title that is just the id can be recognised. */
	id: string | null;
}

/**
 * Whether `text` opens with a routing prefix (`<lowercase-slug> / `): prompt
 * text a surface passed along as a title, not a title a person wrote.
 */
export function isRoutedPrompt(text: string | null | undefined): boolean {
	return ROUTING_PREFIX.test((text ?? '').trim());
}

/**
 * The drawer heading. A real `label` or `title` wins as given; a missing, blank
 * or id-shaped one is replaced by the prompt's goal. A label or title that opens
 * with a routing prefix (`web-researcher / …`) is prompt text, so it goes
 * through `titleFromPrompt` like the description does. When there is no prose to
 * use, the surface's title comes back unchanged (possibly `null`).
 */
export function deriveTaskTitle({ label, title, description, id }: TaskTitleInput): string | null {
	const shownLabel = label?.trim() ?? '';
	if (shownLabel !== '' && !looksLikeId(shownLabel, id)) {
		if (!isRoutedPrompt(shownLabel)) return shownLabel;
		const fromLabel = titleFromPrompt(shownLabel);
		if (fromLabel !== null) return fromLabel;
	}
	if (title !== null && title.trim() !== '' && !looksLikeId(title, id)) {
		if (!isRoutedPrompt(title)) return title;
		const fromTitle = titleFromPrompt(title);
		if (fromTitle !== null) return fromTitle;
	}
	return titleFromPrompt(description) ?? title;
}

const comparable = (text: string): string =>
	text.toLowerCase().replace(/[.!?…]+$/, '').replace(/\s+/g, ' ').trim();

/**
 * The description row under the title. A prompt that opens with a routing
 * prefix (`slug / …`) loses it and the directive sentences behind it; any other
 * description is shown exactly as written. `null` when nothing is left or the
 * brief would only repeat the title.
 */
export function deriveTaskBrief(
	description: string | null | undefined,
	title: string | null
): string | null {
	const raw = (description ?? '').trim();
	if (raw === '') return null;
	const brief = ROUTING_PREFIX.test(raw) ? stripDirectives(raw) : raw;
	if (brief === '') return null;
	if (title !== null && comparable(brief) === comparable(title)) return null;
	return brief;
}

/**
 * A short, human-scannable form of a generated id: any leading word prefix
 * (`task_`, `execution-`) dropped, then the first six characters —
 * `task_2679cefd6f…` → `2679ce`. Used where an id has to stand in for a title.
 */
export function shortTaskId(id: string | null | undefined): string | null {
	const value = (id ?? '').trim();
	if (value === '') return null;
	const bare = value.replace(/^[a-z]+[_-](?=[a-z0-9])/i, '');
	return bare.slice(0, 6);
}

/**
 * A title for a task that only has an id and maybe a label (the chat live
 * card): the label through `deriveTaskTitle`'s rules, else `Task · <short id>`,
 * else `Task`. Never the raw id.
 */
export function labelOrShortIdTitle(
	label: string | null | undefined,
	id: string | null | undefined
): string {
	const derived = deriveTaskTitle({ label: label ?? null, title: null, description: null, id: id ?? null });
	if (derived !== null && derived.trim() !== '') return derived;
	const short = shortTaskId(id);
	return short ? `Task · ${short}` : 'Task';
}
