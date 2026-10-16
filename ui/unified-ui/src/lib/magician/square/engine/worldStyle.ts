/**
 * World STYLE — the structural half of the look (the colour half is the
 * --fleet-* palette in fleet-theme.css). The world is one modern campus: the
 * village grid, roads, A* and behaviours are fixed, and these knobs pick the
 * geometry idioms and material moods the builder renders.
 *
 * The idiom fields below each carry ONE member, the campus's own, and the
 * builders render it unconditionally — the alternatives the four retired themes
 * selected (medieval cottages, sci-fi spires and energy walls, office cubicle
 * partitions, data-avatar citizens) are gone from civWorld.ts and citizens.ts
 * along with their geometry. Re-widening a union here is therefore a request
 * for a branch that no longer exists: a second look means writing its geometry
 * back, not just adding a member.
 *
 * `assetPack` is the exception and stays nullable on purpose. Model packs load
 * best-effort, so the procedural rendering of these same idioms is a supported
 * degraded state, not dead code.
 */

import type { AssetPackId } from './assets';

export interface WorldStyle {
	/** Bundled model pack for the world (null = fully procedural world). */
	assetPack: AssetPackId | null;
	/** Guild/hall structure idiom: a flat-roofed slab block. */
	building: 'flat';
	/** Base guild building height (hall scales up from it). */
	buildingHeight: number;
	/** Vary tower heights per guild (city skylines). */
	heightVariance: boolean;
	/** Lets structures light themselves independently of the sun. Campus sets
	 * 0, so every material this feeds currently resolves to no glow. */
	buildingEmissive: number;
	/** Vegetation idiom: trimmed spheres on a trunk. */
	treeStyle: 'round';
	/** Prop rendered at each guild work spot (the bench itself). */
	benchProp: 'desk';
	/** Citizens render as animated characters. */
	citizenMode: 'character';
	/** Work-animation preference (regex, tried in order against the character's
	 * clips) so a working citizen mimes plausible campus work instead of
	 * falling back to a generic clip. */
	workClips: RegExp[];
	/** Realm border idiom: a greenbelt hedge around the campus. */
	borderStyle: 'hedge';
}

export const CAMPUS_STYLE: WorldStyle = {
	assetPack: 'city',
	building: 'flat',
	buildingHeight: 1.4,
	heightVariance: true,
	buildingEmissive: 0,
	treeStyle: 'round',
	benchProp: 'desk',
	citizenMode: 'character',
	workClips: [/interact/i, /use_item/i, /pickup/i],
	borderStyle: 'hedge'
};
