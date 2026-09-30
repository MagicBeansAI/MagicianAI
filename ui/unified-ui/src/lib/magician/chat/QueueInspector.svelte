<!--
  QueueInspector — pending-replay queue surface for a chat session.

  Renders a compact "N queued" strip above the composer's session chooser.
  Expanding it exposes waiting messages with Stop & send, Run in parallel,
  Copy and Remove actions; the panel header carries Clear all.

  Queue source-of-truth is the magician backend
  (`/api/magician/v2/chat/sessions/{id}/queue`). The component polls
  every 2s while mounted and re-fetches on demand after every mutation.
  Polling is cheap (one GET returning an empty array in the common case)
  and is the simplest way to stay live without subscribing to a new
  realtime event variant. We can swap to event-driven later by listening
  for `ChatMessageQueued` once that ships in the realtime taxonomy.

  Cancellation (`Stop turn`) lives here too — it calls the chat-run
  cancel endpoint that drops partial output without clearing the queue
  (per backend decision #2). The next queued message drains after the
  cancelled turn settles.
-->
<script lang="ts">
    import { onMount, onDestroy } from 'svelte';
    import { fade, slide } from 'svelte/transition';
    import { chatStore, type QueuedMessage } from '$lib/stores/chatStore';
    import { showError, showInfo, showSuccess } from '$lib/shared/stores/notifications';

    export let sessionId: string;
    /** Optional — when true, the cancel control is rendered alongside
     *  the queue pill. Default true; pass `false` from surfaces that
     *  already have their own stop button (e.g. ChatBubble). */
    export let showCancelControl: boolean = true;
    /** Poll interval in ms. Set to 0 to disable polling (manual
     *  refresh only via `refresh()`). */
    export let pollIntervalMs: number = 2000;

    let queued: QueuedMessage[] = [];
    let expanded: boolean = false;
    let pollHandle: ReturnType<typeof setInterval> | null = null;
    let isMutating: boolean = false;

    async function refresh(): Promise<void> {
        if (!sessionId) {
            queued = [];
            return;
        }
        const requestedSession = sessionId;
        try {
            const result = await chatStore.listQueuedMessages(requestedSession);
            if (sessionId !== requestedSession) return;
            queued = result;
            if (queued.length === 0) {
                expanded = false;
            }
        } catch (err) {
            console.warn('[QueueInspector] refresh failed:', err);
        }
    }

    onMount(() => {
        void refresh();
        if (pollIntervalMs > 0) {
            pollHandle = setInterval(() => {
                void refresh();
            }, pollIntervalMs);
        }
    });

    onDestroy(() => {
        if (pollHandle) {
            clearInterval(pollHandle);
            pollHandle = null;
        }
    });

    // Depend on the primitive revision, not the whole store. A queue read also
    // publishes run liveness; subscribing directly to that write would refetch
    // recursively and saturate the browser's local connections.
    $: queueRevision = $chatStore.queueRevision ?? 0;
    $: if (sessionId && queueRevision >= 0) void refresh();

    async function copyMessage(msg: QueuedMessage): Promise<void> {
        const text = msg.text ?? '';
        // `navigator.clipboard.writeText` requires a secure context
        // (HTTPS or localhost). When that fails, fall back to a
        // throw-away textarea + document.execCommand so the action
        // still works on plain-HTTP intranet deploys.
        try {
            if (navigator.clipboard?.writeText) {
                await navigator.clipboard.writeText(text);
                showInfo('Copied queued message to clipboard');
                return;
            }
        } catch {
            // fall through to the legacy path
        }
        try {
            const ta = document.createElement('textarea');
            ta.value = text;
            ta.setAttribute('readonly', '');
            ta.style.position = 'fixed';
            ta.style.opacity = '0';
            document.body.appendChild(ta);
            ta.select();
            const ok = document.execCommand('copy');
            document.body.removeChild(ta);
            if (ok) {
                showInfo('Copied queued message to clipboard');
            } else {
                showError('Failed to copy — clipboard access denied');
            }
        } catch {
            showError('Failed to copy — clipboard access denied');
        }
    }

    async function deleteMessage(msg: QueuedMessage): Promise<void> {
        if (isMutating) return;
        isMutating = true;
        try {
            const ok = await chatStore.deleteQueuedMessage(sessionId, msg.id);
            if (ok) {
                queued = queued.filter((m) => m.id !== msg.id);
                if (queued.length === 0) expanded = false;
            } else {
                // Either already drained or unknown id; resync to be safe.
                await refresh();
            }
        } finally {
            isMutating = false;
        }
    }

    async function clearAll(): Promise<void> {
        if (isMutating || queued.length === 0) return;
        isMutating = true;
        try {
            const cleared = await chatStore.clearQueuedMessages(sessionId);
            queued = [];
            expanded = false;
            if (cleared > 0) {
                showSuccess(
                    `Cleared ${cleared} queued message${cleared === 1 ? '' : 's'}`
                );
            }
        } finally {
            isMutating = false;
        }
    }

    async function cancelRun(): Promise<void> {
        if (isMutating) return;
        isMutating = true;
        try {
            const ok = await chatStore.cancelChatRun(sessionId);
            if (ok) {
                showInfo('Stopped current turn. Queue preserved.');
            } else {
                showInfo('Nothing to stop — no turn is currently in flight.');
            }
            // The next queued message will drain on the backend's
            // cancellation cleanup; refresh shortly to pick up the new
            // state.
            setTimeout(() => void refresh(), 300);
        } finally {
            isMutating = false;
        }
    }

    async function actOnMessage(msg: QueuedMessage, action: 'parallel' | 'stop_and_send'): Promise<void> {
        if (isMutating) return;
        isMutating = true;
        try {
            await chatStore.actOnQueuedMessage(sessionId, msg.id, action);
            showInfo(action === 'parallel' ? 'Running in parallel. Current work continues.' : 'Stopping the current reply. This message will run next.');
        } catch (error) {
            showError('Could not update queued message', error instanceof Error ? error.message : String(error));
        } finally { isMutating = false; await refresh(); }
    }

    function formatRelative(ms: number): string {
        const now = Date.now();
        const diff = Math.max(0, now - ms);
        if (diff < 1000) return 'just now';
        const seconds = Math.floor(diff / 1000);
        if (seconds < 60) return `${seconds}s ago`;
        const minutes = Math.floor(seconds / 60);
        if (minutes < 60) return `${minutes}m ago`;
        const hours = Math.floor(minutes / 60);
        return `${hours}h ago`;
    }

    function preview(text: string | undefined, max = 120): string {
        if (!text) return '(attachments only)';
        return text.length <= max ? text : `${text.slice(0, max - 1)}…`;
    }
</script>

{#if queued.length > 0 || (showCancelControl && isMutating)}
    <div class="queue-inspector" transition:fade={{ duration: 140 }}>
        <div class="queue-row">
            <button
                type="button"
                class="queue-pill"
                class:expanded
                disabled={queued.length === 0}
                aria-expanded={expanded}
                aria-label={expanded ? 'Collapse queued messages' : 'Expand queued messages'}
                on:click={() => (expanded = !expanded)}
            >
                <span class="queue-pill-dot" aria-hidden="true"></span>
                <span class="queue-pill-count">{queued.length}</span>
                <span class="queue-pill-label">
                    queued message{queued.length === 1 ? '' : 's'}
                </span>
                {#if queued.length > 0}
                    <span class="queue-preview">· {preview(queued[0]?.text)}</span>
                    <svg class="queue-pill-caret" width="14" height="14" viewBox="0 0 16 16" aria-hidden="true">
                        <path d={expanded ? 'M4 6l4 4 4-4' : 'M4 10l4-4 4 4'} fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
                    </svg>
                {/if}
            </button>

            {#if showCancelControl && expanded}
                <button
                    type="button"
                    class="queue-stop-button"
                    on:click={cancelRun}
                    disabled={isMutating}
                    title="Cancel the in-flight turn (queue is preserved)"
                >
                    Stop turn
                </button>
            {/if}
        </div>

        {#if expanded && queued.length > 0}
            <div class="queue-panel" transition:slide={{ duration: 160 }}>
                <div class="queue-panel-head">
                    <span class="queue-panel-title">
                        Waiting for current reply ({queued.length})
                    </span>
                    <button
                        type="button"
                        class="queue-panel-clear"
                        on:click={clearAll}
                        disabled={isMutating}
                    >
                        Clear all
                    </button>
                </div>

                <ul class="queue-list" role="list">
                    {#each queued as msg, idx (msg.id)}
                        <li class="queue-item">
                            <div class="queue-item-meta">
                                <span class="queue-item-index">#{idx + 1}</span>
                                <span class="queue-item-time">
                                    {formatRelative(msg.queued_at)}
                                </span>
                            </div>
                            <div class="queue-item-text" title={msg.text ?? ''}>
                                {preview(msg.text)}
                            </div>
                            <div class="queue-item-actions">
                                <span class="queue-item-time">Waiting</span>
                                <button type="button" class="queue-item-button" disabled={isMutating} on:click={() => actOnMessage(msg, 'stop_and_send')}>Stop &amp; send</button>
                                {#if msg.text?.trim() && !msg.attachment_ids?.length}
                                    <button type="button" class="queue-item-button" disabled={isMutating} on:click={() => actOnMessage(msg, 'parallel')}>Run in parallel</button>
                                {/if}
                                <button
                                    type="button"
                                    class="queue-item-button"
                                    on:click={() => copyMessage(msg)}
                                    title="Copy this queued message"
                                >
                                    Copy
                                </button>
                                <button
                                    type="button"
                                    class="queue-item-button queue-item-button--danger"
                                    on:click={() => deleteMessage(msg)}
                                    disabled={isMutating}
                                    title="Delete this queued message"
                                >
                                    Remove
                                </button>
                            </div>
                        </li>
                    {/each}
                </ul>

                <div class="queue-panel-footer">
                    <span>
                        Runs in order after the current reply finishes.
                    </span>
                </div>
            </div>
        {/if}
    </div>
{/if}

<style>
    .queue-inspector {
        margin: 0;
        min-width: 0;
        border-bottom: 1px solid var(--border-soft);
        border-radius: calc(var(--radius-lg, 14px) - 1px) calc(var(--radius-lg, 14px) - 1px) 0 0;
        font-family: var(--font-primary, system-ui, sans-serif);
        font-size: 13px;
        color: var(--text-primary, #1c1b17);
    }

    .queue-row {
        display: flex;
        align-items: center;
        gap: 0;
        border-radius: inherit;
    }

    .queue-pill {
        display: inline-flex;
        align-items: center;
        gap: 8px;
        padding: 0 10px;
        height: 30px;
        min-width: 0;
        flex: 1;
        background: transparent;
        color: var(--text-primary);
        border: 0;
        border-radius: inherit;
        font-size: 12px;
        font-weight: 500;
        cursor: pointer;
        transition: background var(--transition-fast, 0.12s ease);
    }

    .queue-pill:hover:not(:disabled) {
        background: color-mix(in srgb, var(--accent-primary) 18%, transparent);
    }

    .queue-pill:disabled {
        cursor: default;
        opacity: 0.6;
    }

    .queue-pill-dot {
        width: 7px;
        height: 7px;
        border-radius: 50%;
        background: var(--accent-primary);
        flex-shrink: 0;
    }

    .queue-pill-count {
        font-variant-numeric: tabular-nums;
        font-weight: 600;
    }

    .queue-pill-label {
        opacity: 0.85;
    }

    .queue-preview { flex: 1; min-width: 0; text-align: left; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; color: var(--text-muted); }
    .queue-pill-label { white-space: nowrap; }
    .queue-pill-caret { flex-shrink: 0; margin-left: auto; }


    .queue-stop-button {
        display: inline-flex;
        align-items: center;
        padding: 5px 12px;
        background: transparent;
        color: var(--text-secondary, #5f6668);
        border: 1px solid var(--border-soft, #eee4dc);
        border-radius: 999px;
        font-size: 12px;
        font-weight: 500;
        cursor: pointer;
        transition:
            background var(--transition-fast, 0.12s ease),
            color var(--transition-fast, 0.12s ease),
            border-color var(--transition-fast, 0.12s ease);
    }

    .queue-stop-button:hover:not(:disabled) {
        background: var(--color-warning-soft, rgba(255, 230, 109, 0.22));
        border-color: var(--color-warning, #ffe66d);
        color: var(--text-primary, #1c1b17);
    }

    .queue-stop-button:disabled {
        opacity: 0.5;
        cursor: default;
    }

    .queue-panel {
        margin-top: 0;
        background: var(--bg-card);
        border: 1px solid var(--border-soft, #eee4dc);
        border-radius: 0;
        overflow: hidden;
    }

    .queue-panel-head {
        display: flex;
        align-items: center;
        justify-content: space-between;
        padding: 10px 14px;
        border-bottom: 1px solid var(--border-soft, #eee4dc);
        background: var(--bg-soft, #f6f1e8);
    }

    .queue-panel-title {
        font-family: var(--font-mono, ui-monospace, monospace);
        font-size: 11px;
        letter-spacing: 0.12em;
        text-transform: uppercase;
        color: var(--text-secondary, #5f6668);
    }

    .queue-panel-clear {
        font-size: 12px;
        background: transparent;
        color: var(--accent-primary);
        border: 1px solid transparent;
        padding: 3px 10px;
        border-radius: var(--radius-sm, 6px);
        cursor: pointer;
        transition:
            background var(--transition-fast, 0.12s ease),
            border-color var(--transition-fast, 0.12s ease);
    }

    .queue-panel-clear:hover:not(:disabled) {
        background: var(--accent-primary-soft);
        border-color: var(--accent-primary-soft);
    }

    .queue-panel-clear:disabled {
        opacity: 0.5;
        cursor: default;
    }

    .queue-list {
        list-style: none;
        margin: 0;
        padding: 0;
        max-height: 260px;
        overflow-y: auto;
    }

    .queue-item {
        display: grid;
        grid-template-columns: minmax(60px, auto) 1fr auto;
        gap: 10px;
        align-items: center;
        padding: 10px 14px;
        border-bottom: 1px solid var(--border-soft, #eee4dc);
    }

    .queue-item:last-child {
        border-bottom: none;
    }

    .queue-item-meta {
        display: flex;
        flex-direction: column;
        align-items: flex-start;
        font-family: var(--font-mono, ui-monospace, monospace);
        font-size: 10px;
        color: var(--text-secondary, #5f6668);
        line-height: 1.3;
    }

    .queue-item-index {
        color: var(--accent-primary);
        font-weight: 600;
    }

    .queue-item-time {
        opacity: 0.75;
    }

    .queue-item-text {
        font-size: 13px;
        color: var(--text-primary, #1c1b17);
        line-height: 1.45;
        word-break: break-word;
        min-width: 0;
    }

    .queue-item-actions {
        flex-wrap: wrap;
        display: inline-flex;
        gap: 4px;
        flex-shrink: 0;
    }

    .queue-item-button {
        font-size: 11px;
        font-weight: 500;
        background: transparent;
        color: var(--text-secondary, #5f6668);
        border: 1px solid var(--border-soft, #eee4dc);
        padding: 3px 8px;
        border-radius: var(--radius-sm, 6px);
        cursor: pointer;
        transition:
            background var(--transition-fast, 0.12s ease),
            border-color var(--transition-fast, 0.12s ease),
            color var(--transition-fast, 0.12s ease);
    }

    .queue-item-button:hover:not(:disabled) {
        background: var(--bg-soft, #f6f1e8);
        color: var(--text-primary, #1c1b17);
    }

    .queue-item-button--danger:hover:not(:disabled) {
        background: var(--accent-primary-soft);
        color: var(--accent-primary);
        border-color: var(--accent-primary-soft);
    }

    .queue-item-button:disabled {
        opacity: 0.5;
        cursor: default;
    }

    .queue-panel-footer {
        padding: 8px 14px;
        font-size: 11px;
        color: var(--text-secondary, #5f6668);
        font-style: italic;
        background: var(--bg-soft, #f6f1e8);
        border-top: 1px solid var(--border-soft, #eee4dc);
    }
</style>
