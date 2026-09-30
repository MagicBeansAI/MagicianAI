/**
 * Realtime media + control rails — TS mirror of `magician_v2::media_rails`.
 *
 * Phase 0 substrate shared by every client surface (mobile/desktop web,
 * tray, extension) that participates in the realtime media rails. Keep
 * field names in sync with the Rust structs; both ends serialise via
 * `serde(rename_all = "snake_case")`.
 */

import type { AudioStage, AudioSurface, ResolvedAudioProfile } from './audioSettings';

export type SurfaceType =
	| 'web_mobile'
	| 'web_desktop'
	| 'mascot_macos'
	| 'mascot_windows'
	| 'mascot_linux'
	| 'tray_macos'
	| 'tray_windows'
	| 'tray_linux'
	| 'extension'
	| 'unknown';

export type TransportType = 'sse' | 'websocket' | 'webrtc' | 'bridge';

export type SessionStatus = 'connected' | 'paused' | 'revoked' | 'disconnected';

export type PermissionState = 'unknown' | 'granted' | 'denied' | 'revoked';

export interface SurfaceCapabilities {
	mascot_overlay: boolean;
	text_bubble: boolean;
	browser_tts: boolean;
	provider_tts: boolean;
	realtime_voice: boolean;
	mic: boolean;
	camera: boolean;
	screen_capture: boolean;
	pointer_overlay: boolean;
	system_audio: boolean;
	desktop_action: boolean;
}

export interface MediaPermissions {
	mic: PermissionState;
	camera: PermissionState;
	screen_capture: PermissionState;
	system_audio: PermissionState;
	transcription: PermissionState;
	raw_media_persistence: PermissionState;
}

export interface RealtimeSession {
	session_id: string;
	principal: string;
	workspace: string;
	thread_id?: string | null;
	surface_type: SurfaceType;
	transport: TransportType;
	status: SessionStatus;
	capabilities: SurfaceCapabilities;
	permissions: MediaPermissions;
	created_at_ms: number;
	last_seen_at_ms: number;
	user_agent?: string | null;
	display_label?: string | null;
	audio_surface?: AudioSurface | null;
	resolved_audio_profile?: ResolvedAudioProfile | null;
}

export type AudioStageOptions = Partial<Record<AudioStage, string>>;

export function defaultCapabilities(): SurfaceCapabilities {
	return {
		mascot_overlay: false,
		text_bubble: false,
		browser_tts: false,
		provider_tts: false,
		realtime_voice: false,
		mic: false,
		camera: false,
		screen_capture: false,
		pointer_overlay: false,
		system_audio: false,
		desktop_action: false
	};
}

export function defaultPermissions(): MediaPermissions {
	return {
		mic: 'unknown',
		camera: 'unknown',
		screen_capture: 'unknown',
		system_audio: 'unknown',
		transcription: 'unknown',
		raw_media_persistence: 'unknown'
	};
}

// ─── Event types (whitelisted on the backend post endpoint) ─────────

export const MEDIA_TTS_STARTED = 'media.tts.started';
export const MEDIA_TTS_COMPLETED = 'media.tts.completed';
export const MEDIA_TTS_CANCELLED = 'media.tts.cancelled';
export const MEDIA_TTS_ERROR = 'media.tts.error';

export const MEDIA_STT_STARTED = 'media.stt.started';
export const MEDIA_TRANSCRIPT_DELTA = 'media.transcript.delta';
export const MEDIA_TRANSCRIPT_FINAL = 'media.transcript.final';
export const MEDIA_STT_ERROR = 'media.stt.error';

export const MEDIA_CAPTURE_STARTED = 'media.capture.started';
export const MEDIA_CAPTURE_COMPLETED = 'media.capture.completed';
export const MEDIA_CAPTURE_CANCELLED = 'media.capture.cancelled';
export const MEDIA_CAPTURE_ERROR = 'media.capture.error';

export const MEDIA_POINTER_COMMANDED = 'media.pointer.commanded';
export const MEDIA_ARTIFACT_CREATED = 'media.artifact.created';

export const MEDIA_MASCOT_VISIBLE = 'media.mascot.visible';
export const MEDIA_MASCOT_HIDDEN = 'media.mascot.hidden';
export const MEDIA_MASCOT_INVOKED = 'media.mascot.invoked';
export const MEDIA_MASCOT_BUBBLE_OPENED = 'media.mascot.bubble.opened';
export const MEDIA_MASCOT_BUBBLE_CLOSED = 'media.mascot.bubble.closed';
export const MEDIA_MASCOT_STATE_CHANGED = 'media.mascot.state.changed';
export const MEDIA_MASCOT_QUIET_CHANGED = 'media.mascot.quiet.changed';

export const MEDIA_PERMISSION_GRANTED = 'media.permission.granted';
export const MEDIA_PERMISSION_DENIED = 'media.permission.denied';
export const MEDIA_PERMISSION_REVOKED = 'media.permission.revoked';

export const MEDIA_VOICE_BRIDGE_CONNECTED = 'media.voice.bridge.connected';
export const MEDIA_VOICE_BRIDGE_DISCONNECTED = 'media.voice.bridge.disconnected';
export const MEDIA_VOICE_CLIENT_MESSAGE = 'media.voice.client_message';
export const MEDIA_VOICE_CLIENT_AUDIO = 'media.voice.client_audio';
export const MEDIA_VOICE_CONTROLLER_COMMAND = 'media.voice.controller_command';
export const MEDIA_VOICE_BRIDGE_ERROR = 'media.voice.bridge.error';

export type MediaEventType =
	| typeof MEDIA_TTS_STARTED
	| typeof MEDIA_TTS_COMPLETED
	| typeof MEDIA_TTS_CANCELLED
	| typeof MEDIA_TTS_ERROR
	| typeof MEDIA_STT_STARTED
	| typeof MEDIA_TRANSCRIPT_DELTA
	| typeof MEDIA_TRANSCRIPT_FINAL
	| typeof MEDIA_STT_ERROR
	| typeof MEDIA_CAPTURE_STARTED
	| typeof MEDIA_CAPTURE_COMPLETED
	| typeof MEDIA_CAPTURE_CANCELLED
	| typeof MEDIA_CAPTURE_ERROR
	| typeof MEDIA_POINTER_COMMANDED
	| typeof MEDIA_ARTIFACT_CREATED
	| typeof MEDIA_MASCOT_VISIBLE
	| typeof MEDIA_MASCOT_HIDDEN
	| typeof MEDIA_MASCOT_INVOKED
	| typeof MEDIA_MASCOT_BUBBLE_OPENED
	| typeof MEDIA_MASCOT_BUBBLE_CLOSED
	| typeof MEDIA_MASCOT_STATE_CHANGED
	| typeof MEDIA_MASCOT_QUIET_CHANGED
	| typeof MEDIA_PERMISSION_GRANTED
	| typeof MEDIA_PERMISSION_DENIED
	| typeof MEDIA_PERMISSION_REVOKED
	| typeof MEDIA_VOICE_BRIDGE_CONNECTED
	| typeof MEDIA_VOICE_BRIDGE_DISCONNECTED
	| typeof MEDIA_VOICE_CLIENT_MESSAGE
	| typeof MEDIA_VOICE_CLIENT_AUDIO
	| typeof MEDIA_VOICE_CONTROLLER_COMMAND
	| typeof MEDIA_VOICE_BRIDGE_ERROR;
