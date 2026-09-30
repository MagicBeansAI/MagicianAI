/**
 * Explicit voice commands that enter Tutor/App Copilot from the Web surface.
 *
 * This intentionally mirrors the server's small command grammar. We do not ask
 * a voice model to infer that an ambiguous "this" authorizes screen capture:
 * users say `Tutor [Quick] screen ...`, `Tutor [Quick] blackboard ...`, or
 * `App Copilot ...`. Dictation normalizes those spoken forms before auto-send;
 * Live/Hands-free are normalized by the server before their deterministic
 * takeover.
 */

import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

export type VoiceGuidedFlowFeature = 'tutor' | 'app_copilot';
export type VoiceGuidedFlowCanvas = 'screen_overlay' | 'blackboard';

export interface VoiceGuidedFlowInvocation {
	feature: VoiceGuidedFlowFeature;
	canvas: VoiceGuidedFlowCanvas;
	quick: boolean;
	normalizedText: string;
	requiresScreenCapture: boolean;
}

export interface VoiceGuidedFlowAttachment {
	attachment_id: string;
	filename: string;
	mime_type: string;
	size: number;
	label: string;
	server_registered_capture: true;
}

export interface VoiceGuidedFlowScope {
	principal: string;
	workspace: string;
}

export function lockedVoiceGuidedFlowMessage(feature: VoiceGuidedFlowFeature): string {
	return feature === 'app_copilot'
		? 'Please unlock your screen to use App Copilot.'
		: 'Please unlock your screen to use Tutor.';
}

function scopedCaptureHeaders(scope: VoiceGuidedFlowScope): Headers {
	void scope;
	return scopedRequestHeaders({ 'Content-Type': 'application/json' });
}

export function voiceGuidedFlowCaptureStillCurrent(
	expectedSessionId: string,
	expectedDraft: string,
	currentSessionId: string | null,
	currentDraft: string
): boolean {
	return currentSessionId === expectedSessionId && currentDraft === expectedDraft;
}

interface VoiceScreenCaptureResponse {
	cancelled?: boolean;
	attachments?: Array<{
		attachment_id: string;
		stored_name: string;
		mime_type: string;
		size_bytes: number;
	}>;
}

interface VoiceScreenCaptureDiscardResponse {
	discarded_attachment_ids?: string[];
	retained_attachment_ids?: string[];
}

interface CommandToken {
	value: string;
	end: number;
}

const STARTERS = new Set(['hey', 'start', 'open', 'launch', 'use']);
const QUICK = new Set(['quick', '#quick']);

function commandTokens(text: string): CommandToken[] {
	const tokens: CommandToken[] = [];
	for (const match of text.matchAll(/[\p{L}\p{N}@#_-]+/gu)) {
		const raw = match[0];
		const start = match.index ?? 0;
		tokens.push({ value: raw.toLowerCase(), end: start + raw.length });
	}
	return tokens;
}

function requestsScreen(tokens: CommandToken[], start: number): boolean {
	const words = tokens.slice(start).map((token) => token.value);
	if (['screen', 'screenshot', 'screencap', 'screen-capture', 'display']
		.includes(words[0] ?? '')) return true;
	const action = ['take', 'capture', 'use', 'share', 'show'].includes(words[0] ?? '');
	if (!action) return false;
	const source = words[1] === 'a' ? words[2] : words[1];
	return ['screenshot', 'screen'].includes(source ?? '');
}

export function parseVoiceGuidedFlowInvocation(
	text: string
): VoiceGuidedFlowInvocation | null {
	const tokens = commandTokens(text);
	let cursor = 0;
	if (STARTERS.has(tokens[cursor]?.value ?? '')) cursor += 1;

	let quick = false;
	if (QUICK.has(tokens[cursor]?.value ?? '')) {
		quick = true;
		cursor += 1;
	}

	let feature: VoiceGuidedFlowFeature;
	const featureToken = tokens[cursor]?.value;
	if (['tutor', 'tutur', '@tutor', '@tutur'].includes(featureToken ?? '')) {
		feature = 'tutor';
		cursor += 1;
	} else if (
		['copilot', 'app-copilot', '@copilot', '@appcopilot', '@app-copilot', '@app_copilot']
			.includes(featureToken ?? '')
	) {
		feature = 'app_copilot';
		cursor += 1;
	} else if (featureToken === 'app' && tokens[cursor + 1]?.value === 'copilot') {
		feature = 'app_copilot';
		cursor += 2;
	} else {
		return null;
	}

	if (QUICK.has(tokens[cursor]?.value ?? '')) {
		quick = true;
		cursor += 1;
	}
	const hasCanonicalQuickFlag = tokens.slice(cursor).some((token) => token.value === '#quick');
	quick = quick || hasCanonicalQuickFlag;

	// Source selection is command grammar, not topic inference. Only a selector
	// immediately after `Tutor [Quick]` can authorize capture or blackboard.
	const explicitBlackboard = tokens[cursor]?.value === 'blackboard';
	const screen = feature === 'app_copilot'
		|| (!explicitBlackboard && requestsScreen(tokens, cursor));
	const canvas: VoiceGuidedFlowCanvas = screen ? 'screen_overlay' : 'blackboard';

	const commandEnd = tokens[Math.max(0, cursor - 1)]?.end ?? 0;
	const remainder = text
		.slice(commandEnd)
		.replace(/^[\s,:;-]+/u, '')
		.trim();
	let normalizedText = feature === 'app_copilot' ? '@copilot' : '@tutor';
	if (quick && !hasCanonicalQuickFlag) normalizedText += ' #quick';
	if (remainder) normalizedText += ` ${remainder}`;

	return {
		feature,
		canvas,
		quick,
		normalizedText,
		requiresScreenCapture: screen
	};
}

/**
 * Capture the host display into the exact chat session that will own the
 * dictated turn. The returned attachment carries the backend's capture
 * provenance; callers must not manufacture this flag for browser-supplied
 * files.
 */
export async function captureVoiceGuidedFlowScreen(
	sessionId: string,
	scope: VoiceGuidedFlowScope,
	fetcher: typeof fetch = fetch
): Promise<VoiceGuidedFlowAttachment[]> {
	const response = await fetcher('/api/magician/v2/screen/capture', {
		method: 'POST',
		headers: scopedCaptureHeaders(scope),
		body: JSON.stringify({ mode: 'screenshot', session_id: sessionId })
	});
	if (!response.ok) {
		const message = await response.text().catch(() => '');
		throw new Error(message || `screen capture failed (${response.status})`);
	}
	const capture = await response.json() as VoiceScreenCaptureResponse;
	if (capture.cancelled) throw new Error('Screen capture was cancelled.');
	const attachments = (capture.attachments ?? []).filter((attachment) =>
		attachment.mime_type.toLowerCase().startsWith('image/')
	);
	if (attachments.length === 0) {
		throw new Error('Screen capture completed without a usable image.');
	}
	return attachments.map((attachment) => ({
		attachment_id: attachment.attachment_id,
		filename: attachment.stored_name,
		mime_type: attachment.mime_type,
		size: attachment.size_bytes,
		label: 'voice screen capture',
		server_registered_capture: true
	}));
}

/**
 * Discard a capture when its owning draft/session changed before send. The
 * backend only removes unreferenced server-attested captures; an attachment
 * already committed or queued for chat is retained.
 */
export async function discardVoiceGuidedFlowCapture(
	sessionId: string,
	attachments: VoiceGuidedFlowAttachment[],
	scope: VoiceGuidedFlowScope,
	fetcher: typeof fetch = fetch
): Promise<void> {
	const attachmentIds = attachments.map((attachment) => attachment.attachment_id);
	if (attachmentIds.length === 0) return;
	const response = await fetcher('/api/magician/v2/screen/capture/discard', {
		method: 'POST',
		headers: scopedCaptureHeaders(scope),
		keepalive: true,
		body: JSON.stringify({ session_id: sessionId, attachment_ids: attachmentIds })
	});
	if (!response.ok) {
		const message = await response.text().catch(() => '');
		throw new Error(message || `screen capture cleanup failed (${response.status})`);
	}
	const result = await response.json() as VoiceScreenCaptureDiscardResponse;
	if ((result.retained_attachment_ids ?? []).length > 0) {
		throw new Error('The capture is already in use by a chat turn and was retained.');
	}
}
