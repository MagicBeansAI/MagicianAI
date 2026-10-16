import * as THREE from 'three';
import type { WorldPalette, GuildVM } from './types';
import { guildHue } from './palette';
import { tileToWorld, idx, type Village, type TileKind } from './grid';
import { cloneModel, fitToFootprint, enableShadows, modelHeight, type ThemeAssets } from './assets';
import type { WorldStyle } from './worldStyle';

/**
 * World builder. One village grid, one look — the modern campus. Where the
 * bundled model pack supplies a model (buildings, landmarks, trees) it is
 * placed; everything else, and everything when a pack fails to load, is built
 * procedurally from the palette (fleet-theme.css) + the WorldStyle knobs
 * (worldStyle.ts): flat slab structures, heights, emissive glow, round trees,
 * and a hedge border. Terrain tiles are always procedural.
 *
 * The procedural half is NOT a fallback that can be dropped: packs load
 * best-effort, so it is the world the user sees whenever a model 404s.
 */

export interface FadeableStructure {
	/** Same key space as pick targets: guild:<id> / landmark:<id>. */
	key: string;
	mats: THREE.Material[];
}

export interface WorldHandles {
	group: THREE.Group;
	/** Pickable building meshes; userData.guildId is set. */
	buildingPickMeshes: THREE.Mesh[];
	/** Guild id -> world position of the building centre (HUD anchors). */
	buildingCenterOf: Map<string, THREE.Vector3>;
	/** Pickable civic-landmark meshes; userData.landmarkId is set. */
	landmarkPickMeshes: THREE.Mesh[];
	/** Landmark id -> world position of the structure centre. */
	landmarkCenterOf: Map<string, THREE.Vector3>;
	/** Structures the engine may fade translucent when they occlude citizens. */
	fadeables: FadeableStructure[];
	dispose: () => void;
}

function disposeObject(root: THREE.Object3D): void {
	root.traverse((obj) => {
		const mesh = obj as THREE.Mesh;
		if (mesh.isMesh && mesh.userData.sharedWithTemplate !== true) {
			mesh.geometry?.dispose();
			const mat = mesh.material as THREE.Material | THREE.Material[] | undefined;
			if (Array.isArray(mat)) mat.forEach((m) => m.dispose());
			else if (mat) mat.dispose();
		}
	});
}

/** Mark every mesh in a cloned model as template-shared (skip disposal). */
function markShared(root: THREE.Object3D): void {
	root.traverse((o) => {
		const mesh = o as THREE.Mesh;
		if (mesh.isMesh) mesh.userData.sharedWithTemplate = true;
	});
}

function collectTiles(village: Village, kind: TileKind): { x: number; z: number }[] {
	const out: { x: number; z: number }[] = [];
	for (let z = 0; z < village.n; z++)
		for (let x = 0; x < village.n; x++)
			if (village.kinds[idx(village.n, x, z)] === kind) out.push({ x, z });
	return out;
}

/** Invisible-but-raycastable box proxy so a whole model picks as one target. */
function pickProxy(w: number, h: number, d: number, x: number, y: number, z: number): THREE.Mesh {
	const proxy = new THREE.Mesh(
		new THREE.BoxGeometry(w, h, d),
		new THREE.MeshBasicMaterial({ transparent: true, opacity: 0, depthWrite: false })
	);
	proxy.position.set(x, y, z);
	return proxy;
}

/**
 * Face a structure squarely down one of the four cardinal directions.
 *
 * The raw centre→door bearing is NOT square: a lot's door sits above one half
 * of its 2x2 footprint rather than centred on it, so the bearing lands ~18° off
 * true. That skew read as organic on the old radial layout, which had no
 * reference line — against a lattice of straight avenues, every building is
 * visibly cocked, and the Hall (whose rotation is a hardcoded half-turn) is the
 * only square one, which makes the rest look wrong rather than deliberate.
 */
function faceCardinal(dx: number, dz: number): number {
	const quarterTurn = Math.PI / 2;
	return Math.round(Math.atan2(dx, dz) / quarterTurn) * quarterTurn;
}

/**
 * Register a structure for occlusion-fading. Materials must be unique per
 * structure to fade independently: model clones share the template's
 * materials, so those are CLONED here (cloneMats=true; disposed via
 * fadeables in dispose()); procedural structures already own theirs.
 */
function collectFadeable(
	root: THREE.Object3D,
	key: string,
	cloneMats: boolean,
	fadeables: FadeableStructure[]
): void {
	const mats: THREE.Material[] = [];
	root.traverse((o) => {
		const mesh = o as THREE.Mesh;
		if (!mesh.isMesh || !mesh.material) return;
		let mat = mesh.material;
		if (cloneMats) {
			mat = Array.isArray(mat) ? mat.map((m) => m.clone()) : mat.clone();
			mesh.material = mat;
		}
		for (const m of Array.isArray(mat) ? mat : [mat]) {
			m.transparent = true;
			mats.push(m);
		}
	});
	if (mats.length > 0) fadeables.push({ key, mats });
}

/**
 * Procedural structure: a slab-roofed block. Drawn wherever the model pack has
 * nothing to place — for a guild or landmark that means the whole pack failed
 * to load. Returns the group and its height (for pick proxies / HUD anchors).
 * `hue` colours the identity part, the roof slab; walls come from the palette.
 */
function buildStructure(
	style: WorldStyle,
	palette: WorldPalette,
	hue: string,
	footprint: number,
	heightScale: number,
	seed: number
): { group: THREE.Group; height: number } {
	const g = new THREE.Group();
	const variance = style.heightVariance ? 0.7 + (seed % 100) / 70 : 1;
	const h = style.buildingHeight * heightScale * variance;
	const wallMat = new THREE.MeshStandardMaterial({
		color: palette.wall,
		roughness: 0.4,
		emissive: new THREE.Color(hue),
		emissiveIntensity: style.buildingEmissive * 0.5
	});
	const hueMat = new THREE.MeshStandardMaterial({
		color: hue,
		roughness: 0.65,
		emissive: new THREE.Color(hue),
		emissiveIntensity: style.buildingEmissive
	});

	const walls = new THREE.Mesh(
		new THREE.BoxGeometry(footprint * 0.95, h, footprint * 0.95),
		wallMat
	);
	walls.position.y = h / 2;
	walls.castShadow = walls.receiveShadow = true;
	g.add(walls);

	// flat roof: a thin hue slab capping the walls
	const slab = new THREE.Mesh(new THREE.BoxGeometry(footprint, 0.08, footprint), hueMat);
	slab.position.y = h + 0.04;
	slab.castShadow = true;
	g.add(slab);

	return { group: g, height: h + 0.08 };
}


/**
 * Depth of the desk prop along its own facing axis. The bench placement below
 * offsets by half of this to sit the desk flush inside its tile, so the two
 * must move together.
 */
const DESK_DEPTH = 0.28;

/** Campus bench prop: a low-poly desk + glowing monitor (chair implied). */
function buildDesk(palette: WorldPalette): THREE.Group {
	const g = new THREE.Group();
	const wood = new THREE.MeshStandardMaterial({ color: palette.trunk, roughness: 0.8 });
	const top = new THREE.Mesh(new THREE.BoxGeometry(0.46, 0.04, DESK_DEPTH), wood);
	top.position.y = 0.34;
	top.castShadow = true;
	g.add(top);
	const legGeo = new THREE.BoxGeometry(0.035, 0.32, 0.035);
	for (const [lx, lz] of [
		[-0.2, -0.11],
		[0.2, -0.11],
		[-0.2, 0.11],
		[0.2, 0.11]
	]) {
		const leg = new THREE.Mesh(legGeo, wood);
		leg.position.set(lx, 0.16, lz);
		g.add(leg);
	}
	const screen = new THREE.Mesh(
		new THREE.BoxGeometry(0.2, 0.14, 0.02),
		new THREE.MeshStandardMaterial({
			color: '#1c2733',
			roughness: 0.3,
			emissive: new THREE.Color('#2a3d55'),
			emissiveIntensity: 0.6
		})
	);
	screen.position.set(0, 0.45, 0.08);
	g.add(screen);
	return g;
}



/**
 * World edge treatment: a huge ground SKIRT so the camera never sees void
 * beyond the map (fog swallows its far edge), and a greenbelt HEDGE ringing the
 * playable area — the visual "you shall not pass" matching the hard rule that
 * citizens can only walk tiles. Palette-tinted, one instanced ring of segments.
 */
function buildBorder(group: THREE.Group, n: number, palette: WorldPalette): void {
	const half = n / 2;

	const skirt = new THREE.Mesh(
		new THREE.PlaneGeometry(240, 240),
		new THREE.MeshStandardMaterial({ color: palette.groundAlt, roughness: 1 })
	);
	skirt.rotation.x = -Math.PI / 2;
	skirt.position.y = -0.06;
	group.add(skirt);

	const d = half + 1.1;
	const h = 0.85;
	const segLen = 2.2;
	const mat = new THREE.MeshStandardMaterial({ color: palette.foliage, roughness: 1 });
	const geo = new THREE.BoxGeometry(segLen * 0.94, h, 0.7);
	const spots: { x: number; z: number; rot: number }[] = [];
	for (let t = -d; t <= d + 0.001; t += segLen) {
		spots.push({ x: t, z: -d, rot: 0 }, { x: t, z: d, rot: 0 });
		spots.push({ x: -d, z: t, rot: Math.PI / 2 }, { x: d, z: t, rot: Math.PI / 2 });
	}
	const segs = new THREE.InstancedMesh(geo, mat, spots.length);
	segs.castShadow = false;
	const sm = new THREE.Matrix4();
	spots.forEach((p, i) => {
		sm.compose(
			new THREE.Vector3(p.x, h / 2, p.z),
			new THREE.Quaternion().setFromEuler(new THREE.Euler(0, p.rot, 0)),
			new THREE.Vector3(1, 1, 1)
		);
		segs.setMatrixAt(i, sm);
	});
	group.add(segs);
}

export function buildCivWorld(
	village: Village,
	guilds: GuildVM[],
	palette: WorldPalette,
	assets: ThemeAssets | null,
	style: WorldStyle
): WorldHandles {
	const group = new THREE.Group();
	const n = village.n;
	const m = new THREE.Matrix4();
	const buildingPickMeshes: THREE.Mesh[] = [];
	const buildingCenterOf = new Map<string, THREE.Vector3>();
	const landmarkPickMeshes: THREE.Mesh[] = [];
	const landmarkCenterOf = new Map<string, THREE.Vector3>();
	const fadeables: FadeableStructure[] = [];
	const useModels = style.assetPack !== null && assets !== null;

	// --- ground: every tile is a thin box; checker via per-instance colour ---
	const tileGeo = new THREE.BoxGeometry(1, 0.1, 1);
	const groundMat = new THREE.MeshStandardMaterial({ roughness: 0.95, metalness: 0 });
	const allTiles: { x: number; z: number; kind: TileKind }[] = [];
	for (let z = 0; z < n; z++)
		for (let x = 0; x < n; x++) allTiles.push({ x, z, kind: village.kinds[idx(n, x, z)] });
	const ground = new THREE.InstancedMesh(tileGeo, groundMat, allTiles.length);
	ground.receiveShadow = true;
	const grass = new THREE.Color(palette.ground);
	const grassAlt = new THREE.Color(palette.groundAlt);
	const road = new THREE.Color(palette.path);
	const plaza = new THREE.Color(palette.plaza);
	allTiles.forEach((t, i) => {
		const w = tileToWorld(n, t);
		const raised = t.kind === 'road' || t.kind === 'plaza' ? 0.02 : 0;
		m.makeTranslation(w.x, -0.05 + raised, w.z);
		ground.setMatrixAt(i, m);
		// road and plaza resolve to the same paving colour today — the campus
		// palette deliberately gives them one value. The branch stays because the
		// two tokens are the seam: splitting paving back into path and plaza is a
		// change to fleet-theme.css, not to this file.
		const color =
			t.kind === 'road'
				? road
				: t.kind === 'plaza'
					? plaza
					: (t.x + t.z) % 2 === 0
						? grass
						: grassAlt;
		ground.setColorAt(i, color);
	});
	if (ground.instanceColor) ground.instanceColor.needsUpdate = true;
	group.add(ground);

	// --- world edges: ground skirt + hedge ring (no void, no exit) ---
	buildBorder(group, n, palette);

	// --- vegetation ---
	const trees = collectTiles(village, 'tree');
	const rocks = collectTiles(village, 'rock');
	if (useModels && assets && assets.trees.length > 0) {
		trees.forEach((t, i) => {
			const w = tileToWorld(n, t);
			const tpl = assets.trees[(t.x * 7 + t.z * 13 + i) % assets.trees.length];
			const tree = cloneModel(tpl);
			markShared(tree);
			const s = 0.8 + ((t.x * 3 + t.z * 17) % 10) / 25;
			fitToFootprint(tree, 0.9 * s);
			enableShadows(tree);
			tree.position.x = w.x;
			tree.position.z = w.z;
			tree.rotation.y = ((t.x + t.z * 3) % 8) * (Math.PI / 4);
			group.add(tree);
		});
	} else if (trees.length > 0) {
		// round trees: a trunk under a trimmed sphere of foliage
		const trunkGeo = new THREE.CylinderGeometry(0.05, 0.08, 0.3, 6);
		const trunkMat = new THREE.MeshStandardMaterial({ color: palette.trunk, roughness: 0.9 });
		const trunkMesh = new THREE.InstancedMesh(trunkGeo, trunkMat, trees.length);
		const foliageGeo = new THREE.SphereGeometry(0.3, 10, 8);
		const foliageMat = new THREE.MeshStandardMaterial({ color: palette.foliage, roughness: 0.85 });
		const foliage = new THREE.InstancedMesh(foliageGeo, foliageMat, trees.length);
		trunkMesh.castShadow = foliage.castShadow = true;
		trees.forEach((t, i) => {
			const w = tileToWorld(n, t);
			const s = 0.85 + ((t.x * 3 + t.z * 17) % 10) / 28;
			m.makeTranslation(w.x, 0.15, w.z);
			trunkMesh.setMatrixAt(i, m);
			m.compose(
				new THREE.Vector3(w.x, 0.55 * s, w.z),
				new THREE.Quaternion(),
				new THREE.Vector3(s, s, s)
			);
			foliage.setMatrixAt(i, m);
		});
		group.add(trunkMesh, foliage);
	}
	if (rocks.length > 0) {
		if (useModels && assets && assets.rocks.length > 0) {
			rocks.forEach((t) => {
				const w = tileToWorld(n, t);
				const tpl = assets.rocks[(t.x + t.z) % assets.rocks.length];
				const rock = cloneModel(tpl);
				markShared(rock);
				fitToFootprint(rock, 0.5);
				enableShadows(rock);
				rock.position.x = w.x;
				rock.position.z = w.z;
				rock.rotation.y = (t.x * t.z) % 7;
				group.add(rock);
			});
		} else {
			const rockGeo = new THREE.DodecahedronGeometry(0.14, 0);
			const rockMat = new THREE.MeshStandardMaterial({ color: palette.rock, roughness: 1 });
			const rockMesh = new THREE.InstancedMesh(rockGeo, rockMat, rocks.length);
			rockMesh.castShadow = true;
			rocks.forEach((t, i) => {
				const w = tileToWorld(n, t);
				const s = 0.7 + ((t.x * 5 + t.z * 7) % 10) / 18;
				m.compose(
					new THREE.Vector3(w.x, 0.06, w.z),
					new THREE.Quaternion().setFromEuler(new THREE.Euler(0, (t.x + t.z) * 1.3, 0)),
					new THREE.Vector3(s, s * 0.75, s)
				);
				rockMesh.setMatrixAt(i, m);
			});
			group.add(rockMesh);
		}
	}

	// --- guild buildings ---
	guilds.forEach((guild, gi) => {
		const plot = village.buildings.find((b) => b.guildId === guild.id);
		if (!plot) return;
		const cw = tileToWorld(n, { x: plot.x, z: plot.z });
		const cx = cw.x + 0.5;
		const cz = cw.z + 0.5;
		const hue = guildHue(gi);

		if (useModels && assets && assets.buildings.length > 0) {
			const tpl = assets.buildings[gi % assets.buildings.length];
			const building = cloneModel(tpl);
			markShared(building);
			fitToFootprint(building, 1.9);
			enableShadows(building);
			building.position.x = cx;
			building.position.z = cz;
			const dx = plot.door.x - (plot.x + 0.5);
			const dz = plot.door.z - (plot.z + 0.5);
			building.rotation.y = faceCardinal(dx, dz);
			group.add(building);
			collectFadeable(building, `guild:${guild.id}`, true, fadeables);
			const h = modelHeight(building);
			const proxy = pickProxy(1.9, Math.max(h, 1), 1.9, cx, Math.max(h, 1) / 2, cz);
			proxy.userData.guildId = guild.id;
			group.add(proxy);
			buildingPickMeshes.push(proxy);
			buildingCenterOf.set(guild.id, new THREE.Vector3(cx, h + 0.15, cz));
		} else {
			const { group: b, height } = buildStructure(style, palette, hue, 1.9, 1, gi * 37 + 11);
			b.position.set(cx, 0, cz);
			group.add(b);
			collectFadeable(b, `guild:${guild.id}`, false, fadeables);
			const proxy = pickProxy(1.9, Math.max(height, 1), 1.9, cx, Math.max(height, 1) / 2, cz);
			proxy.userData.guildId = guild.id;
			group.add(proxy);
			buildingPickMeshes.push(proxy);
			buildingCenterOf.set(guild.id, new THREE.Vector3(cx, height + 0.15, cz));
		}

		// banner pole by the door in the guild hue — identity in every theme
		const doorW = tileToWorld(n, plot.door);
		const pole = new THREE.Mesh(
			new THREE.CylinderGeometry(0.02, 0.02, 0.9, 5),
			new THREE.MeshStandardMaterial({ color: palette.trunk })
		);
		pole.position.set(doorW.x + 0.3, 0.45, doorW.z + 0.3);
		const banner = new THREE.Mesh(
			new THREE.PlaneGeometry(0.22, 0.3),
			new THREE.MeshStandardMaterial({ color: hue, side: THREE.DoubleSide })
		);
		banner.position.set(doorW.x + 0.3, 0.72, doorW.z + 0.3 + 0.01);
		group.add(pole, banner);

		// Work-driven district state. Active operations illuminate the plot,
		// blockers use the attention color, and accepted deliveries leave up to
		// three permanent merit markers beside the district banner.
		if (guild.activeQuestCount > 0 || guild.blockedQuestCount > 0 || guild.deliveredQuestCount > 0) {
			const stateColor = guild.blockedQuestCount > 0
				? palette.status.needs
				: guild.activeQuestCount > 0
					? palette.status.working
					: hue;
			const districtRing = new THREE.Mesh(
				new THREE.RingGeometry(1.03, 1.1, 32),
				new THREE.MeshBasicMaterial({
					color: stateColor,
					transparent: true,
					opacity: guild.blockedQuestCount > 0 ? 0.5 : 0.3,
					side: THREE.DoubleSide
				})
			);
			districtRing.rotation.x = -Math.PI / 2;
			districtRing.position.set(cx, 0.025, cz);
			group.add(districtRing);
		}
		const meritCount = Math.min(3, Math.ceil(guild.deliveredQuestCount / 3));
		for (let merit = 0; merit < meritCount; merit += 1) {
			const marker = new THREE.Mesh(
				new THREE.OctahedronGeometry(0.055, 0),
				new THREE.MeshStandardMaterial({
					color: '#f3c969',
					emissive: '#8c641d',
					emissiveIntensity: 0.35,
					roughness: 0.45
				})
			);
			marker.position.set(doorW.x + 0.18 + merit * 0.12, 0.93, doorW.z + 0.31);
			group.add(marker);
		}
	});

	// --- bench props at guild work spots (campus desks) ---
	//
	// Every work spot is an AVENUE tile, necessarily: benches are chosen from
	// tiles the lattice has ALREADY paved so a citizen can walk to one (see
	// grid.ts — carving a bench out of lawn is what used to strand crew on road
	// islands). So the spots cannot move, and putting a desk on the centre of
	// each of them stood furniture in the traffic lanes — including the spot
	// diagonally off the lot corner, which is where two avenues cross, so every
	// building parked a desk in a four-way intersection.
	//
	// The correction is on this side only; the spots are read, never changed:
	//   - a desk is drawn only where it has a wall to back onto, i.e. the tile
	//     it faces belongs to the lot's own footprint. That drops the crossing
	//     and the loose tile past the lot's far edge — the two spots that sit
	//     mid-street with no building to belong to.
	//   - the desks that remain are pushed off the tile centre to the kerb,
	//     flush against the lot boundary, leaving the walking lane and the
	//     citizen's standing point clear. The citizen then reads as standing at
	//     the desk instead of inside it.
	//
	// Half a tile less half the prop, so the desk's back edge lands exactly on
	// the lot boundary and the whole prop stays inside its own tile.
	const kerbOffset = 0.5 - DESK_DEPTH / 2;
	for (const plot of village.buildings) {
		const pc = tileToWorld(n, { x: plot.x, z: plot.z });
		const bcx = pc.x + 0.5;
		const bcz = pc.z + 0.5;
		for (const spot of plot.workSpots) {
			const w = tileToWorld(n, spot);
			// Square-on to the avenue grid, like every other structure: the raw
			// centre-ward bearing lands 18-45 degrees off true because a work
			// spot sits beside the 2x2 footprint rather than square to its centre.
			const face = faceCardinal(bcx - w.x, bcz - w.z);
			const stepX = Math.round(Math.sin(face));
			const stepZ = Math.round(Math.cos(face));
			const backsOntoLot =
				spot.x + stepX >= plot.x &&
				spot.x + stepX < plot.x + plot.size &&
				spot.z + stepZ >= plot.z &&
				spot.z + stepZ < plot.z + plot.size;
			if (!backsOntoLot) continue;
			const prop = buildDesk(palette);
			prop.position.set(w.x + stepX * kerbOffset, 0, w.z + stepZ * kerbOffset);
			prop.rotation.y = face;
			group.add(prop);
		}
	}

	// --- civic landmarks: the Armory (tools) + the Council (command network) ---
	for (const lm of village.landmarks) {
		const lw = tileToWorld(n, { x: lm.x, z: lm.z });
		const cx = lw.x + 0.5;
		const cz = lw.z + 0.5;
		if (useModels && assets) {
			const tpl = lm.id === 'armory' ? assets.armory : assets.council;
			const structure = cloneModel(tpl);
			markShared(structure);
			fitToFootprint(structure, 1.9);
			enableShadows(structure);
			structure.position.x = cx;
			structure.position.z = cz;
			const dx = lm.door.x - (lm.x + 0.5);
			const dz = lm.door.z - (lm.z + 0.5);
			structure.rotation.y = faceCardinal(dx, dz);
			group.add(structure);
			collectFadeable(structure, `landmark:${lm.id}`, true, fadeables);
			const h = modelHeight(structure);
			const proxy = pickProxy(1.9, Math.max(h, 1), 1.9, cx, Math.max(h, 1) / 2, cz);
			proxy.userData.landmarkId = lm.id;
			group.add(proxy);
			landmarkPickMeshes.push(proxy);
			landmarkCenterOf.set(lm.id, new THREE.Vector3(cx, h + 0.15, cz));
		} else {
			const hue = lm.id === 'armory' ? palette.rock : palette.roofHall;
			const { group: b, height } = buildStructure(style, palette, hue, 1.7, 1.15, lm.id === 'armory' ? 5 : 9);
			b.position.set(cx, 0, cz);
			group.add(b);
			collectFadeable(b, `landmark:${lm.id}`, false, fadeables);
			const proxy = pickProxy(1.7, Math.max(height, 1), 1.7, cx, Math.max(height, 1) / 2, cz);
			proxy.userData.landmarkId = lm.id;
			group.add(proxy);
			landmarkPickMeshes.push(proxy);
			landmarkCenterOf.set(lm.id, new THREE.Vector3(cx, height + 0.2, cz));
		}
	}

	// --- the Hall (you) ---
	{
		const hw = tileToWorld(n, { x: village.hall.x, z: village.hall.z });
		const cx = hw.x + 0.5;
		const cz = hw.z + 0.5;
		if (useModels && assets) {
			const castle = cloneModel(assets.hall);
			markShared(castle);
			fitToFootprint(castle, 2.6);
			enableShadows(castle);
			castle.position.x = cx;
			castle.position.z = cz;
			castle.rotation.y = Math.PI; // gate faces south (the hall front tile)
			group.add(castle);
			collectFadeable(castle, 'landmark:hall', true, fadeables);
			const h = modelHeight(castle);
			const hallProxy = pickProxy(2.5, Math.max(h, 1.4), 2.5, cx, Math.max(h, 1.4) / 2, cz);
			hallProxy.userData.landmarkId = 'hall';
			group.add(hallProxy);
			landmarkPickMeshes.push(hallProxy);
			landmarkCenterOf.set('hall', new THREE.Vector3(cx, h + 0.2, cz));
		} else {
			const base = new THREE.Mesh(
				new THREE.BoxGeometry(2.4, 0.22, 2.4),
				new THREE.MeshStandardMaterial({ color: palette.rock, roughness: 1 })
			);
			base.position.set(cx, 0.11, cz);
			base.receiveShadow = true;
			group.add(base);
			const { group: b, height } = buildStructure(style, palette, palette.roofHall, 2.2, 1.5, 3);
			b.position.set(cx, 0.22, cz);
			group.add(b);
			collectFadeable(b, 'landmark:hall', false, fadeables);
			const total = height + 0.22;
			const hallProxy = pickProxy(2.3, Math.max(total, 1.4), 2.3, cx, Math.max(total, 1.4) / 2, cz);
			hallProxy.userData.landmarkId = 'hall';
			group.add(hallProxy);
			landmarkPickMeshes.push(hallProxy);
			landmarkCenterOf.set('hall', new THREE.Vector3(cx, total + 0.2, cz));
		}
	}

	return {
		group,
		buildingPickMeshes,
		buildingCenterOf,
		landmarkPickMeshes,
		landmarkCenterOf,
		fadeables,
		dispose: () => {
			disposeObject(group);
			// model-path fadeable materials are per-structure clones the
			// shared-template skip above never touches (double-dispose is safe)
			for (const f of fadeables) for (const m of f.mats) m.dispose();
		}
	};
}
