<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onDestroy, onMount } from "svelte";
  import { magicianFetch } from "./magicianAuth.js";
  import {
    canApproveMacosPairing,
    macosPairingPhaseLabel,
    readBoundedResponseText,
    validMacosBundleId,
  } from "./macosPairingUiModel.js";

  interface Props { magicianPort: number }
  interface Enrollment {
    desktop_identity_key_id: string;
    desktop_identity_public_key_hex: string;
    desktop_identity_fingerprint: string;
    owner_approval_code: string;
    expires_at_ms: number;
  }
  interface NativeTarget { targetRef: string; bundleId: string }
  interface NativeStatus {
    paired: boolean;
    ownerApprovalRequired: boolean;
    setupId: string | null;
    generation: number | null;
    expiresAtMs: number | null;
    scopeBindingRef: string | null;
    requestedTargets: NativeTarget[];
    reviewMaterialDigest: string | null;
    hostIdentityDigest: string | null;
    desktopIdentityDigest: string | null;
    generationFloor: number;
    resetEligible: boolean;
    activeActions: number;
  }
  interface RuntimeStatus {
    phase: string;
    active_generation: number | null;
    transition_generation: number | null;
    generation_high_water: number;
    finalization_ready: boolean;
  }
  interface ResetChallenge {
    schema: string;
    scope_binding_ref: string;
    reset_nonce: string;
    issued_at_ms: number;
    expires_at_ms: number;
  }

  const MAX_CONTROL_RESPONSE_BYTES = 16 * 1024;
  let { magicianPort }: Props = $props();
  let nativeStatus = $state<NativeStatus | null>(null);
  let runtimeStatus = $state<RuntimeStatus | null>(null);
  let enrollment = $state<Enrollment | null>(null);
  let bundleId = $state("");
  let cuaBinary = $state("");
  let fingerprintConfirmed = $state(false);
  let reviewConfirmedDigest = $state("");
  let resetChallenge = $state<ResetChallenge | null>(null);
  let resetConfirmed = $state(false);
  let busy = $state(false);
  let error = $state("");
  let message = $state("");
  let pollTimer: ReturnType<typeof setInterval> | null = null;

  function apiUrl(path: string): string {
    return `http://127.0.0.1:${magicianPort}/api/magician/v2/apps/macos-pairing${path}`;
  }

  async function runtimeJson<T>(path: string, method = "GET", body?: unknown): Promise<T> {
    const abort = new AbortController();
    const deadline = setTimeout(() => abort.abort(), 15_000);
    try {
      const response = await magicianFetch(apiUrl(path), {
        method,
        cache: "no-store",
        redirect: "error",
        headers: {
          "Accept": "application/json",
          "Content-Type": "application/json",
        },
        body: body === undefined ? undefined : JSON.stringify(body),
        signal: abort.signal,
      });
      const declared = Number(response.headers.get("content-length") ?? "0");
      if (declared > MAX_CONTROL_RESPONSE_BYTES) throw new Error("oversized response");
      const text = await readBoundedResponseText(response.body, MAX_CONTROL_RESPONSE_BYTES);
      const value = JSON.parse(text) as T & { message?: string };
      if (!response.ok) throw new Error(value.message ?? "pairing control failed");
      return value;
    } finally {
      clearTimeout(deadline);
    }
  }

  function runtimeRequest(path: string, method = "GET", body?: unknown): Promise<RuntimeStatus> {
    return runtimeJson<RuntimeStatus>(path, method, body);
  }

  async function refresh(): Promise<void> {
    try {
      const [native, runtime] = await Promise.allSettled([
        invoke<NativeStatus>("get_app_macos_host_pairing_status"),
        runtimeRequest(""),
      ]);
      if (native.status === "fulfilled"
        && native.value.reviewMaterialDigest !== nativeStatus?.reviewMaterialDigest) {
        reviewConfirmedDigest = "";
      }
      if (native.status === "fulfilled") nativeStatus = native.value;
      if (runtime.status === "fulfilled") runtimeStatus = runtime.value;
      else runtimeStatus = null;
      error = native.status === "rejected"
        ? `Native pairing owner unavailable: ${String(native.reason)}`
        : runtime.status === "rejected"
          ? `Runtime pairing store unavailable: ${String(runtime.reason)}`
          : "";
    } catch (cause) {
      error = `Pairing status unavailable: ${String(cause)}`;
    }
  }

  async function generateOwnerApproval(): Promise<void> {
    busy = true;
    error = "";
    message = "";
    try {
      enrollment = await invoke<Enrollment>("begin_app_macos_host_identity_approval");
      fingerprintConfirmed = false;
    } catch (cause) {
      error = `Could not create native approval: ${String(cause)}`;
    } finally {
      busy = false;
    }
  }

  async function beginSetup(): Promise<void> {
    if (!enrollment || !fingerprintConfirmed || !validMacosBundleId(bundleId)) {
      error = "Verify the displayed fingerprint and enter one exact bundle identifier.";
      return;
    }
    busy = true;
    error = "";
    const approval = enrollment;
    try {
      runtimeStatus = await runtimeRequest("/setup", "POST", {
        action_url: "http://127.0.0.1:3017/host/apps/macos/action",
        bundle_id: bundleId,
        desktop_identity_key_id: approval.desktop_identity_key_id,
        desktop_identity_public_key_hex: approval.desktop_identity_public_key_hex,
        desktop_identity_fingerprint: approval.desktop_identity_fingerprint,
        owner_approval_code: approval.owner_approval_code,
      });
      message = "Proposal submitted. Review the exact scope and targets below.";
    } catch (cause) {
      error = `Setup failed: ${String(cause)}`;
    } finally {
      // The live one-time authority is memory-only and discarded immediately
      // after the single bounded request, regardless of its outcome.
      approval.owner_approval_code = "";
      enrollment = null;
      fingerprintConfirmed = false;
      busy = false;
      await refresh();
    }
  }

  async function approveDisplayedProposal(): Promise<void> {
    const status = nativeStatus;
    if (!status || !canApproveMacosPairing(status, reviewConfirmedDigest)
      || !status.setupId || !status.generation || cuaBinary.length === 0 || cuaBinary.length > 1024) {
      error = "Approval must match the currently displayed scope, targets and executable.";
      return;
    }
    busy = true;
    error = "";
    try {
      await invoke("approve_app_macos_host_pairing", {
        setupId: status.setupId,
        expectedGeneration: status.generation,
        expectedReviewMaterialDigest: reviewConfirmedDigest,
        cuaDriverBinary: cuaBinary,
      });
      runtimeStatus = await runtimeRequest("/advance", "POST");
      message = "Native approval recorded and exact finalization requested.";
    } catch (cause) {
      error = `Approval/finalization is incomplete: ${String(cause)}`;
    } finally {
      busy = false;
      await refresh();
    }
  }

  async function advance(): Promise<void> {
    busy = true;
    error = "";
    try {
      runtimeStatus = await runtimeRequest("/advance", "POST");
      message = "Pairing recovery advanced.";
    } catch (cause) {
      error = `Recovery remains pending: ${String(cause)}`;
    } finally {
      busy = false;
      await refresh();
    }
  }

  async function revoke(): Promise<void> {
    const generation = runtimeStatus?.active_generation ?? runtimeStatus?.transition_generation;
    if (!generation) { error = "No exact pairing generation is available to revoke."; return; }
    busy = true;
    error = "";
    try {
      runtimeStatus = await runtimeRequest("/revoke", "POST", { expected_generation: generation });
      message = `Signed generation ${generation} revocation completed.`;
    } catch (cause) {
      error = `Signed revoke is uncertain; use Recover before retrying: ${String(cause)}`;
    } finally {
      busy = false;
      await refresh();
    }
  }

  async function emergencyNativeRevoke(): Promise<void> {
    busy = true;
    error = "";
    try {
      const floor = await invoke<number>("revoke_app_macos_host_pairing");
      message = `Native owner stopped and preserved anti-rollback floor ${floor}.`;
    } catch (cause) {
      error = `Native emergency revoke failed: ${String(cause)}`;
    } finally {
      busy = false;
      await refresh();
    }
  }

  async function beginOwnerReset(): Promise<void> {
    busy = true;
    error = "";
    try {
      resetChallenge = await runtimeJson<ResetChallenge>("/reset/challenge", "POST");
      resetConfirmed = false;
      message = "Review the exact reset scope, identity and anti-rollback floor below.";
    } catch (cause) {
      error = `Reset challenge unavailable: ${String(cause)}`;
    } finally {
      busy = false;
    }
  }

  async function completeOwnerReset(): Promise<void> {
    const native = nativeStatus;
    const challenge = resetChallenge;
    if (!native?.resetEligible || !native.hostIdentityDigest || !native.desktopIdentityDigest
      || !challenge || !resetConfirmed || native.generationFloor <= 0) {
      error = "Reset must match the displayed native identity and generation floor.";
      return;
    }
    busy = true;
    error = "";
    try {
      const acknowledgment = await invoke("reset_app_macos_host_pairing", {
        challenge,
        expectedHostIdentityDigest: native.hostIdentityDigest,
        expectedGenerationFloor: native.generationFloor,
        expectedDesktopIdentityDigest: native.desktopIdentityDigest,
      });
      runtimeStatus = await runtimeRequest("/reset", "POST", acknowledgment);
      message = `Both stores now retain floor ${native.generationFloor}; the next setup will use a higher generation.`;
      resetChallenge = null;
      resetConfirmed = false;
    } catch (cause) {
      error = `Reset delivery is incomplete; retry the exact retained challenge: ${String(cause)}`;
    } finally {
      busy = false;
      await refresh();
    }
  }

  onMount(() => {
    void refresh();
    pollTimer = setInterval(() => void refresh(), 5_000);
  });
  onDestroy(() => {
    if (pollTimer) clearInterval(pollTimer);
    if (enrollment) enrollment.owner_approval_code = "";
    enrollment = null;
  });
</script>

<section class="macos-pairing" aria-labelledby="macos-pairing-title">
  <div class="title-row">
    <div>
      <h2 id="macos-pairing-title">macOS App Observation</h2>
      <p>Owner-reviewed, read-only Accessibility snapshots for one exact app and workspace scope.</p>
    </div>
    <span class:active={runtimeStatus?.phase === "active"}>{macosPairingPhaseLabel(runtimeStatus?.phase)}</span>
  </div>

  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if message}<p class="message" role="status">{message}</p>{/if}

  {#if runtimeStatus?.phase === "active"}
    <dl>
      <dt>Generation</dt><dd>{runtimeStatus.active_generation}</dd>
      <dt>Native owner</dt><dd>{nativeStatus?.paired ? "paired" : "recovery required"}</dd>
      <dt>Active actions</dt><dd>{nativeStatus?.activeActions ?? 0}</dd>
    </dl>
    <div class="actions">
      <button type="button" onclick={advance} disabled={busy}>Verify / Recover</button>
      <button class="danger" type="button" onclick={revoke} disabled={busy}>Signed Revoke</button>
    </div>
  {:else}
    {#if nativeStatus?.paired && !runtimeStatus}
      <div class="proposal-review">
        <strong>Runtime state is unavailable while native authority is still live.</strong>
        <p>Stop the physical owner first. This preserves the desktop anti-rollback floor; it does not silently reset pairing.</p>
        <button class="danger" type="button" onclick={emergencyNativeRevoke} disabled={busy}>Native Emergency Revoke</button>
      </div>
    {/if}

    {#if nativeStatus?.resetEligible}
      <div class="proposal-review">
        <h3>Exceptional two-store recovery</h3>
        <p>Use only after runtime pairing state was lost. The desktop floor is preserved and transferred in a signed acknowledgment.</p>
        {#if !resetChallenge}
          <button type="button" onclick={beginOwnerReset} disabled={busy}>Begin Explicit Reset Recovery</button>
        {:else}
          <dl>
            <dt>Scope binding</dt><dd><code>{resetChallenge.scope_binding_ref}</code></dd>
            <dt>Reset nonce</dt><dd><code>{resetChallenge.reset_nonce}</code></dd>
            <dt>Desktop identity</dt><dd><code>{nativeStatus.desktopIdentityDigest}</code></dd>
            <dt>Host identity</dt><dd><code>{nativeStatus.hostIdentityDigest}</code></dd>
            <dt>Preserved floor</dt><dd>{nativeStatus.generationFloor}</dd>
          </dl>
          <label class="confirm"><input type="checkbox" bind:checked={resetConfirmed} /> I explicitly approve resetting both stores for this exact scope while preserving this generation floor.</label>
          <button class="danger" type="button" onclick={completeOwnerReset} disabled={busy || !resetConfirmed}>Sign &amp; Complete Two-Store Reset</button>
        {/if}
      </div>
    {/if}

    <div class="setup-grid">
      <label>Exact app bundle identifier
        <input bind:value={bundleId} maxlength="255" placeholder="com.example.Editor" spellcheck="false" />
      </label>
      <button type="button" onclick={generateOwnerApproval} disabled={busy}>Generate Native Approval</button>
    </div>

    {#if enrollment}
      <div class="identity-review">
        <strong>Desktop identity fingerprint</strong>
        <code>{enrollment.desktop_identity_fingerprint}</code>
        <strong>One-time owner code</strong>
        <code>{enrollment.owner_approval_code}</code>
        <label class="confirm"><input type="checkbox" bind:checked={fingerprintConfirmed} /> I verified this fingerprint and code in this native Settings window.</label>
        <button type="button" onclick={beginSetup} disabled={busy || !fingerprintConfirmed}>Submit Owner-Mediated Setup</button>
      </div>
    {/if}

    {#if nativeStatus?.ownerApprovalRequired && nativeStatus.reviewMaterialDigest}
      <div class="proposal-review">
        <h3>Exact pending authority</h3>
        <dl>
          <dt>Setup</dt><dd><code>{nativeStatus.setupId}</code></dd>
          <dt>Generation</dt><dd>{nativeStatus.generation}</dd>
          <dt>Scope binding</dt><dd><code>{nativeStatus.scopeBindingRef}</code></dd>
        </dl>
        <ul>
          {#each nativeStatus.requestedTargets as target}
            <li><code>{target.targetRef}</code> → <strong>{target.bundleId}</strong></li>
          {/each}
        </ul>
        <label>Reviewed staged CUA source executable
          <input bind:value={cuaBinary} maxlength="1024" placeholder="/Applications/CuaDriver.app/Contents/MacOS/cua-driver" spellcheck="false" />
        </label>
        <label class="confirm"><input type="checkbox" checked={reviewConfirmedDigest === nativeStatus.reviewMaterialDigest} onchange={(event) => reviewConfirmedDigest = event.currentTarget.checked ? nativeStatus?.reviewMaterialDigest ?? "" : ""} /> I approve exactly this setup, generation, scope, target list, and executable.</label>
        <button type="button" onclick={approveDisplayedProposal} disabled={busy || reviewConfirmedDigest !== nativeStatus.reviewMaterialDigest}>Approve &amp; Finalize</button>
      </div>
    {:else if runtimeStatus && runtimeStatus.phase !== "unpaired" && runtimeStatus.phase !== "revoked"}
      <button type="button" onclick={advance} disabled={busy}>Recover Pending Finalization</button>
    {/if}
  {/if}
</section>

<style>
  /* Semantic tokens only: the tray Settings window renders under the synced
     unified-ui theme, and a hardcoded dark pill inherits dark text in a light
     theme (dark-on-dark). */
  .macos-pairing { background: var(--bg-secondary); border: 1px solid var(--border); border-radius: 12px; padding: 18px; }
  .title-row { display: flex; justify-content: space-between; gap: 16px; align-items: flex-start; }
  h2, h3, p { margin-top: 0; }
  .title-row span { border-radius: 999px; padding: 4px 9px; white-space: nowrap; color: var(--text-muted); background: color-mix(in srgb, var(--text-muted) 14%, transparent); border: 1px solid var(--border); }
  .title-row span.active { color: var(--success); background: color-mix(in srgb, var(--success) 12%, transparent); border-color: color-mix(in srgb, var(--success) 42%, var(--border)); }
  .setup-grid, .actions { display: flex; gap: 10px; align-items: end; flex-wrap: wrap; }
  .identity-review, .proposal-review { display: grid; gap: 10px; margin-top: 14px; padding: 14px; border: 1px solid var(--border); border-radius: 8px; }
  label { display: grid; gap: 5px; }
  label.confirm { display: flex; align-items: flex-start; gap: 8px; }
  input:not([type]) { min-width: 280px; }
  code { overflow-wrap: anywhere; }
  dl { display: grid; grid-template-columns: max-content 1fr; gap: 6px 12px; }
  dt { color: var(--text-muted); }
  dd { margin: 0; overflow-wrap: anywhere; }
  .error { color: var(--danger); }
  .message { color: var(--success); }
  button.danger { color: var(--danger); }
</style>
