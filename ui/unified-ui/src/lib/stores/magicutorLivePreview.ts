import { browser } from '$app/environment';
import { writable, type Readable } from 'svelte/store';

const LIVE_PREVIEW_CHANNEL = 'magicutor-live-preview';
const LIVE_PREVIEW_READY_TIMEOUT_MS = 1500;
const LIVE_PREVIEW_REFRESH_MS = 3000;

type BridgeMessageType =
	| 'magicutor_live_preview_ready'
	| 'magicutor_live_preview_sources'
	| 'magicutor_live_preview_frame'
	| 'magicutor_live_preview_tab_closed'
	| 'magicutor_live_preview_error'
	| 'magicutor_live_preview_capture_pending';

interface BridgeMessageBase {
	channel: typeof LIVE_PREVIEW_CHANNEL;
	sender: 'extension';
	type: BridgeMessageType;
	viewerId: string;
	executionId?: string | null;
}

interface ReadyBridgeMessage extends BridgeMessageBase {
	type: 'magicutor_live_preview_ready';
	extensionVersion?: string;
}

interface PreviewSource {
	executionId: string;
	sessionId: string | null;
	tabId: number;
	windowId: number;
	title: string;
	url: string;
	favIconUrl?: string;
	status: string;
	active: boolean;
	sessionTabCount: number;
}

interface SourcesBridgeMessage extends BridgeMessageBase {
	type: 'magicutor_live_preview_sources';
	tabs?: PreviewSource[];
}

interface FrameBridgeMessage extends BridgeMessageBase {
	type: 'magicutor_live_preview_frame';
	tabId: number;
	mimeType?: string;
	data?: string;
	receivedAt?: number;
}

interface TabClosedBridgeMessage extends BridgeMessageBase {
	type: 'magicutor_live_preview_tab_closed';
	tabId: number;
	reason?: string;
}

interface ErrorBridgeMessage extends BridgeMessageBase {
	type: 'magicutor_live_preview_error';
	code?: string;
	message?: string;
}

interface CapturePendingBridgeMessage extends BridgeMessageBase {
	type: 'magicutor_live_preview_capture_pending';
	tabId?: number;
	message?: string;
}

type BridgeMessage =
	| ReadyBridgeMessage
	| SourcesBridgeMessage
	| FrameBridgeMessage
	| TabClosedBridgeMessage
	| ErrorBridgeMessage
	| CapturePendingBridgeMessage;

export type MagicutorPreviewTabState = 'connecting' | 'streaming' | 'closed' | 'error';
export type MagicutorLivePreviewStatus =
	| 'idle'
	| 'probing'
	| 'connecting'
	| 'streaming'
	| 'empty'
	| 'unavailable'
	| 'error';

export interface MagicutorPreviewTab {
	executionId: string;
	sessionId: string | null;
	tabId: number;
	windowId: number;
	title: string;
	url: string;
	favIconUrl: string;
	tabStatus: string;
	active: boolean;
	sessionTabCount: number;
	previewState: MagicutorPreviewTabState;
	frameSrc: string | null;
	lastFrameAt: number | null;
	error: string | null;
}

export interface MagicutorLivePreviewSnapshot {
	viewerId: string;
	executionId: string | null;
	extensionAvailable: boolean | null;
	extensionVersion: string | null;
	status: MagicutorLivePreviewStatus;
	pendingCapture: boolean;
	tabs: MagicutorPreviewTab[];
	lastUpdatedAt: number | null;
	error: string | null;
}

export interface MagicutorLivePreviewController extends Readable<MagicutorLivePreviewSnapshot> {
	destroy: () => void;
	refresh: () => void;
	retryCapture: () => void;
}

function makeViewerId(): string {
	if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
		return `magicutor-live-${crypto.randomUUID()}`;
	}
	return `magicutor-live-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
}

function getTargetOrigin(): string {
	if (typeof window === 'undefined') {
		return '*';
	}
	return window.location.origin && window.location.origin !== 'null'
		? window.location.origin
		: '*';
}

function postDashboardMessage(message: Record<string, unknown>): void {
	if (!browser) {
		return;
	}

	window.postMessage(
		{
			channel: LIVE_PREVIEW_CHANNEL,
			sender: 'dashboard',
			...message
		},
		getTargetOrigin()
	);
}

function toPreviewTab(source: PreviewSource, previous?: MagicutorPreviewTab): MagicutorPreviewTab {
	return {
		executionId: source.executionId,
		sessionId: source.sessionId || null,
		tabId: source.tabId,
		windowId: source.windowId,
		title: source.title || `Tab ${source.tabId}`,
		url: source.url || '',
		favIconUrl: source.favIconUrl || '',
		tabStatus: source.status || 'unknown',
		active: !!source.active,
		sessionTabCount: source.sessionTabCount || 0,
		previewState:
			previous?.previewState === 'streaming' || previous?.frameSrc
				? 'streaming'
				: previous?.previewState === 'closed'
					? 'closed'
					: previous?.previewState === 'error'
						? 'error'
						: 'connecting',
		frameSrc: previous?.frameSrc || null,
		lastFrameAt: previous?.lastFrameAt || null,
		error: previous?.error || null
	};
}

function applySources(
	state: MagicutorLivePreviewSnapshot,
	executionId: string | null,
	sources: PreviewSource[]
): MagicutorLivePreviewSnapshot {
	const previousByTabId = new Map(state.tabs.map((tab) => [tab.tabId, tab]));
	const tabs = sources.map((source) => toPreviewTab(source, previousByTabId.get(source.tabId)));
	return {
		...state,
		executionId,
		tabs,
		lastUpdatedAt: Date.now(),
		status: tabs.length > 0 ? (tabs.some((tab) => tab.frameSrc) ? 'streaming' : 'connecting') : 'empty',
		error: null
	};
}

function applyFrame(
	state: MagicutorLivePreviewSnapshot,
	message: FrameBridgeMessage
): MagicutorLivePreviewSnapshot {
	const nextTabs = [...state.tabs];
	const index = nextTabs.findIndex((tab) => tab.tabId === message.tabId);
	const frameSrc = message.data ? `data:${message.mimeType || 'image/jpeg'};base64,${message.data}` : null;

	if (index >= 0) {
		nextTabs[index] = {
			...nextTabs[index],
			frameSrc,
			lastFrameAt: message.receivedAt || Date.now(),
			previewState: 'streaming',
			error: null
		};
	} else {
		nextTabs.push({
			executionId: message.executionId || state.executionId || '',
			sessionId: null,
			tabId: message.tabId,
			windowId: 0,
			title: `Tab ${message.tabId}`,
			url: '',
			favIconUrl: '',
			tabStatus: 'unknown',
			active: false,
			sessionTabCount: 0,
			previewState: 'streaming',
			frameSrc,
			lastFrameAt: message.receivedAt || Date.now(),
			error: null
		});
	}

	return {
		...state,
		executionId: message.executionId || state.executionId,
		tabs: nextTabs,
		lastUpdatedAt: Date.now(),
		status: 'streaming',
		error: null
	};
}

function applyTabClosed(
	state: MagicutorLivePreviewSnapshot,
	message: TabClosedBridgeMessage
): MagicutorLivePreviewSnapshot {
	return {
		...state,
		tabs: state.tabs.map((tab) =>
			tab.tabId === message.tabId
				? {
						...tab,
						frameSrc: null,
						previewState: 'closed',
						error: message.reason || 'Preview disconnected.'
					}
				: tab
		),
		lastUpdatedAt: Date.now()
	};
}

function createInitialSnapshot(
	viewerId: string,
	executionId: string | null | undefined
): MagicutorLivePreviewSnapshot {
	return {
		viewerId,
		executionId: executionId || null,
		extensionAvailable: null,
		extensionVersion: null,
		status: executionId ? 'probing' : 'idle',
		pendingCapture: false,
		tabs: [],
		lastUpdatedAt: null,
		error: null
	};
}

export function createMagicutorLivePreviewController(
	executionId: string | null | undefined
): MagicutorLivePreviewController {
	const viewerId = makeViewerId();
	const store = writable<MagicutorLivePreviewSnapshot>(createInitialSnapshot(viewerId, executionId));

	if (!browser) {
		return {
			subscribe: store.subscribe,
			refresh: () => {},
			retryCapture: () => {},
			destroy: () => {}
		};
	}

	let readyTimeoutId: ReturnType<typeof setTimeout> | null = null;
	let refreshIntervalId: ReturnType<typeof setInterval> | null = null;

	function clearReadyTimeout(): void {
		if (readyTimeoutId) {
			clearTimeout(readyTimeoutId);
			readyTimeoutId = null;
		}
	}

	function postPing(): void {
		postDashboardMessage({
			type: 'magicutor_live_preview_ping',
			viewerId
		});
	}

	function subscribeExecution(): void {
		if (!executionId) {
			return;
		}

		postDashboardMessage({
			type: 'magicutor_live_preview_subscribe',
			viewerId,
			executionId
		});
	}

	function postRefresh(): void {
		if (!executionId) {
			return;
		}

		postDashboardMessage({
			type: 'magicutor_live_preview_refresh',
			viewerId,
			executionId
		});
	}

	function startReadyTimeout(): void {
		clearReadyTimeout();
		readyTimeoutId = setTimeout(() => {
			store.update((state) => ({
				...state,
				extensionAvailable: false,
				status: 'unavailable',
				error: 'Magicutor extension was not detected on this page.'
			}));
		}, LIVE_PREVIEW_READY_TIMEOUT_MS);
	}

	function refresh(): void {
		postRefresh();
	}

	function retryCapture(): void {
		postDashboardMessage({
			type: 'magicutor_live_preview_retry_capture',
			viewerId,
			executionId: executionId || null
		});
		store.update((state) => ({
			...state,
			pendingCapture: false,
			error: null
		}));
	}

	function handleMessage(event: MessageEvent): void {
		if (event.source !== window) {
			return;
		}

		const message = event.data as BridgeMessage | undefined;
		if (
			!message
			|| message.channel !== LIVE_PREVIEW_CHANNEL
			|| message.sender !== 'extension'
			|| message.viewerId !== viewerId
		) {
			return;
		}

		clearReadyTimeout();

		if (message.type === 'magicutor_live_preview_ready') {
			store.update((state) => ({
				...state,
				extensionAvailable: true,
				extensionVersion: message.extensionVersion || state.extensionVersion,
				status: executionId ? (state.tabs.length > 0 ? state.status : 'connecting') : 'idle',
				error: null
			}));
			return;
		}

		if (message.type === 'magicutor_live_preview_sources') {
			store.update((state) => ({
				...applySources(state, message.executionId || executionId || null, message.tabs || []),
				extensionAvailable: true
			}));
			return;
		}

		if (message.type === 'magicutor_live_preview_frame') {
			store.update((state) => ({
				...applyFrame(state, message),
				extensionAvailable: true,
				pendingCapture: false
			}));
			return;
		}

		if (message.type === 'magicutor_live_preview_tab_closed') {
			store.update((state) => ({
				...applyTabClosed(state, message),
				extensionAvailable: true
			}));
			return;
		}

		if (message.type === 'magicutor_live_preview_error') {
			store.update((state) => ({
				...state,
				extensionAvailable: true,
				status: state.tabs.some((tab) => tab.previewState === 'streaming') ? 'streaming' : 'error',
				error: message.message || 'Failed to load live previews.',
				lastUpdatedAt: Date.now()
			}));
			return;
		}

		if (message.type === 'magicutor_live_preview_capture_pending') {
			store.update((state) => ({
				...state,
				extensionAvailable: true,
				pendingCapture: true,
				error: (message as CapturePendingBridgeMessage).message || 'Click to start live preview',
				lastUpdatedAt: Date.now()
			}));
		}
	}

	function destroy(): void {
		clearReadyTimeout();
		if (refreshIntervalId) {
			clearInterval(refreshIntervalId);
			refreshIntervalId = null;
		}
		window.removeEventListener('message', handleMessage);
		postDashboardMessage({
			type: 'magicutor_live_preview_unsubscribe',
			viewerId,
			executionId: executionId || null
		});
	}

	window.addEventListener('message', handleMessage);

	if (executionId) {
		startReadyTimeout();
		postPing();
		subscribeExecution();
		refreshIntervalId = setInterval(() => {
			postRefresh();
		}, LIVE_PREVIEW_REFRESH_MS);
	} else {
		store.update((state) => ({
			...state,
			status: 'idle'
		}));
	}

	return {
		subscribe: store.subscribe,
		refresh,
		retryCapture,
		destroy
	};
}
