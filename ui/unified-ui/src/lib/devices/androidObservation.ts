const CAPABILITY = 'host.android-observation';
export const ANDROID_OBSERVATION_DESKTOP_LINK = 'magican://android-observation';
const LOCAL_ANDROID_OBSERVATION_URL = 'http://127.0.0.1:3017/host/ui/android-observation';

export type AndroidAutomationTrustMode = 'play_integrity' | 'owner_pinned_private_build';

export interface AndroidAutomationTrustOption {
	mode: AndroidAutomationTrustMode;
	label: string;
	description: string;
	recommended: boolean;
	ready: boolean;
	missing: string[];
}

export function androidObservationDesktopLink(mode: AndroidAutomationTrustMode): string {
	return mode === 'play_integrity'
		? 'magican://android-observation/play-integrity'
		: ANDROID_OBSERVATION_DESKTOP_LINK;
}

export interface AndroidObservationDesktop {
	deviceId: string;
	clientVersion: string;
}

export function canOpenLocalAndroidObservation(
	url: Pick<URL, 'hostname'>,
	userAgent: string
): boolean {
	const hostname = url.hostname.toLowerCase().replace(/^\[|\]$/g, '');
	const loopback = hostname === 'localhost' || hostname === '127.0.0.1' || hostname === '::1';
	const mobile = /Android|iPhone|iPad|iPod/i.test(userAgent);
	const desktop = /Macintosh|Mac OS X|Windows NT|X11|Linux (?:x86_64|i686|aarch64)/i.test(userAgent);
	return loopback && desktop && !mobile;
}

interface EdgeDeviceEnvelope {
	devices?: Array<{
		device_id?: string;
		client_version?: string;
		capabilities?: Array<{ capability?: string; operations?: string[] }>;
	}>;
}

interface EdgeCallResult {
	status?: string;
	payload?: { opened?: boolean } | null;
	error?: { message?: string } | null;
}

async function responseJson<T>(response: Response): Promise<T> {
	const body = await response.json().catch(() => ({}));
	if (!response.ok) {
		const detail = typeof body?.detail === 'string'
			? body.detail
			: typeof body?.error === 'string'
				? body.error
				: `HTTP ${response.status}`;
		throw new Error(detail);
	}
	return body as T;
}

export async function findAndroidObservationDesktop(): Promise<AndroidObservationDesktop | null> {
	const envelope = await responseJson<EdgeDeviceEnvelope>(
		await fetch('/api/magician/v2/edge/devices', { headers: { Accept: 'application/json' } })
	);
	const desktop = envelope.devices?.find((candidate) =>
		candidate.capabilities?.some((capability) =>
			capability.capability === CAPABILITY && capability.operations?.includes('open-settings')
		)
	);
	return desktop?.device_id
		? { deviceId: desktop.device_id, clientVersion: desktop.client_version ?? 'unknown' }
		: null;
}

export async function getAndroidAutomationTrustOptions(): Promise<AndroidAutomationTrustOption[]> {
	const envelope = await responseJson<{ options?: AndroidAutomationTrustOption[] }>(
		await fetch('/api/magician/v2/devices/apps-automation/trust-options', {
			headers: { Accept: 'application/json' }
		})
	);
	return envelope.options?.filter((option) =>
		option.mode === 'play_integrity' || option.mode === 'owner_pinned_private_build'
	) ?? [];
}

export async function openAndroidObservationApproval(
	deviceId: string,
	trustMode: AndroidAutomationTrustMode
): Promise<void> {
	const executionId = typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function'
		? crypto.randomUUID()
		: `android-observation-${Date.now()}`;
	const result = await responseJson<EdgeCallResult>(
		await fetch(`/api/magician/v2/edge/devices/${encodeURIComponent(deviceId)}/invoke`, {
			method: 'POST',
			headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
			body: JSON.stringify({
				execution_id: executionId,
				execution_epoch: 1,
				idempotency_key: executionId,
				capability: CAPABILITY,
				operation: 'open-settings',
				payload: { trust_mode: trustMode },
				timeout_ms: 10_000
			})
		})
	);
	if (result.status !== 'succeeded' || result.payload?.opened !== true) {
		throw new Error(result.error?.message ?? 'The desktop did not open Android observation setup.');
	}
}

export async function openLocalAndroidObservationApproval(
	trustMode: AndroidAutomationTrustMode
): Promise<void> {
	const result = await responseJson<{ opened?: boolean }>(
		await fetch(`${LOCAL_ANDROID_OBSERVATION_URL}?trust=${encodeURIComponent(trustMode)}`, { method: 'POST' })
	);
	if (result.opened !== true) {
		throw new Error('Magican Desktop did not open Android observation setup.');
	}
}
