<script lang="ts">
	/**
	 * FloatingComposer — glass-blur composer that floats above the chat
	 * canvas.
	 *
	 * Phase 1 builds this as a stateless chrome component: the consumer
	 * (chat/+page.svelte, t/[name]/+page.svelte) owns the actual chat
	 * state and wires it through props + events. The chat-page extraction
	 * happens in Phase 3.
	 *
	 * Slots:
	 *   • top — compact updates above the session dock
	 *   • attachments — rendered above the textarea (e.g. the staged
	 *     attachments strip)
	 *   • attachment-actions — beside Attach files, when attachments are allowed
	 *   • banner — rendered above the composer body (e.g. read-only
	 *     viewing-archived banner)
	 *   • warnings — rendered below the composer (e.g. profile warnings
	 *     about attachment support)
	 */
	import { createEventDispatcher } from 'svelte';
	import { clickOutside } from '$lib/shared/clickOutside';
	import { type ComposerMentionItem } from '$lib/magician/chat/composerMentions';
	import MentionTextarea from '$lib/magician/chat/MentionTextarea.svelte';
	import MentionPicker from '$lib/magician/chat/MentionPicker.svelte';
	import SlashCommandPicker from '$lib/magician/chat/SlashCommandPicker.svelte';
	import {
		EMPTY_SLASH_CONTEXT,
		isSlashCommandInput,
		resolveSlashInput,
		slashPrefix,
		slashViewFor,
		type SlashAction,
		type SlashCommandContext,
		type SlashItem,
		type SlashStep,
		type SlashView
	} from '$lib/magician/chat/composerCommands';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import VoiceControl from '$lib/magician/components/chat/VoiceControl.svelte';
	import CodingEnginePicker from './CodingEnginePicker.svelte';
	import ComposerModelChip from './ComposerModelChip.svelte';
	import type { ModelFeedback } from '$lib/llm/modelCatalog';
	import ComposerTalkCodeSwitch from './ComposerTalkCodeSwitch.svelte';
	import { draftInvokesVibedev, type CodingProfile } from '$lib/stores/codingProfileStore';

	// Live call controls occupy the banner slot while this text input remains available.

	export let value = '';
	export let placeholder = 'Reply…';
	export let mode: 'ask' | 'accept_in_scope' | 'plan' = 'ask';
	/** The remembered permission posture of Do mode. Kept separate from `mode`
	 *  so switching to Plan and back returns to the posture in force, and so
	 *  the split button can label itself while Plan is active. */
	export let permission: 'ask' | 'accept_in_scope' = 'ask';
	let permissionMenuOpen = false;
	let caretEl: HTMLButtonElement | null = null;

	function selectPermission(next: 'ask' | 'accept_in_scope') {
		permissionMenuOpen = false;
		dispatch('setMode', next);
	}
	/** The plan-mode composer is answering an existing planner question,
	 *  rather than creating a new plan. The host supplies the concrete target
	 *  through the banner slot, so hide the generic "draft a plan" banner. */
	export let planReplyActive = false;
	export let disabled = false;
	export let isReadOnly = false;
	export let isSending = false;
	export let isUploading = false;
	export let canSend = true;
	export let allowParallel = false;
	let sendOptionsOpen = false;
	/**
	 * Whether the composer holds anything sendable (text OR staged attachments).
	 * Attachments live in a slot the composer can't see, so the host reports it.
	 * `null` (default) → derive from the typed draft alone.
	 */
	export let hasContent: boolean | null = null;
	export let supportsAttachments = true;
	export let profileName: string | null = null;
	export let engineName: string | null = null;
	export let profileModel: string | null = null;
	export let profileModelLabel: string | null = null;
	export let profileThinkingLabel: string | null = null;
	export let profileFeedback: ModelFeedback | null = null;
	export let profileMenuOpen = false;
	/** Composer column cap. Accepts a number (treated as px) or any CSS
	 *  length string (e.g. `var(--chat-col)`) so the composer can share
	 *  a width token with the surface that mounts it. */
	export let maxWidth: number | string = 760;
	export let variant: 'chat' | 'dev' = 'chat';
	/** Page navigation belongs to full app surfaces, not the persistent HUD. */
	export let showModeSwitch = true;
	/**
	 * Render the docked chrome row above the input (the `dock` and
	 * `dock-actions` slots). Off by default so surfaces that don't dock —
	 * and the dev/workbench variant, which has no session identity to
	 * show — are untouched.
	 */
	export let dock = false;

	export let mentionItems: ComposerMentionItem[] = [];
	/** Named + Auto coding rows. Shown only while the draft invokes `@vibedev`. */
	export let codingProfiles: CodingProfile[] = [];
	/** Engines the server reports as blocked; shown disabled with their reason. */
	export let blockedCodingProfiles: CodingProfile[] = [];
	export let selectedCodingProfileId: string | null = null;
	export let codingProfileLocked = false;
	/** Offer the `/` command picker (`/budget`, `/engine`, …). Off by default so
	 *  surfaces that cannot run the commands keep `/` as plain text. */
	export let slashCommandsEnabled = false;
	/** The data the command options read (engines, tasks, budget defaults).
	 *  The host loads it on `slashNavigate` and runs `slashCommand`. */
	export let slashContext: SlashCommandContext | null = null;

	const dispatch = createEventDispatcher<{
		/** The `/` picker opened or moved to another level — load its data. */
		slashNavigate: { path: SlashStep[] };
		/** An option was chosen. The composer is already cleared; the host runs
		 *  it client-side (it is never sent as a message) and confirms. */
		slashCommand: SlashAction;
		send: void;
        parallel: void;
        stopAndSend: void;
		setMode: 'ask' | 'accept_in_scope' | 'plan';
		attach: void;
		attachFiles: { files: File[] };
		toggleProfile: void;
		selectCodingProfile: { id: string };
		input: void;
		micCapture: { file: File; durationMs: number };
		micTranscribe: { transcript: string; durationMs: number };
		micTranscribeDelta: { transcript: string };
		// Mid-flight cancel of the in-flight chat turn. Fires when the
		// user clicks the button while `isSending && !canSend` (busy
		// with nothing typed) — the button shows a stop icon in that
		// state instead of the progress spinner. Parent wires this to
		// `chatStore.cancelChatRun(sessionId)` which hits
		// `DELETE /chat/sessions/{id}/run`; the backend's per-session
		// `CancellationToken` aborts the LLM provider request mid-stream.
		stop: void;
	}>();

	// Mention state lives in <MentionTextarea> now; these are bound from it so the
	// host can render its own picker dock (Option A — placement/CSS unchanged).
	let textareaEl: MentionTextarea | null = null;
	let mentionOpen = false;
	let mentionMatches: ComposerMentionItem[] = [];
	let mentionActiveIndex = 0;

	// ── `/` command picker ──────────────────────────────────────────────────
	// Text-driven (composerCommands.ts): the draft reads `/<chosen tokens> <query>`,
	// so deleting into a chosen token steps back on its own. `slashPath` holds the
	// levels chosen so far; `slashDismissed` is set by Escape at the top level and
	// cleared once the draft stops being a command.
	let slashPath: SlashStep[] = [];
	let slashDismissed = false;
	let slashActiveIndex = 0;
	let slashView: SlashView | null = null;
	let lastSlashNavKey: string | null = null;

	$: slashActive = slashCommandsEnabled && variant !== 'dev' && !isReadOnly;
	$: if (!isSlashCommandInput(value)) slashDismissed = false;
	// `slashPath` is read inside the function, not in the `$:` line, so it is not
	// a reactive dependency (it is written here — a dependency would be a cycle).
	// Every path change also changes `value`, which re-runs this.
	function resolveSlash(draft: string, enabled: boolean): ReturnType<typeof resolveSlashInput> {
		if (!enabled) return { open: false, path: [], query: '' };
		const resolved = resolveSlashInput(draft, slashPath);
		slashPath = resolved.path;
		return resolved;
	}
	$: slashResolved = resolveSlash(value, slashActive && !slashDismissed);
	$: slashView = slashResolved.open
		? slashViewFor(slashResolved.path, slashResolved.query, slashContext ?? EMPTY_SLASH_CONTEXT)
		: null;
	// At the top level a draft that matches no command is ordinary text (`/usr/bin
	// …`), so the picker steps aside and Enter sends it. Below it, the picker
	// stays up — the user is mid-command.
	$: slashOpen = !!slashView && (slashResolved.path.length > 0 || slashView.items.length > 0);
	// The send options cannot send a command draft; keep them shut while it is one.
	$: if (slashOpen) sendOptionsOpen = false;
	$: if (slashView && slashActiveIndex >= slashView.items.length) {
		slashActiveIndex = Math.max(slashView.items.length - 1, 0);
	}
	$: {
		const key = slashOpen ? slashResolved.path.map((step) => step.id).join('/') : null;
		if (key !== lastSlashNavKey) {
			lastSlashNavKey = key;
			slashActiveIndex = 0;
			if (key !== null) dispatch('slashNavigate', { path: slashResolved.path });
		}
	}

	function setSlashDraft(path: SlashStep[]): void {
		slashPath = path;
		value = slashPrefix(path);
		slashActiveIndex = 0;
		textareaEl?.focusEnd();
		dispatch('input');
	}

	function chooseSlashItem(item: SlashItem | undefined): void {
		if (!item) return;
		if (item.next) {
			setSlashDraft([...slashResolved.path, item.next]);
			return;
		}
		if (!item.action) return;
		slashPath = [];
		value = '';
		slashActiveIndex = 0;
		dispatch('slashCommand', item.action);
		dispatch('input');
		textareaEl?.focusEnd();
	}

	function slashBack(): void {
		if (slashResolved.path.length === 0) {
			slashDismissed = true;
			return;
		}
		setSlashDraft(slashResolved.path.slice(0, -1));
	}

	/** Keys the `/` picker consumes while open. Returns true when handled. */
	function handleSlashKeydown(event: KeyboardEvent): boolean {
		if (!slashOpen || !slashView) return false;
		const count = slashView.items.length;
		if (event.key === 'ArrowDown' && count > 0) {
			event.preventDefault();
			slashActiveIndex = (slashActiveIndex + 1) % count;
			return true;
		}
		if (event.key === 'ArrowUp' && count > 0) {
			event.preventDefault();
			slashActiveIndex = (slashActiveIndex - 1 + count) % count;
			return true;
		}
		if (event.key === 'Tab' || (event.key === 'Enter' && !event.shiftKey)) {
			// Never let a half-typed command go out as a chat message.
			event.preventDefault();
			chooseSlashItem(slashView.items[slashActiveIndex]);
			return true;
		}
		if (event.key === 'Escape') {
			event.preventDefault();
			slashBack();
			return true;
		}
		if (
			event.key === 'Backspace' &&
			slashResolved.path.length > 0 &&
			slashResolved.query.length === 0 &&
			!event.altKey &&
			!event.metaKey &&
			!event.ctrlKey
		) {
			event.preventDefault();
			slashBack();
			return true;
		}
		return false;
	}

	export function focus(): void {
		textareaEl?.focus();
	}

	// The profile-picker listbox itself renders in the host (ChatPanel);
	// the host calls this to return focus to the trigger chip when the
	// listbox closes via Escape.
	let profileTriggerEl: HTMLButtonElement | null = null;

	export function focusProfileTrigger(): void {
		profileTriggerEl?.focus();
	}
	export function profileTrigger(): HTMLButtonElement | null {
		return profileTriggerEl;
	}

	// Only fires for keys the mention layer did NOT consume (i.e. the picker is
	// closed) — the host keeps its submit chord. Shift+Enter is never consumed by
	// the mention layer, so it falls through here and inserts a newline.
	function handleKeydown(event: KeyboardEvent): void {
		if (handleSlashKeydown(event)) return;
		if (event.key === 'Enter' && !event.shiftKey) {
			event.preventDefault();
			if (canSend && !disabled && !isReadOnly) dispatch('send');
		}
	}

	function handleSendClick(): void {
		// Stop-affordance branch: busy with nothing queued to send → the
		// button is showing the stop icon (see `showStopAffordance`).
		// Dispatch `stop` so the parent can cancel the in-flight turn.
		if (showStopAffordance) {
			dispatch('stop');
			return;
		}
		// A draft in the `/` picker is a command, not a message: the arrow picks
		// the highlighted option, exactly as Enter does.
		if (slashOpen && slashView) {
			chooseSlashItem(slashView.items[slashActiveIndex]);
			return;
		}
		if (canSend && !disabled && !isReadOnly) dispatch('send');
	}

	// The send arrow appears only once there is something to send: a permanently
	// visible, permanently disabled control on an empty composer is dead chrome
	// competing with the mic/voice buttons beside it. Enter still sends, and
	// Shift+Enter still newlines, regardless of the button's presence.
	//
	// The stop square REPLACES it while a turn is in flight and shows even on an
	// empty composer, because there's no other way to cancel mid-turn.
	$: showStopAffordance = isSending && !composerHasContent && !isReadOnly && !disabled;
	// Emptiness is the host's call when it knows more than the draft does — a
	// staged attachment with no text is still a sendable turn — so `hasContent`
	// wins when provided and the typed draft is the fallback.
	$: composerHasContent = hasContent ?? value.trim().length > 0;

	// Normalise the width prop to a CSS length once so the template
	// stays a plain var() assignment.
	$: composerMaxWidth = typeof maxWidth === 'number' ? `${maxWidth}px` : maxWidth;
	$: showCodingPicker = variant !== 'dev' && codingProfiles.length > 0 && draftInvokesVibedev(value);
	$: selectedCodingProfile =
		codingProfiles.find((profile) => profile.id === selectedCodingProfileId) ?? null;

	// File drag-and-drop onto the composer. The Tauri HUD window disables the
	// native drag-drop interception (`drag_drop_enabled(false)`) so these HTML5
	// events reach the page; in a plain browser they arrive natively. Gated on
	// `supportsAttachments` so hosts with attachment-less profiles see nothing.
	let fileDragActive = false;

	function fileDropAllowed(): boolean {
		return supportsAttachments && !isReadOnly && !disabled;
	}

	function handleFileDragOver(event: DragEvent): void {
		if (!fileDropAllowed() || !event.dataTransfer?.types.includes('Files')) return;
		event.preventDefault();
		fileDragActive = true;
	}

	function handleFileDragLeave(event: DragEvent): void {
		const target = event.currentTarget as HTMLElement;
		const related = event.relatedTarget as Node | null;
		if (related && target.contains(related)) return;
		fileDragActive = false;
	}

	function handleFileDrop(event: DragEvent): void {
		fileDragActive = false;
		if (!fileDropAllowed() || !event.dataTransfer?.files?.length) return;
		event.preventDefault();
		// Hosts may run their own drop zone on an ancestor (the HUD stage);
		// without this the same files would upload twice.
		event.stopPropagation();
		dispatch('attachFiles', { files: Array.from(event.dataTransfer.files) });
	}

</script>

<div
	class="composer-wrap"
	role="group"
	aria-label="Message composer"
	class:plan-mode={variant !== 'dev' && mode === 'plan'}
	class:composer-wrap--dev={variant === 'dev'}
	class:composer-wrap--dragging={fileDragActive}
	style="--composer-max-width: {composerMaxWidth};"
	on:dragenter={handleFileDragOver}
	on:dragover={handleFileDragOver}
	on:dragleave={handleFileDragLeave}
	on:drop={handleFileDrop}
>
	<div
		class="composer-shell"
		class:plan-on={variant !== 'dev' && mode === 'plan'}
		class:composer-shell--dev={variant === 'dev'}
	>
		<slot name="top" />
		<!-- Docked chrome row — the session ContextPill, plus whatever the
		     host pins to the right (the Tauri HUD puts theme + expand there).
		     Rendered above the banner so it reads as the composer's title bar.

		     Gated on an explicit `dock` prop rather than on slot presence:
		     `<svelte:fragment slot>` can't be wrapped in `{#if}`, so a host
		     that fills the slot conditionally would still register it, and
		     the row would paint its padding + border around nothing. -->
		{#if dock}
			<div class="composer-dock">
				<slot name="dock" />
				<slot name="dock-actions" />
			</div>
		{/if}

		<slot name="banner" />

		{#if variant !== 'dev' && mode === 'plan' && !planReplyActive}
			<div class="plan-banner" role="status">
				<span class="dot" aria-hidden="true"></span>
				<span><b>Plan mode</b> — I'll draft a plan first. Switch to Do for direct execution.</span>
			</div>
		{/if}

		{#if variant !== 'dev'}
			<slot name="attachments" />
		{/if}

		<div class="composer-row">
			<MentionTextarea
				bind:this={textareaEl}
				bind:value
				{placeholder}
				disabled={disabled || isReadOnly}
				minHeight={32}
				maxHeight={220}
				{mentionItems}
				mentionsEnabled={variant !== 'dev'}
				bind:mentionOpen
				bind:mentionMatches
				bind:mentionActiveIndex
				on:input={() => dispatch('input')}
				on:focus={() => dispatch('input')}
				on:keydown={(e) => handleKeydown(e.detail)}
			/>
			<!-- Send arrow: present only once there's something to send (see
			     `composerHasContent`), so an empty composer isn't carrying a
			     permanently disabled button. Enter still sends and Shift+Enter
			     still newlines (`handleKeydown` above) either way. While a turn
			     is in flight this slot becomes the stop square instead, shown
			     even on an empty composer so the turn can always be cancelled. -->
			{#if showStopAffordance}
				<button
					type="button"
					class="send stop"
					on:click={handleSendClick}
					aria-label="Stop generating"
					title="Stop — cancel the in-flight turn"
				>
					<svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor" stroke="none" aria-hidden="true">
						<rect x="6" y="6" width="12" height="12" rx="1.5"/>
					</svg>
				</button>
			{:else if composerHasContent}
				<button
					type="button"
					class="send send--revealed"
					on:click={handleSendClick}
					disabled={!canSend || disabled || isReadOnly}
					aria-label={isSending ? "Queue message" : "Send message"}
					title={isSending ? "Queue — Enter" : "Send — Enter"}
				>
					<!-- arrow-up-right rotated -45° points straight up:
					     the classic "send" glyph without a new icon. -->
					<Icon name="arrow-up-right" size={14} style="transform: rotate(-45deg)" />
				</button>
			{/if}

            {#if composerHasContent && variant !== 'dev' && (allowParallel || isSending)}
                <button type="button" class="icon-btn" aria-label="Send options" aria-expanded={sendOptionsOpen}
                    disabled={disabled || isReadOnly || slashOpen} on:click={() => sendOptionsOpen = !sendOptionsOpen}>
                    <Icon name="chevron-down" size={14} />
                </button>
            {/if}

			<!-- Two voice icons in circular slots right of the send button:
			     tap-to-record voice note (inline orb + timer while recording)
			     and realtime voice call. No modal — recording state lives in
			     the slot itself, so the composer never grows a banner. -->
			{#if variant !== 'dev'}
				<VoiceControl
					{disabled}
					{isReadOnly}
					{isUploading}
					{isSending}
					on:micCapture={(e) => dispatch('micCapture', e.detail)}
					on:micTranscribe={(e) => dispatch('micTranscribe', e.detail)}
					on:micTranscribeDelta={(e) => dispatch('micTranscribeDelta', e.detail)}
				/>
			{/if}
		</div>

        <!-- A draft in the `/` picker is a command: none of these may send it. -->
        {#if sendOptionsOpen && composerHasContent && !slashOpen}
            <div class="send-options" role="group" aria-label="Send options">
                <button type="button" disabled={!canSend || disabled || isReadOnly} on:click={() => { sendOptionsOpen = false; dispatch('send'); }}>{isSending ? 'Queue message' : 'Send message'}</button>
                {#if isSending}<button type="button" disabled={disabled || isReadOnly} on:click={() => { sendOptionsOpen = false; dispatch('stopAndSend'); }}>Stop &amp; send</button>{/if}
                {#if allowParallel}<button type="button" disabled={disabled || isReadOnly} on:click={() => { sendOptionsOpen = false; dispatch('parallel'); }}>Run in parallel</button>{/if}
            </div>
        {/if}

		{#if slashOpen && slashView}
			<div class="mention-dock">
				<SlashCommandPicker
					view={slashView}
					activeIndex={slashActiveIndex}
					canGoBack={slashResolved.path.length > 0}
					on:select={(e) => chooseSlashItem(e.detail)}
					on:back={() => slashBack()}
				/>
			</div>
		{:else if mentionOpen}
			<div class="mention-dock">
				<MentionPicker
					matches={mentionMatches}
					activeIndex={mentionActiveIndex}
					on:select={(e) => textareaEl?.applyMention(e.detail)}
				/>
			</div>
		{/if}

		{#if variant === 'dev'}
			<div class="composer-dev-controls">
				<slot name="dev-controls" />
			</div>
		{:else}
		<div class="composer-tools">
			{#if showModeSwitch}
				<ComposerTalkCodeSwitch active="talk" draftText={value} />
			{/if}
			<span class="seg" class:plan-on={mode === 'plan'} class:accept-on={mode === 'accept_in_scope'} role="tablist" aria-label="Composer mode">
				<!-- Do is a split control: the face selects Do, the caret picks how
				     much it asks. Accept is a permission posture, not a peer mode,
				     so it belongs inside Do rather than beside it. -->
				<span class="seg-split" class:active={mode !== 'plan'}>
				<button
					type="button"
					class="seg-face"
					class:active={mode !== 'plan'}
					on:click={() => dispatch('setMode', permission)}
				>Do <span class="seg-posture">· {permission === 'accept_in_scope' ? 'Accept' : 'Ask'}</span></button>
				<button
					type="button"
					class="seg-caret"
					class:active={mode !== 'plan'}
					aria-haspopup="menu"
					aria-expanded={permissionMenuOpen}
					aria-label="Choose what Do asks before editing files"
					title="Choose what Do asks before editing files"
					bind:this={caretEl}
					on:click|stopPropagation={() => (permissionMenuOpen = !permissionMenuOpen)}
				>
					<svg width="9" height="9" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><polyline points="6 9 12 15 18 9" /></svg>
				</button>
				</span>
				<button
					type="button"
					class:active={mode === 'plan'}
					on:click={() => dispatch('setMode', 'plan')}
				>Plan</button>
			</span>
			{#if permissionMenuOpen}
				<!-- svelte-ignore a11y-no-static-element-interactions -->
				<div
					class="permission-menu"
					role="menu"
					use:clickOutside={{
						handler: () => (permissionMenuOpen = false),
						exclude: [caretEl]
					}}
				>
					<button
						type="button"
						role="menuitemradio"
						aria-checked={permission === 'ask'}
						on:click={() => selectPermission('ask')}
					>
						<span class="tick">{permission === 'ask' ? '✓' : ''}</span>
						<span class="permission-copy">
							<strong>Ask</strong>
							<small>Prompt before each file edit</small>
						</span>
					</button>
					<button
						type="button"
						role="menuitemradio"
						aria-checked={permission === 'accept_in_scope'}
						on:click={() => selectPermission('accept_in_scope')}
					>
						<span class="tick">{permission === 'accept_in_scope' ? '✓' : ''}</span>
						<span class="permission-copy">
							<strong>Accept</strong>
							<small>In-scope file edits, no prompt. Anything outside the workspace still asks.</small>
						</span>
					</button>
				</div>
			{/if}
			<button
				type="button"
				class="icon-btn"
				title={supportsAttachments ? 'Attach files' : 'Attachments require a vision-capable profile'}
				disabled={!supportsAttachments || isUploading || isReadOnly}
				on:click={() => dispatch('attach')}
				aria-label="Attach files"
			>
				<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
					<path d="M21.44 11.05l-8.49 8.49a6 6 0 0 1-8.49-8.49l8.49-8.48a4 4 0 0 1 5.66 5.65l-8.49 8.49a2 2 0 0 1-2.83-2.83l7.78-7.78"/>
				</svg>
			</button>
			<!-- Optional host actions share attachment availability. -->
			{#if supportsAttachments && !isUploading && !isReadOnly}
				<slot name="attachment-actions" />
			{/if}
			<!-- Spacer splits the row: mode + attach stay left with the input,
			     the profile chip and its thinking-mode status park right. -->
			<span class="spacer"></span>
			{#if showCodingPicker}
				<CodingEnginePicker
					compact
					ariaLabel="VibeDev coding engine"
					profiles={codingProfiles}
					blockedProfiles={blockedCodingProfiles}
					selectedProfileId={selectedCodingProfileId}
					selectedProfile={selectedCodingProfile}
					disabled={codingProfileLocked}
					on:select={(event) => dispatch('selectCodingProfile', { id: event.detail.id })}
				/>
			{/if}
			{#if profileName}
				<ComposerModelChip
					engine={engineName}
					model={profileModelLabel ?? profileModel}
					modelIdentifier={profileModel ?? profileName}
					feedback={profileFeedback}
					thinking={profileThinkingLabel}
					expanded={profileMenuOpen}
					bind:triggerElement={profileTriggerEl}
					on:click={() => dispatch('toggleProfile')}
					ariaLabel={`Chat engine: ${engineName ?? 'Magician'}, selection: ${profileName}${profileThinkingLabel ? `, thinking: ${profileThinkingLabel}` : ''}`}
					title={`${engineName ?? 'Magician'}: ${profileName}${profileModel ? ` (${profileModel})` : ''}`}
				/>
			{/if}
		</div>
		{/if}

		{#if variant !== 'dev'}
			<slot name="warnings" />
		{/if}
	</div>
</div>

<style>
    .send-options { display: flex; flex-wrap: wrap; gap: 4px; padding: 4px 10px 8px; border-top: 1px solid var(--border-soft); }
    .send-options button { font-size: 12px; padding: 5px 8px; border-radius: var(--radius-chip, 6px); color: var(--text-primary); background: var(--bg-soft); }
    .send-options button:hover { background: var(--accent-primary-soft); }

	.composer-wrap {
		/* Static positioning: composer is a normal flex child of
		   `.chat-main` (which uses `display: flex; flex-direction:
		   column`). Combined with `chat-messages-area`'s `flex: 1`,
		   the composer sits naturally below the messages with NO
		   overlap and no big bottom-padding hack needed on the
		   messages area.
		   Previously this was `position: fixed; bottom: 10px;` which
		   pinned the composer to the viewport and required the
		   messages area to reserve ~120px of padding at the bottom
		   so the last message could scroll above it. */
		display: flex;
		justify-content: center;
		flex: 0 0 auto;
	}

	.composer-shell {
		pointer-events: auto;
		width: min(var(--composer-max-width, 760px), calc(100% - 48px));
		background: var(--bg-elevated, #fff);
		border: 1px solid var(--border-default, rgba(0, 0, 0, 0.18));
		border-radius: var(--radius-composer, var(--radius-lg, 14px));
		box-shadow:
			0 1px 0 rgba(255, 255, 255, 0.04) inset,
			var(--shadow-lg, 0 24px 48px -12px rgba(0, 0, 0, 0.25));
		/* `visible` (not `hidden`) so the absolutely-positioned recording
		   overlays — the live `0:03` timer pill above the mic and the
		   post-recording preview row — can poke above the composer shell
		   without being clipped by its rounded-corner mask. The inner
		   surfaces don't paint to the shell's edges, so we lose nothing
		   visually. */
		overflow: visible;
		transition: border-color 0.18s ease, box-shadow 0.18s ease;
		backdrop-filter: blur(20px) saturate(140%);
		-webkit-backdrop-filter: blur(20px) saturate(140%);
	}

	/* Dock row. Sits inside the shell's rounded top, above the banner. The
	   bottom hairline separates it from the input without adding a second
	   heavy border — the tools row at the bottom already carries one. */
    .composer-shell > :global(.queue-inspector:not(:first-child)),
    .composer-shell > :global(.voice-requests:not(:first-child)) {
        border-top-left-radius: 0; border-top-right-radius: 0;
    }
	/* The card owns its corner geometry: whichever in-card row lands first
	   (top-slot rows, or a banner when no dock row renders) follows the
	   card's own radius token, so it matches the card in every theme. */
	.composer-shell > :global(.queue-inspector:first-child),
	.composer-shell > :global(.voice-requests:first-child),
	.composer-shell > :global(.tray-live-banner:first-child),
	.composer-shell > :global(.voice-autosend:first-child) {
		border-top-left-radius: calc(var(--radius-composer, var(--radius-lg, 14px)) - 1px);
		border-top-right-radius: calc(var(--radius-composer, var(--radius-lg, 14px)) - 1px);
	}
	.composer-dock {
		display: flex;
		align-items: center;
		gap: 8px;
		min-width: 0;
		padding: 4px 12px 4px 14px;
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
	}
	.composer-dock:first-child {
		border-radius: calc(var(--radius-composer, var(--radius-lg, 14px)) - 1px) calc(var(--radius-composer, var(--radius-lg, 14px)) - 1px) 0 0;
	}
	.composer-dock:hover, .composer-dock:focus-within {
		background: var(--accent-primary-soft);
	}
	.composer-dock:has(:global(:focus-visible)) {
		outline: 2px solid var(--accent-primary);
		outline-offset: -2px;
	}

	/* Reveal the docked pill's actions from anywhere on the composer, not
	   just the pill itself — the pill is a thin strip and hunting for it
	   would be fussy. `:global()` because the pill is a child component. */
	.composer-shell:hover .composer-dock :global(.ctx-btn),
	.composer-shell:hover .composer-dock :global(.more-wrap),
	.composer-shell:focus-within .composer-dock :global(.ctx-btn),
	.composer-shell:focus-within .composer-dock :global(.more-wrap) {
		opacity: 1;
	}

	.composer-wrap--dev {
		z-index: 950;
	}

	/* File-drag state: the shell adopts the focus treatment so the drop target
	   is unmistakable before the user lets go. Same border/shadow vocabulary
	   the shell already uses on hover/focus, so no new tokens. */
	.composer-wrap--dragging .composer-shell {
		border-color: var(--accent, rgba(90, 140, 250, 0.7));
		box-shadow:
			0 0 0 3px color-mix(in srgb, var(--accent, rgba(90, 140, 250, 0.7)) 22%, transparent),
			var(--shadow-lg, 0 24px 48px -12px rgba(0, 0, 0, 0.25));
	}

	.composer-shell--dev {
		overflow: visible;
	}

	.composer-shell:focus-within {
		border-color: var(--accent-primary, #c2502a);
		box-shadow:
			0 1px 0 rgba(255, 255, 255, 0.06) inset,
			var(--shadow-lg, 0 28px 56px -12px rgba(0, 0, 0, 0.32)),
			var(--input-focus-shadow, 0 0 0 4px rgba(194, 80, 42, 0.12));
	}

	.composer-shell.plan-on {
		border-color: var(--accent-primary, #c2502a);
		box-shadow:
			0 1px 0 rgba(255, 255, 255, 0.06) inset,
			var(--shadow-lg, 0 24px 48px -12px rgba(0, 0, 0, 0.25)),
			var(--input-focus-shadow, 0 0 0 4px rgba(194, 80, 42, 0.12));
	}

	.composer-dev-controls {
		display: flex;
		align-items: flex-end;
		gap: 10px;
		padding: 4px 12px 11px;
		border-top: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
	}

	.plan-banner {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 5px 14px 3px;
		font-family: var(--font-primary);
		font-size: 12.5px;
		color: var(--accent-primary-hover, var(--accent-primary, #c2502a));
		border-bottom: 1px solid var(--accent-primary-soft, rgba(194, 80, 42, 0.12));
		background: var(--accent-primary-soft, rgba(194, 80, 42, 0.08));
	}

	/* Round only when nothing sits above it: under the dock row a rounded top
	   left a notch between the two bands. */
	.plan-banner:first-child {
		border-radius: calc(var(--radius-composer, var(--radius-lg, 14px)) - 1px) calc(var(--radius-composer, var(--radius-lg, 14px)) - 1px) 0 0;
	}

	.plan-banner .dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: var(--accent-primary, #c2502a);
		animation: pulse 1.8s ease-in-out infinite;
	}

	@keyframes pulse {
		0%, 100% { opacity: 1; }
		50% { opacity: 0.4; }
	}

	.composer-row {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 12px 12px 12px 14px;
	}

	.send {
		width: 32px;
		height: 32px;
		border-radius: var(--radius-full, 999px);
		background: var(--button-primary-bg, var(--text-ink, #1a1a1a));
		color: var(--button-primary-color, #fff);
		display: inline-flex;
		align-items: center;
		justify-content: center;
		border: 0;
		cursor: pointer;
		transition: background 0.15s ease, transform 0.15s ease, opacity 0.15s ease;
		flex-shrink: 0;
	}

	.send:hover:not(:disabled) {
		background: var(--accent-fill, #c2502a);
		transform: translateY(-1px);
	}

	.send:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	/* The send arrow mounts the moment a draft exists, so it grows in rather
	   than popping — a 120ms scale/fade that reads as the composer arming
	   itself. Honors reduced-motion below. */
	.send--revealed {
		animation: send-reveal 0.12s ease-out;
	}

	@keyframes send-reveal {
		from {
			opacity: 0;
			transform: scale(0.6);
		}
		to {
			opacity: 1;
			transform: scale(1);
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.send--revealed {
			animation: none;
		}
	}

	/* Stop state of the send button — shown while the composer is
	   busy. The danger palette reads the destructive intent at a
	   glance. */
	.send.stop {
		background: var(--accent-fill, #ff6b6b);
		color: #fff;
		border: 0;
	}

	.send.stop:hover:not(:disabled) {
		background: var(--accent-fill-hover, #e85555);
		transform: translateY(-1px);
	}

	.mention-dock {
		margin: 0 12px 8px 14px;
	}

	/* Bottom chrome row. Deliberately tighter than the dock row above: the
	   dock carries identity (which session am I in) and wants breathing
	   room, while these are dense controls the eye scans rather than
	   reads. */
	.composer-tools {
		display: flex;
		/* Symmetric padding, so the controls sit centred in the row rather
		   than riding high in it. The old `2px 12px 6px` put three times more
		   space below than above, which read as top-aligned. */
		align-items: center;
		gap: 4px;
		padding: 2px 12px;
		border-top: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
		/* Anchor for the permission menu, which opens upward out of this row. */
		position: relative;
	}

	.seg {
		display: inline-flex;
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
		border-radius: 5px;
		padding: 1.5px;
	}

	.seg button {
		padding: 2px 7px;
		font-family: var(--font-mono);
		font-size: 9.5px;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-muted, #888);
		background: transparent;
		border: 0;
		border-radius: 3.5px;
		cursor: pointer;
		transition: background 0.12s ease, color 0.12s ease;
	}

	/* Selected Do: an accent-tinted pill with accent ink and a thin accent
	   ring, so "selected" reads at a glance. The ink is the accent mixed
	   toward the text colour to keep contrast at 9.5px. Plan and Accept keep
	   their solid fills below. */
	.seg button.active {
		background: var(--accent-primary-soft);
		color: color-mix(in srgb, var(--accent-primary) 72%, var(--text-primary));
		font-weight: 700;
		box-shadow: 0 0 0 1px color-mix(in srgb, var(--accent-primary) 40%, transparent);
	}

	/* The primary-button pair is contrast-checked (4.5:1 on every stop) in
	   every theme; plain white on --accent-primary is not, at 9.5px. */
	.seg.plan-on button.active {
		background: var(--button-primary-bg, var(--accent-primary));
		color: var(--button-primary-color, var(--text-on-accent));
	}

	/* Page-ground ink on the success fill: dark on light greens, light on
	   dark ones — white failed on the dark themes' pale greens. */
	.seg.accept-on button.active {
		background: var(--color-success);
		color: var(--bg-base);
	}

	.icon-btn {
		width: 20px;
		height: 20px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		color: var(--text-muted, #888);
		background: transparent;
		border: 0;
		border-radius: 5px;
		cursor: pointer;
		transition: background 0.15s ease, color 0.15s ease;
	}
	.icon-btn svg {
		width: 11px;
		height: 11px;
	}

	.icon-btn:hover:not(:disabled) {
		background: var(--bg-soft, rgba(0, 0, 0, 0.06));
		color: var(--text-primary, #1a1a1a);
	}

	.icon-btn:disabled {
		opacity: 0.4;
		cursor: not-allowed;
	}


	.spacer {
		flex: 1;
	}

	/* ── Mobile composer reflow ───────────────────────────────────────
	   Below 768px the toolbar overflows: hide low-frequency controls
	   so Mic / Camera / VoiceCall / Send keep their thumb real estate.
	   `.seg` = Do/Plan tablist (power-user surface, hidden on phone
	   per same rationale as /chat's `.chat-plan-mode-toggle`).
	   `.icon-btn` first instance = attach (system picker on mobile is
	   redundant with Camera). Auto-speak is a setting, not a per-turn
	   action — hide via :global() since AutoSpeakToggle owns its own
	   scoped styles. */
	/* ── Cross-viewport composer tidy ─────────────────────────────────
	   Apply at all widths (desktop AND mobile): park Mic + VoiceCall at the
	   right edge with VoiceCall rightmost (audio actions cluster on the
	   dominant-hand side). The mobile-only hides + touch-target bumps live
	   in the @media block below.

	   The spacer is live again — it was disabled when the ↩ kbd hint chip it
	   used to push right was removed, and now splits mode + attach (left,
	   with the input) from the profile chip (right).
	*/

	.composer-tools :global(.mic-capture) {
		order: 90;
		margin-left: auto;
	}
	.composer-tools :global(.voice-call-btn) {
		order: 91;
	}

	/* Shrink the native bordered chrome of AutoSpeakToggle inside the
	   2nd-row tools cluster so it matches the smaller `.icon-btn` /
	   `.seg` scale (20px box, 11px glyph). Kept in lockstep with
	   `.icon-btn` above — if that changes, change this. */
	.composer-tools :global(.auto-speak-toggle) {
		width: 20px;
		height: 20px;
		min-width: 20px;
		min-height: 20px;
		border-radius: 5px;
	}
	.composer-tools :global(.auto-speak-toggle svg) {
		width: 11px;
		height: 11px;
	}

	@media (max-width: 767px) {
		.composer-tools .seg {
			display: none;
		}
		.composer-tools :global(.mic-capture__btn),
		.composer-tools :global(.capture-btn),
		.composer-tools :global(.voice-call-btn),
		.composer-tools :global(.auto-speak-toggle button),
		.composer-tools :global(.auto-speak-toggle__btn),
		.composer-tools > .icon-btn[aria-label="Attach files"] {
			width: 40px;
			height: 40px;
		}

	}

	/* Split control: Do carries its permission posture on its face, the caret
	   opens the choice. Accept is a permission, not a peer mode, so it reads as
	   part of Do rather than as a third thing to pick. */
	.seg .seg-face { display: inline-flex; align-items: baseline; gap: 0.25em; }
	.seg .seg-posture { opacity: 0.72; font-weight: 500; }
	.seg .seg-caret {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		padding-inline: 0.2em 0.45em;
	}
	/* Selected Do is ONE pill around face + caret. Each button drew its own
	   pill and ring before, and the rings overlapped where they met in themes
	   with a visible ring or a thick button border. */
	.seg .seg-split {
		display: inline-flex;
		align-items: stretch;
		border-radius: 3.5px;
		transition: background 0.12s ease, box-shadow 0.12s ease;
	}
	.seg .seg-split.active {
		background: var(--accent-primary-soft);
		box-shadow: 0 0 0 1px color-mix(in srgb, var(--accent-primary) 40%, transparent);
	}
	.seg.accept-on .seg-split.active {
		background: var(--color-success);
		box-shadow: none;
	}
	.seg .seg-split button,
	.seg .seg-split button.active,
	.seg.accept-on .seg-split button.active {
		background: transparent;
		box-shadow: none;
		border: 0;
		outline-offset: -2px;
	}
	.seg .seg-split .seg-face { padding-inline-end: 3px; }
	.seg .seg-caret svg { display: block; }

	.permission-menu {
		position: absolute;
		bottom: calc(100% + 6px);
		left: 0;
		z-index: 40;
		display: flex;
		flex-direction: column;
		min-width: 16rem;
		max-width: min(22rem, 80vw);
		padding: 4px;
		border-radius: 10px;
		border: 1px solid var(--border-default);
		background: var(--surface-raised, var(--bg-elevated));
		box-shadow: var(--shadow-lg);
	}
	.permission-menu button {
		display: grid;
		grid-template-columns: 1.1em 1fr;
		gap: 0.5em;
		align-items: start;
		width: 100%;
		padding: 7px 8px;
		border: 0;
		border-radius: 7px;
		background: transparent;
		color: inherit;
		text-align: left;
		cursor: pointer;
	}
	.permission-menu button:hover,
	.permission-menu button:focus-visible { background: var(--surface-hover, rgba(255, 255, 255, 0.07)); }
	.permission-menu .tick { line-height: 1.35; opacity: 0.9; }
	.permission-copy { display: flex; flex-direction: column; gap: 1px; min-width: 0; }
	.permission-copy strong { font-weight: 600; }
	/* The pair min-width:0 + overflow-wrap keeps the long Accept description
	   from forcing a one-character column. */
	.permission-copy small { opacity: 0.68; line-height: 1.35; overflow-wrap: anywhere; }
</style>
