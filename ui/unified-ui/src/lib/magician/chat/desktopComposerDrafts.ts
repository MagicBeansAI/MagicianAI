import type { UploadedAttachment } from '$lib/stores/chatStore';
import type { PlanReplyComposerState } from './planReplyState';

// Same-WebView handoff only: no draft text or attachment metadata in URLs or disk.
export const desktopComposerDrafts = new Map<string, {
	composer: PlanReplyComposerState<UploadedAttachment>;
	voiceOrigin: boolean;
}>();
