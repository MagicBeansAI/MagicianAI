import { timedFetch } from '$lib/shared/fetch';

/**
 * Program-document reads for the game (guild "mission chips"), against
 * `GET /v2/programs/{name}` (magician >= 0.6.1004). Best-effort with a
 * short cache: on 404/older-binary/network failure the caller gets null and
 * the panel simply shows nothing — the chips light up once the backend runs
 * a build that serves the endpoint.
 */

export interface ProgramMission {
	title: string;
	priority: string;
}

export interface ProgramMissions {
	programTitle: string;
	/** Raw managed-section body (markdown). */
	section: string;
	/** Parsed mission bullets (empty when the section doesn't parse). */
	missions: ProgramMission[];
}

/** Parse the editor's rendered format: lines like `- **Title** (priority)`. */
export function parseMissions(section: string): ProgramMission[] {
	const out: ProgramMission[] = [];
	for (const line of section.split('\n')) {
		const m = line.trim().match(/^-\s+\*\*(.+?)\*\*\s*(?:\(([a-z]+)\))?/i);
		if (m) out.push({ title: m[1].trim(), priority: (m[2] ?? 'medium').toLowerCase() });
	}
	return out;
}

const cache = new Map<string, { at: number; value: ProgramMissions | null }>();
const CACHE_MS = 60_000;

/** Owner revert (magician >= 0.6.1007): restore the program from its newest
 * `.history/` snapshot — the backend snapshots the current content first, so
 * a revert is itself revertible. Busts the missions cache on success. */
export async function revertProgramMissions(programName: string): Promise<boolean> {
	try {
		const res = await timedFetch(
			`/api/magician/v2/programs/${encodeURIComponent(programName)}/revert`,
			{ method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}' }
		);
		if (!res.ok) return false;
		cache.delete(programName);
		return true;
	} catch {
		return false;
	}
}

export async function fetchProgramMissions(
	programName: string
): Promise<ProgramMissions | null> {
	const hit = cache.get(programName);
	if (hit && Date.now() - hit.at < CACHE_MS) return hit.value;
	let value: ProgramMissions | null = null;
	try {
		const res = await timedFetch(`/api/magician/v2/programs/${encodeURIComponent(programName)}`);
		if (res.ok) {
			const body = (await res.json()) as {
				title?: string;
				missions_section?: string | null;
			};
			const section = (body.missions_section ?? '').trim();
			if (section.length > 0) {
				value = {
					programTitle: body.title ?? programName,
					section,
					missions: parseMissions(section)
				};
			}
		}
	} catch {
		value = null;
	}
	cache.set(programName, { at: Date.now(), value });
	return value;
}
