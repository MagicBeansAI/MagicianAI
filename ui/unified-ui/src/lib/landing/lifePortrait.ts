// Vector Lottie portrait for the landing cycle: a still silhouette facing a
// hanging frame, with one icon layer per CYCLE label. Icon layers occupy
// sequential 1-frame windows so the player can `goToAndStop(i, true)`.

import { CYCLE } from './lifeCycle';

export const PORTRAIT_INK = '#1a1612';

const W = 300;
const H = 400;
const FR = 30;
const INK = [0.102, 0.0863, 0.0706, 1] as const;

const FRAME_CX = 225;
const FRAME_CY = 200;
const FRAME_W = 140;
const FRAME_H = 180;

type Pt = [number, number];

type LottieLayer = {
	ddd: number;
	ind: number;
	ty: number;
	nm: string;
	sr: number;
	ks: object;
	ao: number;
	shapes: object[];
	ip: number;
	op: number;
	st: number;
	bm: number;
};

export type LifePortraitLottie = {
	v: string;
	fr: number;
	ip: number;
	op: number;
	w: number;
	h: number;
	nm: string;
	ddd: number;
	assets: unknown[];
	layers: LottieLayer[];
};

function staticNum(k: number) {
	return { a: 0 as const, k };
}

function el(x: number, y: number, w: number, h: number) {
	return { ty: 'el', p: { a: 0, k: [x, y] }, s: { a: 0, k: [w, h] } };
}

function rc(x: number, y: number, w: number, h: number, r = 0) {
	return { ty: 'rc', p: { a: 0, k: [x, y] }, s: { a: 0, k: [w, h] }, r: staticNum(r) };
}

function sh(verts: Pt[], closed = true) {
	return {
		ty: 'sh',
		ks: {
			a: 0,
			k: {
				i: verts.map(() => [0, 0] as Pt),
				o: verts.map(() => [0, 0] as Pt),
				v: verts,
				c: closed
			}
		}
	};
}

function fill() {
	return { ty: 'fl', c: { a: 0, k: [...INK] }, o: staticNum(100), r: 1 };
}

function stroke(w: number) {
	return {
		ty: 'st',
		c: { a: 0, k: [...INK] },
		o: staticNum(100),
		w: staticNum(w),
		lc: 2,
		lj: 2
	};
}

function groupTr() {
	return {
		ty: 'tr',
		p: { a: 0, k: [0, 0] },
		a: { a: 0, k: [0, 0] },
		s: { a: 0, k: [100, 100] },
		r: staticNum(0),
		o: staticNum(100)
	};
}

function group(items: object[], paint: 'fill' | 'stroke', sw = 4) {
	return {
		ty: 'gr',
		it: [...items, paint === 'stroke' ? stroke(sw) : fill(), groupTr()]
	};
}

function layerKs() {
	return {
		o: staticNum(100),
		r: staticNum(0),
		p: { a: 0, k: [0, 0, 0] },
		a: { a: 0, k: [0, 0, 0] },
		s: { a: 0, k: [100, 100, 100] }
	};
}

function shapeLayer(nm: string, ind: number, ip: number, op: number, shapes: object[]): LottieLayer {
	return {
		ddd: 0,
		ind,
		ty: 4,
		nm,
		sr: 1,
		ks: layerKs(),
		ao: 0,
		shapes,
		ip,
		op,
		st: 0,
		bm: 0
	};
}

function icon(dx: number, dy: number): Pt {
	return [FRAME_CX + dx, FRAME_CY + dy];
}

function iconEl(dx: number, dy: number, w: number, h: number) {
	return el(FRAME_CX + dx, FRAME_CY + dy, w, h);
}

function iconRc(dx: number, dy: number, w: number, h: number, r = 0) {
	return rc(FRAME_CX + dx, FRAME_CY + dy, w, h, r);
}

function iconSh(verts: Pt[], closed = true) {
	return sh(
		verts.map(([x, y]) => icon(x, y)),
		closed
	);
}

function iconShapes(label: string): { items: object[]; paint: 'fill' | 'stroke'; sw?: number } {
	switch (label) {
		case 'shipping':
			return {
				items: [iconRc(0, 6, 40, 28, 2), iconSh([[-20, -8], [0, -22], [20, -8]], false)],
				paint: 'stroke',
				sw: 3
			};
		case 'cooking':
			return { items: [iconEl(0, 4, 42, 18), iconRc(28, 4, 22, 6, 2)], paint: 'fill' };
		case 'freelancing':
			return { items: [iconRc(0, -8, 36, 24, 2), iconRc(0, 10, 48, 8, 2)], paint: 'fill' };
		case 'rehearsing':
			return { items: [iconSh([[-8, -18], [16, 0], [-8, 18]])], paint: 'fill' };
		case 'parenting':
			return {
				items: [iconEl(-12, -10, 16, 16), iconRc(-12, 10, 18, 24, 4), iconEl(14, -4, 12, 12)],
				paint: 'fill'
			};
		case 'saving':
			return { items: [iconEl(0, 0, 36, 36), iconRc(0, 0, 16, 4)], paint: 'stroke', sw: 3 };
		case 'drawing':
			return {
				items: [iconSh([[0, -22], [10, 18], [-10, 18]]), iconRc(0, 24, 8, 8)],
				paint: 'fill'
			};
		case 'running':
			return {
				items: [
					iconSh([[-16, -14], [8, 0], [-16, 14]], false),
					iconSh([[-2, -14], [22, 0], [-2, 14]], false)
				],
				paint: 'stroke',
				sw: 4
			};
		case 'briefing':
			return {
				items: [iconRc(0, 0, 32, 40, 2), iconRc(0, -6, 18, 3), iconRc(0, 4, 18, 3)],
				paint: 'stroke',
				sw: 3
			};
		case 'repairing':
			return {
				items: [
					iconSh([
						[-18, -16],
						[-8, -16],
						[-4, -4],
						[18, 18],
						[8, 24],
						[-10, 2],
						[-18, -6]
					])
				],
				paint: 'fill'
			};
		case 'hosting':
			return {
				items: [iconSh([[0, -22], [24, 0], [-24, 0]]), iconRc(0, 16, 32, 24)],
				paint: 'fill'
			};
		case 'studying':
			return {
				items: [iconSh([[0, 0], [-22, -12], [-22, 12]]), iconSh([[0, 0], [22, -12], [22, 12]])],
				paint: 'fill'
			};
		case 'gardening':
			return {
				items: [iconRc(0, 12, 6, 28), iconEl(-10, -8, 18, 12), iconEl(10, -8, 18, 12)],
				paint: 'fill'
			};
		case 'composing':
			return {
				items: [iconEl(-10, 6, 14, 10), iconEl(10, 10, 14, 10), iconRc(-10, -8, 3, 22)],
				paint: 'fill'
			};
		case 'hiring':
			return {
				items: [
					iconEl(0, -12, 16, 16),
					iconRc(0, 8, 20, 20, 4),
					iconSh([
						[16, -6],
						[28, -6],
						[28, 2],
						[24, 2],
						[24, 10],
						[20, 10],
						[20, 2],
						[16, 2]
					])
				],
				paint: 'fill'
			};
		case 'travelling':
			return { items: [iconRc(0, 2, 36, 28, 3), iconRc(0, -16, 16, 10, 2)], paint: 'fill' };
		case 'filing':
			return {
				items: [iconSh([[-22, -8], [-10, -18], [22, -18], [22, 18], [-22, 18]])],
				paint: 'fill'
			};
		case 'practising':
			return { items: [iconEl(0, 0, 40, 40), iconEl(0, 0, 18, 18)], paint: 'stroke', sw: 3 };
		case 'showing up':
			return {
				items: [iconSh([[-16, 2], [-4, 16], [20, -16]], false)],
				paint: 'stroke',
				sw: 5
			};
		case 'painting':
			return {
				items: [iconRc(0, 10, 8, 28, 2), iconSh([[-12, -16], [12, -16], [6, 0], [-6, 0]])],
				paint: 'fill'
			};
		case 'coding':
			return {
				items: [
					iconSh([[-6, -16], [-22, 0], [-6, 16]], false),
					iconSh([[6, -16], [22, 0], [6, 16]], false)
				],
				paint: 'stroke',
				sw: 4
			};
		case 'coaching':
			return {
				items: [iconSh([[-8, -12], [8, -20], [8, 20], [-8, 12]]), iconEl(16, 0, 12, 20)],
				paint: 'fill'
			};
		case 'unpacking':
			return {
				items: [iconRc(0, 10, 36, 20, 2), iconSh([[-18, 0], [0, -20], [18, 0]], false)],
				paint: 'stroke',
				sw: 3
			};
		case 'listening':
			return {
				items: [
					iconEl(-16, 0, 14, 18),
					iconEl(16, 0, 14, 18),
					iconSh([[-16, -8], [0, -22], [16, -8]], false)
				],
				paint: 'stroke',
				sw: 4
			};
		case 'all your side quests':
			return { items: [iconEl(0, 0, 10, 10)], paint: 'fill' };
		default:
			return { items: [iconEl(0, 0, 12, 12)], paint: 'fill' };
	}
}

function silhouetteShapes() {
	// Three-quarter from behind, standing left, facing the hanging frame.
	return [
		group(
			[
				el(88, 92, 38, 46),
				rc(88, 118, 14, 16, 2),
				rc(80, 170, 52, 96, 6),
				rc(64, 282, 16, 104, 3),
				rc(96, 282, 16, 104, 3),
				rc(52, 176, 12, 58, 4),
				rc(118, 158, 14, 72, 4)
			],
			'fill'
		)
	];
}

function frameShapes() {
	return [group([rc(FRAME_CX, FRAME_CY, FRAME_W, FRAME_H, 2)], 'stroke', 4)];
}

export function buildLifePortraitLottie(): LifePortraitLottie {
	const op = CYCLE.length;
	const layers: LottieLayer[] = [
		shapeLayer('silhouette', 1, 0, op, silhouetteShapes()),
		shapeLayer('frame', 2, 0, op, frameShapes())
	];
	CYCLE.forEach((label, i) => {
		const spec = iconShapes(label);
		layers.push(
			shapeLayer(label, 3 + i, i, i + 1, [group(spec.items, spec.paint, spec.sw ?? 4)])
		);
	});
	return {
		v: '5.7.4',
		fr: FR,
		ip: 0,
		op,
		w: W,
		h: H,
		nm: 'life portrait',
		ddd: 0,
		assets: [],
		layers
	};
}
