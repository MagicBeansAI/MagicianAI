/**
 * Detect what the current browser surface can actually do, on first
 * render. Cheap, synchronous, no permission prompts — the answers here
 * only describe API availability, not user consent. Permission state
 * is tracked separately in `permissions.ts`.
 */

import { browser } from '$app/environment';
import { defaultCapabilities, type SurfaceCapabilities, type SurfaceType } from './types';

export function detectSurfaceType(): SurfaceType {
	if (!browser) return 'unknown';
	const ua = navigator.userAgent || '';
	// Coarse-grain: anything matching common mobile UAs is mobile web.
	// Tray / extension surfaces never run inside the bundled SvelteKit
	// build today, so we only differentiate mobile-vs-desktop here.
	const isMobile = /Android|iPhone|iPad|iPod|Mobile/i.test(ua);
	return isMobile ? 'web_mobile' : 'web_desktop';
}

export function detectBrowserCapabilities(): SurfaceCapabilities {
	if (!browser) return defaultCapabilities();
	const caps = defaultCapabilities();
	// Browser-native TTS uses `window.speechSynthesis`. Available on
	// virtually every evergreen browser and on iOS Safari.
	caps.browser_tts = typeof window.speechSynthesis !== 'undefined';
	caps.mic =
		typeof navigator.mediaDevices !== 'undefined'
		&& typeof navigator.mediaDevices.getUserMedia === 'function';
	// `getUserMedia` covers both audio + video — camera availability
	// depends on whether the device actually has one. We optimistically
	// advertise true if the API exists; the permission flow surfaces
	// the real answer at request time.
	caps.camera = caps.mic;
	// Screen capture / pointer overlay / system audio are tray/extension
	// surface features. The browser cannot offer them.
	caps.screen_capture = false;
	caps.pointer_overlay = false;
	caps.system_audio = false;
	// Provider TTS / realtime voice come online when the backend
	// advertises a provider. Phase 3+ will flip these from feature
	// flags or capability negotiation.
	caps.provider_tts = false;
	caps.realtime_voice = false;
	return caps;
}
