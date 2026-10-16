// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
const socket = vi.hoisted(() => ({ events: null as ((events: unknown[]) => void) | null,
  connection: null as ((status: string) => void) | null, offEvents: vi.fn(), offConnection: vi.fn() }));
vi.mock('$lib/realtime/v2-websocket', () => ({
  getV2EventSequence: (event: { sequence: number }) => event.sequence,
  v2Events: { subscribe: (fn: typeof socket.events) => { socket.events = fn; return socket.offEvents; },
    connectionStatus: { subscribe: (fn: typeof socket.connection) => { socket.connection = fn; return socket.offConnection; } },
    getConnectionState: () => 'OPEN', connectGlobal: vi.fn() },
}));
import { watchAppLiveCollection, type AppLiveCollection } from './appLiveCollection';

let stop: (() => void) | undefined;
beforeEach(() => { vi.useFakeTimers(); Object.defineProperty(document, 'hidden', { value: false, configurable: true }); });
afterEach(() => { stop?.(); stop = undefined; vi.useRealTimers(); vi.clearAllMocks(); });
function collection(reopen?: () => void) {
  const value = { state: { moreChanges: false }, synchronize: vi.fn(async () => {}), dispose: vi.fn() };
  stop = watchAppLiveCollection(value as unknown as AppLiveCollection, 'install_1', { reopen }); return value;
}
it('requests fresh binding after an app update instead of retrying an obsolete revision forever', async () => {
  const reopen = vi.fn(); const c = collection(reopen);
  c.synchronize.mockRejectedValueOnce(Object.assign(new Error('updated'), { reasonCode: 'app_surface_stale' }));
  await vi.advanceTimersByTimeAsync(50); expect(reopen).toHaveBeenCalledOnce();
  await vi.advanceTimersByTimeAsync(60000); expect(c.synchronize).toHaveBeenCalledOnce();
});
it('pauses while hidden, catches up on return/reconnect, and disposes listeners', async () => {
  const c = collection(); await vi.advanceTimersByTimeAsync(50); expect(c.synchronize).toHaveBeenCalledTimes(1);
  Object.defineProperty(document, 'hidden', { value: true, configurable: true }); document.dispatchEvent(new Event('visibilitychange'));
  await vi.advanceTimersByTimeAsync(60000); expect(c.synchronize).toHaveBeenCalledTimes(1);
  Object.defineProperty(document, 'hidden', { value: false, configurable: true }); document.dispatchEvent(new Event('visibilitychange'));
  await vi.advanceTimersByTimeAsync(50); expect(c.synchronize).toHaveBeenCalledTimes(2);
  socket.connection?.('connected'); await vi.advanceTimersByTimeAsync(50); expect(c.synchronize).toHaveBeenCalledTimes(3);
  stop?.(); stop = undefined; await vi.advanceTimersByTimeAsync(60000);
  expect(c.dispose).toHaveBeenCalledOnce(); expect(socket.offEvents).toHaveBeenCalledOnce();
});
it('does not overlap requests and yields between bounded catch-up pages', async () => {
  const c = collection(); let release!: () => void;
  c.synchronize.mockImplementationOnce(() => new Promise<void>((resolve) => { release = resolve; }));
  await vi.advanceTimersByTimeAsync(50); socket.connection?.('connected');
  await vi.advanceTimersByTimeAsync(60000); expect(c.synchronize).toHaveBeenCalledTimes(1);
  c.state.moreChanges = true; release(); await vi.advanceTimersByTimeAsync(50);
  expect(c.synchronize).toHaveBeenCalledTimes(2);
  c.state.moreChanges = false; await vi.advanceTimersByTimeAsync(50); expect(c.synchronize).toHaveBeenCalledTimes(3);
  await vi.advanceTimersByTimeAsync(500); expect(c.synchronize).toHaveBeenCalledTimes(3);
});
it('backs off failures instead of spinning on an unavailable server', async () => {
  const c = collection(); c.synchronize.mockRejectedValueOnce(new Error('offline'));
  await vi.advanceTimersByTimeAsync(50); await vi.advanceTimersByTimeAsync(1999); expect(c.synchronize).toHaveBeenCalledTimes(1);
  await vi.advanceTimersByTimeAsync(1); expect(c.synchronize).toHaveBeenCalledTimes(2);
});
it('uses only matching app events as wake-ups, with transport gaps caught up durably', async () => {
  const c = collection(); await vi.advanceTimersByTimeAsync(50);
  const event = (sequence: number, installation_id: string) => ({ sequence, event_type: 'AgentEvent', data: { event: {
    event_type: 'app.entity.changed', payload: { installation_id, surface_revision: 1, first_change_sequence: 1,
      last_change_sequence: 1, reset_required: false, changes: [{ entity: 'message', record_id: 'one', record_revision: 1, change_sequence: 1 }] } } } });
  socket.events?.([event(1, 'install_other')]); await vi.advanceTimersByTimeAsync(50); expect(c.synchronize).toHaveBeenCalledTimes(1);
  socket.events?.([event(2, 'install_1')]); await vi.advanceTimersByTimeAsync(50); expect(c.synchronize).toHaveBeenCalledTimes(2);
  socket.events?.([event(4, 'install_other')]); await vi.advanceTimersByTimeAsync(50); expect(c.synchronize).toHaveBeenCalledTimes(3);
});
