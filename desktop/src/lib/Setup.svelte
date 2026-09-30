<script lang="ts">
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount, onDestroy } from "svelte";
  import PermissionChecklist from "./PermissionChecklist.svelte";
  import Onboarding from "./Onboarding.svelte";
  import { HOST_APP_NAME } from "./presentationIdentity.generated.js";
  import {
    magicianConnectionAuth,
    magicianFetch,
    signInMagician,
  } from "./magicianAuth.js";

  type Placement = "managed_container" | "native_install" | "existing_local" | "remote";
  type Phase = "loading" | "choose" | "consent" | "progress" | "edge_enrollment" | "authenticate" | "capabilities" | "failed";

  interface SetupProgress {
    step: string | { Failed: string };
    progress: number;
    message: string;
  }

  interface InstallPlan {
    needs_homebrew: boolean;
    runtime_name: string;
    needs_runtime_install: boolean;
    host_prerequisites: Array<{ id: string; label: string; installed: boolean }>;
    replaces_existing_container: boolean;
    summary: string[];
  }

  interface PlacementOption {
    id: Placement;
    label: string;
    description: string;
    available: boolean;
    unavailable_reason?: string;
    recommended: boolean;
  }

  interface SetupOptions {
    platform: string;
    current_placement: Placement;
    current_engine_url: string;
    data_root: string;
    placements: PlacementOption[];
  }

  interface SetupStartResult {
    next_action: "install" | "edge_enrollment" | "authenticate";
    install_plan?: InstallPlan;
    replace_existing_container: boolean;
    message: string;
  }

  interface Props {
    onComplete: (capabilitiesCompleted?: boolean) => Promise<void>;
  }

  let { onComplete }: Props = $props();

  let phase = $state<Phase>("loading");
  let options = $state<SetupOptions | null>(null);
  let selectedPlacement = $state<Placement>("managed_container");
  let engineUrl = $state("http://127.0.0.1:3002");
  let dataRoot = $state("");
  let progress = $state(0);
  let message = $state("Inspecting this computer…");
  let currentStep = $state("CheckingPrerequisites");
  let failedAtStep = $state<string | null>(null);
  let failMessage = $state("");
  let installPlan = $state<InstallPlan | null>(null);
  let replaceExistingContainer = $state(false);
  let sessionOrigin = $state("");
  let sessionUsername = $state("");
  let sessionPassword = $state("");
  let sessionBusy = $state(false);
  let sessionError = $state("");
  let edgeEnrollmentUri = $state("");
  let edgeEnrollmentBusy = $state(false);
  let edgeEnrollmentError = $state("");
  const capabilityOnly = new URLSearchParams(window.location.search).get("mode") === "capabilities";

  const steps = [
    { key: "CheckingPrerequisites", label: "Check this computer" },
    { key: "AwaitingConsent", label: "Review installation" },
    { key: "InstallingPrerequisites", label: "Prepare host tools" },
    { key: "InstallingRuntime", label: "Install container runtime" },
    { key: "PullingImage", label: "Download backend image" },
    { key: "CreatingDataDirs", label: "Prepare data folder" },
    { key: "StartingContainer", label: "Start services" },
    { key: "WaitingForHealth", label: "Verify connection" },
    { key: "Ready", label: "Ready" },
  ];

  let unlistenProgress: (() => void) | null = null;
  let unlistenConsent: (() => void) | null = null;
  let unlistenEdgeEnrollment: (() => void) | null = null;

  onMount(async () => {
    if (capabilityOnly) {
      await requireSession();
      return;
    }
    unlistenProgress = await listen<SetupProgress>("setup-progress", (event) => {
      const data = event.payload;
      progress = data.progress;
      message = data.message;

      if (typeof data.step === "string") {
        currentStep = data.step;
        if (data.step === "AwaitingConsent") phase = "consent";
        else if (data.step === "Ready" && (selectedPlacement === "managed_container" || selectedPlacement === "native_install")) void requireSession();
        else phase = "progress";
      } else if (data.step && "Failed" in data.step) {
        failMessage = data.step.Failed;
        failedAtStep = currentStep;
        phase = "failed";
      }
    });

    unlistenConsent = await listen<InstallPlan>("setup-consent-needed", (event) => {
      installPlan = event.payload;
      currentStep = "AwaitingConsent";
      phase = "consent";
    });

    unlistenEdgeEnrollment = await listen<{ ok: boolean; error?: string }>("desktop-edge-enrollment", (event) => {
      if (event.payload.ok) void requireSession();
      else {
        edgeEnrollmentError = event.payload.error ?? "Desktop Edge enrollment failed.";
        phase = "edge_enrollment";
      }
    });

    try {
      options = await invoke<SetupOptions>("get_setup_options");
      selectedPlacement = options.current_placement;
      engineUrl = options.current_engine_url;
      dataRoot = options.data_root;
      phase = "choose";
    } catch (error) {
      failMessage = String(error);
      phase = "failed";
    }
  });

  onDestroy(() => {
    unlistenProgress?.();
    unlistenConsent?.();
    unlistenEdgeEnrollment?.();
  });

  function selectPlacement(option: PlacementOption) {
    if (!option.available) return;
    selectedPlacement = option.id;
    failMessage = "";
    if (option.id === "existing_local" && !engineUrl.startsWith("http://")) {
      engineUrl = "http://127.0.0.1:3002";
    }
    if (option.id === "remote" && !engineUrl.startsWith("https://")) {
      engineUrl = "https://connect.magican.ai";
    }
  }

  function canContinue(): boolean {
    if (selectedPlacement === "remote") return engineUrl.trim().startsWith("https://");
    if (selectedPlacement === "existing_local") return engineUrl.trim().length > 0;
    return (selectedPlacement === "managed_container" || selectedPlacement === "native_install") && dataRoot.trim().length > 0;
  }

  async function continueSetup() {
    if (!canContinue()) return;
    phase = "progress";
    progress = 0;
    currentStep = "CheckingPrerequisites";
    message = selectedPlacement === "managed_container"
      ? "Checking container requirements…"
      : selectedPlacement === "native_install"
        ? "Checking the native backend package…"
        : "Verifying the server…";
    failMessage = "";

    try {
      const result = await invoke<SetupStartResult>("apply_setup_selection", {
        selection: {
          placement: selectedPlacement,
          engine_url: selectedPlacement === "managed_container" || selectedPlacement === "native_install" ? null : engineUrl.trim(),
          data_root: selectedPlacement === "managed_container" || selectedPlacement === "native_install" ? dataRoot.trim() : null,
        },
      });
      message = result.message;
      if (result.next_action === "install") {
        installPlan = result.install_plan ?? null;
        replaceExistingContainer = result.replace_existing_container;
        phase = "consent";
      } else if (result.next_action === "edge_enrollment") {
        progress = 1;
        currentStep = "Ready";
        phase = "edge_enrollment";
      } else {
        progress = 1;
        currentStep = "Ready";
        await requireSession();
      }
    } catch (error) {
      failMessage = String(error);
      failedAtStep = currentStep;
      phase = "failed";
    }
  }

  async function openRemoteEnrollmentPage(): Promise<void> {
    edgeEnrollmentError = "";
    try {
      await invoke("open_remote_enrollment_page");
    } catch (error) {
      edgeEnrollmentError = String(error);
    }
  }

  async function enrollDesktopEdge(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    if (!edgeEnrollmentUri.trim() || edgeEnrollmentBusy) return;
    edgeEnrollmentBusy = true;
    edgeEnrollmentError = "";
    try {
      await invoke("enroll_edge_client", { enrollmentUri: edgeEnrollmentUri.trim() });
      edgeEnrollmentUri = "";
      await requireSession();
    } catch (error) {
      edgeEnrollmentError = String(error);
      phase = "edge_enrollment";
    } finally {
      edgeEnrollmentBusy = false;
    }
  }

  async function approveSetup() {
    phase = "progress";
    message = "Installing…";
    try {
      await invoke("approve_setup", {
        selection: {
          placement: selectedPlacement,
          engine_url: null,
          data_root: dataRoot.trim(),
        },
        replaceExistingContainer,
      });
    } catch (error) {
      failMessage = String(error);
      failedAtStep = currentStep;
      phase = "failed";
    }
  }

  async function requireSession(): Promise<void> {
    sessionBusy = true;
    sessionError = "";
    try {
      const auth = await magicianConnectionAuth();
      sessionOrigin = auth.origin;
      const response = await magicianFetch(`${auth.origin}/api/magician/v2/auth/session`);
      if (response.ok) {
        phase = "capabilities";
        return;
      }
      if (response.status === 401) {
        message = "Create the owner account or sign in to this server.";
        phase = "authenticate";
        return;
      }
      throw new Error(`The server could not verify the desktop session (${response.status}).`);
    } catch (error) {
      sessionError = String(error);
      phase = "authenticate";
    } finally {
      sessionBusy = false;
    }
  }

  async function signInSetup(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    if (!sessionUsername.trim() || !sessionPassword) return;
    sessionBusy = true;
    sessionError = "";
    try {
      await signInMagician(sessionUsername, sessionPassword);
      sessionPassword = "";
      await requireSession();
    } catch (error) {
      sessionError = String(error);
      sessionPassword = "";
    } finally {
      sessionBusy = false;
    }
  }

  function returnToPlacement(): void {
    sessionUsername = "";
    sessionPassword = "";
    sessionError = "";
    edgeEnrollmentUri = "";
    edgeEnrollmentError = "";
    edgeEnrollmentBusy = false;
    phase = "choose";
  }

  function stepStatus(stepKey: string): "done" | "active" | "pending" | "failed" {
    const visibleSteps = setupSteps();
    if (phase === "failed" && failedAtStep) {
      const thisIndex = visibleSteps.findIndex((step) => step.key === stepKey);
      const failedIndex = visibleSteps.findIndex((step) => step.key === failedAtStep);
      if (thisIndex < failedIndex) return "done";
      if (thisIndex === failedIndex) return "failed";
      return "pending";
    }
    const currentIndex = visibleSteps.findIndex((step) => step.key === currentStep);
    const thisIndex = visibleSteps.findIndex((step) => step.key === stepKey);
    if (thisIndex < currentIndex) return "done";
    if (thisIndex === currentIndex) return "active";
    return "pending";
  }

  async function retry() {
    failMessage = "";
    failedAtStep = null;
    if (installPlan && (selectedPlacement === "managed_container" || selectedPlacement === "native_install")) await approveSetup();
    else {
      phase = "choose";
      message = "Choose where the backend runs.";
    }
  }

  function setupSteps() {
    if (selectedPlacement === "native_install") {
      return [
        { key: "CheckingPrerequisites", label: "Check native package" },
        { key: "AwaitingConsent", label: "Review installation" },
        { key: "InstallingPrerequisites", label: "Prepare host tools" },
        { key: "InstallingBackend", label: "Install backend" },
        { key: "CreatingDataDirs", label: "Prepare data folder" },
        { key: "RegisteringService", label: "Register background service" },
        { key: "WaitingForHealth", label: "Verify connection" },
        { key: "Ready", label: "Ready" },
      ];
    }
    return steps;
  }

</script>

<main class="setup-shell">
  {#if phase !== "capabilities"}<header>
    <div class="eyebrow">MAGICAN DESKTOP</div>
    <h1>{phase === "choose" ? "Where should the backend run?" : `Set up ${HOST_APP_NAME}`}</h1>
    <p>
      {phase === "choose"
        ? "The desktop stays on this computer for CUA, browser access, and platform tools. Choose where the engine and its data live."
        : message}
    </p>
  </header>{/if}

  {#if phase === "loading"}
    <div class="loading-card"><span class="spinner"></span>{message}</div>
  {:else if phase === "choose" && options}
    <section class="placement-grid" aria-label="Backend placement">
      {#each options.placements as option}
        <button
          type="button"
          class="placement-card"
          class:selected={selectedPlacement === option.id}
          class:unavailable={!option.available}
          disabled={!option.available}
          onclick={() => selectPlacement(option)}
        >
          <span class="placement-heading">
            <strong>{option.label}</strong>
            {#if option.recommended}<span class="badge">Recommended</span>{/if}
          </span>
          <span>{option.description}</span>
          {#if option.unavailable_reason}
            <small>{option.unavailable_reason}</small>
          {/if}
        </button>
      {/each}
    </section>

    {#if selectedPlacement === "managed_container" || selectedPlacement === "native_install"}
      <section class="details-card">
        <label for="data-root">Data folder</label>
        <input id="data-root" bind:value={dataRoot} autocomplete="off" spellcheck="false" />
        <p>Notes, chats, tasks, credentials, and runtime state persist here. Reinstalling the app does not remove it.</p>
        {#if selectedPlacement === "managed_container"}
          <p>
            {options.platform === "windows"
              ? "Install and start Docker Desktop first, including its WSL and virtualization requirements. Setup verifies the running Docker engine before downloading and starting the Linux backend image."
              : options.platform === "linux"
                ? "Install and start Docker Engine first, and make sure `docker info` works for this user without sudo. Setup verifies it before downloading and starting the Linux backend image."
                : "On eligible Apple Silicon Macs, setup uses Apple Container and Apple's virtualization stack. Other Macs use Docker with Colima. Setup can install or start the selected macOS runtime."}
          </p>
        {/if}
      </section>
    {:else if selectedPlacement === "existing_local"}
      <section class="details-card">
        <label for="local-engine-url">Local engine URL</label>
        <input id="local-engine-url" type="url" bind:value={engineUrl} autocomplete="url" spellcheck="false" />
        <p>
          Enter the loopback URL exposed by the Linux backend container already running on this computer. The container runtime owns service lifecycle; Desktop owns local host tools.
        </p>
      </section>
    {:else if selectedPlacement === "remote"}
      <section class="details-card">
        <label for="remote-engine-url">Remote engine URL</label>
        <input id="remote-engine-url" type="url" bind:value={engineUrl} autocomplete="url" spellcheck="false" />
        <p>
          Use this when the Linux backend container runs elsewhere and this computer should supply CUA, browser, and platform tools. HTTPS is required; setup guides sign-in and any required Desktop Edge authorization in the order this server needs.
        </p>
      </section>
    {/if}

    <div class="actions">
      <button class="primary" type="button" disabled={!canContinue()} onclick={continueSetup}>Continue</button>
    </div>
  {:else if phase === "consent" && installPlan}
    <section class="consent-card">
      <h2>Review installation</h2>
      <ul>
        {#each installPlan.summary as item}<li>{item}</li>{/each}
      </ul>
      {#if installPlan.host_prerequisites.length > 0}
        <p class="muted">
          Host prerequisites:
          {installPlan.host_prerequisites.map((item) => `${item.label} (${item.installed ? "ready" : "will install"})`).join(", ")}.
        </p>
      {/if}
      <p>Runtime: <strong>{installPlan.runtime_name}</strong></p>
      <p class="muted">
        Your existing data folder is preserved.
        {selectedPlacement === "managed_container"
          ? " A system authorization prompt may appear when the container runtime needs installation."
          : " The background service is installed for this user and starts without a terminal."}
      </p>
      <div class="actions split">
        <button class="secondary" type="button" onclick={() => (phase = "choose")}>Back</button>
        <button class="primary" type="button" onclick={approveSetup}>Install and start</button>
      </div>
    </section>
  {:else if phase === "capabilities"}
    <Onboarding maintenance={capabilityOnly} onComplete={() => onComplete(true)} />
  {:else if phase === "edge_enrollment"}
    <section class="details-card auth-card">
      <div>
        <h2>Authorize this desktop</h2>
        <p>Server: <code>{engineUrl}</code></p>
      </div>
      <p>
        This server uses an outer access gate. Open its Settings page in your browser, sign in there, choose <strong>Connect Desktop Edge</strong>, then open or paste the one-time Magican link. The link supplies a desktop-only credential; browser cookies and passwords stay in the browser.
      </p>
      <div class="actions split">
        <button class="secondary" type="button" disabled={edgeEnrollmentBusy} onclick={returnToPlacement}>Change server</button>
        <button class="primary" type="button" disabled={edgeEnrollmentBusy} onclick={openRemoteEnrollmentPage}>Open server Settings</button>
      </div>
      <form onsubmit={enrollDesktopEdge}>
        <label for="desktop-edge-uri">One-time Desktop Edge link</label>
        <input id="desktop-edge-uri" type="url" placeholder="magican://connect?…" autocomplete="off" spellcheck="false" bind:value={edgeEnrollmentUri} disabled={edgeEnrollmentBusy} />
        {#if edgeEnrollmentError}<p class="auth-error" role="alert">{edgeEnrollmentError}</p>{/if}
        <div class="actions">
          <button class="primary" type="submit" disabled={edgeEnrollmentBusy || !edgeEnrollmentUri.trim().startsWith("magican://connect?")}>{edgeEnrollmentBusy ? "Authorizing…" : "Authorize desktop"}</button>
        </div>
      </form>
    </section>
  {:else if phase === "authenticate"}
    <section class="details-card auth-card">
      <div>
        <h2>Sign in to this server</h2>
        <p>Server: <code>{sessionOrigin || "Resolving…"}</code></p>
      </div>
      <p>
        On a new server, the first login creates its owner account and keeps the existing
        <code>anonymous/default</code> data. If an owner already exists, enter that account's credentials.
        The session is saved in this computer's credential store; the password is not saved.
      </p>
      <form onsubmit={signInSetup}>
        <label for="setup-session-username">Username</label>
        <input id="setup-session-username" autocomplete="username" bind:value={sessionUsername} required disabled={sessionBusy} />
        <label for="setup-session-password">Password</label>
        <input id="setup-session-password" type="password" autocomplete="current-password" bind:value={sessionPassword} required disabled={sessionBusy} />
        {#if sessionError}<p class="auth-error" role="alert">{sessionError}</p>{/if}
        <div class="actions split">
          {#if !capabilityOnly}<button class="secondary" type="button" disabled={sessionBusy} onclick={returnToPlacement}>Change server</button>{/if}
          <button class="primary" type="submit" disabled={sessionBusy || !sessionUsername.trim() || !sessionPassword}>
            {sessionBusy ? "Signing in…" : "Create owner or sign in"}
          </button>
        </div>
      </form>
    </section>
  {:else if phase === "progress" || phase === "failed"}
    <section class="progress-card">
      <div class="steps-list">
        {#each setupSteps() as step}
          <div class="step" class:done={stepStatus(step.key) === "done"} class:active={stepStatus(step.key) === "active"} class:failed={stepStatus(step.key) === "failed"}>
            <span class="step-indicator">
              {#if stepStatus(step.key) === "done"}✓{:else if stepStatus(step.key) === "failed"}×{:else}<span class="dot"></span>{/if}
            </span>
            <span>{step.label}</span>
          </div>
        {/each}
      </div>

      {#if phase === "progress"}
        <div class="progress-section">
          <div class="progress-bar"><div class="fill" style="width: {progress * 100}%"></div></div>
          <p>{message}</p>
        </div>
      {:else if phase === "failed"}
        <div class="result error" role="alert">
          <strong>Setup stopped</strong>
          <span>{failMessage}</span>
          <div class="actions"><button class="primary" type="button" onclick={retry}>Try again</button></div>
        </div>
      {/if}
    </section>
  {/if}

  {#if phase === "choose" || phase === "edge_enrollment" || phase === "authenticate"}
    <PermissionChecklist compact />
  {/if}
</main>

<style>
  .setup-shell {
    box-sizing: border-box;
    width: min(760px, 100%);
    min-height: 100vh;
    margin: 0 auto;
    padding: 40px 38px 48px;
    display: flex;
    flex-direction: column;
    gap: 24px;
  }

  header { display: grid; gap: 8px; }
  header h1 { margin: 0; font-size: 28px; line-height: 1.15; }
  header p { margin: 0; color: var(--text-muted); line-height: 1.5; max-width: 680px; }
  .eyebrow { color: var(--accent); font-size: 11px; font-weight: 750; letter-spacing: 0.13em; }

  .placement-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
  .placement-card {
    appearance: none;
    min-height: 142px;
    padding: 16px;
    border: 1px solid var(--border);
    border-radius: 14px;
    background: color-mix(in srgb, var(--surface) 92%, transparent);
    color: var(--text);
    text-align: left;
    display: flex;
    flex-direction: column;
    gap: 9px;
    cursor: pointer;
  }
  .placement-card:hover:not(:disabled) { border-color: color-mix(in srgb, var(--accent) 55%, var(--border)); }
  .placement-card.selected { border-color: var(--accent); box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent) 18%, transparent); }
  .placement-card.unavailable { opacity: 0.58; cursor: not-allowed; }
  .placement-card span:not(.placement-heading):not(.badge), .placement-card small { color: var(--text-muted); line-height: 1.35; }
  .placement-card small { margin-top: auto; }
  .placement-heading { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; }
  .badge { padding: 3px 7px; border-radius: 999px; background: color-mix(in srgb, var(--accent) 16%, transparent); color: var(--accent); font-size: 10px; font-weight: 700; }

  .details-card, .consent-card, .progress-card, .loading-card {
    border: 1px solid var(--border);
    border-radius: 14px;
    padding: 18px;
    background: color-mix(in srgb, var(--surface) 92%, transparent);
  }
  .details-card { display: grid; gap: 9px; }
  .details-card label { font-size: 13px; font-weight: 700; }
  .details-card input { box-sizing: border-box; width: 100%; padding: 11px 12px; border: 1px solid var(--border); border-radius: 9px; background: var(--background); color: var(--text); font: inherit; }
  .details-card input:focus { outline: 2px solid color-mix(in srgb, var(--accent) 35%, transparent); border-color: var(--accent); }
  .details-card p, .consent-card p { margin: 0; color: var(--text-muted); font-size: 12px; line-height: 1.45; }
  .auth-card h2 { margin: 0 0 6px; font-size: 18px; }
  .auth-card form { display: grid; gap: 9px; }
  .auth-card form .actions { margin-top: 7px; }
  .auth-error { color: var(--error) !important; }

  .consent-card { display: grid; gap: 16px; }
  .consent-card h2 { margin: 0; font-size: 18px; }
  .consent-card ul { margin: 0; padding-left: 22px; display: grid; gap: 8px; }
  .muted { color: var(--text-muted); }

  .actions { display: flex; justify-content: flex-end; gap: 10px; }
  .actions.split { justify-content: space-between; }
  button.primary, button.secondary { min-height: 40px; padding: 0 17px; border-radius: 9px; font: inherit; font-weight: 680; cursor: pointer; }
  button.primary { border: 1px solid var(--accent); background: var(--accent); color: var(--accent-contrast, white); }
  button.primary:disabled { opacity: 0.45; cursor: not-allowed; }
  button.secondary { border: 1px solid var(--border); background: transparent; color: var(--text); }

  .progress-card { display: grid; gap: 22px; }
  .steps-list { display: grid; grid-template-columns: 1fr 1fr; gap: 11px 22px; }
  .step { display: flex; align-items: center; gap: 10px; color: var(--text-muted); font-size: 13px; }
  .step.done { color: var(--success); }
  .step.active { color: var(--text); font-weight: 650; }
  .step.failed { color: var(--error); }
  .step-indicator { width: 20px; height: 20px; display: grid; place-items: center; font-weight: 800; }
  .dot { width: 8px; height: 8px; border-radius: 999px; background: var(--border); }
  .active .dot { background: var(--accent); animation: pulse 1.4s infinite; }
  @keyframes pulse { 50% { opacity: 0.35; } }

  .progress-section { display: grid; gap: 9px; }
  .progress-section p { margin: 0; text-align: center; color: var(--text-muted); font-size: 13px; }
  .progress-bar { height: 7px; overflow: hidden; border-radius: 999px; background: var(--border); }
  .fill { height: 100%; border-radius: inherit; background: var(--accent); transition: width 0.25s ease; }

  .result { display: grid; gap: 9px; padding: 14px; border-radius: 10px; }
  .result span { color: var(--text-muted); line-height: 1.45; }
  .result.error { background: color-mix(in srgb, var(--error) 10%, transparent); }
  .result.error strong { color: var(--error); }

  .loading-card { display: flex; justify-content: center; align-items: center; gap: 12px; color: var(--text-muted); }
  .spinner { width: 18px; height: 18px; border: 2px solid var(--border); border-top-color: var(--accent); border-radius: 999px; animation: spin 0.8s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }

  @media (max-width: 660px) {
    .setup-shell { padding: 28px 20px 36px; }
    .placement-grid, .steps-list { grid-template-columns: 1fr; }
  }
</style>
