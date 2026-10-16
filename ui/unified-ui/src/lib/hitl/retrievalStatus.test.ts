import { beforeEach, describe, expect, it, vi } from 'vitest';

type Listener = (events: unknown[]) => void;
type ConnectionListener = (status: string) => void;
const listeners: Listener[] = [];
const connectionListeners: ConnectionListener[] = [];

vi.mock('$lib/realtime/v2-websocket', () => ({
	v2Events: {
		subscribe: (callback: Listener) => {
			listeners.push(callback);
			return () => listeners.splice(listeners.indexOf(callback), 1);
		},
		connectionStatus: {
			subscribe: (callback: ConnectionListener) => {
				connectionListeners.push(callback);
				callback('connected');
				return () => connectionListeners.splice(connectionListeners.indexOf(callback), 1);
			}
		}
	}
}));

vi.mock('$lib/stores/scopeIdentityStore', () => ({
	scopedRequestHeaders: (headers: Record<string, string>) => headers
}));

import { parseRetrievalStatus, retrievalStatusLine, subscribeRetrievalStatus } from './retrievalStatus';

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

describe('retrievalStatus', () => {
	beforeEach(() => {
		listeners.length = 0;
		connectionListeners.length = 0;
	});

	it('follows the resolver: one fetch, then the status events, then a fetch on reconnect', async () => {
		const fetched = vi.fn(async () =>
			new Response(JSON.stringify({ correlation_id: 'req-1', status: 'waiting', sources: ['gmail'] }), { status: 200 })
		);
		vi.stubGlobal('fetch', fetched);
		const seen: string[] = [];
		const unsubscribe = subscribeRetrievalStatus('req-1', (status) => seen.push(status.status));
		await flush();
		expect(seen).toEqual(['waiting']);
		expect(fetched).toHaveBeenCalledTimes(1);
		// Events for this ask move the line; another ask's do not.
		for (const listener of listeners) {
			listener([
				{ event_type: 'VerificationRetrievalStatus', data: { correlation_id: 'req-2', status: 'code_used' } },
				{ event_type: 'VerificationRetrievalStatus', data: { correlation_id: 'req-1', status: 'ambiguous', sources: ['gmail'], code: '482913' } },
				{ event_type: 'HitlResolved', data: { correlation_id: 'req-1' } }
			]);
		}
		expect(seen).toEqual(['waiting', 'ambiguous']);
		expect(fetched).toHaveBeenCalledTimes(1);
		// A reconnect closes the gap with one fetch.
		for (const listener of connectionListeners) listener('disconnected');
		for (const listener of connectionListeners) listener('connected');
		await flush();
		expect(fetched).toHaveBeenCalledTimes(2);
		expect(seen).toEqual(['waiting', 'ambiguous', 'waiting']);
		unsubscribe();
		expect(listeners).toHaveLength(0);
		expect(connectionListeners).toHaveLength(0);
		for (const listener of listeners) listener([]);
		vi.unstubAllGlobals();
	});

	it('parses tolerantly and never carries a code', () => {
		const status = parseRetrievalStatus(
			{ correlation_id: 'req-1', status: 'waiting', sources: ['gmail', 'android_notification'], reason: null, code: '123456' },
			'req-1'
		);
		expect(status.status).toBe('waiting');
		expect(status.sources).toEqual(['gmail', 'android_notification']);
		expect(JSON.stringify(status)).not.toContain('123456');
		expect(parseRetrievalStatus({ status: 'weird' }, 'x').status).toBe('none');
		expect(parseRetrievalStatus(null, 'x').status).toBe('none');
	});

	it('words each state for the owner', () => {
		expect(retrievalStatusLine(parseRetrievalStatus({ status: 'waiting', sources: ['gmail', 'android_notification'] }, 'r'))).toBe(
			'Waiting for the verification code from email or your phone… you can also type it below.'
		);
		expect(retrievalStatusLine(parseRetrievalStatus({ status: 'code_used' }, 'r'))).toBe('Code received and used.');
		expect(retrievalStatusLine(parseRetrievalStatus({ status: 'ambiguous' }, 'r'))).toMatch(/More than one code/);
		expect(retrievalStatusLine(parseRetrievalStatus({ status: 'unavailable', sources: [] }, 'r'))).toBeNull();
		expect(retrievalStatusLine(parseRetrievalStatus({ status: 'unavailable', sources: ['gmail'] }, 'r'))).toMatch(/could not find/);
		expect(retrievalStatusLine(parseRetrievalStatus({ status: 'none' }, 'r'))).toBeNull();
		expect(retrievalStatusLine(parseRetrievalStatus({ status: 'stopped' }, 'r'))).toBeNull();
	});
});
