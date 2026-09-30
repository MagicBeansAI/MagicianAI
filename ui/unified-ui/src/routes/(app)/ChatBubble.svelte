<script lang="ts">
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { onMount, tick } from 'svelte';
	import { fade, fly } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import {
		collapseTaskProgressMessages,
		chatStore,
		getEscalationResolvedSummary,
		getMessageContentBlocks,
		getMessageText,
		normalizeMessages,
		type ChatRenderTaskExecutionGroup
	} from '$lib/stores/chatStore';
	import ChatContentBlocks from '$lib/magician/components/chat/ChatContentBlocks.svelte';
	import ChatMarkdown from '$lib/magician/components/chat/ChatMarkdown.svelte';

	let textareaEl: HTMLTextAreaElement;
	let messagesContainerEl: HTMLDivElement;
	let messagesEndEl: HTMLDivElement;
	let inputText = '';
	let initialized = false;
	let lastBubbleOpen = false;

	// Hide the floating bubble on phone-sized viewports. /mobile/chat is
	// the canonical chat surface there, and the bubble's centred modal
	// overlay doesn't translate cleanly to a 360px-wide screen. Tracked
	// reactively so a viewport resize (eg orientation flip, browser
	// devtools) re-evaluates on the fly instead of latching at mount.
	let isMobileViewport = false;

	$: bubbleOpen = $chatStore.bubbleOpen;
	$: activeSessionId = $chatStore.activeSessionId;
	$: messages = $chatStore.messages;
	$: displayMessages = collapseTaskProgressMessages(normalizeMessages(messages));
	$: isLoading = $chatStore.isLoading;
	$: isSendingMessage = $chatStore.isSendingMessage;
	$: isReadOnly = $chatStore.viewingArchivedId !== null;

	// Show only last 10 messages in bubble overlay
	$: recentMessages = displayMessages.slice(-10);

	// Don't show bubble on the full chat page
	$: isOnChatPage = $page.url.pathname === '/chat' || $page.url.pathname.startsWith('/chat/');

	// Thread-aware load: when the bubble opens on `/t/<name>`, fetch THAT
	// thread's active session, not the default `general`. Default
	// `loadActiveSession()` is `'general'`, which on a thread page would
	// clobber the page's loaded session and show the wrong conversation.
	function currentThreadFromPath(pathname: string): string {
		if (pathname.startsWith('/t/')) {
			const slug = pathname.slice(3).split('/')[0];
			const decoded = decodeURIComponent(slug ?? '').trim();
			if (decoded) return decoded;
		}
		return 'general';
	}
	$: currentThreadId = currentThreadFromPath($page.url.pathname);

	// Scroll to bottom when messages change in bubble
	$: if (bubbleOpen && recentMessages.length && browser) {
		tick().then(scrollToBottom);
	}

	// Focus the textarea + reload session whenever the bubble transitions
	// from closed → open, regardless of trigger source (click, ⌘J shortcut,
	// `g j` leader chord, or CommandPalette "Quick chat" entry).
	$: {
		if (browser && bubbleOpen && !lastBubbleOpen) {
			initialized = true;
			chatStore.loadActiveSession(currentThreadId);
			tick().then(() => {
				if (textareaEl) textareaEl.focus();
				scrollToBottom();
			});
		}
		lastBubbleOpen = bubbleOpen;
	}

	function scrollToBottom() {
		if (messagesContainerEl) {
			messagesContainerEl.scrollTo({
				top: messagesContainerEl.scrollHeight,
				behavior: 'smooth'
			});
		} else if (messagesEndEl) {
			messagesEndEl.scrollIntoView({ behavior: 'smooth' });
		}
	}

	function handleToggle() {
		// Reactive `bubbleOpen` watcher above handles focus + session load
		// when the overlay opens — regardless of trigger source. Click,
		// ⌘J shortcut, leader `g j`, and CommandPalette all funnel through
		// the same single source of truth.
		chatStore.toggleBubble();
	}

	function handleClose() {
		chatStore.closeBubble();
	}

	function handleOpenFull() {
		chatStore.closeBubble();
		// Preserve the thread context — clicking "Open full chat" while the
		// bubble is showing the marketing thread should take you to
		// /t/marketing, not the default /chat (general).
		const target =
			currentThreadId && currentThreadId !== 'general'
				? `/t/${encodeURIComponent(currentThreadId)}`
				: '/chat';
		goto(target);
	}

	function handleKeydown(event: KeyboardEvent) {
		if (event.key === 'Enter' && !event.shiftKey) {
			event.preventDefault();
			handleSend();
		}
		if (event.key === 'Escape') {
			handleClose();
		}
	}

	async function handleSend() {
		if (!inputText.trim() || isSendingMessage || isReadOnly || !activeSessionId) return;
		const text = inputText;
		inputText = '';
		await chatStore.sendMessage(activeSessionId, text);
	}

	function formatTime(ts: number): string {
		return new Date(ts).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
	}

	function taskProgressUpdateLabel(count: number): string {
		return count <= 1 ? '' : `Progress · ${count} updates`;
	}

	function taskExecutionLabel(
		group: ChatRenderTaskExecutionGroup,
		index: number,
		total: number
	): string {
		const executionId = group.message.content.execution_id?.trim();
		if (!executionId) {
			return total > 1 ? 'Primary run' : 'Run';
		}
		return total > 1 ? `Run ${index + 1}` : 'Run';
	}

	onMount(() => {
		if (browser && bubbleOpen) {
			initialized = true;
			chatStore.loadActiveSession(currentThreadId);
		}

		if (!browser) return;
		const mql = window.matchMedia('(max-width: 767px)');
		isMobileViewport = mql.matches;
		const onChange = (event: MediaQueryListEvent): void => {
			isMobileViewport = event.matches;
		};
		mql.addEventListener('change', onChange);
		return () => mql.removeEventListener('change', onChange);
	});

	// If the user navigates between pages while the bubble is open, refetch
	// for the new thread so the panel keeps tracking the page they're on.
	let lastLoadedThread = currentThreadId;
	$: if (browser && bubbleOpen && currentThreadId !== lastLoadedThread) {
		lastLoadedThread = currentThreadId;
		chatStore.loadActiveSession(currentThreadId);
	}
</script>

{#if !isOnChatPage && !isMobileViewport}
	<!-- Floating Bubble Button -->
	<button
		class="chat-bubble-btn btn btn-circle btn-primary"
		on:click={handleToggle}
		aria-label={bubbleOpen ? 'Close chat' : 'Open chat'}
	>
		{#if bubbleOpen}
			<svg xmlns="http://www.w3.org/2000/svg" width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>
		{:else}
			<svg xmlns="http://www.w3.org/2000/svg" width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/></svg>
		{/if}
	</button>

	<!-- Centered modal overlay with glass-blur backdrop. Backdrop click,
	     Escape key, and the close button all dismiss. Transitions match
	     the CommandPalette (fade backdrop, fly+scale panel). -->
	{#if bubbleOpen}
		<!-- svelte-ignore a11y_click_events_have_key_events -->
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div
			class="chat-bubble-shade"
			on:click={(e) => { if (e.target === e.currentTarget) chatStore.closeBubble(); }}
			transition:fade={{ duration: 140 }}
		>
		<div class="chat-bubble-overlay"
			 role="dialog"
			 aria-label="Chat"
			 aria-modal="true"
			 tabindex="-1"
			 transition:fly={{ y: 14, duration: 200, easing: cubicOut }}
			 on:keydown={(e) => { if (e.key === 'Escape') chatStore.closeBubble(); }}>
			<!-- Header -->
			<div class="chat-bubble-header">
				<button class="chat-bubble-open-full" on:click={handleOpenFull}>
					<svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="15 3 21 3 21 9"/><polyline points="9 21 3 21 3 15"/><line x1="21" y1="3" x2="14" y2="10"/><line x1="3" y1="21" x2="10" y2="14"/></svg>
					Open full chat
				</button>
				<button class="chat-bubble-close-btn" on:click={handleClose} aria-label="Close chat">
					<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>
				</button>
			</div>

			<!-- Messages -->
			<div class="chat-bubble-messages" aria-live="polite" bind:this={messagesContainerEl}>
				{#if recentMessages.length === 0 && !isLoading}
					<div class="chat-bubble-empty">
						<p>Start chatting with your agent</p>
					</div>
				{:else}
					{#each recentMessages as visibleMessage (visibleMessage.id)}
						{@const message = visibleMessage.message}
						{#if message.content.type === 'tool_call_executed'}
							<!-- Compact executed card in bubble -->
							<div class="chat-bubble-action-msg chat-bubble-action-done">
								<span class="chat-bubble-action-text">
									Action completed: {message.content.tool_name}
								</span>
								{#if message.content.summary}
									<div class="chat-bubble-action-summary">
										<ChatMarkdown content={message.content.summary} />
									</div>
								{/if}
							</div>
						{:else if message.content.type === 'rich_tool_result'}
							<div class="chat-bubble-action-msg chat-bubble-action-done chat-bubble-action-rich">
								<span class="chat-bubble-action-text">
									Action completed: {message.content.tool_name}
								</span>
								{#if message.content.summary}
									<div class="chat-bubble-action-summary">
										<ChatMarkdown content={message.content.summary} />
									</div>
								{/if}
								<ChatContentBlocks
									sessionId={message.session_id}
									blocks={getMessageContentBlocks(message.content)}
								/>
							</div>
						{:else if message.content.type === 'task_status_update'}
							<!-- Compact status update in bubble -->
							<div class="chat-bubble-action-msg chat-bubble-action-status">
								<span class="chat-bubble-action-text">
									{message.content.display_label ?? `Task ${message.content.task_id}`}: {message.content.status}
								</span>
								{#if visibleMessage.taskExecutionGroups.length > 1}
									<span class="chat-bubble-action-meta">
										{visibleMessage.taskExecutionGroups.length} runs
									</span>
								{:else if visibleMessage.taskProgressUpdates.length > 1}
									<span class="chat-bubble-action-meta">
										{taskProgressUpdateLabel(visibleMessage.taskProgressUpdates.length)}
									</span>
								{/if}
								{#if message.content.summary}
									<div class="chat-bubble-action-summary">
										<ChatMarkdown content={message.content.summary} />
									</div>
								{/if}
								<div class="chat-bubble-task-runs">
									{#each visibleMessage.taskExecutionGroups as group, index (group.id)}
										<div class="chat-bubble-task-run">
											<div class="chat-bubble-task-run-header">
												<span class="chat-bubble-task-run-label">
													{taskExecutionLabel(group, index, visibleMessage.taskExecutionGroups.length)}
												</span>
												<span class="chat-bubble-task-run-state">{group.message.content.status}</span>
											</div>
											{#if group.updates.length > 1}
												<span class="chat-bubble-task-run-meta">{group.updates.length} updates</span>
											{/if}
											{#if group.message.content.summary}
												<div class="chat-bubble-action-summary">
													<ChatMarkdown content={group.message.content.summary} />
												</div>
											{/if}
											<ChatContentBlocks
												sessionId={group.message.session_id}
												blocks={getMessageContentBlocks(group.message.content)}
											/>
										</div>
									{/each}
								</div>
							</div>
						{:else if message.content.type === 'escalation'}
							<!-- Compact escalation card in bubble -->
							<div class="chat-bubble-action-msg chat-bubble-action-escalation">
								<div class="chat-bubble-action-badge">{message.content.inactive_reason === 'Task deleted' ? 'Task Deleted' : message.content.resolved ? 'Resolved' : message.content.stale ? 'Inactive' : 'Action Required'}</div>
								<span class="chat-bubble-action-text">
									{message.content.question}
								</span>
								{#if !message.content.resolved && !message.content.stale}
									<span class="chat-bubble-action-hint">Open full chat to respond</span>
								{:else if message.content.inactive_reason}
									<span class="chat-bubble-action-hint">{message.content.inactive_reason}</span>
								{:else if message.content.stale}
									<span class="chat-bubble-action-hint">No longer active</span>
								{/if}
							</div>
						{:else if message.content.type === 'escalation_resolved'}
							<!-- Compact resolution notice in bubble -->
							<div class="chat-bubble-action-msg chat-bubble-action-done">
								<span class="chat-bubble-action-text">
									{getEscalationResolvedSummary(message.content)}
								</span>
								{#if (message.content.output_files?.length ?? 0) > 0}
									<ChatContentBlocks
										sessionId={message.session_id}
										blocks={message.content.output_files ?? []}
									/>
								{/if}
							</div>
						{:else if message.content.type === 'attachment'}
							<div class="chat-bubble-msg from-user">
								<div class="chat-bubble-msg-meta">
									<span class="chat-bubble-msg-sender">You</span>
									<span class="chat-bubble-msg-time">{formatTime(message.created_at)}</span>
								</div>
								<div class="chat-bubble-msg-content chat-bubble-msg-content-rich">
									<ChatContentBlocks
										sessionId={message.session_id}
										blocks={getMessageContentBlocks(message.content)}
									/>
								</div>
							</div>
						{:else if message.direction === 'system'}
							<div class="chat-bubble-system-msg">
								<div class="chat-bubble-system-msg-content">
									<ChatMarkdown content={getMessageText(message.content)} />
								</div>
							</div>
						{:else}
							<div class="chat-bubble-msg" class:from-user={message.direction === 'user'}>
								<div class="chat-bubble-msg-meta">
									<span class="chat-bubble-msg-sender">
										{message.direction === 'user' ? 'You' : 'Assistant'}
									</span>
									<span class="chat-bubble-msg-time">{formatTime(message.created_at)}</span>
								</div>
								<div class="chat-bubble-msg-content" class:user-msg={message.direction === 'user'}>
									{#if message.id.startsWith('streaming-') && !getMessageText(message.content).trim()}
										<span class="chat-typing-dots" aria-label="Assistant is thinking">
											<span></span><span></span><span></span>
										</span>
									{:else}
										<ChatMarkdown content={getMessageText(message.content)} />
									{/if}
								</div>
							</div>
						{/if}
					{/each}

					{#if isSendingMessage && !recentMessages.some((m) => m.message.id.startsWith('streaming-'))}
						<div class="chat-bubble-msg">
							<div class="chat-bubble-msg-meta">
								<span class="chat-bubble-msg-sender">Assistant</span>
							</div>
							<div class="chat-bubble-msg-content">
								<span class="chat-typing-dots" aria-label="Assistant is thinking">
									<span></span><span></span><span></span>
								</span>
							</div>
						</div>
					{/if}
				{/if}
				<div bind:this={messagesEndEl}></div>
			</div>

			<!-- Input -->
			<div class="chat-bubble-input">
				<textarea
					bind:this={textareaEl}
					class="textarea textarea-bordered chat-bubble-textarea"
					placeholder="Type a message..."
					bind:value={inputText}
					on:keydown={handleKeydown}
					disabled={isLoading || isSendingMessage || isReadOnly}
					rows="1"
				></textarea>
				<button
					class="btn btn-primary btn-sm chat-bubble-send"
					on:click={handleSend}
					disabled={isLoading || isSendingMessage || isReadOnly || !inputText.trim()}
					aria-label="Send message"
				>
					<svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="22" y1="2" x2="11" y2="13"/><polygon points="22 2 15 22 11 13 2 9 22 2"/></svg>
				</button>
			</div>
		</div>
		</div>
	{/if}
{/if}

<style>
	/* ===== Floating Button ===== */
	.chat-bubble-btn {
		position: fixed;
		bottom: calc(var(--attention-bar-offset, 0px) + 1.25rem);
		right: 1.25rem;
		z-index: 1000;
		width: 44px;
		height: 44px;
		box-shadow: 0 4px 16px rgba(0, 0, 0, 0.18);
		transition: transform 0.15s, box-shadow 0.15s;
	}

	.chat-bubble-btn:hover {
		transform: scale(1.06);
		box-shadow: 0 6px 24px rgba(0, 0, 0, 0.22);
	}

	/* ===== Backdrop (glass blur) =====
	   Mirrors the CommandPalette `.palette-shade` look so the chat overlay
	   feels like part of the same modal family. Backdrop click closes via
	   the inline target-check handler in markup. */
	.chat-bubble-shade {
		position: fixed;
		inset: 0;
		background: rgba(0, 0, 0, 0.45);
		backdrop-filter: blur(4px);
		-webkit-backdrop-filter: blur(4px);
		z-index: 600;
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 1.5rem;
	}

	/* ===== Overlay Panel — centered modal ===== */
	.chat-bubble-overlay {
		position: relative;
		width: min(420px, calc(100% - 1rem));
		height: min(520px, calc(100vh - 6rem));
		display: flex;
		flex-direction: column;
		background: var(--bg-elevated, var(--bg-card, #ffffff));
		color: var(--text-primary, #1a1a1a);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		border-radius: var(--radius-lg, 16px);
		box-shadow: var(--shadow-lg, 0 20px 50px rgba(0, 0, 0, 0.18));
		overflow: hidden;
	}

	/* ===== Header ===== */
	.chat-bubble-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 0.55rem 0.75rem;
		border-bottom: 1px solid var(--border-soft, #ebe7e0);
		background: var(--bg-soft, #f8f6f2);
	}

	.chat-bubble-open-full {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		font-size: 0.72rem;
		font-weight: 600;
		color: var(--accent-primary, #e85d5d);
		background: none;
		border: none;
		cursor: pointer;
		font-family: inherit;
		padding: 0.15rem 0.35rem;
		border-radius: 6px;
		transition: background 0.15s;
	}

	.chat-bubble-open-full:hover {
		background: rgba(232, 93, 93, 0.08);
	}

	.chat-bubble-close-btn {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 28px;
		height: 28px;
		border: none;
		background: none;
		cursor: pointer;
		color: var(--text-muted, #8a847a);
		border-radius: 6px;
		transition: background 0.15s, color 0.15s;
	}

	.chat-bubble-close-btn:hover {
		background: rgba(0, 0, 0, 0.06);
		color: var(--text-primary, #2d2a26);
	}

	/* ===== Messages ===== */
	.chat-bubble-messages {
		flex: 1;
		overflow-y: auto;
		padding: 0.65rem 0.75rem;
		display: flex;
		flex-direction: column;
		gap: 0.45rem;
	}

	.chat-bubble-empty {
		display: flex;
		align-items: center;
		justify-content: center;
		flex: 1;
	}

	.chat-bubble-empty p {
		font-size: 0.78rem;
		color: var(--text-muted, #8a847a);
		margin: 0;
	}

	.chat-bubble-msg {
		display: flex;
		flex-direction: column;
		gap: 0.12rem;
	}

	.chat-bubble-msg.from-user {
		align-items: flex-end;
	}

	.chat-bubble-msg-meta {
		display: flex;
		align-items: center;
		gap: 0.4rem;
	}

	.chat-bubble-msg-sender {
		font-size: 0.62rem;
		font-weight: 600;
		color: var(--text-muted, #8a847a);
	}

	.chat-bubble-msg-time {
		font-size: 0.58rem;
		color: var(--text-muted, #b0aaa0);
	}

	.chat-bubble-msg-content {
		font-size: 0.78rem;
		line-height: 1.45;
		padding: 0.35rem 0.6rem;
		border-radius: 10px;
		background: var(--bg-soft, #f5f3ef);
		color: var(--text-primary, #2d2a26);
		max-width: 85%;
		min-width: 0;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		word-break: break-word;
	}

	.chat-bubble-msg-content.user-msg {
		background: var(--accent-primary, #e85d5d);
		color: var(--text-on-accent, #ffffff);
	}

	.chat-typing-dots {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 0.22rem;
		min-width: 1.45rem;
		min-height: 0.8rem;
	}

	.chat-typing-dots span {
		width: 5px;
		height: 5px;
		border-radius: 50%;
		background: var(--text-muted, #8a847a);
		animation: chat-bubble-typing-bounce 1.2s infinite;
	}

	.chat-typing-dots span:nth-child(2) {
		animation-delay: 0.15s;
	}

	.chat-typing-dots span:nth-child(3) {
		animation-delay: 0.3s;
	}

	@keyframes chat-bubble-typing-bounce {
		0%, 60%, 100% { opacity: 0.35; transform: translateY(0); }
		30% { opacity: 1; transform: translateY(-2px); }
	}

	.chat-bubble-system-msg {
		display: flex;
		justify-content: center;
		padding: 0.2rem 0;
	}

	.chat-bubble-system-msg-content {
		display: inline-block;
		font-size: 0.65rem;
		line-height: 1.45;
		color: var(--text-muted, #8a847a);
		background: var(--bg-soft, rgba(0, 0, 0, 0.03));
		padding: 0.3rem 0.55rem;
		border-radius: 0.75rem;
		border: 1px solid color-mix(in srgb, var(--text-primary, #111827) 8%, transparent);
		max-width: min(100%, 90%);
		min-width: 0;
		text-align: left;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		word-break: break-word;
	}

	/* ===== Action Messages (tool proposals, executed, status updates) ===== */
	.chat-bubble-action-msg {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
		padding: 0.4rem 0.6rem;
		border-radius: 10px;
		border-left: 3px solid var(--accent-primary, #e85d5d);
		background: var(--bg-soft, #f5f3ef);
		max-width: 90%;
		min-width: 0;
	}

	.chat-bubble-action-badge {
		font-size: 0.58rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: var(--accent-primary, #e85d5d);
	}

	.chat-bubble-action-text {
		font-size: 0.72rem;
		font-weight: 600;
		color: var(--text-primary, #2d2a26);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		word-break: break-word;
	}

	.chat-bubble-action-hint {
		font-size: 0.6rem;
		color: var(--text-muted, #8a847a);
		font-style: italic;
	}

	.chat-bubble-action-summary {
		font-size: 0.65rem;
		color: var(--text-secondary, #5e5952);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		word-break: break-word;
	}

	.chat-bubble-action-meta {
		font-size: 0.56rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: var(--text-muted, #8a847a);
	}

	.chat-bubble-action-done {
		border-left-color: #5cb85c;
	}

	.chat-bubble-action-status {
		border-left-color: #5bc0de;
	}

	.chat-bubble-action-escalation {
		border-left-color: #eab308;
	}

	.chat-bubble-action-rich {
		max-width: 100%;
	}

	.chat-bubble-msg-content-rich {
		max-width: 100%;
	}

	.chat-bubble-msg-content-rich :global(.chat-rich-blocks) {
		margin-top: 0;
	}

	.chat-bubble-action-rich :global(.chat-rich-blocks) {
		margin-top: 0.45rem;
	}

	.chat-bubble-task-runs {
		display: flex;
		flex-direction: column;
		gap: 0.45rem;
		margin-top: 0.35rem;
	}

	.chat-bubble-task-run {
		display: flex;
		flex-direction: column;
		gap: 0.16rem;
		padding: 0.4rem 0.5rem;
		border-radius: 8px;
		background: rgba(255, 255, 255, 0.72);
		border: 1px solid rgba(91, 192, 222, 0.22);
	}

	.chat-bubble-task-run-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
	}

	.chat-bubble-task-run-label {
		font-size: 0.63rem;
		font-weight: 700;
		color: var(--text-primary, #2d2a26);
	}

	.chat-bubble-task-run-state {
		font-size: 0.55rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: var(--text-muted, #8a847a);
	}

	.chat-bubble-task-run-meta {
		font-size: 0.56rem;
		color: var(--text-muted, #8a847a);
	}

	/* ===== Input ===== */
	.chat-bubble-input {
		display: flex;
		gap: 0.35rem;
		align-items: center;
		padding: 0.5rem 0.65rem 0.6rem;
		border-top: 1px solid var(--border-soft, #ebe7e0);
	}

	.chat-bubble-textarea {
		flex: 1;
		resize: none;
		min-height: 36px;
		max-height: 80px;
		font-size: 0.78rem;
		font-family: var(--font-primary);
		/* line-height + padding tuned so a single-line placeholder /
		   user input sits vertically centered in the 36px min-height
		   row (the textarea grows downward when content wraps). */
		line-height: 1.5;
		border-radius: 10px;
		padding: 0.55rem 0.65rem;
	}

	.chat-bubble-send {
		flex-shrink: 0;
		width: 36px;
		height: 36px;
		padding: 0;
		display: flex;
		align-items: center;
		justify-content: center;
		border-radius: 8px;
	}

	/* ===== Responsive =====
	   Centered modal naturally adapts via min() in width/height; on small
	   viewports we just relax the backdrop padding so the panel fills the
	   screen with breathing room. */
	@media (max-width: 520px) {
		.chat-bubble-shade {
			padding: 0.75rem;
			align-items: flex-end;
		}

		.chat-bubble-overlay {
			width: 100%;
			height: min(80vh, calc(100vh - 5rem));
		}
	}
</style>
