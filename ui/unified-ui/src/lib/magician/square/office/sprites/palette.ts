/**
 * Office sprite palette and character casting.
 *
 * TWO PALETTES, ON PURPOSE, and the split is the whole theming story:
 *
 *   - ARCHITECTURE AND FURNITURE are theme-toned. Their colours are the
 *     `--office-*` tokens in office-theme.css, which resolve against the app's
 *     light/dark tone (see officeTone.ts). A cream floor in a dark theme would
 *     be a slab of daylight in a dark page.
 *   - PEOPLE are not. Skin, hair and clothing are fixed values declared here.
 *     A person's skin tone is not a surface, and re-tinting it per theme would
 *     both look wrong and quietly turn the crew's diversity into a function of
 *     which theme the owner picked. The values below are chosen to hold
 *     against both office floors.
 *
 * Status colours are NEITHER: they alias the shared `--fleet-*` tokens, which
 * the HUD also paints from. See the note at the top of fleet-theme.css before
 * touching them — retuning one here would repaint the crew leaderboard.
 */

import type { CitizenVM, Vibe } from '../../engine/types';
import { planHash } from '../floorPlan';

/** Five skin tones, spaced across the range rather than clustered. */
export const SKIN_TONES = ['#f6d3b4', '#eab894', '#cf9469', '#a36a43', '#6f4529'] as const;
/** A slightly darker partner tone for the ear/neck shadow on each skin. */
export const SKIN_SHADE = ['#e0b492', '#d19b74', '#b57a4f', '#8a5432', '#57341e'] as const;

export const HAIR_COLOURS = [
	'#2b2118', // near-black
	'#4a3524', // dark brown
	'#7a5230', // brown
	'#b5793c', // auburn
	'#d9b25f', // blonde
	'#8e8e96', // grey
	'#3f2d4e', // blue-black
	'#a63f3f' // dyed red
] as const;

/** Office-appropriate clothing, warm and cool mixed so a room of five reads as
 * five people rather than a uniform. */
export const OUTFIT_COLOURS = [
	'#4f77b8',
	'#c9564f',
	'#3f9a78',
	'#d18b3c',
	'#7c5fb5',
	'#2f6f86',
	'#b8566f',
	'#5b7a3f',
	'#8a6b4e',
	'#576274'
] as const;

export type HairStyle =
	| 'short'
	| 'buzz'
	| 'sidepart'
	| 'curly'
	| 'long'
	| 'bob'
	| 'bun'
	| 'ponytail';

/** Female- and male-presenting hair sets. Both sets are drawn by the same
 * component; casting only decides which list to draw from. */
export const HAIR_FEMME: HairStyle[] = ['long', 'bob', 'bun', 'ponytail', 'curly'];
export const HAIR_MASC: HairStyle[] = ['short', 'buzz', 'sidepart', 'curly'];

export type Gender = 'f' | 'm';

export interface PersonLook {
	gender: Gender;
	skin: string;
	skinShade: string;
	hair: string;
	hairStyle: HairStyle;
	outfit: string;
	/** A collared shirt under the top, drawn for the suited roles. */
	collar: boolean;
	glasses: boolean;
	/** ⭐ primary, ✨ envoy, ♛ CEO — the same three the HUD singles out. */
	badge: 'primary' | 'envoy' | 'ceo' | null;
}

/**
 * Cast a crew member's appearance from their id.
 *
 * Deterministic and id-derived so a person keeps their face across polls,
 * reloads and both view modes. Four independent draws off one hash (gender,
 * skin, hair, outfit) rather than one index into a table of premade
 * characters, which is what keeps an eleven-person floor from looking like a
 * repeated sprite sheet.
 */
export function personLookOf(citizen: Pick<CitizenVM, 'id' | 'isPrimary' | 'isEnvoy' | 'isCeo'>): PersonLook {
	const h = planHash(citizen.id);
	const gender: Gender = ((h >>> 3) & 1) === 0 ? 'f' : 'm';
	const skinIndex = (h >>> 5) % SKIN_TONES.length;
	const styles = gender === 'f' ? HAIR_FEMME : HAIR_MASC;
	const badge = citizen.isPrimary ? 'primary' : citizen.isCeo ? 'ceo' : citizen.isEnvoy ? 'envoy' : null;
	return {
		gender,
		skin: SKIN_TONES[skinIndex],
		skinShade: SKIN_SHADE[skinIndex],
		hair: HAIR_COLOURS[(h >>> 9) % HAIR_COLOURS.length],
		hairStyle: styles[(h >>> 13) % styles.length],
		outfit: OUTFIT_COLOURS[(h >>> 17) % OUTFIT_COLOURS.length],
		collar: badge !== null || ((h >>> 21) & 3) === 0,
		glasses: ((h >>> 23) & 3) === 0,
		badge
	};
}

/** Shared with the HUD — never a local colour. */
export const STATUS_COLOUR: Record<Vibe, string> = {
	working: 'var(--fleet-working, #20773d)',
	needs: 'var(--fleet-needs, #ae6500)',
	paused: 'var(--fleet-paused, #8b5cf6)',
	idle: 'var(--fleet-idle, #0078a4)',
	offline: 'var(--fleet-offline, #4f5860)'
};
