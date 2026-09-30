<script lang="ts">
	import VibeComposer from '$lib/shell/VibeComposer.svelte';
	import type { UploadedAttachment } from '$lib/stores/chatStore';
	import type { CodingProfile } from '$lib/stores/codingProfileStore';

	export let submitDisabled = false;
	export let submitBlocker: string | null = null;
	export let busy = false;
	export let showActiveRun = true;
	export let blockedProfiles: CodingProfile[] = [];

	const profiles: CodingProfile[] = [
		{
			id: 'fast',
			label: 'Fast',
			llm_profile: 'coding-fast',
			provider: 'openai',
			model: 'gpt-fast',
			supports_user_image_inputs: false,
			is_default: true
		},
		{
			id: 'deep',
			label: 'Deep',
			llm_profile: 'coding-deep',
			provider: 'openai',
			model: 'gpt-deep',
			supports_user_image_inputs: true,
			is_default: false
		}
	];

	let value = '';
	let studioMode: 'build' | 'discuss' | 'autopilot' = 'build';
	let selectedProfileId = 'fast';
	let stagedAttachments: UploadedAttachment[] = [
		{
			attachment_id: 'attachment-1',
			filename: 'architecture.md',
			mime_type: 'text/markdown',
			size: 2048
		}
	];
	let lastAction = '';

	$: selectedProfile = profiles.find((profile) => profile.id === selectedProfileId) ?? null;
</script>

<VibeComposer
	bind:value
	mode="fresh"
	{profiles}
	{blockedProfiles}
	{selectedProfileId}
	{selectedProfile}
	{stagedAttachments}
	{submitDisabled}
	{submitBlocker}
	submitting={busy}
	studioMode={studioMode}
	activeRun={showActiveRun
		? {
				label: 'Running',
				title: 'Refactor the execution panel',
				meta: ['3 files', '2m'],
				blocked: false
			}
		: null}
	on:submit={() => (lastAction = `submit:${value}`)}
	on:setStudioMode={(event) => {
		studioMode = event.detail;
		lastAction = `mode:${studioMode}`;
	}}
	on:selectProfile={(event) => {
		selectedProfileId = event.detail.id;
		lastAction = `profile:${selectedProfileId}`;
	}}
	on:attachFiles={(event) => (lastAction = `attach:${event.detail.files.length}`)}
	on:removeAttachment={(event) => {
		stagedAttachments = stagedAttachments.filter(
			(attachment) => attachment.attachment_id !== event.detail.attachmentId
		);
		lastAction = `remove:${event.detail.attachmentId}`;
	}}
	on:newRun={() => (lastAction = 'new-run')}
/>

<output data-testid="vibe-value">{value}</output>
<output data-testid="vibe-mode">{studioMode}</output>
<output data-testid="vibe-profile">{selectedProfileId}</output>
<output data-testid="vibe-action">{lastAction}</output>
