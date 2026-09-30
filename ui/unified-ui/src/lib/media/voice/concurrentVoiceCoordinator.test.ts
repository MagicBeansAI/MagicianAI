import { describe, expect, it, vi } from 'vitest';
import { isVoiceRequestVisible, ConcurrentVoiceCoordinator, type ConcurrentVoiceRequest, type ConcurrentVoiceSnapshot, type PlaybackOutcome } from './concurrentVoiceCoordinator';

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(r => { resolve = r; });
  return { promise, resolve };
}

function fixture() {
  let now = 10_000;
  let capturingDuringClaim = false;
  let outputBusy = false;
  const state: ConcurrentVoiceSnapshot = { revision: 0, requests: [], output: undefined };
  const ends: ReturnType<typeof deferred<PlaybackOutcome>>[] = [];
  const starts: (() => void)[] = [];
  const command = vi.fn(async (body: Record<string, unknown>) => {
    state.revision++;
    if (body.action === 'acquire') state.output = { device_id: 'device', interaction_id: 'call', epoch: 1, expires_at: now + 30_000 };
    const record = state.requests.find(r => r.id === body.request_id);
    if (body.action === 'claim' && record) {
      record.delivery_status = 'claimed';
      if (capturingDuringClaim) coordinator.captureStarted();
    }
    if (body.action === 'playback' && record) {
      record.delivery_status = body.event === 'completed' ? 'played' : body.event === 'interrupted' ? 'deferred' : body.event === 'rejected' ? 'pending' : record.delivery_status;
    }
    return structuredClone(state);
  });
  const stopPlayback = vi.fn(() => ends.at(-1)?.resolve('cancelled'));
  const play = vi.fn((_request: ConcurrentVoiceRequest, started: () => void) => {
    const end = deferred<PlaybackOutcome>();
    starts.push(started);
    ends.push(end);
    return end.promise;
  });
  const coordinator = new ConcurrentVoiceCoordinator({ deviceId: 'device', interactionId: 'call', now: () => now,
    id: () => `attempt-${ends.length}`, list: async () => structuredClone(state), command,
    eligible: () => true, outputBusy: () => outputBusy, play, stopPlayback, changed: vi.fn(), error: vi.fn() });
  const add = (id: string) => state.requests.push({ id, title: id, parent_session_id: 'parent', branch_session_id: `branch-${id}`,
    chat_turn_id: id, source_surface: 'web', work_status: 'completed', delivery_status: 'pending', speech_text: `Answer ${id}`, created_at: now, updated_at: now });
  return { coordinator, state, command, play, starts, ends, add, stopPlayback, setBusy: (value: boolean) => { outputBusy = value; }, advance: () => { now += 1000; }, raceClaim: () => { capturingDuringClaim = true; } };
}

describe('concurrent voice delivery', () => {
  it('queues several ready results through physical completion, not generation completion', async () => {
    const f = fixture(); f.add('a'); f.add('b'); f.coordinator.activate();
    await f.coordinator.tick(); expect(f.play).toHaveBeenCalledTimes(1);
    f.starts[0](); await f.coordinator.tick(); expect(f.play).toHaveBeenCalledTimes(1);
    f.ends[0].resolve('completed'); await vi.waitFor(() => expect(f.state.requests[0].delivery_status).toBe('played'));
    f.advance(); await f.coordinator.tick(); expect(f.play).toHaveBeenCalledTimes(2);
    expect(f.play.mock.calls[1][0].id).toBe('b');
  });

  it('holds ready answers through capture and unresolved transcription', async () => {
    const f = fixture(); f.add('a'); f.coordinator.captureStarted();
    await f.coordinator.tick(); expect(f.play).not.toHaveBeenCalled();
    f.coordinator.captureStopped(); await f.coordinator.tick(); expect(f.play).not.toHaveBeenCalled();
    f.coordinator.inputSettled(); f.advance(); await f.coordinator.tick(); expect(f.play).toHaveBeenCalledOnce();
  });

  it('rejects an offered result if capture starts during the claim round trip', async () => {
    const f = fixture(); f.add('a'); f.coordinator.activate(); f.raceClaim();
    await f.coordinator.tick(); expect(f.play).not.toHaveBeenCalled();
    expect(f.state.requests[0].delivery_status).toBe('pending');
  });

  it('barge-in keeps work completed and freezes the context the user heard', async () => {
    const f = fixture(); f.add('a'); f.add('b'); f.coordinator.activate();
    await f.coordinator.tick(); f.starts[0]();
    const focus = f.coordinator.captureStarted(); expect(focus?.id).toBe('a');
    await vi.waitFor(() => expect(f.state.requests[0].delivery_status).toBe('deferred'));
    expect(f.state.requests[0].work_status).toBe('completed');
    expect(f.coordinator.contextForCapture('parent')).toBe('branch-a');
    await f.coordinator.tick(); expect(f.play).toHaveBeenCalledTimes(1);
    expect(f.command.mock.calls.some(([c]) => c.action === 'cancel')).toBe(false);
  });

  it('leaves completed background work quiet outside an active interaction', async () => {
    const f = fixture(); f.add('a'); await f.coordinator.tick(); expect(f.play).not.toHaveBeenCalled();
  });
});

it('a foreground reply interrupts only presentation, without opening a microphone gate', async () => {
  const f = fixture(); f.add('a'); f.add('b'); f.coordinator.activate();
  await f.coordinator.tick(); f.starts[0]();
  f.coordinator.foregroundStarted();
  await vi.waitFor(() => expect(f.state.requests[0].delivery_status).toBe('deferred'));
  expect(f.state.requests[0].work_status).toBe('completed');
  f.advance(); await f.coordinator.tick();
  expect(f.play).toHaveBeenCalledTimes(2);
  expect(f.play.mock.calls[1][0].id).toBe('b');
});

it('disconnect preserves work and replay needs an explicit request', async () => {
  const f = fixture(); f.add('a'); f.coordinator.activate();
  await f.coordinator.tick(); f.starts[0]();
  await f.coordinator.deactivate();
  await vi.waitFor(() => expect(f.state.requests[0].delivery_status).toBe('deferred'));
  f.advance(); f.coordinator.activate(); await f.coordinator.tick();
  expect(f.play).toHaveBeenCalledTimes(1);
  f.coordinator.replay('a'); await f.coordinator.tick();
  expect(f.play).toHaveBeenCalledTimes(2);
  expect(f.command.mock.calls.some(([command]) => command.action === 'cancel')).toBe(false);
});

it('retains several explicit read requests made while another reply is playing', async () => {
  const f = fixture(); f.add('a'); f.add('b'); f.add('c');
  for (const request of f.state.requests) request.delivery_status = 'played';
  f.coordinator.replay('a'); await f.coordinator.tick(); f.starts[0]();
  f.coordinator.replay('b'); f.coordinator.replay('c');
  f.ends[0].resolve('completed'); await vi.waitFor(() => expect(f.state.requests[0].delivery_status).toBe('played'));
  f.advance(); await f.coordinator.tick(); expect(f.play.mock.calls[1][0].id).toBe('b'); f.starts[1]();
  f.ends[1].resolve('completed'); await vi.waitFor(() => expect(f.state.requests[1].delivery_status).toBe('played'));
  f.advance(); await f.coordinator.tick(); expect(f.play.mock.calls[2][0].id).toBe('c');
});

it('a response from another conversation becomes the addressed context before capture', async () => {
  const f = fixture(); f.add('a'); f.state.requests[0].parent_session_id = 'other-parent';
  f.coordinator.activate(); await f.coordinator.tick(); f.starts[0]();
  f.coordinator.captureStarted();
  expect(f.coordinator.targetForCapture('current-parent')).toEqual({ parentSessionId: 'other-parent', contextSessionId: 'branch-a' });
});

it('a later topic selection cannot retarget an utterance awaiting transcription', () => {
  const f = fixture(); f.add('a'); f.add('b');
  f.coordinator.selectContext(f.state.requests[0]);
  f.coordinator.captureStarted(); f.coordinator.captureStopped();
  f.coordinator.selectContext(f.state.requests[1]);
  expect(f.coordinator.contextForCapture('parent')).toBe('branch-a');
  f.coordinator.inputSettled();
  expect(f.coordinator.contextForCapture('parent')).toBe('branch-b');
});

it('disconnect clears unfinished capture so a later explicit replay can proceed', async () => {
  const f = fixture(); f.add('a'); f.coordinator.captureStarted();
  await f.coordinator.deactivate();
  f.coordinator.replay('a'); await f.coordinator.tick();
  expect(f.play).toHaveBeenCalledOnce();
  expect(f.state.requests[0].work_status).toBe('completed');
});


it('shows active and unread work, keeps selected context, and hides consumed results', () => {
  const f = fixture(); f.add('a');
  const row = f.state.requests[0];
  expect(isVoiceRequestVisible(row)).toBe(true);
  row.read_at = 42;
  expect(isVoiceRequestVisible(row)).toBe(false);
  expect(isVoiceRequestVisible(row, row.id)).toBe(true);
  row.pending_tasks = ['task'];
  expect(isVoiceRequestVisible(row)).toBe(true);
  row.pending_tasks = []; row.delivery_status = 'played'; row.read_at = undefined;
  expect(isVoiceRequestVisible(row)).toBe(false);
  expect(isVoiceRequestVisible(row, row.id)).toBe(true);
  row.delivery_status = 'dismissed';
  expect(isVoiceRequestVisible(row, row.id)).toBe(false);
});

it('read acknowledgements suppress automatic speech but allow explicit replay', async () => {
  const f = fixture(); f.add('a'); f.state.requests[0].read_at = 42; f.coordinator.activate();
  await f.coordinator.tick(); expect(f.play).not.toHaveBeenCalled();
  f.coordinator.replay('a'); await f.coordinator.tick(); expect(f.play).toHaveBeenCalledOnce();
});

it('dismissal clears selection while preserving an already captured utterance', () => {
  const f = fixture(); f.add('a'); f.coordinator.update(structuredClone(f.state));
  f.coordinator.selectContext(f.state.requests[0]); f.coordinator.captureStarted();
  f.state.requests[0].delivery_status = 'dismissed'; f.state.revision++;
  f.coordinator.update(structuredClone(f.state));
  expect(f.coordinator.contextForCapture('parent')).toBe('branch-a');
  f.coordinator.captureStopped(); f.coordinator.inputSettled();
  expect(f.coordinator.contextForCapture('parent')).toBeUndefined();
});

it('typed admission keeps its displayed session and preserves unresolved voice context', async () => {
  const f = fixture(); f.add('a'); f.add('b'); f.coordinator.activate();
  f.coordinator.selectContext(f.state.requests[0]);
  f.coordinator.captureStarted(); f.coordinator.captureStopped();
  f.coordinator.selectContext(f.state.requests[1]);
  expect(f.coordinator.targetForSubmission('typed-parent', false)).toEqual({ parentSessionId: 'typed-parent' });
  f.coordinator.submissionSettled(false);
  expect(f.coordinator.targetForSubmission('typed-parent', true)).toEqual({ parentSessionId: 'parent', contextSessionId: 'branch-a' });
  f.advance(); await f.coordinator.tick(); expect(f.play).not.toHaveBeenCalled();
  f.coordinator.submissionSettled(true); f.advance(); await f.coordinator.tick();
  expect(f.play).toHaveBeenCalledOnce();
  expect(f.command.mock.calls.some(([c]) => c.action === 'cancel')).toBe(false);
});

it('ready voice results wait through typed foreground work and resume when it drains', async () => {
  const f = fixture(); f.add('a'); f.add('b'); f.coordinator.activate();
  f.setBusy(true); f.coordinator.foregroundStarted();
  await f.coordinator.tick(); f.advance(); await f.coordinator.tick();
  expect(f.play).not.toHaveBeenCalled();
  expect(f.state.requests.every(r => r.work_status === 'completed')).toBe(true);
  f.setBusy(false); f.coordinator.foregroundStopped();
  await f.coordinator.tick(); expect(f.play).not.toHaveBeenCalled();
  f.advance(); await f.coordinator.tick(); expect(f.play).toHaveBeenCalledOnce();
  f.starts[0](); f.ends[0].resolve('completed');
  await vi.waitFor(() => expect(f.state.requests[0].delivery_status).toBe('played'));
  f.advance(); await f.coordinator.tick(); expect(f.play).toHaveBeenCalledTimes(2);
  expect(f.command.mock.calls.some(([c]) => c.action === 'cancel')).toBe(false);
});
