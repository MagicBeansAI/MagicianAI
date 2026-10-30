import { beforeEach, describe, expect, it, vi } from 'vitest';
const fixture = vi.hoisted(() => ({ read: vi.fn(), revision: 0 }));
vi.mock('./taskAttention', async original => ({
	...await original<typeof import('./taskAttention')>(), readTaskRunState: fixture.read
}));
vi.mock('$lib/stores/scopeIdentityStore', async original => ({
	...await original<typeof import('$lib/stores/scopeIdentityStore')>(),
	getCurrentScopeCredentialRevision: () => fixture.revision,
	scopeCredentialIdentityIsCurrent: (revision: number) => revision === fixture.revision
}));
import { readCompletedTaskReport } from './completedTaskReport';

const panel = (changes = {}) => ({ ok: true, state: {
	overview: { status: 'completed', task_id: 'task', execution_id: 'run', principal: 'owner', workspace: 'space', ...changes },
	output: { result: { report: '<h1>Actual report</h1>' } }
} });
beforeEach(() => { fixture.read.mockReset(); fixture.revision += 1; });

describe('completed task reports', () => {
	it('shares simultaneous reads and reuses an immutable report for the same scoped run', async () => {
		fixture.read.mockResolvedValue(panel());
		expect(await Promise.all([
			readCompletedTaskReport('task', 'run', 'owner', 'space'),
			readCompletedTaskReport('task', 'run', 'owner', 'space')
		])).toEqual(['<h1>Actual report</h1>', '<h1>Actual report</h1>']);
		expect(await readCompletedTaskReport('task', 'run', 'owner', 'space')).toContain('Actual report');
		expect(fixture.read).toHaveBeenCalledTimes(1);
	});
	it.each([{ execution_id: 'different-run' }, { task_id: 'different-task' }, { principal: 'different-owner' }, { workspace: 'different-space' }])('rejects a mismatched document: %s', async changes => {
		fixture.read.mockResolvedValue(panel(changes));
		expect(await readCompletedTaskReport('task', 'run', 'owner', 'space')).toBeNull();
	});
	it('discards a response and cached content from an earlier login', async () => {
		let finish!: (value: ReturnType<typeof panel>) => void;
		fixture.read.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
		const waiting = readCompletedTaskReport('task', 'run', 'owner', 'space');
		fixture.revision += 1;
		finish(panel());
		expect(await waiting).toBeNull();
		fixture.read.mockResolvedValue(panel());
		expect(await readCompletedTaskReport('task', 'run', 'owner', 'space')).toContain('Actual report');
		expect(fixture.read).toHaveBeenCalledTimes(2);
	});
	it('keeps an unavailable report retryable and rejects oversized UTF-8 content', async () => {
		fixture.read.mockResolvedValueOnce({ ok: false, reason: 'offline' });
		expect(await readCompletedTaskReport('task', 'run', 'owner', 'space')).toBeNull();
		const large = panel(); large.state.output.result.report = '界'.repeat(100_000);
		fixture.read.mockResolvedValueOnce(large);
		expect(await readCompletedTaskReport('task', 'run', 'owner', 'space')).toBeNull();
		fixture.read.mockResolvedValueOnce(panel());
		expect(await readCompletedTaskReport('task', 'run', 'owner', 'space')).toContain('Actual report');
	});
});
