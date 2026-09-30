<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { AppSurfaceClientError } from './appSurfaceRuntime';
	import {
		bridgeEventSourceIsAdmitted,
		frameBridgeRequestFromEvent,
		postScriptedSurfaceBridge,
		postScriptedSurfaceReloadNote,
		SCRIPTED_SURFACE_FAILED_NOTICE,
		SCRIPTED_SURFACE_MAX_BRIDGE_MESSAGES,
		SCRIPTED_SURFACE_SANDBOX,
		ScriptedSurfaceBridgeSubmissionFifo,
		ScriptedSurfaceReloadBudget,
		surfaceFrameSourceIsAdmitted,
		type AppScriptedSurfaceHostPlan
	} from './appScriptedSurface';

	export let plan: AppScriptedSurfaceHostPlan;
	export let installationId: string;

	let frame: HTMLIFrameElement | null = null;
	let tornDownNotice = '';
	let lastSequence = 0;
	let bridgeMessages = 0;
	let bridgeHostAlive = true;
	const reloadBudget = new ScriptedSurfaceReloadBudget();
	const bridgeSubmissions = new ScriptedSurfaceBridgeSubmissionFifo();

	// The sandbox attribute is the kernel constant and is never widened:
	// `allow-scripts` only, never `allow-same-origin`, so the frame keeps
	// an opaque origin and cannot reach the host page, other surfaces, or
	// Tauri IPC.
	$: sandboxAdmitted = plan.sandbox === SCRIPTED_SURFACE_SANDBOX;
	$: sourceAdmitted = surfaceFrameSourceIsAdmitted(plan.entry_url);

	// Downloads, popups, top navigation, forms, modals, storage access and
	// pointer lock stay denied: the sandbox token list is exactly
	// allow-scripts, and the deny-list tokens are simply absent.
	function onWindowMessage(event: MessageEvent): void {
		if (tornDownNotice || !bridgeEventSourceIsAdmitted(event, frame, plan)) return;
		const request = frameBridgeRequestFromEvent(event, plan, lastSequence);
		if (!request || bridgeMessages >= SCRIPTED_SURFACE_MAX_BRIDGE_MESSAGES) {
			teardown(SCRIPTED_SURFACE_FAILED_NOTICE);
			return;
		}
		lastSequence = request.sequence;
		bridgeMessages += 1;
		// The kernel's sequence fence is server-side. Keep the HTTP leg FIFO:
		// two frame messages posted in one turn must not become racing fetches
		// whose arrival order differs from their admitted sequence. Handle the
		// result inside the queue item so one bounded operation error does not
		// reject and poison the queue tail. Replies remain keyed by the request
		// captured for this exact item.
		void bridgeSubmissions.enqueue(async () => {
			if (!bridgeHostAlive || tornDownNotice) return;
			try {
				const reply = await postScriptedSurfaceBridge(installationId, request);
				if (bridgeHostAlive && !tornDownNotice) postReply(request.request_id, null, reply);
			} catch (cause: unknown) {
				// A watchdog refusal tears the session down; a bounded op
				// error is relayed to the surface as its own error result.
				if (cause instanceof AppSurfaceClientError && (cause.status === 409 || cause.status === 410)) {
					teardown(SCRIPTED_SURFACE_FAILED_NOTICE);
					return;
				}
				if (bridgeHostAlive && !tornDownNotice) {
					postReply(request.request_id, cause instanceof Error ? cause.message : 'bridge_failed', null);
				}
			}
		});
	}

	function postReply(requestId: string, error: string | null, result: unknown): void {
		const target = frame?.contentWindow ?? null;
		target?.postMessage(
			{
				channel: 'magician-surface-bridge',
				kind: 'reply',
				request_id: requestId,
				...(error === null ? { result } : { error })
			},
			'*'
		);
	}

	function onFrameError(): void {
		noteLifecycleEvent();
	}

	function onFrameLoad(): void {
		// Any load after the initial entry document is a navigation or
		// reload attempt against a frame that may only load once.
		if (frame?.dataset.loaded === '1') {
			noteLifecycleEvent();
		} else if (frame) {
			frame.dataset.loaded = '1';
		}
	}

	function noteLifecycleEvent(): void {
		const verdict = reloadBudget.noteReload();
		// The kernel counts the same event server-side: the reload-note
		// route keeps the reload/crash budget authoritative when the local
		// count has not tripped yet, and its 409 (budget quarantined,
		// session gone — the server only ever answers 409 on this route;
		// the 410 branch below is defensive client parity) runs the exact
		// closed path a refused bridge message runs.
		postScriptedSurfaceReloadNote(installationId, plan.session_ref).catch(
			(cause: unknown) => {
				if (
					cause instanceof AppSurfaceClientError &&
					(cause.status === 409 || cause.status === 410)
				) {
					teardown(SCRIPTED_SURFACE_FAILED_NOTICE);
				}
			}
		);
		if (verdict.exceeded) {
			teardown(verdict.notice);
		}
	}

	function teardown(notice: string): void {
		tornDownNotice = notice;
		bridgeHostAlive = false;
		frame = null;
	}

	onMount(() => {
		window.addEventListener('message', onWindowMessage);
	});

	onDestroy(() => {
		bridgeHostAlive = false;
		window.removeEventListener('message', onWindowMessage);
	});
</script>

{#if !sandboxAdmitted || !sourceAdmitted}
	<div class="scripted-surface-notice" role="alert">
		{SCRIPTED_SURFACE_FAILED_NOTICE}
	</div>
{:else if tornDownNotice}
	<div class="scripted-surface-notice" role="alert">
		{tornDownNotice}
	</div>
{:else}
	<iframe
		class="scripted-surface-frame"
		title={`Custom surface ${plan.entry_route}`}
		sandbox={SCRIPTED_SURFACE_SANDBOX}
		referrerpolicy="no-referrer"
		loading="eager"
		src={plan.entry_url}
		bind:this={frame}
		on:error={onFrameError}
		on:load={onFrameLoad}
	></iframe>
{/if}

<style>
	.scripted-surface-frame {
		display: block;
		width: 100%;
		min-height: 28rem;
		border: 0;
		background: var(--bg-page, #fff);
	}

	.scripted-surface-notice {
		display: flex;
		align-items: center;
		justify-content: center;
		min-height: 8rem;
		border: 1px solid var(--border-color, #ddd);
		border-radius: 0.5rem;
		padding: 1rem;
		text-align: center;
		color: var(--text-muted, #666);
		background: var(--bg-page, #fff);
	}
</style>
