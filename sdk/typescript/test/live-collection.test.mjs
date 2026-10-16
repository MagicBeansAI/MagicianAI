import test from 'node:test';
import assert from 'node:assert/strict';
import { AppLiveCollection, compareAppTimestamps } from '../dist/live-collection.js';

test('timestamp ordering preserves nanoseconds and equivalent timezone representations', () => {
  assert.ok(compareAppTimestamps('2026-09-11T10:00:00.000000001Z', '2026-09-11T10:00:00Z') > 0);
  assert.equal(compareAppTimestamps('2026-09-11T15:30:00.123456789+05:30', '2026-09-11T10:00:00.123456789Z'), 0);
});

const row = (id, time = Number(id), revision = 1, category = 'feed') => ({ entity: 'message',
  record_id: String(id), record_revision: revision, fields: { id: String(id), time, category, body: `message ${id}` } });
const compare = (a, b) => b.fields.time - a.fields.time || (a.record_id < b.record_id ? -1 : a.record_id > b.record_id ? 1 : 0);
function fixture(count = 2000) {
  const records = new Map(Array.from({ length: count }, (_, i) => { const r = row(i + 1); return [r.record_id, r]; }));
  const events = [];
  const queries = [];
  const snapshots = new Map();
  let cursorId = 0;
  let changeReads = 0;
  let reset = false;
  let ahead = false;
  let expire = false;
  let failQuery = false;
  let binding = 'scope:one';
  let duringQuery;
  const matches = (r, predicate) => {
    if (!predicate) return true;
    const evaluate = (i) => { const n = predicate.nodes[i];
      if (n.kind === 'all') return n.children.every(evaluate);
      if (n.kind === 'any') return n.children.some(evaluate);
      if (n.kind === 'not') return !evaluate(n.child);
      if (n.kind === 'in') return n.values.includes(r.fields[n.field]);
      return r.fields[n.field] === n.value;
    }; return evaluate(predicate.root);
  };
  const transport = {
    async queryData(_id, request, options) {
      queries.push(structuredClone(request));
      if (failQuery) { failQuery = false; throw new Error('temporary read failure'); }
      if (expire && request.cursor) { expire = false; throw Object.assign(new Error('expired'), { reasonCode: 'app_data_cursor_unavailable' }); }
      assert.equal(request.pagination, 'keyset');
      const after = request.cursor ? snapshots.get(request.cursor) : undefined;
      if (request.cursor) assert.ok(after);
      const snapshot = [...records.values()].filter((r) => matches(r, request.predicate) && (!after || compare(r, after) > 0)).sort(compare);
      assert.ok(snapshot);
      const value = structuredClone(snapshot.slice(0, request.limit));
      let next_cursor;
      if (snapshot.length > request.limit) { next_cursor = `cursor:${++cursorId}`; snapshots.set(next_cursor, structuredClone(value.at(-1))); }
      if (duringQuery) { const hook = duringQuery; duringQuery = undefined; await hook(); }
      options?.signal?.throwIfAborted();
      return { envelope: { value, scope_binding_ref: binding, installation_id: 'install:one', package_revision_ref: 'package:one', schema_revision: 1, grant_revision: 1 }, ...(next_cursor ? { next_cursor } : {}) };
    },
    async readEntityChanges(_id, options) {
      changeReads++;
      if (ahead && options.limit !== 1) { ahead = false; throw Object.assign(new Error('cursor ahead'), { reasonCode: 'app_change_cursor_ahead' }); }
      const after = options.afterChangeSequence ?? 0;
      const changes = events.slice(after, after + options.limit);
      const through = after + changes.length;
      if (reset && options.limit !== 1) { reset = false; return { changes: [], current_change_sequence: events.length, through_change_sequence: events.length, reset_required: true }; }
      return { changes, through_change_sequence: through, current_change_sequence: events.length, has_more: through < events.length, reset_required: false };
    },
  };
  const collection = new AppLiveCollection(transport, { surfaceRevision: 1, recordIdField: 'id', compare,
    request: { protocol_version: '1', source_installation_id: 'install:one', entity: 'message', select: ['id', 'time', 'category', 'body'], purpose: 'test',
      order: [{ field: 'time', direction: 'descending' }],
      predicate: { root: 0, nodes: [{ kind: 'compare', field: 'category', operator: 'equal', value: 'feed' }] } } });
  return { collection, records, queries, get changeReads() { return changeReads; },
    change(r, deleted = false) { if (deleted) records.delete(r.record_id); else records.set(r.record_id, r);
      events.push({ entity: r.entity, record_id: r.record_id, record_revision: r.record_revision, change_sequence: events.length + 1 }); },
    reset() { reset = true; }, ahead() { ahead = true; }, expire() { expire = true; }, fail() { failQuery = true; },
    bind(value) { binding = value; }, duringQuery(hook) { duringQuery = hook; },
  };
}

test('100,001 stored messages: open reads exactly 25; each explicit Load more adds 25', async () => {
  const f = fixture(100001);
  await f.collection.start();
  assert.equal(f.collection.state.records.length, 25);
  assert.equal(f.queries.length, 1);
  await f.collection.loadMore();
  assert.equal(f.collection.state.records.length, 50);
  assert.ok(f.queries[1].cursor);
  assert.equal(f.queries.length, 2); // one current page per click; no identity reread
  assert.ok(f.queries.every((q) => q.limit === 25));
});

test('live insertion preserves loaded history and its cursor; tied timestamps stay deterministic', async () => {
  const f = fixture(80);
  await f.collection.start(); await f.collection.loadMore();
  f.change(row('z', 100)); f.change(row('a', 100));
  await f.collection.synchronize();
  assert.deepEqual(f.collection.state.records.slice(0, 2).map((r) => r.record_id), ['a', 'z']);
  assert.equal(f.collection.state.records.length, 52);
  await f.collection.loadMore();
  assert.equal(f.collection.state.records.length, 77);
  assert.equal(new Set(f.collection.state.records.map((r) => r.record_id)).size, 77);
});

test('a commit between head capture and initial page completion is not lost', async () => {
  const f = fixture(0);
  f.duringQuery(() => { f.change(row(1)); });
  await f.collection.start(); assert.equal(f.collection.state.records.length, 0);
  await f.collection.synchronize(); assert.equal(f.collection.state.records[0].record_id, '1');
});

test('large bursts catch up in bounded pages, and unchanged polls perform no row queries', async () => {
  const f = fixture(1000); await f.collection.start();
  await f.collection.synchronize(); assert.equal(f.queries.length, 1);
  for (let i = 1001; i <= 1170; i++) f.change(row(i));
  let polls = 0;
  do { await f.collection.synchronize(); polls++; } while (f.collection.state.moreChanges);
  assert.equal(polls, 3); assert.equal(f.collection.state.records.length, 195);
  assert.ok(f.queries.every((q) => q.limit <= 25));
});

test('updates and deletes including unopened history cannot be resurrected by keyset pages', async () => {
  const f = fixture(50); await f.collection.start();
  f.change(row(49, 49, 2, 'group')); // leaves filter
  f.change(row(48, 48, 2), true);
  f.change(row(10, 10, 2), true); // unopened history deletion
  f.change({ ...row(11, 11, 2), fields: { ...row(11).fields, body: 'edited' } });
  await f.collection.synchronize(); await f.collection.loadMore();
  const rows = f.collection.state.records;
  assert.ok(!rows.some((r) => ['49', '48', '10'].includes(r.record_id)));
  assert.equal(rows.find((r) => r.record_id === '11').fields.body, 'edited');
  assert.equal(f.collection.state.hasMore, false);
});

test('an old record change does not pull unrequested history into the visible window', async () => {
  const f = fixture(); await f.collection.start(); f.change(row(1, 1, 2));
  await f.collection.synchronize(); assert.equal(f.collection.state.records.length, 25);
  f.change(row(1, 9999, 3)); await f.collection.synchronize();
  assert.equal(f.collection.state.records[0].record_id, '1');
});

test('failed changed-record read does not acknowledge its sequence; retry applies the change', async () => {
  const f = fixture(); await f.collection.start(); f.change(row(2001)); f.fail();
  await assert.rejects(f.collection.synchronize(), /temporary/);
  assert.equal(f.collection.state.changeSequence, 0);
  await f.collection.synchronize(); assert.equal(f.collection.state.changeSequence, 1);
  assert.equal(f.collection.state.records[0].record_id, '2001');
});

test('simultaneous Load more and change wake-ups coalesce and serialize correctly', async () => {
  const f = fixture(); await f.collection.start(); f.change(row(2001));
  await Promise.all([f.collection.loadMore(), f.collection.loadMore(), f.collection.synchronize(), f.collection.synchronize()]);
  assert.equal(f.collection.state.records.length, 51);
  assert.equal(f.changeReads, 2); // head capture + one sync
});

test('expired cursors and missing change history recover with a bounded explicit reset', async () => {
  const f = fixture(); await f.collection.start(); await f.collection.loadMore(); f.expire();
  await f.collection.loadMore(); assert.equal(f.collection.state.records.length, 25);
  assert.equal(f.collection.state.resetReason, 'cursor_expired');
  f.reset(); await f.collection.synchronize(); assert.equal(f.collection.state.resetReason, 'history_unavailable');
  assert.equal(f.collection.state.records.length, 25);
  f.ahead(); await f.collection.synchronize(); assert.equal(f.collection.state.resetReason, 'history_unavailable');
  assert.equal(f.collection.state.records.length, 25);
});

test('dispose suppresses stale in-flight publications; binding changes never merge scopes', async () => {
  const f = fixture(); const states = []; f.collection.subscribe((s) => states.push(s));
  f.duringQuery(() => { f.collection.dispose(); });
  await assert.rejects(f.collection.start()); assert.equal(states.at(-1).records.length, 0);
  const g = fixture(); await g.collection.start(); g.bind('scope:two');
  await assert.rejects(g.collection.loadMore(), /binding changed/);
  assert.equal(g.collection.state.records.length, 0);
});
