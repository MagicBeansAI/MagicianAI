/** Transcripts are filed under this folder, next to the recording. */
export const VOICE_NOTES_DIR = 'Audio Notes';

export function isVoiceNotePath(path: string): boolean {
	const normalized = path.replaceAll('\\', '/').replace(/^\/+|\/+$/g, '');
	return normalized === VOICE_NOTES_DIR || normalized.startsWith(`${VOICE_NOTES_DIR}/`);
}
