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

    it('reads a delivered file URL as a task output and encodes it only once', () => {
        const href = `/api/magician/v3/tasks/${TASK}/outputs/delivered/exec_1/Q3%20report%232.md`;
        const ref = classifyRef(href);
        expect(ref).toEqual({
            kind: 'taskOutput',
            taskId: TASK,
            relativePath: 'delivered/exec_1/Q3 report#2.md',
        });
        // The same URL comes back, so the authenticated open fetches the served file.
        expect(resolveHref(ref)).toBe(href);
    });

    it('accepts any task id on the served route and drops traversal segments', () => {
        expect(classifyRef('/api/magician/v3/tasks/task_app_abc/outputs/delivered/e/a.csv')).toEqual({
            kind: 'taskOutput',
            taskId: 'task_app_abc',
            relativePath: 'delivered/e/a.csv',
        });
        expect(classifyRef(`/api/magician/v3/tasks/${TASK}/outputs/delivered/../../secret`)).toMatchObject({
            kind: 'taskOutput',
            relativePath: 'delivered/secret',
        });
    });

    it('keeps a task workdir file on the host-open path', () => {
        const internal = `/Users/me/MagicianNotes/internal_tasks/${TASK}/workdir/report.md`;
        expect(classifyRef(internal)).toEqual({ kind: 'filePath', absolutePath: internal });
        // A workdir folder named `outputs` is not the task's outputs directory.
        const nested = `/data/tasks/${TASK}/workdir/build/outputs/report.md`;
        expect(classifyRef(nested)).toEqual({ kind: 'filePath', absolutePath: nested });
        expect(classifyRef(`file://${internal}`)).toEqual({ kind: 'filePath', absolutePath: internal });
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
