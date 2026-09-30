<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

  type PermissionState = "granted" | "missing" | "unknown" | "unsupported";

  interface DesktopPermission {
    key: string;
    title: string;
    description: string;
    state: PermissionState;
    required: boolean;
    settings_label: string;
    details: string;
  }

  interface Props {
    compact?: boolean;
    only?: string[];
  }

  let { compact = false, only = undefined }: Props = $props();

  let permissions = $state<DesktopPermission[]>([]);
  const shown = $derived(
    only ? permissions.filter((p) => only!.includes(p.key)) : permissions,
  );
  let loading = $state(true);
  let requestingKey = $state("");
  let error = $state("");

  const stateLabel: Record<PermissionState, string> = {
    granted: "Allowed",
    missing: "Needs access",
    unknown: "Not checked",
    unsupported: "Unavailable",
  };

  function permissionStateLabel(permission: DesktopPermission): string {
    if (permission.state === "unknown" && permission.key === "speech_recognition") {
      return "Not requested";
    }
    if (permission.state === "unknown" && permission.key === "cua_driver") {
      return "Not verified";
    }
    return stateLabel[permission.state];
  }

  onMount(() => {
    void refreshPermissions();
  });

  async function refreshPermissions() {
    loading = true;
    error = "";
    try {
      permissions = await invoke<DesktopPermission[]>("get_desktop_permissions");
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  async function openPermission(permission: DesktopPermission) {
    requestingKey = permission.key;
    error = "";
    try {
      const shouldRequest =
        permission.key === "cua_driver" ||
        ((permission.key === "microphone" ||
          permission.key === "speech_recognition" ||
          permission.key === "accessibility") &&
          permission.state !== "granted");
      if (shouldRequest) {
        await invoke("request_desktop_permission", {
          permissionKey: permission.key,
        });
        await refreshPermissions();
        return;
      }
      await invoke("open_desktop_permission_settings", {
        permissionKey: permission.key,
      });
    } catch (e) {
      error = String(e);
      try {
        await invoke("open_desktop_permission_settings", {
          permissionKey: permission.key,
        });
      } catch {
        // Keep the original permission-request error visible.
      }
    } finally {
      requestingKey = "";
    }
  }

  function actionLabel(permission: DesktopPermission): string {
    if (requestingKey === permission.key) return "Requesting...";
    if (
      (permission.key === "microphone" ||
        permission.key === "speech_recognition" ||
        permission.key === "accessibility") &&
      permission.state !== "granted"
    ) {
      return "Request Access";
    }
    return permission.settings_label;
  }
</script>

<section class="permission-card" class:compact>
  <div class="permission-header">
    <div>
      <h2>Desktop Permissions</h2>
      <p>
        Magican Desktop checks the host permissions needed for voice, global gestures,
        and desktop presence.
      </p>
    </div>
    <button class="secondary compact-button" onclick={refreshPermissions} disabled={loading}>
      {loading ? "Checking..." : "Refresh"}
    </button>
  </div>

  {#if error}
    <p class="permission-error">{error}</p>
  {/if}

  {#if loading && permissions.length === 0}
    <div class="permission-loading">Checking permissions...</div>
  {:else}
    <div class="permission-list">
      {#each shown as permission (permission.key)}
        <article class="permission-row" data-state={permission.state}>
          <div class="permission-status">
            <span class="status-dot" data-state={permission.state}></span>
          </div>
          <div class="permission-copy">
            <div class="permission-title-row">
              <h3>{permission.title}</h3>
              <span class="state-pill" data-state={permission.state}>
                {permissionStateLabel(permission)}
              </span>
              {#if permission.required}
                <span class="required-pill">Required</span>
              {/if}
            </div>
            <p>{permission.description}</p>
            {#if !compact}
              <p class="permission-detail">{permission.details}</p>
            {/if}
          </div>
          <button
            class="secondary permission-action"
            onclick={() => openPermission(permission)}
            disabled={permission.state === "unsupported" || requestingKey === permission.key}
          >
            {actionLabel(permission)}
          </button>
        </article>
      {/each}
    </div>
  {/if}
</section>

<style>
  .permission-card {
    background:
      linear-gradient(135deg, rgba(99, 102, 241, 0.09), rgba(14, 165, 233, 0.06)),
      var(--card, var(--surface, #ffffff));
    border: 1px solid color-mix(in srgb, var(--border, #e2e8f0) 80%, var(--accent, #3b82f6));
    border-radius: 12px;
    padding: 18px;
    box-shadow: 0 18px 50px rgba(15, 23, 42, 0.08);
  }

  .permission-card.compact {
    padding: 16px;
  }

  .permission-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 14px;
  }

  .permission-header h2 {
    font-size: 15px;
    line-height: 1.2;
    margin: 0 0 6px;
    color: var(--text, #111827);
  }

  .permission-header p {
    margin: 0;
    color: var(--text-muted, #6b7280);
    font-size: 13px;
    line-height: 1.45;
  }

  .permission-list {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .permission-row {
    display: grid;
    grid-template-columns: 18px minmax(0, 1fr) auto;
    align-items: start;
    gap: 10px;
    padding: 12px;
    border-radius: 10px;
    border: 1px solid color-mix(in srgb, var(--border, #e2e8f0) 78%, transparent);
    background: color-mix(in srgb, var(--surface, #ffffff) 86%, transparent);
  }

  .permission-row[data-state="missing"] {
    border-color: color-mix(in srgb, var(--warning, #f59e0b) 45%, var(--border, #e2e8f0));
    background: color-mix(in srgb, var(--warning, #f59e0b) 8%, var(--surface, #ffffff));
  }

  .permission-row[data-state="granted"] {
    border-color: color-mix(in srgb, var(--success, #22c55e) 32%, var(--border, #e2e8f0));
  }

  .permission-status {
    padding-top: 4px;
  }

  .status-dot {
    display: block;
    width: 10px;
    height: 10px;
    border-radius: 999px;
    background: var(--text-muted, #94a3b8);
  }

  .status-dot[data-state="granted"] {
    background: var(--success, #22c55e);
    box-shadow: 0 0 0 4px color-mix(in srgb, var(--success, #22c55e) 15%, transparent);
  }

  .status-dot[data-state="missing"] {
    background: var(--warning, #f59e0b);
    box-shadow: 0 0 0 4px color-mix(in srgb, var(--warning, #f59e0b) 16%, transparent);
  }

  .status-dot[data-state="unknown"] {
    background: var(--accent, #3b82f6);
    box-shadow: 0 0 0 4px color-mix(in srgb, var(--accent, #3b82f6) 14%, transparent);
  }

  .permission-title-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 7px;
    margin-bottom: 5px;
  }

  .permission-title-row h3 {
    margin: 0;
    color: var(--text, #111827);
    font-size: 13px;
    line-height: 1.2;
  }

  .permission-copy p {
    margin: 0;
    color: var(--text-muted, #6b7280);
    font-size: 12px;
    line-height: 1.4;
  }

  .permission-detail {
    margin-top: 6px !important;
  }

  .state-pill,
  .required-pill {
    display: inline-flex;
    align-items: center;
    height: 20px;
    padding: 0 8px;
    border-radius: 999px;
    font-size: 11px;
    font-weight: 600;
    white-space: nowrap;
  }

  .state-pill {
    color: var(--text-muted, #64748b);
    background: color-mix(in srgb, var(--text-muted, #64748b) 12%, transparent);
  }

  .state-pill[data-state="granted"] {
    color: color-mix(in srgb, var(--success, #16a34a) 82%, var(--text, #111827));
    background: color-mix(in srgb, var(--success, #22c55e) 13%, transparent);
  }

  .state-pill[data-state="missing"] {
    color: color-mix(in srgb, var(--warning, #f59e0b) 72%, var(--text, #111827));
    background: color-mix(in srgb, var(--warning, #f59e0b) 15%, transparent);
  }

  .required-pill {
    color: var(--accent, #2563eb);
    background: color-mix(in srgb, var(--accent, #3b82f6) 12%, transparent);
  }

  .permission-action {
    align-self: center;
    white-space: nowrap;
  }

  .compact-button {
    white-space: nowrap;
  }

  .permission-error {
    margin: 0 0 10px;
    padding: 10px 12px;
    border-radius: 8px;
    background: color-mix(in srgb, var(--error, #ef4444) 12%, transparent);
    color: var(--error, #ef4444);
    font-size: 12px;
  }

  .permission-loading {
    color: var(--text-muted, #6b7280);
    font-size: 13px;
  }

  button.secondary {
    border: 1px solid var(--border, #d1d5db);
    background: var(--surface, #ffffff);
    color: var(--text, #111827);
    border-radius: 8px;
    min-height: 32px;
    padding: 0 12px;
    font-size: 12px;
    cursor: pointer;
  }

  button.secondary:disabled {
    opacity: 0.55;
    cursor: default;
  }

  @media (max-width: 620px) {
    .permission-header {
      flex-direction: column;
      align-items: stretch;
    }

    .permission-row {
      grid-template-columns: 18px minmax(0, 1fr);
    }

    .permission-action {
      grid-column: 2;
      justify-self: start;
    }
  }
</style>
