import * as THREE from 'three';
import { GLTFLoader } from 'three/examples/jsm/loaders/GLTFLoader.js';
import { clone as skeletonClone } from 'three/examples/jsm/utils/SkeletonUtils.js';

/**
 * Asset registry (all CC0). A pack's models are bundled under
 * static/fleet/<pack>/models, and the pack's licenses sit one level up at the
 * pack root, beside models/ — one pack today:
 *   city — KayKit City Builder Bits
 * Assets that belong to no single pack (sky decor, shared atlases) live under
 * static/fleet/shared instead, licenses alongside — never inside a pack, so
 * retiring a pack can never take them with it.
 * Characters are pack-independent: the wardrobe is a flat static/fleet/chars
 * folder, models and license together. Loading is best-effort and cached per
 * pack/URL: a failure keeps the procedural world; it never blanks the hero.
 */

export type AssetPackId = 'city';

export interface CharacterTemplate {
	scene: THREE.Object3D;
	clips: THREE.AnimationClip[];
}

export interface ThemeAssets {
	buildings: THREE.Object3D[];
	hall: THREE.Object3D;
	armory: THREE.Object3D;
	council: THREE.Object3D;
	trees: THREE.Object3D[];
	rocks: THREE.Object3D[];
}

interface PackManifest {
	base: string;
	buildings: string[];
	hall: string;
	armory: string;
	council: string;
	trees: string[];
	rocks: string[];
}

const PACKS: Record<AssetPackId, PackManifest> = {
	city: {
		base: '/fleet/city/models',
		// building_C is held out of the guild pool: it is the Hall, and the
		// headquarters at the centre of the quad should not have a twin.
		buildings: [
			'building_A.gltf',
			'building_B.gltf',
			'building_D.gltf',
			'building_E.gltf',
			'building_F.gltf'
		],
		// The tallest model in the pack's generic-building set (2.98 units against
		// 1.65-2.97 for the rest), and rendered at a 2.6 footprint against the
		// guilds' 1.9 — so the HQ is the largest thing on the quad from every
		// angle. It replaces watertower.gltf, which read as municipal
		// infrastructure rather than a headquarters and was a fifth the size of a
		// guild building before being scaled 5x to fit. The water tower stays in
		// the pack, unreferenced: there is no ground-furniture slot to demote it
		// to without new placement code.
		hall: 'building_C.gltf',
		armory: 'building_G.gltf',
		council: 'building_H.gltf',
		trees: ['bush.gltf'],
		rocks: []
	}
};

/** Quaternius "Ultimate Animated Character Pack" people (CC0, bundled under
 * static/fleet/chars with license) — every model ships the same
 * CharacterArmature|Idle/Walk/Run/Interact/Wave/Punch clip set, so the
 * regex-based clip picker works across the whole wardrobe. */
const Q_CHARS = '/fleet/chars';
/** Campus dress code: suits, hoodies, casual, and site crew. Farmer.glb ships
 * in the same wardrobe and is deliberately left out. */
export const CAMPUS_CHARACTERS: string[] = [
	`${Q_CHARS}/BusinessMan.glb`,
	`${Q_CHARS}/Casual.glb`,
	`${Q_CHARS}/Hoodie.glb`,
	`${Q_CHARS}/Worker.glb`
];

/** Stylized cloud models — pack-agnostic sky decor (KayKit Medieval Hexagon,
 * CC0; hand-made whites), kept outside every pack so the world's clouds
 * survive a pack being retired. Best-effort like everything else: an empty
 * array keeps the procedural cumuli. */
const SHARED_ASSETS = '/fleet/shared';
const CLOUD_URLS = [`${SHARED_ASSETS}/cloud_big.gltf`, `${SHARED_ASSETS}/cloud_small.gltf`];

const loader = new GLTFLoader();
const packCache = new Map<AssetPackId, Promise<ThemeAssets | null>>();
const characterCache = new Map<string, Promise<CharacterTemplate | null>>();
let cloudCache: Promise<THREE.Object3D[]> | null = null;

export function loadCloudModels(): Promise<THREE.Object3D[]> {
	if (!cloudCache) {
		cloudCache = Promise.all(
			CLOUD_URLS.map(
				(url): Promise<THREE.Object3D | null> =>
					loader
						.loadAsync(url)
						.then((g): THREE.Object3D => g.scene)
						.catch((err) => {
							console.warn(`[fleet] cloud model unavailable: ${url}`, err);
							return null;
						})
			)
		).then((list) => list.filter((m): m is THREE.Object3D => m !== null));
	}
	return cloudCache;
}

export function loadThemeAssets(pack: AssetPackId): Promise<ThemeAssets | null> {
	let cached = packCache.get(pack);
	if (!cached) {
		cached = loadPack(PACKS[pack]).catch((err) => {
			console.warn(`[fleet] ${pack} assets unavailable — keeping procedural world`, err);
			return null;
		});
		packCache.set(pack, cached);
	}
	return cached;
}

async function loadPack(manifest: PackManifest): Promise<ThemeAssets> {
	const load = (file: string) => loader.loadAsync(`${manifest.base}/${file}`);
	const [buildings, hall, armory, council, trees, rocks] = await Promise.all([
		Promise.all(manifest.buildings.map(load)),
		load(manifest.hall),
		load(manifest.armory),
		load(manifest.council),
		Promise.all(manifest.trees.map(load)),
		Promise.all(manifest.rocks.map(load))
	]);
	return {
		buildings: buildings.map((g) => g.scene),
		hall: hall.scene,
		armory: armory.scene,
		council: council.scene,
		trees: trees.map((g) => g.scene),
		rocks: rocks.map((g) => g.scene)
	};
}

export async function loadCharacters(urls: string[]): Promise<CharacterTemplate[]> {
	const results = await Promise.all(
		urls.map((url) => {
			let cached = characterCache.get(url);
			if (!cached) {
				cached = loader
					.loadAsync(url)
					.then((g) => ({ scene: g.scene, clips: g.animations }))
					.catch((err) => {
						console.warn(`[fleet] character unavailable: ${url}`, err);
						return null;
					});
				characterCache.set(url, cached);
			}
			return cached;
		})
	);
	return results.filter((c): c is CharacterTemplate => c !== null);
}

/** Clone a template (works for static and skinned/animated models alike). */
export function cloneModel(template: THREE.Object3D): THREE.Object3D {
	return skeletonClone(template);
}

/** Bounding box at the model's RENDERED size. Must be `precise` so skinned
 * meshes are measured through their bone transforms (getVertexPosition): an
 * armature can carry a large node scale that skinning cancels at render time,
 * and the naive matrixWorld-only measure believes it — it reports a model
 * orders of magnitude too tall, which then fits microscopically (the
 * invisible-crew bug). */
function renderedBox(obj: THREE.Object3D): THREE.Box3 {
	obj.updateMatrixWorld(true);
	return new THREE.Box3().setFromObject(obj, true);
}

/**
 * Uniform-scale a model so its largest XZ extent equals `footprint` world
 * units, then sit it on the ground (bbox min.y -> 0). Returns the scale used.
 */
export function fitToFootprint(obj: THREE.Object3D, footprint: number): number {
	const box = renderedBox(obj);
	const size = box.getSize(new THREE.Vector3());
	const extent = Math.max(size.x, size.z) || 1;
	const s = footprint / extent;
	obj.scale.multiplyScalar(s);
	const box2 = renderedBox(obj);
	obj.position.y -= box2.min.y;
	return s;
}

/** Uniform-scale a model to a target height, then ground-align it. */
export function fitToHeight(obj: THREE.Object3D, height: number): number {
	const box = renderedBox(obj);
	const size = box.getSize(new THREE.Vector3());
	const s = height / (size.y || 1);
	obj.scale.multiplyScalar(s);
	const box2 = renderedBox(obj);
	obj.position.y -= box2.min.y;
	return s;
}

export function enableShadows(obj: THREE.Object3D): void {
	obj.traverse((o) => {
		const mesh = o as THREE.Mesh;
		if (mesh.isMesh) {
			mesh.castShadow = true;
			mesh.receiveShadow = false;
		}
	});
}

/** Height of a model's bounding box after any scaling (for HUD anchors). */
export function modelHeight(obj: THREE.Object3D): number {
	const box = renderedBox(obj);
	return box.getSize(new THREE.Vector3()).y;
}
