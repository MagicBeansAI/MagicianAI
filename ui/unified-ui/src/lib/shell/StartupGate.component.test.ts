import { render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import StartupGate from './StartupGate.svelte';

afterEach(() => { vi.unstubAllGlobals(); });

describe('StartupGate', () => {
  it('waits for explicit readiness and then releases initialization', async () => {
    const fetcher = vi.fn()
      .mockResolvedValueOnce({ ok: true, json: async () => ({ service: 'magician', status: 'starting', ready: false }) })
      .mockResolvedValue({ ok: true, json: async () => ({ service: 'magician', status: 'ready', ready: true }) });
    vi.stubGlobal('fetch', fetcher);
    const ready = vi.fn();
    render(StartupGate, { props: { enabled: true }, events: { ready } });
    expect(screen.getByText('Starting your workspace…')).toBeTruthy();
    expect(ready).not.toHaveBeenCalled();
    await waitFor(() => expect(ready).toHaveBeenCalledTimes(1), { timeout: 2000 });
    expect(screen.queryByText('Starting your workspace…')).toBeNull();
  });

  it('shows the Settings skeleton instead of the message while Settings waits', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true, json: async () => ({ service: 'magician', status: 'starting', ready: false }) }));
    render(StartupGate, { props: { enabled: true, skeleton: true } });
    expect(screen.getByRole('status', { name: 'Loading settings' })).toBeTruthy();
    expect(screen.queryByText('Starting your workspace…')).toBeNull();
  });

  it('still says the workspace failed when Settings was waiting on it', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true, json: async () => ({ service: 'magician', status: 'failed', ready: false }) }));
    render(StartupGate, { props: { enabled: true, skeleton: true } });
    await waitFor(() => expect(screen.getByText('Workspace could not start')).toBeTruthy());
  });

  it('preserves older-server and offline behavior', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: false, status: 404 }));
    const ready = vi.fn();
    render(StartupGate, { props: { enabled: true }, events: { ready } });
    await waitFor(() => expect(ready).toHaveBeenCalledTimes(1));
  });

  it('does not mount the application when initialization failed', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true, json: async () => ({ service: 'magician', status: 'failed', ready: false }) }));
    const ready = vi.fn();
    render(StartupGate, { props: { enabled: true }, events: { ready } });
    await waitFor(() => expect(screen.getByText('Workspace could not start')).toBeTruthy());
    expect(ready).not.toHaveBeenCalled();
  });

  it('checks again at once when a suspended panel is shown, without abandoning a probe', async () => {
    vi.useFakeTimers();
    try {
      let answer = { service: 'magician', status: 'starting', ready: false };
      const fetcher = vi.fn().mockImplementation(async () => ({ ok: true, json: async () => answer }));
      vi.stubGlobal('fetch', fetcher);
      const ready = vi.fn();
      render(StartupGate, { props: { enabled: true }, events: { ready } });
      await vi.advanceTimersByTimeAsync(0);
      expect(fetcher).toHaveBeenCalledTimes(1);
      // The panel's timer never fires (suspended); the service became ready.
      answer = { service: 'magician', status: 'ready', ready: true };
      window.dispatchEvent(new Event('focus'));
      await vi.advanceTimersByTimeAsync(0);
      expect(fetcher).toHaveBeenCalledTimes(2);
      expect(ready).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it('does not contact the backend for bypassed public surfaces', async () => {
    const fetcher = vi.fn();
    vi.stubGlobal('fetch', fetcher);
    render(StartupGate, { enabled: false });
    expect(fetcher).not.toHaveBeenCalled();
  });
});
