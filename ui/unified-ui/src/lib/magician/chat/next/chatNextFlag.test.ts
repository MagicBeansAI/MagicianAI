import { describe, expect, it } from 'vitest';
import { resolveChatNext } from './chatNextFlag';

describe('resolveChatNext', () => {
	it('defaults on in every theme', () => {
		expect(resolveChatNext({})).toBe(true);
	});
	it('a stored choice beats the default', () => {
		expect(resolveChatNext({ stored: 'off' })).toBe(false);
		expect(resolveChatNext({ stored: 'on' })).toBe(true);
	});
	it('the URL beats everything', () => {
		expect(resolveChatNext({ stored: 'off', url: '1' })).toBe(true);
		expect(resolveChatNext({ stored: 'on', url: '0' })).toBe(false);
	});
	it('ignores junk values', () => {
		expect(resolveChatNext({ stored: 'maybe', url: 'yes' })).toBe(true);
	});
});
