const test = require('node:test');
const assert = require('node:assert/strict');

const latency = require('./gmail_mail_assist_latency.js');

test('measures Gmail chip rendering against a hard budget', () => {
  const result = latency.measureRender(() => 'chip', { budgetMs: 1000 });
  assert.equal(result.value, 'chip');
  assert.equal(result.sample.surface, 'gmail_chip');
  assert.equal(result.sample.within_budget, true);
});

test('records and reports budget violations', () => {
  const sample = latency.record(25, 10, 'gmail_chip_fixture');
  assert.equal(sample.within_budget, false);
  const stats = latency.stats();
  assert.ok(stats.count >= 2);
  assert.ok(stats.violations >= 1);
  assert.equal(typeof stats.p95_ms, 'number');
});
