/**
 * Where each chat gesture lands, now that both land on the same panel.
 *
 * The two destinations used to be two components — the tabbed `ExecutionPanel`
 * for a task and the tab-less `DeepWorkPanel` for a run — and this file existed
 * to keep them apart. They are one component now, so what has to stay apart is
 * the **model**: a task-status card and a task id in prose resolve a real store
 * `Task` and go through `toTaskPanelModel`; an activity card's *Inspect run*
 * carries an execution and goes through `toExecutionPanelModel`. Route either
 * one to the other's adapter and the panel would be right about the wrong thing.
 */
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

const chatPanel = readFileSync(join(process.cwd(), 'src/lib/magician/chat/ChatPanel.svelte'), 'utf8');

describe('ChatPanel inspection destinations', () => {
	it('keeps durable task cards on the task adapter and activity cards on the run adapter', () => {
		expect(chatPanel).toContain('onOpenTask={openTaskDetailsFromStatus}');
		expect(chatPanel).toMatch(
			/function openTaskDetailsFromStatus[\s\S]*?void openTaskPanel\(taskId, content\.execution_id\)/
		);
		expect(chatPanel).toContain('on:inspect={(e) => openInspectionPanelFromIds(e.detail)}');
		expect(chatPanel).toContain('task={inspectPanelModel}');
		expect(chatPanel).toContain('task={taskPanelModel}');
	});

	/**
	 * Both gestures reach one drawer component, and neither reaches a panel of
	 * its own. The two that used to answer them are gone from this file — one of
	 * them from the repo.
	 */
	it('mounts the shared drawer twice and no other panel', () => {
		expect(chatPanel.match(/<TaskPanelDrawer/g)).toHaveLength(2);
		expect(chatPanel).not.toContain('ExecutionPanel.svelte');
		expect(chatPanel).not.toContain('DeepWorkPanel');
	});

	/**
	 * The one behaviour that changed rather than moved. A status card carrying
	 * only an execution id used to synthesize `agent-cycle:<execution>`, which
	 * routes to the execution-scoped panel endpoint — not populated for the
	 * task-backed delegate runs these cards describe, so it opened a panel that
	 * could only say `execution panel state not found`. Both ids are required
	 * now, which is what `RequestActivityCard` had already decided for its own
	 * control.
	 */
	it('refuses to open a run it has no task id for', () => {
		expect(chatPanel).toMatch(
			/function inspectionTargetOf[\s\S]*?if \(!executionId \|\| !taskId\) return null;/
		);
		expect(chatPanel).not.toContain('agent-cycle:${executionId}');
	});
});
