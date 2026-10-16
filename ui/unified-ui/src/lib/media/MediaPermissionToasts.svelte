<script lang="ts">
  import { HOST_APP_NAME } from '$lib/presentationIdentity';
  import { onDestroy, onMount } from 'svelte';
  import { v2Events } from '$lib/realtime/v2-websocket';
  import { showWarning } from '$lib/shared/stores/notifications';

  // Surfaces a toast whenever the backend reports a media permission
  // denial or revocation. On iOS Safari (and parts of Android Chrome)
  // a denied mic/camera prompt is otherwise silent — the user taps a
  // button and nothing happens. The toast turns that into an
  // observable, debuggable signal.
  //
  // Mounted in (app)/+layout.svelte so it runs across every page the
  // user touches.

  let unsubscribe: (() => void) | null = null;

  // Dedupe: the same (event_type, channel) pair within 10s collapses
  // to a single toast. Permission events can fan out (every requester
  // sees the same denial) so without this we'd stack 3–4 toasts.
  const recent = new Map<string, number>();
  const DEDUPE_WINDOW_MS = 10_000;

  function channelLabel(channel: string): string {
    switch (channel) {
      case 'mic':
        return 'Microphone';
      case 'camera':
        return 'Camera';
      case 'screen_capture':
        return 'Screen capture';
      case 'system_audio':
        return 'System audio';
      case 'transcription':
        return 'Transcription';
      default:
        return channel;
    }
  }

  // Screen capture + system audio are macOS HOST permissions (reported by the
  // native presence host, which captures via ScreenCaptureKit) — they live in
  // System Settings → Privacy & Security → Screen Recording, NOT browser
  // settings. The host re-reports on a heartbeat, so a freshly-granted
  // permission is picked up automatically without a restart.
  const MACOS_HOST_CHANNELS = new Set(['screen_capture', 'system_audio']);

  function remediation(channel: string, revoked: boolean): string {
    if (MACOS_HOST_CHANNELS.has(channel)) {
      const verb = revoked ? 'Re-enable' : 'Grant';
      return `${verb} Screen Recording for ${HOST_APP_NAME} in System Settings → Privacy & Security. It’s picked up automatically — no restart needed.`;
    }
    return revoked
      ? 'Re-enable it from your browser settings to keep using this feature.'
      : 'Allow it from your browser settings to use this feature.';
  }

  function shouldShow(key: string): boolean {
    const now = Date.now();
    const last = recent.get(key);
    if (last !== undefined && now - last < DEDUPE_WINDOW_MS) return false;
    recent.set(key, now);
    // Garbage-collect entries older than the window so the map doesn't grow unbounded.
    if (recent.size > 32) {
      for (const [k, ts] of recent) {
        if (now - ts >= DEDUPE_WINDOW_MS) recent.delete(k);
      }
    }
    return true;
  }

  onMount(() => {
    unsubscribe = v2Events.subscribe((events) => {
      for (const event of events) {
        const type = String(event.event_type);
        if (type !== 'MediaPermissionDenied' && type !== 'MediaPermissionRevoked') {
          continue;
        }
        const channel = String((event.data as { channel?: unknown })?.channel ?? 'unknown');
        const key = `${type}:${channel}`;
        if (!shouldShow(key)) continue;

        const label = channelLabel(channel);
        const revoked = type === 'MediaPermissionRevoked';
        showWarning(
          `${label} access ${revoked ? 'revoked' : 'denied'}`,
          remediation(channel, revoked)
        );
      }
    });
  });

  onDestroy(() => {
    unsubscribe?.();
  });
</script>
