import * as THREE from 'three';
import {
	EntityManager,
	Vehicle,
	Path as YukaPath,
	FollowPathBehavior,
	SeparationBehavior
} from 'yuka';
import { Vector3 as YukaVector3 } from 'yuka';
import type { CitizenVM, WorldPalette } from './types';
import { guildHue } from './palette';
import { astar, tileToWorld, type Tile, type Village } from './grid';
import { cloneModel, fitToHeight, enableShadows, type CharacterTemplate } from './assets';
import type { WorldStyle } from './worldStyle';
import { flatClone, toonGradient } from './flatLook';

/**
 * Citizen actors: campus characters driven by a small behaviour state
 * machine. Behaviour IS the ambient HUD (see the design doc's Jarvis HUD
 * section): working = at the guild bench, playing a work animation; idle =
 * wandering their own block; resting = napping at the bench (💤); needs-you =
 * walks to the plaza before the Hall; paused/offline = dimmed, still.
 *
 * MOVEMENT ARCHITECTURE: A* over walkable tiles stays the ROUTING spine
 * (citizens cannot cross buildings); locomotion is a yuka steering layer —
 * each actor is a Vehicle with FollowPathBehavior (the A* waypoints) plus
 * SeparationBehavior (no ghosting through each other), integrated by one
 * EntityManager. A hard clamp keeps steering deviation within the road tile.
 * Future behaviours (wander, cohesion for plaza gatherings, flee, FSMs) plug
 * into the same steering stack.
 */

type Mode = 'stand' | 'walk' | 'work' | 'wander-wait';

const WALK_SPEED = 1.7; // tiles/sec
const BODY_Y = 0.34;
const CHARACTER_HEIGHT = 0.78;
/** Flip if the character models walk backwards (glTF forward-axis variance). */
const FACING_OFFSET = 0;
const HALO_Y = 1.02;
/** Max lateral deviation from the routed segment (stays within the road). */
const ROAD_DEVIATION = 0.3;
/**
 * Idle wander reach, in tiles (Manhattan) from the citizen's own bench. The
 * near radius is about one campus block, so a wandering citizen works the
 * avenues that front their own building and the crossings at either end.
 */
const WANDER_HOME_RADIUS = 8;
/** The occasional longer trip: a couple of blocks over, or across the quad. */
const WANDER_ROAM_RADIUS = 20;
/** How often a wander takes that longer trip. */
const WANDER_ROAM_CHANCE = 0.15;

interface Actor {
	vm: CitizenVM;
	group: THREE.Group;
	body: THREE.Mesh;
	head: THREE.Mesh;
	ring: THREE.Mesh;
	bodyMat: THREE.MeshToonMaterial;
	headMat: THREE.MeshToonMaterial;
	ringMat: THREE.MeshBasicMaterial;
	guildColor: THREE.Color;
	mode: Mode;
	tile: Tile;
	waitT: number;
	phase: number;
	arriveMode: Mode;
	/** Deterministic within-tile offset so co-located citizens don't overlap. */
	offX: number;
	offZ: number;
	/** God-Hand: behaviour frozen + lifted while the operator carries them. */
	dragging: boolean;
	/** The primary agent's halo (null for everyone else). */
	halo: THREE.Mesh | null;
	/** X-ray silhouette — renders ONLY where the citizen is occluded. */
	ghost: THREE.Mesh;
	ghostMat: THREE.MeshBasicMaterial;
	// --- yuka locomotion ---
	vehicle: Vehicle;
	followPath: FollowPathBehavior;
	separation: SeparationBehavior;
	routeTiles: Tile[];
	routeIdx: number;
	routeFrom: Tile;
	finalX: number;
	finalZ: number;
	// animated character (when assets are loaded)
	model: THREE.Object3D | null;
	modelMats: THREE.Material[];
	mixer: THREE.AnimationMixer | null;
	actions: { idle?: THREE.AnimationAction; walk?: THREE.AnimationAction; work?: THREE.AnimationAction };
	current: THREE.AnimationAction | null;
}

function hashString(str: string): number {
	let h = 2166136261;
	for (let i = 0; i < str.length; i++) {
		h ^= str.charCodeAt(i);
		h = Math.imul(h, 16777619);
	}
	return h >>> 0;
}

/**
 * Flip `transparent` safely.
 *
 * `transparent` is part of three's program cache key, so changing it on a
 * material that has already rendered leaves the old program bound and the new
 * alpha is silently discarded. That is not theoretical: the citizen pick proxy
 * was set to `transparent = true, opacity = 0` after first paint, kept its
 * opaque program, and drew every crew member as a solid guild-coloured capsule
 * swallowing their character model. Blending state alone is never enough —
 * the recompile has to be requested.
 */
function setTransparent(mat: THREE.Material, transparent: boolean): void {
	if (mat.transparent === transparent) return;
	mat.transparent = transparent;
	mat.needsUpdate = true;
}

/**
 * Show or hide the capsule that stands in for a citizen's body.
 *
 * Hiding is `colorWrite`, not alpha: the mesh must survive for raycast picking,
 * and a colour-suppressed draw states that intent directly instead of encoding
 * it as a fully transparent blend that a stale program can undo.
 */
function hideCapsule(mat: THREE.Material, hidden: boolean): void {
	mat.colorWrite = !hidden;
	mat.opacity = 1;
	setTransparent(mat, false);
}

function findClip(clips: THREE.AnimationClip[], patterns: RegExp[]): THREE.AnimationClip | undefined {
	for (const p of patterns) {
		const hit = clips.find((c) => p.test(c.name));
		if (hit) return hit;
	}
	return undefined;
}

export class CitizenSystem {
	readonly group = new THREE.Group();
	private actors = new Map<string, Actor>();
	/** One steering world for the whole crew (separation needs neighbours). */
	private entityManager = new EntityManager();
	/** Shared halo geometry/materials — the primary agent's golden crown and
	 * the envoy's smaller silver one; the same in every theme so both read
	 * instantly over any character body. */
	private haloGeo = new THREE.TorusGeometry(0.2, 0.028, 10, 32);
	private haloMat = new THREE.MeshBasicMaterial({
		color: '#ffd54a',
		transparent: true,
		opacity: 0.92,
		depthWrite: false
	});
	private envoyHaloMat = new THREE.MeshBasicMaterial({
		color: '#cfd8e6',
		transparent: true,
		opacity: 0.85,
		depthWrite: false
	});
	/** Shared x-ray silhouette shape (League/Diablo-style): drawn with
	 * depthFunc=GreaterDepth so it appears exactly where geometry hides the
	 * citizen — behind buildings, trees, anything — and nowhere else. */
	private ghostGeo = new THREE.CapsuleGeometry(0.17, 0.32, 4, 10);
	private village: Village | null = null;
	/** Every walkable tile (avenues + quad) — the wander candidate pool. */
	private wanderTiles: Tile[] = [];
	private palette: WorldPalette;
	private guildIndexOf = new Map<string, number>();
	private characters: CharacterTemplate[] | null = null;
	private workClips: RegExp[] = [];

	constructor(palette: WorldPalette) {
		this.palette = palette;
	}

	/** World style: the work-animation preference, re-applied to live actors. */
	setStyle(style: WorldStyle): void {
		this.workClips = style.workClips;
		for (const a of this.actors.values()) this.applyStyle(a);
	}

	/** Swap the theme's character set — every actor re-dresses in place. */
	setCharacters(templates: CharacterTemplate[] | null): void {
		if (this.characters === templates) return;
		this.characters = templates;
		for (const a of this.actors.values()) {
			this.detachModel(a);
			if (templates && templates.length > 0) this.attachModel(a);
			else this.applyStyle(a);
		}
	}

	setPalette(palette: WorldPalette): void {
		this.palette = palette;
		for (const a of this.actors.values()) this.applyVibe(a);
	}

	setVillage(village: Village, guildOrder: string[]): void {
		this.village = village;
		this.wanderTiles = [...village.roadTiles, ...village.plazaTiles];
		this.guildIndexOf = new Map(guildOrder.map((g, i) => [g, i]));
		for (const a of this.actors.values()) this.enterVibe(a, true);
	}

	/** Minimap dots: current ground position + vibe for every actor. */
	dots(): { id: string; x: number; z: number; vibe: string }[] {
		return Array.from(this.actors.values()).map((a) => ({
			id: a.vm.id,
			x: a.group.position.x,
			z: a.group.position.z,
			vibe: a.vm.vibe
		}));
	}

	/** The character template this citizen wears (same id-hash pick as
	 * attachModel) — the character sheet renders its portrait from it. */
	characterTemplateFor(id: string): CharacterTemplate | null {
		if (!this.characters || this.characters.length === 0) return null;
		return this.characters[hashString(id) % this.characters.length];
	}

	/** Pickable meshes (invisible capsule proxies); userData.citizenId is set. */
	pickMeshes(): THREE.Mesh[] {
		return Array.from(this.actors.values()).map((a) => a.body);
	}

	worldPos(id: string): THREE.Vector3 | null {
		const a = this.actors.get(id);
		return a ? a.group.position.clone() : null;
	}

	sync(citizens: CitizenVM[]): void {
		const seen = new Set<string>();
		for (const vm of citizens) {
			seen.add(vm.id);
			const existing = this.actors.get(vm.id);
			if (existing) {
				const vibeChanged = existing.vm.vibe !== vm.vibe;
				const guildChanged = existing.vm.guildId !== vm.guildId;
				const restingChanged = existing.vm.resting !== vm.resting;
				existing.vm = vm;
				this.applyHalo(existing);
				if (guildChanged) this.applyGuildColor(existing);
				if (vibeChanged || guildChanged || restingChanged) {
					this.applyVibe(existing);
					this.enterVibe(existing, false);
				}
			} else {
				const actor = this.createActor(vm);
				this.actors.set(vm.id, actor);
				this.group.add(actor.group);
				this.enterVibe(actor, true);
			}
		}
		for (const [id, a] of this.actors) {
			if (!seen.has(id)) {
				this.disposeActor(a);
				this.actors.delete(id);
			}
		}
	}

	/** God-Hand pickup: freeze behaviour + steering, engine carries the group. */
	beginDrag(id: string): void {
		const a = this.actors.get(id);
		if (!a) return;
		a.dragging = true;
		a.followPath.active = false;
		a.separation.active = false;
		a.vehicle.velocity.set(0, 0, 0);
	}

	dragTo(id: string, x: number, z: number): void {
		const a = this.actors.get(id);
		if (a?.dragging) a.group.position.set(x, 0.55, z);
	}

	/** Release: settle back onto the logical tile (behaviour resumes as-was). */
	endDrag(id: string): void {
		const a = this.actors.get(id);
		if (!a || !this.village) return;
		a.dragging = false;
		const w = tileToWorld(this.village.n, a.tile);
		a.group.position.set(w.x + a.offX, 0, w.z + a.offZ);
		a.vehicle.position.set(a.group.position.x, 0, a.group.position.z);
		if (a.mode === 'walk') {
			a.followPath.active = true;
			a.separation.active = true;
		}
	}

	/**
	 * @param visible ids currently on screen (engine-computed). Off-screen
	 * citizens keep MOVING (simulation continuity — they arrive on time) but
	 * skip animation mixers and cosmetic pulses: skinning is the real CPU
	 * cost, and it's wasted on actors the camera can't see (the same
	 * visibility-gated simulation big-map strategy games use).
	 */
	update(dt: number, now: number, visible?: ReadonlySet<string>): void {
		if (!this.village) return;
		// one steering step for the whole crew (path-follow + separation)
		this.entityManager.update(dt);
		for (const a of this.actors.values()) {
			const onScreen = visible ? visible.has(a.vm.id) : true;
			if (a.dragging) {
				// carried by the God-Hand: keep the mixer breathing, skip behaviour
				if (a.mixer && this.usesModel(a)) a.mixer.update(dt);
				continue;
			}
			// movement runs on/off screen alike (arrivals stay honest)
			if (a.mode === 'walk') this.syncWalk(a);
			else if (a.mode === 'wander-wait') {
				a.waitT -= dt;
				if (a.waitT <= 0) this.startWander(a);
			}
			if (!onScreen) continue; // cosmetics below are view-only
			if (a.mode === 'work' && !this.usesModel(a)) {
				// capsule figure: bench bob
				a.body.position.y = BODY_Y + Math.sin(now * 5 + a.phase) * 0.025;
				a.head.position.y = 0.62 + Math.sin(now * 5 + a.phase) * 0.03;
			}
			if (a.mode === 'stand' && a.vm.vibe === 'needs') {
				a.group.rotation.y += Math.sin(now * 2 + a.phase) * 0.002;
			}
			// animation state follows behaviour mode (characters only)
			if (a.mixer && this.usesModel(a)) {
				this.playFor(a);
				a.mixer.update(dt);
			}
			const active = a.vm.vibe === 'working' || a.vm.vibe === 'needs';
			const pulse = active ? 0.75 + Math.sin(now * 4 + a.phase) * 0.25 : 0.55;
			a.ringMat.opacity = pulse;
			// the halo spins slowly and bobs — alive, not a static decal
			if (a.halo) {
				a.halo.rotation.z = now * 0.8;
				a.halo.position.y = HALO_Y + Math.sin(now * 2 + a.phase) * 0.035;
			}
		}
	}

	dispose(): void {
		this.sync([]);
		this.haloGeo.dispose();
		this.haloMat.dispose();
		this.envoyHaloMat.dispose();
		this.ghostGeo.dispose();
	}

	// --- internals ---

	private createActor(vm: CitizenVM): Actor {
		const group = new THREE.Group();
		const bodyMat = new THREE.MeshToonMaterial({ gradientMap: toonGradient() });
		const headMat = new THREE.MeshToonMaterial({
			color: '#f2d0a8',
			gradientMap: toonGradient()
		});
		const ringMat = new THREE.MeshBasicMaterial({
			transparent: true,
			opacity: 0.7,
			side: THREE.DoubleSide,
			depthWrite: false
		});

		const body = new THREE.Mesh(new THREE.CapsuleGeometry(0.14, 0.26, 4, 10), bodyMat);
		body.position.y = BODY_Y;
		body.castShadow = true;
		body.userData.citizenId = vm.id;

		const head = new THREE.Mesh(new THREE.SphereGeometry(0.11, 12, 10), headMat);
		head.position.y = 0.62;
		head.castShadow = true;
		head.userData.citizenId = vm.id;

		const ring = new THREE.Mesh(new THREE.RingGeometry(0.24, 0.33, 24), ringMat);
		ring.rotation.x = -Math.PI / 2;
		ring.position.y = 0.02;

		const ghostMat = new THREE.MeshBasicMaterial({
			transparent: true,
			opacity: 0.35,
			depthWrite: false,
			depthFunc: THREE.GreaterDepth
		});
		const ghost = new THREE.Mesh(this.ghostGeo, ghostMat);
		ghost.position.y = BODY_Y;
		ghost.renderOrder = 5;

		group.add(body, head, ring, ghost);

		// yuka locomotion: path-following + separation, integrated centrally
		const vehicle = new Vehicle();
		vehicle.maxSpeed = WALK_SPEED;
		vehicle.maxForce = 8;
		vehicle.updateNeighborhood = true;
		vehicle.neighborhoodRadius = 0.7;
		const followPath = new FollowPathBehavior(new YukaPath(), 0.4);
		followPath.active = false;
		const separation = new SeparationBehavior();
		separation.weight = 1.4;
		separation.active = false;
		vehicle.steering.add(followPath);
		vehicle.steering.add(separation);
		this.entityManager.add(vehicle);

		const h = hashString(vm.id);
		const offAngle = ((h % 360) * Math.PI) / 180;
		const offRadius = 0.1 + ((h >> 8) % 10) / 80;
		const actor: Actor = {
			vm,
			group,
			body,
			head,
			ring,
			bodyMat,
			headMat,
			ringMat,
			guildColor: new THREE.Color('#888888'),
			mode: 'stand',
			tile: { x: 0, z: 0 },
			waitT: 0,
			phase: Math.random() * Math.PI * 2,
			arriveMode: 'stand',
			offX: Math.cos(offAngle) * offRadius,
			offZ: Math.sin(offAngle) * offRadius,
			dragging: false,
			halo: null,
			ghost,
			ghostMat,
			vehicle,
			followPath,
			separation,
			routeTiles: [],
			routeIdx: 0,
			routeFrom: { x: 0, z: 0 },
			finalX: 0,
			finalZ: 0,
			model: null,
			modelMats: [],
			mixer: null,
			actions: {},
			current: null
		};
		this.applyGuildColor(actor);
		this.applyVibe(actor);
		this.applyHalo(actor);
		if (this.characters && this.characters.length > 0) this.attachModel(actor);
		return actor;
	}

	/** Halos: the primary agent (Presto) wears gold; the envoy (his public
	 * face) a smaller silver one. Primary wins if an agent is somehow both. */
	private applyHalo(a: Actor): void {
		const want: 'primary' | 'envoy' | null = a.vm.isPrimary
			? 'primary'
			: a.vm.isEnvoy
				? 'envoy'
				: null;
		const have = (a.halo?.userData.kind as string | undefined) ?? null;
		if (want === have) return;
		if (a.halo) {
			a.group.remove(a.halo);
			a.halo = null;
		}
		if (!want) return;
		const halo = new THREE.Mesh(
			this.haloGeo,
			want === 'primary' ? this.haloMat : this.envoyHaloMat
		);
		if (want === 'envoy') halo.scale.setScalar(0.7);
		halo.userData.kind = want;
		halo.rotation.x = Math.PI / 2;
		halo.position.y = HALO_Y;
		a.halo = halo;
		a.group.add(halo);
	}

	/** Remove the actor's character model (theme swap / disposal). */
	private detachModel(a: Actor): void {
		if (!a.model) return;
		a.mixer?.stopAllAction();
		a.mixer = null;
		a.actions = {};
		a.current = null;
		a.group.remove(a.model);
		for (const m of a.modelMats) m.dispose();
		a.modelMats = [];
		a.model = null;
	}

	private attachModel(a: Actor): void {
		if (!this.characters || this.characters.length === 0) return;
		const tpl = this.characters[hashString(a.vm.id) % this.characters.length];
		const model = cloneModel(tpl.scene);
		fitToHeight(model, CHARACTER_HEIGHT);
		enableShadows(model);
		model.rotation.y = FACING_OFFSET;
		// per-actor material clones so dimming one citizen can't dim its siblings
		const mats: THREE.Material[] = [];
		model.traverse((o) => {
			const mesh = o as THREE.Mesh;
			if (mesh.isMesh && mesh.material) {
				// per-actor materials (the guild tint below is individual), on the
				// same stepped ramp as the rest of the world
				const cloned = Array.isArray(mesh.material)
					? mesh.material.map((mm) => flatClone(mm))
					: flatClone(mesh.material);
				mesh.material = cloned;
				if (Array.isArray(cloned)) mats.push(...cloned);
				else mats.push(cloned);
				mesh.userData.citizenId = a.vm.id;
			}
		});
		// guild-tint the primary garment material (robot shell, suit, hi-vis
		// vest, ...) — skin/hair/eyes are never matched
		for (const m of mats) {
			const garment = m as THREE.MeshToonMaterial;
			if (/main|body|suit|vest|hoodie|overall|shirt|jacket/i.test(m.name ?? '') && garment.color) {
				garment.color.copy(a.guildColor).lerp(new THREE.Color('#ffffff'), 0.45);
			}
		}
		a.model = model;
		a.modelMats = mats;
		a.group.add(model);
		this.applyStyle(a);

		a.mixer = new THREE.AnimationMixer(model);
		const idleClip = findClip(tpl.clips, [/^idle$/i, /idle/i]);
		const walkClip = findClip(tpl.clips, [/^walking_a$/i, /^walking$/i, /walk/i, /run/i]);
		// the style's preferred work motions first, then a generic fallback chain
		// so an unknown clip set still mimes something rather than standing idle
		const workClip = findClip(tpl.clips, [
			...this.workClips,
			/interact/i,
			/use_item/i,
			/punch/i,
			/wave/i,
			/chop/i,
			/spellcast/i,
			/attack/i
		]);
		if (idleClip) a.actions.idle = a.mixer.clipAction(idleClip);
		if (walkClip) a.actions.walk = a.mixer.clipAction(walkClip);
		if (workClip) a.actions.work = a.mixer.clipAction(workClip);
		this.applyVibe(a);
	}

	/** Crossfade to the action matching the current behaviour mode. */
	private playFor(a: Actor): void {
		const want =
			a.mode === 'walk'
				? a.actions.walk ?? a.actions.idle
				: a.mode === 'work'
					? a.actions.work ?? a.actions.idle
					: a.actions.idle;
		if (!want || a.current === want) return;
		want.reset();
		want.setLoop(THREE.LoopRepeat, Infinity);
		want.play();
		if (a.current) a.current.crossFadeTo(want, 0.25, false);
		a.current = want;
	}

	private applyGuildColor(a: Actor): void {
		const gi = this.guildIndexOf.get(a.vm.guildId) ?? 0;
		a.guildColor = new THREE.Color(guildHue(gi));
		a.bodyMat.color.copy(a.guildColor);
	}

	/** True when this actor should render its character model (vs the capsule). */
	private usesModel(a: Actor): boolean {
		return a.model !== null;
	}

	/**
	 * Character model vs capsule presentation. The capsule is not a stylistic
	 * alternative — it is what an actor wears when the character wardrobe failed
	 * to load, so this branch is the world's degraded state, not dead code.
	 */
	private applyStyle(a: Actor): void {
		if (this.usesModel(a)) {
			if (a.model) a.model.visible = true;
			// The capsule stays in the scene as the raycast pick proxy but must
			// draw nothing. `colorWrite = false` says exactly that, and says it
			// as GL state rather than as a blend: it cannot be defeated by the
			// program cache (see hideCapsule), costs no transparent-pass draw,
			// and still raycasts because picking never consults it.
			hideCapsule(a.bodyMat, true);
			a.bodyMat.depthWrite = false;
			a.body.castShadow = false;
			a.head.visible = false;
		} else {
			if (a.model) a.model.visible = false;
			hideCapsule(a.bodyMat, false);
			a.bodyMat.depthWrite = true;
			a.body.castShadow = true;
			a.head.visible = true;
		}
		this.applyVibe(a);
	}

	private applyVibe(a: Actor): void {
		a.ringMat.color = new THREE.Color(this.palette.status[a.vm.vibe]);
		a.ghostMat.color.copy(a.ringMat.color);
		const dim = a.vm.vibe === 'paused' || a.vm.vibe === 'offline';
		if (this.usesModel(a)) {
			for (const m of a.modelMats) {
				m.transparent = dim;
				m.opacity = dim ? 0.55 : 1;
			}
		} else {
			a.bodyMat.color.copy(a.guildColor);
			if (dim) a.bodyMat.color.lerp(new THREE.Color('#9aa0a6'), 0.6);
			a.bodyMat.opacity = dim ? 0.75 : 1;
			setTransparent(a.bodyMat, dim);
		}
	}

	private placeAt(a: Actor, tile: Tile): void {
		if (!this.village) return;
		a.tile = tile;
		const w = tileToWorld(this.village.n, tile);
		a.group.position.set(w.x + a.offX, 0, w.z + a.offZ);
		a.vehicle.position.set(a.group.position.x, 0, a.group.position.z);
		a.vehicle.velocity.set(0, 0, 0);
	}

	private stopSteering(a: Actor): void {
		a.followPath.active = false;
		a.separation.active = false;
		a.vehicle.velocity.set(0, 0, 0);
	}

	/** Route via A* and hand the waypoints to the steering layer. */
	private startRoute(a: Actor, dest: Tile, arrive: Mode): void {
		if (!this.village) return;
		const tiles = astar(this.village, a.tile, dest);
		if (tiles.length === 0) {
			// no route — stay put (never walk off-graph)
			this.stopSteering(a);
			a.mode = arrive;
			if (arrive === 'wander-wait') a.waitT = 1.5 + Math.random() * 4;
			return;
		}
		const n = this.village.n;
		const path = new YukaPath();
		for (const t of tiles) {
			const w = tileToWorld(n, t);
			path.add(new YukaVector3(w.x, 0, w.z));
		}
		// final point settles into the citizen's own spot within the tile
		const last = tileToWorld(n, tiles[tiles.length - 1]);
		a.finalX = last.x + a.offX;
		a.finalZ = last.z + a.offZ;
		path.add(new YukaVector3(a.finalX, 0, a.finalZ));
		a.followPath.path = path;
		a.followPath.active = true;
		a.separation.active = true;
		a.vehicle.position.set(a.group.position.x, 0, a.group.position.z);
		a.routeTiles = tiles;
		a.routeIdx = 0;
		a.routeFrom = a.tile;
		a.arriveMode = arrive;
		a.mode = 'walk';
	}

	/** Sync the visual from the vehicle; clamp to the road; detect arrival. */
	private syncWalk(a: Actor): void {
		if (!this.village) return;
		const n = this.village.n;
		const v = a.vehicle;

		// hard on-road clamp: cap deviation from the current routed segment
		const targetTile = a.routeTiles[Math.min(a.routeIdx, a.routeTiles.length - 1)];
		const fromTile = a.routeIdx > 0 ? a.routeTiles[a.routeIdx - 1] : a.routeFrom;
		const tw = tileToWorld(n, targetTile);
		const fw = tileToWorld(n, fromTile);
		const segX = tw.x - fw.x;
		const segZ = tw.z - fw.z;
		const segLen2 = segX * segX + segZ * segZ;
		if (segLen2 > 1e-6) {
			const t = Math.max(
				0,
				Math.min(1, ((v.position.x - fw.x) * segX + (v.position.z - fw.z) * segZ) / segLen2)
			);
			const cx = fw.x + segX * t;
			const cz = fw.z + segZ * t;
			const dx = v.position.x - cx;
			const dz = v.position.z - cz;
			const dev = Math.hypot(dx, dz);
			if (dev > ROAD_DEVIATION) {
				const s = ROAD_DEVIATION / dev;
				v.position.x = cx + dx * s;
				v.position.z = cz + dz * s;
			}
		}
		v.position.y = 0;

		a.group.position.set(v.position.x, 0, v.position.z);
		if (v.velocity.squaredLength() > 0.0004) {
			a.group.rotation.y = Math.atan2(v.velocity.x, v.velocity.z) + FACING_OFFSET;
		}

		// waypoint progression keeps a.tile honest for future re-routes
		if (a.routeIdx < a.routeTiles.length) {
			const next = a.routeTiles[a.routeIdx];
			const w = tileToWorld(n, next);
			const ddx = v.position.x - w.x;
			const ddz = v.position.z - w.z;
			if (ddx * ddx + ddz * ddz < 0.5 * 0.5) {
				a.tile = next;
				a.routeIdx++;
			}
		}

		// arrival: the path is exhausted and we're at the settle point
		const fx = v.position.x - a.finalX;
		const fz = v.position.z - a.finalZ;
		if (a.routeIdx >= a.routeTiles.length && fx * fx + fz * fz < 0.12 * 0.12) {
			this.stopSteering(a);
			a.group.position.set(a.finalX, 0, a.finalZ);
			v.position.set(a.finalX, 0, a.finalZ);
			a.tile = a.routeTiles[a.routeTiles.length - 1] ?? a.tile;
			a.mode = a.arriveMode;
			if (a.arriveMode === 'wander-wait') a.waitT = 1.5 + Math.random() * 4;
			this.faceHall(a);
		}
	}

	/** Set behaviour for the current vibe; teleport=true places instantly (first spawn / rebuild). */
	private enterVibe(a: Actor, teleport: boolean): void {
		if (!this.village) return;
		const v = this.village;
		const work = v.workSpotOf.get(a.vm.id) ?? v.plazaTiles[0];
		const plazaSpot = v.plazaSpotOf.get(a.vm.id) ?? v.plazaTiles[0];

		const go = (dest: Tile, arrive: Mode) => {
			if (teleport) {
				this.stopSteering(a);
				this.placeAt(a, dest);
				a.mode = arrive;
				this.faceHall(a);
			} else {
				this.startRoute(a, dest, arrive);
			}
		};

		switch (a.vm.vibe) {
			case 'working':
				go(work, 'work');
				break;
			case 'needs':
				go(plazaSpot, 'stand');
				break;
			case 'idle':
				if (a.vm.resting) {
					// napping at the bench (💤 rides on the HUD anchor)
					go(work, 'stand');
					break;
				}
				if (teleport) {
					this.stopSteering(a);
					this.placeAt(a, work);
				}
				a.mode = 'wander-wait';
				a.waitT = 0.5 + Math.random() * 2.5;
				break;
			case 'paused':
			case 'offline':
				go(work, 'stand');
				break;
		}
	}

	/**
	 * Idle wander: a destination in the citizen's OWN neighbourhood, not a
	 * uniform draw over the whole campus.
	 *
	 * The avenue lattice runs edge to edge across the full grid and its tile
	 * count is the same whether the roster holds three programs or two hundred,
	 * so a uniform draw is a draw over the entire map: measured on the live
	 * eleven-crew roster it sent idle members a mean of 26 tiles from their own
	 * building, p95 49, worst case 66 — half a minute of walking one way at
	 * WALK_SPEED, usually into empty parkland. Anchoring on their bench keeps
	 * them milling around their own program; WANDER_ROAM_CHANCE of trips take
	 * the wider radius so there is still traffic between blocks and across the
	 * quad, and the crew reads as inhabiting the campus rather than tethered.
	 *
	 * Candidates are only ever road/plaza tiles, and the lattice is paved before
	 * anything is placed on it, so the entire walkable set is one connected
	 * component — every draw is reachable and no citizen can strand. A citizen
	 * whose program has no lot inherits a bench on the fallback plot (grid.ts)
	 * and anchors there, which is where they already stand to work.
	 */
	private startWander(a: Actor): void {
		if (!this.village) return;
		const pool = this.wanderTiles;
		if (pool.length === 0) return;
		const home = this.village.workSpotOf.get(a.vm.id) ?? this.village.plazaTiles[0];
		const radius = Math.random() < WANDER_ROAM_CHANCE ? WANDER_ROAM_RADIUS : WANDER_HOME_RADIUS;
		const near = pool.filter((t) => Math.abs(t.x - home.x) + Math.abs(t.z - home.z) <= radius);
		// an empty neighbourhood would only mean a degenerate layout; fall back to
		// the whole network rather than leaving the citizen with nowhere to go
		const candidates = near.length > 0 ? near : pool;
		const dest = candidates[Math.floor(Math.random() * candidates.length)];
		this.startRoute(a, dest, 'wander-wait');
	}

	private faceHall(a: Actor): void {
		if (!this.village) return;
		if (a.vm.vibe !== 'needs') return;
		const c = tileToWorld(this.village.n, { x: this.village.n / 2, z: this.village.n / 2 });
		const dx = c.x - a.group.position.x;
		const dz = c.z - a.group.position.z;
		a.group.rotation.y = Math.atan2(dx, dz) + FACING_OFFSET;
	}

	private disposeActor(a: Actor): void {
		this.entityManager.remove(a.vehicle);
		// model geometries are shared with the template — detach disposes only
		// our per-actor material clones
		this.detachModel(a);
		this.group.remove(a.group);
		a.body.geometry.dispose();
		a.head.geometry.dispose();
		a.ring.geometry.dispose();
		a.bodyMat.dispose();
		a.headMat.dispose();
		a.ringMat.dispose();
		a.ghostMat.dispose();
	}
}
