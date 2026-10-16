import { describe, expect, it } from 'vitest';
import { classifyRef, resolveHref } from './classifyRef';

const TASK = 'task_c8523790cf9a4d0ea60f46b86721fd3d';

describe('classifyRef task outputs', () => {
    it('keeps the execution segment of a yielded file link', () => {
        const href = `/api/magician/v3/tasks/${TASK}/outputs/executions/exec_9c16/outputs/gmv_result.md`;
        const ref = classifyRef(href);
        expect(ref).toMatchObject({
            kind: 'taskOutput',
            taskId: TASK,
            relativePath: 'executions/exec_9c16/outputs/gmv_result.md',
        });
        expect(resolveHref(ref)).toBe(href);
    });

    it('still reduces a task-level output path to its file', () => {
        const ref = classifyRef(`/data/tasks/${TASK}/outputs/report.pdf`);
        expect(ref).toMatchObject({ kind: 'taskOutput', relativePath: 'report.pdf' });
    });
});
