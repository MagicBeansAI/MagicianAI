<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount, onDestroy } from "svelte";
  import PermissionChecklist from "./PermissionChecklist.svelte";
  import MacosAppPairing from "./MacosAppPairing.svelte";
  import AndroidAppsAuthority from "./AndroidAppsAuthority.svelte";
  import AppMemoryContributions from "./AppMemoryContributions.svelte";
  import { HOST_APP_NAME, PRODUCT_NAME } from "./presentationIdentity.generated.js";
  import { shortcutIntent } from "../orb/orbUiModel.js";
  import {
    MAGICIAN_REALTIME_WEBSOCKET_PROTOCOL,
    magicianFetch,
    magicianWebSocketProtocols,
    magicianConnectionAuth,
    signInMagician,
    signOutMagician,
  } from "./magicianAuth.js";
  const desktopUserAgent = typeof navigator === "undefined" ? "" : navigator.userAgent;
  const isMacDesktop = /Macintosh|Mac OS X/i.test(desktopUserAgent);
  const isWindowsDesktop = /Windows/i.test(desktopUserAgent);
  const desktopDeviceLabel = isMacDesktop
    ? "Mac"
    : isWindowsDesktop
      ? "Windows PC"
      : "Linux device";

  interface StatusResponse {
    container_running: boolean;
    container_status: string;
    runtime_name: string;
    image: string;
    health: string;
    needs_setup: boolean;
    runtime_managed: boolean;
  }

  interface AggregatedHealth {
    magician: unknown;
    magicutor: unknown;
    container_running: boolean;
  }

  interface DesktopConfig {
    general: {
      launch_at_login: boolean;
      notifications_enabled: boolean;
      prevent_sleep: boolean;
      container_image: string;
      container_name: string;
      manage_runtime_stack: boolean;
      runtime_root: string;
      quick_overlay_shortcut: string;
      quick_overlay_gesture: string;
      screen_ask_shortcut: string;
      screen_clip_shortcut: string;
      screen_region_shortcut: string;
      screen_watch_shortcut: string;
    };
    contextual_assist: {
      enabled: boolean;
      show_on_selected_text: boolean;
      show_in_writable_fields: boolean;
      explicit_hotkey_only: boolean;
      default_personality: string;
      excluded_apps: string[];
    };
    container: {
      cpu_cores: number;
      memory_gb: number;
    };
    network: {
      magician_port: number;
      magicutor_port: number;
      engine_base_url: string;
    };
    voice: {
      voice_note_shortcut: string;
      voice_note_gesture: string;
      live_ptt_shortcut: string;
      default_thread_id: string;
      live_ptt_realtime_profile: string;
      retain_voice_note_audio: boolean;
      output_muted: boolean;
    };
    orb: {
      enabled: boolean;
      wake_enabled: boolean;
      voice_mode: "dictation" | "hands_free" | "realtime" | string;
      voice_mode_seeded: boolean;
      hotkey: string;
      leash_minutes: number;
      follow_up_seconds: number;
      wake_phrases: string[];
      armed_on_battery: boolean;
      auto_expand_on_wake: boolean;
      resting_x?: number | null;
      resting_y?: number | null;
      expanded_x?: number | null;
      expanded_y?: number | null;
    };
    updates: {
      auto_check: boolean;
      auto_update_container: boolean;
    };
  }

  type ConnectRouteBackend = "local" | "container" | "remote";

  interface ConnectRouteStatus {
    hostname: string;
    public_origin: string;
    selected_backend: string;
    local_url: string;
    local_health: string;
    container_url: string;
    container_health: string;
    remote_url: string | null;
    remote_health: string;
    management_available: boolean;
    management_message: string;
  }

  interface Props {
    status: StatusResponse | null;
  }

  interface VoiceRecordingTestResult {
    duration_ms: number;
    audio_bytes: number;
    samples_written: number;
    input_label: string;
    playback_ok: boolean;
    playback_duration_ms: number | null;
    playback_error: string | null;
    transcript: string | null;
    stt_model: string | null;
    language: string | null;
    transcription_error: string | null;
  }

  interface MediaProviderInfo {
    id: string;
    label?: string | null;
    model: string;
    voice?: string | null;
  }

  interface MediaProviderSnapshot {
    tts?: MediaProviderInfo | null;
    tts_fallbacks?: MediaProviderInfo[] | null;
    stt?: MediaProviderInfo | null;
    stt_fallbacks?: MediaProviderInfo[] | null;
    realtime_voice_profiles?: RealtimeVoiceProfileOption[];
    realtime_voice_default_profile?: string | null;
  }

  interface RealtimeVoiceProfileOption {
    profile_id: string;
    label: string;
    provider: string;
    model: string;
    topology: string;
    mode: string;
    available: boolean;
    unavailable_reason?: string | null;
  }

  interface EnvEntry {
    key: string;
    label: string;
    category: string;
    description: string;
    known: boolean;
    secret: boolean;
    present: boolean;
    empty: boolean;
    value_preview: string;
    requires_restart: boolean;
    config_path?: string | null;
  }

  interface EnvSnapshot {
    mode: string;
    path: string;
    exists: boolean;
    entries: EnvEntry[];
    restart_required: boolean;
  }

  interface EnvValueResponse {
    key: string;
    present: boolean;
    value: string;
  }

  interface ProviderSelectOption {
    id: string;
    label: string;
  }

  type SkillKind = "procedure" | "personality-mode" | "compiled" | string;

  interface SkillListEntry {
    name: string;
    description?: string;
    kind: SkillKind;
    layer?: string;
  }

  interface SkillListResponse {
    skills?: SkillListEntry[];
  }

  type SettingsTab = "settings" | "keyMappings" | "envMapping" | "environment";

  interface HotkeyMappingItem {
    id: string;
    label: string;
    description: string;
    trigger: string;
    source: string;
    status: string;
    active: boolean;
    note?: string | null;
  }

  interface HotkeyMappingGroup {
    id: string;
    label: string;
    items: HotkeyMappingItem[];
  }

  interface HotkeyMappingsResponse {
    updated_at_ms: number;
    groups: HotkeyMappingGroup[];
  }

  const DEFAULT_CONTEXTUAL_PERSONALITY_OPTIONS: ProviderSelectOption[] = [
    { id: "active", label: "Active personality" },
  ];
  const ENV_CATEGORY_ORDER = ["Provider Keys", "Runtime", "Meetings", "Host Helpers", "Advanced"];

  let { status }: Props = $props();

  let config = $state<DesktopConfig | null>(null);
  let saving = $state(false);
  let saveMessage = $state("");
  let containerRunning = $state(false);
  let runtimeName = $state("unknown");
  let runtimeManaged = $state(true);
  let healthLabel = $state("unknown");
  let actionInProgress = $state(false);
  let voiceTestRunning = $state(false);
  let voiceTestResult = $state<VoiceRecordingTestResult | null>(null);
  let voiceTestError = $state("");
  let mediaProviderSnapshot = $state<MediaProviderSnapshot | null>(null);
  let activeTab = $state<SettingsTab>("settings");
  let hotkeyMappings = $state<HotkeyMappingsResponse | null>(null);
  let hotkeyMappingsLoading = $state(false);
  let hotkeyMappingsError = $state("");
  let envSnapshot = $state<EnvSnapshot | null>(null);
  let envLoading = $state(false);
  let envMessage = $state("");
  let envError = $state("");
  let envRevealed = $state<Record<string, string>>({});
  let envEditing = $state<Record<string, string>>({});
  let envSavingKey = $state("");
  let newEnvKey = $state("");
  let newEnvValue = $state("");
  let contextualPersonalityOptions = $state<ProviderSelectOption[]>(
    DEFAULT_CONTEXTUAL_PERSONALITY_OPTIONS,
  );
  let contextualPersonalitiesLoading = $state(false);
  let contextualPersonalitiesError = $state("");
  let recordingOrbHotkey = $state(false);
  let webSettingsError = $state("");
  let connectRouteStatus = $state<ConnectRouteStatus | null>(null);
  let connectRouteTarget = $state<ConnectRouteBackend>("local");
  let connectRouteRemoteUrl = $state("");
  let connectRouteBusy = $state(false);
  let connectRouteMessage = $state("");
  let connectRouteError = $state("");
  let androidObservationApprovalVisible = $state(false);
  let androidObservationTrustMode = $state("owner_pinned_private_build");

  let showRestartConfirm = $state(false);
  let unlisteners: (() => void)[] = [];
  let mediaPreferencesSocket: WebSocket | null = null;
  let mediaPreferencesReconnect: ReturnType<typeof setTimeout> | null = null;
  let destroyed = false;

  async function consumeAndroidObservationApprovalRequest(): Promise<void> {
    try {
      const trustMode = await invoke<string | null>("take_android_observation_approval_request");
      if (trustMode) {
        androidObservationTrustMode = trustMode;
        androidObservationApprovalVisible = true;
      }
    } catch (error) {
      console.error("Failed to read Android observation approval request:", error);
    }
  }

  $effect(() => {
    containerRunning = status?.container_running ?? false;
    runtimeName = status?.runtime_name ?? "unknown";
    runtimeManaged = status?.runtime_managed ?? true;
    healthLabel = status?.health ?? "unknown";
  });

  onMount(async () => {
    try {
      config = await invoke<DesktopConfig>("get_config");
      ensureDesktopConnectionConfig(config);
      ensureContextualAssistConfig(config);
      ensureOrbConfig(config);
      await loadConnectRouteStatus(false);
      await loadMediaProviders();
      await loadMediaPreferences();
      await loadHotkeyMappings(false);
      await loadContextualPersonalities(false);
      await loadEnvironment();
      await loadSessionScope();
      connectMediaPreferencesSocket();
    } catch (e) {
      console.error("Failed to load config:", e);
    }

    const unlistenTray = await listen<string>("tray-state", (event) => {
      const state = event.payload;
      if (runtimeManaged) {
        containerRunning = state === "Running";
      }
      if (state === "Running") portConflicts = [];
    });
    unlisteners.push(unlistenTray);

    const unlistenHealth = await listen<AggregatedHealth>("health-status", (event) => {
      const health = event.payload;
      if (!runtimeManaged) {
        containerRunning = isHealthy(health.magician);
        healthLabel = externalHealthLabel(health);
      }
    });
    unlisteners.push(unlistenHealth);

    const unlistenRestart = await listen<void>("config-changed-restart-needed", () => {
      showRestartConfirm = true;
    });
    unlisteners.push(unlistenRestart);

    const unlistenSetup = await listen<void>("needs-setup", () => {
      uninstalled = true;
      containerRunning = false;
    });
    unlisteners.push(unlistenSetup);

    const unlistenMediaPreferences = await listen("media-preferences-updated", () => {
      applyMediaPreferencesUpdate();
    });
    unlisteners.push(unlistenMediaPreferences);

    const unlistenMediaConfig = await listen<void>("media-config-updated", () => {
      void loadMediaProviders();
    });
    unlisteners.push(unlistenMediaConfig);

    const unlistenHotkeyMappings = await listen<void>("hotkey-mappings-updated", () => {
      void loadHotkeyMappings(false);
    });
    unlisteners.push(unlistenHotkeyMappings);

    const unlistenPortConflict = await listen<PortCheckResult>("port-conflict", (event) => {
      portConflicts = event.payload.conflicts;
    });
    unlisteners.push(unlistenPortConflict);

    const unlistenAndroidObservation = await listen<string>(
      "open-android-observation-approval",
      (event) => {
        androidObservationTrustMode = event.payload;
        void consumeAndroidObservationApprovalRequest();
      },
    );
    unlisteners.push(unlistenAndroidObservation);
    await consumeAndroidObservationApprovalRequest();
  });

  onDestroy(() => {
    destroyed = true;
    if (recordingOrbHotkey) void invoke("orb_set_shortcut_recording", { recording: false });
    for (const fn_ of unlisteners) fn_();
    if (mediaPreferencesReconnect) clearTimeout(mediaPreferencesReconnect);
    mediaPreferencesSocket?.close();
  });

  async function saveSettings() {
    if (!config) return;
    saving = true;
    saveMessage = "";
    try {
      await invoke("save_config", { config });
      config = await invoke<DesktopConfig>("get_config");
      ensureDesktopConnectionConfig(config);
      ensureContextualAssistConfig(config);
      ensureOrbConfig(config);
      await loadMediaProviders();
      await loadMediaPreferences();
      await loadHotkeyMappings(false);
      await loadContextualPersonalities(false);
      saveMessage = "Settings saved";
      setTimeout(() => (saveMessage = ""), 3000);
    } catch (e) {
      saveMessage = `Error: ${e}`;
    } finally {
      saving = false;
    }
  }

  function ensureContextualAssistConfig(target: DesktopConfig | null) {
    if (!target) return;
    target.contextual_assist ??= {
      enabled: true,
      show_on_selected_text: true,
      show_in_writable_fields: true,
      explicit_hotkey_only: false,
      default_personality: "active",
      excluded_apps: [],
    };
    target.contextual_assist.enabled ??= true;
    target.contextual_assist.show_on_selected_text ??= true;
    target.contextual_assist.show_in_writable_fields ??= true;
    target.contextual_assist.explicit_hotkey_only ??= false;
    target.contextual_assist.default_personality ||= "active";
    target.contextual_assist.excluded_apps ??= [];
  }

  function ensureDesktopConnectionConfig(target: DesktopConfig | null) {
    if (!target) return;
    if (isWindowsDesktop) target.general.manage_runtime_stack = false;
    else target.general.manage_runtime_stack ??= true;
    target.general.runtime_root ??= "";
    target.network.engine_base_url ??= "";
  }

  async function loadConnectRouteStatus(preserveDraft: boolean) {
    connectRouteError = "";
    try {
      const next = await invoke<ConnectRouteStatus>("get_connect_route_status");
      connectRouteStatus = next;
      if (!preserveDraft) {
        connectRouteTarget = (["local", "container", "remote"].includes(next.selected_backend)
          ? next.selected_backend
          : "local") as ConnectRouteBackend;
        connectRouteRemoteUrl = next.remote_url ?? "";
      }
    } catch (error) {
      connectRouteError = String(error);
    }
  }

  async function applyConnectRoute() {
    connectRouteBusy = true;
    connectRouteMessage = "";
    connectRouteError = "";
    try {
      connectRouteStatus = await invoke<ConnectRouteStatus>("set_connect_route", {
        selection: {
          backend: connectRouteTarget,
          remote_url: connectRouteTarget === "remote" ? connectRouteRemoteUrl.trim() : null,
        },
      });
      connectRouteRemoteUrl = connectRouteStatus.remote_url ?? connectRouteRemoteUrl;
      connectRouteMessage = `Remote devices now reach ${connectRouteTarget === "local" ? "the backend on this computer" : connectRouteTarget === "container" ? "the local container" : "the remote backend"}.`;
    } catch (error) {
      connectRouteError = String(error);
    } finally {
      connectRouteBusy = false;
    }
  }

  function ensureOrbConfig(target: DesktopConfig | null) {
    if (!target) return;
    target.orb ??= {
      enabled: true,
      wake_enabled: false,
      voice_mode: "hands_free",
      voice_mode_seeded: false,
      hotkey: "Alt+Space",
      leash_minutes: 120,
      follow_up_seconds: 8,
      wake_phrases: ["hey assistant"],
      armed_on_battery: true,
      auto_expand_on_wake: true,
      resting_x: null,
      resting_y: null,
      expanded_x: null,
      expanded_y: null,
    };
    target.orb.voice_mode ??= "hands_free";
    target.orb.voice_mode_seeded ??= false;
    target.orb.wake_enabled ??= false;
    target.orb.wake_phrases ??= ["hey assistant"];
  }

  function markOrbVoiceModeSeeded() {
    if (!config) return;
    config.orb.voice_mode_seeded = true;
  }

  function captureOrbHotkey(event: KeyboardEvent) {
    if (!config) return;
    const intent = shortcutIntent(event);
    if (intent.kind === "navigate") return;
    event.preventDefault();
    event.stopPropagation();
    if (intent.kind === "cancel") {
      (event.currentTarget as HTMLInputElement).blur();
      return;
    }
    if (intent.kind === "clear") {
      config.orb.hotkey = "";
      return;
    }
    if (intent.kind === "modifier") return;
    if (intent.kind === "error") {
      saveMessage = "Use Command, Control, or Option in the orb shortcut.";
      return;
    }
    if (intent.kind !== "record" || !intent.value) return;
    config.orb.hotkey = intent.value;
    saveMessage = "";
    (event.currentTarget as HTMLInputElement).blur();
  }

  function setOrbHotkeyRecording(recording: boolean) {
    recordingOrbHotkey = recording;
    void invoke("orb_set_shortcut_recording", { recording }).catch((error) => {
      saveMessage = `Shortcut recorder error: ${error}`;
    });
  }

  function setOrbWakePhrases(value: string) {
    if (!config) return;
    config.orb.wake_phrases = value
      .split(/[\n,]+/)
      .map(phrase => phrase.trim())
      .filter(Boolean);
  }

  function parseExcludedApps(value: string): string[] {
    return value
      .split(",")
      .map((item) => item.trim())
      .filter(Boolean);
  }

  function setContextualAssistExcludedApps(value: string) {
    if (!config) return;
    config.contextual_assist.excluded_apps = parseExcludedApps(value);
  }

  async function openContextualAssistPreview() {
    try {
      await invoke("show_contextual_assist");
    } catch (e) {
      saveMessage = `Error: ${e}`;
    }
  }

  async function confirmRestart() {
    if (!config) return;
    showRestartConfirm = false;
    actionInProgress = true;
    try {
      await invoke("restart_with_new_config", { config });
      saveMessage = "Container restarted with new settings";
      setTimeout(() => (saveMessage = ""), 3000);
    } catch (e) {
      saveMessage = `Restart error: ${e}`;
    } finally {
      actionInProgress = false;
    }
  }

  function dismissRestart() {
    showRestartConfirm = false;
  }

  async function startContainer() {
    actionInProgress = true;
    saveMessage = "";
    try {
      await invoke("start_container");
    } catch (e) {
      saveMessage = `Start failed: ${e}`;
    } finally {
      actionInProgress = false;
    }
  }

  async function stopContainer() {
    actionInProgress = true;
    try {
      await invoke("stop_container");
    } catch (e) {
      console.error("Stop failed:", e);
    } finally {
      actionInProgress = false;
    }
  }

  async function restartContainer() {
    actionInProgress = true;
    saveMessage = "";
    try {
      await invoke("restart_container");
    } catch (e) {
      saveMessage = `Restart failed: ${e}`;
    } finally {
      actionInProgress = false;
    }
  }

  async function checkUpdates() {
    try {
      const result = await invoke("check_for_updates");
      console.log("Update check result:", result);
    } catch (e) {
      console.error("Update check failed:", e);
    }
  }

  async function runVoiceRecordingTest() {
    voiceTestRunning = true;
    voiceTestResult = null;
    voiceTestError = "";
    try {
      voiceTestResult = await invoke<VoiceRecordingTestResult>("run_voice_note_recording_test");
    } catch (e) {
      voiceTestError = `${e}`;
    } finally {
      voiceTestRunning = false;
    }
  }

  async function loadMediaProviders() {
    try {
      mediaProviderSnapshot = await invoke<MediaProviderSnapshot>("get_media_providers");
    } catch {
      mediaProviderSnapshot = null;
    }
  }

  // The Orb's Live PTT runs through Magician, so browser-only direct-WebRTC
  // profiles cannot serve it, and a translator is not a conversation. Order
  // is the backend's picker order (its `display_order`), the same the web
  // and phone pickers show.
  const liveEngineOptions = $derived(
    (mediaProviderSnapshot?.realtime_voice_profiles ?? []).filter(
      (option) => option.topology === "backend_proxied" && option.mode !== "translation"
    )
  );

  function liveEngineOptionLabel(option: RealtimeVoiceProfileOption): string {
    const isDefault = option.profile_id === mediaProviderSnapshot?.realtime_voice_default_profile;
    const suffix = option.available
      ? isDefault ? " · backend default" : ""
      : ` · unavailable${option.unavailable_reason ? `: ${option.unavailable_reason}` : ""}`;
    return `${option.label} (${option.model})${suffix}`;
  }

  async function loadMediaPreferences() {
    if (!config) return;
    try {
      // The fetch mirrors the shared voice mode into this Mac's startup cache.
      // Editing those preferences happens in web settings.
      await invoke("get_media_preferences");
    } catch (e) {
      console.warn("Failed to load backend media preferences:", e);
    }
    await adoptSharedVoiceModeMirror();
    await refreshAmbientOrbModeFromNativeConfig();
  }

  function applyMediaPreferencesUpdate() {
    void adoptSharedVoiceModeMirror();
    void refreshAmbientOrbModeFromNativeConfig();
    void loadHotkeyMappings(false);
  }

  // get_media_preferences writes voice.voice_mode onto disk. The settings form
  // keeps the whole config object, including that field, even though the form
  // does not edit it. Copy the mirrored value back so the next Save does not
  // replace the shared mode with the copy this window loaded earlier.
  async function adoptSharedVoiceModeMirror() {
    const target = config;
    if (!target) return;
    try {
      const native = await invoke<DesktopConfig>("get_config");
      if (config !== target) return;
      const mirrored = (native.voice as { voice_mode?: unknown }).voice_mode;
      if (typeof mirrored === "string" && mirrored.trim()) {
        (target.voice as { voice_mode?: string }).voice_mode = mirrored;
      }
    } catch (error) {
      console.warn("Failed to keep the shared voice-mode mirror:", error);
    }
  }

  async function refreshAmbientOrbModeFromNativeConfig() {
    const target = config;
    if (!target || target.orb.voice_mode_seeded) return;
    try {
      const nativeConfig = await invoke<DesktopConfig>("get_config");
      ensureOrbConfig(nativeConfig);
      if (
        config !== target
        || !nativeConfig.orb.voice_mode_seeded
        || target.orb.voice_mode_seeded
      ) return;
      target.orb.voice_mode = nativeConfig.orb.voice_mode;
      target.orb.voice_mode_seeded = true;
    } catch (error) {
      console.warn("Failed to refresh the device-local Ambient Orb mode:", error);
    }
  }

  async function loadHotkeyMappings(showSpinner = true) {
    if (showSpinner) hotkeyMappingsLoading = true;
    hotkeyMappingsError = "";
    try {
      hotkeyMappings = await invoke<HotkeyMappingsResponse>("get_hotkey_mappings");
    } catch (e) {
      hotkeyMappingsError = `Hotkey mappings unavailable: ${e}`;
    } finally {
      if (showSpinner) hotkeyMappingsLoading = false;
    }
  }

  function hotkeyLastUpdatedLabel(): string {
    if (!hotkeyMappings?.updated_at_ms) return "Not loaded";
    return new Date(hotkeyMappings.updated_at_ms).toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  }

  async function connectMediaPreferencesSocket() {
    if (destroyed) return;
    if (!config) return;
    if (mediaPreferencesSocket) {
      mediaPreferencesSocket.close();
      mediaPreferencesSocket = null;
    }
    if (mediaPreferencesReconnect) {
      clearTimeout(mediaPreferencesReconnect);
      mediaPreferencesReconnect = null;
    }

    const url = `${magicianApiBase()?.replace(/^http/, "ws")}/realtime/ws`;
    try {
      const socket = new WebSocket(
        url,
        await magicianWebSocketProtocols(url, [MAGICIAN_REALTIME_WEBSOCKET_PROTOCOL]),
      );
      mediaPreferencesSocket = socket;
      socket.onmessage = (event) => {
        try {
          const frame = JSON.parse(String(event.data)) as {
            event_type?: string;
            data?: { event?: { event_type?: string; payload?: unknown } };
          };
          const envelope = frame.event_type === "AgentEvent" ? frame.data?.event : null;
          if (envelope?.event_type === "media.config.updated") {
            void loadMediaProviders();
            return;
          }
          if (envelope?.event_type !== "media.preferences.updated") return;
          applyMediaPreferencesUpdate();
        } catch {
          // Ignore unrelated or malformed realtime frames.
        }
      };
      socket.onclose = () => {
        if (destroyed) return;
        if (mediaPreferencesSocket !== socket) return;
        mediaPreferencesSocket = null;
        mediaPreferencesReconnect = setTimeout(() => connectMediaPreferencesSocket(), 3000);
      };
      socket.onerror = () => {
        socket.close();
      };
    } catch {
      if (destroyed) return;
      mediaPreferencesReconnect = setTimeout(() => connectMediaPreferencesSocket(), 3000);
    }
  }

  function magicianApiBase(): string | null {
    if (!config) return null;
    const origin = config.network.engine_base_url?.trim().replace(/\/$/, "")
      || `http://127.0.0.1:${config.network.magician_port}`;
    return `${origin}/api/magician/v2`;
  }

  // The tray's Magician bearer, resolved server-side. With auth tightened,
  // every tray → backend call needs this to be live; surfacing the resolved
  // principal·workspace (or the absence of one) is the first diagnostic.
  let sessionScope = $state<{ identity: string; principal: string; workspace: string } | null>(null);
  let sessionScopeChecked = $state(false);
  let sessionOrigin = $state("");
  let sessionUsername = $state("");
  let sessionPassword = $state("");
  let sessionBusy = $state(false);
  let sessionError = $state("");

  async function signInDesktop(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    sessionBusy = true;
    sessionError = "";
    try {
      await signInMagician(sessionUsername, sessionPassword);
      await loadSessionScope();
      await invoke("restart_onboarding_for_current_engine");
      if (await invoke<boolean>("onboarding_completion_pending")) {
        await openSetup("capabilities");
      }
    } catch (error) {
      sessionError = String(error);
    } finally {
      sessionPassword = "";
      sessionBusy = false;
    }
  }

  async function signOutDesktop(): Promise<void> {
    sessionBusy = true;
    sessionError = "";
    try {
      await signOutMagician();
      await loadSessionScope();
    } catch (error) {
      sessionError = String(error);
    } finally {
      sessionBusy = false;
    }
  }

  async function loadSessionScope(): Promise<void> {
    sessionScopeChecked = false;
    const base = magicianApiBase();
    if (!base) {
      return;
    }
    try {
      sessionOrigin = (await magicianConnectionAuth()).origin;
      const response = await magicianFetch(`${base}/auth/session`);
      if (!response.ok) {
        sessionScope = null;
        return;
      }
      const session = await response.json() as {
        identity: { name: string };
        principal: string;
        workspace: string;
      };
      sessionScope = {
        identity: session.identity?.name ?? "—",
        principal: session.principal,
        workspace: session.workspace,
      };
    } catch {
      sessionScope = null;
    } finally {
      sessionScopeChecked = true;
    }
  }

  function formatPersonalityLabel(name: string): string {
    return name
      .replace(/[_-]+/g, " ")
      .split(" ")
      .filter(Boolean)
      .map((part) => part.slice(0, 1).toUpperCase() + part.slice(1))
      .join(" ");
  }

  function addPersonalityOption(
    options: ProviderSelectOption[],
    seen: Set<string>,
    option: ProviderSelectOption,
  ) {
    const id = option.id.trim();
    if (!id) return;
    const key = id.toLowerCase();
    if (seen.has(key)) return;
    seen.add(key);
    options.push({ id, label: option.label });
  }

  function normalizeContextualPersonalityOptions(
    skills: SkillListEntry[],
    selectedPersonality: string,
  ): ProviderSelectOption[] {
    const options: ProviderSelectOption[] = [];
    const seen = new Set<string>();
    for (const option of DEFAULT_CONTEXTUAL_PERSONALITY_OPTIONS) {
      addPersonalityOption(options, seen, option);
    }
    for (const skill of skills) {
      if (skill.kind !== "personality-mode") continue;
      addPersonalityOption(options, seen, {
        id: skill.name,
        label: formatPersonalityLabel(skill.name),
      });
    }
    if (selectedPersonality && !seen.has(selectedPersonality.toLowerCase())) {
      addPersonalityOption(options, seen, {
        id: selectedPersonality,
        label: `${formatPersonalityLabel(selectedPersonality)} (unavailable)`,
      });
    }
    return options;
  }

  async function loadContextualPersonalities(showSuccessMessage: boolean) {
    const base = magicianApiBase();
    if (!base) return;
    contextualPersonalitiesLoading = true;
    contextualPersonalitiesError = "";
    try {
      const response = await magicianFetch(`${base}/skills`, {
        headers: {
        },
      });
      if (!response.ok) throw new Error(`server returned ${response.status}`);
      const payload = await response.json() as SkillListResponse;
      contextualPersonalityOptions = normalizeContextualPersonalityOptions(
        payload.skills ?? [],
        config?.contextual_assist.default_personality ?? "active",
      );
      if (showSuccessMessage) {
        saveMessage = "Personalities refreshed";
        setTimeout(() => (saveMessage = ""), 2500);
      }
    } catch (e) {
      contextualPersonalitiesError = `Personality list unavailable: ${e}`;
      contextualPersonalityOptions = normalizeContextualPersonalityOptions(
        [],
        config?.contextual_assist.default_personality ?? "active",
      );
    } finally {
      contextualPersonalitiesLoading = false;
    }
  }

  async function loadEnvironment() {
    envLoading = true;
    envError = "";
    try {
      envSnapshot = await invoke<EnvSnapshot>("get_environment_snapshot");
    } catch (e) {
      envError = `Environment load failed: ${e}`;
    } finally {
      envLoading = false;
    }
  }

  function envKnownCategories(): string[] {
    if (!envSnapshot) return [];
    const categories = new Set(
      envSnapshot.entries
        .filter((entry) => entry.known)
        .map((entry) => entry.category),
    );
    return ENV_CATEGORY_ORDER.filter((category) => categories.has(category));
  }

  function envKnownEntryCount(): number {
    return envSnapshot?.entries.filter((entry) => entry.known).length ?? 0;
  }

  function envEntriesForCategory(category: string): EnvEntry[] {
    return envSnapshot?.entries.filter((entry) => entry.known && entry.category === category) ?? [];
  }

  function envMappingEntriesForCategory(category: string): EnvEntry[] {
    return envEntriesForCategory(category);
  }

  function envAdditionalEntries(): EnvEntry[] {
    return envSnapshot?.entries.filter((entry) => entry.present && !entry.known) ?? [];
  }

  function envConfigPath(entry: EnvEntry): string {
    const path = entry.config_path?.trim();
    return path && path.length > 0 ? path : "Env only";
  }

  function envMappingStatusLabel(entry: EnvEntry): string {
    if (entry.config_path) {
      if (entry.present && !entry.empty) return "Env override";
      if (entry.empty) return "Empty override";
      return "Config default";
    }
    if (!entry.present) return "Optional";
    if (entry.empty) return "Empty";
    return "Set";
  }

  function envValueForDisplay(entry: EnvEntry): string {
    if (envRevealed[entry.key] !== undefined) return envRevealed[entry.key];
    return entry.value_preview;
  }

  function envIsRevealed(key: string): boolean {
    return envRevealed[key] !== undefined;
  }

  async function revealEnvKey(key: string): Promise<string> {
    const response = await invoke<EnvValueResponse>("reveal_environment_value", { key });
    envRevealed = { ...envRevealed, [key]: response.value };
    return response.value;
  }

  function hideEnvKey(key: string) {
    const next = { ...envRevealed };
    delete next[key];
    envRevealed = next;
  }

  async function copyEnvKey(key: string) {
    try {
      const value = envRevealed[key] ?? await revealEnvKey(key);
      await navigator.clipboard.writeText(value);
      envMessage = `${key} copied`;
      setTimeout(() => (envMessage = ""), 2500);
    } catch (e) {
      envError = `Copy failed: ${e}`;
    }
  }

  async function startEnvEdit(entry: EnvEntry) {
    try {
      const value = envRevealed[entry.key] ?? await revealEnvKey(entry.key);
      envEditing = { ...envEditing, [entry.key]: value };
    } catch (e) {
      envError = `Edit failed: ${e}`;
    }
  }

  function cancelEnvEdit(key: string) {
    const next = { ...envEditing };
    delete next[key];
    envEditing = next;
  }

  async function saveEnvKey(key: string) {
    envSavingKey = key;
    envError = "";
    try {
      const value = envEditing[key] ?? "";
      envSnapshot = await invoke<EnvSnapshot>("save_environment_value", {
        request: { key, value },
      });
      envRevealed = { ...envRevealed, [key]: value };
      cancelEnvEdit(key);
      envMessage = `${key} saved`;
      setTimeout(() => (envMessage = ""), 2500);
    } catch (e) {
      envError = `Save failed: ${e}`;
    } finally {
      envSavingKey = "";
    }
  }

  async function removeEnvKey(key: string) {
    if (!window.confirm(`Remove ${key} from the active env file?`)) return;
    envSavingKey = key;
    envError = "";
    try {
      envSnapshot = await invoke<EnvSnapshot>("save_environment_value", {
        request: { key, value: null },
      });
      hideEnvKey(key);
      cancelEnvEdit(key);
      envMessage = `${key} removed`;
      setTimeout(() => (envMessage = ""), 2500);
    } catch (e) {
      envError = `Remove failed: ${e}`;
    } finally {
      envSavingKey = "";
    }
  }

  function envKeyIsValid(key: string): boolean {
    return /^[A-Za-z_][A-Za-z0-9_]*$/.test(key.trim());
  }

  async function addEnvKey() {
    const key = newEnvKey.trim();
    if (!envKeyIsValid(key)) {
      envError = "Environment variable names must use letters, numbers, and underscores.";
      return;
    }
    envSavingKey = key;
    envError = "";
    try {
      envSnapshot = await invoke<EnvSnapshot>("save_environment_value", {
        request: { key, value: newEnvValue },
      });
      envRevealed = { ...envRevealed, [key]: newEnvValue };
      newEnvKey = "";
      newEnvValue = "";
      envMessage = `${key} added`;
      setTimeout(() => (envMessage = ""), 2500);
    } catch (e) {
      envError = `Add failed: ${e}`;
    } finally {
      envSavingKey = "";
    }
  }

  function envStatusLabel(entry: EnvEntry): string {
    if (!entry.present) return "Missing";
    if (entry.empty) return "Empty";
    return "Set";
  }

  // -- Port conflicts --
  interface PortConflict {
    port: number;
    pid: number | null;
    process_name: string | null;
    is_own_container: boolean;
  }
  interface PortCheckResult {
    conflicts: PortConflict[];
  }
  let portConflicts = $state<PortConflict[]>([]);
  let freeing = $state(false);

  async function freeConflictedPorts() {
    const pids = portConflicts.filter((c) => c.pid).map((c) => c.pid!);
    if (pids.length === 0) return;
    freeing = true;
    try {
      const stillBlocked = await invoke<number[]>("free_ports", { pids });
      if (stillBlocked.length === 0) {
        portConflicts = [];
        // Auto-retry start
        await invoke("start_container");
      } else {
        portConflicts = portConflicts.filter((c) => stillBlocked.includes(c.port));
      }
    } catch (e) {
      console.error("Failed to free ports:", e);
    } finally {
      freeing = false;
    }
  }

  // -- Uninstall --
  let showUninstallConfirm = $state(false);
  let uninstallMode = $state("tools-and-data");
  let uninstalling = $state(false);
  let uninstallResult = $state("");
  let uninstalled = $state(false);

  function requestUninstall(mode: string) {
    uninstallMode = mode;
    showUninstallConfirm = true;
  }

  function dismissUninstall() {
    showUninstallConfirm = false;
    uninstallResult = "";
  }

  async function confirmUninstall() {
    showUninstallConfirm = false;
    uninstalling = true;
    uninstallResult = "";
    try {
      const result = await invoke<string>("uninstall", { mode: uninstallMode });
      uninstallResult = result;
      uninstalled = true;
    } catch (e) {
      uninstallResult = `Error: ${e}`;
    } finally {
      uninstalling = false;
    }
  }

  async function openSetup(mode?: "capabilities") {
    try {
      await invoke("open_setup", { mode: mode ?? null });
    } catch (e) {
      console.error("Failed to open setup:", e);
    }
  }

  async function openWebSettings(path: "/settings" | "/settings/model-routing") {
    webSettingsError = "";
    try {
      await invoke("open_app_at", { path });
    } catch (e) {
      webSettingsError = `Could not open web settings: ${e}`;
    }
  }

  function isHealthy(value: unknown): boolean {
    return value === "Healthy";
  }

  function externalHealthLabel(health: AggregatedHealth): string {
    if (isHealthy(health.magician) && isHealthy(health.magicutor)) return "healthy";
    if (isHealthy(health.magician) || isHealthy(health.magicutor)) return "partial";
    return health.container_running ? "starting" : "unreachable";
  }
</script>

<div class="settings-container">
  <header class="settings-header">
    <div class="header-left">
      <h1>{PRODUCT_NAME} Settings</h1>
      <div class="status-row">
        <span class="status-dot" class:running={containerRunning} class:stopped={!containerRunning}></span>
        <span class="status-text">
          {containerRunning ? "Running" : "Stopped"} &middot; {runtimeName}
          {#if !runtimeManaged}
            &middot; externally managed &middot; {healthLabel}
          {/if}
        </span>
      </div>
    </div>
    <div class="header-actions">
      <button class="secondary" onclick={() => openSetup("capabilities")}>Manage capabilities</button>
      <button class="secondary" onclick={() => openSetup()}>Change setup</button>
      {#if !runtimeManaged}
        <span class="external-runtime-pill">External engine</span>
      {:else if containerRunning}
        <button class="secondary" onclick={stopContainer} disabled={actionInProgress}>Stop</button>
        <button class="secondary" onclick={restartContainer} disabled={actionInProgress}>Restart</button>
      {:else}
        <button class="primary" onclick={startContainer} disabled={actionInProgress}>Start</button>
      {/if}
    </div>
  </header>

  {#if portConflicts.length > 0}
    <div class="port-conflict-banner">
      <p class="conflict-title">Port conflict detected</p>
      <ul class="conflict-list">
        {#each portConflicts as conflict}
          <li>
            Port <strong>{conflict.port}</strong> is used by
            <strong>{conflict.process_name ?? "unknown process"}</strong>
            {#if conflict.pid}(pid {conflict.pid}){/if}
          </li>
        {/each}
      </ul>
      <div class="conflict-actions">
        <button class="primary" onclick={freeConflictedPorts} disabled={freeing}>
          {freeing ? "Freeing..." : "Free Ports & Start"}
        </button>
        <span class="conflict-hint">or change ports in the Network section below</span>
      </div>
    </div>
  {/if}

  {#if config}
    {#if uninstalled}
      <div class="sections">
        <section class="card">
          <h2>Set Up {PRODUCT_NAME}</h2>
          {#if uninstallResult}
            <div class="uninstall-result">
              <pre>{uninstallResult}</pre>
            </div>
          {/if}
          <p class="section-desc">{PRODUCT_NAME} has been uninstalled. You can set it up again to re-detect your runtime, pull the container image, and start the backend service.</p>
          <button class="primary" onclick={() => openSetup()}>Set Up {PRODUCT_NAME}</button>
        </section>
      </div>
    {:else}
      <section class="card web-settings-card" aria-labelledby="web-settings-title">
        <div>
          <p class="settings-kicker">Shared settings</p>
          <h2 id="web-settings-title">{PRODUCT_NAME} settings on the web</h2>
          <p class="section-desc">
            Manage your account, workspace, trust rules, model routing, voice, notes,
            storage, and connected devices in the browser. Keep permissions, local runtime,
            shortcuts, the Orb, and other controls for this {desktopDeviceLabel} here.
          </p>
          {#if webSettingsError}
            <p class="inline-notice error" role="alert">{webSettingsError}</p>
          {/if}
        </div>
        <div class="web-settings-actions">
          <button class="primary" type="button" onclick={() => openWebSettings("/settings")}>
            Open Web Settings
          </button>
          <button class="secondary" type="button" onclick={() => openWebSettings("/settings/model-routing")}>
            Model Routing
          </button>
        </div>
      </section>

      <nav class="settings-tabs" aria-label="Settings sections">
        <button
          class:active={activeTab === "settings"}
          onclick={() => (activeTab = "settings")}
          type="button"
        >
          This {desktopDeviceLabel}
        </button>
        <button
          class:active={activeTab === "keyMappings"}
          onclick={() => (activeTab = "keyMappings")}
          type="button"
        >
          Key Mappings
        </button>
        <button
          class:active={activeTab === "envMapping"}
          onclick={() => (activeTab = "envMapping")}
          type="button"
        >
          Env Map
        </button>
        <button
          class:active={activeTab === "environment"}
          onclick={() => (activeTab = "environment")}
          type="button"
        >
          Environment
        </button>
      </nav>

      {#if activeTab === "settings"}
        <div class="settings-save-bar" role="toolbar" aria-label="Settings actions">
          <span
            class="save-message"
            class:visible={saveMessage !== ""}
            role="status"
            aria-live="polite"
          >{saveMessage}</span>
          <button class="primary" onclick={saveSettings} disabled={saving}>
            {saving ? "Saving..." : "Save Settings"}
          </button>
        </div>
        <div class="sections">
        <!-- Permissions -->
        <PermissionChecklist />

        {#if isMacDesktop}
          <!-- Owner-mediated typed macOS app observation -->
          <MacosAppPairing magicianPort={config.network.magician_port} />
        {/if}

        <!-- Which scope this tray's bearer operates in -->
        <section class="card">
          <h2>Signed-in scope</h2>
          {#if sessionOrigin}<p class="section-desc">Server: <code>{sessionOrigin}</code></p>{/if}
          {#if sessionScope}
            <p class="section-desc">
              Signed in as <strong>{sessionScope.identity}</strong> in workspace
              <strong>{sessionScope.workspace}</strong> — scope
              <code>{sessionScope.principal}/{sessionScope.workspace}</code>.
              This desktop session is saved securely for this server.
            </p>
            <button onclick={signOutDesktop} disabled={sessionBusy}>Sign out of desktop</button>
          {:else if sessionScopeChecked}
            <p class="section-desc">
              Sign in to connect desktop features to this server. Your session is
              saved in the system credential store; your password is not saved.
            </p>
            <form onsubmit={signInDesktop}>
              <div class="field-row">
                <div class="field">
                  <label for="desktop-session-username">Username</label>
                  <input id="desktop-session-username" autocomplete="username" bind:value={sessionUsername} required disabled={sessionBusy} />
                </div>
                <div class="field">
                  <label for="desktop-session-password">Password</label>
                  <input id="desktop-session-password" type="password" autocomplete="current-password" bind:value={sessionPassword} required disabled={sessionBusy} />
                </div>
              </div>
              <button type="submit" class="primary" disabled={sessionBusy}>{sessionBusy ? "Signing in…" : "Sign in to desktop"}</button>
            </form>
          {:else}
            <p class="section-desc">Resolving session…</p>
          {/if}
          {#if sessionError}<p role="alert">{sessionError}</p>{/if}
        </section>

        <!-- Owner-mediated app memory promotion -->
        <AppMemoryContributions magicianPort={config.network.magician_port} />

        {#if isMacDesktop}
          <!-- iMessage is a macOS-only host capability. -->
          <section class="card">
            <h2>iMessage access</h2>
            <p class="section-desc">
              Reading your iMessage history needs <strong>Full Disk Access</strong> on {HOST_APP_NAME}
              background process; sending messages needs <strong>Automation → Messages</strong> on this
              desktop app.
            </p>
            <PermissionChecklist only={["full_disk_access", "automation_messages"]} />
          </section>
        {/if}

        <!-- Network -->
        <section class="card">
          <h2>Network</h2>
          <div class="field">
            <label for="engine-base-url">Backend URL</label>
            <input
              id="engine-base-url"
              type="url"
              bind:value={config.network.engine_base_url}
              placeholder={`http://127.0.0.1:${config.network.magician_port}`}
              spellcheck="false"
            />
            <p class="field-help">
              Leave blank for the local backend, or enter the HTTPS URL of an approved remote backend. Desktop credentials remain isolated per server origin.
            </p>
          </div>
          <div class="connect-route-panel">
            <div class="connect-route-heading">
              <div>
                <strong>Remote device route</strong>
                <p class="field-help">
                  Phones using Remote keep the stable endpoint <code>{connectRouteStatus?.public_origin ?? "connect.<your-zone>"}</code>. Choose which healthy Magician backend receives its API traffic.
                </p>
              </div>
              {#if connectRouteStatus}
                <span class="route-pill">Active: {connectRouteStatus.selected_backend}</span>
              {/if}
            </div>
            <div class="field">
              <label for="connect-route-target">Remote traffic target</label>
              <select id="connect-route-target" bind:value={connectRouteTarget} disabled={connectRouteBusy}>
                <option value="local">Backend on this computer</option>
                <option value="container">Local backend container</option>
                <option value="remote">Remote backend server or container</option>
              </select>
            </div>
            {#if connectRouteTarget === "remote"}
              <div class="field">
                <label for="connect-route-remote-url">Remote backend origin</label>
                <input
                  id="connect-route-remote-url"
                  type="url"
                  bind:value={connectRouteRemoteUrl}
                  placeholder="https://engine.example.com"
                  autocomplete="url"
                  spellcheck="false"
                  disabled={connectRouteBusy}
                />
                <p class="field-help">
                  Enter the backend's direct HTTPS origin. Do not enter {connectRouteStatus?.public_origin ?? "the public connection URL"}, which would create a routing loop.
                </p>
              </div>
            {/if}
            {#if connectRouteStatus}
              <div class="connect-route-health" aria-label="Remote device route targets">
                <span>This computer <strong>{connectRouteStatus.local_health}</strong></span>
                <span>Local container <strong>{connectRouteStatus.container_health}</strong></span>
                <span>Remote server <strong>{connectRouteStatus.remote_health}</strong></span>
              </div>
              <p class="field-help">{connectRouteStatus.management_message}</p>
            {/if}
            {#if connectRouteMessage}<p class="inline-notice success" role="status">{connectRouteMessage}</p>{/if}
            {#if connectRouteError}<p class="inline-notice error" role="alert">{connectRouteError}</p>{/if}
            <div class="connect-route-actions">
              <button class="secondary compact" type="button" onclick={() => loadConnectRouteStatus(true)} disabled={connectRouteBusy}>Refresh</button>
              <button
                class="primary"
                type="button"
                onclick={applyConnectRoute}
                disabled={connectRouteBusy || !connectRouteStatus?.management_available || (connectRouteTarget === "remote" && !connectRouteRemoteUrl.trim())}
              >
                {connectRouteBusy ? "Verifying and switching…" : "Use this route"}
              </button>
            </div>
          </div>
          <div class="field-row">
            <div class="field">
              <label for="magician-port">Magician backend port</label>
              <input id="magician-port" type="number" bind:value={config.network.magician_port}
                     min="1024" max="65535" />
            </div>
            <div class="field">
              <label for="magicutor-port">Magicutor Port</label>
              <input id="magicutor-port" type="number" bind:value={config.network.magicutor_port}
                     min="1024" max="65535" />
            </div>
          </div>
        </section>

        <!-- Resources -->
        <section class="card">
          <h2>Resources</h2>
          <div class="field-row">
            <div class="field">
              <label for="cpu">CPU Cores</label>
              <input id="cpu" type="number" bind:value={config.container.cpu_cores}
                     min="1" max="16" step="0.5" />
            </div>
            <div class="field">
              <label for="memory">Memory (GB)</label>
              <input id="memory" type="number" bind:value={config.container.memory_gb}
                     min="1" max="64" />
            </div>
          </div>
        </section>

        <!-- Ambient Orb -->
        <section class="card orb-settings-card">
          <div class="orb-settings-heading">
            <div>
              <h2>Ambient Orb</h2>
              <p class="section-desc">A voice-aware presence on this {desktopDeviceLabel}. Saved changes apply immediately.</p>
            </div>
            <span class="orb-preview" aria-hidden="true"></span>
          </div>
          <div class="toggle-row">
            <label class="toggle-label">
              <input type="checkbox" bind:checked={config.orb.enabled} />
              Keep the Orb ready
            </label>
          </div>
          <div class="toggle-row orb-wake-toggle">
            <label class="toggle-label">
              <input type="checkbox" bind:checked={config.orb.wake_enabled} />
              Let the wake phrase summon the Orb
            </label>
            <p class="field-help">
              Off by default. Hold Left ⌥ to talk; a double-tap of the same key still opens Quick Automate. When this is on, the phrase works even while the Orb is hidden or listening is stopped. A recognized phrase summons and starts the Orb; it never opens the web composer.
            </p>
          </div>
          <div class="field-row">
            <div class="field">
              <label for="orb-voice-mode">Conversation Mode</label>
              <select
                id="orb-voice-mode"
                bind:value={config.orb.voice_mode}
                onchange={markOrbVoiceModeSeeded}
              >
                <option value="dictation">Dictation · record, understand, reply</option>
                <option value="hands_free">Hands-free · FluidAudio pipeline</option>
                <option value="realtime">Live · realtime provider</option>
              </select>
              <p class="field-help">Hold Left ⌥ to talk, then let go. Dictation records that hold. Hands-free and Live use the same hold. Live keeps its connection after you let go, so the next hold skips the first-time setup. This device-local choice is seeded once from backend voice settings and controls the Orb only.</p>
            </div>
          </div>
          <div class="field-row">
            <div class="field">
              <label for="orb-hotkey">Summon Shortcut</label>
              <input
                id="orb-hotkey"
                class:shortcut-recording={recordingOrbHotkey}
                type="text"
                value={recordingOrbHotkey ? "Press your shortcut…" : config.orb.hotkey}
                placeholder="Alt+Space"
                readonly
                onfocus={() => setOrbHotkeyRecording(true)}
                onblur={() => setOrbHotkeyRecording(false)}
                onkeydown={captureOrbHotkey}
              />
              <p class="field-help">Click, then press a chord. Delete clears it; Escape cancels. Default: ⌥Space. Saving rejects collisions.</p>
            </div>
            <div class="field">
              <label for="orb-leash">Listening Window</label>
              <select id="orb-leash" bind:value={config.orb.leash_minutes}>
                <option value={30}>30 minutes</option>
                <option value={60}>1 hour</option>
                <option value={120}>2 hours</option>
                <option value={240}>4 hours</option>
              </select>
            </div>
          </div>
          <div class="field-row">
            <div class="field">
              <label for="orb-phrases">Wake Phrases</label>
              <textarea
                id="orb-phrases"
                rows="2"
                value={config.orb.wake_phrases.join(", ")}
                oninput={(event) => setOrbWakePhrases(event.currentTarget.value)}
                placeholder="hey assistant"
                spellcheck="false"
              ></textarea>
              <p class="field-help">Comma or line separated. The first phrase drives the Orb-only native listener today.</p>
            </div>
            <div class="field">
              <label for="orb-follow-up">Follow-up Window (seconds)</label>
              <input id="orb-follow-up" type="number" min="1" max="60" bind:value={config.orb.follow_up_seconds} />
            </div>
          </div>
          <div class="toggle-grid">
            <label class="toggle-label">
              <input type="checkbox" bind:checked={config.orb.auto_expand_on_wake} />
              Arrive at center screen when listening starts, then settle to the side
            </label>
            <label class="toggle-label">
              <input type="checkbox" bind:checked={config.orb.armed_on_battery} />
              Keep wake listening ready on battery power
            </label>
          </div>
          <div class="settings-group">
            <div class="settings-group-header">
              <h3>Conversation Routing</h3>
            </div>
            <div class="field-row">
              <div class="field">
                <label for="voice-thread">Default Conversation Thread</label>
                <input
                  id="voice-thread"
                  type="text"
                  bind:value={config.voice.default_thread_id}
                  placeholder="general"
                  spellcheck="false"
                />
                <p class="field-help">Used when the Orb starts without an existing conversation context.</p>
              </div>
              <div class="field">
                <label for="live-profile">Realtime Voice Engine</label>
                {#if liveEngineOptions.length > 0}
                  <select id="live-profile" bind:value={config.voice.live_ptt_realtime_profile}>
                    {#if config.voice.live_ptt_realtime_profile && !liveEngineOptions.some((option) => option.profile_id === config?.voice.live_ptt_realtime_profile)}
                      <option value={config.voice.live_ptt_realtime_profile}>
                        {config.voice.live_ptt_realtime_profile} · no longer offered by the backend
                      </option>
                    {/if}
                    {#each liveEngineOptions as option (option.profile_id)}
                      <option value={option.profile_id} disabled={!option.available}>
                        {liveEngineOptionLabel(option)}
                      </option>
                    {/each}
                  </select>
                  <p class="field-help">The engine the Orb's Live conversation mode calls, fetched from the backend's realtime catalog. Greyed entries are missing a credential on the backend.</p>
                {:else}
                  <input
                    id="live-profile"
                    type="text"
                    bind:value={config.voice.live_ptt_realtime_profile}
                    placeholder="voice_realtime_openai_backend"
                    spellcheck="false"
                  />
                  <p class="field-help">The backend's realtime catalog could not be fetched, so this is the profile id as configured there.</p>
                {/if}
              </div>
            </div>
          </div>
        </section>

        <!-- Audio privacy and host-native diagnostics -->
        <section class="card">
          <h2>Audio Privacy &amp; Diagnostics</h2>
          <p class="section-desc">
            Control host-native playback and artifact retention, or verify the desktop microphone and speech helper.
          </p>
          <div class="settings-group">
            <div class="settings-group-header">
              <h3>Privacy &amp; Playback</h3>
            </div>
            <div class="toggle-grid">
              <label class="toggle-label">
                <input type="checkbox" bind:checked={config.voice.retain_voice_note_audio} />
                Retain raw recorded audio artifacts
              </label>
              <label class="toggle-label">
                <input type="checkbox" bind:checked={config.voice.output_muted} />
                Mute assistant audio output
              </label>
            </div>
            {#if isMacDesktop}
              <div class="voice-test-panel">
                <div class="voice-test-copy">
                  <p class="voice-test-title">macOS STT Record Test</p>
                  <p class="field-help">
                    Records 3 seconds in the tray app, plays the clip back locally, then transcribes it with the macOS Speech helper.
                  </p>
                </div>
                <button class="secondary" onclick={runVoiceRecordingTest} disabled={voiceTestRunning}>
                  {voiceTestRunning ? "Testing..." : "Record Test"}
                </button>
              </div>
              {#if voiceTestError}
                <div class="voice-test-result voice-test-error">
                  <p>{voiceTestError}</p>
                </div>
              {/if}
              {#if voiceTestResult}
                <div class="voice-test-result">
                  <div class="voice-test-grid">
                    <span>Input</span>
                    <strong>{voiceTestResult.input_label}</strong>
                    <span>Captured</span>
                    <strong>{Math.round(voiceTestResult.audio_bytes / 1024)} KB · {voiceTestResult.duration_ms} ms</strong>
                    <span>Playback</span>
                    <strong>
                      {voiceTestResult.playback_ok
                        ? `OK${voiceTestResult.playback_duration_ms ? ` · ${voiceTestResult.playback_duration_ms} ms` : ""}`
                        : voiceTestResult.playback_error}
                    </strong>
                    <span>STT</span>
                    <strong>
                      {voiceTestResult.transcription_error
                        ? voiceTestResult.transcription_error
                        : `${voiceTestResult.stt_model ?? "macOS Speech"}${voiceTestResult.language ? ` · ${voiceTestResult.language}` : ""}`}
                    </strong>
                  </div>
                  {#if voiceTestResult.transcript}
                    <p class="voice-test-transcript">{voiceTestResult.transcript}</p>
                  {/if}
                </div>
              {/if}
            {/if}
          </div>
          <p class="field-help">
            Muting assistant audio keeps transcripts and Orb status visible. The recording test may require microphone and Speech Recognition permission.
          </p>
        </section>

        {#if isMacDesktop}
          <!-- Contextual Assist currently depends on AppKit focus and Option-key gestures. -->
          <section class="card">
          <h2>Contextual Assist</h2>
          <p class="section-desc">
            Host-native writing and task affordance for selected text and
            focused writable fields. Use a single left Option tap to open the
            menu centered in the focused window when an eligible target is
            active; double left Option continues to open the main overlay.
            Generation and insertion are wired later; the trigger surface is
            enabled by default.
          </p>
          <div class="toggle-grid">
            <label class="toggle-label">
              <input type="checkbox" bind:checked={config.contextual_assist.enabled} />
              Enable Contextual Assist
            </label>
            <label class="toggle-label">
              <input
                type="checkbox"
                bind:checked={config.contextual_assist.show_on_selected_text}
                disabled={!config.contextual_assist.enabled}
              />
              Enable selected text targets
            </label>
            <label class="toggle-label">
              <input
                type="checkbox"
                bind:checked={config.contextual_assist.show_in_writable_fields}
                disabled={!config.contextual_assist.enabled}
              />
              Enable writable field targets
            </label>
          </div>
          <div class="field">
            <label for="contextual-personality">Default Personality</label>
            <select
              id="contextual-personality"
              bind:value={config.contextual_assist.default_personality}
              disabled={!config.contextual_assist.enabled}
            >
              {#each contextualPersonalityOptions as option}
                <option value={option.id}>{option.label}</option>
              {/each}
            </select>
            <p class="field-help">The expanded assist palette can switch personality before generation. Installed personalities are loaded from the workspace-scoped skills catalog.</p>
            {#if contextualPersonalitiesLoading}
              <p class="field-help">Loading workspace personalities...</p>
            {:else if contextualPersonalitiesError}
              <p class="inline-notice warning">{contextualPersonalitiesError}</p>
            {/if}
          </div>
          <div class="field">
            <label for="contextual-excluded-apps">Excluded Apps</label>
            <input
              id="contextual-excluded-apps"
              type="text"
              value={config.contextual_assist.excluded_apps.join(", ")}
              disabled={!config.contextual_assist.enabled}
              placeholder="Optional, comma-separated app names"
              oninput={(event) => {
                setContextualAssistExcludedApps((event.currentTarget as HTMLInputElement).value);
              }}
            />
            <p class="field-help">Native app matching suppresses the left-Option menu in matching apps.</p>
          </div>
          <div class="voice-test-panel">
            <div class="voice-test-copy">
              <p class="voice-test-title">Surface Preview</p>
              <p>Open the compact assist menu. Action buttons are intentionally placeholders.</p>
            </div>
            <button class="secondary" onclick={openContextualAssistPreview}>
              Preview
            </button>
          </div>
          </section>
        {/if}

        <!-- General -->
        <section class="card">
          <h2>General</h2>
          <div class="field">
            <label for="image">Container Image</label>
            <input id="image" type="text" bind:value={config.general.container_image} />
          </div>
          <div class="field">
            <label for="runtime-root">Local engine data folder</label>
            <input id="runtime-root" type="text" bind:value={config.general.runtime_root} placeholder="~/MagicianNotes" spellcheck="false" />
            <p class="field-help">Used by a desktop-managed local container. Remote engines keep their data on the server.</p>
          </div>
          <div class="toggle-row">
            <label class="toggle-label">
              <input
                type="checkbox"
                bind:checked={config.general.manage_runtime_stack}
                disabled={isWindowsDesktop}
              />
              Manage a local container backend
            </label>
            <p class="field-help">
              {#if isWindowsDesktop}
                Windows uses a remote backend in this release. Set its URL in Network; host CUA and the browser extension continue to run locally.
              {:else}
                Disable this when the desktop should connect to a separately managed local or remote backend.
              {/if}
            </p>
          </div>
          {#if isMacDesktop}
            <div class="field">
              <label for="overlay-gesture">Quick Overlay Gesture</label>
              <select id="overlay-gesture" bind:value={config.general.quick_overlay_gesture}>
                <option value="Double Left Option">Double Left ⌥</option>
                <option value="Double Right Option">Double Right ⌥</option>
                <option value="Disabled">Disabled</option>
              </select>
              <p class="field-help">Double-tap to summon the quick overlay. Needs Input Monitoring permission (same as the push-to-talk gesture).</p>
            </div>
          {/if}
          <div class="field">
            <label for="overlay-shortcut">Quick Overlay Shortcut (optional)</label>
            <input
              id="overlay-shortcut"
              type="text"
              bind:value={config.general.quick_overlay_shortcut}
              placeholder="Optional, e.g. CmdOrCtrl+Shift+Space"
              spellcheck="false"
            />
            <p class="field-help">Optional global chord for the quick overlay. Tauri syntax, e.g. <code>CmdOrCtrl+Shift+Space</code>.</p>
          </div>
          <div class="toggle-row">
            <label class="toggle-label">
              <input type="checkbox" bind:checked={config.general.launch_at_login} />
              Launch at login
            </label>
          </div>
          <div class="toggle-row">
            <label class="toggle-label">
              <input type="checkbox" bind:checked={config.general.notifications_enabled} />
              Show notifications
            </label>
          </div>
          <div class="toggle-row">
            <label class="toggle-label">
              <input type="checkbox" bind:checked={config.general.prevent_sleep} />
              Prevent this {desktopDeviceLabel} from sleeping while app is open
            </label>
          </div>
        </section>

        <!-- Screen Capture shortcuts -->
        <section class="card">
          <h2>Screen Capture</h2>
          <p class="section-desc">
            Global chords for the screen capture-and-ask features. Tauri syntax
            (e.g. <code>Shift+Alt+S</code>); leave a field empty to disable that
            chord. Changes apply immediately — no restart.
          </p>
          <div class="field">
            <label for="screen-ask-shortcut">Screenshot &amp; Ask</label>
            <input
              id="screen-ask-shortcut"
              type="text"
              bind:value={config.general.screen_ask_shortcut}
              placeholder="e.g. Shift+Alt+S"
              spellcheck="false"
            />
          </div>
          <div class="field">
            <label for="screen-region-shortcut">Region &amp; Ask</label>
            <input
              id="screen-region-shortcut"
              type="text"
              bind:value={config.general.screen_region_shortcut}
              placeholder="e.g. Shift+Alt+A"
              spellcheck="false"
            />
          </div>
          <div class="field">
            <label for="screen-clip-shortcut">Clip &amp; Ask</label>
            <input
              id="screen-clip-shortcut"
              type="text"
              bind:value={config.general.screen_clip_shortcut}
              placeholder="e.g. Shift+Alt+R"
              spellcheck="false"
            />
          </div>
          <div class="field">
            <label for="screen-watch-shortcut">Watch Screen</label>
            <input
              id="screen-watch-shortcut"
              type="text"
              bind:value={config.general.screen_watch_shortcut}
              placeholder="e.g. Shift+Alt+W"
              spellcheck="false"
            />
          </div>
        </section>

        <!-- Updates -->
        <section class="card">
          <h2>Updates</h2>
          <div class="toggle-row">
            <label class="toggle-label">
              <input type="checkbox" bind:checked={config.updates.auto_check} />
              Automatically check for updates
            </label>
          </div>
          <div class="toggle-row">
            <label class="toggle-label">
              <input type="checkbox" bind:checked={config.updates.auto_update_container} />
              Automatically apply container updates
            </label>
          </div>
          <button class="secondary" onclick={checkUpdates}>Check for Updates Now</button>
        </section>

        <!-- Uninstall -->
        <section class="card danger-zone">
          <h2>Uninstall</h2>
          <p class="section-desc">Remove {PRODUCT_NAME} components from your system. Only artifacts installed by {PRODUCT_NAME} will be removed — pre-existing tools are left in place.</p>

          <div class="uninstall-actions">
            <button class="danger" onclick={() => requestUninstall("tools-and-data")} disabled={uninstalling}>
              {uninstalling ? "Removing..." : "Remove Everything"}
            </button>
            <div class="uninstall-secondary">
              <button class="secondary" onclick={() => requestUninstall("only-tools")} disabled={uninstalling}>
                Remove Tools Only
              </button>
              <button class="secondary" onclick={() => requestUninstall("only-data")} disabled={uninstalling}>
                Remove Data Only
              </button>
            </div>
          </div>
          <p class="hint"><strong>Remove Everything</strong> — container, runtime tools, and data directories.<br/>
            <strong>Remove Tools Only</strong> — container and runtime tools; keeps your data.<br/>
            <strong>Remove Data Only</strong> — data directories; keeps tools for reinstall.</p>
        </section>
        </div>

      {:else if activeTab === "keyMappings"}
        <div class="sections">
          <section class="card key-mappings-card">
            <div class="card-header-row">
              <div>
                <h2>Key Mappings</h2>
                <p class="section-desc">
                  Current desktop triggers and typed composer commands for writing assistant, voice, quick automate, tutor, copilot, and screen capture.
                </p>
              </div>
              <button class="secondary" onclick={() => loadHotkeyMappings(true)} disabled={hotkeyMappingsLoading}>
                {hotkeyMappingsLoading ? "Refreshing..." : "Refresh"}
              </button>
            </div>

            {#if hotkeyMappingsError}
              <div class="inline-notice error">{hotkeyMappingsError}</div>
            {/if}

            <div class="key-mapping-meta">
              <span>Live from desktop runtime</span>
              <strong>Updated {hotkeyLastUpdatedLabel()}</strong>
            </div>

            {#if hotkeyMappings}
              <div class="key-mapping-groups">
                {#each hotkeyMappings.groups as group}
                  <div class="key-mapping-group">
                    <h3>{group.label}</h3>
                    <div class="key-mapping-list">
                      {#each group.items as item}
                        <div class="key-mapping-row" class:disabled={!item.active}>
                          <div class="key-mapping-main">
                            <div class="key-mapping-title-row">
                              <strong>{item.label}</strong>
                              <span class="key-status" class:active={item.active}>
                                {item.active ? "Active" : "Disabled"}
                              </span>
                            </div>
                            <p>{item.description}</p>
                            {#if item.note}
                              <span class="key-note">{item.note}</span>
                            {/if}
                          </div>
                          <div class="key-mapping-trigger">
                            <span>Trigger</span>
                            <code>{item.trigger}</code>
                          </div>
                          <div class="key-mapping-source">
                            <span>Source</span>
                            <strong>{item.source}</strong>
                          </div>
                        </div>
                      {/each}
                    </div>
                  </div>
                {/each}
              </div>
            {:else if hotkeyMappingsLoading}
              <p class="section-desc">Loading key mappings...</p>
            {:else}
              <button class="primary" onclick={() => loadHotkeyMappings(true)}>Load Key Mappings</button>
            {/if}
          </section>
        </div>
      {:else if activeTab === "envMapping"}
        <div class="sections">
          <section class="card env-map-card">
            <div class="card-header-row">
              <div>
                <h2>Environment Mapping</h2>
                <p class="section-desc">
                  Config-owned settings and their environment override names.
                </p>
              </div>
              <button class="secondary" onclick={loadEnvironment} disabled={envLoading}>
                {envLoading ? "Refreshing..." : "Refresh"}
              </button>
            </div>

            {#if envSnapshot}
              <div class="env-meta">
                <div>
                  <span>Mode</span>
                  <strong>{envSnapshot.mode}</strong>
                </div>
                <div>
                  <span>File</span>
                  <code>{envSnapshot.path}</code>
                </div>
                <div>
                  <span>Status</span>
                  <strong>{envSnapshot.exists ? "Found" : "Will be created"}</strong>
                </div>
              </div>
            {/if}

            {#if envError}
              <div class="inline-notice error">{envError}</div>
            {/if}

            {#if envSnapshot}
              <div class="key-mapping-meta">
                <span>Known variables</span>
                <strong>{envKnownEntryCount()}</strong>
              </div>
              <div class="env-map-groups">
                {#each envKnownCategories() as category}
                  <div class="env-category">
                    <h4>{category}</h4>
                    <div class="env-map-list">
                      {#each envMappingEntriesForCategory(category) as entry}
                        <div
                          class="env-map-row"
                          class:override-active={entry.present && !entry.empty}
                          class:env-only={!entry.config_path}
                        >
                          <div class="env-map-main">
                            <strong>{entry.label}</strong>
                            <code>{entry.key}</code>
                            <span>{entry.description}</span>
                          </div>
                          <div class="env-map-config">
                            <span>Config path</span>
                            <code>{envConfigPath(entry)}</code>
                          </div>
                          <div class="env-map-state">
                            <span>Precedence</span>
                            <strong>{envMappingStatusLabel(entry)}</strong>
                          </div>
                        </div>
                      {/each}
                    </div>
                  </div>
                {/each}
              </div>
            {:else if envLoading}
              <p class="section-desc">Loading environment mapping...</p>
            {:else}
              <button class="primary" onclick={loadEnvironment}>Load Environment Mapping</button>
            {/if}
          </section>
        </div>
      {:else if activeTab === "environment"}
        <div class="sections">
          <section class="card env-card">
            <div class="card-header-row">
              <div>
                <h2>Environment</h2>
                <p class="section-desc">
                  Edit the active env file used for this desktop run. Values are applied after the runtime is restarted.
                </p>
              </div>
              <button class="secondary" onclick={loadEnvironment} disabled={envLoading}>
                {envLoading ? "Refreshing..." : "Refresh"}
              </button>
            </div>

            {#if envSnapshot}
              <div class="env-meta">
                <div>
                  <span>Mode</span>
                  <strong>{envSnapshot.mode}</strong>
                </div>
                <div>
                  <span>File</span>
                  <code>{envSnapshot.path}</code>
                </div>
                <div>
                  <span>Status</span>
                  <strong>{envSnapshot.exists ? "Found" : "Will be created"}</strong>
                </div>
              </div>
            {/if}

            {#if envMessage}
              <div class="inline-notice success">{envMessage}</div>
            {/if}
            {#if envError}
              <div class="inline-notice error">{envError}</div>
            {/if}

            {#if envSnapshot}
              <div class="settings-group">
                <div class="settings-group-header">
                  <h3>Known Variables</h3>
                </div>
                {#each envKnownCategories() as category}
                  <div class="env-category">
                    <h4>{category}</h4>
                    <div class="env-list">
                      {#each envEntriesForCategory(category) as entry}
                        <div class="env-row" class:missing={!entry.present}>
                          <div class="env-key">
                            <strong>{entry.label}</strong>
                            <code>{entry.key}</code>
                            <span>{entry.description}</span>
                          </div>
                          <div class="env-value">
                            {#if envEditing[entry.key] !== undefined}
                              <input
                                type="text"
                                bind:value={envEditing[entry.key]}
                                spellcheck="false"
                                autocomplete="off"
                              />
                              <div class="env-actions">
                                <button
                                  class="primary compact"
                                  onclick={() => saveEnvKey(entry.key)}
                                  disabled={envSavingKey === entry.key}
                                >
                                  {envSavingKey === entry.key ? "Saving..." : "Save"}
                                </button>
                                <button class="secondary compact" onclick={() => cancelEnvEdit(entry.key)}>
                                  Cancel
                                </button>
                              </div>
                            {:else}
                              <code class:secret-value={entry.secret && !envIsRevealed(entry.key)}>
                                {envValueForDisplay(entry)}
                              </code>
                              <div class="env-actions">
                                {#if entry.present}
                                  {#if envIsRevealed(entry.key)}
                                    <button class="secondary compact" onclick={() => hideEnvKey(entry.key)}>
                                      Hide
                                    </button>
                                  {:else}
                                    <button class="secondary compact" onclick={() => revealEnvKey(entry.key)}>
                                      Reveal
                                    </button>
                                  {/if}
                                  <button class="secondary compact" onclick={() => copyEnvKey(entry.key)}>
                                    Copy
                                  </button>
                                {/if}
                                <button class="secondary compact" onclick={() => startEnvEdit(entry)}>
                                  Edit
                                </button>
                                {#if entry.present}
                                  <button
                                    class="danger compact"
                                    onclick={() => removeEnvKey(entry.key)}
                                    disabled={envSavingKey === entry.key}
                                  >
                                    Remove
                                  </button>
                                {/if}
                              </div>
                            {/if}
                          </div>
                          <span
                            class="env-status"
                            class:set={entry.present && !entry.empty}
                            class:empty={entry.empty}
                          >
                            {envStatusLabel(entry)}
                          </span>
                        </div>
                      {/each}
                    </div>
                  </div>
                {/each}
              </div>

              <div class="settings-group">
                <div class="settings-group-header">
                  <h3>Add Variable</h3>
                </div>
                <div class="env-add-row">
                  <div class="field">
                    <label for="new-env-key">Key</label>
                    <input
                      id="new-env-key"
                      type="text"
                      bind:value={newEnvKey}
                      placeholder="CUSTOM_API_KEY"
                      spellcheck="false"
                      autocomplete="off"
                    />
                  </div>
                  <div class="field">
                    <label for="new-env-value">Value</label>
                    <input
                      id="new-env-value"
                      type="text"
                      bind:value={newEnvValue}
                      placeholder="value"
                      spellcheck="false"
                      autocomplete="off"
                    />
                  </div>
                  <button
                    class="primary env-add-button"
                    onclick={addEnvKey}
                    disabled={!envKeyIsValid(newEnvKey) || envSavingKey === newEnvKey.trim()}
                  >
                    Add
                  </button>
                </div>
              </div>

              <details class="settings-group advanced-env">
                <summary>Additional Variables ({envAdditionalEntries().length})</summary>
                {#if envAdditionalEntries().length === 0}
                  <p class="field-help">No custom variables are present in the active env file.</p>
                {:else}
                  <div class="env-list compact-list">
                    {#each envAdditionalEntries() as entry}
                      <div class="env-row">
                        <div class="env-key">
                          <strong>{entry.key}</strong>
                          <span>{entry.description}</span>
                        </div>
                        <div class="env-value">
                          {#if envEditing[entry.key] !== undefined}
                            <input
                              type="text"
                              bind:value={envEditing[entry.key]}
                              spellcheck="false"
                              autocomplete="off"
                            />
                            <div class="env-actions">
                              <button
                                class="primary compact"
                                onclick={() => saveEnvKey(entry.key)}
                                disabled={envSavingKey === entry.key}
                              >
                                {envSavingKey === entry.key ? "Saving..." : "Save"}
                              </button>
                              <button class="secondary compact" onclick={() => cancelEnvEdit(entry.key)}>
                                Cancel
                              </button>
                            </div>
                          {:else}
                            <code class:secret-value={entry.secret && !envIsRevealed(entry.key)}>
                              {envValueForDisplay(entry)}
                            </code>
                            <div class="env-actions">
                              {#if envIsRevealed(entry.key)}
                                <button class="secondary compact" onclick={() => hideEnvKey(entry.key)}>
                                  Hide
                                </button>
                              {:else}
                                <button class="secondary compact" onclick={() => revealEnvKey(entry.key)}>
                                  Reveal
                                </button>
                              {/if}
                              <button class="secondary compact" onclick={() => copyEnvKey(entry.key)}>
                                Copy
                              </button>
                              <button class="secondary compact" onclick={() => startEnvEdit(entry)}>
                                Edit
                              </button>
                              <button
                                class="danger compact"
                                onclick={() => removeEnvKey(entry.key)}
                                disabled={envSavingKey === entry.key}
                              >
                                Remove
                              </button>
                            </div>
                          {/if}
                        </div>
                        <span class="env-status set">Set</span>
                      </div>
                    {/each}
                  </div>
                {/if}
              </details>

              <p class="field-help">
                The editor preserves comments and ordering where possible. Existing running processes keep their current environment until restarted.
              </p>
            {:else if envLoading}
              <p class="section-desc">Loading environment...</p>
            {:else}
              <button class="primary" onclick={loadEnvironment}>Load Environment</button>
            {/if}
          </section>
        </div>
      {/if}
    {/if}
  {:else}
    <div class="loading">
      <p>Loading configuration...</p>
    </div>
  {/if}

  {#if androidObservationApprovalVisible && isMacDesktop}
    <div class="restart-overlay android-observation-overlay" role="presentation">
      <div class="android-observation-dialog" role="dialog" aria-modal="true" aria-label="Android observation approval">
        <button
          class="secondary android-observation-close"
          type="button"
          onclick={() => (androidObservationApprovalVisible = false)}
        >Close</button>
        <AndroidAppsAuthority trustMode={androidObservationTrustMode} />
      </div>
    </div>
  {/if}

  {#if showUninstallConfirm}
    <div class="restart-overlay">
      <div class="restart-dialog">
        <p class="dialog-title">Confirm Uninstall</p>
        <p>
          {#if uninstallMode === "tools-and-data"}
            This will remove the container, runtime tools, and all data directories. This cannot be undone.
          {:else if uninstallMode === "only-tools"}
            This will remove the container and runtime tools. Your data will be preserved.
          {:else}
            This will remove all data directories. Tools and container runtime will be preserved.
          {/if}
        </p>
        <p class="hint">You may be prompted for your password.</p>
        <div class="restart-actions">
          <button class="secondary" onclick={dismissUninstall}>Cancel</button>
          <button class="danger" onclick={confirmUninstall}>Uninstall</button>
        </div>
      </div>
    </div>
  {/if}

  {#if showRestartConfirm}
    <div class="restart-overlay">
      <div class="restart-dialog">
        <p>Settings saved. Restart container to apply changes?</p>
        <div class="restart-actions">
          <button class="secondary" onclick={dismissRestart}>Later</button>
          <button class="primary" onclick={confirmRestart} disabled={actionInProgress}>
            {actionInProgress ? "Restarting..." : "Restart Now"}
          </button>
        </div>
      </div>
    </div>
  {/if}
</div>

<style>
  .port-conflict-banner {
    background: rgba(239, 68, 68, 0.08);
    border: 1px solid rgba(239, 68, 68, 0.3);
    border-radius: 8px;
    padding: 16px;
  }

  .conflict-title {
    font-weight: 600;
    font-size: 14px;
    color: var(--error, #ef4444);
    margin: 0 0 8px 0;
  }

  .conflict-list {
    list-style: none;
    padding: 0;
    margin: 0 0 12px 0;
  }

  .conflict-list li {
    font-size: 13px;
    padding: 2px 0;
    color: var(--text, #1a1a1a);
  }

  .conflict-actions {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .conflict-hint {
    font-size: 12px;
    color: var(--text-muted, #6b7280);
  }

  .settings-container {
    display: flex;
    flex-direction: column;
    padding: 24px;
    max-width: 680px;
    margin: 0 auto;
    gap: 20px;
  }

  .settings-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
  }

  .settings-header h1 {
    font-size: 20px;
    font-weight: 600;
    margin-bottom: 6px;
  }

  .status-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .status-text {
    font-size: 13px;
    color: var(--text-muted);
  }

  .header-actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .external-runtime-pill {
    display: inline-flex;
    align-items: center;
    min-height: 32px;
    border-radius: 999px;
    border: 1px solid color-mix(in srgb, var(--border, #334155) 72%, var(--accent, #533483));
    background: color-mix(in srgb, var(--accent, #533483) 16%, transparent);
    color: var(--text, #e2e8f0);
    font-size: 12px;
    font-weight: 600;
    padding: 0 12px;
    white-space: nowrap;
  }

  .sections {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  .settings-tabs {
    display: inline-flex;
    align-self: flex-start;
    gap: 4px;
    padding: 4px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: color-mix(in srgb, var(--surface) 86%, var(--bg-subtle));
  }

  .settings-tabs button {
    min-height: 30px;
    border: 0;
    border-radius: 6px;
    background: transparent;
    color: var(--text-muted);
    font-size: 13px;
    font-weight: 600;
    padding: 0 12px;
    cursor: pointer;
  }

  .settings-tabs button.active {
    background: var(--accent);
    color: #fff;
  }

  .web-settings-card {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 20px;
    background:
      linear-gradient(135deg, color-mix(in srgb, var(--accent) 12%, transparent), transparent 68%),
      var(--bg-card);
  }

  .web-settings-card h2 {
    margin: 2px 0 6px;
  }

  .settings-kicker {
    margin: 0;
    color: var(--accent-light, var(--accent));
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .web-settings-actions {
    display: flex;
    flex: 0 0 auto;
    flex-direction: column;
    gap: 8px;
    min-width: 156px;
  }

  .card h2 {
    font-size: 15px;
    font-weight: 600;
    margin-bottom: 12px;
  }

  .orb-settings-card {
    position: relative;
    overflow: hidden;
    background:
      radial-gradient(circle at 94% 5%, color-mix(in srgb, var(--accent) 26%, transparent), transparent 32%),
      var(--bg-card);
  }

  .orb-settings-heading {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 20px;
  }

  .orb-preview {
    flex: 0 0 38px;
    height: 38px;
    border-radius: 48% 52% 55% 45%;
    background: conic-gradient(from 35deg, #563e8a, #a37df4, #4f64ac, #563e8a);
    box-shadow: 0 0 22px #8964e575, inset -7px -8px 12px #18122499, inset 4px 3px 10px #fff5;
    animation: orb-settings-breathe 5s ease-in-out infinite alternate;
  }

  input.shortcut-recording {
    border-color: color-mix(in srgb, var(--accent) 72%, white);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 20%, transparent);
    color: var(--accent-light, #b8a1ff);
  }

  @keyframes orb-settings-breathe {
    to { transform: scale(1.07) rotate(8deg); border-radius: 57% 43% 46% 54%; }
  }

  @media (prefers-reduced-motion: reduce) {
    .orb-preview { animation: none; }
  }

  .field {
    margin-bottom: 12px;
  }

  .field:last-child {
    margin-bottom: 0;
  }

  .field-help {
    font-size: 12px;
    color: var(--text-muted, #6b7280);
    margin-top: 6px;
    line-height: 1.4;
  }

  .connect-route-panel {
    margin: 16px 0;
    padding: 14px;
    border: 1px solid var(--border);
    border-radius: 10px;
    background: color-mix(in srgb, var(--surface) 74%, transparent);
  }

  .connect-route-heading,
  .connect-route-actions {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
  }

  .connect-route-heading {
    margin-bottom: 12px;
  }

  .route-pill {
    flex: 0 0 auto;
    padding: 5px 9px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--accent) 14%, transparent);
    color: var(--accent-light, var(--accent));
    font-size: 11px;
    font-weight: 700;
    text-transform: capitalize;
  }

  .connect-route-health {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 8px;
    margin: 8px 0;
  }

  .connect-route-health span {
    display: grid;
    gap: 2px;
    padding: 8px;
    border-radius: 8px;
    background: color-mix(in srgb, var(--bg) 65%, transparent);
    color: var(--text-muted);
    font-size: 11px;
  }

  .connect-route-health strong {
    color: var(--text);
    font-size: 12px;
  }

  .connect-route-actions {
    align-items: center;
    justify-content: flex-end;
    margin-top: 12px;
  }

  .field-row {
    display: flex;
    gap: 12px;
  }

  .field-row .field {
    flex: 1;
  }

  .settings-group {
    border-top: 1px solid color-mix(in srgb, var(--border, #334155) 72%, transparent);
    padding-top: 14px;
    margin-top: 14px;
  }

  .settings-group-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 10px;
  }

  .settings-group-header h3 {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
    color: var(--text, #1a1a1a);
  }

  .toggle-grid {
    display: grid;
    gap: 8px;
    margin: 4px 0 0;
  }

  .card-header-row {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
  }

  .env-card {
    max-width: none;
  }

  .env-map-card {
    max-width: none;
  }

  .key-mappings-card {
    max-width: none;
  }

  .key-mapping-meta {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    width: fit-content;
    margin: 4px 0 2px;
    padding: 6px 9px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: color-mix(in srgb, var(--surface) 92%, var(--bg-subtle));
    color: var(--text-muted);
    font-size: 12px;
  }

  .key-mapping-meta strong {
    color: var(--text);
    font-weight: 600;
  }

  .key-mapping-groups {
    display: grid;
    gap: 16px;
    margin-top: 14px;
  }

  .key-mapping-group {
    display: grid;
    gap: 8px;
  }

  .key-mapping-group h3 {
    margin: 0;
    color: var(--text-muted);
    font-size: 12px;
    font-weight: 700;
    text-transform: uppercase;
  }

  .key-mapping-list {
    display: grid;
    gap: 8px;
  }

  .key-mapping-row {
    display: grid;
    grid-template-columns: minmax(220px, 1fr) minmax(150px, 0.45fr) minmax(130px, 0.35fr);
    gap: 12px;
    align-items: start;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: color-mix(in srgb, var(--surface) 94%, var(--accent));
  }

  .key-mapping-row.disabled {
    background: color-mix(in srgb, var(--surface) 94%, var(--bg-subtle));
  }

  .key-mapping-main {
    min-width: 0;
    display: grid;
    gap: 4px;
  }

  .key-mapping-title-row {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }

  .key-mapping-title-row strong {
    min-width: 0;
    color: var(--text);
    font-size: 13px;
    overflow-wrap: anywhere;
  }

  .key-mapping-main p {
    margin: 0;
    color: var(--text-muted);
    font-size: 12px;
    line-height: 1.4;
  }

  .key-note {
    color: var(--text-muted);
    font-size: 12px;
    line-height: 1.4;
  }

  .key-status {
    flex: 0 0 auto;
    padding: 2px 7px;
    border: 1px solid var(--border);
    border-radius: 999px;
    color: var(--text-muted);
    font-size: 11px;
    font-weight: 700;
  }

  .key-status.active {
    border-color: color-mix(in srgb, var(--success) 48%, var(--border));
    color: var(--success);
    background: color-mix(in srgb, var(--success) 10%, transparent);
  }

  .key-mapping-trigger,
  .key-mapping-source {
    min-width: 0;
    display: grid;
    gap: 4px;
  }

  .key-mapping-trigger span,
  .key-mapping-source span {
    color: var(--text-muted);
    font-size: 11px;
    font-weight: 700;
    text-transform: uppercase;
  }

  .key-mapping-trigger code,
  .key-mapping-source strong {
    min-width: 0;
    color: var(--text);
    font-size: 12px;
    overflow-wrap: anywhere;
  }

  .env-meta {
    display: grid;
    grid-template-columns: minmax(80px, 0.6fr) minmax(0, 2.4fr) minmax(90px, 0.8fr);
    gap: 8px;
    padding: 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: color-mix(in srgb, var(--surface) 92%, var(--accent));
  }

  .env-meta div {
    min-width: 0;
    display: grid;
    gap: 3px;
  }

  .env-meta span {
    font-size: 11px;
    font-weight: 700;
    color: var(--text-muted);
    text-transform: uppercase;
  }

  .env-meta strong,
  .env-meta code {
    min-width: 0;
    color: var(--text);
    font-size: 12px;
    overflow-wrap: anywhere;
  }

  .inline-notice {
    padding: 10px 12px;
    border-radius: 8px;
    font-size: 12px;
    line-height: 1.4;
  }

  .inline-notice.success {
    border: 1px solid color-mix(in srgb, var(--success) 42%, var(--border));
    color: var(--success);
    background: color-mix(in srgb, var(--success) 10%, transparent);
  }

  .inline-notice.error {
    border: 1px solid color-mix(in srgb, var(--danger) 42%, var(--border));
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 10%, transparent);
  }

  .inline-notice.warning {
    border: 1px solid color-mix(in srgb, var(--warning, #f59e0b) 42%, var(--border));
    color: var(--warning, #f59e0b);
    background: color-mix(in srgb, var(--warning, #f59e0b) 10%, transparent);
  }

  .env-category {
    display: grid;
    gap: 8px;
    margin-top: 14px;
  }

  .env-category h4 {
    margin: 0;
    font-size: 12px;
    font-weight: 700;
    color: var(--text-muted);
    text-transform: uppercase;
  }

  .env-list {
    display: grid;
    gap: 8px;
  }

  .env-map-groups {
    display: grid;
    gap: 16px;
    margin-top: 14px;
  }

  .env-map-list {
    display: grid;
    gap: 8px;
  }

  .env-map-row {
    display: grid;
    grid-template-columns: minmax(220px, 1.2fr) minmax(190px, 0.8fr) minmax(120px, 0.45fr);
    gap: 12px;
    align-items: start;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface);
  }

  .env-map-row.override-active {
    border-color: color-mix(in srgb, var(--warning, #f59e0b) 40%, var(--border));
    background: color-mix(in srgb, var(--surface) 94%, var(--warning, #f59e0b));
  }

  .env-map-row.env-only {
    background: color-mix(in srgb, var(--surface) 94%, var(--bg-subtle));
  }

  .env-map-main,
  .env-map-config,
  .env-map-state {
    min-width: 0;
    display: grid;
    gap: 4px;
  }

  .env-map-main strong {
    color: var(--text);
    font-size: 13px;
  }

  .env-map-main span {
    color: var(--text-muted);
    font-size: 12px;
    line-height: 1.35;
  }

  .env-map-main code,
  .env-map-config code {
    min-width: 0;
    color: var(--text);
    font-size: 12px;
    overflow-wrap: anywhere;
  }

  .env-map-main code {
    color: var(--text-muted);
    font-size: 11px;
  }

  .env-map-config span,
  .env-map-state span {
    color: var(--text-muted);
    font-size: 11px;
    font-weight: 700;
    text-transform: uppercase;
  }

  .env-map-state strong {
    width: fit-content;
    min-height: 24px;
    display: inline-flex;
    align-items: center;
    border-radius: 999px;
    padding: 0 8px;
    background: color-mix(in srgb, var(--text-muted) 14%, transparent);
    color: var(--text-muted);
    font-size: 11px;
    font-weight: 700;
  }

  .env-row {
    display: grid;
    grid-template-columns: minmax(160px, 1.1fr) minmax(220px, 1.3fr) 70px;
    gap: 12px;
    align-items: start;
    padding: 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface);
  }

  .env-row.missing {
    background: color-mix(in srgb, var(--surface) 94%, var(--text-muted));
  }

  .env-key {
    min-width: 0;
    display: grid;
    gap: 3px;
  }

  .env-key strong {
    font-size: 13px;
    color: var(--text);
  }

  .env-key code {
    font-size: 11px;
    color: var(--text-muted);
    overflow-wrap: anywhere;
  }

  .env-key span {
    font-size: 12px;
    line-height: 1.35;
    color: var(--text-muted);
  }

  .env-value {
    min-width: 0;
    display: grid;
    gap: 8px;
  }

  .env-value > code {
    min-height: 32px;
    display: flex;
    align-items: center;
    width: 100%;
    padding: 7px 9px;
    border: 1px solid color-mix(in srgb, var(--border) 72%, transparent);
    border-radius: 6px;
    background: color-mix(in srgb, var(--bg-subtle) 82%, transparent);
    color: var(--text);
    font-size: 12px;
    line-height: 1.35;
    overflow-wrap: anywhere;
    word-break: break-word;
  }

  .env-value > code.secret-value {
    color: var(--text-muted);
  }

  .env-value input {
    width: 100%;
    min-height: 34px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface);
    color: var(--text);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace;
    font-size: 12px;
    padding: 0 9px;
  }

  .env-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  button.compact {
    min-height: 28px;
    border-radius: 6px;
    padding: 0 9px;
    font-size: 12px;
  }

  .env-status {
    justify-self: end;
    display: inline-flex;
    align-items: center;
    min-height: 24px;
    border-radius: 999px;
    padding: 0 8px;
    background: color-mix(in srgb, var(--text-muted) 14%, transparent);
    color: var(--text-muted);
    font-size: 11px;
    font-weight: 700;
  }

  .env-status.set {
    background: color-mix(in srgb, var(--success) 14%, transparent);
    color: var(--success);
  }

  .env-status.empty {
    background: color-mix(in srgb, var(--warning) 16%, transparent);
    color: var(--warning);
  }

  .env-add-row {
    display: grid;
    grid-template-columns: minmax(150px, 0.8fr) minmax(220px, 1.2fr) auto;
    gap: 12px;
    align-items: end;
  }

  .env-add-button {
    min-height: 34px;
    margin-bottom: 12px;
  }

  .advanced-env summary {
    cursor: pointer;
    font-size: 13px;
    font-weight: 700;
    color: var(--text);
  }

  .compact-list {
    margin-top: 12px;
  }

  .field select {
    width: 100%;
    min-height: 34px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--surface);
    color: var(--text);
    padding: 0 10px;
    font-size: 14px;
  }

  .toggle-row {
    margin-bottom: 8px;
  }

  .toggle-label {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 14px;
    color: var(--text);
    cursor: pointer;
  }

  .toggle-label input[type="checkbox"] {
    width: 16px;
    height: 16px;
    accent-color: var(--accent);
  }

  .voice-test-panel {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin: 12px 0;
    padding: 12px;
    border: 1px solid color-mix(in srgb, var(--border, #334155) 76%, var(--accent, #533483));
    border-radius: 10px;
    background: color-mix(in srgb, var(--surface, #fff) 86%, var(--accent, #533483));
  }

  .voice-test-copy {
    min-width: 0;
  }

  .voice-test-title {
    margin: 0;
    font-size: 13px;
    font-weight: 700;
    color: var(--text);
  }

  .voice-test-result {
    margin: 10px 0 12px;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: 10px;
    background: color-mix(in srgb, var(--surface, #fff) 92%, var(--accent, #533483));
  }

  .voice-test-error {
    border-color: color-mix(in srgb, var(--danger, #e53e3e) 55%, var(--border));
    background: color-mix(in srgb, var(--danger, #e53e3e) 10%, var(--surface, #fff));
  }

  .voice-test-error p {
    margin: 0;
    font-size: 12px;
    line-height: 1.4;
    color: var(--danger, #e53e3e);
  }

  .voice-test-grid {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    gap: 6px 12px;
    font-size: 12px;
    line-height: 1.4;
  }

  .voice-test-grid span {
    color: var(--text-muted);
  }

  .voice-test-grid strong {
    min-width: 0;
    color: var(--text);
    font-weight: 600;
    overflow-wrap: anywhere;
  }

  .voice-test-transcript {
    margin: 10px 0 0;
    padding-top: 10px;
    border-top: 1px solid var(--border);
    font-size: 13px;
    line-height: 1.45;
    color: var(--text);
  }

  .settings-save-bar {
    position: sticky;
    top: 0;
    z-index: 20;
    display: flex;
    justify-content: flex-end;
    align-items: center;
    gap: 12px;
    min-height: 52px;
    padding: 10px 0;
    border-bottom: 1px solid var(--border);
    background: color-mix(in srgb, var(--bg) 94%, transparent);
    box-shadow: 0 10px 18px -18px rgba(0, 0, 0, 0.8);
    backdrop-filter: blur(12px);
  }

  .save-message {
    font-size: 13px;
    color: var(--success);
    opacity: 0;
    transition: opacity 0.3s;
  }

  .save-message.visible {
    opacity: 1;
  }

  .loading {
    display: flex;
    justify-content: center;
    padding: 40px;
    color: var(--text-muted);
  }

  .restart-overlay {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }

  .restart-dialog {
    background: var(--bg, #fff);
    border-radius: 12px;
    padding: 24px;
    max-width: 360px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.2);
  }

  .restart-dialog p {
    font-size: 14px;
    margin-bottom: 16px;
    color: var(--text);
  }

  .restart-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  /* Uninstall / Danger zone */
  .danger-zone {
    border: 1px solid var(--danger, #e53e3e);
  }

  .danger-zone h2 {
    color: var(--danger, #e53e3e);
  }

  .section-desc {
    font-size: 13px;
    color: var(--text-muted);
    margin-bottom: 12px;
    line-height: 1.4;
  }

  .hint {
    font-size: 12px;
    color: var(--text-muted);
    margin-top: 10px;
    line-height: 1.5;
  }

  .dialog-title {
    font-weight: 600;
    font-size: 15px;
    margin-bottom: 4px;
  }

  .uninstall-actions {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .uninstall-secondary {
    display: flex;
    gap: 8px;
  }

  .uninstall-result {
    background: var(--bg-subtle, #f7f7f7);
    border-radius: 8px;
    padding: 12px;
  }

  .uninstall-result pre {
    font-size: 12px;
    white-space: pre-wrap;
    word-break: break-word;
    margin-bottom: 8px;
    color: var(--text);
  }

  button.danger {
    background: var(--danger, #e53e3e);
    color: #fff;
    border: none;
    border-radius: 6px;
    padding: 8px 16px;
    font-size: 14px;
    cursor: pointer;
  }

  button.danger:hover {
    background: var(--danger-hover, #c53030);
  }

  button.danger:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .android-observation-overlay {
    align-items: flex-start;
    overflow: auto;
    padding: 28px;
  }

  .android-observation-dialog {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 14px;
    box-shadow: 0 18px 60px color-mix(in srgb, black 28%, transparent);
    margin: auto;
    max-width: 760px;
    padding: 12px;
    position: relative;
    width: 100%;
  }

  .android-observation-close {
    float: right;
    margin: 6px 6px 10px 12px;
    position: relative;
    z-index: 1;
  }

  @media (max-width: 720px) {
    .connect-route-heading {
      flex-direction: column;
    }

    .connect-route-health {
      grid-template-columns: 1fr;
    }

    .web-settings-card {
      align-items: stretch;
      flex-direction: column;
    }

    .web-settings-actions {
      display: grid;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      min-width: 0;
    }

    .env-meta,
    .env-row,
    .env-add-row,
    .env-map-row,
    .key-mapping-row {
      grid-template-columns: 1fr;
    }

    .env-status {
      justify-self: start;
    }

    .settings-tabs {
      width: 100%;
      flex-wrap: wrap;
    }

    .settings-tabs button {
      flex: 1 1 auto;
    }

    .card-header-row {
      flex-direction: column;
    }
  }
</style>
