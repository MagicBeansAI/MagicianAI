<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onDestroy, onMount } from "svelte";

  type OwnerOperation =
    | "enroll_attested_device"
    | "approve_actions"
    | "revoke_actions";

  type OwnerAction =
    | "snapshot"
    | "screenshot"
    | "launch"
    | "close"
    | "tap"
    | "type"
    | "key"
    | "scroll";

  interface Proposal {
    schema: string;
    proposal_id: string;
    owner_generation: number;
    previous_receipt_digest: string | null;
    desktop_identity_digest: string;
    principal: string;
    workspace: string;
    operation: OwnerOperation;
    enrollment_id: string | null;
    target_ref: string;
    device_label: string;
    key_id: string;
    automation_identity_digest: string;
    owner_app_package: string;
    app_version_code: number;
    app_signing_sha256: string;
    apk_sha256: string;
    attestation_root_sha256: string;
    attestation_security_level: "tee" | "strongbox";
    attestation_policy_digest: string;
    expected_review_generation: number;
    resulting_review_generation: number;
    actions: OwnerAction[];
    allowed_packages: string[];
    issued_at_ms: number;
    expires_at_ms: number;
    display_digest: string;
  }

  interface RecoveryChallenge {
    schema: string;
    recovery_nonce: string;
    desktop_identity_digest: string;
    known_owner_generation: number;
    known_latest_receipt_digest: string | null;
    issued_at_ms: number;
    expires_at_ms: number;
    display_digest: string;
  }

  interface NativeStatus {
    desktopIdentityKeyId: string;
    desktopIdentityDigest: string;
    ownerGeneration: number;
    latestReceiptDigest: string | null;
    identityBootstrapAvailable: boolean;
    pending: Proposal | null;
    pendingRecovery: RecoveryChallenge | null;
    pendingIdentityBootstrap: {
      bootstrap: BootstrapEnrollment["bootstrap"];
      desktopIdentityFingerprint: string;
      completionReady: boolean;
    } | null;
    pendingRebindOffer: RebindOffer | null;
  }

  interface RebindOffer {
    schema: string;
    bootstrap: BootstrapEnrollment["bootstrap"];
    desktop_identity_key_id: string;
    desktop_identity_digest: string;
    previous_bootstrap_status_digest: string;
    owner_generation: number;
    latest_receipt_digest: string | null;
    issued_at_ms: number;
    expires_at_ms: number;
    display_digest: string;
  }

  interface Enrollment {
    enrollment_id: string;
    enrollment_uri: string;
    qr_svg: string;
    expires_at_ms: number;
    challenge_base64: string;
    principal: string;
    workspace: string;
    trust_mode: "play_integrity" | "owner_pinned_private_build";
    connection_mode: "same_wifi" | "remote";
  }

  interface Props {
    trustMode: "play_integrity" | "owner_pinned_private_build" | string;
  }

  let { trustMode }: Props = $props();

  interface BootstrapEnrollment {
    bootstrap: {
      schema: string;
      bootstrap_nonce: string;
      issued_at_ms: number;
      expires_at_ms: number;
    };
    desktopIdentityKeyId: string;
    desktopIdentityFingerprint: string;
    ownerApprovalCode: string;
    expiresAtMs: number;
    rebindDisplayDigest: string | null;
  }

  interface Target {
    principal: string;
    workspace: string;
    target_ref: string;
    device_label: string;
    automation_identity_digest: string;
    owner_app_package: string;
    app_version_code: number;
    app_signing_sha256: string;
    apk_sha256: string;
    attestation_root_sha256: string;
    attestation_security_level: "tee" | "strongbox";
    attestation_policy_digest: string;
    paired_at_ms: number;
    last_seen_ms: number | null;
    review_generation: number;
    reviewed_actions: OwnerAction[];
    reviewed_packages: string[];
  }

  let status = $state<NativeStatus | null>(null);
  let bootstrapEnrollment = $state<BootstrapEnrollment | null>(null);
  let bootstrapConfirmedFingerprint = $state("");
  let confirmedRebindDigest = $state("");
  let targets = $state<Target[]>([]);
  let enrollment = $state<Enrollment | null>(null);
  let enrollmentQrUrl = $state("");
  let selectedTargetRef = $state("");
  let operation = $state<OwnerOperation>("approve_actions");
  let packageText = $state("");
  let connectionMode = $state<"same_wifi" | "remote">("same_wifi");
  let confirmedProposalDigest = $state("");
  let confirmedRecoveryDigest = $state("");
  let busy = $state(false);
  let ownerReady = $state(false);
  let polling = false;
  let pollTimer: ReturnType<typeof setInterval> | null = null;
  let error = $state("");
  let message = $state("");
  let activeTarget = $derived(
    targets.find((target) => target.target_ref === selectedTargetRef),
  );

  function clearQr(): void {
    if (enrollmentQrUrl) URL.revokeObjectURL(enrollmentQrUrl);
    enrollmentQrUrl = "";
  }

  function setEnrollment(value: Enrollment | null): void {
    clearQr();
    enrollment = value;
    if (value) {
      enrollmentQrUrl = URL.createObjectURL(
        new Blob([value.qr_svg], { type: "image/svg+xml" }),
      );
    }
  }

  function packagesForRequest(): string[] {
    return packageText
      .split(/[\n,]/)
      .map((value) => value.trim())
      .filter((value, index, values) => value.length > 0 && values.indexOf(value) === index)
      .sort();
  }

  function adoptStatus(next: NativeStatus): void {
    if (next.pending?.display_digest !== status?.pending?.display_digest) {
      confirmedProposalDigest = "";
    }
    if (next.pendingRecovery?.display_digest !== status?.pendingRecovery?.display_digest) {
      confirmedRecoveryDigest = "";
    }
    if (next.pendingRebindOffer?.display_digest !== status?.pendingRebindOffer?.display_digest) {
      confirmedRebindDigest = "";
    }
    status = next;
  }

  async function refreshLocal(): Promise<void> {
    adoptStatus(await invoke<NativeStatus>("get_app_android_authority_status"));
  }

  async function refreshPending(): Promise<void> {
    busy = true;
    error = "";
    try {
      adoptStatus(await invoke<NativeStatus>("refresh_app_android_authority_pending"));
      ownerReady = true;
    } catch (cause) {
      error = `Pending owner decisions are unavailable: ${String(cause)}`;
    } finally {
      busy = false;
    }
  }

  async function pollOwner(): Promise<void> {
    if (polling || busy) return;
    polling = true;
    try {
      if (bootstrapEnrollment && bootstrapEnrollment.expiresAtMs <= Date.now()) {
        bootstrapEnrollment.ownerApprovalCode = "";
        bootstrapEnrollment = null;
        bootstrapConfirmedFingerprint = "";
        message = "The one-time owner code expired. Waiting for a fresh verified runtime code.";
      }
      await refreshLocal();
      if (!status?.identityBootstrapAvailable && !status?.pendingIdentityBootstrap) {
        try {
          adoptStatus(await invoke<NativeStatus>("refresh_app_android_authority_pending"));
          ownerReady = true;
        } catch {
          ownerReady = false;
        }
      }
    } catch {
      ownerReady = false;
    } finally {
      polling = false;
    }
  }

  async function refreshTargets(): Promise<void> {
    busy = true;
    error = "";
    try {
      const response = await invoke<{ targets: Target[] }>("list_app_android_authority_targets");
      targets = response.targets;
      if (!targets.some((target) => target.target_ref === selectedTargetRef)) {
        selectedTargetRef = targets[0]?.target_ref ?? "";
      }
    } catch (cause) {
      error = `Reviewed Android targets are unavailable: ${String(cause)}`;
    } finally {
      busy = false;
    }
  }

  async function beginEnrollment(): Promise<void> {
    if (!ownerReady) {
      error = "Pin the desktop owner identity before beginning Android enrollment.";
      return;
    }
    busy = true;
    error = "";
    message = "";
    try {
      setEnrollment(await invoke<Enrollment>("begin_app_android_apps_enrollment", {
        trustMode,
        connectionMode,
      }));
      message = "Scan this one-time enrollment only with the reviewed Magdroid app.";
    } catch (cause) {
      error = `Android enrollment could not begin: ${String(cause)}`;
    } finally {
      busy = false;
    }
  }

  async function beginIdentityBootstrap(): Promise<void> {
    const rebind = status?.pendingRebindOffer;
    if (rebind && confirmedRebindDigest !== rebind.display_digest) {
      error = "Rebind confirmation must match the complete displayed durable-head digest.";
      return;
    }
    busy = true;
    error = "";
    message = "";
    try {
      bootstrapEnrollment = await invoke<BootstrapEnrollment>(
        "begin_app_android_authority_identity_bootstrap",
        { expectedRebindDisplayDigest: rebind?.display_digest ?? null },
      );
      bootstrapConfirmedFingerprint = "";
      message = "Verify this Keychain identity and one-time code before pinning it to Android Apps.";
    } catch (cause) {
      error = `Android owner identity bootstrap could not begin: ${String(cause)}`;
    } finally {
      busy = false;
    }
  }

  async function completeIdentityBootstrap(): Promise<void> {
    const enrollment = bootstrapEnrollment;
    if (enrollment && enrollment.expiresAtMs <= Date.now()) {
      enrollment.ownerApprovalCode = "";
      bootstrapEnrollment = null;
      bootstrapConfirmedFingerprint = "";
      await refreshLocal();
      error = "The one-time owner code expired. Create a fresh code and confirm that identity instead.";
      return;
    }
    const retained = status?.pendingIdentityBootstrap;
    const nonce = enrollment?.bootstrap.bootstrap_nonce ?? retained?.bootstrap.bootstrap_nonce;
    const fingerprint = enrollment?.desktopIdentityFingerprint
      ?? retained?.desktopIdentityFingerprint;
    const rebindDisplayDigest = enrollment?.rebindDisplayDigest
      ?? status?.pendingRebindOffer?.display_digest
      ?? null;
    if (!nonce || !fingerprint
      || (enrollment && bootstrapConfirmedFingerprint !== fingerprint)
      || (!enrollment && !retained?.completionReady)) {
      error = "Bootstrap confirmation must match the displayed Keychain fingerprint.";
      return;
    }
    busy = true;
    error = "";
    try {
      const next = await invoke<NativeStatus>("complete_app_android_authority_identity_bootstrap", {
        expectedBootstrapNonce: nonce,
        expectedDesktopIdentityFingerprint: fingerprint,
        expectedRebindDisplayDigest: rebindDisplayDigest,
      });
      adoptStatus(next);
      ownerReady = false;
      if (enrollment) enrollment.ownerApprovalCode = "";
      bootstrapEnrollment = null;
      bootstrapConfirmedFingerprint = "";
      message = "The exact owner confirmation is armed for the verified runtime socket; refresh until finalization clears it.";
    } catch (cause) {
      error = `Desktop identity bootstrap is incomplete; retry the exact retained approval: ${String(cause)}`;
    } finally {
      busy = false;
    }
  }

  async function cancelEnrollment(): Promise<void> {
    if (!enrollment) return;
    busy = true;
    error = "";
    try {
      await invoke("cancel_app_android_apps_enrollment", {
        enrollmentId: enrollment.enrollment_id,
      });
      setEnrollment(null);
      message = "The one-time enrollment was cancelled.";
    } catch (cause) {
      error = `Android enrollment cancellation failed: ${String(cause)}`;
    } finally {
      busy = false;
    }
  }

  async function proposeReview(): Promise<void> {
    const target = activeTarget;
    if (!target) {
      error = "Select one exact attested Android target.";
      return;
    }
    const allowedPackages = operation === "approve_actions"
      ? packagesForRequest()
      : target.reviewed_packages.slice().sort();
    if (operation === "approve_actions" && allowedPackages.length === 0) {
      error = "Android action approval requires at least one exact package.";
      return;
    }
    if (allowedPackages.includes(target.owner_app_package)
      || allowedPackages.includes("ai.magicbeans.magdroid")) {
      error = "The attested Magdroid owner app cannot be an observation target.";
      return;
    }
    busy = true;
    error = "";
    message = "";
    try {
      adoptStatus(await invoke<NativeStatus>("propose_app_android_authority_review", {
        body: {
          target_ref: target.target_ref,
          expected_review_generation: target.review_generation,
          operation,
          allowed_packages: allowedPackages,
        },
      }));
      message = "Review every field below before signing this exact transition.";
    } catch (cause) {
      error = `Android owner proposal was rejected: ${String(cause)}`;
    } finally {
      busy = false;
    }
  }

  async function confirmProposal(): Promise<void> {
    const proposal = status?.pending;
    if (!proposal || confirmedProposalDigest !== proposal.display_digest) {
      error = "Confirmation must match the complete displayed proposal digest.";
      return;
    }
    busy = true;
    error = "";
    try {
      const enrollingDevice = proposal.operation === "enroll_attested_device";
      await invoke("confirm_app_android_authority_proposal", {
        proposalId: proposal.proposal_id,
        expectedOwnerGeneration: proposal.owner_generation,
        expectedDisplayDigest: proposal.display_digest,
      });
      confirmedProposalDigest = "";
      if (enrollingDevice) setEnrollment(null);
      message = enrollingDevice
        ? "The signed enrollment receipt was accepted; the runtime published the attested device only after that acceptance."
        : "The signed owner transition was durably recorded and submitted.";
      await Promise.all([refreshLocal(), refreshTargets()]);
    } catch (cause) {
      error = `Owner transition delivery is incomplete; retry the same digest: ${String(cause)}`;
    } finally {
      busy = false;
    }
  }

  async function beginRecovery(): Promise<void> {
    busy = true;
    error = "";
    message = "";
    try {
      adoptStatus(await invoke<NativeStatus>("begin_app_android_authority_recovery"));
      message = "Recovery will disclose signed target records only after exact confirmation.";
    } catch (cause) {
      error = `Android owner recovery could not begin: ${String(cause)}`;
    } finally {
      busy = false;
    }
  }

  async function confirmRecovery(): Promise<void> {
    const recovery = status?.pendingRecovery;
    if (!recovery || confirmedRecoveryDigest !== recovery.display_digest) {
      error = "Recovery confirmation must match the complete displayed digest.";
      return;
    }
    busy = true;
    error = "";
    try {
      await invoke("confirm_app_android_authority_recovery", {
        recoveryNonce: recovery.recovery_nonce,
        expectedDisplayDigest: recovery.display_digest,
      });
      confirmedRecoveryDigest = "";
      message = "The exact signed recovery snapshot was accepted.";
      await Promise.all([refreshLocal(), refreshTargets()]);
    } catch (cause) {
      error = `Recovery delivery is incomplete; retry the retained exact snapshot: ${String(cause)}`;
    } finally {
      busy = false;
    }
  }

  onMount(() => {
    void pollOwner().catch((cause) => {
      error = `Native Android owner is unavailable: ${String(cause)}`;
    });
    pollTimer = setInterval(() => void pollOwner(), 2000);
  });
  onDestroy(() => {
    if (pollTimer) clearInterval(pollTimer);
    clearQr();
    if (bootstrapEnrollment) bootstrapEnrollment.ownerApprovalCode = "";
    bootstrapEnrollment = null;
  });
</script>

<section class="android-owner" aria-labelledby="android-owner-title">
  <div class="title-row">
    <div>
      <h2 id="android-owner-title">Secure Android observation approval</h2>
      <p>Web Settings requested this trusted desktop confirmation. Follow the one current step; signing keys remain in Keychain.</p>
    </div>
    <span class:active={ownerReady}>{ownerReady ? "Ready" : status ? "Setup required" : "Unavailable"}</span>
  </div>

  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if message}<p class="message" role="status">{message}</p>{/if}

  {#if status}
    <details class="technical-details">
      <summary>Technical identity details</summary>
      <dl>
        <dt>Desktop key</dt><dd><code>{status.desktopIdentityKeyId}</code></dd>
        <dt>Desktop identity</dt><dd><code>{status.desktopIdentityDigest}</code></dd>
        <dt>Owner generation</dt><dd>{status.ownerGeneration}</dd>
        <dt>Latest receipt</dt><dd><code>{status.latestReceiptDigest ?? "none"}</code></dd>
        <dt>Runtime bootstrap</dt><dd>{ownerReady ? "verified" : status.identityBootstrapAvailable ? "ready for approval" : "waiting"}</dd>
      </dl>
    </details>
  {/if}

  {#if !ownerReady || bootstrapEnrollment || status?.pendingIdentityBootstrap || status?.pendingRebindOffer}
  <div class="review-card">
    <h3>1. Verify this desktop</h3>
    <p>This one-time step pins the desktop signing identity to the selected Magician server.</p>
    {#if status?.pendingRebindOffer}
      {@const rebind = status.pendingRebindOffer}
      <div class="critical rebind-review">
        <h4>Recover a runtime that lost its Android authority store</h4>
        <p>This preserves the same Keychain identity and the complete desktop owner high-water. It does not clear or lower any prior approval.</p>
        <dl>
          <dt>Schema</dt><dd><code>{rebind.schema}</code></dd>
          <dt>New runtime nonce</dt><dd><code>{rebind.bootstrap.bootstrap_nonce}</code></dd>
          <dt>Desktop key</dt><dd><code>{rebind.desktop_identity_key_id}</code></dd>
          <dt>Desktop identity</dt><dd><code>{rebind.desktop_identity_digest}</code></dd>
          <dt>Previous bootstrap</dt><dd><code>{rebind.previous_bootstrap_status_digest}</code></dd>
          <dt>Preserved owner generation</dt><dd>{rebind.owner_generation}</dd>
          <dt>Preserved latest receipt</dt><dd><code>{rebind.latest_receipt_digest ?? "none"}</code></dd>
          <dt>Issued</dt><dd>{new Date(rebind.issued_at_ms).toLocaleString()}</dd>
          <dt>Expires</dt><dd>{new Date(rebind.expires_at_ms).toLocaleString()}</dd>
          <dt>Display digest</dt><dd><code>{rebind.display_digest}</code></dd>
        </dl>
        <label class="confirm"><input type="checkbox" checked={confirmedRebindDigest === rebind.display_digest} onchange={(event) => confirmedRebindDigest = event.currentTarget.checked ? rebind.display_digest : ""} /> I authorize only this exact runtime rebind while preserving the displayed desktop identity and owner high-water.</label>
      </div>
    {/if}
    {#if !bootstrapEnrollment && status?.pendingIdentityBootstrap?.completionReady}
      <dl>
        <dt>Retained nonce</dt><dd><code>{status.pendingIdentityBootstrap.bootstrap.bootstrap_nonce}</code></dd>
        <dt>Retained fingerprint</dt><dd><code>{status.pendingIdentityBootstrap.desktopIdentityFingerprint}</code></dd>
      </dl>
      <p>The approved signed completion was durably retained after an interrupted response. Retrying sends byte-identical public proof.</p>
      <button type="button" onclick={completeIdentityBootstrap} disabled={busy}>Retry Retained Bootstrap Completion</button>
    {:else if !bootstrapEnrollment}
      <button type="button" onclick={beginIdentityBootstrap} disabled={busy || !status?.identityBootstrapAvailable || (!!status?.pendingRebindOffer && confirmedRebindDigest !== status.pendingRebindOffer.display_digest)}>{status?.pendingRebindOffer ? "Begin Exact Runtime Rebind" : "Begin Owner Identity Bootstrap"}</button>
    {:else}
      <dl>
        <dt>Schema</dt><dd><code>{bootstrapEnrollment.bootstrap.schema}</code></dd>
        <dt>Bootstrap nonce</dt><dd><code>{bootstrapEnrollment.bootstrap.bootstrap_nonce}</code></dd>
        <dt>Desktop key</dt><dd><code>{bootstrapEnrollment.desktopIdentityKeyId}</code></dd>
        <dt>Fingerprint</dt><dd><code>{bootstrapEnrollment.desktopIdentityFingerprint}</code></dd>
        <dt>One-time owner code</dt><dd><code>{bootstrapEnrollment.ownerApprovalCode}</code></dd>
        <dt>Issued</dt><dd>{new Date(bootstrapEnrollment.bootstrap.issued_at_ms).toLocaleString()}</dd>
        <dt>Expires</dt><dd>{new Date(bootstrapEnrollment.expiresAtMs).toLocaleString()}</dd>
        {#if bootstrapEnrollment.rebindDisplayDigest}
          <dt>Rebind display digest</dt><dd><code>{bootstrapEnrollment.rebindDisplayDigest}</code></dd>
        {/if}
      </dl>
      <label class="confirm"><input type="checkbox" checked={bootstrapConfirmedFingerprint === bootstrapEnrollment.desktopIdentityFingerprint} onchange={(event) => bootstrapConfirmedFingerprint = event.currentTarget.checked ? bootstrapEnrollment?.desktopIdentityFingerprint ?? "" : ""} /> I verified this exact Keychain fingerprint, bootstrap nonce, one-time owner code{bootstrapEnrollment.rebindDisplayDigest ? ", and preserved rebind digest" : ""}.</label>
      <button type="button" onclick={completeIdentityBootstrap} disabled={busy || bootstrapConfirmedFingerprint !== bootstrapEnrollment.desktopIdentityFingerprint}>Pin Exact Desktop Identity</button>
    {/if}
  </div>
  {/if}

  {#if ownerReady && !enrollment}
    <div class="primary-step">
      <div>
        <h3>2. Connect Android observation</h3>
        <p>{trustMode === "play_integrity" ? "Google Play release" : "Private / self-hosted build"} selected in Web Settings. Choose the route this phone can reach, then create one attested QR code.</p>
        <div class="route-options" role="radiogroup" aria-label="Android observation connection route">
          <label>
            <input type="radio" name="android-observation-route" value="same_wifi" bind:group={connectionMode} />
            <span><strong>Same Wi-Fi</strong><small>Direct to this Magician on the local network.</small></span>
          </label>
          <label>
            <input type="radio" name="android-observation-route" value="remote" bind:group={connectionMode} />
            <span><strong>Remote</strong><small>Use the secured connect.magican.ai route from anywhere.</small></span>
          </label>
        </div>
      </div>
      <button type="button" onclick={beginEnrollment} disabled={busy}>Create observation QR</button>
    </div>
  {/if}

  <details class="advanced-actions">
    <summary>Advanced and recovery</summary>
    <div class="actions">
      <button type="button" onclick={pollOwner} disabled={busy}>Refresh status</button>
      <button type="button" onclick={refreshTargets} disabled={busy || !ownerReady}>Load reviewed targets</button>
      <button type="button" onclick={refreshPending} disabled={busy || !ownerReady}>Check pending approval</button>
      <button type="button" onclick={beginRecovery} disabled={busy || !ownerReady}>Begin recovery</button>
    </div>
  </details>

  {#if enrollment}
    <div class="review-card">
      <h3>One-time attested enrollment</h3>
      <dl>
        <dt>Principal</dt><dd>{enrollment.principal}</dd>
        <dt>Workspace</dt><dd>{enrollment.workspace}</dd>
        <dt>Trust method</dt><dd>{enrollment.trust_mode === "play_integrity" ? "Google Play release" : "Private / self-hosted build"}</dd>
        <dt>Connection</dt><dd>{enrollment.connection_mode === "same_wifi" ? "Same Wi-Fi" : "Remote"}</dd>
        <dt>Enrollment</dt><dd><code>{enrollment.enrollment_id}</code></dd>
        <dt>Expires</dt><dd>{new Date(enrollment.expires_at_ms).toLocaleString()}</dd>
        <dt>Challenge</dt><dd><code>{enrollment.challenge_base64}</code></dd>
        <dt>Enrollment URI</dt><dd><code>{enrollment.enrollment_uri}</code></dd>
      </dl>
      {#if enrollmentQrUrl}
        <img class="qr" src={enrollmentQrUrl} alt="One-time Android Apps enrollment QR code" />
      {/if}
      <button class="danger" type="button" onclick={cancelEnrollment} disabled={busy}>Cancel Enrollment</button>
    </div>
  {/if}

  {#if targets.length > 0}
    <details class="advanced-actions">
      <summary>App Pilot action permissions</summary>
      <div class="review-card">
      <p>Optionally approve or revoke the supported actions and choose which Android apps this phone may control.</p>
      <label>Attested target
        <select bind:value={selectedTargetRef}>
          {#each targets as target}
            <option value={target.target_ref}>{target.device_label} — {target.target_ref}</option>
          {/each}
        </select>
      </label>
      {#if activeTarget}
        <dl>
          <dt>Scope</dt><dd>{activeTarget.principal} / {activeTarget.workspace}</dd>
          <dt>Owner app package</dt><dd><code>{activeTarget.owner_app_package}</code> v{activeTarget.app_version_code}</dd>
          <dt>App signer</dt><dd><code>{activeTarget.app_signing_sha256}</code></dd>
          <dt>APK digest</dt><dd><code>{activeTarget.apk_sha256}</code></dd>
          <dt>Attestation root</dt><dd><code>{activeTarget.attestation_root_sha256}</code></dd>
          <dt>Security</dt><dd>{activeTarget.attestation_security_level}</dd>
          <dt>Policy digest</dt><dd><code>{activeTarget.attestation_policy_digest}</code></dd>
          <dt>Review generation</dt><dd>{activeTarget.review_generation}</dd>
          <dt>Reviewed actions</dt><dd>{activeTarget.reviewed_actions.join(", ") || "none"}</dd>
          <dt>Allowed target packages</dt><dd>{activeTarget.reviewed_packages.join(", ") || "none"}</dd>
        </dl>
      {/if}
      <label>Operation
        <select bind:value={operation}>
          <option value="approve_actions">Approve exact eight-action roster</option>
          <option value="revoke_actions">Revoke exact eight-action roster</option>
        </select>
      </label>
      {#if operation === "approve_actions"}
        <label>Exact allowed target packages (comma or newline separated; never the Magdroid owner app)
          <textarea bind:value={packageText} rows="3" maxlength="32768" spellcheck="false"></textarea>
        </label>
      {/if}
      <button type="button" onclick={proposeReview} disabled={busy}>Review permission change</button>
      </div>
    </details>
  {/if}

  {#if status?.pending}
    {@const proposal = status.pending}
    <div class="review-card critical">
      <h3>Exact pending owner decision</h3>
      <dl>
        <dt>Schema</dt><dd><code>{proposal.schema}</code></dd>
        <dt>Proposal</dt><dd><code>{proposal.proposal_id}</code></dd>
        <dt>Owner generation</dt><dd>{proposal.owner_generation}</dd>
        <dt>Previous receipt</dt><dd><code>{proposal.previous_receipt_digest ?? "none"}</code></dd>
        <dt>Desktop identity</dt><dd><code>{proposal.desktop_identity_digest}</code></dd>
        <dt>Scope</dt><dd>{proposal.principal} / {proposal.workspace}</dd>
        <dt>Operation</dt><dd>{proposal.operation}</dd>
        <dt>Enrollment</dt><dd><code>{proposal.enrollment_id ?? "none"}</code></dd>
        <dt>Target</dt><dd><code>{proposal.target_ref}</code></dd>
        <dt>Device label</dt><dd>{proposal.device_label}</dd>
        <dt>Device key</dt><dd><code>{proposal.key_id}</code></dd>
        <dt>Automation identity</dt><dd><code>{proposal.automation_identity_digest}</code></dd>
        <dt>Owner app package</dt><dd><code>{proposal.owner_app_package}</code> v{proposal.app_version_code}</dd>
        <dt>App signer</dt><dd><code>{proposal.app_signing_sha256}</code></dd>
        <dt>APK digest</dt><dd><code>{proposal.apk_sha256}</code></dd>
        <dt>Attestation root</dt><dd><code>{proposal.attestation_root_sha256}</code></dd>
        <dt>Attestation security</dt><dd>{proposal.attestation_security_level}</dd>
        <dt>Attestation policy</dt><dd><code>{proposal.attestation_policy_digest}</code></dd>
        <dt>Review generation</dt><dd>{proposal.expected_review_generation} → {proposal.resulting_review_generation}</dd>
        <dt>Actions</dt><dd>{proposal.actions.join(", ") || "none"}</dd>
        <dt>Allowed target packages</dt><dd>{proposal.allowed_packages.join(", ") || "none"}</dd>
        <dt>Issued</dt><dd>{new Date(proposal.issued_at_ms).toLocaleString()}</dd>
        <dt>Expires</dt><dd>{new Date(proposal.expires_at_ms).toLocaleString()}</dd>
        <dt>Display digest</dt><dd><code>{proposal.display_digest}</code></dd>
      </dl>
      <label class="confirm"><input type="checkbox" checked={confirmedProposalDigest === proposal.display_digest} onchange={(event) => confirmedProposalDigest = event.currentTarget.checked ? proposal.display_digest : ""} /> I approve exactly every displayed field and this complete digest.</label>
      <button type="button" onclick={confirmProposal} disabled={busy || confirmedProposalDigest !== proposal.display_digest}>Sign, Persist &amp; Submit</button>
    </div>
  {/if}

  {#if status?.pendingRecovery}
    {@const recovery = status.pendingRecovery}
    <div class="review-card critical">
      <h3>Explicit full-record recovery</h3>
      <dl>
        <dt>Schema</dt><dd><code>{recovery.schema}</code></dd>
        <dt>Recovery nonce</dt><dd><code>{recovery.recovery_nonce}</code></dd>
        <dt>Desktop identity</dt><dd><code>{recovery.desktop_identity_digest}</code></dd>
        <dt>Runtime known generation</dt><dd>{recovery.known_owner_generation}</dd>
        <dt>Runtime known receipt</dt><dd><code>{recovery.known_latest_receipt_digest ?? "none"}</code></dd>
        <dt>Issued</dt><dd>{new Date(recovery.issued_at_ms).toLocaleString()}</dd>
        <dt>Expires</dt><dd>{new Date(recovery.expires_at_ms).toLocaleString()}</dd>
        <dt>Display digest</dt><dd><code>{recovery.display_digest}</code></dd>
      </dl>
      <label class="confirm"><input type="checkbox" checked={confirmedRecoveryDigest === recovery.display_digest} onchange={(event) => confirmedRecoveryDigest = event.currentTarget.checked ? recovery.display_digest : ""} /> I approve disclosing the signed latest record for every target under this exact recovery digest.</label>
      <button class="danger" type="button" onclick={confirmRecovery} disabled={busy || confirmedRecoveryDigest !== recovery.display_digest}>Sign &amp; Submit Exact Recovery</button>
    </div>
  {/if}
</section>

<style>
  /* Semantic tokens only: the tray Settings window renders under the synced
     unified-ui theme, and a hardcoded dark pill inherits dark text in a light
     theme (dark-on-dark). */
  .android-owner { background: var(--bg-secondary); border: 1px solid var(--border); border-radius: 12px; padding: 18px; }
  .title-row { display: flex; justify-content: space-between; gap: 16px; align-items: flex-start; }
  .title-row span { border-radius: 999px; padding: 4px 9px; white-space: nowrap; color: var(--text-muted); background: color-mix(in srgb, var(--text-muted) 14%, transparent); border: 1px solid var(--border); }
  .title-row span.active { color: var(--success); background: color-mix(in srgb, var(--success) 12%, transparent); border-color: color-mix(in srgb, var(--success) 42%, var(--border)); }
  h2, h3, p { margin-top: 0; }
  .actions { display: flex; gap: 10px; flex-wrap: wrap; margin-top: 14px; }
  .primary-step { align-items: center; display: flex; justify-content: space-between; gap: 16px; margin-top: 14px; padding: 14px; border: 1px solid var(--border); border-radius: 8px; }
  .primary-step h3, .primary-step p { margin-bottom: 0; }
  .route-options { display: grid; gap: 8px; grid-template-columns: repeat(2, minmax(0, 1fr)); margin-top: 12px; }
  .route-options label { align-items: flex-start; border: 1px solid var(--border); border-radius: 8px; cursor: pointer; display: flex; gap: 8px; padding: 10px; }
  .route-options span { display: grid; gap: 3px; }
  .route-options small { color: var(--text-muted); }
  .technical-details, .advanced-actions { margin-top: 12px; }
  .technical-details summary, .advanced-actions summary { color: var(--text-muted); cursor: pointer; font-weight: 600; }
  .review-card { display: grid; gap: 10px; margin-top: 14px; padding: 14px; border: 1px solid var(--border); border-radius: 8px; }
  .review-card.critical { border-color: color-mix(in srgb, var(--warning) 60%, var(--border)); }
  .rebind-review { display: grid; gap: 10px; padding: 12px; border: 1px solid color-mix(in srgb, var(--warning) 60%, var(--border)); border-radius: 8px; }
  dl { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: 6px 12px; }
  dt { color: var(--text-muted); }
  dd { margin: 0; overflow-wrap: anywhere; }
  label { display: grid; gap: 5px; }
  label.confirm { display: flex; align-items: flex-start; gap: 8px; }
  textarea, select { max-width: 100%; }
  code { overflow-wrap: anywhere; }
  .qr { width: 240px; max-width: 100%; background: white; padding: 8px; border-radius: 8px; }
  .error { color: var(--danger); }
  .message { color: var(--success); }
  button.danger { color: var(--danger); }
</style>
