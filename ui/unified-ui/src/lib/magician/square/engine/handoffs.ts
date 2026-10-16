import * as THREE from 'three';

/**
 * Delegation hand-off beams: when agent A's execution delegates to agent B, a
 * soft arc connects the two citizens with a glowing orb travelling A -> B —
 * the design doc's "Agent A physically hands a glowing orb to Agent B",
 * rendered from live /execution-tree edges. Endpoints re-resolve every frame
 * (citizens walk), so beams follow their people.
 */

export interface HandoffEdge {
	/** Unique per from->to pair. */
	key: string;
	fromAgent: string;
	toAgent: string;
}

const CURVE_POINTS = 24;
const ORB_SPEED = 0.22; // curve traversals per second

interface Beam {
	edge: HandoffEdge;
	line: THREE.Line;
	lineGeo: THREE.BufferGeometry;
	lineMat: THREE.LineBasicMaterial;
	orb: THREE.Mesh;
	orbMat: THREE.MeshBasicMaterial;
	phase: number;
}

export class HandoffSystem {
	readonly group = new THREE.Group();
	private beams = new Map<string, Beam>();
	private color = new THREE.Color('#e8b64c');
	private orbGeo = new THREE.SphereGeometry(0.09, 10, 8);

	setColor(hex: string): void {
		this.color = new THREE.Color(hex);
		for (const b of this.beams.values()) {
			b.lineMat.color.copy(this.color);
			b.orbMat.color.copy(this.color);
		}
	}

	sync(edges: HandoffEdge[]): void {
		const seen = new Set<string>();
		for (const edge of edges) {
			seen.add(edge.key);
			if (this.beams.has(edge.key)) continue;
			const lineGeo = new THREE.BufferGeometry();
			lineGeo.setAttribute(
				'position',
				new THREE.BufferAttribute(new Float32Array((CURVE_POINTS + 1) * 3), 3)
			);
			const lineMat = new THREE.LineBasicMaterial({
				color: this.color,
				transparent: true,
				opacity: 0.35
			});
			const line = new THREE.Line(lineGeo, lineMat);
			line.frustumCulled = false;
			const orbMat = new THREE.MeshBasicMaterial({ color: this.color });
			const orb = new THREE.Mesh(this.orbGeo, orbMat);
			const beam: Beam = {
				edge,
				line,
				lineGeo,
				lineMat,
				orb,
				orbMat,
				phase: Math.random()
			};
			this.group.add(line, orb);
			this.beams.set(edge.key, beam);
		}
		for (const [key, b] of this.beams) {
			if (!seen.has(key)) {
				this.group.remove(b.line, b.orb);
				b.lineGeo.dispose();
				b.lineMat.dispose();
				b.orbMat.dispose();
				this.beams.delete(key);
			}
		}
	}

	/** Per-frame: re-anchor each beam to its (moving) citizens. */
	update(now: number, resolve: (agentId: string) => THREE.Vector3 | null): void {
		for (const b of this.beams.values()) {
			const from = resolve(b.edge.fromAgent);
			const to = resolve(b.edge.toAgent);
			const visible = Boolean(from && to);
			b.line.visible = visible;
			b.orb.visible = visible;
			if (!from || !to) continue;
			const a = new THREE.Vector3(from.x, 0.55, from.z);
			const c = new THREE.Vector3(to.x, 0.55, to.z);
			const mid = a
				.clone()
				.add(c)
				.multiplyScalar(0.5)
				.setY(0.55 + Math.min(3.2, a.distanceTo(c) * 0.35 + 0.8));
			const curve = new THREE.QuadraticBezierCurve3(a, mid, c);
			const pos = b.lineGeo.getAttribute('position') as THREE.BufferAttribute;
			for (let i = 0; i <= CURVE_POINTS; i++) {
				const p = curve.getPoint(i / CURVE_POINTS);
				pos.setXYZ(i, p.x, p.y, p.z);
			}
			pos.needsUpdate = true;
			const t = (now * ORB_SPEED + b.phase) % 1;
			const op = curve.getPoint(t);
			b.orb.position.copy(op);
			const s = 0.85 + Math.sin(now * 6 + b.phase * 7) * 0.2;
			b.orb.scale.set(s, s, s);
		}
	}

	dispose(): void {
		this.sync([]);
		this.orbGeo.dispose();
	}
}
