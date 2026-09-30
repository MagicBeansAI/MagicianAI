<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import VoiceControl from '$lib/magician/components/chat/VoiceControl.svelte';
	import MentionTextarea from '$lib/magician/chat/MentionTextarea.svelte';
	import MentionPicker from '$lib/magician/chat/MentionPicker.svelte';
	import type { ComposerMentionItem } from '$lib/magician/chat/composerMentions';
	import type { ChipFieldToken } from '$lib/magician/chat/chipMarkup';
	import type { UploadedAttachment } from '$lib/stores/chatStore';
	import {
		codingProfileOptionLabel,
		groupCodingProfilesByEngine,
		type CodingProfile
	} from '$lib/stores/codingProfileStore';

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

	const dispatch = createEventDispatcher<{
		submit: void;
		setStudioMode: 'build' | 'discuss' | 'autopilot';
		selectProfile: { id: string };
		attachFiles: { files: File[] };
		removeAttachment: { attachmentId: string };
		newRun: void;
		input: void;
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

	$: busy = submitting || uploading || preparingSession;
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

	function handleProfileChange(event: Event): void {
		const id = (event.currentTarget as HTMLSelectElement).value;
		if (id) dispatch('selectProfile', { id });
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
	on:dragenter={handleDragOver}
	on:dragover={handleDragOver}
	on:dragleave={handleDragLeave}
	on:drop={handleDrop}
>
	{#if activeRun}
		<div class="vibe-active-run" class:vibe-active-run--blocked={activeRun.blocked}>
			<div>
				<div class="vibe-active-run__label">{activeRun.label}</div>
				<div class="vibe-active-run__title">{activeRun.title}</div>
				{#if activeRun.meta.length > 0}
					<div class="vibe-active-run__meta">
						{#each activeRun.meta as item}
							<span>{item}</span>
						{/each}
					</div>
				{/if}
			</div>
			<button type="button" class="vibe-composer__small-btn" on:click={() => dispatch('newRun')}>
				New run
			</button>
		</div>
	{/if}

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

		<MentionTextarea
			bind:this={textareaEl}
			class="vibe-input-field"
			bind:value
			{placeholder}
			minHeight={112}
			maxHeight={288}
			{mentionItems}
			mentionsEnabled={mentionItems.length > 0}
			allowSpacesInQuery={true}
			bind:mentionOpen
			bind:mentionMatches
			bind:mentionActiveIndex
			on:input={() => dispatch('input')}
			on:keydown={(e) => handleKeydown(e.detail)}
		/>

		<div class="vibe-composer__tools">
			<button
				type="button"
				class="vibe-composer__icon-btn"
				disabled={busy}
				title="Attach files"
				aria-label="Attach files"
				on:click={openFilePicker}
			>
				<svg
					width="15"
					height="15"
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
					class="vibe-composer__icon-btn"
					disabled={busy}
					title="Attach images"
					aria-label="Attach images"
					on:click={openImagePicker}
				>
					<svg
						width="15"
						height="15"
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

			<label class="vibe-composer__profile" title={selectedProfile?.description ?? selectedProfile?.model ?? 'Coding profile'}>
				<select
					value={selectedProfileId ?? ''}
					disabled={profiles.length === 0 || profileLocked}
					on:change={handleProfileChange}
					aria-label="Coding profile"
				>
					{#each groupCodingProfilesByEngine([...profiles, ...blockedProfiles]) as group (group.label ?? '')}
						{#if group.label}
							<optgroup label={group.label}>
								{#each group.profiles as profile (profile.id)}
									<option
								value={profile.id}
								disabled={profile.selectable === false}
								title={profile.selectable === false ? (profile.reason ?? undefined) : undefined}
							>{codingProfileOptionLabel(profile)}</option>
								{/each}
							</optgroup>
						{:else}
							{#each group.profiles as profile (profile.id)}
								<option
								value={profile.id}
								disabled={profile.selectable === false}
								title={profile.selectable === false ? (profile.reason ?? undefined) : undefined}
							>{codingProfileOptionLabel(profile)}</option>
							{/each}
						{/if}
					{/each}
				</select>
			</label>

			<span class="vibe-composer__spacer"></span>

			{#if studioMode}
				<div
					class="vibe-composer__mode"
					class:mode-discuss={studioMode === 'discuss'}
					class:mode-autopilot={studioMode === 'autopilot'}
					role="group"
					aria-label="Build, discuss, or autopilot mode"
				>
					<button
						type="button"
						class:active={studioMode === 'build'}
						on:click={() => dispatch('setStudioMode', 'build')}
						title="Build — the coding agent edits files and stages diff proposals">Build</button
					>
					<button
						type="button"
						class:active={studioMode === 'discuss'}
						on:click={() => dispatch('setStudioMode', 'discuss')}
						title="Discuss — read-only: explain, plan, or review (no file changes)">Discuss</button
					>
					<button
						type="button"
						class:active={studioMode === 'autopilot'}
						on:click={() => dispatch('setStudioMode', 'autopilot')}
						title="Autopilot — run unattended on a branch and ping you when done">Autopilot</button
					>
				</div>
			{/if}

			<VoiceControl
				disabled={submitting}
				isUploading={uploading}
				isSending={submitting}
				on:micCapture={(event) => dispatch('micCapture', event.detail)}
				on:micTranscribe={(event) => dispatch('micTranscribe', event.detail)}
				on:micTranscribeDelta={(event) => dispatch('micTranscribeDelta', event.detail)}
			/>
			<button
				type="submit"
				class="vibe-composer__go"
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
						width="16"
						height="16"
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						stroke-width="2"
						stroke-linecap="round"
						stroke-linejoin="round"
						aria-hidden="true"
					>
						<path d="M5 12h14" />
						<path d="M13 6l6 6-6 6" />
					</svg>
				{/if}
			</button>
		</div>
	</form>

	{#if mentionOpen}
		<div class="vibe-mention-dock">
			<MentionPicker
				matches={mentionMatches}
				activeIndex={mentionActiveIndex}
				on:select={(e) => textareaEl?.applyMention(e.detail)}
			/>
		</div>
	{/if}

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
		gap: 0.75rem;
	}

	.vibe-composer--dragging .vibe-composer__shell {
		border-color: color-mix(in srgb, var(--vibe-accent) 68%, var(--vibe-border));
		background: color-mix(in srgb, var(--vibe-accent) 6%, var(--vibe-surface));
		box-shadow:
			var(--shadow-sm, 0 1px 2px color-mix(in srgb, var(--vibe-text) 10%, transparent)),
			0 0 0 3px color-mix(in srgb, var(--vibe-accent) 14%, transparent);
	}

	.vibe-composer__file-input {
		display: none;
	}

	.vibe-active-run {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.85rem;
		border: 1px solid color-mix(in srgb, var(--vibe-accent) 32%, var(--vibe-border));
		border-radius: 8px;
		background: color-mix(in srgb, var(--vibe-accent) 6%, var(--vibe-surface));
		padding: 0.72rem;
	}

	.vibe-active-run--blocked {
		border-color: color-mix(in srgb, var(--vibe-warning) 42%, var(--vibe-border));
		background: color-mix(in srgb, var(--vibe-warning) 7%, var(--vibe-surface));
	}

	.vibe-active-run__label {
		color: var(--vibe-accent);
		font-size: 0.7rem;
		font-weight: 900;
		line-height: 1.2;
		text-transform: uppercase;
	}

	.vibe-active-run__title {
		margin-top: 0.18rem;
		font-size: 0.9rem;
		font-weight: 850;
		line-height: 1.25;
		overflow-wrap: anywhere;
	}

	.vibe-active-run__meta {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
		margin-top: 0.28rem;
		color: var(--vibe-text-muted);
		font-size: 0.72rem;
		font-weight: 800;
		overflow-wrap: anywhere;
	}

	.vibe-composer__shell {
		display: flex;
		flex-direction: column;
		overflow: visible;
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-lg, 14px);
		background: var(--bg-elevated, var(--vibe-surface));
		box-shadow:
			0 1px 0 color-mix(in srgb, var(--vibe-surface) 55%, transparent) inset,
			var(--shadow-lg, 0 18px 36px -18px color-mix(in srgb, var(--vibe-text) 34%, transparent));
		backdrop-filter: blur(20px) saturate(140%);
		-webkit-backdrop-filter: blur(20px) saturate(140%);
		transition:
			border-color 0.16s ease,
			box-shadow 0.16s ease,
			background 0.16s ease;
	}

	.vibe-composer__shell:focus-within {
		border-color: var(--vibe-accent);
		box-shadow:
			0 1px 0 color-mix(in srgb, var(--vibe-surface) 70%, transparent) inset,
			var(--shadow-lg, 0 24px 48px -18px color-mix(in srgb, var(--vibe-text) 38%, transparent)),
			0 0 0 4px color-mix(in srgb, var(--vibe-accent) 12%, transparent);
	}

	.vibe-staged-attachments {
		display: flex;
		flex-wrap: wrap;
		gap: 0.45rem;
		padding: 0.7rem 0.75rem 0;
	}

	.vibe-staged-attachment {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.12rem;
		min-width: min(14rem, 100%);
		max-width: 100%;
		border: 1px solid var(--vibe-border);
		border-radius: 8px;
		background: color-mix(in srgb, var(--vibe-page-surface) 70%, var(--vibe-surface));
		color: var(--vibe-text);
		padding: 0.5rem 0.65rem;
		text-align: left;
		cursor: pointer;
	}

	.vibe-staged-attachment:hover {
		border-color: var(--vibe-accent);
	}

	.vibe-staged-attachment-name {
		max-width: 100%;
		font-size: 0.78rem;
		font-weight: 800;
		line-height: 1.25;
		overflow-wrap: anywhere;
	}

	.vibe-staged-attachment-meta {
		color: var(--vibe-text-muted);
		font-size: 0.7rem;
		line-height: 1.2;
		overflow-wrap: anywhere;
	}

	/* The standardized MentionTextarea replaces the raw <textarea>. The `class`
	   lands on the outer `.chip-textarea-wrap`; restyle the contenteditable + its
	   placeholder to keep the old composer look (padding/font). Sizing is
	   content-driven autosize between minHeight/maxHeight (7rem–18rem) in place of
	   the old manual drag handle. */
	.vibe-composer__shell :global(.vibe-input-field) {
		flex: 0 0 auto;
		width: 100%;
		box-sizing: border-box;
	}

	.vibe-composer__shell :global(.vibe-input-field .chip-textarea) {
		padding: 0.9rem 0.95rem;
		font-family: inherit;
		font-size: 0.95rem;
		line-height: 1.45;
		color: var(--input-text, var(--vibe-text));
	}

	.vibe-composer__shell :global(.vibe-input-field .chip-textarea__placeholder) {
		top: 0.9rem;
		left: 0.95rem;
		font-family: inherit;
		font-size: 0.95rem;
		line-height: 1.45;
		color: color-mix(in srgb, var(--vibe-text-muted) 82%, transparent);
	}

	.vibe-mention-dock {
		margin: 0 0.65rem 0.5rem;
	}

	.vibe-composer__tools {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		padding: 0.45rem 0.65rem 0.6rem;
		border-top: 1px solid var(--border-soft, color-mix(in srgb, var(--vibe-border) 82%, transparent));
		min-width: 0;
	}

	.vibe-composer__profile {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		min-width: min(12rem, 34vw);
		max-width: min(18rem, 42vw);
		border-radius: 7px;
		background: var(--bg-soft, color-mix(in srgb, var(--vibe-text) 5%, transparent));
		padding: 0.12rem 0.22rem;
		color: var(--vibe-text-muted);
		font-size: 0.72rem;
		font-weight: 850;
	}

	.vibe-composer__profile select {
		min-width: 0;
		flex: 1;
		height: 1.5rem;
		/* The global select chrome pads 0.42rem top and bottom; inside this
		   fixed 1.5rem box that clipped the label and pushed it off-centre.
		   Vertical padding goes; line-height centres the text; the right
		   padding keeps the global chevron clear of the label. */
		padding: 0 1.45rem 0 0.4rem;
		line-height: 1.5rem;
		border: 0;
		border-radius: 5px;
		background-color: transparent;
		background-position: right 0.4rem center;
		color: var(--vibe-text);
		font: inherit;
		font-size: 0.76rem;
		font-weight: 800;
		outline: none;
	}

	.vibe-composer__profile select:disabled {
		cursor: not-allowed;
		opacity: 0.62;
	}

	.vibe-composer__spacer {
		flex: 1 1 auto;
		min-width: 0.4rem;
	}

	.vibe-composer__icon-btn,
	.vibe-composer__go,
	.vibe-composer__small-btn {
		display: inline-grid;
		place-items: center;
		border: 0;
		border-radius: 8px;
		font: inherit;
		cursor: pointer;
		transition:
			background 0.15s ease,
			color 0.15s ease,
			transform 0.15s ease,
			opacity 0.15s ease;
	}

	.vibe-composer__icon-btn {
		width: 1.9rem;
		height: 1.9rem;
		background: transparent;
		color: var(--vibe-text-muted);
	}

	.vibe-composer__icon-btn:hover:not(:disabled) {
		background: var(--bg-soft, color-mix(in srgb, var(--vibe-text) 7%, transparent));
		color: var(--vibe-accent);
	}

	.vibe-composer__go {
		width: 2rem;
		height: 2rem;
		background: var(--button-primary-bg, var(--vibe-text));
		color: var(--button-primary-color, var(--vibe-surface));
	}

	.vibe-composer__go:hover:not(:disabled) {
		background: var(--vibe-accent);
		transform: translateY(-1px);
	}
	/* Matches the chat composer's Do | Plan `.seg` radio (mono, uppercase, soft
	   inset, solid active), with a per-mode active colour. */
	.vibe-composer__mode {
		display: inline-flex;
		background: var(--bg-soft, color-mix(in srgb, var(--vibe-text) 5%, transparent));
		border-radius: 6px;
		padding: 2px;
	}
	.vibe-composer__mode button {
		padding: 3px 8px 2px;
		font-family: var(--font-mono);
		font-size: 10px;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--vibe-text-muted, #888);
		background: transparent;
		border: 0;
		border-radius: 4px;
		cursor: pointer;
		transition:
			background 0.12s ease,
			color 0.12s ease;
	}
	.vibe-composer__mode button:hover {
		color: var(--vibe-text);
	}
	.vibe-composer__mode button.active {
		background: var(--vibe-text, #1a1a1a);
		color: var(--vibe-surface, #fff);
	}
	/* Discuss = read-only / plan → accent (parity with the chat's Plan mode). */
	.vibe-composer__mode.mode-discuss button.active {
		background: var(--vibe-accent);
		color: var(--button-primary-color, #fff);
	}
	/* Autopilot = autonomous → success green (matches the ✈ banner). */
	.vibe-composer__mode.mode-autopilot button.active {
		background: var(--color-success, #2f9e6f);
		color: #fff;
	}

	.vibe-composer__small-btn {
		min-height: 2rem;
		border: 1px solid var(--vibe-border-strong);
		background: transparent;
		color: var(--vibe-text);
		padding: 0.38rem 0.65rem;
		font-size: 0.78rem;
		font-weight: 800;
	}

	.vibe-composer__small-btn:hover {
		border-color: var(--vibe-accent);
		color: var(--vibe-accent);
	}

	.vibe-composer__icon-btn:disabled,
	.vibe-composer__go:disabled,
	.vibe-composer__small-btn:disabled {
		cursor: not-allowed;
		opacity: 0.5;
		transform: none;
	}

	.vibe-composer__icon-btn:focus-visible,
	.vibe-composer__go:focus-visible,
	.vibe-composer__small-btn:focus-visible,
	.vibe-composer__profile select:focus-visible,
	.vibe-staged-attachment:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--vibe-accent) 68%, transparent);
		outline-offset: 2px;
	}

	.vibe-composer__spinner {
		width: 0.85rem;
		height: 0.85rem;
		border-radius: 999px;
		border: 2px solid currentColor;
		border-right-color: transparent;
		animation: vibe-composer-spin 0.75s linear infinite;
	}

	.vibe-composer__blocker,
	.vibe-composer__error {
		margin: -0.25rem 0 0;
		font-size: 0.78rem;
		font-weight: 800;
	}

	.vibe-composer__blocker {
		color: var(--vibe-warning);
	}

	.vibe-composer__error {
		color: var(--vibe-error);
	}

	.vibe-composer__tools :global(.voice-trigger) {
		width: 1.9rem;
		height: 1.9rem;
		min-width: 1.9rem;
		min-height: 1.9rem;
		border-radius: 8px;
		background: transparent;
		color: var(--vibe-text-muted);
	}

	.vibe-composer__tools :global(.voice-trigger:hover) {
		background: var(--bg-soft, color-mix(in srgb, var(--vibe-text) 7%, transparent));
		color: var(--vibe-accent);
	}

	@keyframes vibe-composer-spin {
		to {
			transform: rotate(360deg);
		}
	}

	@media (max-width: 720px) {
		.vibe-active-run {
			align-items: stretch;
			flex-direction: column;
		}

		.vibe-composer__tools {
			flex-wrap: wrap;
		}

		.vibe-composer__profile {
			order: 10;
			width: 100%;
			max-width: none;
		}

		.vibe-composer__spacer {
			display: none;
		}
	}
</style>
