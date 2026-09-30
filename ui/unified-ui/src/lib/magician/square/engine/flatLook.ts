import * as THREE from 'three';

/**
 * Flat look — the shading half of the pixel-art direction (the resolution half
 * lives in FleetEngine's PIXEL_SCALE, the colour half in the --fleet-* tokens).
 *
 * Standard/physical materials shade with a smooth lambert falloff. Downsampled
 * to a third of the frame that falloff has nowhere to hide: it lands as muddy
 * dithered noise across every surface. Toon shading quantises the same term
 * through a stepped ramp, so a wall reads as two or three flat colour blocks —
 * which is what survives the downsample, and what pixel art is made of.
 *
 * Conversion happens after a subtree is built (procedural meshes and glTF
 * clones alike), so the builders stay ignorant of the art direction.
 */

let gradient: THREE.DataTexture | null = null;

/** Three hard bands: core shadow, mid, lit. Nearest filtering is what keeps the
 * steps hard — a linear ramp would reintroduce the falloff we just removed.
 *
 * The darkest band sits high on purpose. In a stepped ramp the shadow band is a
 * flat FILL, not a gradient's tail: every away-facing surface in the scene gets
 * that exact value across its whole area. Set it low and half the world is a
 * hole — which is what a cream wall rendering as mid-grey looks like. */
export function toonGradient(): THREE.DataTexture {
	if (gradient) return gradient;
	const steps = new Uint8Array([150, 205, 255]);
	const tex = new THREE.DataTexture(steps, steps.length, 1, THREE.RedFormat);
	tex.minFilter = THREE.NearestFilter;
	tex.magFilter = THREE.NearestFilter;
	tex.generateMipmaps = false;
	tex.needsUpdate = true;
	gradient = tex;
	return tex;
}

function isStandard(m: THREE.Material): m is THREE.MeshStandardMaterial {
	return (m as THREE.MeshStandardMaterial).isMeshStandardMaterial === true;
}

/**
 * A material's toon twin. Everything the world actually varies at runtime is
 * carried across — colour, maps, transparency (the occluder fade), emissive
 * (the powered sci-fi structures), vertex colours (the KayKit packs), side.
 * Roughness and metalness are deliberately dropped: under a stepped ramp they
 * describe a highlight that no longer exists.
 *
 * Non-standard inputs (basic, sprite, already-toon) are returned untouched.
 */
export function toToon(src: THREE.Material): THREE.Material {
	if (!isStandard(src)) return src;
	const toon = new THREE.MeshToonMaterial({
		name: src.name,
		color: src.color,
		map: src.map,
		gradientMap: toonGradient(),
		emissive: src.emissive,
		emissiveMap: src.emissiveMap,
		emissiveIntensity: src.emissiveIntensity,
		alphaMap: src.alphaMap,
		alphaTest: src.alphaTest,
		transparent: src.transparent,
		opacity: src.opacity,
		depthWrite: src.depthWrite,
		side: src.side,
		vertexColors: src.vertexColors,
		fog: src.fog
	});
	// Three's renderer honors this shared material flag for toon shaders, but
	// the installed MeshToonMaterialParameters type omits it from the constructor
	// even though MeshStandardMaterial exposes it. Assign before first compile.
	(toon as THREE.MeshToonMaterial & { flatShading: boolean }).flatShading = src.flatShading;
	return toon;
}

/** Toon twin of a material that must not be shared — the per-actor garment
 * tint would otherwise bleed across every citizen wearing the same model. */
export function flatClone(src: THREE.Material): THREE.Material {
	return isStandard(src) ? toToon(src) : src.clone();
}

/**
 * Convert a whole subtree in place.
 *
 * Returns old -> new so callers holding material references (the occluder-fade
 * list) can re-point them. The originals are NOT disposed: glTF clones share
 * their materials with the cached pack template, and freeing those would strip
 * every future clone of the theme's models.
 */
export function flattenMaterials(root: THREE.Object3D): Map<THREE.Material, THREE.Material> {
	const swapped = new Map<THREE.Material, THREE.Material>();
	const convert = (m: THREE.Material): THREE.Material => {
		const seen = swapped.get(m);
		if (seen) return seen;
		const next = toToon(m);
		if (next !== m) swapped.set(m, next);
		return next;
	};
	root.traverse((obj) => {
		const mesh = obj as THREE.Mesh;
		if (!mesh.isMesh || !mesh.material) return;
		mesh.material = Array.isArray(mesh.material)
			? mesh.material.map(convert)
			: convert(mesh.material);
	});
	return swapped;
}
