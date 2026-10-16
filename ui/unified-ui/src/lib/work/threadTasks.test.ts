import { describe, expect, it } from 'vitest';
import type { Task } from '$lib/stores/taskStore';
import { filteredThreadTasks, tasksInThread, threadTaskCounts } from './threadTasks';

function task(id: string, status: Task['status'], thread = 'launch', updatedAt = '2026-10-06T10:00:00Z'): Task {
 return { id, status, uiThreadId: thread, title: id, description: '', source: 'task', tags: [], updatedAt, createdAt: updatedAt };
}

describe('thread task projection', () => {
 it('normalizes thread scope and computes counts without leaking other threads', () => {
  const scoped = tasksInThread([task('live', 'running', ' Launch '), task('done', 'completed'), task('foreign', 'running', 'other')], 'LAUNCH');
  expect(scoped.map(t => t.id)).toEqual(['live', 'done']);
  expect(threadTaskCounts(scoped, new Set())).toMatchObject({ running: 1, completed: 1 });
  expect(tasksInThread([task('default', 'pending', '')], 'general')).toHaveLength(1);
 });
 it('keeps execution-first ordering, newest first within a bucket, without changing the corpus', () => {
  const corpus = [task('ready', 'ready'), task('older', 'running'), task('paused', 'paused'), task('newer', 'running', 'launch', '2026-10-06T11:00:00Z')];
  expect(filteredThreadTasks(corpus, 'all', '', new Set()).map(t => t.id)).toEqual(['newer', 'older', 'paused', 'ready']);
  expect(corpus[0].id).toBe('ready');
 });
 it('retains completion grace and applies tag and search filters inside the thread', () => {
  const completed: Task = { ...task('launch checklist', 'completed'), tags: [] };
  const scoped = [completed, task('unrelated', 'running')];
  expect(filteredThreadTasks(scoped, 'inbox', 'checklist', new Set([completed.id])).map(t => t.id)).toEqual([completed.id]);
  completed.tags = [{ name: 'release', color: '#123456' }];
  expect(filteredThreadTasks(scoped, { custom: 'release' }, 'checklist', new Set()).map(t => t.id)).toEqual([completed.id]);
 });
});
