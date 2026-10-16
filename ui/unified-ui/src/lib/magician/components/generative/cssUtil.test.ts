import { describe, expect, it } from 'vitest';
import {
	sanitizeCssValue,
	sanitizeCssKeyword,
	sanitizeCssLengthList,
	sanitizeCssLength,
	sanitizeCssNonNegativeLength
} from './cssUtil';

// R612: Comprehensive tests for sanitizeCssValue CSS injection defense
describe('sanitizeCssValue', () => {
	it('returns empty string for null/undefined', () => {
		expect(sanitizeCssValue(null)).toBe('');
		expect(sanitizeCssValue(undefined)).toBe('');
	});

	it('converts numbers to string', () => {
		expect(sanitizeCssValue(42)).toBe('42');
		expect(sanitizeCssValue(0)).toBe('0');
		expect(sanitizeCssValue(-1.5)).toBe('-1.5');
	});

	it('returns empty for objects and arrays', () => {
		expect(sanitizeCssValue({})).toBe('');
		expect(sanitizeCssValue([])).toBe('');
		expect(sanitizeCssValue({ color: 'red' })).toBe('');
	});

	it('strips semicolons to prevent multi-property injection', () => {
		// Semicolons stripped first, then url( is stripped
		expect(sanitizeCssValue('red; background: url(evil)')).toBe('red background: evil)');
	});

	it('strips curly braces to prevent rule injection', () => {
		expect(sanitizeCssValue('} .evil { color: red }')).toBe(' .evil  color: red ');
	});

	it('strips expression() for legacy IE XSS prevention', () => {
		expect(sanitizeCssValue('expression(alert(1))')).toBe('alert(1))');
	});

	it('strips expression() case insensitively', () => {
		expect(sanitizeCssValue('EXPRESSION(alert(1))')).toBe('alert(1))');
		expect(sanitizeCssValue('Expression(alert(1))')).toBe('alert(1))');
	});

	it('strips url() to prevent external resource loading', () => {
		expect(sanitizeCssValue('url(http://evil.com/img.png)')).toBe('http://evil.com/img.png)');
	});

	it('strips url() case insensitively', () => {
		expect(sanitizeCssValue('URL(http://evil.com/img.png)')).toBe('http://evil.com/img.png)');
	});

	it('strips CSS comments to prevent obfuscation', () => {
		// Comment stripped first → "url(evil)" → url( stripped → "evil)"
		expect(sanitizeCssValue('u/**/rl(evil)')).toBe('evil)');
		expect(sanitizeCssValue('red /* comment */ blue')).toBe('red  blue');
	});

	it('strips CSS unicode escape sequences', () => {
		// \\75 is a single CSS unicode escape (matches /\\[0-9a-fA-F]{1,6}\s?/)
		// After stripping, remaining "rl(evil)" is not matched by url\s*\( since it lacks "url"
		expect(sanitizeCssValue('\\75rl(evil)')).toBe('rl(evil)');
		// \\000075 is a 6-char CSS unicode escape
		expect(sanitizeCssValue('\\000075rl(evil)')).toBe('rl(evil)');
	});

	it('strips backslash escapes', () => {
		// \\u matches \\. (backslash escape), then 0072l remains
		expect(sanitizeCssValue('\\u0072l(evil)')).toBe('0072l(evil)');
	});

	it('passes through safe values', () => {
		expect(sanitizeCssValue('10px')).toBe('10px');
		expect(sanitizeCssValue('#ff0000')).toBe('#ff0000');
		expect(sanitizeCssValue('calc(100% - 20px)')).toBe('calc(100% - 20px)');
		expect(sanitizeCssValue('var(--color-primary)')).toBe('var(--color-primary)');
	});

	it('handles bigint input', () => {
		expect(sanitizeCssValue(BigInt(42))).toBe('42');
	});
});

describe('sanitizeCssKeyword', () => {
	it('accepts allowed keyword', () => {
		expect(sanitizeCssKeyword('row', ['row', 'column'], 'row')).toBe('row');
	});

	it('falls back on disallowed keyword', () => {
		expect(sanitizeCssKeyword('evil', ['row', 'column'], 'row')).toBe('row');
	});

	it('is case insensitive', () => {
		expect(sanitizeCssKeyword('ROW', ['row', 'column'], 'row')).toBe('row');
	});

	it('falls back on null/undefined', () => {
		expect(sanitizeCssKeyword(null, ['row'], 'fallback')).toBe('fallback');
		expect(sanitizeCssKeyword(undefined, ['row'], 'fallback')).toBe('fallback');
	});

	it('strips injection attempts', () => {
		expect(sanitizeCssKeyword('row; evil', ['row'], 'fallback')).toBe('fallback');
	});
});

describe('cssUtil length sanitizers', () => {
	it('keeps valid shorthand lengths', () => {
		expect(sanitizeCssLengthList('8px 12px', '0', 2, { allowNegative: false })).toBe('8px 12px');
	});

	it('falls back when shorthand includes negative lengths for non-negative props', () => {
		expect(sanitizeCssLengthList('-8px 12px', '0', 2, { allowNegative: false })).toBe('0');
	});

	it('falls back non-negative single length on negative literal', () => {
		expect(sanitizeCssNonNegativeLength('-16px', '100px')).toBe('100px');
	});

	it('keeps negative single length when generic length sanitizer is used', () => {
		expect(sanitizeCssLength('-16px', '100px')).toBe('-16px');
	});

	it('normalizes unitless numeric length tokens to px', () => {
		expect(sanitizeCssNonNegativeLength(12, '100px')).toBe('12px');
	});

	it('rejects unary-negative functional lengths for non-negative props', () => {
		expect(sanitizeCssNonNegativeLength('calc(-10px)', '100px')).toBe('100px');
	});

	it('keeps subtraction-based functional lengths for non-negative props', () => {
		expect(sanitizeCssNonNegativeLength('calc(100% - 10px)', '100px')).toBe('calc(100% - 10px)');
	});

	it('rejects multiplicative unary-negative functional lengths for non-negative props', () => {
		expect(sanitizeCssNonNegativeLength('calc(100%*-1)', '100px')).toBe('100px');
	});

	it('rejects additive unary-negative functional lengths for non-negative props', () => {
		expect(sanitizeCssNonNegativeLength('calc(0px + -10px)', '100px')).toBe('100px');
	});
});
