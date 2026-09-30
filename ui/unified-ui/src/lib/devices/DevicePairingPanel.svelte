<script lang="ts">
	import { browser } from '$app/environment';
	import { onDestroy, onMount } from 'svelte';
	import { requestConfirmation } from '$lib/stores/confirmationStore';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import {
		ANDROID_OBSERVATION_DESKTOP_LINK,
		androidObservationDesktopLink,
		canOpenLocalAndroidObservation,
		findAndroidObservationDesktop,
		getAndroidAutomationTrustOptions,
		openAndroidObservationApproval,
		openLocalAndroidObservationApproval,
		type AndroidAutomationTrustMode,
		type AndroidAutomationTrustOption,
		type AndroidObservationDesktop
	} from './androidObservation';
	import {
		beginDeviceEnrollment,
		cancelDeviceEnrollment,
		completedEnrollmentDevice,
		listPairedDevices,
		pairedDeviceSnapshot,
		unpairDevice,
		fetchDevicePolicy,
		setDeviceVerificationCodes,
		type DeviceEnrollment,
		type DeviceConnectionMode,
		type DeviceConnectionOptions,
		type PairedDevice
	} from './devicePairing';

	let devices: PairedDevice[] = [];
	/** Secure HITL P6: paired devices permitted as verification-code sources. */
	let verificationCodeDevices: string[] = [];
	let verificationBusy = '';
	async function loadDevicePolicy() {
		try {
			const policy = await fetchDevicePolicy();
			verificationCodeDevices = policy.verification_code_devices ?? [];
		} catch {
			verificationCodeDevices = [];
		}
	}
	async function toggleVerificationCodes(device: PairedDevice) {
		const permitted = !verificationCodeDevices.includes(device.device_id);
		verificationBusy = device.device_id;
		try {
			verificationCodeDevices = await setDeviceVerificationCodes(device.device_id, permitted);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not change the verification-code permission.';
		} finally {
			verificationBusy = '';
		}
	}
	let enrollment: DeviceEnrollment | null = null;
	let loading = true;
	let creating = false;
	let pairingAvailable = true;
	let connectionOptions: DeviceConnectionOptions = { same_wifi: null, remote: null };
	let choosingClientKind: DeviceEnrollment['client_kind'] | null = null;
	let error: string | null = null;
	let edgeBusy = false;
	let androidObservationDesktop: AndroidObservationDesktop | null = null;
	let androidObservationBusy = false;
	let androidObservationChecked = false;
	let androidObservationError: string | null = null;
	let androidTrustOptions: AndroidAutomationTrustOption[] = [];
	let selectedAndroidTrustMode: AndroidAutomationTrustMode = 'owner_pinned_private_build';
	let localAndroidObservationLink: string | null = null;
	let edgeStatus: {
		available: boolean;
		enrolled: boolean;
		connected: boolean;
		serverOrigin: string | null;
		deviceId: string | null;
		label: string | null;
		principal: string | null;
		workspace: string | null;
		error: string | null;
	} | null = null;
	let now = Date.now();
	let enrollmentBaseline = new Map<string, number>();
	let timer: ReturnType<typeof setInterval> | null = null;
	let rosterPoll: ReturnType<typeof setInterval> | null = null;

	$: secondsRemaining = enrollment
		? Math.max(0, Math.ceil((enrollment.expires_at_ms - now) / 1000))
		: 0;
	$: expired = enrollment !== null && secondsRemaining === 0;

	function relativeTime(value: number | null): string {
		if (!value) return 'Never connected';
		const seconds = Math.max(0, Math.round((Date.now() - value) / 1000));
		if (seconds < 60) return seconds < 5 ? 'Just now' : `${seconds}s ago`;
		if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
		if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
		return `${Math.floor(seconds / 86400)}d ago`;
	}

	function countdown(seconds: number): string {
		const minutes = Math.floor(seconds / 60);
		return `${minutes}:${String(seconds % 60).padStart(2, '0')}`;
	}

	function deviceKindLabel(kind: PairedDevice['client_kind']): string {
		if (kind === 'ios') return 'iPhone';
		if (kind === 'desktop') return 'Desktop Edge';
		return 'Android device';
	}

	function isTauriRuntime(): boolean {
		return browser && typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
	}

	async function refreshEdgeStatus(): Promise<void> {
		if (!isTauriRuntime()) return;
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			edgeStatus = await invoke<typeof edgeStatus>('get_edge_client_status');
		} catch {
			edgeStatus = null;
		}
	}

	async function connectThisDesktop(): Promise<void> {
		if (edgeBusy || !isTauriRuntime()) return;
		edgeBusy = true;
		error = null;
		let ticket: DeviceEnrollment | null = null;
		try {
			ticket = await beginDeviceEnrollment('desktop');
			const { invoke } = await import('@tauri-apps/api/core');
			edgeStatus = await invoke<typeof edgeStatus>('enroll_edge_client', {
				enrollmentUri: ticket.enrollment_uri
			});
			await refresh(true);
			showSuccess('This desktop is enrolled and connecting');
		} catch (cause) {
			if (ticket) await cancelDeviceEnrollment(ticket.enrollment_id).catch(() => undefined);
			const message = cause instanceof Error ? cause.message : 'Could not enroll this desktop.';
			error = message;
			showError(message);
		} finally {
			edgeBusy = false;
		}
	}

	async function refreshAndroidObservation(): Promise<void> {
		try {
			const [desktop, options] = await Promise.all([
				findAndroidObservationDesktop(),
				getAndroidAutomationTrustOptions()
			]);
			androidObservationDesktop = desktop;
			androidTrustOptions = options;
			if (!options.some((option) => option.mode === selectedAndroidTrustMode && option.ready)) {
				selectedAndroidTrustMode = options.find((option) => option.ready && option.recommended)?.mode
					?? options.find((option) => option.ready)?.mode
					?? options.find((option) => option.recommended)?.mode
					?? 'owner_pinned_private_build';
			}
			androidObservationError = null;
		} catch (cause) {
			androidObservationDesktop = null;
			androidObservationError = cause instanceof Error ? cause.message : 'Could not check desktop observation support.';
		} finally {
			androidObservationChecked = true;
		}
	}

	async function manageAndroidObservation(): Promise<void> {
		const selected = androidTrustOptions.find((option) => option.mode === selectedAndroidTrustMode);
		if ((!androidObservationDesktop && !localAndroidObservationLink) || androidObservationBusy || !selected?.ready) return;
		androidObservationBusy = true;
		androidObservationError = null;
		try {
			if (androidObservationDesktop) {
				await openAndroidObservationApproval(androidObservationDesktop.deviceId, selectedAndroidTrustMode);
			} else {
				try {
					await openLocalAndroidObservationApproval(selectedAndroidTrustMode);
				} catch (cause) {
					window.location.assign(androidObservationDesktopLink(selectedAndroidTrustMode));
					throw cause;
				}
			}
			showSuccess('Continue the secure approval in Magican Desktop');
		} catch (cause) {
			const message = cause instanceof Error ? cause.message : 'Could not open Android observation setup.';
			androidObservationError = message;
			showError(message);
		} finally {
			androidObservationBusy = false;
		}
	}

	async function refreshAll(): Promise<void> {
		await Promise.all([refresh(), refreshAndroidObservation(), refreshEdgeStatus()]);
	}

	async function refresh(silent = false): Promise<void> {
		if (!silent) loading = true;
		try {
			const result = await listPairedDevices();
			devices = result.devices;
			pairingAvailable = result.pairing_available !== false;
			connectionOptions = result.connection_options ?? {
				same_wifi: null,
				remote: 'Configured remote address'
			};
			error = null;
			const mobileCompleted = enrollment &&
				completedEnrollmentDevice(devices, enrollment.client_kind, enrollmentBaseline);
			if (enrollment && mobileCompleted) {
				const connectedKind = enrollment.client_kind;
				enrollment = null;
				enrollmentBaseline = new Map();
				showSuccess(`${deviceKindLabel(connectedKind)} connected`);
			}
		} catch (cause) {
			if (!silent) error = cause instanceof Error ? cause.message : 'Could not load paired devices.';
		} finally {
			if (!silent) loading = false;
		}
	}

	function chooseConnection(clientKind: DeviceEnrollment['client_kind']): void {
		if (!pairingAvailable) return;
		choosingClientKind = clientKind;
	}

	async function createEnrollment(
		clientKind: DeviceEnrollment['client_kind'],
		connectionMode: DeviceConnectionMode
	): Promise<void> {
		if (!browser || creating || !pairingAvailable) return;
		creating = true;
		error = null;
		try {
			if (enrollment) {
				await cancelDeviceEnrollment(enrollment.enrollment_id).catch(() => undefined);
			}
			enrollmentBaseline = pairedDeviceSnapshot(devices);
			enrollment = await beginDeviceEnrollment(clientKind, connectionMode);
			choosingClientKind = null;
			now = Date.now();
		} catch (cause) {
			const message = cause instanceof Error ? cause.message : 'Could not create a pairing code.';
			error = message;
			showError(message);
		} finally {
			creating = false;
		}
	}

	async function closeEnrollment(): Promise<void> {
		const id = enrollment?.enrollment_id;
		enrollment = null;
		enrollmentBaseline = new Map();
		if (id) {
			await cancelDeviceEnrollment(id).catch(() => undefined);
		}
	}

	async function copyEnrollmentLink(): Promise<void> {
		if (!enrollment || enrollment.client_kind !== 'desktop') return;
		try {
			await navigator.clipboard.writeText(enrollment.enrollment_uri);
			showSuccess('Desktop Edge link copied');
		} catch {
			showError('Could not copy the Desktop Edge link.');
		}
	}

	async function revoke(device: PairedDevice): Promise<void> {
		const confirmed = await requestConfirmation({
			title: `Unpair ${device.label || device.device_id}?`,
			message: 'The device will lose Magican access immediately and must be enrolled again to reconnect.',
			confirmLabel: 'Unpair',
			destructive: true
		});
		if (!confirmed) return;
		try {
			if (device.client_kind === 'desktop' && isTauriRuntime() && edgeStatus?.deviceId === device.device_id) {
				const { invoke } = await import('@tauri-apps/api/core');
				edgeStatus = await invoke<typeof edgeStatus>('revoke_edge_client');
			} else {
				await unpairDevice(device.device_id);
			}
			devices = devices.filter((candidate) => candidate.device_id !== device.device_id);
			showSuccess(`${deviceKindLabel(device.client_kind)} disconnected`);
		} catch (cause) {
			const message = cause instanceof Error ? cause.message : 'Could not unpair this device.';
			showError(message);
		}
	}

	onMount(() => {
		if (browser && canOpenLocalAndroidObservation(new URL(window.location.href), navigator.userAgent)) {
			localAndroidObservationLink = ANDROID_OBSERVATION_DESKTOP_LINK;
		}
		void refresh();
		void loadDevicePolicy();
		void refreshEdgeStatus();
		void refreshAndroidObservation();
		timer = setInterval(() => (now = Date.now()), 1000);
		rosterPoll = setInterval(() => {
			if (enrollment && !expired) void refresh(true);
			if (edgeStatus?.enrolled) void refreshEdgeStatus();
		}, 2000);
	});

	onDestroy(() => {
		if (timer) clearInterval(timer);
		if (rosterPoll) clearInterval(rosterPoll);
	});
</script>

<section class="device-panel" aria-labelledby="connected-devices-title">
	<div class="panel-header">
		<div>
			<p class="overline">Self-hosted connection</p>
			<h2 id="connected-devices-title">Connected devices</h2>
			<p>Connect an iPhone or Android for chat, tasks and voice. Manage Android screen observation here; the enrolled desktop handles only the final signing confirmation. Desktop Edge hosts also provide local CUA and browser capabilities.</p>
		</div>
		<div class="actions">
			<button class="secondary" type="button" disabled={loading} on:click={refreshAll}>
				{loading ? 'Refreshing…' : 'Refresh'}
			</button>
			<button class="primary" type="button" disabled={creating || loading || !pairingAvailable} on:click={() => chooseConnection('ios')}>
				Connect iPhone
			</button>
			<button class="primary" type="button" disabled={creating || loading || !pairingAvailable} on:click={() => chooseConnection('android')}>
				Connect Android
			</button>
			{#if edgeStatus?.available && !edgeStatus.enrolled}
				<button class="primary" type="button" disabled={edgeBusy || loading || !pairingAvailable} on:click={connectThisDesktop}>
					{edgeBusy ? 'Connecting…' : 'Connect this desktop'}
				</button>
			{:else if !isTauriRuntime()}
				<button class="primary" type="button" disabled={creating || loading || !pairingAvailable || !connectionOptions.remote} on:click={() => createEnrollment('desktop', 'remote')}>
					{creating ? 'Preparing…' : 'Connect Desktop Edge'}
				</button>
			{/if}
		</div>
	</div>

	{#if edgeStatus?.enrolled}
		<div class="edge-status" class:online={edgeStatus.connected}>
			<strong>{edgeStatus.connected ? 'Desktop Edge connected' : 'Desktop Edge enrolled'}</strong>
			<span>{edgeStatus.label ?? edgeStatus.deviceId} · {edgeStatus.serverOrigin}</span>
			{#if edgeStatus.error}<span class="edge-error">{edgeStatus.error}</span>{/if}
		</div>
	{/if}

	<div class="observation-card">
		<div>
			<p class="step">Android screen control</p>
			<h3>Android observation</h3>
			<p>Choose how this Android build proves it is trusted. Both choices require hardware-backed Android attestation and final owner approval in Magican Desktop.</p>
			<div class="trust-options" role="radiogroup" aria-label="Android app trust method">
				{#each androidTrustOptions as option (option.mode)}
					<label class:ready={option.ready} class:selected={selectedAndroidTrustMode === option.mode}>
						<input
							type="radio"
							name="android-trust-mode"
							value={option.mode}
							bind:group={selectedAndroidTrustMode}
							disabled={!option.ready}
						/>
						<span>
							<strong>{option.label}{option.recommended ? ' · Recommended' : ''}</strong>
							<small>{option.description}</small>
							{#if !option.ready}<small class="missing">Needs: {option.missing.join(', ')}</small>{/if}
						</span>
					</label>
				{/each}
				{#if androidObservationChecked && androidTrustOptions.length === 0}
					<span class="observation-error">Trust choices could not be read from Magician.</span>
				{/if}
			</div>
			{#if androidTrustOptions.length > 0 && !androidTrustOptions.some((option) => option.ready)}
				<p class="trust-help">Missing values belong under <code>mobile_access</code> in the live <code>magician-config.yaml</code>. Restart Magician after changing them, then refresh this page.</p>
			{/if}
			{#if androidObservationError}<span class="observation-error" role="alert">{androidObservationError}</span>{/if}
			{#if androidObservationDesktop}
				<span class="observation-status">Ready on {androidObservationDesktop.deviceId}</span>
			{:else if localAndroidObservationLink}
				<span class="observation-status">Ready in Magican Desktop on this computer.</span>
			{:else if androidObservationChecked}
				<span class="observation-status">Connect and keep a supported desktop online to continue.</span>
			{:else}
				<span class="observation-status">Checking desktop support…</span>
			{/if}
		</div>
		{#if androidObservationDesktop}
			<button class="primary" type="button" disabled={androidObservationBusy || !androidTrustOptions.some((option) => option.mode === selectedAndroidTrustMode && option.ready)} on:click={manageAndroidObservation}>
				{androidObservationBusy ? 'Opening…' : 'Set up or manage'}
			</button>
		{:else if localAndroidObservationLink}
			<button class="primary" type="button" disabled={androidObservationBusy || !androidTrustOptions.some((option) => option.mode === selectedAndroidTrustMode && option.ready)} on:click={manageAndroidObservation}>
				{androidObservationBusy ? 'Opening…' : 'Set up or manage'}
			</button>
		{:else}
			<button class="primary" type="button" disabled>Set up or manage</button>
		{/if}
	</div>

	{#if error}
		<div class="alert" role="alert">{error}</div>
	{/if}
	{#if !pairingAvailable}
		<div class="alert" role="alert">Device pairing storage is unavailable. Restore the server’s credential storage and restart Magician, then refresh this page.</div>
	{/if}

	{#if choosingClientKind}
		<div class="connection-choice" aria-labelledby="connection-choice-title">
			<div class="choice-heading">
				<div>
					<p class="step">Connect {choosingClientKind === 'ios' ? 'iPhone' : 'Android'}</p>
					<h3 id="connection-choice-title">Where will this phone use Magican?</h3>
				</div>
				<button class="secondary" type="button" on:click={() => (choosingClientKind = null)}>Cancel</button>
			</div>
			<div class="choice-grid">
				<button
					class="route-card"
					type="button"
					disabled={creating || !connectionOptions.same_wifi}
					on:click={() => choosingClientKind && createEnrollment(choosingClientKind, 'same_wifi')}
				>
					<strong>Same Wi-Fi · this computer</strong>
					<span>Choose this when the phone and this computer are on the same trusted Wi-Fi. It is direct and stays on your local network.</span>
					<code>{connectionOptions.same_wifi ?? 'Unavailable on the current Magician listener'}</code>
				</button>
				<button
					class="route-card"
					type="button"
					disabled={creating || !connectionOptions.remote}
					on:click={() => choosingClientKind && createEnrollment(choosingClientKind, 'remote')}
				>
					<strong>Remote · works anywhere</strong>
					<span>Choose this on a different network or when the computer is remote. Traffic uses the secured connect.magican.ai route.</span>
					<code>{connectionOptions.remote ?? 'connect.magican.ai is not configured'}</code>
				</button>
			</div>
			<p class="localhost-note">“localhost” on a phone means the phone itself, so local pairing uses this computer’s private network address.</p>
		</div>
	{/if}

	{#if enrollment}
		<div class:desktop-enrollment={enrollment.client_kind === 'desktop'} class="enrollment" aria-live="polite">
			{#if enrollment.client_kind === 'desktop'}
				<div class:expired class="desktop-link-mark" aria-hidden="true">↗</div>
			{:else}
				<div class:expired class="qr" aria-label={`${enrollment.client_kind === 'ios' ? 'iPhone' : 'Android'} connection QR code`}>
					{@html enrollment.qr_svg}
				</div>
			{/if}
			<div class="instructions">
				<p class="step">On {enrollment.client_kind === 'desktop' ? 'this computer' : enrollment.client_kind === 'ios' ? 'iPhone' : 'Android'}</p>
				{#if enrollment.client_kind === 'desktop'}
					<h3>Authorize the installed Magican Desktop</h3>
					<p>Open this one-time link. Magican Desktop verifies this server and stores its Desktop Edge credential in the operating system credential store.</p>
				{:else}
					<h3>Open Magican and scan this connection QR</h3>
					<p>Confirm the displayed Magician address. The app exchanges this one-time code and stores its device credential securely.</p>
				{/if}
				<div class="route-summary">
					<strong>{enrollment.connection_mode === 'same_wifi' ? 'Same Wi-Fi' : 'Remote'}</strong>
					<code>{enrollment.origin}</code>
				</div>
				<div class="scope"><span>Scope</span><strong>{enrollment.principal} / {enrollment.workspace}</strong></div>
				<div class:expired class="expiry">
					{expired ? 'Code expired' : `Expires in ${countdown(secondsRemaining)}`}
				</div>
				<div class="actions">
					{#if enrollment.client_kind === 'desktop' && !expired}
						<a class="primary link-button" href={enrollment.enrollment_uri}>Open Magican Desktop</a>
						<button class="secondary" type="button" on:click={copyEnrollmentLink}>Copy one-time link</button>
					{/if}
					{#if expired}
					<button class="primary" type="button" disabled={creating || !pairingAvailable} on:click={() => enrollment && createEnrollment(enrollment.client_kind, enrollment.connection_mode)}>Create a new code</button>
					{/if}
					<button class="secondary" type="button" on:click={closeEnrollment}>Close</button>
				</div>
			</div>
		</div>
	{/if}

	{#if loading && devices.length === 0}
		<div class="empty">Loading paired devices…</div>
	{:else if devices.length === 0 && pairingAvailable}
		<div class="empty">No mobile device is connected yet.</div>
	{:else}
		<div class="device-list">
			{#each devices as device (device.device_id)}
				<div class="device-card">
					<div class="device-row">
						<div class="device-dot" class:online={!!device.last_seen_ms && Date.now() - device.last_seen_ms < 120_000}></div>
						<div class="device-copy">
							<strong>{device.label || deviceKindLabel(device.client_kind)}</strong>
							<span title={device.device_id}>{device.device_id}</span>
						</div>
						<div class="last-seen">{relativeTime(device.last_seen_ms ?? device.paired_at_ms)}</div>
						<button class="danger" type="button" on:click={() => revoke(device)}>Unpair</button>
					</div>
					{#if device.client_kind === 'android'}
						<label class="device-purpose" title="Let this phone read a one-time login code from its notifications while a verification request is open, and answer that request itself. Protected apps stay excluded; the code never reaches the agent.">
							<input
								type="checkbox"
								checked={verificationCodeDevices.includes(device.device_id)}
								disabled={verificationBusy === device.device_id}
								aria-label={`Use ${device.label || device.device_id} for verification codes`}
								on:change={() => void toggleVerificationCodes(device)}
							/>
							<span>Use notifications for verification codes</span>
						</label>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
</section>

<style>
	.device-purpose { align-items: center; color: var(--text-secondary); display: flex; font-size: .78rem; gap: .45rem; margin: .45rem 0 0 1.35rem; }
	.device-panel { background: var(--bg-card); border: 1px solid var(--border-soft); border-radius: 8px; color: var(--text-primary); padding: 1rem; }
	.panel-header { align-items: flex-start; display: flex; gap: 1rem; justify-content: space-between; }
	.overline { color: var(--text-secondary); font-size: .75rem; font-weight: 700; margin: 0 0 .25rem; text-transform: uppercase; }
	h2, h3, p { margin-top: 0; }
	h2 { color: var(--text-primary); font-size: 1.05rem; font-weight: 600; margin-bottom: .35rem; }
	h3 { font-size: 1rem; margin-bottom: .4rem; }
	.panel-header p:not(.overline), .instructions p { color: var(--text-secondary); font-size: .82rem; margin-bottom: 0; }
	.actions { display: flex; flex-wrap: wrap; gap: .5rem; }
	button { border-radius: 6px; cursor: pointer; font: inherit; font-size: .86rem; font-weight: 600; min-height: 2.25rem; padding: .52rem .78rem; }
	button:disabled { cursor: wait; opacity: .6; }
	.primary { background: var(--accent-primary); border: 1px solid var(--accent-primary); color: var(--text-on-accent, var(--accent-on-primary, #fff)); }
	.secondary { background: var(--button-secondary-bg, var(--bg-soft)); border: 1px solid var(--button-secondary-border, var(--border-soft)); color: var(--button-secondary-color, var(--text-primary)); }
	.danger { background: transparent; border: 1px solid color-mix(in srgb, var(--color-error) 35%, var(--border-soft)); color: var(--color-error); }
	.alert { background: color-mix(in srgb, var(--color-error) 10%, var(--bg-card)); border: 1px solid color-mix(in srgb, var(--color-error) 30%, var(--border-soft)); border-radius: 6px; color: var(--text-primary); margin-top: .85rem; padding: .7rem; }
	.connection-choice { background: var(--bg-soft); border: 1px solid var(--border-soft); border-radius: 10px; margin-top: 1rem; padding: 1rem; }
	.choice-heading { align-items: flex-start; display: flex; gap: 1rem; justify-content: space-between; }
	.choice-grid { display: grid; gap: .75rem; grid-template-columns: repeat(2, minmax(0, 1fr)); margin-top: .9rem; }
	.route-card { align-items: flex-start; background: var(--bg-card); border: 1px solid var(--border-soft); color: var(--text-primary); display: flex; flex-direction: column; gap: .35rem; min-height: 8.5rem; padding: .85rem; text-align: left; }
	.route-card:hover:not(:disabled) { border-color: var(--accent-primary); }
	.route-card span, .localhost-note { color: var(--text-secondary); font-size: .76rem; font-weight: 400; line-height: 1.4; }
	.route-card code, .route-summary code { color: var(--text-muted); font-size: .7rem; overflow-wrap: anywhere; }
	.localhost-note { margin: .75rem 0 0; }
	.edge-status { background: var(--bg-soft); border: 1px solid var(--border-soft); border-radius: 7px; display: flex; flex-direction: column; font-size: .76rem; gap: .2rem; margin-top: .85rem; padding: .7rem; }
	.edge-status.online { border-color: color-mix(in srgb, var(--color-success) 45%, var(--border-soft)); }
	.edge-status span { color: var(--text-muted); }
	.edge-status .edge-error { color: var(--color-error); }
	.observation-card { align-items: flex-start; background: var(--bg-soft); border: 1px solid var(--border-soft); border-radius: 8px; display: flex; gap: 1rem; justify-content: space-between; margin-top: .85rem; padding: .85rem; }
	.observation-card > div { flex: 1; min-width: 0; }
	.observation-card h3 { margin-bottom: .25rem; }
	.observation-card p:not(.step) { color: var(--text-secondary); font-size: .78rem; margin-bottom: .35rem; }
	.observation-status { color: var(--text-muted); display: block; font-size: .72rem; }
	.observation-error { color: var(--color-error); display: block; font-size: .75rem; margin-bottom: .3rem; }
	.trust-options { display: grid; gap: .45rem; margin: .7rem 0; }
	.trust-options label { align-items: flex-start; background: var(--bg-card); border: 1px solid var(--border-soft); border-radius: 7px; display: flex; gap: .55rem; padding: .65rem; }
	.trust-options label.ready { cursor: pointer; }
	.trust-options label.selected { border-color: var(--accent-primary); box-shadow: 0 0 0 1px color-mix(in srgb, var(--accent-primary) 35%, transparent); }
	.trust-options input { margin-top: .18rem; }
	.trust-options span { display: flex; flex-direction: column; gap: .18rem; }
	.trust-options strong { font-size: .78rem; }
	.trust-options small { color: var(--text-secondary); font-size: .72rem; font-weight: 400; line-height: 1.35; }
	.trust-options .missing { color: var(--color-error); }
	.trust-help { color: var(--text-muted) !important; font-size: .7rem !important; }
	.enrollment { background: var(--bg-soft); border: 1px solid var(--border-soft); border-radius: 10px; display: grid; gap: 1.1rem; grid-template-columns: minmax(220px, 288px) 1fr; margin-top: 1rem; padding: 1rem; }
	.qr { align-items: center; background: white; border-radius: 8px; display: flex; justify-content: center; overflow: hidden; padding: .6rem; }
	.qr :global(svg) { display: block; height: auto; max-width: 100%; width: 100%; }
	.qr.expired { filter: grayscale(1); opacity: .4; }
	.desktop-link-mark { align-items: center; background: var(--accent-primary); border-radius: 999px; color: var(--text-on-accent, #fff); display: flex; font-size: 3rem; height: 7rem; justify-content: center; margin: auto; width: 7rem; }
	.desktop-link-mark.expired { filter: grayscale(1); opacity: .4; }
	.instructions { align-self: center; }
	.link-button { align-items: center; border-radius: 6px; display: inline-flex; font-size: .86rem; font-weight: 600; min-height: 2.25rem; padding: .52rem .78rem; text-decoration: none; }
	.step { color: var(--accent-primary) !important; font-size: .7rem !important; font-weight: 700; letter-spacing: .08em; text-transform: uppercase; }
	.scope { display: flex; flex-direction: column; font-size: .76rem; gap: .15rem; margin-top: .8rem; }
	.scope span { color: var(--text-muted); }
	.scope strong { overflow-wrap: anywhere; }
	.route-summary { display: flex; flex-direction: column; font-size: .76rem; gap: .15rem; margin-top: .8rem; }
	.expiry { color: var(--color-success); font-size: .78rem; font-weight: 700; margin: .75rem 0; }
	.expiry.expired { color: var(--color-error); }
	.empty { border: 1px dashed var(--border-soft); border-radius: 7px; color: var(--text-muted); margin-top: .9rem; padding: 1rem; text-align: center; }
	.device-list { border-top: 1px solid var(--border-soft); margin-top: 1rem; }
	.device-card { border-bottom: 1px solid var(--border-soft); padding: .75rem .25rem; }
	.device-row { align-items: center; display: grid; gap: .7rem; grid-template-columns: auto minmax(0, 1fr) auto auto; }
	.device-dot { background: var(--text-muted); border-radius: 999px; height: .52rem; width: .52rem; }
	.device-dot.online { background: var(--color-success); box-shadow: 0 0 0 3px color-mix(in srgb, var(--color-success) 14%, transparent); }
	.device-copy { display: flex; flex-direction: column; min-width: 0; }
	.device-copy strong { font-size: .84rem; }
	.device-copy span, .last-seen { color: var(--text-muted); font-size: .7rem; }
	.device-copy span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	@media (max-width: 700px) {
		.panel-header { flex-direction: column; }
		.observation-card { align-items: stretch; flex-direction: column; }
		.enrollment { grid-template-columns: 1fr; }
		.choice-grid { grid-template-columns: 1fr; }
		.qr { margin: auto; max-width: 288px; }
		.device-row { grid-template-columns: auto minmax(0, 1fr) auto; }
		.last-seen { display: none; }
	}
</style>
