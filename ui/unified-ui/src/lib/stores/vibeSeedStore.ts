/**
 * VibeDev seed store (M4 — cross-surface ingress).
 *
 * A one-shot draft store that lets any surface (a chat thread, a meeting
 * transcript, …) open the VibeDev cockpit *seeded* with that context:
 * `putSeed(draft)` stashes the payload in `sessionStorage` and returns a short
 * id; the surface navigates to `/vibe?seed=<id>`; the cockpit `takeSeed(id)`s it
 * on mount (read-and-DELETE, so a refresh can't re-inject a stale build) and
 * surfaces it as a dismissible "Seeded context" chip. The seed flows into the
 * run only through `submit.ts`'s `seedContextBlock`, exactly like the project
 * context block — the user's free-text ask stays clean.
 *
 * Ephemeral by design: the URL carries only an id (transcripts blow past URL
 * limits), the payload never persists past consumption, and there is no backend.
 */
import { browser } from '$app/environment';

export type VibeSeedSource = 'chat' | 'meeting' | 'observation' | 'voice';

export interface VibeSeedDraft {
	id: string;
	source: VibeSeedSource;
	/** Chip text, e.g. "design-sync · 2026-06-14" or "Chat thread". */
	label: string;
	/**
	 * The originating surface's id (meeting `thread_id` / chat `session_id`), for a durable
	 * reverse link from the built project back to its source (§13.3 #20). Optional.
	 */
	sourceId?: string;
	/** The serialized seed block body (transcript / messages). */
	content: string;
	/** Pre-fills the composer (editable, never auto-submits). */
	suggestedPrompt?: string;
	createdAt: number;
}

export interface SeedTurn {
	role: string;
	text: string;
}

const KEY_PREFIX = 'vibe.seed.';
const MAX_TURNS = 30;
const MAX_TURN_CHARS = 1200;
const MAX_SUMMARY_CHARS = 4000;
const MAX_CONTENT_CHARS = 24_000;

function newId(): string {
	const rand =
		browser && typeof crypto !== 'undefined' && 'randomUUID' in crypto
			? crypto.randomUUID().slice(0, 8)
			: Math.random().toString(36).slice(2, 10);
	return `seed-${rand}`;
}

/** Stash a seed draft; returns the id to put in `/vibe?seed=<id>`. */
export function putSeed(draft: Omit<VibeSeedDraft, 'id' | 'createdAt'>): string {
	const id = newId();
	if (!browser) return id;
	const full: VibeSeedDraft = { ...draft, id, createdAt: Date.now() };
	try {
		sessionStorage.setItem(KEY_PREFIX + id, JSON.stringify(full));
	} catch {
		// sessionStorage unavailable / quota — the seed is simply skipped.
	}
	return id;
}

/** Read a seed AND delete it (one-shot). Returns null if absent/unparseable. */
export function takeSeed(id: string): VibeSeedDraft | null {
	if (!browser || !id) return null;
	const key = KEY_PREFIX + id;
	try {
		const raw = sessionStorage.getItem(key);
		sessionStorage.removeItem(key);
		return raw ? (JSON.parse(raw) as VibeSeedDraft) : null;
	} catch {
		return null;
	}
}

/** Read without deleting (tests / debug). */
export function peekSeed(id: string): VibeSeedDraft | null {
	if (!browser || !id) return null;
	try {
		const raw = sessionStorage.getItem(KEY_PREFIX + id);
		return raw ? (JSON.parse(raw) as VibeSeedDraft) : null;
	} catch {
		return null;
	}
}

function clampTurn(text: string): string {
	const t = text.replace(/\s+/g, ' ').trim();
	return t.length > MAX_TURN_CHARS ? `${t.slice(0, MAX_TURN_CHARS)}…` : t;
}

/**
 * Clamp a multi-line block (e.g. a meeting summary's `## Decisions` / `## Action items`
 * markdown) for the seed. Unlike `clampTurn`, this PRESERVES newlines — it only collapses
 * runs of spaces/tabs within a line and squeezes 3+ blank lines — so the summary's structure
 * survives into the build seed instead of being flattened to one line.
 */
function clampBlock(text: string): string {
	const t = text
		.split('\n')
		.map((line) => line.replace(/[ \t]+/g, ' ').trimEnd())
		.join('\n')
		.replace(/\n{3,}/g, '\n\n')
		.trim();
	return t.length > MAX_SUMMARY_CHARS ? `${t.slice(0, MAX_SUMMARY_CHARS)}…` : t;
}

function clampContent(value: string): string {
	if (value.length <= MAX_CONTENT_CHARS) return value;
	return `${value.slice(0, MAX_CONTENT_CHARS)}\n…(seed truncated)`;
}

function renderTurns(turns: SeedTurn[], limit: number): string {
	return turns
		.map((turn) => ({ role: turn.role || 'speaker', text: clampTurn(turn.text || '') }))
		.filter((turn) => turn.text.length > 0)
		.slice(-limit)
		.map((turn) => `- ${turn.role}: ${turn.text}`)
		.join('\n');
}

/** Serialize the most-recent chat turns into a seed block body. */
export function buildChatSeedContent(turns: SeedTurn[], limit = MAX_TURNS): string {
	const body = renderTurns(turns, limit);
	return clampContent(body || '(no chat content)');
}

/** Serialize a meeting transcript (+ title/date/summary header) into a seed block body. */
export function buildMeetingSeedContent(
	turns: SeedTurn[],
	meta: { title?: string | null; date?: string | null; summary?: string | null }
): string {
	const header: string[] = [];
	if (meta.title) header.push(`Meeting: ${meta.title}`);
	if (meta.date) header.push(`Date: ${meta.date}`);
	const summary = meta.summary ? clampBlock(meta.summary) : '';
	// Single-line summaries render inline ("Summary: x"); a multi-line summary (the agreed
	// Decisions / Action items prose) renders as a labeled block so its structure survives.
	if (summary) header.push(summary.includes('\n') ? `Summary:\n${summary}` : `Summary: ${summary}`);
	const body = renderTurns(turns, MAX_TURNS);
	const out = [...header, header.length ? '' : null, 'Transcript:', body]
		.filter((line): line is string => line !== null)
		.join('\n');
	return clampContent(out);
}
