import * as THREE from 'three';
import { mergeGeometries } from 'three/examples/jsm/utils/BufferGeometryUtils.js';
import type { WorldPalette } from './types';
import { sunPosition, moonPosition } from './astro';

/**
 * EnvironmentSystem — real-time day/night, sun & moon, and weather.
 *
 * - The local wall clock drives the sun's arc every frame; lighting follows
 *   (warm noon -> amber dusk -> soft blue night). The hero's sky gradient is
 *   blended toward night by writing --fleet-sky-* inline on the hero element
 *   (base values come from the theme palette the engine passes in).
 * - The MOON PHASE is deterministic: days since a known new-moon epoch mod the
 *   synodic month position the moon relative to the sun (full moon = opposite,
 *   new moon = alongside), and because the moon is a real sphere lit by the
 *   sun's directional light, the crescent shape/size renders physically
 *   correct for the actual date — today and henceforth.
 * - Weather (set externally from Open-Meteo data): puffy multi-lobe clouds
 *   confined to a horizon band OUTSIDE the playfield (they orbit the realm,
 *   never crossing over the village) with a camera-aware near-sector fade so
 *   no tilt or orbit ever puts one between you and the crew. Cover over the
 *   village itself reads through drifting ground shadow-patches (the RTS
 *   trick) plus the dimmed sun. Rain/snow remain overhead particle fields.
 * - Night stays WELL LIT by design: the key light is floored at
 *   NIGHT_LIGHT_FLOOR and held above the horizon, the sky may only blend
 *   NIGHT_SKY_BLEND_MAX of the way to black, and a soft blue moonlight fills
 *   in — gameplay legibility beats realism.
 */

const SYNODIC_MONTH_DAYS = 29.530588853;
/** A known new moon: 2000-01-06 18:14 UTC. */
const NEW_MOON_EPOCH_MS = Date.UTC(2000, 0, 6, 18, 14, 0);

export function moonPhase(nowMs: number): number {
	const days = (nowMs - NEW_MOON_EPOCH_MS) / 86_400_000;
	return ((days / SYNODIC_MONTH_DAYS) % 1 + 1) % 1; // 0 new .. 0.5 full .. ->1 new
}

export type WeatherKind = 'clear' | 'clouds' | 'rain' | 'snow';

export interface WeatherState {
	kind: WeatherKind;
	/** 0..1 — drives cloud opacity/count even when kind is rain/snow. */
	cloudCover: number;
	/** 0..1 precipitation strength (WMO light/moderate/heavy bands) — scales
	 * particle count, fall speed, and opacity; 0 when kind is clear/clouds. */
	intensity: number;
}

const SKY_RADIUS = 34;
const NIGHT_TOP = new THREE.Color('#0a1228');
const NIGHT_BOTTOM = new THREE.Color('#141e38');
/** Night is a STYLIZED blue hour, never a blackout. The world keeps this much
 * of its daylight however far below the horizon the real sun is: flat toon
 * bands have no gradient to hide in, so an unlit frame is not a moody frame —
 * it is a muddy one. Legibility beats realism (see the class note). */
const NIGHT_LIGHT_FLOOR = 0.72;
/** How far the sky gradient may travel toward NIGHT_* — full blend drowns the
 * hero background that the whole page is composed against. */
const NIGHT_SKY_BLEND_MAX = 0.45;
/** The key light never rakes in from below the horizon: past sunset the sun's
 * true position would light every roof from underneath. The sun MESH still
 * sets/rises truthfully — only the lighting rig is kept overhead. */
const KEY_LIGHT_MIN_Y = SKY_RADIUS * 0.34;
/** Most the cloud deck may take off the key light at full overcast. */
const CLOUD_DIM_MAX = 0.2;
const RAIN_COUNT = 1600;
const SNOW_COUNT = 700;
const CLOUD_COUNT = 10;
/** Clouds live in this ring, strictly outside the ±24 playfield. */
const CLOUD_BAND_INNER = 27;
const CLOUD_BAND_OUTER = 33;
const CLOUD_SHADOW_COUNT = 6;
/** Half-extent the ground shadow-patches drift across (the playfield). */
const CLOUD_SHADOW_WRAP = 26;

/** A cartoon cumulus: a row of overlapping flattened lobes, biggest in the
 * middle, merged into one geometry (one draw call per cloud). */
function makeCloudGeometry(): THREE.BufferGeometry {
	const lobes = 4 + Math.floor(Math.random() * 3);
	const parts: THREE.BufferGeometry[] = [];
	let cx = 0;
	for (let p = 0; p < lobes; p++) {
		const t = lobes === 1 ? 0.5 : p / (lobes - 1);
		const mid = 1 - Math.abs(t - 0.5) * 2; // 0 at the ends, 1 in the middle
		const r = 0.55 + mid * 0.75 + Math.random() * 0.2;
		const g = new THREE.SphereGeometry(r, 7, 6);
		g.scale(1.15, 0.8, 1);
		g.translate(cx, r * 0.22, (Math.random() - 0.5) * 0.7);
		parts.push(g);
		cx += r * (1.05 + Math.random() * 0.25);
	}
	const merged = mergeGeometries(parts);
	for (const g of parts) g.dispose();
	if (!merged) return new THREE.SphereGeometry(1.4, 7, 6);
	merged.center();
	return merged;
}

interface Cloud {
	/** Procedural cumulus mesh, or a cloud-model clone once assets arrive. */
	node: THREE.Object3D;
	/** Every material of the node, cloned per cloud so fades are independent. */
	mats: THREE.MeshStandardMaterial[];
	/** True while the node is the procedural mesh (geometry is ours to free);
	 * model clones share geometry with the cached template — never disposed. */
	ownedGeometry: boolean;
	/** Orbit angle around the realm centre (radians). */
	angle: number;
	radius: number;
	height: number;
	/** Orbital drift speed (rad/s). */
	speed: number;
	/** Smoothed near-sector fade 0..1. */
	fade: number;
	/** Smoothed cover-driven activation 0..1. */
	vis: number;
}

export class EnvironmentSystem {
	readonly group = new THREE.Group();

	private hemi: THREE.HemisphereLight;
	private sunLight: THREE.DirectionalLight;
	private moonLight: THREE.DirectionalLight;
	private sunMesh: THREE.Mesh;
	private sunMat: THREE.MeshBasicMaterial;
	private moonMesh: THREE.Mesh;
	private moonMat: THREE.MeshStandardMaterial;

	private clouds: Cloud[] = [];
	/** Smoothed base cloud opacity (cover-driven, shared by all clouds). */
	private cloudOpacity = 0;
	private cloudShadows: THREE.Mesh[] = [];
	private cloudShadowGeo: THREE.PlaneGeometry;
	private cloudShadowMat: THREE.MeshBasicMaterial;
	private cloudShadowTex: THREE.CanvasTexture;
	private rain: THREE.Points;
	private rainMat: THREE.PointsMaterial;
	private rainVel: Float32Array;
	private snow: THREE.Points;
	private snowMat: THREE.PointsMaterial;
	private snowPhase: Float32Array;

	private heroEl: HTMLElement | null;
	private baseTop = new THREE.Color('#a9ddf2');
	private baseBottom = new THREE.Color('#e9f5d8');
	private scene: THREE.Scene;
	private weather: WeatherState = { kind: 'clear', cloudCover: 0.15, intensity: 0 };
	/** Observer coordinates — when set, TRUE solar/lunar positions are used
	 * (SunCalc-style astronomy); otherwise the stylized clock arc. */
	private observer: { lat: number; lon: number } | null = null;

	constructor(scene: THREE.Scene, heroEl: HTMLElement | null) {
		this.scene = scene;
		this.heroEl = heroEl;

		this.hemi = new THREE.HemisphereLight('#dceeff', '#9db76a', 0.85);
		this.sunLight = new THREE.DirectionalLight('#fff3dd', 1.6);
		this.sunLight.castShadow = true;
		this.sunLight.shadow.mapSize.set(2048, 2048);
		this.sunLight.shadow.camera.left = -30;
		this.sunLight.shadow.camera.right = 30;
		this.sunLight.shadow.camera.top = 30;
		this.sunLight.shadow.camera.bottom = -30;
		this.sunLight.shadow.bias = -0.0004;
		this.moonLight = new THREE.DirectionalLight('#9db4ff', 0);
		// the light TARGET must live in the scene so the shadow box can follow
		// the current view (big-map hygiene: shadows render the view, not the world)
		this.group.add(this.hemi, this.sunLight, this.sunLight.target, this.moonLight);

		this.sunMat = new THREE.MeshBasicMaterial({ color: '#ffd76a', fog: false });
		this.sunMesh = new THREE.Mesh(new THREE.SphereGeometry(1.3, 20, 16), this.sunMat);
		// The moon is DELIBERATELY a lit sphere: the sun's light carves the real
		// crescent for the actual date. A touch of emissive keeps it readable.
		this.moonMat = new THREE.MeshStandardMaterial({
			color: '#e8ecf4',
			roughness: 1,
			emissive: new THREE.Color('#5a6480'),
			emissiveIntensity: 0.25,
			fog: false
		});
		this.moonMesh = new THREE.Mesh(new THREE.SphereGeometry(0.95, 20, 16), this.moonMat);
		this.group.add(this.sunMesh, this.moonMesh);

		// clouds — cartoon cumuli orbiting the horizon band, never over the
		// village; per-cloud materials so the near-sector fade is independent
		for (let i = 0; i < CLOUD_COUNT; i++) {
			const mat = new THREE.MeshStandardMaterial({
				color: '#ffffff',
				roughness: 1,
				flatShading: true,
				transparent: true,
				opacity: 0,
				depthWrite: false,
				emissive: new THREE.Color('#b8c4d8'),
				emissiveIntensity: 0.35
			});
			const mesh = new THREE.Mesh(makeCloudGeometry(), mat);
			mesh.scale.setScalar(1.6 + Math.random() * 1.6);
			mesh.rotation.y = Math.random() * Math.PI * 2;
			mesh.castShadow = false;
			mesh.visible = false;
			this.clouds.push({
				node: mesh,
				mats: [mat],
				ownedGeometry: true,
				angle: (i / CLOUD_COUNT) * Math.PI * 2 + Math.random() * 0.5,
				radius: CLOUD_BAND_INNER + Math.random() * (CLOUD_BAND_OUTER - CLOUD_BAND_INNER),
				height: 12 + Math.random() * 5,
				speed: 0.006 + Math.random() * 0.014,
				fade: 1,
				vis: 0
			});
			this.group.add(mesh);
		}

		// ground shadow-patches — soft dark blobs drifting across the village
		// stand in for overhead cover (real clouds stay at the horizon)
		const cnv = document.createElement('canvas');
		cnv.width = cnv.height = 128;
		const cctx = cnv.getContext('2d');
		if (cctx) {
			const grad = cctx.createRadialGradient(64, 64, 8, 64, 64, 62);
			grad.addColorStop(0, 'rgba(0,0,0,0.5)');
			grad.addColorStop(0.65, 'rgba(0,0,0,0.28)');
			grad.addColorStop(1, 'rgba(0,0,0,0)');
			cctx.fillStyle = grad;
			cctx.fillRect(0, 0, 128, 128);
		}
		this.cloudShadowTex = new THREE.CanvasTexture(cnv);
		this.cloudShadowGeo = new THREE.PlaneGeometry(1, 1);
		this.cloudShadowMat = new THREE.MeshBasicMaterial({
			map: this.cloudShadowTex,
			transparent: true,
			opacity: 0,
			depthWrite: false
		});
		for (let i = 0; i < CLOUD_SHADOW_COUNT; i++) {
			const patch = new THREE.Mesh(this.cloudShadowGeo, this.cloudShadowMat);
			patch.rotation.x = -Math.PI / 2;
			patch.scale.set(7 + Math.random() * 5, 4.5 + Math.random() * 3.5, 1);
			patch.position.set(
				Math.random() * CLOUD_SHADOW_WRAP * 2 - CLOUD_SHADOW_WRAP,
				0.05,
				Math.random() * CLOUD_SHADOW_WRAP * 2 - CLOUD_SHADOW_WRAP
			);
			patch.visible = false;
			this.cloudShadows.push(patch);
			this.group.add(patch);
		}

		// rain
		const rainGeo = new THREE.BufferGeometry();
		const rainPos = new Float32Array(RAIN_COUNT * 3);
		this.rainVel = new Float32Array(RAIN_COUNT);
		for (let i = 0; i < RAIN_COUNT; i++) {
			rainPos[i * 3] = Math.random() * 44 - 22;
			rainPos[i * 3 + 1] = Math.random() * 12;
			rainPos[i * 3 + 2] = Math.random() * 44 - 22;
			this.rainVel[i] = 11 + Math.random() * 5;
		}
		rainGeo.setAttribute('position', new THREE.BufferAttribute(rainPos, 3));
		this.rainMat = new THREE.PointsMaterial({
			color: '#9fc4e8',
			size: 0.07,
			transparent: true,
			opacity: 0.7,
			depthWrite: false
		});
		this.rain = new THREE.Points(rainGeo, this.rainMat);
		this.rain.visible = false;
		this.group.add(this.rain);

		// snow
		const snowGeo = new THREE.BufferGeometry();
		const snowPos = new Float32Array(SNOW_COUNT * 3);
		this.snowPhase = new Float32Array(SNOW_COUNT);
		for (let i = 0; i < SNOW_COUNT; i++) {
			snowPos[i * 3] = Math.random() * 44 - 22;
			snowPos[i * 3 + 1] = Math.random() * 12;
			snowPos[i * 3 + 2] = Math.random() * 44 - 22;
			this.snowPhase[i] = Math.random() * Math.PI * 2;
		}
		snowGeo.setAttribute('position', new THREE.BufferAttribute(snowPos, 3));
		this.snowMat = new THREE.PointsMaterial({
			color: '#ffffff',
			size: 0.12,
			transparent: true,
			opacity: 0.85,
			depthWrite: false
		});
		this.snow = new THREE.Points(snowGeo, this.snowMat);
		this.snow.visible = false;
		this.group.add(this.snow);
	}

	/** Base (theme) sky the night blend starts from — re-pass on theme change. */
	setPalette(palette: WorldPalette): void {
		this.baseTop = new THREE.Color(palette.skyTop);
		this.baseBottom = new THREE.Color(palette.skyBottom);
	}

	/** Remove the inline sky override so a palette re-read sees the THEME's own
	 * tokens (call before readWorldPalette on theme change, or the night blend
	 * would compound into the new base). */
	clearSkyOverride(): void {
		if (this.heroEl) {
			this.heroEl.style.removeProperty('--fleet-sky-top');
			this.heroEl.style.removeProperty('--fleet-sky-bottom');
		}
	}

	setWeather(weather: WeatherState): void {
		this.weather = weather;
	}

	setObserver(lat: number, lon: number): void {
		this.observer = { lat, lon };
	}

	/** Upgrade the procedural cumuli in place with real cloud models (same
	 * pattern as the buildings): each slot keeps its orbit/fade state, only
	 * the node is swapped. Materials are cloned per cloud so the near-sector
	 * fade stays independent; template geometry is shared, never disposed. */
	setCloudModels(models: THREE.Object3D[]): void {
		if (models.length === 0) return;
		for (let i = 0; i < this.clouds.length; i++) {
			const c = this.clouds[i];
			const clone = models[i % models.length].clone(true);
			const box = new THREE.Box3().setFromObject(clone);
			const size = box.getSize(new THREE.Vector3());
			const extent = Math.max(size.x, size.z) || 1;
			clone.scale.multiplyScalar((3.4 / extent) * (0.75 + Math.random() * 0.95));
			clone.rotation.y = Math.random() * Math.PI * 2;
			const mats: THREE.MeshStandardMaterial[] = [];
			clone.traverse((o) => {
				const mesh = o as THREE.Mesh;
				if (!mesh.isMesh || Array.isArray(mesh.material)) return;
				mesh.castShadow = false;
				mesh.receiveShadow = false;
				const m = (mesh.material as THREE.MeshStandardMaterial).clone();
				m.transparent = true;
				m.opacity = 0;
				m.depthWrite = false;
				if (m.emissive) {
					m.emissive.set('#b8c4d8');
					m.emissiveIntensity = 0.3;
				}
				mesh.material = m;
				mats.push(m);
			});
			this.group.remove(c.node);
			this.disposeCloud(c);
			c.node = clone;
			c.mats = mats;
			c.ownedGeometry = false;
			clone.visible = false;
			this.group.add(clone);
		}
	}

	private disposeCloud(c: Cloud): void {
		if (c.ownedGeometry) {
			c.node.traverse((o) => {
				const mesh = o as THREE.Mesh;
				if (mesh.isMesh) mesh.geometry.dispose();
			});
		}
		for (const m of c.mats) m.dispose();
	}

	/** alt/az (azimuth from SOUTH, westward+) -> scene vector. Scene compass:
	 * north = -z, east = +x. */
	private skyVector(altitude: number, azimuth: number): THREE.Vector3 {
		const azFromNorth = azimuth + Math.PI;
		const cosAlt = Math.cos(altitude);
		return new THREE.Vector3(
			Math.sin(azFromNorth) * cosAlt * SKY_RADIUS,
			Math.sin(altitude) * SKY_RADIUS,
			-Math.cos(azFromNorth) * cosAlt * SKY_RADIUS
		);
	}

	update(
		dt: number,
		view?: { half: number; target: THREE.Vector3 | null; cameraPos?: THREE.Vector3 | null }
	): void {
		const now = new Date();
		const nowMs = now.getTime();

		// fit the shadow window to the visible area (+margin): zoomed into the
		// plaza, the shadow map covers ~±8 units instead of the whole realm —
		// less GPU per frame AND crisper shadow texels
		if (view) {
			const cam = this.sunLight.shadow.camera;
			if (Math.abs(cam.right - view.half) > 0.75) {
				cam.left = -view.half;
				cam.right = view.half;
				cam.top = view.half;
				cam.bottom = -view.half;
				cam.updateProjectionMatrix();
			}
			if (view.target) this.sunLight.target.position.copy(view.target);
		}

		let sunPos: THREE.Vector3;
		let moonPos: THREE.Vector3;
		let sunAltSin: number;
		if (this.observer) {
			// --- TRUE positions for the observer's lat/lon ---
			const sun = sunPosition(nowMs, this.observer.lat, this.observer.lon);
			const moon = moonPosition(nowMs, this.observer.lat, this.observer.lon);
			sunPos = this.skyVector(sun.altitude, sun.azimuth);
			moonPos = this.skyVector(moon.altitude, moon.azimuth);
			sunAltSin = Math.sin(sun.altitude);
		} else {
			// --- fallback: stylized local-clock arc (no location permission) ---
			const hours = now.getHours() + now.getMinutes() / 60 + now.getSeconds() / 3600;
			const sunHa = ((hours - 12) / 12) * Math.PI; // -π..π, 0 at noon
			sunPos = new THREE.Vector3(
				Math.sin(sunHa) * SKY_RADIUS,
				Math.cos(sunHa) * SKY_RADIUS * 0.8,
				SKY_RADIUS * 0.32
			);
			const phase = moonPhase(nowMs);
			const moonHa = sunHa - phase * Math.PI * 2;
			moonPos = new THREE.Vector3(
				Math.sin(moonHa) * SKY_RADIUS,
				Math.cos(moonHa) * SKY_RADIUS * 0.8,
				SKY_RADIUS * 0.28
			);
			sunAltSin = sunPos.y / (SKY_RADIUS * 0.8);
		}

		this.sunMesh.position.copy(sunPos);
		// the visible sun keeps its true arc; the key light keeps the same
		// azimuth (so shadows still swing through the day) but never drops
		// below KEY_LIGHT_MIN_Y, which would underlight the whole realm
		this.sunLight.position.set(sunPos.x, Math.max(sunPos.y, KEY_LIGHT_MIN_Y), sunPos.z);
		this.sunMesh.visible = sunPos.y > 0.5;
		// smooth twilight: full day above ~24° altitude, night a bit below horizon
		const dayness = THREE.MathUtils.clamp((sunAltSin + 0.08) / 0.48, 0, 1);
		const duskness = THREE.MathUtils.clamp(1 - Math.abs(sunAltSin) / 0.25, 0, 1);

		this.moonMesh.position.copy(moonPos);
		this.moonMesh.visible = moonPos.y > 0.5;
		// moonlight only matters at night; soft and blue, keeps the night lit
		const nightness = 1 - dayness;
		// same rule as the key light: the moon may set, its FILL may not drop
		// below the horizon and uplight the realm
		this.moonLight.position.set(moonPos.x, Math.max(moonPos.y, KEY_LIGHT_MIN_Y), moonPos.z);
		this.moonLight.intensity = 0.45 * nightness * (this.moonMesh.visible ? 1 : 0.4);

		// --- lighting: warm day -> amber dusk -> lit blue night ---
		// `lit` is dayness lifted onto NIGHT_LIGHT_FLOOR: the clock still moves
		// the light's colour and angle, it just can no longer switch it off.
		const lit = NIGHT_LIGHT_FLOOR + (1 - NIGHT_LIGHT_FLOOR) * dayness;
		this.sunLight.intensity = 1.6 * lit;
		this.sunLight.color.setHSL(0.09 + 0.02 * dayness, 0.55, 0.72 + 0.1 * dayness - 0.18 * duskness);
		this.hemi.intensity = 0.55 + 0.35 * lit;
		this.hemi.color.set(dayness > 0.4 ? '#dceeff' : '#aebcdd');
		const cover = this.weather.cloudCover;
		// Overcast reads through the cloud DECK and the flattened shadows, not
		// through a darker world. The old 45% cut stacked on top of the night
		// floor, so a cloudy evening — which is most evenings — landed the whole
		// realm at a third of its palette.
		this.sunLight.intensity *= 1 - cover * CLOUD_DIM_MAX;

		// --- sky blend + fog ---
		const nightBlend = Math.pow(nightness, 1.4) * NIGHT_SKY_BLEND_MAX;
		const top = this.baseTop.clone().lerp(NIGHT_TOP, nightBlend);
		const bottom = this.baseBottom.clone().lerp(NIGHT_BOTTOM, nightBlend);
		if (this.heroEl) {
			this.heroEl.style.setProperty('--fleet-sky-top', `#${top.getHexString()}`);
			this.heroEl.style.setProperty('--fleet-sky-bottom', `#${bottom.getHexString()}`);
		}
		if (this.scene.fog instanceof THREE.Fog) this.scene.fog.color.copy(bottom);

		// --- clouds: orbit the horizon band; count follows cover; near-sector
		// fade keeps the view corridor clear whatever the camera tilt ---
		const clampedCover = THREE.MathUtils.clamp(cover, 0, 1);
		const baseTarget = clampedCover > 0.04 ? 0.55 + clampedCover * 0.35 : 0;
		this.cloudOpacity += (baseTarget - this.cloudOpacity) * Math.min(1, dt);
		const activeClouds = Math.round(clampedCover * CLOUD_COUNT);
		// camera->target direction on the ground plane (for the sector fade)
		let camX = 0;
		let camZ = 0;
		let hasCam = false;
		const target = view?.target ?? null;
		if (target && view?.cameraPos) {
			camX = target.x - view.cameraPos.x;
			camZ = target.z - view.cameraPos.z;
			const len = Math.hypot(camX, camZ);
			if (len > 1e-3) {
				camX /= len;
				camZ /= len;
				hasCam = true;
			}
		}
		for (let i = 0; i < this.clouds.length; i++) {
			const c = this.clouds[i];
			c.angle += c.speed * dt;
			const px = Math.cos(c.angle) * c.radius;
			const pz = Math.sin(c.angle) * c.radius;
			c.node.position.set(px, c.height, pz);
			// a cloud in the corridor between camera and look-target melts away
			let sector = 1;
			if (hasCam && target) {
				const along = ((px - target.x) * camX + (pz - target.z) * camZ) / c.radius;
				sector = THREE.MathUtils.clamp((along + 0.85) / 0.55, 0.05, 1);
			}
			c.fade += (sector - c.fade) * Math.min(1, dt * 3);
			c.vis += ((i < activeClouds ? 1 : 0) - c.vis) * Math.min(1, dt * 0.5);
			const opacity = this.cloudOpacity * c.vis * c.fade;
			for (const m of c.mats) m.opacity = opacity;
			c.node.visible = opacity > 0.02;
		}

		// --- ground shadow-patches carry the cover over the village (daytime
		// only — there is nothing to shade at night) ---
		const shadowTarget = clampedCover > 0.15 ? Math.min(0.32, clampedCover * 0.35) * dayness : 0;
		this.cloudShadowMat.opacity += (shadowTarget - this.cloudShadowMat.opacity) * Math.min(1, dt);
		const patchesOn = this.cloudShadowMat.opacity > 0.015;
		for (const patch of this.cloudShadows) {
			patch.visible = patchesOn;
			if (!patchesOn) continue;
			patch.position.x += dt * 0.5;
			patch.position.z += dt * 0.2;
			if (patch.position.x > CLOUD_SHADOW_WRAP) patch.position.x = -CLOUD_SHADOW_WRAP;
			if (patch.position.z > CLOUD_SHADOW_WRAP) patch.position.z = -CLOUD_SHADOW_WRAP;
		}

		// --- precipitation: WMO intensity scales how many particles fall, how
		// fast, and how visibly — drizzle whispers, a thunderstorm pours ---
		const raining = this.weather.kind === 'rain';
		const snowing = this.weather.kind === 'snow';
		this.rain.visible = raining;
		this.snow.visible = snowing;
		const strength = THREE.MathUtils.clamp(this.weather.intensity, 0, 1);
		if (raining) {
			const active = Math.max(1, Math.floor(RAIN_COUNT * (0.25 + 0.75 * strength)));
			this.rain.geometry.setDrawRange(0, active);
			this.rainMat.opacity = 0.45 + 0.35 * strength;
			const speedScale = 0.75 + 0.55 * strength;
			const pos = this.rain.geometry.getAttribute('position') as THREE.BufferAttribute;
			for (let i = 0; i < active; i++) {
				let y = pos.getY(i) - this.rainVel[i] * speedScale * dt;
				if (y < 0) y = 12;
				pos.setY(i, y);
			}
			pos.needsUpdate = true;
		} else if (snowing) {
			const active = Math.max(1, Math.floor(SNOW_COUNT * (0.3 + 0.7 * strength)));
			this.snow.geometry.setDrawRange(0, active);
			this.snowMat.opacity = 0.6 + 0.3 * strength;
			const fall = 1.1 * (0.8 + 0.5 * strength);
			const pos = this.snow.geometry.getAttribute('position') as THREE.BufferAttribute;
			const t = performance.now() / 1000;
			for (let i = 0; i < active; i++) {
				let y = pos.getY(i) - fall * dt;
				if (y < 0) y = 12;
				pos.setY(i, y);
				pos.setX(i, pos.getX(i) + Math.sin(t + this.snowPhase[i]) * dt * 0.35);
			}
			pos.needsUpdate = true;
		}
	}

	dispose(): void {
		if (this.heroEl) {
			this.heroEl.style.removeProperty('--fleet-sky-top');
			this.heroEl.style.removeProperty('--fleet-sky-bottom');
		}
		this.sunMesh.geometry.dispose();
		this.sunMat.dispose();
		this.moonMesh.geometry.dispose();
		this.moonMat.dispose();
		for (const c of this.clouds) this.disposeCloud(c);
		this.cloudShadowGeo.dispose();
		this.cloudShadowMat.dispose();
		this.cloudShadowTex.dispose();
		this.rain.geometry.dispose();
		this.rainMat.dispose();
		this.snow.geometry.dispose();
		this.snowMat.dispose();
	}
}
