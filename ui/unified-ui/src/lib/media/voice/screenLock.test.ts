import { get } from 'svelte/store';
import { afterEach, describe, expect, it, vi } from 'vitest';

import {
	isScreenLocked,
	normalizeIdleScreenState,
	screenLockStateStore,
	startScreenLockMonitoring,
	stopScreenLockMonitoring
} from './screenLock';

class FakeIdleDetector extends EventTarget {
	static permission = 'granted';
	static last: FakeIdleDetector | null = null;
	static async requestPermission(): Promise<string> {
		return FakeIdleDetector.permission;
	}
	screenState: string | null = 'unlocked';
	constructor() {
		super();
		FakeIdleDetector.last = this;
	}
	async start(): Promise<void> {}
	lock(): void {
		this.screenState = 'locked';
		this.dispatchEvent(new Event('change'));
	}
}

afterEach(() => {
	stopScreenLockMonitoring();
	FakeIdleDetector.permission = 'granted';
	FakeIdleDetector.last = null;
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

describe('screen lock monitoring', () => {
	it('normalizes only exact locked and unlocked states', () => {
		expect(normalizeIdleScreenState('locked')).toBe('locked');
		expect(normalizeIdleScreenState('unlocked')).toBe('unlocked');
		expect(normalizeIdleScreenState('idle')).toBe('unknown');
	});

	it('does not mistake an unsupported browser for a locked screen', async () => {
		vi.stubGlobal('IdleDetector', undefined);
		expect(await startScreenLockMonitoring(true)).toBe('unsupported');
		expect(isScreenLocked()).toBe(false);
	});

	it('does not treat a hidden document as a locked screen', async () => {
		vi.stubGlobal('document', { visibilityState: 'hidden' });
		vi.stubGlobal('IdleDetector', undefined);
		await startScreenLockMonitoring(true);
		expect(isScreenLocked()).toBe(false);
	});

	it('publishes an actual detector lock transition', async () => {
		vi.stubGlobal('IdleDetector', FakeIdleDetector);
		expect(await startScreenLockMonitoring(true)).toBe('unlocked');
		FakeIdleDetector.last?.lock();
		expect(isScreenLocked()).toBe(true);
		expect(get(screenLockStateStore)).toBe('locked');
		FakeIdleDetector.last!.screenState = 'unlocked';
		FakeIdleDetector.last!.dispatchEvent(new Event('change'));
		expect(get(screenLockStateStore)).toBe('unlocked');
	});

	it('refreshes the detector state before a guided-flow send', async () => {
		vi.stubGlobal('IdleDetector', FakeIdleDetector);
		await startScreenLockMonitoring(true);
		FakeIdleDetector.last!.screenState = 'locked';
		expect(await startScreenLockMonitoring(false)).toBe('locked');
		expect(isScreenLocked()).toBe(true);
	});

	it('leaves state unknown when lock permission is denied', async () => {
		FakeIdleDetector.permission = 'denied';
		vi.stubGlobal('IdleDetector', FakeIdleDetector);
		expect(await startScreenLockMonitoring(true)).toBe('unknown');
		expect(isScreenLocked()).toBe(false);
	});

	it('fails open without starting a flow blocker when permission throws', async () => {
		vi.spyOn(FakeIdleDetector, 'requestPermission').mockRejectedValueOnce(
			new DOMException('not allowed', 'NotAllowedError')
		);
		vi.stubGlobal('IdleDetector', FakeIdleDetector);
		expect(await startScreenLockMonitoring(true)).toBe('unknown');
		expect(isScreenLocked()).toBe(false);
	});

	it('upgrades a passive permission query to the later user-gesture prompt', async () => {
		let resolvePassive!: (value: { state: PermissionState }) => void;
		const passivePermission = new Promise<{ state: PermissionState }>((resolve) => {
			resolvePassive = resolve;
		});
		vi.stubGlobal('navigator', {
			permissions: { query: vi.fn(() => passivePermission) }
		});
		vi.stubGlobal('IdleDetector', FakeIdleDetector);

		const passiveStart = startScreenLockMonitoring(false);
		await Promise.resolve();
		const gestureStart = startScreenLockMonitoring(true);

		expect(await gestureStart).toBe('unlocked');
		resolvePassive({ state: 'granted' });
		expect(await passiveStart).toBe('unlocked');
		expect(FakeIdleDetector.last).not.toBeNull();
	});

	it('cannot resurrect monitoring after stop while permission is pending', async () => {
		let resolvePermission!: (value: string) => void;
		vi.spyOn(FakeIdleDetector, 'requestPermission').mockImplementationOnce(
			() => new Promise<string>((resolve) => { resolvePermission = resolve; })
		);
		vi.stubGlobal('IdleDetector', FakeIdleDetector);

		const pending = startScreenLockMonitoring(true);
		await Promise.resolve();
		stopScreenLockMonitoring();
		resolvePermission('granted');

		expect(await pending).toBe('unknown');
		expect(get(screenLockStateStore)).toBe('unknown');
		expect(FakeIdleDetector.last).toBeNull();
	});
});
