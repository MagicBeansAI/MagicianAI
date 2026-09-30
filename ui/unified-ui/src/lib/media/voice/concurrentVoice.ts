import { browser } from '$app/environment';
import { get, writable } from 'svelte/store';
import { getCurrentScopeIdentity, scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { ttsStore } from '$lib/media/tts/store';
import { providerSpeakBlocks, cancelProviderSpeak, primeProviderTtsPlayback } from '$lib/media/tts/providerTts';
import { speak, cancelCurrent, isBrowserTtsAvailable } from '$lib/media/tts/browserTts';
import { isTutorAudioFocusActive } from '$lib/media/tts/tutorAudioFocus';
import { mediaProvidersStore } from '$lib/media/providers';
import type { ContentBlockRecord } from '$lib/stores/chatStore';
import { ConcurrentVoiceCoordinator, type ConcurrentVoiceRequest, type ConcurrentVoiceSnapshot, type PlaybackOutcome } from './concurrentVoiceCoordinator';

export type { ConcurrentVoiceRequest } from './concurrentVoiceCoordinator';
export const concurrentVoiceStore = writable<{ requests: ConcurrentVoiceRequest[]; focus: ConcurrentVoiceRequest | null; error: string | null; speaking: string | null }>({ requests: [], focus: null, error: null, speaking: null });

let coordinator: ConcurrentVoiceCoordinator | null = null;
let currentScope = '';
let monitorUsers = 0;
let stopMonitor: (() => void) | null = null;
let liveState = { active: false, userSpeaking: false, assistantSpeaking: false };
const manualPlayback = new Set<string>();
let foregroundBusy = false;

function scopeKey(): string { const scope = getCurrentScopeIdentity(); return `${scope.principal}\0${scope.workspace}`; }

async function requestJson<T>(path: string, scope: { principal: string; workspace: string }, body?: unknown): Promise<T> {
  const response = await fetch(`/api/magician/v2${path}?workspace=${encodeURIComponent(scope.workspace)}`, {
    method: body === undefined ? 'GET' : 'POST',
    headers: { 'Content-Type': 'application/json', 'X-Magician-Principal': scope.principal, 'X-Magician-Workspace': scope.workspace },
    ...(body === undefined ? {} : { body: JSON.stringify(body) })
  });
  if (!response.ok) {
    const error = await response.json().catch(() => ({}));
    throw new Error(error.error ?? `Voice request failed (${response.status})`);
  }
  return response.json() as Promise<T>;
}

function ensureCoordinator(): ConcurrentVoiceCoordinator | null {
  if (!browser || !get(scopeIdentityStore).isResolved) return null;
  const key = scopeKey();
  if (coordinator && currentScope === key) return coordinator;
  const prior = coordinator;
  coordinator = null;
  currentScope = key;
  manualPlayback.clear();
  void prior?.deactivate();
  concurrentVoiceStore.set({ requests: [], focus: null, error: null, speaking: null });
  const scope = getCurrentScopeIdentity();
  const deviceId = crypto.randomUUID();
  const interactionId = crypto.randomUUID();
  const next = new ConcurrentVoiceCoordinator({
    deviceId, interactionId, now: Date.now, id: () => crypto.randomUUID(),
    list: () => requestJson<ConcurrentVoiceSnapshot>('/media/voice/requests', scope),
    command: body => requestJson<ConcurrentVoiceSnapshot>('/media/voice/delivery', scope, body),
    eligible: () => currentScope === key && !document.hidden && !isTutorAudioFocusActive()
      && (manualPlayback.size > 0 || liveState.active || (get(ttsStore).prefs.autoSpeak && get(ttsStore).userInteracted)),
    outputBusy: () => foregroundBusy || liveState.userSpeaking || liveState.assistantSpeaking || !!get(ttsStore).activeMessageId,
    play: (request, onStarted) => new Promise<PlaybackOutcome>(resolve => {
      if (currentScope !== key) { resolve('cancelled'); return; }
      const messageId = `voice-delivery-${request.id}`;
      const finish = (status: PlaybackOutcome) => {
        if (currentScope === key) {
          concurrentVoiceStore.update(s => ({ ...s, speaking: null }));
          if (get(ttsStore).activeMessageId === messageId) ttsStore.setActive(null);
        }
        manualPlayback.delete(request.id);
        resolve(status);
      };
      const start = () => {
        if (currentScope === key) {
          concurrentVoiceStore.update(s => ({ ...s, speaking: request.id }));
          ttsStore.setActive(messageId);
        }
        // Authority can be lost during audio preparation. The coordinator may
        // stop playback synchronously here; its finish callback must clear the
        // state above rather than having it set again after cancellation.
        onStarted();
      };
      if (get(mediaProvidersStore).tts) {
        void providerSpeakBlocks({ messageId, blocks: [{ text: request.speech_text ?? '' }], onStart: start }, finish)
          .catch(() => finish('error'));
      } else if (isBrowserTtsAvailable()) {
        const prefs = get(ttsStore).prefs;
        speak({ messageId, text: request.speech_text ?? '', voiceName: prefs.voiceName, rate: prefs.rate, pitch: prefs.pitch, onStart: start }, finish);
      } else finish('error');
    }),
    stopPlayback: () => { cancelProviderSpeak(); cancelCurrent(); },
    changed: (snapshot, focus) => {
      if (currentScope === key) concurrentVoiceStore.update(s => ({ ...s, requests: snapshot.requests, focus, error: null }));
    },
    error: error => {
      if (currentScope === key) concurrentVoiceStore.update(s => ({ ...s, error: error instanceof Error ? error.message : 'Voice delivery is temporarily unavailable.' }));
    }
  });
  coordinator = next;
  return next;
}

export function startConcurrentVoiceMonitor(): () => void {
  if (!browser) return () => {};
  monitorUsers++;
  if (!stopMonitor) {
    const refresh = () => { void ensureCoordinator()?.tick(); };
    const unsubscribe = scopeIdentityStore.subscribe(() => { if (scopeKey() !== currentScope) refresh(); });
    const timer = setInterval(refresh, 1_000);
    refresh();
    stopMonitor = () => { clearInterval(timer); unsubscribe(); void coordinator?.deactivate(); coordinator = null; currentScope = ''; };
  }
  return () => { if (--monitorUsers === 0) { stopMonitor?.(); stopMonitor = null; } };
}

export function beginConcurrentVoiceCapture(): void {
  primeProviderTtsPlayback();
  ttsStore.markUserInteracted();
  ensureCoordinator()?.captureStarted();
}
export function endConcurrentVoiceCapture(): void { coordinator?.captureStopped(); }
export function settleConcurrentVoiceInput(): void { coordinator?.inputSettled(); }
export function selectConcurrentVoiceContext(request: ConcurrentVoiceRequest | null): void { ensureCoordinator()?.selectContext(request); }
export function concurrentVoiceContext(parentSessionId: string): string | undefined { return coordinator?.contextForCapture(parentSessionId); }

export async function submitConcurrentVoiceRequest(parentSessionId: string, text: string, options: Record<string, unknown> = {}, voiceInput = false): Promise<ConcurrentVoiceRequest> {
  const controller = ensureCoordinator();
  const scope = getCurrentScopeIdentity();
  const key = scopeKey();
  const target = controller?.targetForSubmission(parentSessionId, voiceInput) ?? { parentSessionId };
  const submission = { ...options, text, submission_id: crypto.randomUUID(), context_session_id: target.contextSessionId };
  controller?.activate();
  try {
    const command = text.trim().replace(/[.!?]+$/, '').trim().toLowerCase().replace(/^please /, '');
    const current = ['cancel that request', 'cancel this request', 'cancel the current request'].includes(command);
    if (current || command === 'cancel the previous request' || command === 'cancel all background requests') {
      const all = command === 'cancel all background requests';
      const state = await requestJson<ConcurrentVoiceSnapshot>('/media/voice/requests', scope);
      let requests = state.requests.filter(r => !r.task_notification && (r.work_status === 'accepted' || r.work_status === 'running' || r.pending_tasks?.length))
        .filter(r => all || (current && target.contextSessionId ? r.branch_session_id === target.contextSessionId : r.parent_session_id === target.parentSessionId))
        .sort((a, b) => b.created_at - a.created_at);
      if (!all) requests = requests.slice(0, 1);
      if (!requests.length) throw new Error('No matching background request is running.');
      for (const request of requests) await cancelConcurrentVoiceRequest(request.id);
      return requests[0];
    }
    // Reuse the exact admission key if the acknowledgement is lost. Never
    // resubmit a possibly accepted request with a new key automatically.
    let receipt: ConcurrentVoiceRequest;
    try {
      receipt = await requestJson<ConcurrentVoiceRequest>(`/chat/sessions/${encodeURIComponent(target.parentSessionId)}/voice/requests`, scope, submission);
    } catch (error) {
      if (!(error instanceof TypeError)) throw error;
      receipt = await requestJson<ConcurrentVoiceRequest>(`/chat/sessions/${encodeURIComponent(target.parentSessionId)}/voice/requests`, scope, submission);
    }
    if (scopeKey() === key) await controller?.tick();
    return receipt;
  } finally { controller?.submissionSettled(voiceInput); }
}

export async function cancelConcurrentVoiceRequest(id: string): Promise<void> {
  const scope = getCurrentScopeIdentity();
  const key = scopeKey();
  const state = await requestJson<ConcurrentVoiceSnapshot>(`/media/voice/requests/${encodeURIComponent(id)}/cancel`, scope, {});
  if (key === currentScope) coordinator?.update(state);
}

export function readConcurrentVoiceResult(id: string): void {
  primeProviderTtsPlayback(); ttsStore.markUserInteracted();
  const controller = ensureCoordinator(); manualPlayback.add(id);
  controller?.replay(id); void controller?.tick();
}

export function setConcurrentVoiceLiveState(state: typeof liveState): void {
  const wasSpeaking = liveState.userSpeaking;
  const wasActive = liveState.active;
  if (state.assistantSpeaking && !liveState.assistantSpeaking) coordinator?.foregroundStarted();
  if (!state.assistantSpeaking && liveState.assistantSpeaking) coordinator?.foregroundStopped();
  liveState = state;
  if (state.active) ensureCoordinator()?.activate();
  if (state.userSpeaking && !wasSpeaking) coordinator?.captureStarted();
  if (!state.userSpeaking && wasSpeaking) coordinator?.captureStopped();
  if (!state.active && wasActive) void coordinator?.deactivate();
}

export function setConcurrentVoiceForegroundBusy(busy: boolean): void {
  if (busy && !foregroundBusy) coordinator?.foregroundStarted();
  if (!busy && foregroundBusy) coordinator?.foregroundStopped();
  foregroundBusy = busy;
}
export async function dismissConcurrentVoiceResult(id: string): Promise<void> {
  const scope = getCurrentScopeIdentity();
  const key = scopeKey();
  const state = await requestJson<ConcurrentVoiceSnapshot>('/media/voice/delivery', scope, { action: 'dismiss', request_id: id });
  if (key === currentScope) coordinator?.update(state);
}

export async function markConcurrentVoiceResultRead(id: string): Promise<void> {
  const key = scopeKey();
  const state = await requestJson<ConcurrentVoiceSnapshot>('/media/voice/delivery', getCurrentScopeIdentity(), { action: 'read', request_id: id });
  if (key === currentScope) coordinator?.update(state);
}

export async function loadConcurrentVoiceResult(id: string): Promise<{ session_id: string; content: { text?: string; summary?: string; output_files?: ContentBlockRecord[]; content_blocks?: ContentBlockRecord[] } }> {
  return requestJson(`/media/voice/requests/${encodeURIComponent(id)}/result`, getCurrentScopeIdentity());
}
