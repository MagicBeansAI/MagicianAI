import { describe, expect, it } from 'vitest';
import { parseHarnessAuthState, signInLabel, withCredentials } from './harnessAuth';

describe('parseHarnessAuthState', () => {
	it('keeps known engines and modes and drops the rest', () => {
		const state = parseHarnessAuthState({
			mode: 'login_first',
			engines: { codex: 'magician', cursor: 'magician', grok: 'sometimes' },
			status: {
				pi: { mode: 'login_first', signed_in: false, uses_magician_credentials: true },
				claude_code: { mode: 'login_first', signed_in: true, uses_magician_credentials: false },
				other: { mode: 'magician', signed_in: true, uses_magician_credentials: true }
			}
		});
		expect(state.engines).toEqual({ codex: 'magician' });
		expect(state.status.pi?.uses_magician_credentials).toBe(true);
		expect(state.status.claude_code?.signed_in).toBe(true);
		expect(Object.keys(state.status)).toEqual(['pi', 'claude_code']);
	});

	it('reads the sign-in state, its reason and remedy, and the key-fallback policy', () => {
		const state = parseHarnessAuthState({
			mode: 'login_first',
			engines: {},
			status: {
				grok: {
					mode: 'login_first',
					signed_in: false,
					sign_in: 'expired',
					sign_in_detail: 'Your session has expired. Run `grok login` to sign in again.',
					remedy: 'Run `grok login`.',
					last_failure: {
						class: 'authentication',
						reason: 'Your session has expired.',
						remedy: 'Run `grok login`.',
						at_ms: 1
					},
					uses_magician_credentials: false,
					key_fallback: 'ask',
					magician_key_available: true
				},
				codex: { mode: 'login_only', signed_in: true, uses_magician_credentials: false }
			}
		});
		const grok = state.status.grok;
		expect(grok?.sign_in).toBe('expired');
		expect(grok?.key_fallback).toBe('ask');
		expect(grok?.last_failure?.reason).toBe('Your session has expired.');
		expect(signInLabel(grok)).toBe('Session expired — run `grok login`');
		// An older server: derived from the boolean and the mode.
		expect(state.status.codex?.sign_in).toBe('signed_in');
		expect(state.status.codex?.key_fallback).toBe('never');
		expect(signInLabel(state.status.codex)).toBe('Signed in');
	});

	it('refuses a body without a known default mode', () => {
		expect(() => parseHarnessAuthState({ mode: 'api' })).toThrow();
	});
});

describe('withCredentials', () => {
	it('labels a harness engine by whose credentials it uses', () => {
		expect(withCredentials('Codex', 'login')).toBe('Codex · your login');
		expect(withCredentials('Grok', 'magician_key')).toBe('Grok · Magician’s key');
		expect(withCredentials('Antigravity', 'none')).toBe('Antigravity · not signed in');
		expect(withCredentials('Grok', 'expired')).toBe('Grok · session expired');
		expect(withCredentials('Magician', undefined)).toBe('Magician');
		expect(withCredentials('Pi', 'later')).toBe('Pi');
	});
});
