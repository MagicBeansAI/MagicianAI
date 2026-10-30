import { describe, expect, it } from 'vitest';
import { classifyRef, resolveHref } from './classifyRef';

const TASK = 'task_c8523790cf9a4d0ea60f46b86721fd3d';

describe('classifyRef task outputs', () => {
    it('resolves a task URL before treating dots in IDs or focus parameters as filenames', () => {
        expect(resolveHref(classifyRef('/tasks/task.one?execution_id=run.two#output')))
            .toBe('/work?execution_id=run.two&selected=task.one#output');
        expect(resolveHref(classifyRef(`/tasks/${TASK}`))).toBe(`/work?selected=${TASK}`);
    });
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


describe('workspace file references', () => {
    it.each(['Pages/Chapter-Map.md', 'Workspaces/Novel/Pages/chapter-map.json', './Reports/latest.html'])(
        'keeps %s on the scoped file-action path', (path) => {
            expect(classifyRef(path)).toEqual({ kind: 'filePath', absolutePath: path });
        }
    );
    it.each(['//other.example/report.md', 'javascript:open/report.md', 'https://example.com/report.md'])(
        'does not treat a URL as a workspace path: %s', (path) => {
            expect(classifyRef(path).kind).not.toBe('filePath');
        }
    );
});
