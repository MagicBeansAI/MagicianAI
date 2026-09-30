export interface PairedDevice {
	principal: string;
	workspace: string;
	device_id: string;
	label: string;
	paired_at_ms: number;
	last_seen_ms: number | null;
	client_kind: 'ios' | 'android' | 'desktop';
	capabilities: Array<'mobile_client' | 'device_automation' | 'edge_client'>;
	automation_review_generation: number;
	automation_review?: DeviceAutomationReview;
}

export interface DeviceAutomationReview {
	target_ref: string;
	generation: number;
	review_digest: string;
	actions: ['snapshot'];
	allowed_packages: string[];
	reviewed_at_ms: number;
}

export interface DeviceEnrollment {
	enrollment_id: string;
	enrollment_uri: string;
	qr_svg: string;
	expires_at_ms: number;
	principal: string;
	workspace: string;
	client_kind: 'ios' | 'android' | 'desktop';
	connection_mode: DeviceConnectionMode;
	origin: string;
}

export type DeviceConnectionMode = 'same_wifi' | 'remote';

export interface DeviceConnectionOptions {
	same_wifi: string | null;
	remote: string | null;
}

interface DeviceListEnvelope {
	principal: string;
	workspace: string;
	devices: PairedDevice[];
	/** Absent on older backends; enrollment itself still checks readiness. */
	pairing_available?: boolean;
	/** Absent on older backends, which support the remote route only. */
	connection_options?: DeviceConnectionOptions;
}

export function pairedDeviceSnapshot(devices: PairedDevice[]): Map<string, number> {
	return new Map(devices.map((device) => [device.device_id, device.paired_at_ms]));
}

/**
 * Resolve completion from an authoritative roster delta, not wall-clock time.
 * Browser and server clocks can differ, and an unrelated platform enrollment
 * may complete while this QR is open. Re-pairing an existing id counts only
 * when its server-owned paired timestamp advances.
 */
export function completedEnrollmentDevice(
	devices: PairedDevice[],
	clientKind: DeviceEnrollment['client_kind'],
	baseline: ReadonlyMap<string, number>
): PairedDevice | undefined {
	return devices.find((device) =>
		device.client_kind === clientKind &&
		device.paired_at_ms > (baseline.get(device.device_id) ?? Number.NEGATIVE_INFINITY)
	);
}

async function json<T>(response: Response, missingRouteMessage?: string): Promise<T> {
	const body = await response.json().catch(() => ({}));
	if (!response.ok) {
		const code = typeof body?.error === 'string' ? body.error : '';
		throw new Error(
			response.status === 404 && missingRouteMessage
				? missingRouteMessage
			: code === 'too_many_pending_enrollments'
				? 'Too many pairing codes are already open. Wait five minutes and try again.'
				: code === 'android_apps_attestation_policy_unavailable'
					? 'Android Apps enrollment is disabled until the reviewed APK signer and Android attestation-root fingerprints are configured.'
				: code === 'mobile_public_origin_not_configured'
					? 'Remote connection is unavailable until connect.magican.ai is configured for this installation.'
				: code === 'mobile_local_origin_not_available'
					? 'Same-Wi-Fi connection is unavailable. Start Magician with local-network access, then refresh.'
				: code === 'pairing_authority_unavailable'
					? 'Device pairing storage is unavailable. Repair it and restart Magician before creating another code.'
				: code === 'public_origin_invalid'
					? 'This Magician page does not have a phone-reachable HTTP address.'
					: `Device pairing failed (HTTP ${response.status}).`
		);
	}
	return body as T;
}

/** The scope-wide device policy (`GET /devices/policy`). */
export interface DevicePolicyEnvelope {
	screenshot_policy?: string;
	/** Paired devices permitted to read verification codes for a live challenge (secure HITL P6). */
	verification_code_devices?: string[];
}

export async function fetchDevicePolicy(): Promise<DevicePolicyEnvelope> {
	return json<DevicePolicyEnvelope>(
		await fetch('/api/magician/v2/devices/policy', { headers: { Accept: 'application/json' } })
	);
}

/**
 * Permit or withdraw one paired Android device as a verification-code
 * source. Pairing alone grants nothing; this is a separate, reversible grant.
 */
export async function setDeviceVerificationCodes(
	deviceId: string,
	permitted: boolean
): Promise<string[]> {
	const body = await json<{ verification_code_devices?: string[] }>(
		await fetch('/api/magician/v2/devices/policy/verification-codes', {
			method: 'PUT',
			headers: { Accept: 'application/json', 'Content-Type': 'application/json' },
			body: JSON.stringify({ device_id: deviceId, permitted })
		})
	);
	return Array.isArray(body.verification_code_devices) ? body.verification_code_devices : [];
}

export async function listPairedDevices(): Promise<DeviceListEnvelope> {
	return json<DeviceListEnvelope>(
		await fetch('/api/magician/v2/devices', { headers: { Accept: 'application/json' } })
	);
}

export async function beginDeviceEnrollment(
	clientKind: DeviceEnrollment['client_kind'],
	connectionMode: DeviceConnectionMode = 'remote'
): Promise<DeviceEnrollment> {
	return json<DeviceEnrollment>(
		await fetch('/api/magician/v2/devices/enrollment', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
			body: JSON.stringify({ client_kind: clientKind, connection_mode: connectionMode })
		}),
		'The running Magician does not expose the current device-enrollment route. Rebuild and restart it before creating another code.'
	);
}

export async function cancelDeviceEnrollment(enrollmentId: string): Promise<void> {
	await json<{ cancelled: boolean }>(
		await fetch(`/api/magician/v2/devices/enrollment/${encodeURIComponent(enrollmentId)}`, {
			method: 'DELETE',
			headers: { Accept: 'application/json' }
		})
	);
}

export async function unpairDevice(deviceId: string): Promise<void> {
	await json<{ removed: boolean }>(
		await fetch(`/api/magician/v2/devices/${encodeURIComponent(deviceId)}`, {
			method: 'DELETE',
			headers: { Accept: 'application/json' }
		})
	);
}
