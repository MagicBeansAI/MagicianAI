import { vi } from 'vitest';

export interface MockFetchCall {
	input: RequestInfo | URL;
	init?: RequestInit;
	url: string;
	method: string;
}

export type MockFetchHandler = (call: MockFetchCall) => Response | Promise<Response>;

export interface MockFetchRoute {
	method?: string;
	match: string | RegExp | ((call: MockFetchCall) => boolean);
	handle: MockFetchHandler;
}

export class MockWebSocket {
	static readonly CONNECTING = 0;
	static readonly OPEN = 1;
	static readonly CLOSING = 2;
	static readonly CLOSED = 3;
	static instances: MockWebSocket[] = [];

	readonly url: string;
	readonly protocol = '';
	readonly extensions = '';
	readonly bufferedAmount = 0;
	binaryType: BinaryType = 'blob';
	readyState = MockWebSocket.CONNECTING;
	onopen: ((event: Event) => void) | null = null;
	onmessage: ((event: MessageEvent) => void) | null = null;
	onerror: ((event: Event) => void) | null = null;
	onclose: ((event: CloseEvent) => void) | null = null;
	readonly send = vi.fn();
	private listeners = new Map<
		string,
		Array<{ callback: EventListenerOrEventListenerObject; once: boolean }>
	>();

	constructor(url: string | URL) {
		this.url = String(url);
		MockWebSocket.instances.push(this);
	}

	open(): void {
		this.readyState = MockWebSocket.OPEN;
		this.dispatchEvent(new Event('open'));
	}

	receive(data: unknown): void {
		this.dispatchEvent(new MessageEvent('message', { data }));
	}

	close(): void {
		this.readyState = MockWebSocket.CLOSED;
	}

	serverClose(code = 1006, reason = ''): void {
		this.readyState = MockWebSocket.CLOSED;
		this.dispatchEvent(new CloseEvent('close', { code, reason }));
	}

	addEventListener(
		type: string,
		callback: EventListenerOrEventListenerObject,
		options?: boolean | AddEventListenerOptions
	): void {
		const once = typeof options === 'object' && options.once === true;
		const listeners = this.listeners.get(type) ?? [];
		listeners.push({ callback, once });
		this.listeners.set(type, listeners);
	}

	removeEventListener(type: string, callback: EventListenerOrEventListenerObject): void {
		this.listeners.set(
			type,
			(this.listeners.get(type) ?? []).filter((listener) => listener.callback !== callback)
		);
	}

	dispatchEvent(event: Event): boolean {
		const propertyHandler = this[`on${event.type}` as keyof MockWebSocket];
		if (typeof propertyHandler === 'function') {
			(propertyHandler as (nextEvent: Event) => void)(event);
		}
		const listeners = [...(this.listeners.get(event.type) ?? [])];
		for (const listener of listeners) {
			if (typeof listener.callback === 'function') listener.callback(event);
			else listener.callback.handleEvent(event);
			if (listener.once) this.removeEventListener(event.type, listener.callback);
		}
		return true;
	}
}

export function jsonResponse(body: unknown, init: ResponseInit = {}): Response {
	const headers = new Headers(init.headers);
	if (!headers.has('Content-Type')) headers.set('Content-Type', 'application/json');
	return new Response(JSON.stringify(body), { ...init, headers });
}

export function textResponse(body: string, init: ResponseInit = {}): Response {
	return new Response(body, init);
}

function requestUrl(input: RequestInfo | URL): string {
	if (typeof input === 'string') return input;
	if (input instanceof URL) return input.toString();
	return input.url;
}

function routeMatches(route: MockFetchRoute, call: MockFetchCall): boolean {
	if (route.method && route.method.toUpperCase() !== call.method) return false;
	if (typeof route.match === 'string') return call.url.includes(route.match);
	if (route.match instanceof RegExp) return route.match.test(call.url);
	return route.match(call);
}

export function installFetchMock(routes: MockFetchRoute[]) {
	const calls: MockFetchCall[] = [];
	const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
		const requestMethod = input instanceof Request ? input.method : undefined;
		const call: MockFetchCall = {
			input,
			init,
			url: requestUrl(input),
			method: (init?.method ?? requestMethod ?? 'GET').toUpperCase()
		};
		calls.push(call);
		const route = routes.find((candidate) => routeMatches(candidate, call));
		if (!route) throw new Error(`Unhandled test request: ${call.method} ${call.url}`);
		return route.handle(call);
	});
	vi.stubGlobal('fetch', fetchMock);
	return { calls, fetchMock };
}

function createMemoryStorage(): Storage {
	const values = new Map<string, string>();
	return {
		get length() {
			return values.size;
		},
		clear(): void {
			values.clear();
		},
		getItem(key: string): string | null {
			return values.get(key) ?? null;
		},
		key(index: number): string | null {
			return Array.from(values.keys())[index] ?? null;
		},
		removeItem(key: string): void {
			values.delete(key);
		},
		setItem(key: string, value: string): void {
			values.set(key, String(value));
		}
	};
}

export function installTestStorage(): void {
	const storage = createMemoryStorage();
	Object.defineProperty(globalThis, 'localStorage', {
		configurable: true,
		value: storage
	});
	if (typeof window !== 'undefined') {
		Object.defineProperty(window, 'localStorage', {
			configurable: true,
			value: storage
		});
	}
}

/**
 * Install one global for the whole file, in a way `vi.unstubAllGlobals()` cannot
 * undo.
 *
 * **Why not `vi.stubGlobal`.** A stub is per-test state that vitest is expected to
 * roll back; these are the *environment* jsdom does not provide, installed once from
 * `setup.ts` before any test runs. Twenty-nine test files call
 * `vi.unstubAllGlobals()` in an `afterEach`, and every one of them was deleting
 * these — so the first test in such a file ran with `ResizeObserver` defined and
 * every test after it ran without, since nothing reinstalls them mid-file.
 *
 * It surfaced as `ResizeObserver is not defined` in nine `InternalTasksWorkspace`
 * cases the moment the task panel's header started measuring its own clamp, in a
 * component that is correct in every browser. The three globals below already sat
 * beside three others (`matchMedia`, `scrollIntoView`, `animate`) that were durable
 * for no stated reason; this makes the file consistent in the safe direction.
 *
 * A test that deliberately stubs one of these still works: `vi.stubGlobal` saves
 * whatever is here and restores it on unstub, which now restores the polyfill
 * instead of removing the property.
 */
function defineGlobal(name: string, value: unknown): void {
	Object.defineProperty(globalThis, name, { configurable: true, writable: true, value });
	if (typeof window !== 'undefined' && window !== (globalThis as unknown as Window)) {
		Object.defineProperty(window, name, { configurable: true, writable: true, value });
	}
}

export function installBrowserTestPolyfills(): void {
	defineGlobal('WebSocket', MockWebSocket);

	// jsdom exposes this method but reports every call as an unimplemented
	// operation. Components already handle a missing 2D context, so model that
	// behavior directly and keep otherwise-successful test runs warning-free.
	if (typeof HTMLCanvasElement !== 'undefined') {
		Object.defineProperty(HTMLCanvasElement.prototype, 'getContext', {
			configurable: true,
			writable: true,
			value: vi.fn(() => null)
		});
	}

	if (!window.matchMedia) {
		Object.defineProperty(window, 'matchMedia', {
			configurable: true,
			value: vi.fn((query: string) => ({
				matches: false,
				media: query,
				onchange: null,
				addEventListener: vi.fn(),
				removeEventListener: vi.fn(),
				addListener: vi.fn(),
				removeListener: vi.fn(),
				dispatchEvent: vi.fn(() => true)
			}))
		});
	}

	if (!Element.prototype.scrollIntoView) {
		Element.prototype.scrollIntoView = vi.fn();
	}

	if (!Element.prototype.animate) {
		Element.prototype.animate = vi.fn(() => {
			const animation = {
				currentTime: 0,
				effect: null,
				onfinish: null as ((event: AnimationPlaybackEvent) => void) | null,
				playState: 'running' as AnimationPlayState,
				cancel: vi.fn(),
				finish: vi.fn(),
				pause: vi.fn(),
				play: vi.fn(),
				reverse: vi.fn(),
				updatePlaybackRate: vi.fn(),
				finished: Promise.resolve(undefined),
				ready: Promise.resolve(undefined),
				playbackRate: 1,
				startTime: 0,
				timeline: null,
				replaceState: 'active' as AnimationReplaceState,
				id: '',
				oncancel: null,
				onremove: null,
				commitStyles: vi.fn(),
				persist: vi.fn()
			};
			queueMicrotask(() => animation.onfinish?.(new Event('finish') as AnimationPlaybackEvent));
			return animation as unknown as Animation;
		});
	}

	if (!globalThis.ResizeObserver) {
		defineGlobal(
			'ResizeObserver',
			class ResizeObserver {
				observe(): void {}
				unobserve(): void {}
				disconnect(): void {}
			}
		);
	}

	if (!globalThis.IntersectionObserver) {
		defineGlobal(
			'IntersectionObserver',
			class IntersectionObserver {
				readonly root = null;
				readonly rootMargin = '0px';
				readonly thresholds = [0];
				observe(): void {}
				unobserve(): void {}
				disconnect(): void {}
				takeRecords(): IntersectionObserverEntry[] {
					return [];
				}
			}
		);
	}
}

export async function settleMicrotasks(turns = 3): Promise<void> {
	for (let index = 0; index < turns; index += 1) await Promise.resolve();
}
