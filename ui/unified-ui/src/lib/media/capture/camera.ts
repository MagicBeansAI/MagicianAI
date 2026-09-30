/**
 * Camera capture helpers (Phase 2).
 *
 * We deliberately use the legacy `<input type="file" accept="image/*"
 * capture="environment">` pattern instead of `getUserMedia` because it
 * works in every browser without permission prompts mid-conversation,
 * triggers the system camera UI on iOS/Android, and gracefully falls
 * back to a file picker on desktop. Camera UX in `getUserMedia` is
 * preview + manual capture button — that's heavier than this phase
 * needs.
 *
 * The component owns the lifecycle event emission; this file is only
 * a thin helper for capability detection and file-name normalisation.
 */

import { browser } from '$app/environment';

export function isCameraCaptureSupported(): boolean {
	if (!browser) return false;
	// Every evergreen browser supports `<input type="file">`. The
	// `capture` attribute is only honored on mobile — desktops fall
	// back to a file picker, which is the desired behaviour anyway.
	return typeof document.createElement('input') !== 'undefined';
}

export function makeCapturedImageFile(blob: Blob, mimeType?: string): File {
	const finalType = mimeType ?? blob.type ?? 'image/jpeg';
	const ext = mimeToExt(finalType);
	const stamp = new Date().toISOString().replace(/[:.]/g, '-');
	return new File([blob], `capture-${stamp}.${ext}`, { type: finalType });
}

function mimeToExt(mime: string): string {
	const normalized = mime.toLowerCase();
	if (normalized.includes('png')) return 'png';
	if (normalized.includes('webp')) return 'webp';
	if (normalized.includes('gif')) return 'gif';
	if (normalized.includes('heic') || normalized.includes('heif')) return 'heic';
	return 'jpg';
}
