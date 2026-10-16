import { describe, expect, it } from 'vitest';

import { evaluateExpression } from './recipeExpression';

// A fixed environment mirroring the identifiers the Swift tests resolve.
const ENV: Record<string, number> = {
	x: 100,
	y: 200,
	size: 28,
	cx: 50,
	cy: 60,
	r: 10,
	a: 90,
	sx: 0,
	sy: 0,
	ex: 10,
	ey: 0
};

function ev(expr: string, env: Record<string, number> = ENV): number | undefined {
	return evaluateExpression(expr, (name) => (name in env ? env[name] : undefined));
}

describe('recipeExpression — arithmetic', () => {
	it('evaluates number literals', () => {
		expect(ev('42')).toBe(42);
		expect(ev('3.5')).toBe(3.5);
		expect(ev('.5')).toBe(0.5);
	});

	it('adds and subtracts left-to-right', () => {
		expect(ev('1+2+3')).toBe(6);
		expect(ev('10-3-2')).toBe(5);
		expect(ev('x+size')).toBe(128);
		expect(ev('(y|cy)+size')).toBe(228);
	});

	it('multiplies and divides with correct precedence', () => {
		expect(ev('2+3*4')).toBe(14);
		expect(ev('2*3+4')).toBe(10);
		expect(ev('r*0.5')).toBe(5);
		expect(ev('20/4/5')).toBe(1);
	});

	it('honours parentheses', () => {
		expect(ev('(2+3)*4')).toBe(20);
		expect(ev('(x+size)/2')).toBe(64);
	});

	it('applies unary minus and plus', () => {
		expect(ev('-5')).toBe(-5);
		expect(ev('-(2+3)')).toBe(-5);
		expect(ev('3+-2')).toBe(1);
		expect(ev('+7')).toBe(7);
	});
});

describe('recipeExpression — coalesce (|, lowest precedence)', () => {
	it('takes the first defined operand', () => {
		expect(ev('cx|x')).toBe(50);
		expect(ev('missing|x')).toBe(100);
		expect(ev('missing1|missing2|size')).toBe(28);
	});

	it('is lower precedence than arithmetic', () => {
		// missing+1 is undefined, so the chain falls through to x.
		expect(ev('missing+1|x')).toBe(100);
		// r*2 = 20 is defined, wins immediately.
		expect(ev('r*2|x')).toBe(20);
	});

	it('returns undefined when every operand is undefined', () => {
		expect(ev('missing1|missing2')).toBeUndefined();
	});
});

describe('recipeExpression — functions', () => {
	it('trig + deg/rad', () => {
		expect(ev('sin(0)')).toBe(0);
		expect(ev('cos(0)')).toBe(1);
		expect(ev('deg(180)')).toBeCloseTo(Math.PI, 12);
		expect(ev('rad(3.141592653589793)')).toBeCloseTo(180, 9);
		expect(ev('cos(deg(a))*r')).toBeCloseTo(0, 12); // cos(90deg)=0
	});

	it('sqrt / abs', () => {
		expect(ev('sqrt(9)')).toBe(3);
		expect(ev('abs(-7)')).toBe(7);
		// sqrt of a negative → undefined (Swift returns nil).
		expect(ev('sqrt(-1)')).toBeUndefined();
	});

	it('min / max take two args', () => {
		expect(ev('min(3,5)')).toBe(3);
		expect(ev('max(3,5)')).toBe(5);
		expect(ev('min(260,size*9+16)')).toBe(260);
	});

	it('wrong arity or undefined arg yields undefined', () => {
		expect(ev('min(3)')).toBeUndefined();
		expect(ev('sin(missing)')).toBeUndefined();
		expect(ev('bogus(1)')).toBeUndefined();
	});
});

describe('recipeExpression — undefined + division-by-zero', () => {
	it('an undefined identifier outside a chain makes the value undefined', () => {
		expect(ev('missing')).toBeUndefined();
		expect(ev('missing+5')).toBeUndefined();
		expect(ev('missing*2')).toBeUndefined();
	});

	it('division by zero yields undefined, not Infinity', () => {
		expect(ev('1/0')).toBeUndefined();
		expect(ev('x/(size-size)')).toBeUndefined();
		// undefined (from /0) in a coalesce chain is skipped → falls through.
		expect(ev('1/0|x')).toBe(100);
	});

	it('rejects trailing garbage / syntax errors', () => {
		expect(ev('1 2')).toBeUndefined();
		expect(ev('(1+2')).toBeUndefined();
		expect(ev('*3')).toBeUndefined();
		expect(ev('')).toBeUndefined();
	});
});
