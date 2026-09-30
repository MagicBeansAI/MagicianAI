import * as THREE from 'three';
import { OrbitControls } from 'three/examples/jsm/controls/OrbitControls.js';
import type { CitizenVM, EngineCallbacks, GuildVM, WorldPalette } from './types';
import { agentTarget, guildTarget, landmarkTarget, parseTarget, type LandmarkId } from './types';
import { readWorldPalette } from './palette';
import { buildVillage, type Village } from './grid';
import { buildCivWorld, type WorldHandles } from './civWorld';
import { CitizenSystem } from './citizens';
import {
	loadThemeAssets,
	loadCharacters,
	loadCloudModels,
	CAMPUS_CHARACTERS,
	type CharacterTemplate,
	type ThemeAssets
} from './assets';
import { HandoffSystem, type HandoffEdge } from './handoffs';
import { CAMPUS_STYLE, type WorldStyle } from './worldStyle';
import { EnvironmentSystem, type WeatherState } from './environment';
import { flattenMaterials } from './flatLook';

/**
 * FleetEngine — the Three.js game engine behind FleetWorld.svelte.
 *
 * Owns: renderer, isometric orthographic camera + orbit controls, the world
 * (rebuilt per roster/theme), citizens, raycast picking (hover/select/dblclick
 * focus), the selection ring, and HUD anchors (world→screen DOM transforms
 * updated every frame — the HUD renders HTML, never in-canvas text).
 *
 * Data flows one way: setData(citizens, guilds) → engine; interactions flow
 * back via callbacks (onHover/onSelect/onCameraMoved).
 */

interface Anchor {
	key: string;
	target: string;
	el: HTMLElement;
	offsetY: number;
	/** False = decorative (health bars): never intercepts pointer events. */
	interactive: boolean;
	/**
	 * True = this anchor competes for room. Collidable anchors are placed
	 * greedily in priority order and any that would land on an already-placed
	 * peer stands down for the frame. Everything else draws unconditionally.
	 */
	collide: boolean;
	/** Lower wins the overlap. Owned by the HUD, and deliberately NOT derived
	 * from screen position — an order that moves with the camera reshuffles the
	 * survivors on every drift. */
	priority: number;
	// --- per-frame scratch, mutated in place so the render loop allocates nothing
	x: number;
	y: number;
	onScreen: boolean;
	w: number;
	h: number;
	/** Survived the last cull. Also the hysteresis state: what this anchor was
	 * doing last frame decides how hard it is to change its mind. */
	placed: boolean;
}

/** What a caller may say about an anchor beyond where it hangs. */
export interface AnchorOptions {
	offsetY?: number;
	interactive?: boolean;
	collide?: boolean;
	priority?: number;
}

/**
 * Deterministic cull order: importance first, then the anchor key, which is
 * stable for the life of the citizen. The tiebreak matters as much as the
 * priority — two equally important plaques must resolve the same way on every
 * frame, or they trade places as the camera moves.
 */
function byCullOrder(a: Anchor, b: Anchor): number {
	if (a.priority !== b.priority) return a.priority - b.priority;
	return a.key < b.key ? -1 : a.key > b.key ? 1 : 0;
}

/** Do two anchor boxes overlap, padded by `pad` px? Anchors are drawn with
 * translate(-50%, -100%), so the box hangs above-centred on (x, y). */
function anchorsOverlap(a: Anchor, b: Anchor, pad: number): boolean {
	return (
		Math.abs(a.x - b.x) < (a.w + b.w) / 2 + pad &&
		Math.abs(a.y - a.h / 2 - (b.y - b.h / 2)) < (a.h + b.h) / 2 + pad
	);
}

/**
 * The cull's dead band, as the two padding values that bound it (CSS px).
 *
 * A stood-down anchor needs SHOW px of clear air around it before it comes
 * back; a standing one keeps standing until it actually touches a placed peer.
 * The gap between the two is what stops the states chattering frame to frame as
 * citizens walk and the camera drifts — chatter would be worse than the pile-up
 * this cull exists to fix. Keeping the HIDE pad at zero rather than negative is
 * what makes "no two visible plaques overlap" a guarantee instead of a
 * tendency: nothing is allowed to graze its neighbour on the way out.
 */
const ANCHOR_CULL_PAD_SHOW = 12;
const ANCHOR_CULL_PAD_HIDE = 0;

const HOME_POLAR = 1.02; // ~58.5° from vertical → classic elevated iso
const HOME_AZIMUTH = Math.PI / 4;
const HOME_RADIUS = 30;
const FRUSTUM_HALF_H = 10.5;
/** Default framing for the docked campus pane: the whole crew should be
 * visible without navigating. Orbit stays available. */
const HOME_ZOOM = 1.35;
/**
 * Device-pixel cap. 2 is retina-sharp; beyond that costs fragments for nothing.
 *
 * This used to be a deliberate 1/3 DOWNSCALE, upscaled hard by the compositor,
 * on the theory that it produced pixel art. It did not, and the distinction is
 * worth keeping written down: authored pixel art is drawn at low resolution, so
 * every texel is a decision. Downsampling a 3D render instead SAMPLES a scene
 * built at full detail, so what lands is aliasing — geometry that was never
 * designed for the grid, chewed into noise. The reference look this chased is
 * sharp: its pixel identity lives in hand-drawn sprites and a bitmap UI font,
 * over crisply rendered everything-else.
 *
 * The flat toon shading is NOT part of that mistake and stays. Banded colour is
 * why the palette's value-separation rule exists, and it survives at any
 * resolution.
 */
const MAX_PIXEL_RATIO = 2;

export class FleetEngine {
	callbacks: EngineCallbacks = {};

	private host: HTMLElement;
	private canvas: HTMLCanvasElement;
	private renderer: THREE.WebGLRenderer | null = null;
	private scene = new THREE.Scene();
	private camera: THREE.OrthographicCamera;
	private controls: OrbitControls | null = null;
	private citizens: CitizenSystem | null = null;
	private handoffs: HandoffSystem | null = null;
	private environment: EnvironmentSystem | null = null;
	private world: WorldHandles | null = null;
	private village: Village | null = null;
	private palette: WorldPalette | null = null;
	private assets: ThemeAssets | null = null;
	private readonly worldStyle: WorldStyle = CAMPUS_STYLE;
	/** Guards against a slow asset load landing after another theme switch. */
	private assetSeq = 0;

	private citizenVMs: CitizenVM[] = [];
	private guildVMs: GuildVM[] = [];
	private worldSignature = '';

	private raf = 0;
	private paused = false;
	private lastT = 0;
	private disposed = false;

	private raycaster = new THREE.Raycaster();
	private pointerNdc = new THREE.Vector2(-2, -2);
	private pointerDown: { x: number; y: number; t: number } | null = null;
	private hovered: string | null = null;
	// God-Hand: grab a citizen (left button), carry along the ground plane,
	// release over a guild -> onGodHand callback (steer composer in the HUD).
	private drag: { citizenId: string; started: boolean } | null = null;
	private dragTargetGuild: string | null = null;
	private groundPlane = new THREE.Plane(new THREE.Vector3(0, 1, 0), 0);
	private selected: string | null = null;
	private selectionRing: THREE.Mesh;
	private selectionRingMat: THREE.MeshBasicMaterial;

	private anchors = new Map<string, Anchor>();
	/** Reused every frame: the on-screen collidable anchors, in cull order. */
	private cullQueue: Anchor[] = [];
	/** Reused every frame: anchor world→NDC scratch. */
	private anchorProjection = new THREE.Vector3();
	/** Occluder-fade state (structure key -> current opacity), lerped. */
	private fadeCurrent = new Map<string, number>();
	private resizeObs: ResizeObserver | null = null;
	private themeObs: MutationObserver | null = null;
	private appThemeObs: MutationObserver | null = null;
	private cameraMoved = false;
	private focusTarget: THREE.Vector3 | null = null;

	constructor(host: HTMLElement, canvas: HTMLCanvasElement) {
		this.host = host;
		this.canvas = canvas;
		this.camera = new THREE.OrthographicCamera(-1, 1, 1, -1, 0.1, 120);
		this.selectionRingMat = new THREE.MeshBasicMaterial({
			color: '#ffffff',
			transparent: true,
			opacity: 0.9,
			side: THREE.DoubleSide,
			depthWrite: false
		});
		this.selectionRing = new THREE.Mesh(new THREE.RingGeometry(0.36, 0.44, 32), this.selectionRingMat);
		this.selectionRing.rotation.x = -Math.PI / 2;
		this.selectionRing.visible = false;
	}

	init(): void {
		// Sharp rig: antialiased at the device's own ratio. Shadows are filtered
		// rather than hard — an unfiltered shadow map that was fine chunked into
		// third-resolution blocks reads as a jagged edge once the frame is crisp.
		this.renderer = new THREE.WebGLRenderer({ canvas: this.canvas, antialias: true, alpha: true });
		this.renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, MAX_PIXEL_RATIO));
		this.renderer.shadowMap.enabled = true;
		this.renderer.shadowMap.type = THREE.PCFSoftShadowMap;

		this.palette = readWorldPalette(this.host);

		// lights, sun/moon, sky, weather — the environment owns them (real-time
		// day/night from the local clock; deterministic moon phase)
		this.environment = new EnvironmentSystem(
			this.scene,
			this.host.closest('[data-game-theme]') as HTMLElement | null
		);
		this.environment.setPalette(this.palette);
		this.scene.add(this.environment.group, this.selectionRing);
		// real cloud models upgrade the procedural cumuli in place (best-effort)
		void loadCloudModels().then((models) => {
			if (this.renderer) this.environment?.setCloudModels(models);
		});

		// camera home = classic isometric
		this.applyCameraHome();
		this.controls = new OrbitControls(this.camera, this.canvas);
		this.controls.enableDamping = true;
		this.controls.dampingFactor = 0.08;
		this.controls.rotateSpeed = 0.6;
		this.controls.minZoom = 0.4;
		this.controls.maxZoom = 4.5;
		// Game controls: LEFT-drag drags the world (pan), RIGHT-drag orbits,
		// wheel zooms. Touch: one finger pans, two fingers pinch+orbit.
		this.controls.mouseButtons = {
			LEFT: THREE.MOUSE.PAN,
			MIDDLE: THREE.MOUSE.DOLLY,
			RIGHT: THREE.MOUSE.ROTATE
		};
		this.controls.touches = { ONE: THREE.TOUCH.PAN, TWO: THREE.TOUCH.DOLLY_ROTATE };
		// pan along the ground plane (drag the map), not the screen plane
		this.controls.screenSpacePanning = false;
		this.controls.minPolarAngle = 0.6;
		this.controls.maxPolarAngle = 1.25;
		this.controls.addEventListener('start', () => {
			this.focusTarget = null; // user takes over
		});

		this.citizens = new CitizenSystem(this.palette);
		this.citizens.setStyle(this.worldStyle);
		this.scene.add(this.citizens.group);
		this.handoffs = new HandoffSystem();
		this.handoffs.setColor(this.palette.handoff);
		this.scene.add(this.handoffs.group);

		this.resize();
		this.resizeObs = new ResizeObserver(() => this.resize());
		this.resizeObs.observe(this.host);

		// re-read the world palette when the GAME theme (hero attr) or app theme changes
		this.themeObs = new MutationObserver(() => this.onThemeChange());
		this.themeObs.observe(this.host.closest('[data-game-theme]') ?? this.host, {
			attributes: true,
			attributeFilter: ['data-game-theme']
		});
		this.appThemeObs = new MutationObserver(() => this.onThemeChange());
		this.appThemeObs.observe(document.documentElement, {
			attributes: true,
			attributeFilter: ['data-theme']
		});

		this.canvas.addEventListener('pointermove', this.onPointerMove);
		this.canvas.addEventListener('pointerdown', this.onPointerDown);
		this.canvas.addEventListener('pointerup', this.onPointerUp);
		this.canvas.addEventListener('pointerleave', this.onPointerLeave);
		this.canvas.addEventListener('dblclick', this.onDblClick);
		// right-drag orbits — keep the browser context menu off the canvas
		this.canvas.addEventListener('contextmenu', this.onContextMenu);

		this.lastT = performance.now();
		this.raf = requestAnimationFrame(this.loop);

		// The campus models + characters (CC0, bundled) load in the background;
		// the procedural world renders immediately and upgrades in place.
		void this.loadAssetsForTheme();
	}

	private async loadAssetsForTheme(): Promise<void> {
		const seq = ++this.assetSeq;
		const pack = this.worldStyle.assetPack;
		const [assets, characters] = await Promise.all([
			pack ? loadThemeAssets(pack) : Promise.resolve(null),
			loadCharacters(CAMPUS_CHARACTERS)
		]);
		if (this.disposed || seq !== this.assetSeq) return; // theme changed meanwhile
		this.assets = assets;
		this.citizens?.setCharacters(characters.length > 0 ? characters : null);
		if (this.village) this.rebuildWorld();
	}

	setPaused(p: boolean): void {
		this.paused = p;
	}

	setData(citizens: CitizenVM[], guilds: GuildVM[]): void {
		this.citizenVMs = citizens;
		this.guildVMs = guilds;
		const signature =
			guilds
				.map((guild) =>
					`${guild.id}:${guild.activeQuestCount}:${guild.blockedQuestCount}:${guild.deliveredQuestCount}`
				)
				.join('|') +
			'::' +
			citizens.map((citizen) => citizen.id).sort().join('|');
		if (signature !== this.worldSignature) {
			this.worldSignature = signature;
			this.rebuildWorld();
		}
		this.citizens?.sync(citizens);
	}

	/** Live delegation edges → hand-off beams between citizens. */
	setHandoffs(edges: HandoffEdge[]): void {
		this.handoffs?.sync(edges);
	}

	/** Local weather → clouds/rain/snow in the world. */
	setWeather(weather: WeatherState): void {
		this.environment?.setWeather(weather);
	}

	/** Observer coordinates → TRUE solar/lunar positions in the sky. */
	setObserver(lat: number, lon: number): void {
		this.environment?.setObserver(lat, lon);
	}

	/** Raycast at host-local pixel coords (HUD drag-drops, e.g. quest scrolls). */
	pickAt(hostX: number, hostY: number): string | null {
		const w = this.host.clientWidth;
		const h = this.host.clientHeight;
		if (w === 0 || h === 0) return null;
		this.pointerNdc.set((hostX / w) * 2 - 1, -((hostY / h) * 2 - 1));
		return this.pick();
	}

	select(target: string | null): void {
		if (this.selected === target) return;
		this.selected = target;
		this.callbacks.onSelect?.(target);
	}

	/** The character template a citizen wears — the character sheet's portrait
	 * viewport clones it (null until the theme's characters load). */
	characterTemplateFor(id: string): CharacterTemplate | null {
		return this.citizens?.characterTemplateFor(id) ?? null;
	}

	// --- minimap / HUD instrumentation ---

	/** The generated village (tiles + plots) — the minimap's static layer. */
	minimapVillage(): Village | null {
		return this.village;
	}

	/** Citizen ground positions + vibes — the minimap's dynamic layer. */
	minimapDots(): { id: string; x: number; z: number; vibe: string }[] {
		return this.citizens?.dots() ?? [];
	}

	/** Current look-target + visible half-extent (the minimap view rect). */
	viewInfo(): { x: number; z: number; half: number } {
		const t = this.controls?.target;
		const aspect = this.host.clientWidth / Math.max(1, this.host.clientHeight);
		const half = (FRUSTUM_HALF_H * Math.max(1, aspect)) / Math.max(0.1, this.camera.zoom);
		return { x: t?.x ?? 0, z: t?.z ?? 0, half };
	}

	/** Camera azimuth (radians) — drives the HUD compass needle. */
	cameraAzimuth(): number {
		return this.controls?.getAzimuthalAngle() ?? HOME_AZIMUTH;
	}

	/** Fly the camera to a world point (minimap click-to-jump). */
	focusWorld(x: number, z: number): void {
		const bound = (this.village?.n ?? 24) / 2 - 3;
		this.focusTarget = new THREE.Vector3(
			THREE.MathUtils.clamp(x, -bound, bound),
			0,
			THREE.MathUtils.clamp(z, -bound, bound)
		);
	}

	focus(target: string): void {
		const pos = this.targetWorldPos(target);
		if (pos) this.focusTarget = new THREE.Vector3(pos.x, 0, pos.z);
	}

	resetView(): void {
		this.focusTarget = null;
		this.applyCameraHome();
		if (this.controls) {
			this.controls.target.set(0, 0, 0);
			this.controls.update();
		}
	}

	registerAnchor(key: string, target: string, el: HTMLElement, opts: AnchorOptions = {}): void {
		const offsetY = opts.offsetY ?? 1.0;
		const interactive = opts.interactive ?? true;
		const collide = opts.collide ?? false;
		const priority = opts.priority ?? 0;
		const existing = this.anchors.get(key);
		if (existing) {
			// The HUD re-registers a key whenever any of its params change
			// (selection, vibe). Updating in place keeps `placed`, so a
			// selection change does not blink every plaque that was standing.
			existing.target = target;
			existing.el = el;
			existing.offsetY = offsetY;
			existing.interactive = interactive;
			existing.collide = collide;
			existing.priority = priority;
			return;
		}
		this.anchors.set(key, {
			key,
			target,
			el,
			offsetY,
			interactive,
			collide,
			priority,
			x: 0,
			y: 0,
			onScreen: false,
			w: 0,
			h: 0,
			placed: false
		});
	}

	unregisterAnchor(key: string): void {
		this.anchors.delete(key);
	}

	dispose(): void {
		this.disposed = true;
		cancelAnimationFrame(this.raf);
		this.canvas.removeEventListener('pointermove', this.onPointerMove);
		this.canvas.removeEventListener('pointerdown', this.onPointerDown);
		this.canvas.removeEventListener('pointerup', this.onPointerUp);
		this.canvas.removeEventListener('pointerleave', this.onPointerLeave);
		this.canvas.removeEventListener('dblclick', this.onDblClick);
		this.canvas.removeEventListener('contextmenu', this.onContextMenu);
		this.resizeObs?.disconnect();
		this.themeObs?.disconnect();
		this.appThemeObs?.disconnect();
		this.controls?.dispose();
		this.world?.dispose();
		this.citizens?.dispose();
		this.handoffs?.dispose();
		this.environment?.dispose();
		this.selectionRing.geometry.dispose();
		this.selectionRingMat.dispose();
		this.renderer?.dispose();
		this.renderer = null;
	}

	// --- internals ---

	private applyCameraHome(): void {
		const t = new THREE.Vector3(0, 0, 0);
		const sp = new THREE.Spherical(HOME_RADIUS, HOME_POLAR, HOME_AZIMUTH);
		const pos = new THREE.Vector3().setFromSpherical(sp).add(t);
		this.camera.position.copy(pos);
		this.camera.zoom = HOME_ZOOM;
		this.camera.lookAt(t);
		this.camera.updateProjectionMatrix();
	}

	private rebuildWorld(): void {
		if (!this.palette) return;
		if (this.world) {
			this.scene.remove(this.world.group);
			this.world.dispose();
			this.world = null;
		}
		this.village = buildVillage(this.citizenVMs, this.guildVMs);
		this.fadeCurrent.clear(); // fresh materials start fully opaque
		this.world = buildCivWorld(this.village, this.guildVMs, this.palette, this.assets, this.worldStyle);
		// flat look: swap the built world onto the stepped toon ramp, then
		// re-point the occluder-fade list — it captured the materials that were
		// on the meshes a moment ago, and those no longer render.
		const swapped = flattenMaterials(this.world.group);
		for (const f of this.world.fadeables) f.mats = f.mats.map((m) => swapped.get(m) ?? m);
		this.scene.add(this.world.group);
		// No distance fog: it exists to soften a far horizon, and softening is
		// exactly what a flat, low-resolution frame cannot render — it arrives
		// as banded mush over the back half of the realm.
		this.scene.fog = null;
		this.citizens?.setVillage(this.village, this.guildVMs.map((g) => g.id));
	}

	private onThemeChange(): void {
		// read the THEME's own sky, not the environment's night-blended override
		this.environment?.clearSkyOverride();
		this.palette = readWorldPalette(this.host);
		// drop the cached models so the world repaints procedurally under the new
		// palette, then upgrades again when the (cached) pack resolves
		this.assets = null;
		this.citizens?.setPalette(this.palette);
		this.citizens?.setStyle(this.worldStyle);
		this.handoffs?.setColor(this.palette.handoff);
		this.environment?.setPalette(this.palette);
		this.rebuildWorld();
		void this.loadAssetsForTheme();
	}

	private resize(): void {
		if (!this.renderer) return;
		const w = this.host.clientWidth;
		const h = this.host.clientHeight;
		if (w === 0 || h === 0) return;
		this.renderer.setSize(w, h, false);
		const aspect = w / h;
		this.camera.left = -FRUSTUM_HALF_H * aspect;
		this.camera.right = FRUSTUM_HALF_H * aspect;
		this.camera.top = FRUSTUM_HALF_H;
		this.camera.bottom = -FRUSTUM_HALF_H;
		this.camera.updateProjectionMatrix();
	}

	private targetWorldPos(target: string): THREE.Vector3 | null {
		const parsed = parseTarget(target);
		if (!parsed) return null;
		if (parsed.kind === 'agent') return this.citizens?.worldPos(parsed.id) ?? null;
		if (parsed.kind === 'landmark')
			return this.world?.landmarkCenterOf.get(parsed.id)?.clone() ?? null;
		return this.world?.buildingCenterOf.get(parsed.id)?.clone() ?? null;
	}

	private pick(): string | null {
		if (!this.citizens) return null;
		if (this.pointerNdc.x < -1) return null;
		this.raycaster.setFromCamera(this.pointerNdc, this.camera);
		const citizenHit = this.raycaster.intersectObjects(this.citizens.pickMeshes(), false)[0];
		const buildingHit = this.world
			? this.raycaster.intersectObjects(this.world.buildingPickMeshes, false)[0]
			: undefined;
		const landmarkHit = this.world
			? this.raycaster.intersectObjects(this.world.landmarkPickMeshes, false)[0]
			: undefined;
		const structureHit =
			buildingHit && landmarkHit
				? buildingHit.distance <= landmarkHit.distance
					? buildingHit
					: landmarkHit
				: (buildingHit ?? landmarkHit);
		if (citizenHit && (!structureHit || citizenHit.distance <= structureHit.distance)) {
			const id = citizenHit.object.userData.citizenId as string | undefined;
			return id ? agentTarget(id) : null;
		}
		if (structureHit) {
			const guildId = structureHit.object.userData.guildId as string | undefined;
			if (guildId) return guildTarget(guildId);
			const landmarkId = structureHit.object.userData.landmarkId as LandmarkId | undefined;
			if (landmarkId) return landmarkTarget(landmarkId);
		}
		return null;
	}

	private onPointerMove = (e: PointerEvent): void => {
		const r = this.canvas.getBoundingClientRect();
		this.pointerNdc.set(((e.clientX - r.left) / r.width) * 2 - 1, -(((e.clientY - r.top) / r.height) * 2 - 1));
		if (this.drag && this.pointerDown) {
			if (!this.drag.started) {
				const moved = Math.hypot(e.clientX - this.pointerDown.x, e.clientY - this.pointerDown.y);
				if (moved > 6) {
					this.drag.started = true;
					this.citizens?.beginDrag(this.drag.citizenId);
					this.host.style.cursor = 'grabbing';
				}
			}
			if (this.drag.started) {
				const p = this.groundPoint();
				if (p) this.citizens?.dragTo(this.drag.citizenId, p.x, p.z);
				this.dragTargetGuild = this.pickGuild();
			}
		}
	};

	private groundPoint(): THREE.Vector3 | null {
		this.raycaster.setFromCamera(this.pointerNdc, this.camera);
		const p = new THREE.Vector3();
		return this.raycaster.ray.intersectPlane(this.groundPlane, p) ? p : null;
	}

	private pickGuild(): string | null {
		if (!this.world) return null;
		this.raycaster.setFromCamera(this.pointerNdc, this.camera);
		const hit = this.raycaster.intersectObjects(this.world.buildingPickMeshes, false)[0];
		const id = hit?.object.userData.guildId as string | undefined;
		return id ?? null;
	}

	private onPointerLeave = (): void => {
		this.pointerNdc.set(-2, -2);
	};

	private onPointerDown = (e: PointerEvent): void => {
		this.pointerDown = { x: e.clientX, y: e.clientY, t: performance.now() };
		// Grabbing a PERSON (left button) is God-Hand, not map panning.
		if (e.button === 0) {
			const r = this.canvas.getBoundingClientRect();
			this.pointerNdc.set(
				((e.clientX - r.left) / r.width) * 2 - 1,
				-(((e.clientY - r.top) / r.height) * 2 - 1)
			);
			const hit = this.pick();
			const parsed = hit ? parseTarget(hit) : null;
			if (parsed?.kind === 'agent') {
				this.drag = { citizenId: parsed.id, started: false };
				if (this.controls) this.controls.enabled = false;
				// keep receiving move/up even if the pointer leaves the canvas
				try {
					this.canvas.setPointerCapture(e.pointerId);
				} catch {
					/* capture unsupported — drop still works inside the canvas */
				}
			}
		}
	};

	private onPointerUp = (e: PointerEvent): void => {
		const d = this.pointerDown;
		this.pointerDown = null;

		// God-Hand release
		const drag = this.drag;
		this.drag = null;
		if (this.controls) this.controls.enabled = true;
		if (drag) {
			this.host.style.cursor = '';
			if (drag.started) {
				this.citizens?.endDrag(drag.citizenId);
				const guildId = this.dragTargetGuild;
				this.dragTargetGuild = null;
				const r = this.canvas.getBoundingClientRect();
				this.callbacks.onGodHand?.({
					citizenId: drag.citizenId,
					guildId,
					screenX: e.clientX - r.left,
					screenY: e.clientY - r.top
				});
				return; // a carry is never a click
			}
		}

		if (!d) return;
		const moved = Math.hypot(e.clientX - d.x, e.clientY - d.y);
		const dt = performance.now() - d.t;
		if (moved > 5 || dt > 450) return; // was a drag, not a click
		this.select(this.hovered);
	};

	private onDblClick = (): void => {
		if (this.hovered) this.focus(this.hovered);
	};

	private onContextMenu = (e: Event): void => {
		e.preventDefault();
	};

	private loop = (nowMs: number): void => {
		if (this.disposed) return;
		this.raf = requestAnimationFrame(this.loop);
		if (this.paused || !this.renderer) return;

		const now = nowMs / 1000;
		const dt = Math.min((nowMs - this.lastT) / 1000, 0.05);
		this.lastT = nowMs;

		// focus tween (cancelled the moment the user grabs the controls)
		if (this.focusTarget && this.controls) {
			this.controls.target.lerp(this.focusTarget, 0.12);
			if (this.controls.target.distanceTo(this.focusTarget) < 0.05) this.focusTarget = null;
		}
		// keep the target inside the village
		if (this.controls) {
			const bound = (this.village?.n ?? 24) / 2 - 3;
			this.controls.target.x = THREE.MathUtils.clamp(this.controls.target.x, -bound, bound);
			this.controls.target.z = THREE.MathUtils.clamp(this.controls.target.z, -bound, bound);
			this.controls.update();
		}

		// shared visibility set: off-screen citizens skip animation/cosmetics
		// (simulation-LOD, the strategy-game pattern) and occlusion raycasts
		const visible = this.computeVisibleCitizens();
		this.citizens?.update(dt, now, visible);
		this.handoffs?.update(now, (id) => this.citizens?.worldPos(id) ?? null);
		this.environment?.update(dt, {
			half: this.shadowViewHalf(),
			target: this.controls?.target ?? null,
			cameraPos: this.camera.position
		});

		// hover picking (suppressed while the God-Hand is carrying someone)
		const carrying = this.drag?.started === true;
		const hit = carrying ? null : this.pick();
		if (hit !== this.hovered) {
			this.hovered = hit;
			if (!carrying) this.host.style.cursor = hit ? 'pointer' : '';
			this.callbacks.onHover?.(hit);
		}

		// God-Hand drop-target ring takes priority over the selection ring
		if (carrying) {
			const guildPos = this.dragTargetGuild
				? this.world?.buildingCenterOf.get(this.dragTargetGuild)
				: null;
			if (guildPos) {
				this.selectionRing.visible = true;
				this.selectionRing.position.set(guildPos.x, 0.03, guildPos.z);
				this.selectionRing.scale.set(3.2, 3.2, 3.2);
				this.selectionRingMat.opacity = 0.65 + Math.sin(now * 6) * 0.3;
			} else {
				this.selectionRing.visible = false;
			}
		} else if (this.selected) {
			const pos = this.targetWorldPos(this.selected);
			if (pos) {
				this.selectionRing.visible = true;
				const isGuild = this.selected.startsWith('guild:');
				this.selectionRing.position.set(pos.x, 0.03, pos.z);
				const s = isGuild ? 3.2 : 1;
				this.selectionRing.scale.set(s, s, s);
				this.selectionRingMat.opacity = 0.6 + Math.sin(now * 4) * 0.25;
			} else {
				this.selectionRing.visible = false;
			}
		} else {
			this.selectionRing.visible = false;
		}

		// camera-moved (drives the HUD Home control)
		const moved =
			(this.controls?.target.lengthSq() ?? 0) > 0.04 ||
			Math.abs(this.camera.zoom - HOME_ZOOM) > 0.05;
		if (moved !== this.cameraMoved) {
			this.cameraMoved = moved;
			this.callbacks.onCameraMoved?.(moved);
		}

		this.updateOcclusionFade(dt, visible);
		this.updateAnchors();
		this.renderer.render(this.scene, this.camera);
	};

	/** Citizens whose anchor point projects inside the view (small margin). */
	private computeVisibleCitizens(): Set<string> {
		const visible = new Set<string>();
		if (!this.citizens) return visible;
		const v = new THREE.Vector3();
		for (const vm of this.citizenVMs) {
			const pos = this.citizens.worldPos(vm.id);
			if (!pos) continue;
			v.set(pos.x, pos.y + 0.4, pos.z).project(this.camera);
			if (v.z <= 1 && Math.abs(v.x) <= 1.15 && Math.abs(v.y) <= 1.15) visible.add(vm.id);
		}
		return visible;
	}

	/** Shadow window half-extent fitted to the current view (+margin) — the
	 * shadow map stops re-rendering the whole 48x48 world when zoomed in. */
	private shadowViewHalf(): number {
		const aspect = this.host.clientWidth / Math.max(1, this.host.clientHeight);
		const half = (FRUSTUM_HALF_H * Math.max(1, aspect)) / Math.max(0.1, this.camera.zoom);
		return THREE.MathUtils.clamp(half * 1.35, 8, 30);
	}

	/**
	 * Sims/BG3-style occluder fade: any structure standing between the camera
	 * and a citizen turns translucent (smoothly) and restores when clear. Rays
	 * go through each citizen's screen position (correct for the ortho camera);
	 * hits nearer along the ray than the citizen mark the structure occluding.
	 * The citizens' own GreaterDepth ghosts cover everything else (trees etc.).
	 */
	private updateOcclusionFade(dt: number, visible: ReadonlySet<string>): void {
		const world = this.world;
		if (!world || !this.citizens || world.fadeables.length === 0) return;
		const proxies = world.buildingPickMeshes.concat(world.landmarkPickMeshes);
		if (proxies.length === 0) return;

		const occluding = new Set<string>();
		const target = new THREE.Vector3();
		const ndc = new THREE.Vector3();
		for (const vm of this.citizenVMs) {
			if (!visible.has(vm.id)) continue; // off-screen: no raycasts
			const pos = this.citizens.worldPos(vm.id);
			if (!pos) continue;
			target.set(pos.x, pos.y + 0.4, pos.z);
			ndc.copy(target).project(this.camera);
			if (ndc.z > 1 || Math.abs(ndc.x) > 1.05 || Math.abs(ndc.y) > 1.05) continue;
			this.raycaster.setFromCamera(new THREE.Vector2(ndc.x, ndc.y), this.camera);
			const citizenDist = target
				.clone()
				.sub(this.raycaster.ray.origin)
				.dot(this.raycaster.ray.direction);
			for (const hit of this.raycaster.intersectObjects(proxies, false)) {
				if (hit.distance >= citizenDist - 0.45) continue;
				const guildId = hit.object.userData.guildId as string | undefined;
				const landmarkId = hit.object.userData.landmarkId as string | undefined;
				if (guildId) occluding.add(`guild:${guildId}`);
				else if (landmarkId) occluding.add(`landmark:${landmarkId}`);
			}
		}

		const k = Math.min(1, dt * 6);
		for (const f of world.fadeables) {
			const want = occluding.has(f.key) ? 0.25 : 1;
			const cur = this.fadeCurrent.get(f.key) ?? 1;
			if (cur === want) continue;
			const next = Math.abs(want - cur) < 0.015 ? want : cur + (want - cur) * k;
			this.fadeCurrent.set(f.key, next);
			for (const m of f.mats) m.opacity = next;
		}
	}

	/**
	 * Project every anchor to screen space, decide which of the competing ones
	 * get to stand, and drive the DOM transforms.
	 *
	 * Three passes on purpose: read the scene and measure the elements first,
	 * decide second, write styles last. Measuring between the transform writes
	 * would force a layout flush per anchor, every frame.
	 */
	private updateAnchors(): void {
		if (this.anchors.size === 0 || !this.renderer) return;
		const w = this.host.clientWidth;
		const h = this.host.clientHeight;
		const v = this.anchorProjection;

		// 1. project, and measure the ones that compete for room (reads only)
		const queue = this.cullQueue;
		queue.length = 0;
		for (const a of this.anchors.values()) {
			const pos = this.targetWorldPos(a.target);
			if (!pos) {
				a.onScreen = false;
				a.placed = false;
				continue;
			}
			v.set(pos.x, pos.y + a.offsetY, pos.z).project(this.camera);
			a.x = (v.x * 0.5 + 0.5) * w;
			a.y = (-v.y * 0.5 + 0.5) * h;
			a.onScreen = v.z < 1 && a.x > -40 && a.x < w + 40 && a.y > -40 && a.y < h + 40;
			if (!a.collide) continue;
			if (!a.onScreen) {
				a.placed = false;
				continue;
			}
			a.w = a.el.offsetWidth;
			a.h = a.el.offsetHeight;
			queue.push(a);
		}

		// 2. cull: place greedily from the most important down. n is a crew, so
		// the quadratic inner walk is a handful of comparisons.
		if (queue.length === 1) {
			queue[0].placed = true;
		} else if (queue.length > 1) {
			queue.sort(byCullOrder);
			let placed = 0;
			for (let i = 0; i < queue.length; i++) {
				const a = queue[i];
				const pad = a.placed ? ANCHOR_CULL_PAD_HIDE : ANCHOR_CULL_PAD_SHOW;
				let blocked = false;
				for (let j = 0; j < placed; j++) {
					if (anchorsOverlap(a, queue[j], pad)) {
						blocked = true;
						break;
					}
				}
				a.placed = !blocked;
				if (blocked) continue;
				// Keep the winners packed into [0, placed) so the inner walk
				// only visits them — a swap, not a second list.
				queue[i] = queue[placed];
				queue[placed] = a;
				placed++;
			}
		}

		// 3. write
		for (const a of this.anchors.values()) {
			const visible = a.onScreen && (!a.collide || a.placed);
			a.el.style.opacity = visible ? '1' : '0';
			a.el.style.pointerEvents = visible && a.interactive ? 'auto' : 'none';
			if (!a.onScreen) continue;
			a.el.style.transform = `translate3d(${a.x}px, ${a.y}px, 0) translate(-50%, -100%)`;
		}
	}
}
