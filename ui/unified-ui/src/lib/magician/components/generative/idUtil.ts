/**
 * Build deterministic DOM IDs from an arbitrary logical ID.
 *
 * Uses two parts:
 * 1) Human-readable normalized slug
 * 2) Compact encoded fingerprint with full-input hash for collision resistance
 */
const MAX_SLUG_LENGTH = 64;
const ENCODE_EDGE_CHARS = 12;

export function normalizeIdPart(value: string): string {
	const normalized = value
		.toLowerCase()
		.replace(/[^a-z0-9_-]/g, '-')
		.replace(/-+/g, '-')
		.replace(/^-|-$/g, '');
	const safe = normalized || 'default';
	return safe.length > MAX_SLUG_LENGTH ? safe.slice(0, MAX_SLUG_LENGTH) : safe;
}

function encodeUtf16Hex(value: string): string {
	let out = '';
	for (let i = 0; i < value.length; i++) {
		out += value.charCodeAt(i).toString(16).padStart(4, '0');
	}
	return out;
}

function hashIdPart(value: string): string {
	// FNV-1a 32-bit hash (deterministic, inexpensive).
	let hash = 0x811c9dc5;
	for (let i = 0; i < value.length; i++) {
		hash ^= value.charCodeAt(i);
		hash = Math.imul(hash, 0x01000193);
	}
	return (hash >>> 0).toString(16).padStart(8, '0');
}

function encodeIdPart(value: string): string {
	if (!value) return '0000l0000h00000000';
	const lenHex = value.length.toString(16).padStart(4, '0');
	if (value.length <= ENCODE_EDGE_CHARS * 2) {
		return `${encodeUtf16Hex(value)}l${lenHex}h${hashIdPart(value)}`;
	}
	const head = value.slice(0, ENCODE_EDGE_CHARS);
	const tail = value.slice(-ENCODE_EDGE_CHARS);
	return `${encodeUtf16Hex(head)}x${encodeUtf16Hex(tail)}l${lenHex}h${hashIdPart(value)}`;
}

export function buildStableDomId(prefix: string, idBase: string): string {
	const source = idBase || 'default';
	return `${prefix}-${normalizeIdPart(source)}-${encodeIdPart(source)}`;
}
