/**
 * Every answerable ask the reader is being waited on, from one place.
 *
 * Attention ("Needs you") was the only surface that read both sources of a
 * pending ask: the live pending store (`HitlRequested` from the hub, plus its
 * backfill) and the durable Attention feed (`/feed/attention`, whose rows carry
 * `task_id`, `ui_thread_id` and an answerable `metadata.hitl_request`). The dock
 * above the composer read only the first, and the task panel only its own run
 * payload, so an ask raised by work that is not loaded in this thread — a
 * Relentless run, whose asks carry a task id and no thread — appeared in
 * Attention and nowhere else. This selector is the one list every surface
 * filters: by thread for the composer dock, by task for the task panel and the
 * Quality view.
 *
 * Deduplicated on every identifier an ask can be named by (its canonical id,
 * the pause/approval/correlation/request ids, the feed row's own id), live
 * entries first, so an ask that is in both sources is listed once and keeps the
 * thread the feed row knows when the live event did not carry one.
 *
 * Answering goes through `answerTaskAsk`, which drops the ask's aliases from
 * both stores on success; a `HitlResolved` from elsewhere does the same through
 * the stores' own handling. Either way the ask leaves every surface at once,
 * because every surface derives from this.
 *
 * See `docs/components/unified-ui/unified-task-panel.md`, *Pending asks*.
 */
import { derived, type Readable } from 'svelte/store';

import type { FeedItem } from '$lib/feed/types';
import type { HitlOpenTarget } from '$lib/hitl/types';
import {
	hitlOpenTargetFromFeedItem,
	hitlOpenTargetFromPendingEntry
} from '$lib/attention/openHitlPrompt';
import { attentionStore, type AttentionStoreState } from '$lib/stores/attentionStore';
import { pendingHitlEntries, type HitlPendingEntry } from '$lib/stores/pendingHitlStore';
import { taskStore } from '$lib/stores/taskStore';
import type { TaskPanelModel } from '$lib/magician/tasks/UnifiedTaskPanel.svelte';

/** One answerable ask, with the task and thread it belongs to. */
export interface PendingAsk {
	target: HitlOpenTarget;
	/** The owning task, from the ask's scope or the feed row. */
	taskId: string | null;
	/**
	 * The thread it belongs to: the ask's own `scope.thread_id`, else the feed
	 * row's `ui_thread_id`, else the owning task's thread from the task store.
	 * `null` when none of them says — never defaulted to `general`.
	 */
	threadId: string | null;
	/**
	 * The chat session that raised it, when the feed row says so outside the
	 * ask's own context (the row's `input_schema` or `hitl_request.scope`), as
	 * Android's `originChatSessionId` reads it.
	 */
	chatSessionId?: string | null;
	at: number;
}

function clean(value: string | null | undefined): string | null {
	const trimmed = typeof value === 'string' ? value.trim() : '';
	return trimmed.length > 0 ? trimmed : null;
}

/** Every identifier this ask may be named by on any stream. */
export function pendingAskAliases(target: HitlOpenTarget, extra: readonly string[] = []): string[] {
	const ids = [
		target.id,
		target.identifiers?.correlation_id,
		target.identifiers?.pause_state_id,
		target.identifiers?.approval_id,
		target.identifiers?.request_id,
		...extra
	];
	return [...new Set(ids.map((id) => clean(id)).filter((id): id is string => id !== null))];
}

function feedMetadata(item: FeedItem): Record<string, unknown> {
	return item.metadata && typeof item.metadata === 'object' && !Array.isArray(item.metadata)
		? (item.metadata as Record<string, unknown>)
		: {};
}

function record(value: unknown): Record<string, unknown> | null {
	return value && typeof value === 'object' && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function text(value: unknown): string | null {
	return typeof value === 'string' ? clean(value) : null;
}

/** The chat session a feed row names: on the row, in its input schema's context, or in its ask's scope. */
function feedOriginSession(item: FeedItem): string | null {
	const metadata = feedMetadata(item);
	const hitl = record(metadata.hitl_request);
	const schema = record(metadata.input_schema) ?? record(hitl?.input_schema);
	return text((item as unknown as Record<string, unknown>).chat_session_id)
		?? text(record(schema?.context)?.chat_session_id)
		?? text(record(hitl?.scope)?.chat_session_id);
}

/**
 * A feed row that can carry an ask: not failed, and not a C3 stalled wait
 * (waiting on the reader with no question delivered — nothing to answer).
 */
function feedRowCanAsk(item: FeedItem): boolean {
	if (item.status === 'failed') return false;
	return feedMetadata(item).attention_reason !== 'waiting_without_question';
}

/**
 * Build the deduplicated list. Pure, so it is tested without stores.
 *
 * `taskThreads` maps a task id to its thread; it is the last resort for an ask
 * that names neither its own thread nor rides a feed row that does.
 */
export function collectPendingAsks(
	entries: readonly HitlPendingEntry[],
	feed: Pick<AttentionStoreState, 'requests' | 'approvals' | 'escalations'>,
	taskThreads: ReadonlyMap<string, string> = new Map()
): PendingAsk[] {
	const asks: PendingAsk[] = [];
	const owners = new Map<string, number>();

	function add(
		target: HitlOpenTarget,
		at: number,
		threadHint: string | null,
		taskHint: string | null,
		extra: string[],
		sessionHint: string | null = null
	): void {
		const aliases = pendingAskAliases(target, extra);
		const existing = aliases.map((alias) => owners.get(alias)).find((index) => index !== undefined);
		if (existing !== undefined) {
			const ask = asks[existing];
			// The live event wins, but only the feed row may know the thread, the
			// task, or that a delegated child raised it (C2).
			asks[existing] = {
				...ask,
				taskId: ask.taskId ?? taskHint ?? clean(target.scope?.task_id),
				threadId: ask.threadId ?? clean(target.scope?.thread_id) ?? threadHint,
				chatSessionId: ask.chatSessionId ?? sessionHint,
				target:
					!ask.target.delegated_agent_id && target.delegated_agent_id
						? { ...ask.target, delegated_agent_id: target.delegated_agent_id }
						: ask.target
			};
			for (const alias of aliases) if (!owners.has(alias)) owners.set(alias, existing);
			return;
		}
		const index = asks.length;
		asks.push({
			target,
			taskId: clean(target.scope?.task_id) ?? taskHint,
			threadId: clean(target.scope?.thread_id) ?? threadHint,
			chatSessionId: sessionHint,
			at
		});
		for (const alias of aliases) owners.set(alias, index);
	}

	for (const entry of [...entries].sort((a, b) => b.at - a.at)) {
		const target = hitlOpenTargetFromPendingEntry(entry);
		if (!target) continue;
		add(target, entry.at, null, null, [entry.correlation_id, entry.pause_state_id ?? '', entry.approval_id ?? '']);
	}
	const seenItems = new Set<string>();
	for (const item of [...feed.requests, ...feed.approvals, ...feed.escalations]) {
		if (seenItems.has(item.id) || !feedRowCanAsk(item)) continue;
		seenItems.add(item.id);
		const target = hitlOpenTargetFromFeedItem(item);
		if (!target) continue;
		add(target, item.updated_at, clean(item.ui_thread_id), clean(item.task_id), [item.id], feedOriginSession(item));
	}

	return asks
		.map((ask) =>
			ask.threadId === null && ask.taskId !== null && taskThreads.has(ask.taskId)
				? { ...ask, threadId: clean(taskThreads.get(ask.taskId)) }
				: ask
		)
		.sort((a, b) => b.at - a.at);
}

/** The asks owned by one task, newest first. */
export function pendingAsksForTask(
	asks: readonly PendingAsk[],
	taskId: string | null | undefined
): HitlOpenTarget[] {
	const id = clean(taskId);
	if (id === null) return [];
	return asks.filter((ask) => ask.taskId === id).map((ask) => ask.target);
}

export interface ThreadAskFilter {
	/** Session on screen; chat questions may have no task or thread stamp. */
	chatSessionId?: string | null;
	/** Normalises a thread id for comparison (the chat uses `normalizeThreadId`). */
	normalize?: (threadId: string) => string;
	/** Tasks known to sit in this thread; an ask on one of them belongs here. */
	threadTaskIds?: ReadonlySet<string>;
	/**
	 * Tasks whose plan approval another control already offers (the planner
	 * dock); their `plan_approval` asks are left to it rather than shown twice.
	 */
	planApprovalTaskIds?: ReadonlySet<string>;
}

/**
 * The asks this conversation is waiting on: those whose resolved thread is this
 * one, or whose task sits in it.
 */
export function pendingAsksForThread(
	asks: readonly PendingAsk[],
	threadId: string,
	filter: ThreadAskFilter = {}
): HitlOpenTarget[] {
	return asks
		.filter((ask) => {
			// God-mode approvals are workspace-wide asks. Their origin is
			// shown by the composer; viewing another chat never changes the grant.
			if (isGodModeAsk(ask)) return true;
			return askBelongsHere(ask, threadId, filter) && !plannerDockOffers(ask, filter);
		})
		.map((ask) => ask.target);
}

/**
 * The composer dock's questions: every open ask, so the reader answers
 * wherever they are, with this conversation's first. `here` names the asks
 * that belong to this conversation (its chat session, thread or one of its
 * tasks, by [`pendingAsksForThread`]'s rule); the dock labels the others with
 * where they came from, and only a `here` ask takes typed composer text. A
 * plan approval the planner dock already offers here is left to it.
 */
export function composerAsks(
	asks: readonly PendingAsk[],
	threadId: string,
	filter: ThreadAskFilter = {}
): { targets: HitlOpenTarget[]; here: Set<string> } {
	const here: HitlOpenTarget[] = [];
	const elsewhere: HitlOpenTarget[] = [];
	for (const ask of asks) {
		if (isDismissOnlyAsk(ask.target)) continue;
		if (askBelongsHere(ask, threadId, filter)) {
			if (!plannerDockOffers(ask, filter)) here.push(ask.target);
		} else {
			elsewhere.push(ask.target);
		}
	}
	return { targets: [...here, ...elsewhere], here: new Set(here.map((target) => target.id)) };
}

/**
 * A notice whose only answer is Dismiss (a service-health notice, an expired
 * notice): nothing to decide, so composers (chat, VibeDev) leave it to the
 * Attention page and the desktop notch.
 */
export function isDismissOnlyAsk(target: HitlOpenTarget): boolean {
	const options = target.input_schema?.options ?? [];
	return options.length === 1 && options[0].id === 'dismiss' && options[0].requires_input !== true;
}

function askContext(ask: PendingAsk): Record<string, unknown> | null {
	const context = ask.target.input_schema?.context;
	return context && typeof context === 'object' && !Array.isArray(context)
		? context as Record<string, unknown> : null;
}

function isGodModeAsk(ask: PendingAsk): boolean {
	return ask.target.source === 'user_request'
		&& ask.target.input_schema?.request_type === 'sandbox_override'
		&& askContext(ask)?.god_mode === true;
}

/**
 * Raised by this conversation: its chat session, its thread, or one of its
 * tasks. A chat's question belongs to that chat only, so another session's
 * question is never local. Every untitled chat shares the default `general`
 * thread, so that thread alone never makes an ask local either (as on Android).
 */
function askBelongsHere(ask: PendingAsk, threadId: string, filter: ThreadAskFilter): boolean {
	const normalize = filter.normalize ?? ((value: string) => value.trim().toLowerCase());
	const context = askContext(ask);
	const originSession = clean(typeof context?.chat_session_id === 'string' ? context.chat_session_id : null)
		?? clean(ask.chatSessionId);
	const thisSession = clean(filter.chatSessionId);
	if (originSession !== null && thisSession !== null) return originSession === thisSession;
	const sharedThread = normalize(threadId) === normalize(DEFAULT_THREAD_ID);
	return (!sharedThread && ask.threadId !== null && normalize(ask.threadId) === normalize(threadId)) ||
		(ask.taskId !== null && (filter.threadTaskIds?.has(ask.taskId) ?? false));
}

/** The thread every untitled chat shares (`normalizeThreadId`'s fallback). */
const DEFAULT_THREAD_ID = 'general';

function plannerDockOffers(ask: PendingAsk, filter: ThreadAskFilter): boolean {
	return ask.target.source === 'plan_approval' &&
		ask.taskId !== null &&
		(filter.planApprovalTaskIds?.has(ask.taskId) ?? false);
}

/**
 * The panel model with every pending ask for its task folded in.
 *
 * Asks the run payload already lists (`ask`, `moreAsks`, a plan question's
 * target) are matched on every alias and skipped. When the payload named no
 * ask, the newest pending one becomes the panel's `ask` — answered in place —
 * with an `attention` stamped from it, since an ask delivered to the reader
 * is what "Waiting on you" means; the rest join `moreAsks`.
 */
export function mergeTaskPanelAsks(
	model: TaskPanelModel | null,
	extra: readonly HitlOpenTarget[]
): TaskPanelModel | null {
	if (model === null || extra.length === 0) return model;
	const known = new Set<string>();
	const remember = (target: HitlOpenTarget | null | undefined) => {
		if (target) for (const alias of pendingAskAliases(target)) known.add(alias);
	};
	remember(model.ask);
	for (const target of model.moreAsks ?? []) remember(target);
	for (const question of model.plan?.questions ?? []) remember(question.target ?? null);
	const fresh: HitlOpenTarget[] = [];
	for (const target of extra) {
		const aliases = pendingAskAliases(target);
		if (aliases.some((alias) => known.has(alias))) continue;
		for (const alias of aliases) known.add(alias);
		fresh.push(target);
	}
	if (fresh.length === 0) return model;
	if (model.ask !== null) {
		return { ...model, moreAsks: [...(model.moreAsks ?? []), ...fresh] };
	}
	const [primary, ...rest] = fresh;
	return {
		...model,
		ask: primary,
		attention: model.attention ?? {
			source: primary.source,
			summary: primary.prompt,
			raisedAt: typeof primary.at === 'number' ? primary.at : null
		},
		// A delivered ask contradicts "waiting with no question" (C3).
		stalledWait: null,
		moreAsks: [...(model.moreAsks ?? []), ...rest]
	};
}

export function createPendingAsksStore(
	entries: Readable<HitlPendingEntry[]> = pendingHitlEntries,
	feed: Readable<AttentionStoreState> = attentionStore,
	tasks: Readable<{ tasks: ReadonlyArray<{ id: string; uiThreadId?: string }> }> = taskStore
): Readable<PendingAsk[]> {
	return derived([entries, feed, tasks], ([$entries, $feed, $tasks]) => {
		const taskThreads = new Map<string, string>();
		for (const task of $tasks.tasks) {
			if (task.uiThreadId) taskThreads.set(task.id, task.uiThreadId);
		}
		return collectPendingAsks($entries, $feed, taskThreads);
	});
}

/**
 * Shared selector. The Attention feed it reads is kept live by the shell
 * (`TopBar` starts `attentionStore`, which refreshes on `HitlRequested` /
 * `HitlResolved` and drops resolved rows in the same frame); the pending store
 * is fed by the hub bridge the same component starts.
 */
const collectedPendingAsks = createPendingAsksStore();
export const pendingAsks: Readable<PendingAsk[]> = {
	subscribe(run, invalidate) {
		const stopFollowing = attentionStore.followPendingAsks();
		const unsubscribe = collectedPendingAsks.subscribe(run, invalidate);
		return () => { unsubscribe(); stopFollowing(); };
	}
};
