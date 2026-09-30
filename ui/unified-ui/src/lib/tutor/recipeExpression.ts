/**
 * Pure, side-effect-free expression evaluator for tutor-primitive recipes.
 *
 * Grammar (identical to the Swift/Rust interpreters — see
 * `magios/Shared/RecipeExpression.swift`, the reference implementation):
 * - number literals, identifiers (resolved via `resolve`), parentheses `( )`
 * - binary `+ - * /`, unary `-`
 * - coalesce `|` — **lowest precedence**, "first defined operand"
 * - functions: `sin cos tan sqrt abs min max deg rad`
 *   (`deg` = degrees→radians, `rad` = radians→degrees)
 *
 * A bare identifier resolves via `resolve`; an undefined operand *inside* a
 * coalesce chain is skipped, while an undefined identifier *outside* a chain
 * makes the whole expression `undefined` (the caller then skips the op if the
 * coordinate is required). Recursion depth is guarded at <= 32. There is no
 * code execution — only arithmetic over a fixed function set. Division by zero
 * yields `undefined` (not Infinity), and only a FINITE value counts in a
 * coalesce chain (Infinity/NaN are treated as undefined).
 */

const MAX_DEPTH = 32;

/**
 * Evaluate `expr` to a number, resolving bare identifiers via `resolve`.
 * Returns `undefined` for a syntax error or an undefined-outside-coalesce
 * result.
 */
export function evaluateExpression(
	expr: string,
	resolve: (name: string) => number | undefined
): number | undefined {
	const parser = new Parser(expr, resolve);
	const value = parser.parseCoalesce(0);
	if (value === PARSE_ERROR) return undefined;
	// Reject trailing garbage — a malformed expression evaluates to undefined.
	parser.skipSpaces();
	if (!parser.isAtEnd()) return undefined;
	return value;
}

// A sentinel distinguishing a hard parse error from a legitimately-undefined
// result. `undefined` means "defined-check failed"; PARSE_ERROR means a syntax
// error (mirrors the Swift `(value, parsedOK)` tuple, where `!ok` == error).
const PARSE_ERROR = Symbol('parse-error');
type ParseResult = number | undefined | typeof PARSE_ERROR;

function isNumberResult(v: ParseResult): v is number {
	return typeof v === 'number';
}

class Parser {
	private readonly chars: string[];
	private pos = 0;
	private readonly resolve: (name: string) => number | undefined;

	constructor(s: string, resolve: (name: string) => number | undefined) {
		this.chars = Array.from(s);
		this.resolve = resolve;
	}

	isAtEnd(): boolean {
		return this.pos >= this.chars.length;
	}

	skipSpaces(): void {
		while (this.pos < this.chars.length && (this.chars[this.pos] === ' ' || this.chars[this.pos] === '\t')) {
			this.pos += 1;
		}
	}

	private peek(): string | undefined {
		return this.pos < this.chars.length ? this.chars[this.pos] : undefined;
	}

	// coalesce `|` — lowest precedence, first-defined-operand.
	// Returns a number, `undefined` (all operands undefined), or PARSE_ERROR.
	parseCoalesce(depth: number): ParseResult {
		if (depth > MAX_DEPTH) return PARSE_ERROR;
		let result: number | undefined = undefined;
		let sawTerm = false;
		for (;;) {
			const value = this.parseAddSub(depth + 1);
			if (value === PARSE_ERROR) return PARSE_ERROR; // hard parse error
			sawTerm = true;
			// Only a FINITE value counts — division-by-zero (Inf) and NaN are
			// treated as "undefined" so a degenerate op is skipped, not drawn at
			// the clamped canvas boundary.
			if (result === undefined && isNumberResult(value) && Number.isFinite(value)) {
				result = value;
			}
			this.skipSpaces();
			if (this.peek() === '|') {
				this.pos += 1;
				continue;
			}
			break;
		}
		return sawTerm ? result : PARSE_ERROR;
	}

	// `undefined` means "defined-check failed" (an undefined identifier);
	// PARSE_ERROR means a syntax error.
	private parseAddSub(depth: number): ParseResult {
		if (depth > MAX_DEPTH) return PARSE_ERROR;
		let left = this.parseMulDiv(depth + 1);
		if (left === PARSE_ERROR) return PARSE_ERROR;
		for (;;) {
			this.skipSpaces();
			const c = this.peek();
			if (c !== '+' && c !== '-') break;
			this.pos += 1;
			const right = this.parseMulDiv(depth + 1);
			if (right === PARSE_ERROR) return PARSE_ERROR;
			if (!isNumberResult(left) || !isNumberResult(right)) {
				left = undefined;
				continue;
			}
			left = c === '+' ? left + right : left - right;
		}
		return left;
	}

	private parseMulDiv(depth: number): ParseResult {
		if (depth > MAX_DEPTH) return PARSE_ERROR;
		let left = this.parseUnary(depth + 1);
		if (left === PARSE_ERROR) return PARSE_ERROR;
		for (;;) {
			this.skipSpaces();
			const c = this.peek();
			if (c !== '*' && c !== '/') break;
			this.pos += 1;
			const right = this.parseUnary(depth + 1);
			if (right === PARSE_ERROR) return PARSE_ERROR;
			if (!isNumberResult(left) || !isNumberResult(right)) {
				left = undefined;
				continue;
			}
			if (c === '/') {
				left = right === 0 ? undefined : left / right; // 0-divide → undefined, not Inf
			} else {
				left = left * right;
			}
		}
		return left;
	}

	private parseUnary(depth: number): ParseResult {
		if (depth > MAX_DEPTH) return PARSE_ERROR;
		this.skipSpaces();
		if (this.peek() === '-') {
			this.pos += 1;
			const v = this.parseUnary(depth + 1);
			if (v === PARSE_ERROR) return PARSE_ERROR;
			if (isNumberResult(v)) return -v;
			return undefined;
		}
		if (this.peek() === '+') {
			this.pos += 1;
			return this.parseUnary(depth + 1);
		}
		return this.parsePrimary(depth + 1);
	}

	private parsePrimary(depth: number): ParseResult {
		if (depth > MAX_DEPTH) return PARSE_ERROR;
		this.skipSpaces();
		const c = this.peek();
		if (c === undefined) return PARSE_ERROR;

		if (c === '(') {
			this.pos += 1;
			const value = this.parseCoalesce(depth + 1); // subexpr may coalesce
			if (value === PARSE_ERROR) return PARSE_ERROR;
			this.skipSpaces();
			if (this.peek() !== ')') return PARSE_ERROR;
			this.pos += 1;
			return value;
		}

		if (isDigit(c) || c === '.') {
			return this.parseNumber();
		}

		if (isLetter(c) || c === '_') {
			return this.parseIdentifierOrCall(depth);
		}

		return PARSE_ERROR;
	}

	private parseNumber(): ParseResult {
		const start = this.pos;
		while (this.pos < this.chars.length && (isDigit(this.chars[this.pos]) || this.chars[this.pos] === '.')) {
			this.pos += 1;
		}
		const str = this.chars.slice(start, this.pos).join('');
		const d = Number(str);
		if (str.length === 0 || Number.isNaN(d)) return PARSE_ERROR;
		return d;
	}

	private parseIdentifierOrCall(depth: number): ParseResult {
		const start = this.pos;
		while (
			this.pos < this.chars.length &&
			(isLetter(this.chars[this.pos]) || isDigit(this.chars[this.pos]) || this.chars[this.pos] === '_')
		) {
			this.pos += 1;
		}
		const name = this.chars.slice(start, this.pos).join('');
		this.skipSpaces();
		if (this.peek() === '(') {
			this.pos += 1;
			const args: Array<number | undefined> = [];
			this.skipSpaces();
			if (this.peek() !== ')') {
				for (;;) {
					const value = this.parseCoalesce(depth + 1);
					if (value === PARSE_ERROR) return PARSE_ERROR;
					args.push(value);
					this.skipSpaces();
					if (this.peek() === ',') {
						this.pos += 1;
						continue;
					}
					break;
				}
			}
			this.skipSpaces();
			if (this.peek() !== ')') return PARSE_ERROR;
			this.pos += 1;
			return applyFunction(name, args);
		}
		// A bare identifier resolves via the environment.
		return this.resolve(name);
	}
}

function applyFunction(name: string, args: Array<number | undefined>): number | undefined {
	switch (name) {
		case 'sin':
		case 'cos':
		case 'tan':
		case 'sqrt':
		case 'abs':
		case 'deg':
		case 'rad': {
			if (args.length !== 1 || args[0] === undefined) return undefined;
			const a = args[0];
			switch (name) {
				case 'sin':
					return Math.sin(a);
				case 'cos':
					return Math.cos(a);
				case 'tan':
					return Math.tan(a);
				case 'sqrt':
					return a < 0 ? undefined : Math.sqrt(a);
				case 'abs':
					return Math.abs(a);
				case 'deg':
					return (a * Math.PI) / 180; // degrees -> radians
				case 'rad':
					return (a * 180) / Math.PI; // radians -> degrees
				default:
					return undefined;
			}
		}
		case 'min':
		case 'max': {
			if (args.length !== 2 || args[0] === undefined || args[1] === undefined) return undefined;
			return name === 'min' ? Math.min(args[0], args[1]) : Math.max(args[0], args[1]);
		}
		default:
			return undefined;
	}
}

// Match Swift's `Character.isNumber` / `Character.isLetter` for the ASCII
// range recipes use. Swift treats Unicode letters/digits as such, but recipe
// identifiers and numbers are ASCII, so an ASCII check is behaviourally
// equivalent for the recipe DSL.
function isDigit(c: string): boolean {
	return c >= '0' && c <= '9';
}

function isLetter(c: string): boolean {
	return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z');
}
