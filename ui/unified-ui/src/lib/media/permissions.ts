/**
 * Best-effort permission state for the realtime media rails.
 *
 * `navigator.permissions.query` is the authoritative read on Chromium
 * and Firefox; Safari only supports a subset. We treat any failure as
 * "unknown" — the runtime never assumes consent it hasn't seen.
 *
 * Permission *requests* are not made here. This file only reads what
 * the browser already knows. Channels that need an explicit prompt
 * (camera/mic/screen) call `getUserMedia` etc. from their own
 * components, and feed the resulting state back via `mapPromptResult`.
 */

import { browser } from '$app/environment';
import {
	defaultPermissions,
	type MediaPermissions,
	type PermissionState
} from './types';

type QueryablePermission = 'microphone' | 'camera';

function asPermissionState(state: PermissionStatus['state']): PermissionState {
	switch (state) {
		case 'granted':
			return 'granted';
		case 'denied':
			return 'denied';
		case 'prompt':
			return 'unknown';
		default:
			return 'unknown';
	}
}

async function queryOne(name: QueryablePermission): Promise<PermissionState> {
	if (!browser || !navigator.permissions || !navigator.permissions.query) {
		return 'unknown';
	}
	try {
		const status = await navigator.permissions.query({ name: name as PermissionName });
		return asPermissionState(status.state);
	} catch {
		return 'unknown';
	}
}

export async function readBrowserPermissions(): Promise<MediaPermissions> {
	const next = defaultPermissions();
	if (!browser) return next;
	const [mic, camera] = await Promise.all([queryOne('microphone'), queryOne('camera')]);
	next.mic = mic;
	next.camera = camera;
	// Screen capture, system audio, and raw_media_persistence aren't
	// queryable via `navigator.permissions` — they're decided at
	// prompt time. Stays 'unknown' until a request resolves.
	return next;
}

/**
 * Turn a getUserMedia / capture-prompt outcome into the canonical
 * permission state we track in the registry.
 */
export function mapPromptResult(result: 'granted' | 'denied' | 'dismissed' | Error): PermissionState {
	if (result === 'granted') return 'granted';
	if (result === 'denied' || result === 'dismissed') return 'denied';
	return 'unknown';
}
