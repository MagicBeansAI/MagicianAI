<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

  type Observed = { state: "present" | "absent" | "unknown"; detail: string };
  type InstallAction =
    | { kind: "with_runtime" }
    | { kind: "make"; target: string; script?: string }
    | { kind: "manual"; steps: string[]; open?: string; secrets?: SecretPrompt[] };

  interface SecretPrompt { variable: string; label: string; bot?: string }
  interface SetupField {
    id: string;
    label: string;
    kind: "text" | "email" | "password";
    required: boolean;
    env: string;
    placeholder?: string;
    help?: string;
  }
  interface SetupInput {
    id: string;
    label: string;
    kind: "text" | "email" | "password";
    placeholder?: string;
    help?: string;
    max_bytes: number;
  }
  interface SetupModeChoice {
    id: string;
    label: string;
    description: string;
    local_generation: boolean;
  }
  interface CuaDriverInstallerFile { name: string; url: string; sha256: string }
  type SetupDriver =
    | { kind: "managed_bot"; bot: string; start?: ("bot" | "auth")[]; fields?: SetupField[]; fixed_env?: Record<string, string>; login_url_hosts?: string[]; input?: SetupInput }
    | { kind: "governed_oauth" }
    | { kind: "configuration_file"; field_label: string; accept: string; destination: string; validator: string; max_bytes: number }
    | { kind: "model_runtime"; privacy_path: string; generation_path: string; local_feature: string; modes: SetupModeChoice[] }
    | { kind: "browser_extension"; bundle_resource: string; management_url: string; probe_path: string; probe_needle: string }
    | { kind: "cua_driver"; version: string; unix_installer: CuaDriverInstallerFile[]; windows_installer: CuaDriverInstallerFile[] };
  interface ResolvedSetup { definition: string; label: string; profile?: string; driver: SetupDriver }
  interface Feature { id: string; name: string; description: string }
  interface FeatureStatus { id: string; availability: { state: string; reason?: string } }
  interface Graph { features: Feature[] }
  interface ComponentCatalog { graph: Graph; report: { features: FeatureStatus[] } }
  interface SetupAccess {
    authorized: boolean;
    token_stored: boolean;
    local_bootstrap: boolean;
    unavailable_reason?: string;
  }
  interface SkillAuth {
    kind: string;
    requirement: string;
    provider?: string;
    profile_selection: { mode: string; default?: string; alias?: string };
    secret_refs: string[];
    has_login: boolean;
    login_interaction?: string;
    setup?: ResolvedSetup;
  }
  interface SkillItem {
    name: string;
    version?: string;
    description: string;
    kind: string;
    origin: string;
    installable: boolean;
    requires_bins: string[];
    missing_bins: string[];
    requires_env: string[];
    install_hint?: string;
    auth?: SkillAuth;
    installed_scopes: string[];
    setup?: { required: boolean; ready: boolean; missing: string[]; detail: string };
  }
  interface Catalog {
    components: ComponentCatalog;
    skills: SkillItem[];
    current_scope: string;
    selected_features: string[];
    preferences_exist: boolean;
    setup_access: SetupAccess;
  }
  interface PlanStep {
    id: string;
    name: string;
    required: boolean;
    cost: string;
    pricing: string;
    install: InstallAction;
    observed: Observed;
    ready: boolean;
    can_confirm: boolean;
    confirmed: boolean;
    setup?: ResolvedSetup;
  }
  interface Plan {
    selected_features: string[];
    steps: PlanStep[];
    unsupported: [string, string][];
    unresolved: string[];
    left_off: [string, string][];
    ready: boolean;
    blocking_count: number;
  }
  interface InstallJob {
    id: string;
    component_id: string;
    component_name: string;
    phase: "queued" | "running" | "succeeded" | "failed";
    output: string;
    error?: string;
    observed?: Observed;
  }
  interface PairingSnapshot {
    component_id: string;
    bot_name: string;
    status: string;
    flow_state: string;
    detail?: string;
    ready: boolean;
    log: string;
    qr_data_url?: string;
    login_url?: string;
  }
  interface LocalGenerationModel {
    id: string;
    label: string;
    ollama: string;
    min_memory_gb: number;
    resident_gb?: number;
    disk_gb?: number;
    notes?: string;
    selected: boolean;
    recommended: boolean;
    rule_ok: boolean;
    installed?: boolean;
    warnings: string[];
  }
  interface ModelRuntimeSnapshot {
    component_id: string;
    modes: SetupModeChoice[];
    local_feature: string;
    mode: string;
    selected?: string;
    recommended?: string;
    models: LocalGenerationModel[];
    warnings: string[];
    configured: boolean;
  }
  interface BrowserExtensionPreparation { source_path: string; management_url: string }
  interface CuaDriverSetupResult { installed: boolean; ready: boolean; detail: string; installer_output: string }
  type HarnessSurface = "vibedev" | "chat" | "agentic_runs" | "background_model_calls";
  interface HarnessItem {
    id: string;
    order: number;
    label: string;
    binary: string;
    coding_engine: string;
    plane_engine: string;
    config_key: string;
    install_url: string;
    install_hint: string;
    surfaces: HarnessSurface[];
    readiness: string;
    reason: string;
    version?: string;
    selectable: boolean;
    installed: boolean;
    models: string[];
  }
  interface HarnessCatalog {
    harnesses: HarnessItem[];
    chat_current: string;
    run_current: string;
    chat_model: string;
    run_model: string;
  }
  interface Props { onComplete: () => Promise<void>; maintenance?: boolean }

  let { onComplete, maintenance = false }: Props = $props();
  let phase = $state<"loading" | "select" | "requirements" | "skills" | "harnesses" | "failed">("loading");
  let catalog = $state<Catalog | null>(null);
  let selected = $state<string[]>([]);
  let plan = $state<Plan | null>(null);
  let error = $state("");
  let query = $state("");
  let skillKind = $state("all");
  let secretValues = $state<Record<string, string>>({});
  let skillSecretValues = $state<Record<string, string>>({});
  let setupToken = $state("");
  let busyStep = $state("");
  let skillBusy = $state("");
  let setupAccessBusy = $state(false);
  let installJobs = $state<Record<string, InstallJob>>({});
  let pairing = $state<Record<string, PairingSnapshot>>({});
  let skillAuth = $state<Record<string, PairingSnapshot>>({});
  let setupFieldValues = $state<Record<string, string>>({});
  let setupInputValues = $state<Record<string, string>>({});
  let modelRuntime = $state<Record<string, ModelRuntimeSnapshot>>({});
  let modelModes = $state<Record<string, string>>({});
  let modelSelections = $state<Record<string, string>>({});
  let extensionPreparations = $state<Record<string, BrowserExtensionPreparation>>({});
  let cuaSetupResults = $state<Record<string, CuaDriverSetupResult>>({});
  let harnessCatalog = $state<HarnessCatalog | null>(null);
  let harnessBusy = $state("");
  let selectedChatEngine = $state("magician");
  let selectedRunEngine = $state("magician");
  let selectedChatModel = $state("default");
  let selectedRunModel = $state("default");
  let authInputBusy = $state(false);
  let checking = $state(false);
  let completing = $state(false);

  const defaultFeatures = ["chat", "memory", "notes", "browser-research", "computer-use"];

  onMount(loadCatalog);

  async function loadCatalog() {
    phase = "loading";
    error = "";
    try {
      catalog = await invoke<Catalog>("get_onboarding_catalog");
      const known = new Set(catalog.components.graph.features.map((feature) => feature.id));
      selected = catalog.preferences_exist
        ? catalog.selected_features.filter((id) => known.has(id))
        : defaultFeatures.filter((id) => known.has(id));
      phase = "select";
    } catch (reason) {
      error = String(reason);
      phase = "failed";
    }
  }

  function isSelected(id: string): boolean { return selected.includes(id); }

  function toggleFeature(id: string) {
    selected = isSelected(id) ? selected.filter((item) => item !== id) : [...selected, id];
  }

  function currentAvailability(id: string): FeatureStatus["availability"] | undefined {
    return catalog?.components.report.features.find((feature) => feature.id === id)?.availability;
  }

  async function buildPlan() {
    checking = true;
    error = "";
    try {
      plan = await invoke<Plan>("plan_onboarding", { selection: { features: selected } });
      selected = plan.selected_features;
      await hydrateRuntimeSetups(plan);
      phase = "requirements";
    } catch (reason) {
      error = String(reason);
    } finally {
      checking = false;
    }
  }

  async function refreshPlan() {
    if (catalog) catalog = await invoke<Catalog>("get_onboarding_catalog");
    await buildPlan();
  }

  async function hydrateRuntimeSetups(nextPlan: Plan) {
    const steps = nextPlan.steps.filter((step) => step.setup?.driver.kind === "model_runtime");
    const snapshots = await Promise.all(steps.map((step) => invoke<ModelRuntimeSnapshot>(
      "get_onboarding_model_runtime",
      { request: { component_id: step.id } },
    )));
    const nextRuntime = { ...modelRuntime };
    const nextModes = { ...modelModes };
    const nextSelections = { ...modelSelections };
    for (const snapshot of snapshots) {
      nextRuntime[snapshot.component_id] = snapshot;
      nextModes[snapshot.component_id] = snapshot.mode;
      nextSelections[snapshot.component_id] = snapshot.selected ?? snapshot.recommended ?? snapshot.models[0]?.id ?? "";
    }
    modelRuntime = nextRuntime;
    modelModes = nextModes;
    modelSelections = nextSelections;
  }

  function selectedMode(step: PlanStep): SetupModeChoice | undefined {
    const snapshot = modelRuntime[step.id];
    return snapshot?.modes.find((mode) => mode.id === modelModes[step.id]);
  }

  async function saveModelRuntime(step: PlanStep) {
    const snapshot = modelRuntime[step.id];
    const driver = step.setup?.driver;
    if (!snapshot || driver?.kind !== "model_runtime") return;
    busyStep = `${step.id}:model-runtime`;
    error = "";
    try {
      const saved = await invoke<ModelRuntimeSnapshot>("configure_onboarding_model_runtime", {
        request: {
          component_id: step.id,
          mode: modelModes[step.id],
          selected_model: selectedMode(step)?.local_generation ? modelSelections[step.id] : null,
        },
      });
      modelRuntime = { ...modelRuntime, [step.id]: saved };
      selected = selectedMode(step)?.local_generation
        ? Array.from(new Set([...selected, driver.local_feature]))
        : selected.filter((feature) => feature !== driver.local_feature);
      await buildPlan();
    } catch (reason) {
      error = String(reason);
    } finally {
      busyStep = "";
    }
  }

  async function prepareBrowserExtension(step: PlanStep) {
    busyStep = `${step.id}:browser-extension`;
    error = "";
    try {
      const prepared = await invoke<BrowserExtensionPreparation>("prepare_onboarding_browser_extension", {
        request: { component_id: step.id },
      });
      extensionPreparations = { ...extensionPreparations, [step.id]: prepared };
    } catch (reason) {
      error = String(reason);
    } finally {
      busyStep = "";
    }
  }

  async function installCuaDriver(step: PlanStep) {
    busyStep = `${step.id}:cua-driver`;
    error = "";
    try {
      const result = await invoke<CuaDriverSetupResult>("install_onboarding_cua_driver", {
        request: { component_id: step.id },
      });
      cuaSetupResults = { ...cuaSetupResults, [step.id]: result };
      await refreshPlan();
    } catch (reason) {
      error = String(reason);
    } finally {
      busyStep = "";
    }
  }

  async function showHarnesses() {
    phase = "harnesses";
    if (!harnessCatalog) await loadHarnesses();
  }

  async function loadHarnesses() {
    harnessBusy = "catalog";
    error = "";
    try {
      const snapshot = await invoke<HarnessCatalog>("get_onboarding_harnesses");
      applyHarnessCatalog(snapshot);
    } catch (reason) {
      error = String(reason);
    } finally {
      harnessBusy = "";
    }
  }

  function applyHarnessCatalog(snapshot: HarnessCatalog) {
    harnessCatalog = snapshot;
    selectedChatEngine = snapshot.chat_current;
    selectedRunEngine = snapshot.run_current;
    selectedChatModel = snapshot.chat_model;
    selectedRunModel = snapshot.run_model;
  }

  async function refreshHarness(harness: HarnessItem) {
    harnessBusy = `${harness.id}:refresh`;
    error = "";
    try {
      applyHarnessCatalog(await invoke<HarnessCatalog>("refresh_onboarding_harness", {
        request: { harness_id: harness.id },
      }));
    } catch (reason) {
      error = String(reason);
    } finally {
      harnessBusy = "";
    }
  }

  function harnessModels(engine: string): string[] {
    if (engine === "magician") return ["default"];
    return harnessCatalog?.harnesses.find((harness) => harness.plane_engine === engine)?.models ?? ["default"];
  }

  function resetHarnessModel(target: "chat" | "run") {
    if (target === "chat") selectedChatModel = harnessModels(selectedChatEngine)[0] ?? "default";
    else selectedRunModel = harnessModels(selectedRunEngine)[0] ?? "default";
  }

  async function saveHarness(target: "chat" | "run") {
    harnessBusy = `${target}:save`;
    error = "";
    try {
      applyHarnessCatalog(await invoke<HarnessCatalog>("configure_onboarding_harness", {
        request: {
          target,
          engine: target === "chat" ? selectedChatEngine : selectedRunEngine,
          model: target === "chat" ? selectedChatModel : selectedRunModel,
        },
      }));
    } catch (reason) {
      error = String(reason);
    } finally {
      harnessBusy = "";
    }
  }

  function harnessSurfaceLabel(surface: HarnessSurface): string {
    return ({
      vibedev: "VibeDev",
      chat: "Chat",
      agentic_runs: "Agentic + scheduled runs",
      background_model_calls: "Background model calls",
    })[surface];
  }

  async function saveSecret(step: PlanStep, secret: SecretPrompt) {
    const value = secretValues[secret.variable]?.trim() ?? "";
    if (!value) return;
    busyStep = `${step.id}:${secret.variable}`;
    error = "";
    try {
      plan = await invoke<Plan>("save_onboarding_secret", {
        request: { key: secret.variable, value },
      });
      secretValues = { ...secretValues, [secret.variable]: "" };
    } catch (reason) {
      error = String(reason);
    } finally {
      busyStep = "";
    }
  }

  async function uploadConfigurationFile(step: PlanStep, file: File | undefined) {
    if (!file) return;
    const driver = step.setup?.driver;
    if (!driver || driver.kind !== "configuration_file") return;
    busyStep = `${step.id}:configuration-file`;
    error = "";
    try {
      if (file.size > driver.max_bytes) throw new Error("Configuration file is too large");
      plan = await invoke<Plan>("save_onboarding_configuration_file", {
        request: { component_id: step.id, content: await file.text() },
      });
    } catch (reason) {
      error = String(reason);
    } finally {
      busyStep = "";
    }
  }

  async function confirmManual(step: PlanStep, confirmed: boolean) {
    busyStep = step.id;
    error = "";
    try {
      plan = await invoke<Plan>("set_onboarding_confirmation", {
        request: { component_id: step.id, confirmed },
      });
    } catch (reason) {
      error = String(reason);
    } finally {
      busyStep = "";
    }
  }

  async function openTarget(target: string) {
    error = "";
    try { await invoke("open_onboarding_target", { target }); }
    catch (reason) { error = String(reason); }
  }

  async function saveSetupToken() {
    const token = setupToken.trim();
    if (!token || !catalog) return;
    setupAccessBusy = true;
    error = "";
    try {
      await invoke<SetupAccess>("save_onboarding_setup_token", {
        request: { token },
      });
      catalog = await invoke<Catalog>("get_onboarding_catalog");
      setupToken = "";
    } catch (reason) {
      error = String(reason);
    } finally {
      setupAccessBusy = false;
    }
  }

  async function clearSetupToken() {
    if (!catalog) return;
    setupAccessBusy = true;
    error = "";
    try {
      catalog.setup_access = await invoke<SetupAccess>("clear_onboarding_setup_token");
      catalog.skills = catalog.skills.map((skill) => skill.setup?.required
        ? { ...skill, setup: { ...skill.setup, ready: false, detail: "Setup access is required to verify write-only credentials" } }
        : skill);
      catalog = { ...catalog };
    } catch (reason) {
      error = String(reason);
    } finally {
      setupAccessBusy = false;
    }
  }

  function installedHere(skill: SkillItem): boolean {
    return catalog ? skill.installed_scopes.includes(catalog.current_scope) : false;
  }

  async function changeSkill(skill: SkillItem, install: boolean) {
    if (!catalog) return;
    skillBusy = skill.name;
    error = "";
    try {
      catalog = await invoke<Catalog>(install ? "install_onboarding_skill" : "uninstall_onboarding_skill", {
        request: { name: skill.name },
      });
    } catch (reason) {
      error = String(reason);
    } finally {
      skillBusy = "";
    }
  }

  async function saveSkillSecret(skill: SkillItem, key: string) {
    if (!catalog) return;
    const id = `${skill.name}:${key}`;
    const value = skillSecretValues[id]?.trim() ?? "";
    if (!value) return;
    skillBusy = id;
    error = "";
    try {
      catalog = await invoke<Catalog>("save_onboarding_skill_secret", {
        skill: { name: skill.name },
        request: { key, value },
      });
      skillSecretValues = { ...skillSecretValues, [id]: "" };
    } catch (reason) {
      error = String(reason);
    } finally {
      skillBusy = "";
    }
  }

  async function installComponent(step: PlanStep) {
    busyStep = step.id;
    error = "";
    try {
      let job = await invoke<InstallJob>("start_onboarding_component_install", {
        request: { component_id: step.id },
      });
      installJobs = { ...installJobs, [step.id]: job };
      while (job.phase === "queued" || job.phase === "running") {
        await new Promise((resolve) => setTimeout(resolve, 850));
        job = await invoke<InstallJob>("get_onboarding_component_install", { jobId: job.id });
        installJobs = { ...installJobs, [step.id]: job };
      }
      if (job.phase === "failed") throw new Error(job.error ?? "Installation failed verification");
      await refreshPlan();
    } catch (reason) {
      error = String(reason);
    } finally {
      busyStep = "";
    }
  }

  function managedDriver(setup: ResolvedSetup | undefined): Extract<SetupDriver, { kind: "managed_bot" }> | undefined {
    return setup?.driver.kind === "managed_bot" ? setup.driver : undefined;
  }

  function fieldValueKey(subject: string, field: SetupField): string {
    return `${subject}:${field.id}`;
  }

  function setupValues(subject: string, setup: ResolvedSetup): Record<string, string> {
    const driver = managedDriver(setup);
    return Object.fromEntries((driver?.fields ?? []).map((field) => [field.id, setupFieldValues[fieldValueKey(subject, field)] ?? ""]));
  }

  function requireSetupFields(subject: string, setup: ResolvedSetup) {
    const missing = (managedDriver(setup)?.fields ?? [])
      .filter((field) => field.required && !(setupFieldValues[fieldValueKey(subject, field)] ?? "").trim())
      .map((field) => field.label);
    if (missing.length) throw new Error(`Complete required setup fields: ${missing.join(", ")}`);
  }

  function clearSetupFields(subject: string, setup: ResolvedSetup) {
    const cleared = { ...setupFieldValues };
    for (const field of managedDriver(setup)?.fields ?? []) delete cleared[fieldValueKey(subject, field)];
    setupFieldValues = cleared;
  }

  function hasManagedPairing(step: PlanStep): boolean {
    return managedDriver(step.setup) !== undefined;
  }

  async function startPairing(step: PlanStep) {
    busyStep = step.id;
    error = "";
    try {
      if (!step.setup) throw new Error("This component has no managed setup flow");
      const driver = managedDriver(step.setup);
      if (driver?.fields?.length) {
        requireSetupFields(step.id, step.setup);
        pairing = { ...pairing, [step.id]: await invoke<PairingSnapshot>("configure_onboarding_component_auth", {
          request: { component_id: step.id, values: setupValues(step.id, step.setup) },
        }) };
        clearSetupFields(step.id, step.setup);
      }
      let snapshot = await invoke<PairingSnapshot>("start_onboarding_bot_pairing", {
        request: { component_id: step.id },
      });
      pairing = { ...pairing, [step.id]: snapshot };
      let openedUrl = "";
      for (let attempt = 0; attempt < 400 && !snapshot.ready; attempt += 1) {
        if (snapshot.login_url && snapshot.login_url !== openedUrl) {
          openedUrl = snapshot.login_url;
          await invoke("open_onboarding_target", { target: snapshot.login_url });
        }
        await new Promise((resolve) => setTimeout(resolve, 1_500));
        snapshot = await invoke<PairingSnapshot>("get_onboarding_bot_pairing", {
          request: { component_id: step.id },
        });
        pairing = { ...pairing, [step.id]: snapshot };
      }
      if (!snapshot.ready) {
        throw new Error("Pairing is still waiting. Leave this page open and try again when the account is connected.");
      }
      await refreshPlan();
    } catch (reason) {
      error = String(reason);
    } finally {
      busyStep = "";
    }
  }

  async function submitPairingInput(step: PlanStep) {
    const input = setupInputValues[step.id] ?? "";
    if (!input) return;
    authInputBusy = true;
    error = "";
    try {
      const snapshot = await invoke<PairingSnapshot>("submit_onboarding_bot_auth_input", {
        request: { component_id: step.id, input },
      });
      pairing = { ...pairing, [step.id]: snapshot };
      setupInputValues = { ...setupInputValues, [step.id]: "" };
    } catch (reason) {
      error = String(reason);
    } finally {
      authInputBusy = false;
    }
  }

  function hasManagedSkillAuth(skill: SkillItem): boolean {
    const kind = skill.auth?.setup?.driver.kind;
    return installedHere(skill) && (kind === "managed_bot" || kind === "governed_oauth");
  }

  async function startSkillAuth(skill: SkillItem) {
    const busyId = `${skill.name}:auth`;
    skillBusy = busyId;
    error = "";
    let openedUrl = "";
    try {
      const setup = skill.auth?.setup;
      if (!setup) throw new Error("This skill has no managed setup flow");
      const driver = managedDriver(setup);
      if (driver?.fields?.length) {
        requireSetupFields(skill.name, setup);
        skillAuth = { ...skillAuth, [skill.name]: await invoke<PairingSnapshot>("configure_onboarding_skill_auth", {
          request: { name: skill.name, values: setupValues(skill.name, setup) },
        }) };
        clearSetupFields(skill.name, setup);
      }
      let snapshot = await invoke<PairingSnapshot>("start_onboarding_skill_auth", {
        request: { name: skill.name },
      });
      skillAuth = { ...skillAuth, [skill.name]: snapshot };
      for (let attempt = 0; attempt < 400 && !snapshot.ready; attempt += 1) {
        if (snapshot.login_url && snapshot.login_url !== openedUrl) {
          openedUrl = snapshot.login_url;
          await invoke("open_onboarding_target", { target: snapshot.login_url });
        }
        await new Promise((resolve) => setTimeout(resolve, 1_500));
        snapshot = await invoke<PairingSnapshot>("get_onboarding_skill_auth", {
          request: { name: skill.name },
        });
        skillAuth = { ...skillAuth, [skill.name]: snapshot };
      }
      if (!snapshot.ready) throw new Error("Account login is still waiting. Leave this page open and try again when it completes.");
      catalog = await invoke<Catalog>("get_onboarding_catalog");
    } catch (reason) {
      error = String(reason);
    } finally {
      skillBusy = "";
    }
  }

  async function submitSkillAuthInput(skill: SkillItem) {
    const input = setupInputValues[skill.name] ?? "";
    if (!input) return;
    const busyId = `${skill.name}:auth-input`;
    skillBusy = busyId;
    error = "";
    try {
      const snapshot = await invoke<PairingSnapshot>("submit_onboarding_skill_auth_input", {
        request: { name: skill.name, input },
      });
      skillAuth = { ...skillAuth, [skill.name]: snapshot };
      setupInputValues = { ...setupInputValues, [skill.name]: "" };
    } catch (reason) {
      error = String(reason);
    } finally {
      skillBusy = "";
    }
  }

  function skillBlockers(): SkillItem[] {
    return (catalog?.skills ?? []).filter((skill) =>
      installedHere(skill) && (
        skill.missing_bins.length > 0
        || (skill.setup?.required && !skill.setup.ready)
      ),
    );
  }

  function skillBlockerReason(skill: SkillItem): string {
    const reasons: string[] = [];
    if (skill.missing_bins.length) reasons.push(`missing programs: ${skill.missing_bins.join(", ")}`);
    if (skill.setup?.required && !skill.setup.ready) {
      reasons.push(`${skill.setup.detail}${skill.setup.missing.length ? ` (${skill.setup.missing.join(", ")})` : ""}`);
    }
    return reasons.join("; ");
  }

  function harnessSelectionBlockers(): string[] {
    if (!harnessCatalog) return ["Harness detection has not completed"];
    const blockers: string[] = [];
    for (const [target, engine] of [["Chat", harnessCatalog.chat_current], ["Agentic runs", harnessCatalog.run_current]] as const) {
      if (engine === "magician") continue;
      const harness = harnessCatalog.harnesses.find((item) => item.plane_engine === engine);
      if (!harness) blockers.push(`${target} uses ${engine}, which is not in the setup catalog`);
      else if (!harness.selectable) blockers.push(`${target} uses ${harness.label}, which is ${harness.readiness.replaceAll("_", " ")}`);
    }
    return blockers;
  }

  function completionBlocked(): boolean {
    if (maintenance) return false;
    return !plan?.ready || skillBlockers().length > 0 || harnessSelectionBlockers().length > 0;
  }

  async function finishOnboarding() {
    completing = true;
    error = "";
    try {
      await onComplete();
    } catch (reason) {
      const completionError = String(reason);
      catalog = await invoke<Catalog>("get_onboarding_catalog").catch(() => catalog);
      if (plan) await refreshPlan().catch(() => undefined);
      error = completionError;
    } finally {
      completing = false;
    }
  }

  function installTarget(step: PlanStep): string {
    return step.install.kind === "manual" ? step.install.open ?? "" : "";
  }

  function authLabel(skill: SkillItem): string {
    if (!skill.auth || skill.auth.kind === "none") return "No login";
    const subject = skill.auth.kind === "secrets"
      ? "API key"
      : skill.auth.kind === "cli_profile"
        ? "Account login"
        : skill.auth.kind === "oauth_session"
          ? "OAuth login"
          : skill.auth.kind === "native_permission"
            ? "System permission"
            : skill.auth.kind.replaceAll("_", " ");
    if (skill.auth.requirement === "conditional") return `${subject} when used`;
    if (skill.auth.requirement === "optional") return `Optional ${subject.toLowerCase()}`;
    if (skill.auth.requirement === "at_least_one") return `${subject} (choose one)`;
    return `${subject} required`;
  }

  function hasRequiredAuth(skill: SkillItem): boolean {
    return ["required", "at_least_one"].includes(skill.auth?.requirement ?? "none");
  }

  function filteredSkills(): SkillItem[] {
    const needle = query.trim().toLowerCase();
    return (catalog?.skills ?? []).filter((skill) => {
      if (skillKind !== "all" && skill.kind !== skillKind) return false;
      if (!needle) return true;
      return `${skill.name} ${skill.description} ${skill.auth?.provider ?? ""}`.toLowerCase().includes(needle);
    });
  }
</script>

<section class="onboarding-shell">
  {#if phase === "loading"}
    <div class="loading"><span class="spinner"></span>Reading capabilities and Skillshub from Magician…</div>
  {:else if phase === "failed"}
    <div class="result error" role="alert">
      <strong>Setup catalog unavailable</strong>
      <span>{error}</span>
      <button class="primary" type="button" onclick={loadCatalog}>Try again</button>
    </div>
  {:else if catalog}
    <div class="section-heading">
      <div>
        <p class="eyebrow">{maintenance ? "INSTALLATIONS" : "GUIDED SETUP"}</p>
        <h2>{phase === "skills" ? "Skillshub inventory" : phase === "harnesses" ? "External AI harnesses" : phase === "requirements" ? "Complete required setup" : "What should Magician do?"}</h2>
        <p>
          {phase === "skills"
            ? `${catalog.skills.length} skills are known to this Magician, with their binaries and login requirements.`
            : phase === "harnesses"
              ? "Detect operator-installed harnesses on the connected Magician host and choose which Ready harness drives Chat or agentic runs."
            : phase === "requirements"
              ? "Every selected capability must pass its probe before setup can finish."
              : "Choose outcomes. Magician resolves the packages, services, keys, logins, and host permissions they need."}
        </p>
      </div>
      <div class="setup-nav" aria-label="Setup sections">
        <button class="secondary compact" class:active={phase === "select"} type="button" onclick={() => (phase = "select")}>1 Capabilities</button>
        <button class="secondary compact" class:active={phase === "requirements"} type="button" onclick={buildPlan} disabled={checking}>2 Requirements</button>
        <button class="secondary compact" class:active={phase === "skills"} type="button" onclick={() => (phase = "skills")} disabled={!maintenance && !plan?.ready}>3 Skills ({catalog.skills.length})</button>
        <button class="secondary compact" class:active={phase === "harnesses"} type="button" onclick={showHarnesses} disabled={!maintenance && (!plan?.ready || skillBlockers().length > 0)}>4 Harnesses</button>
      </div>
    </div>

    {#if !catalog.setup_access.authorized}
      <div class="admin-access" role="group" aria-label="Server setup access">
        <div>
          <strong>Setup access required</strong>
          <p>Enter the setup token for this exact Magician server to install packages or write credentials. It is kept in the operating system credential store.</p>
          {#if catalog.setup_access.unavailable_reason}<small>{catalog.setup_access.unavailable_reason}</small>{/if}
        </div>
        <div class="admin-token-row">
          <input type="password" autocomplete="off" bind:value={setupToken} placeholder="Server setup token" aria-label="Server setup token" />
          <button class="secondary compact" type="button" onclick={saveSetupToken} disabled={setupAccessBusy || !setupToken.trim()}>{setupAccessBusy ? "Verifying…" : "Verify"}</button>
        </div>
      </div>
    {:else if maintenance}
      <div class="authorized-row"><span>✓ Setup access for this server</span><button class="text-button" type="button" onclick={clearSetupToken} disabled={setupAccessBusy}>Forget token</button></div>
    {/if}

    {#if phase === "select"}
      <div class="feature-grid">
        {#each catalog.components.graph.features as feature}
          {@const availability = currentAvailability(feature.id)}
          <label class="feature-card" class:selected={isSelected(feature.id)}>
            <input type="checkbox" checked={isSelected(feature.id)} onchange={() => toggleFeature(feature.id)} />
            <span class="feature-copy">
              <strong>{feature.name}</strong>
              <span>{feature.description}</span>
              <small class:available={availability?.state === "available"}>{availability?.state === "available" ? "Working now" : availability?.reason ?? "Setup required"}</small>
            </span>
          </label>
        {/each}
      </div>
      {#if error}<p class="inline-error" role="alert">{error}</p>{/if}
      <div class="actions">
        <button class="primary" type="button" onclick={buildPlan} disabled={checking}>{checking ? "Checking…" : "Review requirements"}</button>
      </div>
    {:else if phase === "requirements" && plan}
      <div class="readiness" class:ready={plan.ready}>
        <strong>{plan.ready ? "All selected capability requirements are ready" : `${plan.blocking_count} requirement ${plan.blocking_count === 1 ? "item" : "items"} remaining`}</strong>
        <button class="secondary compact" type="button" onclick={refreshPlan} disabled={checking}>{checking ? "Checking…" : "Check again"}</button>
      </div>

      {#if plan.unsupported.length || plan.unresolved.length}
        <div class="blocking-box" role="alert">
          {#each plan.unsupported as item}<p><strong>{item[0]}:</strong> {item[1]}</p>{/each}
          {#each plan.unresolved as item}<p>No package provides <strong>{item}</strong>.</p>{/each}
        </div>
      {/if}

      <div class="requirements-list">
        {#if plan.steps.length === 0}<p class="empty-state">No additional prerequisites are needed.</p>{/if}
        {#each plan.steps as step}
          {@const target = installTarget(step)}
          <article class="requirement" class:ready={step.ready}>
            <div class="requirement-title">
              <span class="status-icon">{step.ready ? "✓" : "!"}</span>
              <div><strong>{step.name}</strong><small>{step.required ? "Required" : step.pricing}</small></div>
            </div>
            <p>{step.observed.detail}</p>
            {#if step.cost}<p class="cost">{step.cost}</p>{/if}

            {#if step.setup?.driver.kind === "model_runtime" && modelRuntime[step.id]}
              {@const runtime = modelRuntime[step.id]}
              <fieldset class="model-runtime">
                <legend>Where should privacy-sensitive processing run?</legend>
                <div class="mode-options">
                  {#each runtime.modes as mode}
                    <label class="mode-option" class:selected={modelModes[step.id] === mode.id}>
                      <input
                        type="radio"
                        name={`processing-mode-${step.id}`}
                        value={mode.id}
                        checked={modelModes[step.id] === mode.id}
                        onchange={() => (modelModes = { ...modelModes, [step.id]: mode.id })}
                      />
                      <span><strong>{mode.label}</strong><small>{mode.description}</small></span>
                    </label>
                  {/each}
                </div>
                {#if selectedMode(step)?.local_generation}
                  <div class="model-options">
                    <strong>Choose one local model</strong>
                    {#each runtime.models as model}
                      <label class="model-option" class:selected={modelSelections[step.id] === model.id}>
                        <input
                          type="radio"
                          name={`local-model-${step.id}`}
                          value={model.id}
                          checked={modelSelections[step.id] === model.id}
                          onchange={() => (modelSelections = { ...modelSelections, [step.id]: model.id })}
                        />
                        <span>
                          <strong>{model.label}{model.recommended ? " · Recommended" : ""}</strong>
                          <small>{model.disk_gb ?? "?"} GB download · about {model.resident_gb ?? "?"} GB RAM · {model.installed ? "already installed" : "download required"}</small>
                          {#if model.notes}<small>{model.notes}</small>{/if}
                          {#each model.warnings as warning}<small class="model-warning">{warning}</small>{/each}
                        </span>
                      </label>
                    {/each}
                  </div>
                {/if}
                {#each runtime.warnings as warning}<small class="model-warning">{warning}</small>{/each}
                <p class="processing-note">The PPLX memory embedder is compulsory in both modes and stays on the Magician machine.</p>
                <button class="secondary compact" type="button" disabled={!catalog.setup_access.authorized || busyStep === `${step.id}:model-runtime`} onclick={() => saveModelRuntime(step)}>{busyStep === `${step.id}:model-runtime` ? "Saving…" : runtime.configured ? "Save processing choice" : "Confirm processing choice"}</button>
              </fieldset>
            {/if}

            {#if step.setup?.driver.kind === "cua_driver"}
              {#if step.install.kind === "manual"}<ol>{#each step.install.steps as instruction}<li>{instruction}</li>{/each}</ol>{/if}
              <div class="extension-actions">
                <button class="primary compact" type="button" disabled={busyStep === `${step.id}:cua-driver`} onclick={() => installCuaDriver(step)}>{busyStep === `${step.id}:cua-driver` ? "Installing and checking…" : step.ready ? "Verify again" : "Install, start and verify"}</button>
                <button class="secondary compact" type="button" disabled={checking} onclick={refreshPlan}>{checking ? "Checking…" : "Check again"}</button>
              </div>
              {#if cuaSetupResults[step.id]}
                <div class="pairing-panel" aria-live="polite">
                  <strong>{cuaSetupResults[step.id].ready ? "Computer use is ready" : cuaSetupResults[step.id].installed ? "CuaDriver needs desktop access" : "CuaDriver installation is incomplete"}</strong>
                  <p>{cuaSetupResults[step.id].detail}</p>
                  {#if cuaSetupResults[step.id].installer_output}<pre class="pairing-log">{cuaSetupResults[step.id].installer_output}</pre>{/if}
                </div>
              {/if}
            {:else if step.setup?.driver.kind === "browser_extension"}
              <ol>
                <li>Desktop copies the bundled unpacked extension into your Downloads folder and opens it.</li>
                <li>On Chrome's Extensions page, turn on <strong>Developer mode</strong>.</li>
                <li>Choose <strong>Load unpacked</strong> and select the opened <strong>Magican Browser Extension</strong> folder.</li>
                <li>Keep Chrome running while Magician uses your tabs.</li>
              </ol>
              <div class="extension-actions">
                <button class="primary compact" type="button" disabled={busyStep === `${step.id}:browser-extension`} onclick={() => prepareBrowserExtension(step)}>{busyStep === `${step.id}:browser-extension` ? "Opening…" : "Open extension installer"}</button>
                <button class="secondary compact" type="button" disabled={checking} onclick={refreshPlan}>{checking ? "Checking…" : "Verify connection"}</button>
              </div>
              {#if extensionPreparations[step.id]}
                <p class="command-hint">Load unpacked from <code>{extensionPreparations[step.id].source_path}</code></p>
              {/if}
            {:else if step.install.kind === "manual"}
              <ol>{#each step.install.steps as instruction}<li>{instruction}</li>{/each}</ol>
              {#if target}<button class="secondary compact" type="button" onclick={() => openTarget(target)}>Open setup page</button>{/if}
              {#each step.install.secrets ?? [] as secret}
                <div class="secret-row">
                  <label for={`secret-${step.id}-${secret.variable}`}>{secret.label}</label>
                  <input id={`secret-${step.id}-${secret.variable}`} type={secret.variable.includes("EMAIL") ? "email" : "password"} autocomplete="off" placeholder={secret.variable} value={secretValues[secret.variable] ?? ""} oninput={(event) => (secretValues = { ...secretValues, [secret.variable]: event.currentTarget.value })} />
                  <button class="secondary compact" type="button" disabled={busyStep === `${step.id}:${secret.variable}`} onclick={() => saveSecret(step, secret)}>{busyStep === `${step.id}:${secret.variable}` ? "Saving…" : "Save"}</button>
                </div>
              {/each}
              {#each managedDriver(step.setup)?.fields ?? [] as field}
                <div class="secret-row">
                  <label for={`setup-field-${step.id}-${field.id}`}>{field.label}{field.required ? " *" : ""}</label>
                  <input id={`setup-field-${step.id}-${field.id}`} type={field.kind} autocomplete="off" placeholder={field.placeholder ?? ""} value={setupFieldValues[fieldValueKey(step.id, field)] ?? ""} oninput={(event) => (setupFieldValues = { ...setupFieldValues, [fieldValueKey(step.id, field)]: event.currentTarget.value })} />
                  {#if field.help}<small>{field.help}</small>{/if}
                </div>
              {/each}
              {#if step.setup?.driver.kind === "configuration_file"}
                <div class="oauth-upload">
                  <label for={`configuration-file-${step.id}`}>{step.setup.driver.field_label}</label>
                  <input id={`configuration-file-${step.id}`} type="file" accept={step.setup.driver.accept} disabled={!catalog.setup_access.authorized || busyStep === `${step.id}:configuration-file`} onchange={(event) => uploadConfigurationFile(step, event.currentTarget.files?.[0])} />
                  <small>The file is stored on the connected Magician server and is never returned to Desktop.</small>
                </div>
              {/if}
              {#if hasManagedPairing(step)}
                <button class="primary compact pairing-button" type="button" disabled={busyStep === step.id} onclick={() => startPairing(step)}>{busyStep === step.id ? "Waiting for pairing…" : "Start account pairing"}</button>
                {#if pairing[step.id]}
                  <div class="pairing-panel" aria-live="polite">
                    <strong>{pairing[step.id].ready ? "Account connected" : `Pairing ${pairing[step.id].status.replaceAll("_", " ")}`}</strong>
                    {#if pairing[step.id].detail}<p>{pairing[step.id].detail}</p>{/if}
                    {#if pairing[step.id].qr_data_url}<img src={pairing[step.id].qr_data_url} alt={`${step.name} pairing QR code`} />{/if}
                    {#if managedDriver(step.setup)?.input && !pairing[step.id].ready}
                      {@const input = managedDriver(step.setup)?.input}
                      <div class="secret-row auth-input-row">
                        <label for={`setup-input-${step.id}`}>{input?.label}</label>
                        <input id={`setup-input-${step.id}`} type={input?.kind ?? "text"} autocomplete="off" placeholder={input?.placeholder ?? ""} value={setupInputValues[step.id] ?? ""} oninput={(event) => (setupInputValues = { ...setupInputValues, [step.id]: event.currentTarget.value })} />
                        <button class="secondary compact" type="button" disabled={authInputBusy || !(setupInputValues[step.id] ?? "")} onclick={() => submitPairingInput(step)}>{authInputBusy ? "Sending…" : "Send"}</button>
                        {#if input?.help}<small>{input.help}</small>{/if}
                      </div>
                    {/if}
                    {#if pairing[step.id].log}<pre class="pairing-log">{pairing[step.id].log}</pre>{/if}
                  </div>
                {/if}
              {/if}
            {:else if step.install.kind === "make"}
              <p class="command-hint">Installer action: <code>{step.install.target}</code>{#if step.install.script} via <code>{step.install.script}</code>{/if}</p>
              <button class="primary compact" type="button" disabled={!catalog.setup_access.authorized || busyStep === step.id} onclick={() => installComponent(step)}>{busyStep === step.id ? "Installing…" : "Install now"}</button>
              {#if installJobs[step.id]}
                <div class="install-progress" aria-live="polite">
                  <strong>{installJobs[step.id].phase === "running" ? "Installer running" : installJobs[step.id].phase}</strong>
                  {#if installJobs[step.id].output}<pre>{installJobs[step.id].output}</pre>{/if}
                </div>
              {/if}
            {:else}
              <p class="command-hint">This arrives with the Magician runtime. Recheck after the service starts.</p>
            {/if}

            {#if step.can_confirm}
              <label class="confirmation"><input type="checkbox" checked={step.confirmed} disabled={busyStep === step.id} onchange={(event) => confirmManual(step, event.currentTarget.checked)} /> I completed this step</label>
            {/if}
          </article>
        {/each}
      </div>
      {#if error}<p class="inline-error" role="alert">{error}</p>{/if}
      <div class="actions split">
        <button class="secondary" type="button" onclick={() => (phase = "select")}>Back</button>
        <button class="primary" type="button" disabled={!plan.ready} onclick={() => (phase = "skills")}>Review skills</button>
      </div>
    {:else if phase === "harnesses"}
      {#if harnessBusy === "catalog" && !harnessCatalog}
        <div class="loading"><span class="spinner"></span>Detecting harnesses on the connected Magician host…</div>
      {:else if harnessCatalog}
        <div class="harness-explanation">
          <strong>Magican never installs these programs.</strong>
          <span>Review readiness first. Install and sign in with the vendor outside Magican, then refresh detection here. Ready harnesses automatically appear in VibeDev.</span>
        </div>

        <div class="harness-list">
          {#each harnessCatalog.harnesses as harness}
            <article class="harness-card" class:ready={harness.selectable}>
              <div class="harness-title">
                <div><strong>{harness.label}</strong><small><code>{harness.binary}</code>{harness.version ? ` · ${harness.version}` : ""}</small></div>
                <span class="harness-status" class:ready={harness.selectable}>{harness.selectable ? "Ready" : harness.readiness.replaceAll("_", " ")}</span>
              </div>
              <p>{harness.reason}</p>
              <p>{harness.install_hint}</p>
              {#if harness.readiness === "disabled"}<p>Enable <code>{harness.config_key}</code> in this server's <code>magician-config.yaml</code>, restart or reload config, then refresh.</p>{/if}
              <div class="surface-chips">{#each harness.surfaces as surface}<span>{harnessSurfaceLabel(surface)}</span>{/each}</div>
              <div class="harness-actions">
                <button class="secondary compact" type="button" onclick={() => openTarget(harness.install_url)}>Open install guide</button>
                <button class="secondary compact" type="button" disabled={harnessBusy === `${harness.id}:refresh`} onclick={() => refreshHarness(harness)}>{harnessBusy === `${harness.id}:refresh` ? "Refreshing…" : "Refresh detection"}</button>
              </div>
            </article>
          {/each}
        </div>

        <div class="harness-switches">
          <form class="harness-switch" onsubmit={(event) => { event.preventDefault(); saveHarness("chat"); }}>
            <div><strong>Chat engine</strong><small>Chooses who reasons and writes ordinary Chat turns.</small></div>
            <select bind:value={selectedChatEngine} onchange={() => resetHarnessModel("chat")} aria-label="Chat harness">
              <option value="magician">Magician</option>
              {#each harnessCatalog.harnesses as harness}<option value={harness.plane_engine} disabled={!harness.selectable}>{harness.label}{harness.selectable ? "" : ` (${harness.readiness.replaceAll("_", " ")})`}</option>{/each}
            </select>
            <select bind:value={selectedChatModel} aria-label="Chat harness model">
              {#each harnessModels(selectedChatEngine) as model}<option value={model}>{model}</option>{/each}
            </select>
            <button class="primary compact" type="submit" disabled={harnessBusy === "chat:save"}>{harnessBusy === "chat:save" ? "Saving…" : "Save chat engine"}</button>
          </form>

          <form class="harness-switch" onsubmit={(event) => { event.preventDefault(); saveHarness("run"); }}>
            <div><strong>Agentic run engine</strong><small>Drives normal, scheduled, and autonomous agent loops.</small></div>
            <select bind:value={selectedRunEngine} onchange={() => resetHarnessModel("run")} aria-label="Agentic run harness">
              <option value="magician">Magician</option>
              {#each harnessCatalog.harnesses as harness}<option value={harness.plane_engine} disabled={!harness.selectable}>{harness.label}{harness.selectable ? "" : ` (${harness.readiness.replaceAll("_", " ")})`}</option>{/each}
            </select>
            <select bind:value={selectedRunModel} aria-label="Agentic run harness model">
              {#each harnessModels(selectedRunEngine) as model}<option value={model}>{model}</option>{/each}
            </select>
            <button class="primary compact" type="submit" disabled={harnessBusy === "run:save"}>{harnessBusy === "run:save" ? "Saving…" : "Save run engine"}</button>
          </form>
        </div>

        <p class="background-routing-note"><strong>Background model calls:</strong> Settings → Model routing controls each operation. Eligible calls can follow the Chat or run engine; an explicit operation choice wins. Privacy-local operations stay local.</p>

        {#if harnessSelectionBlockers().length}
          <div class="blocking-box" role="alert">
            <strong>Selected harnesses must be ready</strong>
            {#each harnessSelectionBlockers() as blocker}<p>{blocker}</p>{/each}
          </div>
        {/if}
      {/if}
      {#if error}<p class="inline-error" role="alert">{error}</p>{/if}
      <div class="actions split">
        <button class="secondary" type="button" onclick={() => (phase = "skills")}>Back to skills</button>
        <button class="primary" type="button" disabled={completing || completionBlocked()} onclick={finishOnboarding}>{completing ? "Verifying…" : maintenance ? "Done" : "Finish setup"}</button>
      </div>
    {:else if phase === "skills"}
      <p class="scope-line">Installing into <strong>{catalog.current_scope}</strong></p>
      <div class="skill-tools">
        <input type="search" bind:value={query} placeholder="Search skills" aria-label="Search skills" />
        <select bind:value={skillKind} aria-label="Skill type">
          <option value="all">All types</option><option value="tool">Tools</option><option value="procedure">Procedures</option><option value="personality">Personalities</option><option value="compiled">Built in</option>
        </select>
      </div>
      <div class="skills-list">
        {#each filteredSkills() as skill}
          <details class="skill-row">
            <summary>
              <span><strong>{skill.name}</strong><small>{skill.kind} {skill.version ? `· v${skill.version}` : ""}</small></span>
              <span class="auth-badge" class:required={hasRequiredAuth(skill)}>{authLabel(skill)}</span>
            </summary>
            <p>{skill.description}</p>
            {#if skill.requires_bins.length}<p><strong>Programs:</strong> {skill.requires_bins.join(", ")}{#if skill.missing_bins.length} · missing: {skill.missing_bins.join(", ")}{/if}</p>{/if}
            {#if skill.auth?.secret_refs.length}<p><strong>Credentials:</strong> {skill.auth.secret_refs.join(", ")}</p>{/if}
            {#if skill.install_hint}<p><strong>Setup:</strong> {skill.install_hint}</p>{/if}
            <p><strong>Installed:</strong> {skill.installed_scopes.length ? skill.installed_scopes.join(", ") : "No workspace scopes"}</p>
            {#if installedHere(skill) && skill.setup}
              <p class:skill-ready={skill.setup.ready} class:skill-missing={!skill.setup.ready}>
                <strong>{skill.setup.ready ? "Ready:" : "Setup required:"}</strong> {skill.setup.detail}{#if skill.setup.missing.length} — {skill.setup.missing.join(", ")}{/if}
              </p>
            {/if}
            <div class="skill-actions">
              {#if installedHere(skill)}
                <button class="secondary compact" type="button" disabled={!catalog.setup_access.authorized || skillBusy === skill.name} onclick={() => changeSkill(skill, false)}>{skillBusy === skill.name ? "Removing…" : "Remove from this workspace"}</button>
              {:else if skill.installable}
                <button class="primary compact" type="button" disabled={!catalog.setup_access.authorized || skillBusy === skill.name} onclick={() => changeSkill(skill, true)}>{skillBusy === skill.name ? "Installing…" : "Install in this workspace"}</button>
              {/if}
              {#if hasManagedSkillAuth(skill)}
                <button class="primary compact" type="button" disabled={skillBusy === `${skill.name}:auth`} onclick={() => startSkillAuth(skill)}>{skillBusy === `${skill.name}:auth` ? "Waiting for login…" : skill.setup?.ready ? "Reconnect account" : "Connect account"}</button>
              {/if}
            </div>
            {#each managedDriver(skill.auth?.setup)?.fields ?? [] as field}
              <div class="secret-row">
                <label for={`skill-auth-field-${skill.name}-${field.id}`}>{field.label}{field.required ? " *" : ""}</label>
                <input id={`skill-auth-field-${skill.name}-${field.id}`} type={field.kind} autocomplete="off" placeholder={field.placeholder ?? ""} value={setupFieldValues[fieldValueKey(skill.name, field)] ?? ""} oninput={(event) => (setupFieldValues = { ...setupFieldValues, [fieldValueKey(skill.name, field)]: event.currentTarget.value })} />
                {#if field.help}<small>{field.help}</small>{/if}
              </div>
            {/each}
            {#if skillAuth[skill.name]}
              <div class="pairing-panel" aria-live="polite">
                <strong>{skillAuth[skill.name].ready ? "Account connected" : `Login ${skillAuth[skill.name].status.replaceAll("_", " ")}`}</strong>
                {#if skillAuth[skill.name].detail}<p>{skillAuth[skill.name].detail}</p>{/if}
                {#if skillAuth[skill.name].qr_data_url}<img src={skillAuth[skill.name].qr_data_url} alt={`${skill.name} account pairing QR code`} />{/if}
                {#if managedDriver(skill.auth?.setup)?.input && !skillAuth[skill.name].ready}
                  {@const input = managedDriver(skill.auth?.setup)?.input}
                  <div class="secret-row auth-input-row">
                    <label for={`skill-auth-input-${skill.name}`}>{input?.label}</label>
                    <input id={`skill-auth-input-${skill.name}`} type={input?.kind ?? "text"} autocomplete="off" placeholder={input?.placeholder ?? ""} value={setupInputValues[skill.name] ?? ""} oninput={(event) => (setupInputValues = { ...setupInputValues, [skill.name]: event.currentTarget.value })} />
                    <button class="secondary compact" type="button" disabled={!catalog.setup_access.authorized || skillBusy === `${skill.name}:auth-input` || !(setupInputValues[skill.name] ?? "")} onclick={() => submitSkillAuthInput(skill)}>Send</button>
                    {#if input?.help}<small>{input.help}</small>{/if}
                  </div>
                {/if}
                {#if skillAuth[skill.name].log}<pre class="pairing-log">{skillAuth[skill.name].log}</pre>{/if}
              </div>
            {/if}
            {#if installedHere(skill) && skill.auth?.secret_refs.length}
              <div class="skill-secrets">
                {#each skill.auth.secret_refs as key}
                  {@const id = `${skill.name}:${key}`}
                  <div class="secret-row">
                    <label for={`skill-secret-${skill.name}-${key}`}>{key}</label>
                    <input id={`skill-secret-${skill.name}-${key}`} type="password" autocomplete="off" placeholder="Write-only credential" value={skillSecretValues[id] ?? ""} oninput={(event) => (skillSecretValues = { ...skillSecretValues, [id]: event.currentTarget.value })} />
                    <button class="secondary compact" type="button" disabled={!catalog.setup_access.authorized || skillBusy === id || !(skillSecretValues[id] ?? "").trim()} onclick={() => saveSkillSecret(skill, key)}>{skillBusy === id ? "Saving…" : "Save"}</button>
                  </div>
                {/each}
              </div>
            {/if}
          </details>
        {/each}
      </div>
      {#if error}<p class="inline-error" role="alert">{error}</p>{/if}
      {#if skillBlockers().length}
        <div class="blocking-box" role="alert">
          <strong>Installed skills need setup</strong>
          {#each skillBlockers() as skill}<p><strong>{skill.name}:</strong> {skillBlockerReason(skill)}</p>{/each}
        </div>
      {/if}
      <div class="actions split">
        <button class="secondary" type="button" onclick={() => (phase = plan ? "requirements" : "select")}>Back</button>
        <button class="primary" type="button" disabled={!maintenance && (!plan?.ready || skillBlockers().length > 0)} onclick={showHarnesses}>Review harnesses</button>
      </div>
    {/if}
  {/if}
</section>

<style>
  .onboarding-shell { display: grid; gap: 20px; }
  .loading { min-height: 220px; display: flex; align-items: center; justify-content: center; gap: 12px; color: var(--text-muted); }
  .spinner { width: 20px; height: 20px; border: 2px solid var(--border); border-top-color: var(--accent); border-radius: 50%; animation: spin .8s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  .section-heading { display: flex; justify-content: space-between; align-items: start; gap: 18px; }
  .section-heading h2 { margin: 3px 0 6px; font-size: 24px; }
  .section-heading p { margin: 0; color: var(--text-muted); line-height: 1.45; }
  .eyebrow { color: var(--accent) !important; font-size: 11px; font-weight: 750; letter-spacing: .13em; }
  .setup-nav { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 6px; }
  .setup-nav button.active { border-color: var(--accent); color: var(--accent); }
  .feature-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; max-height: 430px; overflow: auto; padding: 2px; }
  .feature-card { display: flex; gap: 11px; padding: 14px; border: 1px solid var(--border); border-radius: 12px; background: var(--bg-card); cursor: pointer; }
  .feature-card.selected { border-color: var(--accent); box-shadow: 0 0 0 1px color-mix(in srgb, var(--accent) 40%, transparent); }
  .feature-card input { margin-top: 3px; accent-color: var(--accent); }
  .feature-copy { display: grid; gap: 5px; min-width: 0; }
  .feature-copy > span { color: var(--text-muted); font-size: 12px; line-height: 1.35; }
  .feature-copy small { color: var(--warning, #b46a16); }
  .feature-copy small.available { color: var(--success, #27804a); }
  .actions { display: flex; justify-content: flex-end; gap: 10px; }
  .actions.split { justify-content: space-between; }
  .admin-access { display: grid; grid-template-columns: minmax(0, 1fr) minmax(260px, .8fr); gap: 14px; align-items: center; padding: 13px 14px; border: 1px solid color-mix(in srgb, var(--warning, #d58b2b) 40%, var(--border)); border-radius: 11px; background: color-mix(in srgb, var(--warning, #d58b2b) 8%, var(--bg-card)); }
  .admin-access p { margin: 4px 0; color: var(--text-muted); font-size: 12px; line-height: 1.4; }
  .admin-access small { color: var(--error); }
  .admin-token-row { display: grid; grid-template-columns: 1fr auto; gap: 8px; }
  .authorized-row, .scope-line { display: flex; align-items: center; justify-content: space-between; gap: 10px; margin: 0; color: var(--text-muted); font-size: 12px; }
  .text-button { border: 0; padding: 3px; color: var(--accent); background: transparent; font-size: 12px; }
  button { border-radius: 9px; padding: 10px 16px; font: inherit; font-weight: 650; cursor: pointer; }
  button:disabled { opacity: .5; cursor: default; }
  .primary { color: var(--accent-contrast, white); background: var(--accent); border: 1px solid var(--accent); }
  .secondary { color: var(--text); background: var(--bg-card); border: 1px solid var(--border); }
  .compact { padding: 7px 10px; font-size: 12px; }
  .readiness { display: flex; align-items: center; justify-content: space-between; gap: 14px; padding: 12px 14px; border-radius: 10px; color: var(--warning, #8a5417); background: color-mix(in srgb, var(--warning, #d58b2b) 12%, transparent); }
  .readiness.ready { color: var(--success, #267547); background: color-mix(in srgb, var(--success, #3c9b61) 12%, transparent); }
  .requirements-list { display: grid; gap: 10px; max-height: 430px; overflow: auto; }
  .requirement { padding: 15px; border: 1px solid var(--border); border-radius: 12px; background: var(--bg-card); }
  .requirement.ready { border-color: color-mix(in srgb, var(--success, #3c9b61) 45%, var(--border)); }
  .requirement-title { display: flex; align-items: center; gap: 10px; }
  .requirement-title > div { display: flex; align-items: baseline; gap: 8px; }
  .requirement-title small { color: var(--text-muted); text-transform: capitalize; }
  .status-icon { width: 22px; height: 22px; display: grid; place-items: center; border-radius: 50%; color: white; background: var(--warning, #bc741e); font-weight: 800; }
  .ready .status-icon { background: var(--success, #2d874f); }
  .requirement > p, .requirement li { color: var(--text-muted); font-size: 12px; line-height: 1.45; }
  .cost { padding-left: 12px; border-left: 2px solid var(--border); }
  .model-runtime { display: grid; gap: 10px; margin: 12px 0; padding: 12px; border: 1px solid var(--border); border-radius: 10px; }
  .model-runtime legend { padding: 0 5px; font-weight: 700; }
  .mode-options, .model-options { display: grid; gap: 7px; }
  .mode-option, .model-option { display: flex; align-items: start; gap: 9px; padding: 9px; border: 1px solid var(--border); border-radius: 9px; cursor: pointer; }
  .mode-option.selected, .model-option.selected { border-color: var(--accent); background: color-mix(in srgb, var(--accent) 7%, transparent); }
  .mode-option input, .model-option input { margin-top: 3px; accent-color: var(--accent); }
  .mode-option span, .model-option span { display: grid; gap: 3px; }
  .mode-option small, .model-option small, .processing-note { color: var(--text-muted); font-size: 11px; line-height: 1.4; }
  .model-warning { color: var(--warning, #8a5417) !important; }
  .processing-note { margin: 0; }
  .extension-actions { display: flex; flex-wrap: wrap; gap: 8px; }
  .harness-explanation { display: grid; gap: 4px; padding: 12px 14px; border: 1px solid var(--border); border-radius: 10px; background: var(--bg-card); }
  .harness-explanation span, .harness-switch small, .background-routing-note, .harness-card p { color: var(--text-muted); font-size: 12px; line-height: 1.45; }
  .harness-switches { display: grid; gap: 9px; }
  .harness-switch { display: grid; grid-template-columns: minmax(190px, 1fr) minmax(150px, .7fr) minmax(120px, .55fr) auto; align-items: end; gap: 8px; padding: 12px; border: 1px solid var(--border); border-radius: 10px; background: var(--bg-card); }
  .harness-switch > div { display: grid; gap: 3px; }
  .background-routing-note { margin: 0; padding: 10px 12px; border-left: 3px solid var(--accent); }
  .harness-list { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; max-height: 430px; overflow: auto; }
  .harness-card { display: grid; align-content: start; gap: 8px; padding: 13px; border: 1px solid var(--border); border-radius: 11px; background: var(--bg-card); }
  .harness-card.ready { border-color: color-mix(in srgb, var(--success, #3c9b61) 45%, var(--border)); }
  .harness-card p { margin: 0; }
  .harness-title { display: flex; align-items: start; justify-content: space-between; gap: 10px; }
  .harness-title > div { display: grid; gap: 3px; }
  .harness-title small { color: var(--text-muted); }
  .harness-status { padding: 4px 7px; border-radius: 999px; color: var(--warning, #8a5417); background: color-mix(in srgb, var(--warning, #d58b2b) 12%, transparent); font-size: 10px; text-transform: capitalize; }
  .harness-status.ready { color: var(--success, #267547); background: color-mix(in srgb, var(--success, #3c9b61) 12%, transparent); }
  .surface-chips, .harness-actions { display: flex; flex-wrap: wrap; gap: 6px; }
  .surface-chips span { padding: 4px 7px; border-radius: 999px; color: var(--text-muted); background: var(--bg); font-size: 10px; }
  .secret-row { display: grid; grid-template-columns: minmax(120px, .7fr) minmax(180px, 1fr) auto; align-items: center; gap: 8px; margin-top: 9px; }
  .oauth-upload { display: grid; gap: 6px; margin-top: 10px; padding: 10px; border: 1px solid var(--border); border-radius: 9px; background: var(--bg); }
  .oauth-upload small { color: var(--text-muted); }
  input, select { box-sizing: border-box; border: 1px solid var(--border); border-radius: 8px; background: var(--bg-input, var(--bg)); color: var(--text); padding: 9px 10px; font: inherit; }
  .confirmation { display: flex; align-items: center; gap: 8px; margin-top: 12px; font-size: 12px; font-weight: 650; }
  .command-hint code { color: var(--text); }
  .install-progress { display: grid; gap: 6px; margin-top: 10px; color: var(--text-muted); font-size: 12px; }
  .install-progress pre { box-sizing: border-box; max-height: 150px; overflow: auto; margin: 0; padding: 9px; border-radius: 7px; color: var(--text); background: var(--bg); white-space: pre-wrap; overflow-wrap: anywhere; font: 11px/1.4 ui-monospace, SFMono-Regular, Menlo, monospace; }
  .pairing-button { margin-top: 10px; }
  .pairing-panel { display: grid; justify-items: start; gap: 8px; margin-top: 10px; padding: 10px; border-radius: 9px; background: var(--bg); }
  .pairing-panel > p { margin: 0; }
  .pairing-panel img { display: block; width: min(280px, 100%); aspect-ratio: 1; object-fit: contain; background: white; padding: 8px; border-radius: 8px; }
  .auth-input-row { width: 100%; }
  .pairing-log { box-sizing: border-box; width: 100%; max-height: 260px; overflow: auto; margin: 0; padding: 9px; color: var(--text); background: var(--bg-card); white-space: pre; font: 10px/1 ui-monospace, SFMono-Regular, Menlo, monospace; }
  .blocking-box, .inline-error, .result.error { color: var(--error); background: color-mix(in srgb, var(--error) 9%, transparent); border: 1px solid color-mix(in srgb, var(--error) 35%, transparent); border-radius: 10px; padding: 12px; }
  .blocking-box p { margin: 4px 0; }
  .skill-tools { display: grid; grid-template-columns: 1fr 160px; gap: 10px; }
  .skills-list { display: grid; gap: 8px; max-height: 470px; overflow: auto; }
  .skill-row { border: 1px solid var(--border); border-radius: 10px; background: var(--bg-card); padding: 11px 13px; }
  .skill-row summary { display: flex; align-items: center; justify-content: space-between; gap: 12px; cursor: pointer; }
  .skill-row summary > span:first-child { display: grid; gap: 2px; }
  .skill-row summary small, .skill-row p { color: var(--text-muted); font-size: 12px; }
  .skill-actions { display: flex; justify-content: flex-end; margin-top: 10px; }
  .skill-secrets { display: grid; gap: 7px; padding-top: 10px; border-top: 1px solid var(--border); margin-top: 10px; }
  .auth-badge { white-space: nowrap; font-size: 11px; padding: 4px 7px; border-radius: 999px; color: var(--text-muted); background: var(--bg); }
  .auth-badge.required { color: var(--warning, #8a5417); background: color-mix(in srgb, var(--warning, #d58b2b) 12%, transparent); }
  .skill-ready { color: var(--success); }
  .skill-missing { color: var(--warning, #8a5417); }
  .empty-state { text-align: center; color: var(--text-muted); padding: 30px; }
  .result { display: grid; justify-items: start; gap: 10px; }
  @media (max-width: 860px) { .harness-switch { grid-template-columns: 1fr 1fr; } }
  @media (max-width: 680px) { .feature-grid, .admin-access, .harness-list, .harness-switch { grid-template-columns: 1fr; } .secret-row { grid-template-columns: 1fr; } .section-heading { display: grid; } .setup-nav { justify-content: flex-start; } }
</style>
