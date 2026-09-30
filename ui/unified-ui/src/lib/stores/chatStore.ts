/**
 * Chat Store - Manages chat sessions and messages for Chat Mode.
 *
 * Key behaviors:
 * - Ordinary user threads may retain multiple active sessions; the selected
 *   active session is tracked by its exact ID.
 * - Archived threads are read-only history (viewable but not writable).
 * - Shared between full chat page and floating bubble overlay.
 * - WebSocket events update messages in real-time for the active session.
 */

import { matchesMessageTarget, type ChatMessageOrigin, type ChatMessageTarget } from './chatMessageLinks';
import { writable, derived, get, type Readable } from 'svelte/store';
import { browser } from '$app/environment';
import { getCurrentScopeIdentity, scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { showInfo } from '$lib/shared/stores/notifications';
import { timedFetch as chatFetch, LONG_FETCH_TIMEOUT_MS } from '$lib/shared/fetch';
import type { SpeechBlock } from '$lib/media/tts/speechTags';
import type { StructuredResponseV1 } from '$lib/magician/structuredResponse/types';
import { maybeSpeakTaskCompletion } from '$lib/media/tts/taskCompletionSpeech';
import { codingChoiceFromSelection, codingProfileStore } from '$lib/stores/codingProfileStore';
import { chatHarnessPreferenceStore } from '$lib/stores/chatHarnessPreferenceStore';

// =============================================================================
// Types
// =============================================================================

export type ChatMessageDirection = 'user' | 'assistant' | 'system';

export type ChatSessionStatus = 'active' | 'archived';
export type ChatMessageMode = 'ask' | 'plan' | 'accept_in_scope';

export interface EscalationOption {
    id: string;
    label: string;
    requires_input?: boolean;
}

export interface ContentFileSource {
    type: 'session_output' | 'task_output';
    task_id?: string;
}

export interface ContentBlockRecord {
    type: 'text' | 'file' | 'url';
    text?: string;
    source?: ContentFileSource;
    relative_path?: string;
    display_name?: string;
    mime_type?: string;
    absolute_path?: string;
    label?: string;
    size?: number;
    url?: string;
}

export interface UploadedAttachment {
    attachment_id: string;
    filename: string;
    mime_type: string;
    size: number;
    label?: string;
    /** UI hint only. The backend independently verifies persisted capture
     * provenance before admitting App Copilot. */
    server_registered_capture?: boolean;
}

export interface ChatMessageContent {
    type: 'text' | 'tool_call_executed' | 'rich_tool_result' | 'attachment' | 'task_status_update' | 'escalation' | 'escalation_resolved';
    text?: string;
	/** Durable context for a user answer sent to a specific planner question. */
	plan_reply?: {
		task_id: string;
		task_title: string;
		question_id: string;
		question_text: string;
	};
    tool_name?: string;
    tool_call_id?: string;
    arguments?: Record<string, unknown>;
    summary?: string;
    content_blocks?: ContentBlockRecord[];
    filename?: string;
    mime_type?: string;
    size?: number;
    absolute_path?: string;
    label?: string;
    task_id?: string;
    status?: string;
    display_label?: string;
    ui_thread_id?: string;
    output_files?: ContentBlockRecord[];
    // True while the task has flipped to a terminal status but its final
    // result is still being synthesized — drives the card's "Preparing final
    // result…" state (the backend status guard holds the visible status at
    // "running" during this window).
    synthesis_pending?: boolean;
    // Synthesizer-authored spoken summary for off-call TTS read-out. Present on
    // the terminal-success card. Spoken aloud when the task finishes, no live
    // voice call is running, and auto-speak is unmuted.
    speech_tts?: string;
    // Escalation fields
    execution_id?: string;
    pause_state_id?: string;
    request_id?: string;
    escalation_type?: string;
    /// Pause's expected `input_type` (`external_action`, `confirmation`,
    /// `guidance`, `text`, …). The `agentic-resume` API validates the
    /// resume payload against this exact value, so the UI dispatches
    /// off `input_type` rather than `escalation_type`. Optional for
    /// back-compat with chat history persisted before this field
    /// existed and for `request_id`-backed (UserRequestService)
    /// escalations that route through `/respond` rather than
    /// `/agentic-resume`.
    input_type?: string;
    question?: string;
    options?: EscalationOption[];
    resolved?: boolean;
    stale?: boolean;
    inactive_reason?: string;
}

/** Every turn this client starts thinks with the composer's engine unless the
 *  caller names one: the command palette, the chat bubble, and the war room
 *  send without a picker of their own. */
function withComposerHarness(options: ChatSendOptions): ChatSendOptions {
    // A browser with no choice of its own sends none: the server default applies.
    if (options.harnessEngine || !chatHarnessPreferenceStore.isChosen()) return options;
    const { engine, model } = get(chatHarnessPreferenceStore);
    return { ...options, harnessEngine: engine, harnessModel: model };
}

export interface ChatSendOptions {
    /** Exact chat harness route for this turn. */
    harnessEngine?: string;
    harnessModel?: string;
    mode?: ChatMessageMode;
    planTaskId?: string | null;
    planQuestionId?: string | null;
    /** Surface that originated this turn (`web`, `mobile`, `mascot`, `voice`, ...).
     *  This is metadata on the normal chat ledger, not a separate message type. */
    sourceSurface?: string | null;
    /** Media/control session that produced the turn, when the sender is a presence surface. */
    presenceSessionId?: string | null;
    /** True when this message was generated from a voice transcript.
     *  Propagates to the backend so the chat runtime can append a
     *  `<speech>`-tag system prompt fragment instructing the model to
     *  mark the audible portion of the reply. The frontend TTS reads
     *  only the tagged content; the full reply still renders. */
    voiceOrigin?: boolean;
    /** Explicit VibeDev coding choice for a `@vibedev` turn. The rail
     *  never infers an engine from the transcript. */
    codingChoice?: { kind: 'auto' } | { kind: 'profile'; profile_id: string } | null;
}

function resolvedCodingChoice(
    options: ChatSendOptions
): { kind: 'auto' } | { kind: 'profile'; profile_id: string } | undefined {
    if (options.codingChoice === null) return undefined;
    if (options.codingChoice) return options.codingChoice;
    return codingChoiceFromSelection(get(codingProfileStore).selected) ?? undefined;
}

/**
 * A pending-replay message held in the per-session backend queue. Returned
 * by `chatStore.listQueuedMessages(sessionId)`. Mirror of the Rust
 * `QueuedMessage` struct (`magician/src/magician_v2/chat/models.rs`).
 */
export interface QueuedMessage {
    id: string;
    session_id: string;
    text?: string;
    attachment_ids?: string[];
    profile_override?: string;
    source_surface?: string;
    presence_session_id?: string;
    channel?: string;
    channel_address?: string;
    queued_at: number;
}

export interface ChatMessage {
    context_origin?: ChatMessageOrigin;
    id: string;
    session_id: string;
    direction: ChatMessageDirection;
    content: ChatMessageContent;
    created_at: number;
    /**
     * Per-request correlation id stamped on outbound user messages and
     * mirrored onto the streaming assistant placeholder so the request's
     * activity card can subscribe to the per-turn endpoint
     * (`/api/magician/v2/chat/sessions/{sid}/turns/{cid}/events[/stream]`)
     * and scope its event stream exactly to this turn (chat-side
     * emissions plus delegated/handover sub-agent emissions re-stamped
     * with the same id by the chat-fanout path).
     */
    chat_turn_id?: string;
    /**
     * Surface that originated this message's turn. Presence surfaces
     * (mascot/voice/screen) use the same chat ledger but keep this
     * source metadata for audit/debug/routing.
     */
    source_surface?: string;
    /**
     * Media/control session id associated with the source surface, if any.
     */
    presence_session_id?: string;
    /**
     * `true` when this message belongs to a chat turn whose user
     * message arrived via voice (mic → STT → composer → auto-send).
     * Stamped by the backend on both the user message and the
     * assistant reply for that turn, so any surface can decide
     * whether to auto-read the assistant text aloud without
     * reconstructing the turn relationship from message ordering.
     * Absent (undefined) on typed turns and on messages persisted
     * before the field existed.
     */
    voice_origin?: boolean;
    /**
     * Pre-parsed `<speech>` segments for assistant replies on
     * voice-originated turns. The backend parses once at persist
     * time; every surface reads typed segments from here instead of
     * running its own regex. Each segment carries the optional
     * delivery hints (emotion / style / pace / voice_mode /
     * emphasis) the LLM emitted.
     *
     * Absent when:
     *  - the message has no `<speech>` tags (typed turns)
     *  - the message predates this field
     *  - the message direction isn't `assistant`
     */
    speech_segments?: SpeechBlock[];
    presentation?: StructuredResponseV1;
}

// Re-export the canonical TTS segment + delivery-attr types so callers
// can import everything they need from `chatStore`. The source of
// truth lives in `$lib/media/tts/speechTags` so the regex parser, the
// `SpeechBlock` interface, and the chat-envelope field never drift
// out of shape.
export type {
    SpeechBlock,
    SpeechBlock as ChatSpeechSegment,
    TtsEmotion,
    TtsStyle,
    TtsPace,
    TtsVoiceMode
} from '$lib/media/tts/speechTags';

export interface ChatRenderMessage {
    id: string;
    message: ChatMessage;
    messageIds: string[];
    taskProgressUpdates: ChatMessage[];
    taskExecutionGroups: ChatRenderTaskExecutionGroup[];
    /**
     * Persistent activity cards (pack_progress, task_status_update) that
     * arrived between the preceding user turn and this assistant
     * message — i.e. tools the assistant used to produce this reply.
     *
     * Populated by `attachInTurnActivityToAssistantMessages` at render
     * time (does NOT mutate persisted chat_store state). When non-
     * empty, the chat page renders these inside the assistant
     * bubble's `<AssistantActivitySection />` instead of as standalone
     * timeline cards, so a multi-tool turn shows one collapsed
     * "What happened" dropdown rather than N interleaved pack cards.
     *
     * Activity messages that arrive AFTER the assistant message (e.g.
     * an async task the assistant kicked off) stay as standalone
     * timeline cards — they're updates on background work, not
     * in-turn tool use, and folding them into a prior reply would
     * misrepresent the chronology.
     */
    attachedActivityMessages: ChatMessage[];
    /**
     * Set when this entry is a `pack_progress` / `task_status_update`
     * card that arrived in-turn but the assistant reply hasn't landed
     * yet — its fold-target doesn't exist on the timeline. The chat
     * page suppresses standalone rendering of these via
     * `attachedActivityMessageIds`; the live `<ChatTurnProgress />`
     * bubble represents the activity (one row at a time replacing
     * the previous) while the turn is in flight. Once the assistant
     * message lands, `attachInTurnActivityToAssistantMessages`
     * re-runs and re-attaches the card under the assistant — the
     * flag clears, the message gets ID-collected via the assistant's
     * `attachedActivityMessages` instead.
     */
    inTurnActivityHidden: boolean;
}

export interface ChatRenderTaskExecutionGroup {
    id: string;
    message: ChatMessage;
    messageIds: string[];
    updates: ChatMessage[];
}

export interface ChatChannel {
    channel_type: string;  // "web", "telegram", "discord", "slack", or any custom type
    address?: string;      // channel-specific ID (chat_id, channel_id, phone number, etc.)
}

export interface ChatSession {
    internal_voice?: { kind: 'branch'; parent_session_id: string };
    id: string;
    principal: string;
    workspace: string;
    agent_id: string;
    ui_thread_id: string;
    title: string | null;
    origin_channel: ChatChannel;
    status: ChatSessionStatus;
    history_lane?: 'personal' | 'automated';
    is_default_session?: boolean;
    created_at: number;
    updated_at: number;
}

export interface ChatStoreState {
    queueRevision?: number;
    activeRunSessionId?: string | null;
    activeSessionId: string | null;
    sessions: ChatSession[];
    messages: ChatMessage[];
    isLoading: boolean;
    isLoadingOlder: boolean;
    focusedMessageId?: string | null;
    hasMoreMessages: boolean;
    isSendingMessage: boolean;  // true only while waiting for LLM response after sendMessage
    bubbleOpen: boolean;
    viewingArchivedId: string | null; // non-null when viewing a read-only archived execution
    error: string | null;
}

type EnrollmentResult =
    | { state: 'enrolled'; principal: string }
    | { state: 'pending'; code?: string }
    | { state: 'error'; message: string };

type ChatIdentityQueryResult =
    | { ok: true; query: string; principal: string }
    | { ok: false; error: string };

// =============================================================================
// Enrollment helpers
// =============================================================================

function getChannelAddress(): string {
    if (typeof window === 'undefined') return 'ssr';
    let addr = localStorage.getItem('chat_channel_address');
    if (!addr) {
        addr = crypto.randomUUID();
        localStorage.setItem('chat_channel_address', addr);
    }
    return addr;
}

function getStoredPrincipal(): string | null {
    if (typeof window === 'undefined') return null;
    return localStorage.getItem('chat_principal');
}

function setStoredPrincipal(principal: string): void {
    if (typeof window === 'undefined') return;
    localStorage.setItem('chat_principal', principal);
}

function clearStoredPrincipal(): void {
    if (typeof window === 'undefined') return;
    localStorage.removeItem('chat_principal');
}

function buildIdentityQuery(): string {
    const params = new URLSearchParams({
        channel: 'web',
        channel_address: getChannelAddress()
    });
    return params.toString();
}

function buildScopedIdentityQuery(baseQuery: string, uiThreadId?: string | null): string {
    if (!uiThreadId || uiThreadId.trim().length === 0) {
        return baseQuery;
    }
    const params = new URLSearchParams(baseQuery);
    params.set('ui_thread_id', uiThreadId.trim());
    return params.toString();
}

async function ensureEnrolled(): Promise<EnrollmentResult> {
    if (typeof window === 'undefined') {
        return { state: 'error', message: 'Chat enrollment is unavailable during server-side rendering.' };
    }

    const address = getChannelAddress();
    const existing = getStoredPrincipal();

    if (existing) {
        try {
            const statusParams = new URLSearchParams({
                channel_type: 'web',
                channel_address: address
            });
            const statusRes = await chatFetch(`/api/magician/v2/chat/enroll/status?${statusParams.toString()}`);
            if (statusRes.ok) {
                const status = await statusRes.json();
                if (status.enrolled && status.principal) {
                    setStoredPrincipal(status.principal);
                    return { state: 'enrolled', principal: status.principal };
                }
                // Pending, but the server never repeats a code here (it is
                // shown once, in the enroll response) — 'pending' alone is
                // the answer for a reloaded browser.
                if (!status.enrolled) {
                    clearStoredPrincipal();
                    return { state: 'pending' };
                }

                clearStoredPrincipal();
            } else {
                return { state: 'enrolled', principal: existing };
            }
        } catch (e) {
            console.warn('[chatStore] Enrollment status check failed, using cached principal:', e);
            return { state: 'enrolled', principal: existing };
        }
    }

    try {
        const res = await chatFetch('/api/magician/v2/chat/enroll', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
                channel_type: 'web',
                channel_address: address
            })
        });
        if (!res.ok) {
            return { state: 'error', message: `Failed to enroll chat identity (HTTP ${res.status})` };
        }

        const data = await res.json();
        if (data.enrolled && data.principal) {
            setStoredPrincipal(data.principal);
            return { state: 'enrolled', principal: data.principal };
        }

        clearStoredPrincipal();
        if (!data.enrolled) {
            console.warn('[chatStore] Enrollment pending, code:', data.code);
            return { state: 'pending', code: data.code };
        }

        return { state: 'error', message: 'Enrollment response was missing principal information.' };
    } catch (e) {
        console.error('[chatStore] Enrollment failed:', e);
        return { state: 'error', message: 'Failed to enroll chat identity.' };
    }
}

async function getChatIdentityQuery(): Promise<ChatIdentityQueryResult> {
    const enrollment = await ensureEnrolled();
    if (enrollment.state === 'enrolled') {
        return {
            ok: true,
            query: buildIdentityQuery(),
            principal: enrollment.principal
        };
    }

    if (enrollment.state === 'pending') {
        return {
            ok: false,
            error: 'Your account is pending approval. Please contact an admin.'
        };
    }

    return {
        ok: false,
        error: enrollment.message
    };
}

// =============================================================================
// localStorage helpers
// =============================================================================

const BUBBLE_OPEN_KEY = 'magician:chat:bubbleOpen';

function loadBubbleState(): boolean {
    if (!browser) return false;
    try {
        return localStorage.getItem(BUBBLE_OPEN_KEY) === 'true';
    } catch {
        return false;
    }
}

function saveBubbleState(open: boolean): void {
    if (!browser) return;
    try {
        localStorage.setItem(BUBBLE_OPEN_KEY, String(open));
    } catch {
        // ignore
    }
}

function sortMessagesChronologically(messages: ChatMessage[]): ChatMessage[] {
    // Streaming placeholder bubbles (id prefix "streaming-") are always
    // forced to the END of the rendered stream, regardless of their
    // creation timestamp. Without this, server-emitted in-turn cards
    // (task_status_update, pack_progress, etc.) whose timestamps land
    // AFTER the client-side `Date.now() + 1` we stamp on the pending
    // bubble appear BELOW the typing-dots — i.e. the loading indicator
    // shows up before the cards/messages it represents. Pin it last.
    return [...messages].sort((a, b) => {
        const aStreaming = a.id.startsWith('streaming-') ? 1 : 0;
        const bStreaming = b.id.startsWith('streaming-') ? 1 : 0;
        if (aStreaming !== bStreaming) {
            return aStreaming - bStreaming;
        }
        return a.created_at - b.created_at;
    });
}

function normalizedMessageId(value: string | null | undefined): string {
    return typeof value === 'string' ? value.trim() : '';
}

function escalationMatchesTarget(
    content: ChatMessageContent,
    executionId?: string,
    requestId?: string,
    pauseStateId?: string
): boolean {
    if (content.type !== 'escalation') return false;
    const execution = normalizedMessageId(executionId);
    const request = normalizedMessageId(requestId);
    const pause = normalizedMessageId(pauseStateId);
    if (request) {
        return normalizedMessageId(content.request_id) === request;
    }
    if (pause) {
        return normalizedMessageId(content.pause_state_id) === pause;
    }
    if (execution) {
        return normalizedMessageId(content.execution_id) === execution;
    }
    return false;
}

function isGuidanceEscalation(content: ChatMessageContent): boolean {
    if (content.type !== 'escalation') return false;
    return content.escalation_type === 'cannot_proceed' || content.escalation_type === 'loop_detected';
}

function reconcileInteractiveMessages(messages: ChatMessage[]): ChatMessage[] {
    const resolvedEscalationIndices = new Set<number>();
    const resolvedEscalationReasons = new Map<number, string>();
    const staleEscalationIndices = new Set<number>();
    const latestOpenEscalationIndexByExecution = new Map<string, number>();
    const latestOpenEscalationIndexByRequest = new Map<string, number>();
    const latestOpenEscalationIndexByPause = new Map<string, number>();

    for (const [index, message] of messages.entries()) {
        if (message.content.type === 'escalation') {
            const executionId = message.content.execution_id?.trim();
            const requestId = message.content.request_id?.trim();
            const pauseStateId = message.content.pause_state_id?.trim();
            // UserRequestService-backed escalations (memory clarifications,
            // learning consolidation questions like "should I remember…")
            // each carry their own `request_id` (correlation_id) and are
            // INDEPENDENTLY answerable until explicitly resolved or
            // timed out. The backend stamps `execution_id` to the
            // originating task's exec_id (or the correlation_id as a
            // fallback), so multiple memory questions emitted during the
            // same task would otherwise share an `execution_id` and the
            // execution-supersession path below would mark all-but-the-
            // last as stale — wiping useful "do you want me to remember
            // X" prompts before the user could act on them. Skip the
            // execution-supersession path entirely when a `request_id`
            // is present; supersession of `request_id` flows is the
            // `EscalationResolved` event's job.
            if (executionId && !requestId) {
                const previousOpenIndex = latestOpenEscalationIndexByExecution.get(executionId);
                if (previousOpenIndex !== undefined) {
                    staleEscalationIndices.add(previousOpenIndex);
                }
                latestOpenEscalationIndexByExecution.set(executionId, index);
            }
            if (requestId) {
                latestOpenEscalationIndexByRequest.set(requestId, index);
            }
            if (pauseStateId) {
                latestOpenEscalationIndexByPause.set(pauseStateId, index);
            }
            continue;
        }

        if (message.content.type === 'escalation_resolved') {
            const executionId = message.content.execution_id?.trim();
            const requestId = message.content.request_id?.trim();
            const pauseStateId = message.content.pause_state_id?.trim();

            const openEscalationIndex = (requestId ? latestOpenEscalationIndexByRequest.get(requestId) : undefined)
                ?? (pauseStateId ? latestOpenEscalationIndexByPause.get(pauseStateId) : undefined)
                ?? (executionId ? latestOpenEscalationIndexByExecution.get(executionId) : undefined);
            if (openEscalationIndex === undefined) {
                continue;
            }

            resolvedEscalationIndices.add(openEscalationIndex);
            const summary = message.content.summary?.trim();
            if (summary) {
                resolvedEscalationReasons.set(openEscalationIndex, summary);
            }
            staleEscalationIndices.delete(openEscalationIndex);
            if (executionId) latestOpenEscalationIndexByExecution.delete(executionId);
            if (requestId) latestOpenEscalationIndexByRequest.delete(requestId);
            if (pauseStateId) latestOpenEscalationIndexByPause.delete(pauseStateId);
        }
    }

    return messages
        .map((message, index) => {
            if (message.content.type !== 'escalation') {
                return message;
            }

            const shouldResolve = Boolean(
                message.content.resolved || resolvedEscalationIndices.has(index)
            );
            const shouldMarkStale = Boolean(
                !shouldResolve
                && (staleEscalationIndices.has(index)
                    || (isGuidanceEscalation(message.content) && index < messages.length - 1))
            );
            const inactiveReason = resolvedEscalationReasons.get(index)
                ?? (shouldMarkStale ? message.content.inactive_reason ?? 'No longer active' : message.content.inactive_reason);

            if (
                message.content.resolved === shouldResolve
                && message.content.stale === shouldMarkStale
                && message.content.inactive_reason === inactiveReason
            ) {
                return message;
            }

            return {
                ...message,
                content: {
                    ...message.content,
                    resolved: shouldResolve,
                    stale: shouldMarkStale,
                    inactive_reason: inactiveReason
                }
            };
        });
}

export function normalizeMessages(messages: ChatMessage[]): ChatMessage[] {
    return reconcileInteractiveMessages(sortMessagesChronologically(messages));
}

function taskProgressGroupKey(message: ChatMessage): string | null {
    if (message.content.type !== 'task_status_update') {
        return null;
    }

    const taskId = message.content.task_id?.trim();
    if (!taskId) {
        return null;
    }

    const executionId = message.content.execution_id?.trim();
    return executionId
        ? `task:${taskId}::execution:${executionId}`
        : `task:${taskId}::execution:none`;
}

function isTerminalTaskStatusUpdate(status: string | undefined): boolean {
    return status === 'completed' || status === 'failed' || status === 'cancelled';
}

function isClosedTaskContainer(message: ChatRenderMessage): boolean {
    return (
        message.taskExecutionGroups.length > 0 &&
        message.taskExecutionGroups.every((group) =>
            isTerminalTaskStatusUpdate(group.message.content.status)
        )
    );
}

function rebuildTaskContainerIndicesByTaskId(
    collapsed: ChatRenderMessage[]
): Map<string, number[]> {
    const indices = new Map<string, number[]>();
    collapsed.forEach((entry, index) => {
        const taskId = entry.message.content.type === 'task_status_update'
            ? entry.message.content.task_id?.trim() ?? ''
            : '';
        if (!taskId) {
            return;
        }
        const existing = indices.get(taskId) ?? [];
        existing.push(index);
        indices.set(taskId, existing);
    });
    return indices;
}

export function collapseTaskProgressMessages(messages: ChatMessage[], exactMessageId?: string | null): ChatRenderMessage[] {
    // An explicit source link must show that saved answer even when a later
    // task update would normally replace it in the coalesced timeline.
    const exactIndex = exactMessageId ? messages.findIndex(m => m.id === exactMessageId) : -1;
    if (exactIndex >= 0) return [
        ...collapseTaskProgressMessages(messages.slice(0, exactIndex)),
        ...collapseTaskProgressMessages([messages[exactIndex]]),
        ...collapseTaskProgressMessages(messages.slice(exactIndex + 1)),
    ];
    const collapsed: ChatRenderMessage[] = [];
    let taskContainerIndicesByTaskId = new Map<string, number[]>();

    for (const message of messages) {
        const taskId = message.content.type === 'task_status_update'
            ? message.content.task_id?.trim() ?? ''
            : '';
        const taskGroupKey = taskProgressGroupKey(message);
        if (taskGroupKey && taskId) {
            const candidateIndices = taskContainerIndicesByTaskId.get(taskId) ?? [];
            let existingIndex = [...candidateIndices]
                .reverse()
                .find((index) => collapsed[index]?.taskExecutionGroups.some((group) => group.id === taskGroupKey));

            // Promote a "no-execution placeholder" container to absorb
            // execution-bearing updates for the same task_id. The
            // chat-side `create_task` emit lands as a status_update
            // with `execution_id: null` (no execution exists yet at
            // create-time) — its group key is `task:<id>::execution:none`.
            // The subsequent in-flight / terminal events fire with a
            // real execution_id and would otherwise create a SECOND
            // container, leaving the user with two stacked cards
            // ("Created: …" then "Completed: …" never merging). Fold
            // the placeholder's execution group into the live one so
            // there's a single card per task that morphs through the
            // lifecycle. Rebuild the no-exec group's id to the live
            // execution key so subsequent updates also coalesce.
            if (existingIndex === undefined) {
                const placeholderIndex = [...candidateIndices].reverse().find((index) => {
                    const container = collapsed[index];
                    if (!container || isClosedTaskContainer(container)) return false;
                    return container.taskExecutionGroups.every((group) =>
                        group.id.endsWith('::execution:none')
                    );
                });
                if (placeholderIndex !== undefined) {
                    // Rewrite the placeholder group's id to the live
                    // execution key in place. The next `findIndex` for
                    // `executionGroupIndex` then matches and the
                    // existing "replace message" path runs, REPLACING
                    // the "Created" message with the live one rather
                    // than appending a second group inside the
                    // container. End result: a single card per task,
                    // re-themed by `taskStatusVisual` as the lifecycle
                    // advances (created → running → completed).
                    const container = collapsed[placeholderIndex];
                    const promoted = container.taskExecutionGroups.map((group) =>
                        group.id.endsWith('::execution:none')
                            ? { ...group, id: taskGroupKey }
                            : group,
                    );
                    collapsed[placeholderIndex] = {
                        ...container,
                        taskExecutionGroups: promoted,
                    };
                    existingIndex = placeholderIndex;
                }
            }

            if (existingIndex === undefined) {
                const latestIndex = candidateIndices[candidateIndices.length - 1];
                if (latestIndex !== undefined && !isClosedTaskContainer(collapsed[latestIndex])) {
                    existingIndex = latestIndex;
                }
            }

            if (existingIndex !== undefined) {
                const previous = collapsed[existingIndex];
                const taskExecutionGroups = [...previous.taskExecutionGroups];
                const executionGroupIndex = taskExecutionGroups.findIndex(
                    (group) => group.id === taskGroupKey
                );
                if (executionGroupIndex !== -1) {
                    const previousGroup = taskExecutionGroups[executionGroupIndex];
                    taskExecutionGroups[executionGroupIndex] = {
                        ...previousGroup,
                        message,
                        messageIds: [...previousGroup.messageIds, message.id],
                        updates: [...previousGroup.updates, message]
                    };
                } else {
                    taskExecutionGroups.push({
                        id: taskGroupKey,
                        message,
                        messageIds: [message.id],
                        updates: [message]
                    });
                }

                const updated: ChatRenderMessage = {
                    ...previous,
                    message,
                    messageIds: [...previous.messageIds, message.id],
                    taskProgressUpdates: [...previous.taskProgressUpdates, message],
                    taskExecutionGroups
                };
                collapsed.splice(existingIndex, 1);
                collapsed.push(updated);
                taskContainerIndicesByTaskId = rebuildTaskContainerIndicesByTaskId(collapsed);
                continue;
            }

            collapsed.push({
                id: `task-group:${taskId}:${message.id}`,
                message,
                messageIds: [message.id],
                taskProgressUpdates: [message],
                taskExecutionGroups: [{
                    id: taskGroupKey,
                    message,
                    messageIds: [message.id],
                    updates: [message]
                }],
                attachedActivityMessages: [],
                inTurnActivityHidden: false
            });
            taskContainerIndicesByTaskId = rebuildTaskContainerIndicesByTaskId(collapsed);
            continue;
        }

        collapsed.push({
            id: message.id,
            message,
            messageIds: [message.id],
            taskProgressUpdates: [],
            taskExecutionGroups: [],
            attachedActivityMessages: [],
            inTurnActivityHidden: false
        });
    }

    return collapsed;
}

/**
 * Fold persistent activity cards (pack_progress, task_status_update)
 * that arrived in-turn (between a user message and the next assistant
 * message) into that assistant message's `attachedActivityMessages`.
 *
 * Activity cards that arrive AFTER an assistant message (e.g. an
 * async task the assistant kicked off in background) stay as
 * standalone timeline rows — they're updates on background work,
 * not in-turn tool use, and folding them into a prior reply would
 * misrepresent the chronology.
 *
 * Render-pass only — never mutates the persisted chat_store. The
 * chat page renders attached activity inside the assistant bubble's
 * `<AssistantActivitySection />` collapsed dropdown and SKIPS the
 * standalone card for the same message.
 */
export function attachInTurnActivityToAssistantMessages(
    rendered: ChatRenderMessage[]
): ChatRenderMessage[] {
    if (rendered.length === 0) return rendered;

    // Walk chronologically. After each user message, stash any
    // pack_progress / task_status_update messages we encounter until
    // we hit an assistant text/attachment message — fold the stash
    // into that assistant message and clear the in-flight hidden
    // flag. Reset the stash on the next user message; stash entries
    // that never get a fold target (no following assistant text
    // before the next user message or end-of-list) keep their
    // hidden flag set so the live timeline doesn't show them as
    // standalone cards while the turn is in flight — the
    // `<ChatTurnProgress />` bubble represents the activity instead.
    interface Stashed {
        msg: ChatMessage;
        renderedIndex: number;
    }
    let stash: Stashed[] = [];
    // Whether an assistant text reply has already landed for the
    // current turn (reset on each user message). Once the reply is
    // out, any further activity cards are background-work updates,
    // not in-turn tool use — they render standalone, not folded.
    let assistantReplySeen = false;

    const next: ChatRenderMessage[] = rendered.map((entry) => ({
        ...entry,
        attachedActivityMessages: [],
        inTurnActivityHidden: false,
    }));

    // Helper: clear the in-flight hidden flag on stashed entries that
    // never found a fold target. Called on (a) new user message
    // boundary that drains a stash before an assistant text landed
    // (cancelled / orphaned turn), and would also be called on
    // end-of-list if we wanted, but end-of-list with a non-empty
    // stash is the in-flight turn case — we KEEP those hidden so the
    // live `<ChatTurnProgress />` bubble is the single visible
    // activity surface.
    const unhideStash = (entries: Stashed[]) => {
        for (const { renderedIndex } of entries) {
            next[renderedIndex] = {
                ...next[renderedIndex],
                inTurnActivityHidden: false,
            };
        }
    };

    for (let i = 0; i < next.length; i++) {
        const entry = next[i];
        const msg = entry.message;
        const isActivityCard = msg.content.type === 'task_status_update';

        if (msg.direction === 'user') {
            // New turn starts. Any stash NOT yet folded is orphaned
            // (cancelled turn / no assistant reply landed) — unhide
            // those entries so they fall back to standalone rendering
            // instead of staying invisible forever.
            unhideStash(stash);
            stash = [];
            assistantReplySeen = false;
            continue;
        }

        if (isActivityCard) {
            // A card is in-turn tool use ONLY while the turn is still
            // in flight: the assistant reply hasn't landed yet AND the
            // task isn't already finished. Those get stashed and
            // folded into the assistant bubble's activity dropdown.
            //
            // A card that is TERMINAL (completed/failed/cancelled), or
            // that arrives AFTER the assistant reply already landed, is
            // a background-work update — NOT in-turn activity. Per this
            // function's contract those stay as standalone timeline
            // rows so the card's result + its "Inspect run →" deep-panel
            // affordance actually surface (folding/hiding them is what
            // made a completed delegate run invisible).
            const isTerminal =
                msg.content.type === 'task_status_update' &&
                isTerminalTaskStatusUpdate(msg.content.status);
            if (assistantReplySeen || isTerminal) {
                // Leave standalone (inTurnActivityHidden stays false).
                continue;
            }
            // In-flight, non-terminal: stash to fold into the next
            // assistant text. Initialising the algorithm with the
            // implicit assumption "we may be inside a turn" handles
            // the pagination case where the window opens mid-turn
            // (the preceding user message lives in an earlier page) —
            // the earliest assistant text in the window still pulls
            // those activity cards into its dropdown.
            next[i] = { ...entry, inTurnActivityHidden: true };
            stash.push({ msg, renderedIndex: i });
            continue;
        }

        if (msg.direction === 'assistant' && msg.content.type === 'text') {
            // Fold the in-turn activity into this assistant message
            // and clear the in-flight hidden flag on each stash
            // entry (they're now attached, the assistant's section
            // owns rendering).
            next[i] = {
                ...entry,
                attachedActivityMessages: stash.map((s) => s.msg),
            };
            for (const { renderedIndex } of stash) {
                next[renderedIndex] = {
                    ...next[renderedIndex],
                    inTurnActivityHidden: false,
                };
            }
            stash = [];
            assistantReplySeen = true;
            continue;
        }
    }

    return next;
}

/**
 * Returns the set of message ids that the chat page should NOT
 * render as standalone timeline cards. Two reasons a message ends
 * up in this set:
 *
 *   1. **Attached** — folded into a completed assistant message's
 *      `attachedActivityMessages`. Rendered inside that assistant
 *      bubble's `<AssistantActivitySection />` dropdown.
 *
 *   2. **In-flight pending fold** — `pack_progress` /
 *      `task_status_update` arriving after a user message but
 *      before the assistant reply has landed. These will fold once
 *      the assistant message arrives; in the meantime the live
 *      `<ChatTurnProgress />` "Working…" bubble represents the
 *      activity (one row at a time replacing the previous). Hiding
 *      them keeps the timeline clean — no stacking pack-status
 *      cards alongside the bubble.
 *
 * The fold pass populates both kinds; this helper unions them so
 * the chat page has one filter to apply.
 */
export function attachedActivityMessageIds(
    rendered: ChatRenderMessage[]
): Set<string> {
    const ids = new Set<string>();
    for (const entry of rendered) {
        for (const attached of entry.attachedActivityMessages) {
            ids.add(attached.id);
        }
        if (entry.inTurnActivityHidden) {
            ids.add(entry.message.id);
        }
    }
    return ids;
}

/**
 * Should this chat message be hidden from the rendered timeline?
 *
 * Today this catches a single retirement: legacy `Agent event: <type>`
 * text rows persisted in chat_store before the magician v0.6.502
 * taxonomy-driven suppression shipped. The backend now drops these
 * at the channel-pipeline level (`ChatChannel::deliver` reads
 * `chat_render_kind` and skips internal agentic-stream events), but
 * rows written before the refactor still live in old sessions on
 * disk. The frontend filter at render time keeps them from
 * resurfacing as redundant pills next to the consolidated
 * `<ChatTurnProgress />` / `<AssistantActivitySection />` view.
 *
 * Exported so both `/chat` and `/t/[name]` page render pipelines
 * can apply the same filter — used by `normalizeMessages` callers
 * via `.filter(m => !isHiddenTranscriptMessage(m))`.
 */
export function isHiddenTranscriptMessage(message: ChatMessage): boolean {
    if (message.direction !== 'system') {
        return false;
    }
    // Legacy agentic-stream system-text rows that pre-dated the
    // chat_render_kind filter on the backend.
    if (message.content.type === 'text') {
        const text = (message.content.text ?? '').trim();
        if (text.startsWith('Agent event: ')) {
            return true;
        }
        return false;
    }
    // ─── Resolved / stale escalations + their resolution pips ─────────
    // Once an escalation has been answered (resolved=true), superseded
    // (stale=true), or the system emitted its terminal `EscalationResolved`
    // pip, the chat-row has served its purpose. Hide them so the
    // timeline "fades away" the moment the user answers, rather than
    // accumulating a graveyard of dimmed Q&A cards.
    //
    // Active escalations (no resolved/stale) still render in chat
    // BECAUSE the answer UI today is bound to the chat card's buttons
    // (handleEscalationResponse → respondToHitl → AttentionPromptModal).
    // The attention bar shows the COUNT and links to the relevant
    // surface, but doesn't yet trigger the modal directly. Until that
    // wiring lands, removing the active card would leave the user
    // with no way to answer.
    if (message.content.type === 'escalation') {
        if (message.content.resolved === true) return true;
        if (message.content.stale === true) return true;
        return false;
    }
    if (message.content.type === 'escalation_resolved') {
        return true;
    }
    return false;
}

export function renderMessageContainsId(message: ChatRenderMessage, messageId: string | null | undefined): boolean {
    if (!messageId) {
        return false;
    }
    return message.messageIds.includes(messageId);
}

async function fetchSessionDetail(
    sessionId: string, target?: ChatMessageTarget, isCurrent: () => boolean = () => true
): Promise<{ session: ChatSession; messages: ChatMessage[]; focusedMessageId?: string; hasMore: boolean } | null> {
    const identity = await getChatIdentityQuery();
    if (!identity.ok) {
        return null;
    }

    const response = await chatFetch(
        `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}?${identity.query}`
    );
    if (!response.ok) {
        return null;
    }

    const data = await response.json();
    let messages: ChatMessage[] = Array.isArray(data.messages) ? data.messages.map(convertMessage) : [];
    let hasMore = messages.length >= 200;
    let match = target ? messages.find(m => matchesMessageTarget(m, target)) : undefined;
    const cursors = new Set<string>();
    while (target && !match && hasMore) {
        if (!isCurrent()) return null;
        const before = messages[0]?.id;
        if (!before || cursors.has(before)) throw new Error('Could not locate the original answer. Please try again.');
        cursors.add(before);
        const query = new URLSearchParams(identity.query);
        query.set('limit', '200');
        query.set('before', before);
        const page = await chatFetch(`/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/messages?${query}`);
        if (!page.ok) throw new Error('Could not load the original answer. Please try again.');
        const older = await page.json();
        const rows: ChatMessage[] = Array.isArray(older.messages) ? older.messages.map(convertMessage) : [];
        match = rows.find(m => matchesMessageTarget(m, target));
        messages = [...rows, ...messages];
        hasMore = older.has_more === true && rows.length > 0;
    }
    return {
        session: convertSession(data.session ?? data),
        messages: normalizeMessages(messages), focusedMessageId: match?.id, hasMore
    };
}

async function fetchSessionsForThread(uiThreadId?: string | null): Promise<ChatSession[] | null> {
    const identity = await getChatIdentityQuery();
    if (!identity.ok) {
        return null;
    }

    const response = await chatFetch(
        `/api/magician/v2/chat/sessions?${buildScopedIdentityQuery(identity.query, uiThreadId)}`
    );
    if (!response.ok) {
        return null;
    }

    const data = await response.json();
    return Array.isArray(data.sessions ?? data)
        ? (data.sessions ?? data).map((s: Record<string, unknown>) => convertSession(s))
        : [];
}

function appendUniqueMessagesInOrder(messages: ChatMessage[], additions: ChatMessage[]): ChatMessage[] {
    let next = messages;
    for (const message of additions) {
        const index = next.findIndex(existing => existing.id === message.id);
        if (index < 0) {
            next = [...next, message];
        } else if (isNewerResultProjection(next[index], message)) {
            next = [...next];
            next[index] = { ...message, created_at: next[index].created_at };
        }
    }
    return next;
}

function isNewerResultProjection(existing: ChatMessage, incoming: ChatMessage): boolean {
    const origin = incoming.context_origin;
    const prior = existing.context_origin;
    return incoming.direction === 'assistant' && existing.direction === 'assistant' &&
        !!origin && !!prior && origin.session_id !== incoming.session_id &&
        incoming.session_id === existing.session_id &&
        origin.session_id === prior.session_id && origin.request_id === prior.request_id &&
        Number.isFinite(origin.result_created_at) &&
        origin.result_created_at! > (prior.result_created_at ?? -Infinity);
}

function appendServerTurnMessages(
    messages: ChatMessage[],
    data: Record<string, unknown>,
    options: { includeUserMessage?: boolean } = {}
): ChatMessage[] {
    const { includeUserMessage = true } = options;
    let next = messages;

    if (Array.isArray(data.messages)) {
        next = appendUniqueMessagesInOrder(
            next,
            data.messages.map((raw: unknown) => convertMessage(raw as Record<string, unknown>))
        );
        return normalizeMessages(next);
    }

    const fallbackMessages: ChatMessage[] = [];
    if (includeUserMessage && data.user_message) {
        fallbackMessages.push(convertMessage(data.user_message as Record<string, unknown>));
    }
    if (data.assistant_message) {
        fallbackMessages.push(convertMessage(data.assistant_message as Record<string, unknown>));
    }
    return normalizeMessages(appendUniqueMessagesInOrder(next, fallbackMessages));
}

function currentVisibleSessionId(state: ChatStoreState): string | null {
    return state.viewingArchivedId ?? state.activeSessionId;
}

const authoritativeBatchSessions = new Set<string>();
const bufferedRealtimeMessagesBySession = new Map<string, ChatMessage[]>();
const sendingSessions = new Set<string>();

/**
 * Side-map from chat message id → chat_turn_id. Populated when the
 * optimistic user message is created (tempId → turnId) and re-keyed
 * onto the authoritative server message id once the Done event arrives.
 *
 * Backend now persists `chat_turn_id` on `ChatMessage` and the chat
 * history API returns it, so reloads pick the id straight off the
 * message via `getChatTurnIdForMessage`. The side-map is still needed
 * for the in-session optimistic-swap window: the streaming Done
 * payload's authoritative message may not yet round-trip through a
 * field-preserving codepath, and the optimistic record holds the
 * client-generated turn id we need to keep the in-flight card
 * subscribed.
 *
 * Lifetime: keyed by message id, scoped to the running browser
 * session. Reload safely drops the map — historical messages carry
 * their own `chat_turn_id` field at that point.
 */
const chatTurnIdByMessageIdInternal = writable<Map<string, string>>(new Map());

export const chatTurnIdByMessageIdStore: Readable<Map<string, string>> = derived(
    chatTurnIdByMessageIdInternal,
    ($map) => $map
);

function recordChatTurnIdForMessage(messageId: string, chatTurnId: string): void {
    chatTurnIdByMessageIdInternal.update((current) => {
        if (current.get(messageId) === chatTurnId) return current;
        const next = new Map(current);
        next.set(messageId, chatTurnId);
        return next;
    });
}

export function getChatTurnIdForMessage(
    message: { id: string; chat_turn_id?: string | null },
    map: Map<string, string>
): string | null {
    if (message.chat_turn_id) return message.chat_turn_id;
    return map.get(message.id) ?? null;
}

function beginAuthoritativeBatch(sessionId: string): void {
    authoritativeBatchSessions.add(sessionId);
    bufferedRealtimeMessagesBySession.set(sessionId, []);
}

function isAuthoritativeBatchActive(sessionId: string): boolean {
    return authoritativeBatchSessions.has(sessionId);
}

function bufferRealtimeMessage(sessionId: string, message: ChatMessage): void {
    const buffered = bufferedRealtimeMessagesBySession.get(sessionId) ?? [];
    bufferedRealtimeMessagesBySession.set(sessionId, appendUniqueMessagesInOrder(buffered, [message]));
}

function appendBufferedRealtimeMessages(messages: ChatMessage[], sessionId: string): ChatMessage[] {
    const buffered = bufferedRealtimeMessagesBySession.get(sessionId) ?? [];
    if (buffered.length === 0) {
        return normalizeMessages(messages);
    }
    return normalizeMessages(appendUniqueMessagesInOrder(messages, buffered));
}

function finishAuthoritativeBatch(
    sessionId: string,
    messages: ChatMessage[],
    data: Record<string, unknown>,
    options: { includeUserMessage?: boolean } = {}
): ChatMessage[] {
    const next = appendServerTurnMessages(messages, data, options);
    authoritativeBatchSessions.delete(sessionId);
    const withBuffered = appendBufferedRealtimeMessages(next, sessionId);
    bufferedRealtimeMessagesBySession.delete(sessionId);
    return withBuffered;
}

function cancelAuthoritativeBatch(sessionId: string, messages: ChatMessage[]): ChatMessage[] {
    authoritativeBatchSessions.delete(sessionId);
    const withBuffered = appendBufferedRealtimeMessages(messages, sessionId);
    bufferedRealtimeMessagesBySession.delete(sessionId);
    return withBuffered;
}

function dropAuthoritativeBatch(sessionId: string): void {
    authoritativeBatchSessions.delete(sessionId);
    bufferedRealtimeMessagesBySession.delete(sessionId);
}

function beginSendingSession(sessionId: string): void {
    sendingSessions.add(sessionId);
}

function endSendingSession(sessionId: string): void {
    sendingSessions.delete(sessionId);
}

function applySendingState(state: ChatStoreState): ChatStoreState {
    const visibleSessionId = currentVisibleSessionId(state);
    return {
        ...state,
        isSendingMessage: visibleSessionId !== null && sendingSessions.has(visibleSessionId)
    };
}

function isOptimisticUserMessage(message: ChatMessage): boolean {
    return message.id.startsWith('temp-user-')
        && message.direction === 'user'
        && message.content.type === 'text';
}

function stripMatchingOptimisticUserMessage(
    messages: ChatMessage[],
    incoming: ChatMessage
): ChatMessage[] {
    if (incoming.direction !== 'user' || incoming.content.type !== 'text') {
        return messages;
    }

    const incomingText = incoming.content.text?.trim();
    if (!incomingText) {
        return messages;
    }

    for (let index = messages.length - 1; index >= 0; index -= 1) {
        const candidate = messages[index];
        if (
            isOptimisticUserMessage(candidate)
            && candidate.session_id === incoming.session_id
            && candidate.content.text?.trim() === incomingText
        ) {
            return [...messages.slice(0, index), ...messages.slice(index + 1)];
        }
    }

    return messages;
}

// =============================================================================
// Default State
// =============================================================================

const defaultState: ChatStoreState = {
    activeSessionId: null,
    sessions: [],
    messages: [],
    isLoading: false,
    isLoadingOlder: false,
    hasMoreMessages: false,
    isSendingMessage: false,
    bubbleOpen: loadBubbleState(),
    viewingArchivedId: null,
    error: null
};

type ChatScopeToken = {
    generation: number;
    scopeKey: string;
};

let chatScopeGeneration = 0;

function currentChatScopeKey(): string {
    const scope = getCurrentScopeIdentity();
    return `${scope.principal}:${scope.workspace}`;
}

function nextChatScopeToken(): ChatScopeToken {
    return {
        generation: chatScopeGeneration,
        scopeKey: currentChatScopeKey()
    };
}

function isStaleChatScopeToken(token: ChatScopeToken): boolean {
    return token.generation !== chatScopeGeneration || currentChatScopeKey() !== token.scopeKey;
}

function clearChatRuntimeState(): void {
    authoritativeBatchSessions.clear();
    bufferedRealtimeMessagesBySession.clear();
    sendingSessions.clear();
}

// =============================================================================
// Helper: extract text from ChatMessageContent
// =============================================================================

/** Extract a display-friendly name from a ChatChannel (e.g. "web", "telegram"). */
export function getChannelDisplayName(channel: ChatChannel): string {
    return channel.channel_type ?? 'web';
}

const MAX_ESCALATION_RESOLVED_SUMMARY_CHARS = 280;
const MAX_SYSTEM_MESSAGE_SUMMARY_CHARS = 280;

function normalizeEscalationResolvedWhitespace(text: string): string {
    return text.replace(/\s+/g, ' ').trim();
}

function truncateEscalationResolvedText(text: string, maxChars = MAX_ESCALATION_RESOLVED_SUMMARY_CHARS): string {
    if (text.length <= maxChars) {
        return text;
    }

    const candidate = text.slice(0, maxChars);
    const lastSpace = candidate.lastIndexOf(' ');
    if (lastSpace > maxChars / 2) {
        return `${candidate.slice(0, lastSpace).trimEnd()}...`;
    }
    return `${candidate.trimEnd()}...`;
}

function takeEscalationResolvedSentences(text: string, count: number): string {
    const sentences: string[] = [];
    let remaining = text.trim();

    while (remaining.length > 0 && sentences.length < count) {
        const separatorIndex = remaining.indexOf('. ');
        if (separatorIndex === -1) {
            sentences.push(remaining);
            break;
        }

        sentences.push(remaining.slice(0, separatorIndex + 1).trim());
        remaining = remaining.slice(separatorIndex + 2).trim();
    }

    return sentences.join(' ');
}

function compactEscalationResolvedDetails(details: string): string {
    const normalized = normalizeEscalationResolvedWhitespace(details);
    if (!normalized) {
        return '';
    }

    let candidate = normalized;

    const partialProgressIndex = candidate.indexOf('Partial progress:');
    if (partialProgressIndex !== -1) {
        candidate = candidate.slice(0, partialProgressIndex).trim();
    }

    const testResultsIndex = candidate.indexOf('Test results:');
    if (testResultsIndex !== -1) {
        candidate = candidate.slice(0, testResultsIndex).trim();
    }

    if (candidate.startsWith('Goal achieved:')) {
        const afterGoal = candidate.slice('Goal achieved:'.length).trim();
        const firstSentenceEnd = afterGoal.indexOf('. ');
        if (firstSentenceEnd !== -1) {
            const remainder = afterGoal.slice(firstSentenceEnd + 2).trim();
            if (remainder) {
                candidate = remainder;
            }
        } else if (afterGoal) {
            candidate = afterGoal;
        }
    }

    return truncateEscalationResolvedText(takeEscalationResolvedSentences(candidate, 2));
}

export function getEscalationResolvedSummary(
    summaryOrContent: ChatMessageContent | string | undefined | null
): string {
    const raw = typeof summaryOrContent === 'string'
        ? summaryOrContent
        : summaryOrContent?.type === 'escalation_resolved'
            ? summaryOrContent.summary
            : undefined;
    const normalized = normalizeEscalationResolvedWhitespace(raw ?? '');
    if (!normalized) {
        return 'Escalation resolved';
    }

    if (normalized.startsWith('Execution completed:')) {
        const remainder = normalized.slice('Execution completed:'.length).trim();
        const separatorIndex = remainder.indexOf(' — ');
        if (separatorIndex !== -1) {
            const outcome = remainder.slice(0, separatorIndex).trim();
            const details = compactEscalationResolvedDetails(
                remainder.slice(separatorIndex + 3).trim()
            );
            return details
                ? `Execution completed: ${outcome} — ${details}`
                : `Execution completed: ${outcome}`;
        }
    }

    return compactEscalationResolvedDetails(normalized);
}

function compactDelegatedAgentCompletionText(text: string): string | null {
    const normalized = normalizeEscalationResolvedWhitespace(text);
    if (!normalized.startsWith("Delegated agent '")) {
        return null;
    }

    const match = normalized.match(/^Delegated agent '([^']+)' (completed|failed|cancelled)\.\s*(.*)$/i);
    if (!match) {
        return null;
    }

    const [, agentId, rawStatus, rawDetails] = match;
    const status = rawStatus.toLowerCase();
    const genericTerminalSummary = `Execution ${status}.`;
    let details = rawDetails.trim();

    if (!details || details === genericTerminalSummary) {
        return `Delegated agent '${agentId}' ${status}.`;
    }

    if (details.startsWith(`${genericTerminalSummary} `)) {
        details = details.slice(genericTerminalSummary.length).trim();
    }

    const executionPrefix = `Execution ${status}:`;
    if (details.startsWith(executionPrefix)) {
        details = details.slice(executionPrefix.length).trim();
    }

    const compactedDetails = compactEscalationResolvedDetails(details)
        || truncateEscalationResolvedText(details, MAX_SYSTEM_MESSAGE_SUMMARY_CHARS);

    return compactedDetails
        ? `Delegated agent '${agentId}' ${status}. ${compactedDetails}`
        : `Delegated agent '${agentId}' ${status}.`;
}

function compactHandoverSystemText(text: string): string | null {
    const normalized = normalizeEscalationResolvedWhitespace(text);

    let match = normalized.match(
        /^Handed over execution ownership from '([^']+)' to '([^']+)'\. Context:\s*(.+)$/i
    );
    if (match) {
        const [, fromAgent, toAgent, rawContext] = match;
        const compactedContext = compactEscalationResolvedDetails(rawContext)
            || truncateEscalationResolvedText(rawContext, MAX_SYSTEM_MESSAGE_SUMMARY_CHARS);
        return compactedContext
            ? `Handed over execution ownership from '${fromAgent}' to '${toAgent}'. ${compactedContext}`
            : `Handed over execution ownership from '${fromAgent}' to '${toAgent}'.`;
    }

    match = normalized.match(
        /^Yielded back from '([^']+)' to '([^']+)' after specialist success\. Evidence:\s*(.+)$/i
    );
    if (match) {
        const [, fromAgent, toAgent, rawEvidence] = match;
        const compactedEvidence = compactEscalationResolvedDetails(rawEvidence)
            || truncateEscalationResolvedText(rawEvidence, MAX_SYSTEM_MESSAGE_SUMMARY_CHARS);
        return compactedEvidence
            ? `Yielded back from '${fromAgent}' to '${toAgent}' after specialist success. ${compactedEvidence}`
            : `Yielded back from '${fromAgent}' to '${toAgent}' after specialist success.`;
    }

    match = normalized.match(
        /^Yielded back from '([^']+)' to '([^']+)' after specialist reported CannotProceed:\s*(.+)$/i
    );
    if (match) {
        const [, fromAgent, toAgent, rawEvidence] = match;
        const compactedEvidence = compactEscalationResolvedDetails(rawEvidence)
            || truncateEscalationResolvedText(rawEvidence, MAX_SYSTEM_MESSAGE_SUMMARY_CHARS);
        return compactedEvidence
            ? `Yielded back from '${fromAgent}' to '${toAgent}' after specialist reported CannotProceed. ${compactedEvidence}`
            : `Yielded back from '${fromAgent}' to '${toAgent}' after specialist reported CannotProceed.`;
    }

    return null;
}

function compactTextMessage(text: string): string {
    return (
        compactDelegatedAgentCompletionText(text)
        ?? compactHandoverSystemText(text)
        ?? text
    );
}

export function getStructuredResponsePlainText(content: ChatMessageContent): string {
	switch (content.type) {
		case 'text':
			return content.text || '';
		case 'tool_call_executed':
			return content.summary || '';
		case 'rich_tool_result': {
			if (content.summary?.trim()) return content.summary;
			const textBlock = (content.content_blocks || []).find(
				(block) => block.type === 'text' && Boolean(block.text?.trim())
			);
			return textBlock?.type === 'text' ? textBlock.text || '' : '';
		}
		case 'attachment':
			return content.filename || '';
		case 'task_status_update':
			return content.summary || content.status || '';
		case 'escalation':
			return content.question || '';
		case 'escalation_resolved':
			return content.summary || '';
		default:
			return '';
	}
}

export function getMessageText(content: ChatMessageContent): string {
    switch (content.type) {
        case 'text':
            return compactTextMessage(content.text || '');
        case 'tool_call_executed':
            return `Action completed: ${content.tool_name}\n${content.summary}`;
        case 'rich_tool_result': {
            const blocks = content.content_blocks ?? [];
            const renderedBlocks = blocks.map(block => {
                if (block.type === 'text') {
                    return block.text || '';
                }
                if (block.type === 'url') {
                    return block.label || block.display_name || block.url || 'link';
                }
                return `File: ${block.label || block.display_name || block.relative_path || 'download'}`;
            }).filter(Boolean);
            return renderedBlocks.length > 0
                ? renderedBlocks.join('\n')
                : (content.summary || `Action completed: ${content.tool_name}`);
        }
        case 'attachment':
            return `Attachment: ${content.label || content.filename || 'file'}`;
        case 'task_status_update':
            return `Task ${content.task_id}: ${content.status}${content.summary ? '\n' + content.summary : ''}`;
        case 'escalation':
            return `Action Required: ${content.question || ''}`;
        case 'escalation_resolved':
            return getEscalationResolvedSummary(content.summary);
        default:
            return JSON.stringify(content);
    }
}

export function getMessageContentBlocks(content: ChatMessageContent): ContentBlockRecord[] {
    if (content.type === 'rich_tool_result') {
        return content.content_blocks ?? [];
    }
    if (content.type === 'task_status_update') {
        return content.output_files ?? [];
    }
    if (content.type === 'attachment' && content.filename) {
        return [{
            type: 'file',
            source: { type: 'session_output' },
            relative_path: content.filename,
            display_name: content.label || content.filename,
            mime_type: content.mime_type || 'application/octet-stream',
            absolute_path: content.absolute_path,
            label: content.label,
            size: content.size
        }];
    }
    return [];
}

function normalizeSourceSurface(value: string | null | undefined): string | undefined {
    const trimmed = value?.trim();
    return trimmed ? trimmed : undefined;
}

function normalizePresenceSessionId(value: string | null | undefined): string | undefined {
    const trimmed = value?.trim();
    return trimmed ? trimmed : undefined;
}

// =============================================================================
// Helper: convert API response to ChatMessage
// =============================================================================

export function convertMessage(raw: Record<string, unknown>): ChatMessage {
    const rawContent = raw.content as Record<string, unknown> | string | undefined;
    let content: ChatMessageContent;

    if (typeof rawContent === 'string') {
        content = { type: 'text', text: rawContent };
    } else if (rawContent && typeof rawContent === 'object') {
        // Normalize: if the content has a 'Text' variant (Rust enum), unwrap
        if ('Text' in rawContent) {
            content = { type: 'text', text: rawContent.Text as string };
        } else {
            content = rawContent as unknown as ChatMessageContent;
        }
    } else {
        content = { type: 'text', text: '' };
    }

    // Normalize direction: backend may send capitalized or Rust-enum-style
    let direction = (raw.direction as string) ?? 'system';
    direction = direction.toLowerCase();
    if (direction === 'user' || direction === 'assistant' || direction === 'system') {
        // valid
    } else {
        direction = 'system';
    }

    // Preserve `chat_turn_id` from the API payload so the embedded
    // RequestActivityCard can resolve the per-request event stream on
    // page reload — without this, `convertMessage` silently dropped
    // the field and the card never appeared inside historical assistant
    // bubbles (the in-session side-map is empty after a fresh load).
    const chatTurnId = normalizedMessageId(raw.chat_turn_id as string | null | undefined) || undefined;
    const sourceSurface = normalizeSourceSurface(raw.source_surface as string | null | undefined);
    const presenceSessionId = normalizePresenceSessionId(
        raw.presence_session_id as string | null | undefined
    );
    const voiceOrigin = typeof raw.voice_origin === 'boolean' ? raw.voice_origin : undefined;
    const speechSegments = Array.isArray(raw.speech_segments)
        ? (raw.speech_segments as unknown[]).flatMap((segment): SpeechBlock[] => {
            if (!segment || typeof segment !== 'object') return [];
            const candidate = segment as Record<string, unknown>;
            if (typeof candidate.text !== 'string' || !candidate.text.trim()) return [];
            return [candidate as unknown as SpeechBlock];
        })
        : undefined;
    const presentation =
        typeof raw.presentation === 'object' && raw.presentation !== null
            ? (raw.presentation as StructuredResponseV1)
            : undefined;

    return {
        id: raw.id as string,
        session_id: raw.session_id as string,
        direction: direction as ChatMessageDirection,
        content,
        created_at: raw.created_at as number,
        context_origin: raw.context_origin as ChatMessageOrigin | undefined,
        chat_turn_id: chatTurnId,
        source_surface: sourceSurface,
        presence_session_id: presenceSessionId,
        voice_origin: voiceOrigin,
        speech_segments: speechSegments,
        presentation,
    };
}

function convertSession(raw: Record<string, unknown>): ChatSession {
    let status = (raw.status as string) ?? 'active';
    status = status.toLowerCase();
    if (status !== 'active' && status !== 'archived') status = 'active';

    return {
        id: raw.id as string,
        principal: (raw.principal as string) ?? '',
        workspace: (raw.workspace as string) ?? '',
        agent_id: (raw.agent_id as string) ?? '',
        ui_thread_id: (raw.ui_thread_id as string) ?? 'general',
        title: (raw.title as string | null) ?? null,
        internal_voice: raw.internal_voice as ChatSession['internal_voice'],
        history_lane: raw.history_lane as ChatSession['history_lane'],
        is_default_session: raw.is_default_session === true,
        origin_channel: (raw.origin_channel as ChatChannel) ?? { channel_type: 'web' },
        status: status as ChatSessionStatus,
        created_at: raw.created_at as number,
        updated_at: raw.updated_at as number
    };
}

function extractAssistantMessage(data: Record<string, unknown>): ChatMessage | null {
    if (data.assistant_message && typeof data.assistant_message === 'object') {
        return convertMessage(data.assistant_message as Record<string, unknown>);
    }

    if (Array.isArray(data.messages)) {
        for (let index = data.messages.length - 1; index >= 0; index -= 1) {
            const candidate = convertMessage(data.messages[index] as Record<string, unknown>);
            if (candidate.direction === 'assistant' && candidate.content.type === 'text') {
                return candidate;
            }
        }
    }

    return null;
}

// =============================================================================
// Store Implementation
// =============================================================================

export function createChatStore() {
    const { subscribe, set, update } = writable<ChatStoreState>(defaultState);
    let lastScopeKey = currentChatScopeKey();
    let sessionOpenGeneration = 0;

    if (browser) {
        scopeIdentityStore.subscribe((scope) => {
            const scopeKey = `${scope.principal}:${scope.workspace}`;
            if (scopeKey === lastScopeKey) {
                return;
            }
            lastScopeKey = scopeKey;
            chatScopeGeneration += 1;
            clearChatRuntimeState();
            update((state) => ({
                ...defaultState,
                bubbleOpen: state.bubbleOpen
            }));
        });
    }

    return {
        subscribe,

        // =================================================================
        // Session Operations
        // =================================================================

        /**
         * Load the active session (or create one if none exists).
         * GET /api/magician/v2/chat/active
         */
        loadActiveSession: async (uiThreadId = 'general') => {
            sessionOpenGeneration += 1;
            const scopeToken = nextChatScopeToken();
            update(s => ({ ...s, isLoading: true, error: null }));
            try {
                const identity = await getChatIdentityQuery();
                if (!identity.ok) {
                    if (isStaleChatScopeToken(scopeToken)) {
                        return null;
                    }
                    update(s => ({ ...s, isLoading: false, error: identity.error }));
                    return null;
                }
                const response = await chatFetch(
                    `/api/magician/v2/chat/active?${buildScopedIdentityQuery(identity.query, uiThreadId)}`
                );
                if (!response.ok) {
                    throw new Error(`Failed to load active session (HTTP ${response.status})`);
                }
                const data = await response.json();
                const session = convertSession(data.session ?? data);
                // Track the *raw* (pre-coalesce) count separately —
                // `normalizeMessages` collapses pack_progress runs and
                // reconciles interactive sequences, so its output may
                // be substantially smaller than what the backend
                // returned. We need the raw count to detect "backend
                // hit its 200-message limit, more older history exists".
                const rawMessageCount = Array.isArray(data.messages) ? data.messages.length : 0;
                const messages = normalizeMessages(
                    Array.isArray(data.messages)
                        ? data.messages.map((m: Record<string, unknown>) => convertMessage(m))
                        : []
                );
                const threadSessions = await fetchSessionsForThread(uiThreadId);
                if (isStaleChatScopeToken(scopeToken)) {
                    return null;
                }
                scopeIdentityStore.observe(session.principal, session.workspace);

                // `/chat/active` returns at most 200 messages but does
                // not include a `has_more` flag, so we assume the
                // session is paginated whenever the response fills the
                // limit. If the assumption is wrong (exactly 200 in
                // total), `loadOlderMessages` will hit the backend
                // once, get an empty list, and flip the flag to false.
                // Better than the previous default of `false` which
                // hides the "Load earlier" button + suppresses the
                // scroll-to-top trigger entirely.
                const hasMoreMessages = rawMessageCount >= 200;
                update(s => applySendingState({
                    ...s,
                    activeSessionId: session.id,
                    messages,
                    hasMoreMessages,
                    viewingArchivedId: null,
                    isLoading: false,
                    error: null,
                    sessions:
                        threadSessions
                            ? mergeSession(
                                mergeThreadSessionsAuthoritatively(
                                    s.sessions,
                                    threadSessions,
                                    uiThreadId
                                ),
                                session
                            )
                            : mergeSession(s.sessions, session)
                }));

                return session;
            } catch (error) {
                if (isStaleChatScopeToken(scopeToken)) {
                    return null;
                }
                const msg = error instanceof Error ? error.message : 'Failed to load active session';
                update(s => ({ ...s, isLoading: false, error: msg }));
                console.error('[chatStore] loadActiveSession failed:', error);
                return null;
            }
        },

        /**
         * Create a new execution (archives the current active execution).
         * POST /api/magician/v2/chat/new
         */
        newExecution: async (uiThreadId = 'general') => {
            sessionOpenGeneration += 1;
            const scopeToken = nextChatScopeToken();
            update(s => ({ ...s, isLoading: true, error: null }));
            try {
                const identity = await getChatIdentityQuery();
                if (!identity.ok) {
                    if (isStaleChatScopeToken(scopeToken)) {
                        return null;
                    }
                    update(s => ({ ...s, isLoading: false, error: identity.error }));
                    return null;
                }

                const response = await chatFetch(
                    `/api/magician/v2/chat/new?${buildScopedIdentityQuery(identity.query, uiThreadId)}`,
                    {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' }
                    }
                );
                if (!response.ok) {
                    throw new Error(`Failed to create new execution (HTTP ${response.status})`);
                }
                const data = await response.json();
                const session = convertSession(data.session ?? data);
                if (isStaleChatScopeToken(scopeToken)) {
                    return null;
                }
                scopeIdentityStore.observe(session.principal, session.workspace);

                update(s => {
                    // Mirror the one-active-session-per-thread invariant locally.
                    const updatedSessions = s.sessions.map(sess =>
                        sess.ui_thread_id === uiThreadId && sess.status === 'active' && !sess.internal_voice
                            ? { ...sess, status: 'archived' as ChatSessionStatus }
                            : sess
                    );

                    return applySendingState({
                        ...s,
                        activeSessionId: session.id,
                        messages: [],
                        viewingArchivedId: null,
                        isLoading: false,
                        error: null,
                        sessions: mergeSession(updatedSessions, session)
                    });
                });

                return session;
            } catch (error) {
                if (isStaleChatScopeToken(scopeToken)) {
                    return null;
                }
                const msg = error instanceof Error ? error.message : 'Failed to create new execution';
                update(s => ({ ...s, isLoading: false, error: msg }));
                console.error('[chatStore] newExecution failed:', error);
                return null;
            }
        },

        /**
         * Load all sessions (execution history).
         * GET /api/magician/v2/chat/sessions
         */
        loadSessions: async (uiThreadId?: string | null) => {
            const scopeToken = nextChatScopeToken();
            const selectionGeneration = sessionOpenGeneration;
            try {
                const sessions = await fetchSessionsForThread(uiThreadId);
                if (sessions === null) {
                    const identity = await getChatIdentityQuery();
                    if (!identity.ok) {
                        if (isStaleChatScopeToken(scopeToken)) {
                            return [];
                        }
                        update(s => ({ ...s, error: identity.error }));
                        return [];
                    }
                    throw new Error('Failed to load sessions');
                }
                if (isStaleChatScopeToken(scopeToken)) {
                    return [];
                }
                const scopedSession = sessions[0];
                if (scopedSession) {
                    scopeIdentityStore.observe(scopedSession.principal, scopedSession.workspace);
                }
                // Execution conversations are intentionally absent from the
                // ordinary history list. Revalidate the exact selected session
                // before treating that omission as removal and switching away.
                const selectedState = get({ subscribe });
                const selectedId = currentVisibleSessionId(selectedState);
                const selected = selectedState.sessions.find(session => session.id === selectedId);
                const exactSelection = selectedId && selected
                    && (!uiThreadId || selected.ui_thread_id === uiThreadId)
                    && !sessions.some(session => session.id === selectedId)
                    ? (await fetchSessionDetail(selectedId))?.session : undefined;
                if (isStaleChatScopeToken(scopeToken) || selectionGeneration !== sessionOpenGeneration) return sessions;
                let replacementActiveSessionId: string | null = null;

                update(s => {
                    let merged = mergeThreadSessionsAuthoritatively(
                        s.sessions,
                        sessions,
                        uiThreadId
                    );
                    if (exactSelection) merged = mergeSession(merged, exactSelection);
                    const activeSession =
                        s.activeSessionId
                            ? s.sessions.find(session => session.id === s.activeSessionId)
                                ?? merged.find(session => session.id === s.activeSessionId)
                                ?? null
                            : null;
                    const archivedSession =
                        s.viewingArchivedId
                            ? s.sessions.find(session => session.id === s.viewingArchivedId)
                                ?? merged.find(session => session.id === s.viewingArchivedId)
                                ?? null
                            : null;
                    const scopeMatches = (session: ChatSession | null): boolean =>
                        !uiThreadId || session?.ui_thread_id === uiThreadId;

                    let nextActiveSessionId = s.activeSessionId;
                    let nextViewingArchivedId = s.viewingArchivedId;
                    let nextMessages = s.messages;

                    const candidateThreadId =
                        archivedSession?.ui_thread_id
                        ?? activeSession?.ui_thread_id
                        ?? uiThreadId
                        ?? null;
                    const replacementActive =
                        candidateThreadId
                            ? merged.find(
                                session =>
                                    session.status === 'active'
                                    && !session.internal_voice
                                    && session.ui_thread_id === candidateThreadId
                            ) ?? null
                            : null;

                    if (s.viewingArchivedId && scopeMatches(archivedSession)) {
                        const refreshedArchived = merged.find(
                            session => session.id === s.viewingArchivedId
                        );
                        if (!refreshedArchived || refreshedArchived.status !== 'archived') {
                            nextViewingArchivedId = null;
                            if (replacementActive) {
                                nextActiveSessionId = replacementActive.id;
                                nextMessages = [];
                                replacementActiveSessionId = replacementActive.id;
                            } else {
                                nextMessages = [];
                            }
                        }
                    } else if (s.activeSessionId && scopeMatches(activeSession)) {
                        const refreshedActive = merged.find(
                            session => session.id === s.activeSessionId
                        );
                        if (!refreshedActive || refreshedActive.status !== 'active') {
                            if (replacementActive) {
                                nextActiveSessionId = replacementActive.id;
                                nextMessages = [];
                                replacementActiveSessionId = replacementActive.id;
                            } else {
                                nextActiveSessionId = null;
                                nextMessages = [];
                            }
                        }
                    }

                    return applySendingState({
                        ...s,
                        sessions: merged,
                        activeSessionId: nextActiveSessionId,
                        viewingArchivedId: nextViewingArchivedId,
                        messages: nextMessages,
                        error: null
                    });
                });

                if (replacementActiveSessionId) {
                    const activeDetail = await fetchSessionDetail(replacementActiveSessionId);
                    if (activeDetail && !isStaleChatScopeToken(scopeToken)) {
                        update(s => {
                            if (
                                s.viewingArchivedId !== null
                                || s.activeSessionId !== replacementActiveSessionId
                            ) {
                                return s;
                            }
                            return applySendingState({
                                ...s,
                                sessions: mergeSession(s.sessions, activeDetail.session),
                                messages: normalizeMessages(
                                    appendUniqueMessagesInOrder(activeDetail.messages, s.messages)
                                ),
                                error: null
                            });
                        });
                    }
                }

                return sessions;
            } catch (error) {
                if (isStaleChatScopeToken(scopeToken)) {
                    return [];
                }
                const msg = error instanceof Error ? error.message : 'Failed to load sessions';
                update(s => ({ ...s, error: msg }));
                console.error('[chatStore] loadSessions failed:', error);
                return [];
            }
        },

        /**
         * Load messages for a specific session.
         * GET /api/magician/v2/chat/sessions/{id}
         */
        loadMessages: async (sessionId: string, options: { preserveLive?: boolean } = {}) => {
            const scopeToken = nextChatScopeToken();
            const selectionGeneration = sessionOpenGeneration;
            if (!options.preserveLive) update(s => ({ ...s, isLoading: true, error: null }));
            try {
                const identity = await getChatIdentityQuery();
                if (!identity.ok) {
                    if (isStaleChatScopeToken(scopeToken)) {
                        return [];
                    }
                    update(s => ({ ...s, isLoading: false, error: identity.error }));
                    return [];
                }

                const params = new URLSearchParams(identity.query);
                params.set('limit', '50');
                const response = await chatFetch(
                    `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/messages?${params.toString()}`
                );
                if (!response.ok) {
                    throw new Error(`Failed to load messages (HTTP ${response.status})`);
                }
                const data = await response.json();
                const messages = normalizeMessages(
                    Array.isArray(data.messages)
                        ? data.messages.map((m: Record<string, unknown>) => convertMessage(m))
                        : []
                );
                const hasMore = data.has_more === true;
                if (isStaleChatScopeToken(scopeToken)) {
                    return [];
                }

                update(s => {
                    if (options.preserveLive && (selectionGeneration !== sessionOpenGeneration || currentVisibleSessionId(s) !== sessionId)) return s;
                    // Background admission saves another user question. Merge that
                    // snapshot without removing the foreground stream's placeholder
                    // or its partial text; subsequent token events still address it.
                    const refreshed = options.preserveLive
                        ? normalizeMessages(appendUniqueMessagesInOrder(
                            messages.reduce(stripMatchingOptimisticUserMessage, s.messages), messages))
                        : messages;
                    return { ...s, messages: refreshed, hasMoreMessages: options.preserveLive ? s.hasMoreMessages || hasMore : hasMore,
                        isLoading: false, error: null };
                });

                return messages;
            } catch (error) {
                if (isStaleChatScopeToken(scopeToken)) {
                    return [];
                }
                const msg = error instanceof Error ? error.message : 'Failed to load messages';
                update(s => ({ ...s, isLoading: false, error: msg }));
                console.error('[chatStore] loadMessages failed:', error);
                return [];
            }
        },

        /**
         * Load older messages (scroll-to-top pagination).
         * Prepends older messages before the current oldest message.
         */
        loadOlderMessages: async (sessionId: string) => {
            const scopeToken = nextChatScopeToken();
            // Atomic guard: read + set flag in one update to prevent concurrent calls
            let shouldLoad = false;
            let oldestId: string | undefined;
            update(s => {
                if (s.isLoadingOlder || !s.hasMoreMessages || s.messages.length === 0) {
                    return s;
                }
                shouldLoad = true;
                oldestId = s.messages[0]?.id;
                return { ...s, isLoadingOlder: true };
            });
            if (!shouldLoad) return;
            try {
                const identity = await getChatIdentityQuery();
                if (!identity.ok) {
                    if (isStaleChatScopeToken(scopeToken)) {
                        return;
                    }
                    update(s => ({ ...s, isLoadingOlder: false }));
                    return;
                }

                const params = new URLSearchParams(identity.query);
                params.set('limit', '50');
                if (oldestId) params.set('before', oldestId);

                const response = await chatFetch(
                    `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/messages?${params.toString()}`
                );
                if (!response.ok) throw new Error(`HTTP ${response.status}`);

                const data = await response.json();
                const olderMessages = normalizeMessages(
                    Array.isArray(data.messages)
                        ? data.messages.map((m: Record<string, unknown>) => convertMessage(m))
                        : []
                );
                const hasMore = data.has_more === true;
                if (isStaleChatScopeToken(scopeToken)) {
                    return;
                }

                update(s => ({
                    ...s,
                    messages: normalizeMessages([...olderMessages, ...s.messages]),
                    hasMoreMessages: hasMore,
                    isLoadingOlder: false,
                }));
            } catch (error) {
                console.error('[chatStore] loadOlderMessages failed:', error);
                if (isStaleChatScopeToken(scopeToken)) {
                    return;
                }
                update(s => ({ ...s, isLoadingOlder: false }));
            }
        },

        /**
         * Send a message via SSE streaming endpoint.
         * POST /api/magician/v2/chat/sessions/{id}/messages/stream
         *
         * On stream interruption, reconciles from the authoritative server state
         * instead of blindly retrying the turn.
         */
        sendMessageStreaming: async (
            sessionId: string,
            text: string,
            profile?: string | null,
            attachmentIds: string[] = [],
            options: ChatSendOptions = {}
        ) => {
            options = withComposerHarness(options);
            const scopeToken = nextChatScopeToken();
            const trimmed = text.trim();
            if (!trimmed && attachmentIds.length === 0) return null;
            if (attachmentIds.length > 0) {
                return chatStore.sendMessage(sessionId, text, profile, attachmentIds, options);
            }

            const identity = await getChatIdentityQuery();
            if (!identity.ok) {
                if (isStaleChatScopeToken(scopeToken)) {
                    return null;
                }
                update(s => ({ ...s, error: identity.error }));
                return null;
            }

            // Optimistically add user message
            const tempId = 'temp-user-' + crypto.randomUUID();
            // Per-request correlation id. The backend stamps this into
            // every event emitted while processing this turn — chat-side
            // LLM/tool emissions plus delegated sub-agent runs via the
            // chat-fanout re-stamp. The `RequestActivityCard` subscribes
            // to the per-turn endpoint
            // (`/api/magician/v2/chat/sessions/{sid}/turns/{cid}/events[/stream]`)
            // and gets exactly this request's stream, sourced from the
            // per-turn JSONL projection `ChatTurnEventSink` writes.
            // Stamped onto the user message so the card has a stable
            // anchor.
            const chatTurnId = 'chat-turn-' + crypto.randomUUID();
            const sourceSurface = normalizeSourceSurface(options.sourceSurface);
            const presenceSessionId = normalizePresenceSessionId(options.presenceSessionId);
            const codingChoice = resolvedCodingChoice(options);
            const userMessage: ChatMessage = {
                id: tempId,
                session_id: sessionId,
                direction: 'user',
                content: { type: 'text', text: trimmed },
                created_at: Date.now(),
                chat_turn_id: chatTurnId,
                source_surface: sourceSurface,
                presence_session_id: presenceSessionId
            };
            recordChatTurnIdForMessage(tempId, chatTurnId);

            // Add streaming assistant placeholder
            const streamingMsgId = 'streaming-' + Date.now();
            const streamingMessage: ChatMessage = {
                id: streamingMsgId,
                session_id: sessionId,
                direction: 'assistant',
                content: { type: 'text', text: '' },
                created_at: Date.now() + 1,
                chat_turn_id: chatTurnId,
                source_surface: sourceSurface,
                presence_session_id: presenceSessionId
            };
            recordChatTurnIdForMessage(streamingMsgId, chatTurnId);

            beginSendingSession(sessionId);
            update(s => applySendingState({
                ...s,
                messages: [...s.messages, userMessage, streamingMessage],
                error: null
            }));
            beginAuthoritativeBatch(sessionId);

            try {
                const response = await chatFetch(
                    `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/messages/stream?${identity.query}`,
                    {
                        method: 'POST',
                        headers: { 'Content-Type': 'application/json' },
                        body: JSON.stringify({
                            text: trimmed,
                            chat_turn_id: chatTurnId,
                            ...(profile && (!options.harnessEngine || ['magician', 'pi'].includes(options.harnessEngine)) ? { profile } : {}),
                            ...(options.harnessEngine ? { harness_engine: options.harnessEngine } : {}),
                            ...(options.harnessModel ? { harness_model: options.harnessModel } : {}),
                            ...(options.mode ? { mode: options.mode } : {}),
                            ...(options.planTaskId ? { plan_task_id: options.planTaskId } : {}),
                            ...(options.planQuestionId
                                ? { plan_question_id: options.planQuestionId }
                                : {}),
                            ...(sourceSurface ? { source_surface: sourceSurface } : {}),
                            ...(presenceSessionId ? { presence_session_id: presenceSessionId } : {}),
                            ...(options.voiceOrigin ? { voice_origin: true } : {}),
                            ...(codingChoice ? { coding_choice: codingChoice } : {})
                        }),
                        timeoutMs: LONG_FETCH_TIMEOUT_MS
                    }
                );

                if (!response.ok || !response.body) {
                    throw new Error('Streaming not available');
                }

                const reader = response.body.getReader();
                const decoder = new TextDecoder();
                let buffer = '';
                let streamedText = '';
                let doneData: Record<string, unknown> | null = null;

                while (true) {
                    const { done, value } = await reader.read();
                    if (done) break;
                    if (isStaleChatScopeToken(scopeToken)) {
                        void reader.cancel().catch(() => {});
                        return null;
                    }
                    buffer += decoder.decode(value, { stream: true });

                    // Split on double newlines (SSE event boundary)
                    const parts = buffer.split('\n\n');
                    buffer = parts.pop() ?? '';

                    for (const part of parts) {
                        if (!part.trim()) continue;
                        const eventMatch = part.match(/^event: (.+)$/m);
                        const dataMatch = part.match(/^data: (.+)$/m);
                        if (!dataMatch) continue;

                        const eventType = eventMatch?.[1] ?? 'message';
                        let data: Record<string, unknown>;
                        try {
                            data = JSON.parse(dataMatch[1]);
                        } catch {
                            continue;
                        }

                        if (eventType === 'token') {
                            streamedText += (data.text as string) ?? '';
                            if (isStaleChatScopeToken(scopeToken)) {
                                continue;
                            }
                            // Update the streaming message content in-place
                            update(s => {
                                const msgs = s.messages;
                                const idx = msgs.findIndex(m => m.id === streamingMsgId);
                                if (idx === -1) return s;
                                const updated = [...msgs];
                                updated[idx] = {
                                    ...updated[idx],
                                    content: { type: 'text' as const, text: streamedText }
                                };
                                return { ...s, messages: updated };
                            });
                        } else if (eventType === 'done') {
                            doneData = data;
                        } else if (eventType === 'error') {
                            // Backend emits `{ error: msg }` (see
                            // `chat_api.rs::StreamDelta::Error` → SSE
                            // formatter). Older paths used `message` —
                            // read both so the actual provider error
                            // (e.g. "Your input exceeds the context
                            // window of this model") survives across
                            // the wire instead of collapsing to a
                            // generic "Stream error" toast.
                            const errText =
                                (data.error as string | undefined) ??
                                (data.message as string | undefined) ??
                                'Stream error';
                            throw new Error(errText);
                        }
                    }
                }

                if (!doneData) {
                    throw new Error('Streaming ended before completion event');
                }
                if (isStaleChatScopeToken(scopeToken)) {
                    return null;
                }

                // Transfer the optimistic message's chat_turn_id onto
                // the authoritative server message ids — server doesn't
                // persist `chat_turn_id` on the chat message record, so
                // without this transfer the activity card unmounts the
                // moment the optimistic msg is replaced. Covers both
                // the explicit `user_message` / `assistant_message`
                // shapes and the bulk `messages[]` shape.
                {
                    const userMsg = doneData.user_message as Record<string, unknown> | undefined;
                    if (userMsg && typeof userMsg.id === 'string') {
                        recordChatTurnIdForMessage(userMsg.id, chatTurnId);
                    }
                    const asstMsg = doneData.assistant_message as Record<string, unknown> | undefined;
                    if (asstMsg && typeof asstMsg.id === 'string') {
                        recordChatTurnIdForMessage(asstMsg.id, chatTurnId);
                    }
                    const msgs = doneData.messages as Array<Record<string, unknown>> | undefined;
                    if (Array.isArray(msgs)) {
                        for (const m of msgs) {
                            if (typeof m?.id === 'string') {
                                recordChatTurnIdForMessage(m.id, chatTurnId);
                            }
                        }
                    }
                }

                // Finalize: replace streaming placeholder with real messages
                update(s => {
                    endSendingSession(sessionId);
                    // View changed while we were streaming -- discard stale results
                    if (currentVisibleSessionId(s) !== sessionId) {
                        let updatedSessions = s.sessions;
                        if (doneData.session_title && sessionId) {
                            updatedSessions = updatedSessions.map(sess =>
                                sess.id === sessionId ? { ...sess, title: doneData!.session_title as string } : sess
                            );
                        }
                        dropAuthoritativeBatch(sessionId);
                        return applySendingState({
                            ...s,
                            sessions: updatedSessions
                        });
                    }

                    // Remove temp user message and streaming placeholder
                    let newMessages = s.messages.filter(
                        m => m.id !== tempId && m.id !== streamingMsgId
                    );

                    newMessages = finishAuthoritativeBatch(
                        sessionId,
                        newMessages,
                        doneData as Record<string, unknown>
                    );
                    // Auto-executed tool results are persisted server-side for
                    // audit/debugging but NOT shown in the conversation. The
                    // LLM's assistant message is the user-facing synthesis.
                    // (tool_executed messages are still loadable from history
                    // if the user explicitly requests raw output.)
                    // Update session title if auto-generated
                    let updatedSessions = s.sessions;
                    if (doneData.session_title && sessionId) {
                        updatedSessions = updatedSessions.map(sess =>
                            sess.id === sessionId ? { ...sess, title: doneData!.session_title as string } : sess
                        );
                    }

                    return applySendingState({
                        ...s,
                        messages: newMessages,
                        sessions: updatedSessions,
                        error: null
                    });
                });

                return null;
            } catch (err) {
                console.warn('[chatStore] streaming failed; reconciling from server:', err);
                const recovery = await fetchSessionDetail(sessionId);
                if (isStaleChatScopeToken(scopeToken)) {
                    return null;
                }
                // Always preserve the underlying error message so the
                // user sees the actual failure (e.g. "Your input
                // exceeds the context window of this model"). The
                // server-reload status, when applicable, is appended
                // as a hint rather than replacing the cause — the
                // previous generic "Streaming connection interrupted.
                // Reloaded…" wording dropped the cause entirely and
                // left the user with no way to tell why their turn
                // failed.
                const rawError = err instanceof Error
                    ? err.message
                    : (typeof err === 'string' ? err : 'Streaming interrupted.');
                const recoveryError = recovery
                    ? `${rawError} (reloaded the session from server)`
                    : rawError;

                update(s => {
                    endSendingSession(sessionId);
                    const currentViewId = s.viewingArchivedId ?? s.activeSessionId;
                    // Build a synthetic assistant-side error message so
                    // the failure renders inline next to the user's
                    // turn rather than only as a banner/toast. The
                    // streaming placeholder gets REPLACED (same chat
                    // surface position, no orphaned "thinking" bubble)
                    // and the message id is recognisable as a
                    // synthetic so future code paths can re-render or
                    // dismiss it if needed. Direction is `assistant`
                    // so the bubble appears on the assistant side
                    // (correct visual mapping for "your turn failed").
                    const errorMsg: ChatMessage = {
                        id: 'error-' + crypto.randomUUID(),
                        session_id: sessionId,
                        direction: 'assistant',
                        content: { type: 'text', text: `⚠️ ${rawError}` },
                        created_at: Date.now(),
                        chat_turn_id: chatTurnId,
                        source_surface: sourceSurface,
                        presence_session_id: presenceSessionId
                    };
                    recordChatTurnIdForMessage(errorMsg.id, chatTurnId);

                    const cleanedMessages = s.messages.filter(
                        m => m.id !== tempId && !m.id.startsWith('streaming-')
                    );
                    if (currentViewId !== sessionId) {
                        dropAuthoritativeBatch(sessionId);
                        return applySendingState({
                            ...s,
                            messages: cleanedMessages,
                            error: recoveryError
                        });
                    }

                    const reconciledMessages = recovery
                        ? recovery.messages
                        : cancelAuthoritativeBatch(sessionId, cleanedMessages);
                    const reconciledSessions = recovery
                        ? mergeSession(s.sessions, recovery.session)
                        : s.sessions;

                    // Tack the synthetic error message onto the tail.
                    // Server-reload may have already brought back the
                    // user message (without an assistant reply) — the
                    // error bubble sits right after it, marking the
                    // turn as failed inline. If the server reload
                    // somehow includes its own assistant message
                    // (recovery from a partial commit), the error
                    // bubble lands after it; both are stamped with
                    // the same chat_turn_id so the activity card
                    // still anchors correctly.
                    return applySendingState({
                        ...s,
                        messages: [...reconciledMessages, errorMsg],
                        sessions: reconciledSessions,
                        error: recoveryError
                    });
                });
                return null;
            }
        },

        /**
         * Send a message to the active session.
         * POST /api/magician/v2/chat/sessions/{id}/messages
         */
        sendMessage: async (
            sessionId: string,
            text: string,
            profile?: string | null,
            attachmentIds: string[] = [],
            options: ChatSendOptions = {}
        ) => {
            options = withComposerHarness(options);
            const scopeToken = nextChatScopeToken();
            const trimmed = text.trim();
            if (!trimmed && attachmentIds.length === 0) return null;

            const identity = await getChatIdentityQuery();
            if (!identity.ok) {
                if (isStaleChatScopeToken(scopeToken)) {
                    return null;
                }
                update(s => ({ ...s, error: identity.error }));
                return null;
            }

            // Text-bearing attachment turns should behave like normal chat
            // turns immediately. Screen-capture/tutor HUD prompts always carry
            // a staged image attachment; without the optimistic user bubble the
            // HUD has no visible submitted message or live activity-card anchor
            // until the backend response reconciles the session.
            const useOptimisticText = trimmed.length > 0;
            const tempId = useOptimisticText ? 'temp-user-' + crypto.randomUUID() : null;
            // Same per-request id as the streaming path — see the note
            // on chatTurnId there for the lifecycle contract.
            const chatTurnId = 'chat-turn-' + crypto.randomUUID();
            const sourceSurface = normalizeSourceSurface(options.sourceSurface);
            const presenceSessionId = normalizePresenceSessionId(options.presenceSessionId);
            const userMessage: ChatMessage | null = useOptimisticText ? {
                id: tempId!,
                session_id: sessionId,
                direction: 'user',
                content: { type: 'text', text: trimmed },
                created_at: Date.now(),
                chat_turn_id: chatTurnId,
                source_surface: sourceSurface,
                presence_session_id: presenceSessionId
            } : null;
            const pendingAssistantId = 'streaming-' + Date.now();
            const pendingAssistantMessage: ChatMessage = {
                id: pendingAssistantId,
                session_id: sessionId,
                direction: 'assistant',
                content: { type: 'text', text: '' },
                created_at: Date.now() + 1,
                chat_turn_id: chatTurnId,
                source_surface: sourceSurface,
                presence_session_id: presenceSessionId
            };

            beginSendingSession(sessionId);
            update(s => applySendingState({
                ...s,
                messages: [
                    ...s.messages,
                    ...(userMessage ? [userMessage] : []),
                    pendingAssistantMessage
                ],
                error: null
            }));
            beginAuthoritativeBatch(sessionId);

            try {
                const bodyPayload: Record<string, unknown> = {
                    chat_turn_id: chatTurnId,
                };
                if (trimmed) {
                    bodyPayload.text = trimmed;
                }
                if (attachmentIds.length > 0) {
                    bodyPayload.attachment_ids = attachmentIds;
                }
                if (profile && (!options.harnessEngine || ['magician', 'pi'].includes(options.harnessEngine))) {
                    bodyPayload.profile = profile;
                }
                if (options.harnessEngine) {
                    bodyPayload.harness_engine = options.harnessEngine;
                }
                if (options.harnessModel) {
                    bodyPayload.harness_model = options.harnessModel;
                }
                if (options.mode) {
                    bodyPayload.mode = options.mode;
                }
                if (options.planTaskId) {
                    bodyPayload.plan_task_id = options.planTaskId;
                }
                if (options.planQuestionId) {
                    bodyPayload.plan_question_id = options.planQuestionId;
                }
                if (sourceSurface) {
                    bodyPayload.source_surface = sourceSurface;
                }
                if (presenceSessionId) {
                    bodyPayload.presence_session_id = presenceSessionId;
                }
                if (options.voiceOrigin) {
                    bodyPayload.voice_origin = true;
                }
                const codingChoice = resolvedCodingChoice(options);
                if (codingChoice) {
                    bodyPayload.coding_choice = codingChoice;
                }
                const response = await chatFetch(
                    `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/messages?${identity.query}`,
                    {
                        method: 'POST',
                        headers: { 'Content-Type': 'application/json' },
                        body: JSON.stringify(bodyPayload),
                        timeoutMs: LONG_FETCH_TIMEOUT_MS
                    }
                );

                if (!response.ok) {
                    let errorMessage = `Failed to send message (HTTP ${response.status})`;
                    try {
                        const errorBody = await response.json();
                        if (typeof errorBody?.error === 'string' && errorBody.error.trim()) {
                            errorMessage = errorBody.error;
                        } else if (typeof errorBody?.details === 'string' && errorBody.details.trim()) {
                            errorMessage = errorBody.details;
                        }
                    } catch {
                        // Ignore parse failures and keep the default status-based message.
                    }
                    throw new Error(errorMessage);
                }

                const data = await response.json();
                if (isStaleChatScopeToken(scopeToken)) {
                    return null;
                }

                // Backend held this message in the pending-replay queue
                // because a turn was already in flight. The user's
                // optimistic message should STAY on screen — drop only
                // the pending-assistant placeholder. The realtime
                // `ChatMessageReceived` event will deliver the real user
                // message + assistant reply when the backend's drain
                // dispatch fires.
                if (data?.queued) {
                    update(s => {
                        endSendingSession(sessionId);
                        if (currentVisibleSessionId(s) !== sessionId) {
                            dropAuthoritativeBatch(sessionId);
                            return applySendingState({ ...s });
                        }
                        const nextMessages = s.messages.filter(
                            m => m.id !== pendingAssistantId
                        );
                        return applySendingState({
                            ...s,
                            messages: cancelAuthoritativeBatch(sessionId, nextMessages),
                            error: null,
                        });
                    });
                    try {
                        const queued = data.queued;
                        const position = typeof queued?.position === 'number'
                            ? queued.position
                            : null;
                        const positionText = position !== null ? ` (#${position})` : '';
                        // v0.6.655 — backend now sets `queued.reason` so we
                        // can render a contextual "waiting on task X" string
                        // instead of the generic "queued behind in-flight
                        // turn." Fall back to the generic message when
                        // `reason` is absent (older backend or pre-receipt
                        // entries).
                        const reasonKind = queued?.reason?.kind;
                        let suffix = ' — will dispatch after the current turn finishes.';
                        if (reasonKind === 'tailing_task') {
                            const taskId = queued?.reason?.task_id;
                            suffix = taskId
                                ? ` — waiting on task ${taskId} to finish.`
                                : ' — waiting on the watched task to finish.';
                        } else if (reasonKind === 'in_flight_turn') {
                            suffix = ' — will dispatch after the current turn finishes.';
                        }
                        showInfo(`Message queued${positionText}${suffix}`);
                    } catch {
                        // notification module may not be in scope in all builds
                    }
                    return null;
                }

                // Replace optimistic user message with server version and add assistant response
                update(s => {
                    endSendingSession(sessionId);
                    // View changed while we were waiting -- discard stale results
                    if (currentVisibleSessionId(s) !== sessionId) {
                        let updatedSessions = s.sessions;
                        if (data.session_title && sessionId) {
                            updatedSessions = updatedSessions.map(sess =>
                                sess.id === sessionId ? { ...sess, title: data.session_title } : sess
                            );
                        }
                        dropAuthoritativeBatch(sessionId);
                        return applySendingState({
                            ...s,
                            sessions: updatedSessions
                        });
                    }

                    let newMessages = s.messages;

                    // Always remove the optimistic message (server or WS will provide the real one)
                    newMessages = newMessages.filter(
                        m => m.id !== tempId && m.id !== pendingAssistantId
                    );

                    newMessages = finishAuthoritativeBatch(sessionId, newMessages, data);

                    // Process executed tool results from HTTP response (dedup against WS)
                    // Auto-executed tool results: persisted server-side but not
                    // shown in the conversation (LLM synthesizes the response).

                    // Update session title if auto-generated by backend
                    let updatedSessions = s.sessions;
                    if (data.session_title && sessionId) {
                        updatedSessions = updatedSessions.map(sess =>
                            sess.id === sessionId ? { ...sess, title: data.session_title } : sess
                        );
                    }

                    return applySendingState({
                        ...s,
                        messages: newMessages,
                        sessions: updatedSessions,
                        error: null
                    });
                });

                return extractAssistantMessage(data);
            } catch (error) {
                if (isStaleChatScopeToken(scopeToken)) {
                    return null;
                }
                const errorMessage = error instanceof Error ? error.message : 'Failed to send message';
                // Remove the optimistic message that never reached the server
                update(s => {
                    endSendingSession(sessionId);
                    const currentViewId = s.viewingArchivedId ?? s.activeSessionId;
                    const nextMessages = s.messages.filter(
                        m => m.id !== tempId && m.id !== pendingAssistantId
                    );
                    if (currentViewId !== sessionId) {
                        dropAuthoritativeBatch(sessionId);
                        return applySendingState({
                            ...s,
                            messages: nextMessages,
                            error: errorMessage
                        });
                    }
                    return applySendingState({
                        ...s,
                        messages: cancelAuthoritativeBatch(sessionId, nextMessages),
                        error: errorMessage
                    });
                });
                console.error('[chatStore] sendMessage failed:', error);
                return null;
            }
        },

        uploadAttachment: async (sessionId: string, file: File): Promise<UploadedAttachment> => {
            const scopeToken = nextChatScopeToken();
            const identity = await getChatIdentityQuery();
            if (!identity.ok) {
                if (isStaleChatScopeToken(scopeToken)) {
                    throw new Error('Attachment upload was discarded after scope change');
                }
                update(s => ({ ...s, error: identity.error }));
                throw new Error(identity.error);
            }

            const formData = new FormData();
            formData.append('file', file, file.name);

            const response = await chatFetch(
                `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/attachments?${identity.query}`,
                {
                    method: 'POST',
                    body: formData
                }
            );

            if (!response.ok) {
                let message = `Failed to upload attachment (HTTP ${response.status})`;
                try {
                    const data = await response.json();
                    if (typeof data?.error === 'string' && data.error.trim()) {
                        message = data.error;
                    }
                } catch {
                    // ignore JSON parse failures and use the default message
                }
                if (isStaleChatScopeToken(scopeToken)) {
                    throw new Error('Attachment upload was discarded after scope change');
                }
                update(s => ({ ...s, error: message }));
                throw new Error(message);
            }

            const uploaded = await response.json() as UploadedAttachment;
            if (isStaleChatScopeToken(scopeToken)) {
                throw new Error('Attachment upload was discarded after scope change');
            }
            return {
                ...uploaded,
                label: file.name
            };
        },

        /**
         * Open one exact session from history.
         *
         * Active sessions become the selected writable session. Archived
         * sessions remain a read-only overlay and preserve the selected active
         * session so "Return to active" still has an unambiguous destination.
         */
        openSession: async (sessionId: string, target?: ChatMessageTarget) => {
            const scopeToken = nextChatScopeToken();
            const generation = ++sessionOpenGeneration;
            const stale = () => generation !== sessionOpenGeneration || isStaleChatScopeToken(scopeToken);
            update(s => ({ ...s, isLoading: true, error: null, focusedMessageId: null }));
            try {
                const detail = await fetchSessionDetail(sessionId, target, () => !stale());
                if (stale()) {
                    return null;
                }
                if (!detail) {
                    throw new Error('Failed to load session');
                }

                const { session, messages } = detail;
                scopeIdentityStore.observe(session.principal, session.workspace);

                update(s => applySendingState({
                    ...s,
                    activeSessionId:
                        session.status === 'active' ? session.id : s.activeSessionId,
                    messages,
                    hasMoreMessages: detail.hasMore,
                    focusedMessageId: detail.focusedMessageId ?? null,
                    viewingArchivedId:
                        session.status === 'archived' ? session.id : null,
                    isLoading: false,
                    error: target && !detail.focusedMessageId ? "The original answer is no longer available in this conversation." : null,
                    sessions: mergeSession(s.sessions, session)
                }));
                return session;
            } catch (error) {
                if (stale()) {
                    return null;
                }
                const msg = error instanceof Error ? error.message : 'Failed to load session';
                update(s => ({ ...s, isLoading: false, error: msg }));
                console.error('[chatStore] openSession failed:', error);
                return null;
            }
        },

        /**
         * Return to viewing the active session (after viewing an archived execution).
         */
        returnToActive: async () => {
            sessionOpenGeneration += 1;
            const scopeToken = nextChatScopeToken();
            const state = get({ subscribe });
            if (!state.activeSessionId) return;

            update(s => ({ ...s, viewingArchivedId: null, isLoading: true }));
            try {
                const identity = await getChatIdentityQuery();
                if (!identity.ok) {
                    if (isStaleChatScopeToken(scopeToken)) {
                        return;
                    }
                    update(s => ({ ...s, isLoading: false, viewingArchivedId: null, error: identity.error }));
                    return;
                }

                const response = await chatFetch(
                    `/api/magician/v2/chat/sessions/${encodeURIComponent(state.activeSessionId)}?${identity.query}`
                );
                if (!response.ok) {
                    throw new Error(`Failed to load active session messages (HTTP ${response.status})`);
                }
                const data = await response.json();
                const messages = normalizeMessages(
                    Array.isArray(data.messages)
                        ? data.messages.map((m: Record<string, unknown>) => convertMessage(m))
                        : []
                );
                if (isStaleChatScopeToken(scopeToken)) {
                    return;
                }

                update(s => applySendingState({
                    ...s,
                    messages,
                    isLoading: false,
                    error: null
                }));
            } catch (error) {
                if (isStaleChatScopeToken(scopeToken)) {
                    return;
                }
                const msg = error instanceof Error ? error.message : 'Failed to load active session';
                update(s => ({ ...s, isLoading: false, viewingArchivedId: null, error: msg }));
            }
        },

        /**
         * Update a session title.
         * PATCH /api/magician/v2/chat/sessions/{id}
         */
        updateTitle: async (sessionId: string, title: string) => {
            const scopeToken = nextChatScopeToken();
            try {
                const identity = await getChatIdentityQuery();
                if (!identity.ok) {
                    if (isStaleChatScopeToken(scopeToken)) {
                        return;
                    }
                    update(s => ({ ...s, error: identity.error }));
                    return;
                }

                const response = await chatFetch(`/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}?${identity.query}`, {
                    method: 'PATCH',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ title })
                });

                if (!response.ok) {
                    throw new Error(`Failed to update title (HTTP ${response.status})`);
                }
                if (isStaleChatScopeToken(scopeToken)) {
                    return;
                }

                update(s => ({
                    ...s,
                    sessions: s.sessions.map(sess =>
                        sess.id === sessionId
                            ? { ...sess, title, updated_at: Date.now() }
                            : sess
                    )
                }));
            } catch (error) {
                if (isStaleChatScopeToken(scopeToken)) {
                    return;
                }
                const msg = error instanceof Error ? error.message : 'Failed to update title';
                update(s => ({ ...s, error: msg }));
                console.error('[chatStore] updateTitle failed:', error);
            }
        },

        /**
         * Archive a session (PATCH status → archived).
         */
        archiveSession: async (sessionId: string) => {
            const scopeToken = nextChatScopeToken();
            try {
                const identity = await getChatIdentityQuery();
                if (!identity.ok) return;
                const response = await chatFetch(`/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}?${identity.query}`, {
                    method: 'PATCH',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ status: 'archived' })
                });
                if (!response.ok) throw new Error(`HTTP ${response.status}`);
                if (isStaleChatScopeToken(scopeToken)) {
                    return;
                }
                update(s => ({
                    ...s,
                    sessions: s.sessions.map(sess =>
                        sess.id === sessionId ? { ...sess, status: 'archived' as ChatSessionStatus, updated_at: Date.now() } : sess
                    ),
                    // If we archived the active session, clear it
                    activeSessionId: s.activeSessionId === sessionId ? null : s.activeSessionId,
                    messages: s.activeSessionId === sessionId ? [] : s.messages
                }));
            } catch (error) {
                if (isStaleChatScopeToken(scopeToken)) {
                    return;
                }
                console.error('[chatStore] archiveSession failed:', error);
            }
        },

        /**
         * Unarchive (restore) a session (PATCH status → active).
         *
         * The backend's `activate_session_status` automatically archives
         * any other active sessions in the same thread (see storage.rs —
         * `activate_session_status` → `duplicate_session_ids` loop). So a
         * single PATCH to `status: active` is the entire swap operation;
         * no need to pre-archive on the client. Doing the archive on the
         * client first opens a window with zero active sessions, during
         * which other subscriptions / get_or_create paths spawn a fresh
         * session and the unarchive looks like a "new session" creation.
         */
        unarchiveSession: async (sessionId: string) => {
            const scopeToken = nextChatScopeToken();
            try {
                const currentState = get({ subscribe });
                const targetSession = currentState.sessions.find(sess => sess.id === sessionId) ?? null;
                const targetThreadId = targetSession?.ui_thread_id ?? null;
                const wasViewingArchived = currentState.viewingArchivedId === sessionId;
                const identity = await getChatIdentityQuery();
                if (!identity.ok) return;
                const response = await chatFetch(`/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}?${identity.query}`, {
                    method: 'PATCH',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ status: 'active' })
                });
                if (!response.ok) throw new Error(`HTTP ${response.status}`);
                const sessions = await fetchSessionsForThread(targetThreadId);
                if (sessions === null) {
                    throw new Error('Failed to reload sessions after restore');
                }
                if (isStaleChatScopeToken(scopeToken)) {
                    return;
                }
                // The unarchived session becomes the active one — backend
                // archives the previous active in the same thread.
                const nextActiveSessionId = sessionId;
                const activeDetail = await fetchSessionDetail(nextActiveSessionId);
                if (activeDetail === null) {
                    throw new Error('Failed to reload active session after restore');
                }
                if (isStaleChatScopeToken(scopeToken)) {
                    return;
                }
                let mergedSessions = mergeThreadSessionsAuthoritatively(
                    currentState.sessions,
                    sessions,
                    targetThreadId
                );
                mergedSessions = mergeSession(mergedSessions, activeDetail.session);
                update(s => ({
                    ...s,
                    sessions: mergedSessions,
                    activeSessionId: nextActiveSessionId,
                    messages: activeDetail.messages,
                    viewingArchivedId: wasViewingArchived ? null : s.viewingArchivedId
                }));
            } catch (error) {
                if (isStaleChatScopeToken(scopeToken)) {
                    return;
                }
                console.error('[chatStore] unarchiveSession failed:', error);
            }
        },

        /**
         * Permanently delete a session.
         */
        deleteSession: async (sessionId: string) => {
            const scopeToken = nextChatScopeToken();
            try {
                const identity = await getChatIdentityQuery();
                if (!identity.ok) return;
                const response = await chatFetch(`/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}?${identity.query}`, {
                    method: 'DELETE',
                    signal: AbortSignal.timeout(30_000)
                });
                if (!response.ok) throw new Error(`HTTP ${response.status}`);
                if (isStaleChatScopeToken(scopeToken)) {
                    return;
                }
                update(s => ({
                    ...s,
                    sessions: s.sessions.filter(sess => sess.id !== sessionId),
                    activeSessionId: s.activeSessionId === sessionId ? null : s.activeSessionId,
                    messages: s.activeSessionId === sessionId ? [] : s.messages
                }));
            } catch (error) {
                if (isStaleChatScopeToken(scopeToken)) {
                    return;
                }
                console.error('[chatStore] deleteSession failed:', error);
            }
        },

        /**
         * Permanently delete one or more messages from a session.
         */
        deleteMessage: async (sessionId: string, messageIds: string | string[]) => {
            const ids = (Array.isArray(messageIds) ? messageIds : [messageIds])
                .map(id => id.trim())
                .filter(Boolean);
            if (!sessionId || ids.length === 0) return;

            const scopeToken = nextChatScopeToken();
            try {
                const identity = await getChatIdentityQuery();
                if (!identity.ok) return;

                for (const messageId of ids) {
                    const response = await chatFetch(
                        `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/messages/${encodeURIComponent(messageId)}?${identity.query}`,
                        { method: 'DELETE', signal: AbortSignal.timeout(30_000) }
                    );
                    if (!response.ok && response.status !== 404) {
                        throw new Error(`HTTP ${response.status}`);
                    }
                }

                if (isStaleChatScopeToken(scopeToken)) {
                    return;
                }
                const deleted = new Set(ids);
                update(s => {
                    const currentViewId = s.viewingArchivedId ?? s.activeSessionId;
                    if (currentViewId !== sessionId) {
                        return s;
                    }
                    return {
                        ...s,
                        messages: s.messages.filter(message => !deleted.has(message.id))
                    };
                });
            } catch (error) {
                if (isStaleChatScopeToken(scopeToken)) {
                    return;
                }
                console.error('[chatStore] deleteMessage failed:', error);
                throw error;
            }
        },

        clearMessages: async (sessionId: string) => {
            if (!sessionId) return 0;

            const scopeToken = nextChatScopeToken();
            try {
                const identity = await getChatIdentityQuery();
                if (!identity.ok) return 0;

                // 30s hard timeout via AbortSignal.timeout. Without it, a
                // `fetch` waits forever — and if the browser's per-origin
                // HTTP/1.1 connection pool (cap 6) is saturated by long-
                // lived SSE/WS streams, the DELETE never even reaches the
                // backend. Symptom is "Clearing…" spinner that never
                // resolves and an inability to refresh the tab. 30s is
                // generous — the server-side clear is <2ms in steady
                // state; the only way it stretches is when an in-flight
                // chat turn holds the per-session lock, in which case 30s
                // is the right "give up, surface error" boundary.
                const response = await chatFetch(
                    `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/messages?${identity.query}`,
                    { method: 'DELETE', signal: AbortSignal.timeout(30_000) }
                );
                if (!response.ok && response.status !== 404) {
                    throw new Error(`HTTP ${response.status}`);
                }

                let cleared = 0;
                if (response.ok) {
                    try {
                        const body = await response.json();
                        cleared = typeof body.cleared === 'number' ? body.cleared : 0;
                    } catch {
                        cleared = 0;
                    }
                }

                if (isStaleChatScopeToken(scopeToken)) {
                    return cleared;
                }
                update(s => {
                    const currentViewId = s.viewingArchivedId ?? s.activeSessionId;
                    if (currentViewId !== sessionId) {
                        return s;
                    }
                    return { ...s, messages: [] };
                });
                return cleared;
            } catch (error) {
                if (isStaleChatScopeToken(scopeToken)) {
                    return 0;
                }
                if (error instanceof DOMException && error.name === 'TimeoutError') {
                    console.error('[chatStore] clearMessages timed out after 30s');
                    throw new Error(
                        'Clear timed out. The server may be busy or a long-lived stream is blocking the request — refresh the tab and try again.'
                    );
                }
                console.error('[chatStore] clearMessages failed:', error);
                throw error;
            }
        },

        // =================================================================
        // Chat-run control (Phase 1) + pending-message queue (Phase 2/4)
        // =================================================================

        /**
         * DELETE /api/magician/v2/chat/sessions/{id}/run
         * Cancel the in-flight chat turn and its scoped tutor runtime. Drops
         * partial output. Does NOT clear the pending-replay queue — next
         * queued message drains when the cancelled turn settles.
         */
        // Queue admission is independent of the foreground stream and navigation
        // generation. Never replace its placeholder or abort its subscription.
        queueMessage: async (sessionId: string, text: string, profile?: string | null,
            attachmentIds: string[] = [], options: ChatSendOptions = {}): Promise<string> => {
            const identity = await getChatIdentityQuery();
            if (!identity.ok) throw new Error(identity.error);
            options = withComposerHarness(options);
            const response = await chatFetch(`/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/queue?${identity.query}`, {
                method: 'POST', headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ text, attachment_ids: attachmentIds, profile,
                    mode: options.mode, harness_engine: options.harnessEngine, harness_model: options.harnessModel,
                    source_surface: normalizeSourceSurface(options.sourceSurface),
                    chat_turn_id: 'chat-turn-' + crypto.randomUUID(), coding_choice: resolvedCodingChoice(options) })
            });
            const body = await response.json().catch(() => ({}));
            if (!response.ok) throw new Error(body.error ?? 'Could not queue message.');
            update(s => ({ ...s, queueRevision: (s.queueRevision ?? 0) + 1 }));
            return body.queued.id;
        },

        actOnQueuedMessage: async (sessionId: string, messageId: string, action: 'parallel' | 'stop_and_send'): Promise<void> => {
            const identity = await getChatIdentityQuery();
            if (!identity.ok) throw new Error(identity.error);
            const response = await chatFetch(`/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/queue/${encodeURIComponent(messageId)}/action?${identity.query}`, {
                method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ action })
            });
            const body = await response.json().catch(() => ({}));
            update(s => ({ ...s, queueRevision: (s.queueRevision ?? 0) + 1 }));
            if (!response.ok) throw new Error(body.error === 'queue_message_already_started_or_removed'
                ? 'This message has already started or was removed. The queue has been refreshed.'
                : body.error ?? 'Could not update queued message.');
        },

        cancelChatRun: async (sessionId: string): Promise<boolean> => {
            if (!sessionId) return false;
            const identity = await getChatIdentityQuery();
            if (!identity.ok) return false;
            try {
                const response = await chatFetch(
                    `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/run?${identity.query}`,
                    { method: 'DELETE', signal: AbortSignal.timeout(10_000) }
                );
                if (!response.ok) return false;
                const body = await response.json().catch(() => ({}));
                return Boolean(body.cancelled || body.tutor?.cancelled);
            } catch (error) {
                console.error('[chatStore] cancelChatRun failed:', error);
                return false;
            }
        },

		/**
		 * POST /api/magician/v2/chat/sessions/{id}/actions/invoke
		 * Invoke a structured-response server action referenced by an opaque
		 * action_ref token.
		 */
		invokeStructuredResponseAction: async (sessionId: string, actionRef: string): Promise<void> => {
			const trimmedActionRef = actionRef.trim();
			if (!sessionId) {
				throw new Error('Cannot invoke server action without a chat session id.');
			}
			if (!trimmedActionRef) {
				throw new Error('Action reference is required.');
			}

			const identity = await getChatIdentityQuery();
			if (!identity.ok) {
				throw new Error(identity.error ?? 'Unable to resolve chat identity.');
			}

			const response = await chatFetch(
				`/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/actions/invoke?${identity.query}`,
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({ action_ref: trimmedActionRef })
				}
			);

			if (!response.ok) {
				const body = await response.json().catch(() => ({} as Record<string, unknown>));
				const details =
					(body.error ?? body.message) ?? `Failed to invoke server action (HTTP ${response.status})`;
				throw new Error(typeof details === 'string' ? details : `Failed to invoke server action.`);
			}
		},

		/**
		 * GET /api/magician/v2/chat/sessions/{id}/queue
		 * List pending-replay messages for the session in FIFO order.
		 * Drives the queue inspector pill + list.
		 */
		listQueuedMessages: async (sessionId: string): Promise<QueuedMessage[]> => {
            if (!sessionId) return [];
            const identity = await getChatIdentityQuery();
            if (!identity.ok) return [];
            try {
                const response = await chatFetch(
                    `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/queue?${identity.query}`
                );
                if (!response.ok) return [];
                const body = await response.json().catch(() => ({}));
                update(s => currentVisibleSessionId(s) === sessionId
                    ? { ...s, activeRunSessionId: body.active ? sessionId : null } : s);
                return Array.isArray(body.queued) ? (body.queued as QueuedMessage[]) : [];
            } catch (error) {
                console.error('[chatStore] listQueuedMessages failed:', error);
                return [];
            }
        },

        /**
         * DELETE /api/magician/v2/chat/sessions/{id}/queue/{messageId}
         * Remove one queued message by id. Idempotent.
         */
        deleteQueuedMessage: async (
            sessionId: string,
            messageId: string
        ): Promise<boolean> => {
            if (!sessionId || !messageId) return false;
            const identity = await getChatIdentityQuery();
            if (!identity.ok) return false;
            try {
                const response = await chatFetch(
                    `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/queue/${encodeURIComponent(messageId)}?${identity.query}`,
                    { method: 'DELETE', signal: AbortSignal.timeout(10_000) }
                );
                if (!response.ok) return false;
                const body = await response.json().catch(() => ({}));
                return Boolean(body.deleted);
            } catch (error) {
                console.error('[chatStore] deleteQueuedMessage failed:', error);
                return false;
            }
        },

        /**
         * DELETE /api/magician/v2/chat/sessions/{id}/queue
         * Clear the entire pending-replay queue. Returns the count of
         * messages dropped.
         */
        clearQueuedMessages: async (sessionId: string): Promise<number> => {
            if (!sessionId) return 0;
            const identity = await getChatIdentityQuery();
            if (!identity.ok) return 0;
            try {
                const response = await chatFetch(
                    `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/queue?${identity.query}`,
                    { method: 'DELETE', signal: AbortSignal.timeout(10_000) }
                );
                if (!response.ok) return 0;
                const body = await response.json().catch(() => ({}));
                return typeof body.cleared === 'number' ? body.cleared : 0;
            } catch (error) {
                console.error('[chatStore] clearQueuedMessages failed:', error);
                return 0;
            }
        },

        // =================================================================
        // Bubble State
        // =================================================================

        toggleBubble: () => {
            update(s => {
                const next = !s.bubbleOpen;
                saveBubbleState(next);
                return { ...s, bubbleOpen: next };
            });
        },

        openBubble: () => {
            update(s => {
                saveBubbleState(true);
                return { ...s, bubbleOpen: true };
            });
        },

        closeBubble: () => {
            update(s => {
                saveBubbleState(false);
                return { ...s, bubbleOpen: false };
            });
        },

        // =================================================================
        // WebSocket: handle incoming ChatMessageReceived event
        // =================================================================

        handleChatMessageReceived: (sessionId: string, rawMessage: Record<string, unknown>) => {
            const message = convertMessage(rawMessage);
            // Set when this WS message is a genuinely-new append (not a
            // duplicate, not buffered) so we can run live-only side effects
            // (off-call task-completion TTS) after the store update commits.
            let appendedLive = false;
            update(s => {
                // Only update if we are viewing this session
                const currentViewId = s.viewingArchivedId ?? s.activeSessionId;
                if (currentViewId !== sessionId) return s;

                // While an SSE stream is in-flight, skip WS messages for this
                // session until the matching authoritative HTTP/SSE response is
                // applied. The server-owned `messages` batch defines the
                // correct same-turn ordering; buffer WS events and replay them
                // after that batch lands.
                if (isAuthoritativeBatchActive(sessionId)) {
                    bufferRealtimeMessage(sessionId, message);
                    return s;
                }

                const reconciledMessages = stripMatchingOptimisticUserMessage(s.messages, message);

                // Avoid duplicates
                if (reconciledMessages.some(m => m.id === message.id)) {
                    const refreshed = appendUniqueMessagesInOrder(reconciledMessages, [message]);
                    return refreshed === s.messages
                        ? s
                        : applySendingState({ ...s, messages: refreshed });
                }

                appendedLive = true;
                return applySendingState({
                    ...s,
                    messages: normalizeMessages([...reconciledMessages, message]),
                });
            });
            // Off-call TTS read-out: a completed task card with a `speech_tts`
            // line, arriving live (never on history replay — that doesn't pass
            // through here), is read aloud when no call is running + auto-speak
            // is on. No-ops otherwise.
            if (appendedLive) {
                maybeSpeakTaskCompletion(message);
            }
        },

        // =================================================================
        // Escalation (Steps 5-7)
        // =================================================================

        /**
         * Mark a specific escalation message as resolved (disable buttons locally).
         * Called after a successful escalation response, or when a matching
         * resolved event arrives for the same execution, request, or pause id.
         */
        markEscalationResolved: (
            executionId: string,
            requestId?: string,
            pauseStateId?: string,
            inactiveReason?: string
        ) => {
            const reason = inactiveReason?.trim();
            update(s => ({
                ...s,
                messages: s.messages.map(m =>
                    escalationMatchesTarget(m.content, executionId, requestId, pauseStateId) && !m.content.resolved
                        ? { ...m, content: { ...m.content, resolved: true, inactive_reason: reason || m.content.inactive_reason } }
                        : m
                )
            }));
        },

        reconcilePendingUserRequests: (pendingRequestIds: string[]) => {
            const pending = new Set(pendingRequestIds.map(normalizedMessageId).filter(Boolean));
            update(s => ({
                ...s,
                messages: s.messages.map(m => {
                    if (m.content.type !== 'escalation') return m;
                    const requestId = normalizedMessageId(m.content.request_id);
                    if (!requestId || m.content.resolved) return m;
                    const shouldBeStale = !pending.has(requestId);
                    if (m.content.stale === shouldBeStale) return m;
                    return { ...m, content: { ...m.content, stale: shouldBeStale } };
                })
            }));
        },

        /**
         * Handle incoming EscalationResolved WS event.
         * Marks the matching unresolved escalation message as resolved.
         */
        handleEscalationResolved: (
            executionId: string,
            requestId?: string,
            pauseStateId?: string,
            inactiveReason?: string
        ) => {
            const reason = inactiveReason?.trim();
            update(s => ({
                ...s,
                messages: s.messages.map(m =>
                    escalationMatchesTarget(m.content, executionId, requestId, pauseStateId) && !m.content.resolved
                        ? { ...m, content: { ...m.content, resolved: true, inactive_reason: reason || m.content.inactive_reason } }
                        : m
                )
            }));
        }
    };
}

// =============================================================================
// Helpers
// =============================================================================

function mergeSession(sessions: ChatSession[], session: ChatSession): ChatSession[] {
    const exists = sessions.some(s => s.id === session.id);
    if (exists) {
        return sessions.map(s => s.id === session.id ? session : s);
    }
    return [session, ...sessions];
}

export function mergeThreadSessionsAuthoritatively(
    existingSessions: ChatSession[],
    fetchedSessions: ChatSession[],
    uiThreadId: string | null | undefined
): ChatSession[] {
    if (!uiThreadId) {
        return fetchedSessions.map(session => {
            const local = existingSessions.find(existing => existing.id === session.id);
            return local ? { ...local, ...session } : session;
        });
    }

    const otherThreadSessions = existingSessions.filter(session => session.ui_thread_id !== uiThreadId);
    const mergedThreadSessions = fetchedSessions.map(session => {
        const local = existingSessions.find(existing => existing.id === session.id);
        return local ? { ...local, ...session } : session;
    });
    return [...mergedThreadSessions, ...otherThreadSessions]
        .sort((left, right) => right.updated_at - left.updated_at);
}

// =============================================================================
// Export Singleton
// =============================================================================

export const chatStore = createChatStore();

// Derived stores for convenience
export const activeSessionId = derived({ subscribe: chatStore.subscribe }, s => s.activeSessionId);
export const chatMessages = derived({ subscribe: chatStore.subscribe }, s => s.messages);
export const chatSessions = derived({ subscribe: chatStore.subscribe }, s => s.sessions);
export const chatIsLoading = derived({ subscribe: chatStore.subscribe }, s => s.isLoading);
export const chatIsSendingMessage = derived({ subscribe: chatStore.subscribe }, s => s.isSendingMessage);
export const chatBubbleOpen = derived({ subscribe: chatStore.subscribe }, s => s.bubbleOpen);
export const isViewingArchived = derived({ subscribe: chatStore.subscribe }, s => s.viewingArchivedId !== null);
export const currentViewSessionId = derived(
    { subscribe: chatStore.subscribe },
    s => s.viewingArchivedId ?? s.activeSessionId
);
