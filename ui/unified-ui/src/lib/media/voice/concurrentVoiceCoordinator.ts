/** One physical speaker for many independently running requests. No work
 * cancellation is performed by capture, interruption, or disconnect. */
export interface ConcurrentVoiceRequest {
  id: string;
  parent_session_id: string;
  branch_session_id: string;
  ui_thread_id?: string;
  context_session_id?: string;
  read_at?: number;
  chat_turn_id: string;
  title: string;
  source_surface: string;
  presence_session_id?: string;
  work_status: 'accepted' | 'running' | 'completed' | 'failed' | 'cancelled' | 'interrupted';
  delivery_status: 'waiting' | 'pending' | 'claimed' | 'playing' | 'played' | 'deferred' | 'uncertain' | 'dismissed';
  speech_text?: string;
  result_message_id?: string;
  pending_tasks?: string[];
  task_notification?: boolean;
  error?: string;
  created_at: number;
  updated_at: number;
  attempt?: { id: string; output_epoch: number; focus_epoch: number };
}

/** Completed results stay until read/heard; selected context remains visible. */
export function isVoiceRequestVisible(r: ConcurrentVoiceRequest, selectedId?: string): boolean {
  const active = r.work_status === 'accepted' || r.work_status === 'running'
    || (!!r.pending_tasks?.length && r.work_status !== 'cancelled');
  return active || r.delivery_status === 'claimed' || r.delivery_status === 'playing'
    || (r.delivery_status !== 'dismissed' && (r.id === selectedId
      || (r.read_at == null && r.delivery_status !== 'played')));
}

export interface ConcurrentVoiceSnapshot {
  revision: number;
  requests: ConcurrentVoiceRequest[];
  output?: { device_id: string; interaction_id: string; epoch: number; expires_at: number };
}

export type PlaybackOutcome = 'completed' | 'cancelled' | 'error';
export interface VoiceCoordinatorPorts {
  deviceId: string;
  interactionId: string;
  now(): number;
  id(): string;
  list(): Promise<ConcurrentVoiceSnapshot>;
  command(command: Record<string, unknown>): Promise<ConcurrentVoiceSnapshot>;
  eligible(): boolean;
  outputBusy(): boolean;
  play(request: ConcurrentVoiceRequest, onStarted: () => void): Promise<PlaybackOutcome>;
  stopPlayback(): void;
  changed(snapshot: ConcurrentVoiceSnapshot, focus: ConcurrentVoiceRequest | null): void;
  error(error: unknown): void;
}

export class ConcurrentVoiceCoordinator {
  private snapshot: ConcurrentVoiceSnapshot = { revision: -1, requests: [] };
  private focus: ConcurrentVoiceRequest | null = null;
  private captureFocus: ConcurrentVoiceRequest | null = null;
  private focusEpoch = 0;
  private capturing = false;
  private inputPending = false;
  private active = false;
  private busy = false;
  private quietAfter = 0;
  private lastLeaseRefresh = 0;
  private playing: { request: ConcurrentVoiceRequest; attemptId: string; epoch: number; started: boolean } | null = null;
  private explicitReplay: string[] = [];

  constructor(private readonly ports: VoiceCoordinatorPorts) {}

  activate(): void { this.active = true; }

  async deactivate(): Promise<void> {
    this.active = false;
    this.focusEpoch += 1;
    this.capturing = false;
    this.inputPending = false;
    this.captureFocus = this.focus;
    if (this.playing) this.ports.stopPlayback();
    const output = this.snapshot.output;
    if (output?.device_id === this.ports.deviceId && output.interaction_id === this.ports.interactionId) {
      try { this.update(await this.ports.command({ action: 'release', ...this.identity(output.epoch) })); }
      catch (error) { this.ports.error(error); }
    }
  }

  /** Capture-start freezes the referent before STT or a late result can alter it. */
  captureStarted(): ConcurrentVoiceRequest | null {
    this.activate();
    this.captureFocus = this.focus;
    this.focusEpoch += 1;
    this.capturing = true;
    this.inputPending = true;
    if (this.playing) this.ports.stopPlayback();
    return this.captureFocus;
  }

  foregroundStarted(): void {
    this.focusEpoch += 1;
    if (this.playing) this.ports.stopPlayback();
  }

  foregroundStopped(): void { this.quietAfter = this.ports.now() + 500; }

  captureStopped(): void { this.capturing = false; }

  inputSettled(): void {
    this.inputPending = false;
    this.captureFocus = this.focus;
    this.quietAfter = this.ports.now() + 500;
  }

  targetForCapture(parentSessionId: string): { parentSessionId: string; contextSessionId?: string } {
    const context = this.captureFocus;
    return context ? { parentSessionId: context.parent_session_id, contextSessionId: context.branch_session_id } : { parentSessionId };
  }

  /** Typed work belongs to the visible chat; it cannot consume a voice capture. */
  targetForSubmission(parentSessionId: string, voiceInput: boolean): { parentSessionId: string; contextSessionId?: string } {
    return voiceInput ? this.targetForCapture(parentSessionId) : { parentSessionId };
  }

  submissionSettled(voiceInput: boolean): void { if (voiceInput) this.inputSettled(); }

  contextForCapture(parentSessionId: string): string | undefined {
    return this.captureFocus?.parent_session_id === parentSessionId ? this.captureFocus.branch_session_id : undefined;
  }

  selectContext(request: ConcurrentVoiceRequest | null): void {
    this.focusEpoch += 1;
    this.focus = request;
    if (!this.capturing && !this.inputPending) this.captureFocus = request;
    this.ports.changed(this.snapshot, this.focus);
  }

  replay(requestId: string): void {
    this.activate();
    if (!this.explicitReplay.includes(requestId) && this.playing?.request.id !== requestId) this.explicitReplay.push(requestId);
    this.quietAfter = this.ports.now();
  }

  update(snapshot: ConcurrentVoiceSnapshot): void {
    if (snapshot.revision < this.snapshot.revision) return;
    this.snapshot = snapshot;
    if (this.focus) {
      const current = snapshot.requests.find(r => r.id === this.focus?.id && r.delivery_status !== 'dismissed');
      if (!current) { this.selectContext(null); return; }
      this.focus = current;
    }
    this.ports.changed(snapshot, this.focus);
  }

  private identity(epoch: number): Record<string, unknown> {
    return { device_id: this.ports.deviceId, interaction_id: this.ports.interactionId, epoch };
  }

  private safeToSpeak(): boolean {
    return this.active && !this.capturing && !this.inputPending
      && this.ports.eligible() && !this.ports.outputBusy()
      && this.ports.now() >= this.quietAfter;
  }

  /** Polling is a recovery path too: a lost live event never loses a result. */
  async tick(): Promise<void> {
    if (this.busy) return;
    this.busy = true;
    try {
      this.update(await this.ports.list());
      if (!this.active) return;
      if (this.playing || this.safeToSpeak()) {
        if (this.ports.now() - this.lastLeaseRefresh > 8_000 || !this.snapshot.output) {
          this.update(await this.ports.command({ action: 'acquire', device_id: this.ports.deviceId, interaction_id: this.ports.interactionId }));
          this.lastLeaseRefresh = this.ports.now();
        }
      }
      if (this.playing) {
        const current = this.playing;
        this.update(await this.ports.command({ action: 'playback', ...this.identity(current.epoch), request_id: current.request.id, attempt_id: current.attemptId, event: 'progress' }));
        return;
      }
      if (!this.safeToSpeak()) return;
      const replayId = this.explicitReplay[0];
      const request = replayId
        ? this.snapshot.requests.find(r => r.id === replayId && r.speech_text)
        : [...this.snapshot.requests].filter(r => r.delivery_status === 'pending' && r.read_at == null && r.speech_text)
          .sort((a, b) => a.updated_at - b.updated_at || a.created_at - b.created_at)[0];
      const epoch = this.snapshot.output?.epoch;
      if (!request || !epoch) return;
      const focusEpoch = this.focusEpoch;
      const attemptId = this.ports.id();
      this.update(await this.ports.command({ action: 'claim', ...this.identity(epoch), request_id: request.id,
        attempt_id: attemptId, focus_epoch: focusEpoch, replay: replayId === request.id }));
      if (this.explicitReplay[0] === request.id) this.explicitReplay.shift();
      // The microphone may have opened during the network round trip.
      if (!this.safeToSpeak() || focusEpoch !== this.focusEpoch) {
        this.update(await this.ports.command({ action: 'playback', ...this.identity(epoch), request_id: request.id, attempt_id: attemptId, event: 'rejected' }));
        return;
      }
      const playback = { request, attemptId, epoch, started: false };
      this.playing = playback;
      void this.present(playback, focusEpoch);
    } catch (error) {
      // Losing authority while playing must stop physical output immediately.
      if (this.playing) this.ports.stopPlayback();
      this.ports.error(error);
    } finally {
      this.busy = false;
    }
  }

  private async present(playback: NonNullable<ConcurrentVoiceCoordinator['playing']>, focusEpoch: number): Promise<void> {
    let startedReceipt: Promise<void> = Promise.resolve();
    try {
      const outcome = await this.ports.play(playback.request, () => {
        if (!this.active || this.capturing || this.inputPending || focusEpoch !== this.focusEpoch) {
          this.ports.stopPlayback();
          return;
        }
        if (playback.started) return;
        playback.started = true;
        this.focus = playback.request;
        this.captureFocus = playback.request;
        this.ports.changed(this.snapshot, this.focus);
        startedReceipt = this.ports.command({ action: 'playback', ...this.identity(playback.epoch), request_id: playback.request.id, attempt_id: playback.attemptId, event: 'started' })
          .then(snapshot => this.update(snapshot));
        // Attach rejection handling immediately, while audio is still playing.
        void startedReceipt.catch(error => { this.ports.stopPlayback(); this.ports.error(error); });
      });
      await startedReceipt;
      this.update(await this.ports.command({ action: 'playback', ...this.identity(playback.epoch), request_id: playback.request.id,
        attempt_id: playback.attemptId, event: outcome === 'completed' && playback.started ? 'completed' : 'interrupted' }));
    } catch (error) {
      this.ports.error(error);
    } finally {
      if (this.playing === playback) this.playing = null;
      this.quietAfter = this.ports.now() + 500;
    }
  }
}
