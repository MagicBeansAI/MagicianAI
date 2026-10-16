<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import VoiceControl from '$lib/magician/components/chat/VoiceControl.svelte';
	import SuperGodModeToggle from '$lib/settings/SuperGodModeToggle.svelte';
	import MentionTextarea from '$lib/magician/chat/MentionTextarea.svelte';
	import MentionPicker from '$lib/magician/chat/MentionPicker.svelte';
	import type { ComposerMentionItem } from '$lib/magician/chat/composerMentions';
	import type { ChipFieldToken } from '$lib/magician/chat/chipMarkup';
	import type { UploadedAttachment } from '$lib/stores/chatStore';
	import CodingEnginePicker from './CodingEnginePicker.svelte';
	import ComposerTalkCodeSwitch from './ComposerTalkCodeSwitch.svelte';
	import type { CodingProfile } from '$lib/stores/codingProfileStore';

	type VibeComposerMode = 'fresh' | 'follow_up';
	type VibeComposerActiveRun = {
		label: string;
		title: string;
		meta: string[];
		blocked?: boolean;
	};

	export let value = '';
	export let mode: VibeComposerMode = 'fresh';
	export let placeholder = 'Ask your coding assistant to build, fix, refactor, or explain code...';
	export let profiles: CodingProfile[] = [];
	export let selectedProfileId: string | null = null;
	/** Engines the server reports as blocked; shown disabled with their reason. */
	export let blockedProfiles: CodingProfile[] = [];
	export let selectedProfile: CodingProfile | null = null;
	/** True while a provider/review/verification run is active. The next
	 *  settled request may switch; this one may not. */
	export let profileLocked = false;
	export let profileError: string | null = null;
	export let supportsImages = false;
	export let stagedAttachments: UploadedAttachment[] = [];
	export let submitDisabled = false;
	export let submitBlocker: string | null = null;
	export let submitting = false;
	export let uploading = false;
	export let preparingSession = false;
	export let activeRun: VibeComposerActiveRun | null = null;
	export let activeProjectName: string | null = null;
	/** Studio intent for the NEXT submit. When set, the composer shows a
	 *  Build/Discuss/Autopilot segment next to send and labels the send button by
	 *  mode — so "what happens when I hit enter" is obvious. Null hides it (legacy). */
	export let studioMode: 'build' | 'discuss' | 'autopilot' | null = null;
	/** Task references for the `@` picker — the cockpit builds these from its
	 *  completed persistent tasks. Empty disables mentions (the picker never opens).
	 *  A task chip serializes inline as `task:<id>` (LLM-legible inside the prompt)
	 *  AND, via `chipTokens()`, lets the cockpit attach the referenced task as a
	 *  structured continuation edge (`reference_task_ids`) on the coding run. */
	export let mentionItems: ComposerMentionItem[] = [];
	export let maxWidth: number | string = 760;
	export let costBudgetUsd: number | null = null;
	export let nightly = false;

	const dispatch = createEventDispatcher<{
		submit: void;
		setStudioMode: 'build' | 'discuss' | 'autopilot';
		selectProfile: { id: string };
		attachFiles: { files: File[] };
		removeAttachment: { attachmentId: string };
		newRun: void;
		openRail: void;
		input: void;
		costBudgetChange: { budget: number | null };
		nightlyChange: { nightly: boolean };
		micCapture: { file: File; durationMs: number };
		micTranscribe: { transcript: string; durationMs: number };
		micTranscribeDelta: { transcript: string };
	}>();

	let fileInputEl: HTMLInputElement;
	let imageInputEl: HTMLInputElement;
	let dragActive = false;

	// `@`-mention picker state, bound from <MentionTextarea> (which owns the state
	// machine). The composer renders its own picker dock from these.
	let textareaEl: MentionTextarea | null = null;
	let mentionOpen = false;
	let mentionMatches: ComposerMentionItem[] = [];
	let mentionActiveIndex = 0;

	/** The chips currently in the composer (kind/slug/label). The cockpit reads the
	 *  `task` chips at submit to attach the referenced tasks as structured
	 *  continuation refs — without parsing the serialized prompt text. */
	export function chipTokens(): ChipFieldToken[] {
		return textareaEl?.chipTokens() ?? [];
	}

	/** Focus the prompt field — e.g. after a first-run example chip seeds it. */
	export function focus(): void {
		textareaEl?.focus();
	}

	let recordingActive = false;
	$: busy = submitting || uploading || preparingSession;
	$: composerMaxWidth = typeof maxWidth === 'number' ? `${maxWidth}px` : maxWidth;
	$: hasContent =
		(value.trim().length > 0 || stagedAttachments.length > 0 || submitting || uploading) &&
		!recordingActive;
	$: modeLabel =
		studioMode === 'discuss' ? 'Discuss' : studioMode === 'autopilot' ? 'Autopilot' : 'Build';
	$: submitTitle =
		submitBlocker ??
		(studioMode
			? `${modeLabel}${mode === 'follow_up' ? ' · follow-up' : ''}${studioMode === 'discuss' ? ' (read-only — explain / plan)' : ''}`
			: mode === 'follow_up'
				? 'Send follow-up'
				: 'Start coding');

	function handleSubmit(): void {
		if (!submitDisabled) dispatch('submit');
	}

	function handleKeydown(event: KeyboardEvent): void {
		if ((event.metaKey || event.ctrlKey) && event.key === 'Enter') {
			event.preventDefault();
			handleSubmit();
		}
	}

	function openFilePicker(): void {
		if (busy) return;
		fileInputEl?.click();
	}

	function openImagePicker(): void {
		if (!supportsImages || busy) return;
		imageInputEl?.click();
	}

	function handleFileSelection(event: Event): void {
		const input = event.currentTarget as HTMLInputElement;
		const files = Array.from(input.files ?? []);
		input.value = '';
		if (files.length > 0) dispatch('attachFiles', { files });
	}

	function handleDragOver(event: DragEvent): void {
		if (!event.dataTransfer?.types.includes('Files')) return;
		event.preventDefault();
		dragActive = true;
	}

	function handleDragLeave(event: DragEvent): void {
		const target = event.currentTarget as HTMLElement;
		const related = event.relatedTarget as Node | null;
		if (related && target.contains(related)) return;
		dragActive = false;
	}

	function handleDrop(event: DragEvent): void {
		if (!event.dataTransfer?.files?.length) return;
		event.preventDefault();
		dragActive = false;
		dispatch('attachFiles', { files: Array.from(event.dataTransfer.files) });
	}

	function formatBytes(size?: number): string {
		if (!size || size <= 0) return '';
		const units = ['B', 'KB', 'MB', 'GB'];
		let value = size;
		let unitIndex = 0;
		while (value >= 1024 && unitIndex < units.length - 1) {
			value /= 1024;
			unitIndex += 1;
		}
		return `${value >= 10 || unitIndex === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[unitIndex]}`;
	}
</script>


<section
	class="vibe-composer"
	class:vibe-composer--dragging={dragActive}
	aria-label="VibeDev coding request"
	style="--composer-max-width: {composerMaxWidth};"
	on:dragenter={handleDragOver}
	on:dragover={handleDragOver}
	on:dragleave={handleDragLeave}
	on:drop={handleDrop}
>
	<input
		bind:this={fileInputEl}
		class="vibe-composer__file-input"
		type="file"
		multiple
		tabindex="-1"
		aria-hidden="true"
		on:change={handleFileSelection}
	/>
	<input
		bind:this={imageInputEl}
		class="vibe-composer__file-input"
		type="file"
		accept="image/*"
		multiple
		tabindex="-1"
		aria-hidden="true"
		on:change={handleFileSelection}
	/>

	<form class="vibe-composer__shell" on:submit|preventDefault={handleSubmit}>
		<div class="vibe-composer__dock vibe-context-pill vibe-active-run">
			<button
				type="button"
				class="vibe-context-pill__identity"
				on:click={() => dispatch('openRail')}
				title="Open runs and projects drawer"
				aria-label="Open runs and projects drawer"
			>
				<span class="vibe-context-pill__project">
					<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
						<rect x="3" y="3" width="18" height="18" rx="2" ry="2"/>
						<line x1="9" y1="3" x2="9" y2="21"/>
					</svg>
					<span>{activeProjectName || 'Projects'}</span>
				</span>
				<span class="vibe-context-pill__sep">/</span>
				<span class="vibe-context-pill__run" title={activeRun ? activeRun.title : 'New run'}>
					{activeRun ? activeRun.title : 'New run'}
				</span>
				{#if activeRun?.meta?.length}
					<span class="vibe-context-pill__meta">· {activeRun.meta[0]}</span>
				{/if}
				<svg class="vibe-context-pill__chevron" width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
					<polyline points="6 9 12 15 18 9"/>
				</svg>
			</button>

			<button
				type="button"
				class="vibe-context-pill__new"
				aria-label="New run"
				on:click={() => dispatch('newRun')}
				title="Start a fresh coding run"
			>
				New run
			</button>
		</div>

		{#if studioMode === 'discuss'}
			<div class="discuss-banner" role="status">
				<span class="discuss-dot" aria-hidden="true"></span>
				<span class="discuss-text"
					><b>Discuss mode</b> — ask questions, review, and plan without modifying code files.</span
				>
			</div>
		{/if}

		{#if studioMode === 'autopilot'}
			<div class="autopilot-banner" role="status">
				<span class="autopilot-dot" aria-hidden="true"></span>
				<span class="autopilot-text"
					><b>Autopilot</b> — runs unattended on a branch, pings you when done.</span
				>
				<label class="autopilot-budget" title="Stop and report rather than spend past this (USD)">
					<span aria-hidden="true">$</span>
					<input
						type="number"
						min="0"
						step="1"
						placeholder="budget"
						value={costBudgetUsd ?? ''}
						on:input={(e) =>
							dispatch('costBudgetChange', {
								budget: Number((e.currentTarget as HTMLInputElement).value) || null
							})}
						aria-label="Cost budget in USD"
					/>
				</label>
				<label class="autopilot-nightly">
					<input
						type="checkbox"
						checked={nightly}
						on:change={(e) =>
							dispatch('nightlyChange', {
								nightly: (e.currentTarget as HTMLInputElement).checked
							})}
					/>
					<span>Run nightly</span>
				</label>
			</div>
		{/if}
		{#if stagedAttachments.length > 0}
			<div class="vibe-staged-attachments" aria-label="Attached references">
				{#each stagedAttachments as attachment (attachment.attachment_id)}
					<button
						type="button"
						class="vibe-staged-attachment"
						on:click={() =>
							dispatch('removeAttachment', { attachmentId: attachment.attachment_id })}
						aria-label={`Remove ${attachment.label || attachment.filename}`}
					>
						<span class="vibe-staged-attachment-name">
							{attachment.label || attachment.filename}
						</span>
						<span class="vibe-staged-attachment-meta">
							{attachment.mime_type}
							{#if formatBytes(attachment.size)}
								<span>&middot; {formatBytes(attachment.size)}</span>
							{/if}
						</span>
					</button>
				{/each}
			</div>
		{/if}

		<div class="composer-row">
			<MentionTextarea
				bind:this={textareaEl}
				class="vibe-input-field"
				bind:value
				{placeholder}
				minHeight={32}
				maxHeight={220}
				{mentionItems}
				mentionsEnabled={mentionItems.length > 0}
				allowSpacesInQuery={true}
				bind:mentionOpen
				bind:mentionMatches
				bind:mentionActiveIndex
				on:input={() => dispatch('input')}
				on:keydown={(e) => handleKeydown(e.detail)}
			/>

			{#if hasContent}
				<button
					type="submit"
					class="send send--revealed vibe-composer__go"
					disabled={submitDisabled}
					title={submitTitle}
					aria-label={studioMode
						? submitTitle
						: mode === 'follow_up'
							? 'Send follow-up'
							: 'Start coding'}
				>
					{#if submitting || uploading}
						<span class="vibe-composer__spinner" aria-hidden="true"></span>
					{:else}
						<svg
							width="14"
							height="14"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2.2"
							stroke-linecap="round"
							stroke-linejoin="round"
							style="transform: rotate(-45deg)"
							aria-hidden="true"
						>
							<line x1="7" y1="17" x2="17" y2="7"></line>
							<polyline points="7 7 17 7 17 17"></polyline>
						</svg>
					{/if}
				</button>
			{:else}
				<VoiceControl
					disabled={submitting}
					isUploading={uploading}
					isSending={submitting}
					on:recordingChange={(e) => (recordingActive = e.detail)}
					on:micCapture={(event) => dispatch('micCapture', event.detail)}
					on:micTranscribe={(event) => dispatch('micTranscribe', event.detail)}
					on:micTranscribeDelta={(event) => dispatch('micTranscribeDelta', event.detail)}
				/>
			{/if}
		</div>

		{#if mentionOpen}
			<div class="vibe-mention-dock">
				<MentionPicker
					matches={mentionMatches}
					activeIndex={mentionActiveIndex}
					on:select={(e) => textareaEl?.applyMention(e.detail)}
				/>
			</div>
		{/if}

		<div class="composer-tools vibe-composer__tools">
			<ComposerTalkCodeSwitch active="code" draftText={value} />

			{#if studioMode}
				<span
					class="seg"
					class:mode-discuss={studioMode === 'discuss'}
					class:mode-autopilot={studioMode === 'autopilot'}
					role="group"
					aria-label="Build, discuss, or autopilot mode"
				>
					<button
						type="button"
						class:active={studioMode === 'build'}
						on:click={() => dispatch('setStudioMode', 'build')}
						title="Build — the coding agent edits files and stages diff proposals"
					>Build</button>
					<button
						type="button"
						class:active={studioMode === 'discuss'}
						on:click={() => dispatch('setStudioMode', 'discuss')}
						title="Discuss — read-only: explain, plan, or review (no file changes)"
					>Discuss</button>
					<button
						type="button"
						class:active={studioMode === 'autopilot'}
						on:click={() => dispatch('setStudioMode', 'autopilot')}
						title="Autopilot — run unattended on a branch and ping you when done"
					>Autopilot</button>
				</span>
			{/if}

			<button
				type="button"
				class="icon-btn vibe-composer__icon-btn"
				disabled={busy}
				title="Attach files"
				aria-label="Attach files"
				on:click={openFilePicker}
			>
				<svg
					width="11"
					height="11"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="1.8"
					stroke-linecap="round"
					stroke-linejoin="round"
					aria-hidden="true"
				>
					<path
						d="M21.44 11.05l-8.49 8.49a6 6 0 0 1-8.49-8.49l8.49-8.48a4 4 0 0 1 5.66 5.65l-8.49 8.49a2 2 0 0 1-2.83-2.83l7.78-7.78"
					/>
				</svg>
			</button>
			{#if supportsImages}
				<button
					type="button"
					class="icon-btn vibe-composer__icon-btn"
					disabled={busy}
					title="Attach images"
					aria-label="Attach images"
					on:click={openImagePicker}
				>
					<svg
						width="11"
						height="11"
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						stroke-width="1.8"
						stroke-linecap="round"
						stroke-linejoin="round"
						aria-hidden="true"
					>
						<rect x="3" y="3" width="18" height="18" rx="2" ry="2" />
						<circle cx="8.5" cy="8.5" r="1.5" />
						<path d="M21 15l-5-5L5 21" />
					</svg>
				</button>
			{/if}

			<span class="spacer vibe-composer__spacer"></span>

			<!-- Super god mode (debug): start/stop from the composer, like the
			     chat composer's god-mode orb. Global; Settings shows it too. -->
			<SuperGodModeToggle />

			<CodingEnginePicker
				compact={true}
				{profiles}
				{blockedProfiles}
				{selectedProfileId}
				{selectedProfile}
				disabled={profiles.length === 0 || profileLocked}
				on:select={(event) => dispatch('selectProfile', { id: event.detail.id })}
			/>
		</div>
	</form>

	{#if submitBlocker && value.trim().length > 0}
		<p class="vibe-composer__blocker">{submitBlocker}</p>
	{/if}
	{#if !selectedProfile && profileError}
		<p class="vibe-composer__error">{profileError}</p>
	{/if}
</section>

<style>
	.vibe-composer {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 6px;
		width: 100%;
		flex: 0 0 auto;
		margin-bottom: 8px;
	}

	.vibe-composer--dragging .vibe-composer__shell {
		border-color: var(--accent, rgba(90, 140, 250, 0.7));
		box-shadow:
			0 0 0 3px color-mix(in srgb, var(--accent, rgba(90, 140, 250, 0.7)) 22%, transparent),
			var(--shadow-lg, 0 24px 48px -12px rgba(0, 0, 0, 0.25));
	}

	.vibe-composer__file-input {
		display: none;
	}

	.vibe-composer__shell {
		view-transition-name: command-ask;
		display: flex;
		flex-direction: column;
		overflow: visible;
		position: relative;
		z-index: 5;
		pointer-events: auto;
		width: min(calc(var(--composer-max-width, 900px) - 48px), calc(100% - 48px));
		background: var(--surface-glass, var(--bg-elevated, #fff));
		border: 1px solid var(--border-default, rgba(0, 0, 0, 0.18));
		border-radius: var(--radius-composer, var(--radius-lg, 14px));
		box-shadow:
			0 1px 0 rgba(255, 255, 255, 0.04) inset,
			var(--shadow-lg, 0 24px 48px -12px rgba(0, 0, 0, 0.25));
		backdrop-filter: blur(20px) saturate(140%);
		-webkit-backdrop-filter: blur(20px) saturate(140%);
		transition:
			border-color 0.18s ease,
			box-shadow 0.18s ease,
			background 0.18s ease;
	}

	.vibe-composer__shell:focus-within {
		border-color: var(--accent-primary, #c2502a);
		box-shadow:
			0 1px 0 rgba(255, 255, 255, 0.06) inset,
			var(--shadow-lg, 0 28px 56px -12px rgba(0, 0, 0, 0.32)),
			var(--input-focus-shadow, 0 0 0 4px rgba(194, 80, 42, 0.12));
	}

	.vibe-composer__dock {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
		min-width: 0;
		padding: 4px 12px 4px 14px;
		border-bottom: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
		border-top-left-radius: calc(var(--radius-composer, var(--radius-lg, 14px)) - 1px);
		border-top-right-radius: calc(var(--radius-composer, var(--radius-lg, 14px)) - 1px);
		background: transparent;
		transition: background 140ms ease;
	}

	.vibe-composer__dock:hover,
	.vibe-composer__dock:focus-within {
		background: var(--accent-primary-soft);
	}

	.vibe-context-pill__identity {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		min-width: 0;
		flex: 1;
		white-space: nowrap;
		overflow: hidden;
		background: transparent;
		border: 0;
		padding: 2px 5px;
		margin-left: -5px;
		border-radius: 5px;
		cursor: pointer;
		font-family: var(--font-primary, sans-serif);
		color: inherit;
		text-align: left;
		transition: background 140ms ease;
	}

	.vibe-context-pill__identity:hover {
		background: transparent;
	}

	.vibe-context-pill__project {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		font-family: var(--font-mono);
		font-size: 10.5px;
		font-weight: 500;
		color: var(--text-muted, #888);
		flex-shrink: 0;
	}

	.vibe-context-pill__sep {
		opacity: 0.6;
		font-weight: 400;
		font-size: 11px;
		color: var(--text-faint, #ccc);
		margin: 0 2px;
		flex-shrink: 0;
	}

	.vibe-context-pill__run {
		font-size: 11px;
		font-weight: 400;
		color: var(--text-muted, #888);
		overflow: hidden;
		text-overflow: ellipsis;
		min-width: 0;
	}

	.vibe-context-pill__identity:hover .vibe-context-pill__run {
		color: var(--text-primary, #1a1a1a);
	}

	.vibe-context-pill__meta {
		font-size: 10.5px;
		font-weight: 400;
		color: var(--text-muted, #888);
		margin-left: 4px;
		flex-shrink: 0;
	}

	.vibe-context-pill__chevron {
		opacity: 0.5;
		margin-left: 2px;
		flex-shrink: 0;
		transition: transform 140ms ease;
	}

	.vibe-context-pill__identity:hover .vibe-context-pill__chevron {
		opacity: 0.85;
		transform: translateY(1px);
	}

	.vibe-context-pill__new {
		font-family: var(--font-mono);
		font-size: 9.5px;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		padding: 2px 7px;
		border-radius: 4px;
		background: transparent;
		color: var(--text-muted, #888);
		border: 0;
		cursor: pointer;
		transition: background 120ms ease, color 120ms ease;
		flex-shrink: 0;
	}

	.vibe-context-pill__new:hover {
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
		color: var(--text-primary, #1a1a1a);
	}

	.discuss-banner,
	.autopilot-banner {
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

	.discuss-banner:first-child,
	.autopilot-banner:first-child {
		border-radius: calc(var(--radius-composer, var(--radius-lg, 14px)) - 1px) calc(var(--radius-composer, var(--radius-lg, 14px)) - 1px) 0 0;
	}

	.discuss-dot,
	.autopilot-dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: var(--accent-primary, #c2502a);
		animation: autopilot-pulse 1.8s ease-in-out infinite;
		flex-shrink: 0;
	}

	.discuss-text,
	.autopilot-text {
		flex: 1 1 auto;
		min-width: 0;
		line-height: 1.35;
	}

	.autopilot-budget {
		display: inline-flex;
		align-items: center;
		gap: 3px;
		font-size: 11px;
		font-weight: 600;
		color: var(--text-secondary);
		flex-shrink: 0;
	}

	.autopilot-budget input {
		min-width: 3.8rem;
		width: 4.8rem;
		max-width: 6.5rem;
		height: 20px;
		border: 1px solid var(--border-default, rgba(0, 0, 0, 0.18));
		border-radius: 4px;
		background: var(--bg-surface, #fff);
		color: var(--text-primary);
		font: inherit;
		font-size: 11px;
		padding: 0 4px;
	}

	.autopilot-nightly {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		font-size: 11px;
		font-weight: 500;
		color: var(--text-secondary);
		cursor: pointer;
		flex-shrink: 0;
		user-select: none;
	}

	.autopilot-nightly input {
		margin: 0;
		cursor: pointer;
		accent-color: var(--accent-primary, #c2502a);
		width: 13px;
		height: 13px;
	}

	@keyframes autopilot-pulse {
		0%, 100% { opacity: 1; }
		50% { opacity: 0.4; }
	}

	.vibe-staged-attachments {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
		padding: 6px 14px 2px;
	}

	.vibe-staged-attachment {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		padding: 3px 8px;
		border-radius: 6px;
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		font-size: 11px;
		color: var(--text-secondary, var(--vibe-text));
		cursor: pointer;
		transition: background 120ms ease, border-color 120ms ease;
	}

	.vibe-staged-attachment:hover {
		background: var(--color-error-soft, rgba(239, 68, 68, 0.1));
		border-color: color-mix(in srgb, var(--color-error, #ef4444) 40%, transparent);
		color: var(--color-error, #ef4444);
	}

	.vibe-staged-attachment-name {
		font-weight: 600;
		line-height: 1.25;
	}

	.vibe-staged-attachment-meta {
		color: var(--text-muted, var(--vibe-text-muted));
		font-size: 10px;
	}

	.composer-row {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 12px 12px 12px 14px;
	}

	.vibe-composer__shell :global(.vibe-input-field) {
		flex: 1 1 auto;
		width: 100%;
		min-width: 0;
		box-sizing: border-box;
	}

	.vibe-composer__shell :global(.vibe-input-field .chip-textarea) {
		flex: 1;
		min-width: 0;
		background: transparent;
		border: 0;
		outline: 0;
		font-family: var(--font-primary);
		font-size: 14.5px;
		line-height: 22px;
		color: var(--text-primary, #1a1a1a);
		min-height: 32px;
		max-height: 220px;
		padding: 5px 0;
		letter-spacing: -0.005em;
	}

	.vibe-composer__shell :global(.vibe-input-field .chip-textarea:focus),
	.vibe-composer__shell :global(.vibe-input-field .chip-textarea:focus-visible),
	.vibe-composer__shell :global([tabindex]:focus),
	.vibe-composer__shell :global([tabindex]:focus-visible) {
		outline: none !important;
		border: none !important;
		box-shadow: none !important;
	}

	.vibe-composer__shell :global(.vibe-input-field .chip-textarea__placeholder) {
		top: 5px;
		left: 0;
		font-family: var(--font-primary);
		font-size: 14.5px;
		line-height: 22px;
		color: var(--text-faint, #aaa);
		letter-spacing: -0.005em;
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
		background: var(--accent-fill, var(--accent-primary, #c2502a));
		transform: translateY(-1px);
	}

	.send:disabled {
		opacity: 0.5;
		cursor: not-allowed;
		transform: none;
	}

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

	.vibe-mention-dock {
		margin: 0 12px 8px 14px;
	}

	.composer-tools {
		display: flex;
		align-items: center;
		gap: 4px;
		padding: 2px 12px;
		border-top: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
		position: relative;
	}

	.spacer {
		flex: 1 1 auto;
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

	.seg button:hover:not(.active) {
		color: var(--text-primary);
	}

	.seg button.active {
		background: var(--accent-primary-soft);
		color: color-mix(in srgb, var(--accent-primary) 72%, var(--text-primary));
		font-weight: 700;
		box-shadow: 0 0 0 1px color-mix(in srgb, var(--accent-primary) 40%, transparent);
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

	.vibe-composer__spinner {
		width: 14px;
		height: 14px;
		border-radius: 999px;
		border: 2px solid currentColor;
		border-right-color: transparent;
		animation: vibe-composer-spin 0.75s linear infinite;
	}

	.vibe-composer__blocker,
	.vibe-composer__error {
		margin: -0.25rem 0 0;
		font-size: 0.78rem;
		font-weight: 600;
		text-align: center;
	}

	.vibe-composer__blocker {
		color: var(--vibe-warning, #f59e0b);
	}

	.vibe-composer__error {
		color: var(--vibe-error, #ef4444);
	}

	@keyframes vibe-composer-spin {
		to {
			transform: rotate(360deg);
		}
	}

	@media (max-width: 720px) {
		.vibe-composer__tools {
			flex-wrap: wrap;
		}

		.vibe-composer__tools :global(.engine-picker) {
			order: 10;
			width: 100%;
			max-width: none;
		}

		.spacer {
			display: none;
		}
	}
</style>
