import assert from 'node:assert/strict';
import { chmodSync, existsSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { GmailAdapter } from '../dist/adapter.js';
import {
  isMissingTopicError,
  isWatchActiveLine,
  parseCreatedTopic,
  parseSavedWatchTopic,
  readSavedWatchTopic,
  reusableTopic,
  saveWatchTopic,
  shouldForgetTopic,
  watchArgs,
  WATCH_TOPIC_STATE_FILE,
} from '../dist/watch-topic.js';

const TOPIC = 'projects/magician-gws/topics/gws-gmail-watch-9ea955cc';
// What Pub/Sub answers when a subscription is created on a deleted topic.
const TOPIC_NOT_FOUND = '{"code":404,"message":"Resource not found (resource=gws-gmail-watch-9ea955cc).","status":"NOT_FOUND"}';

test('the topic gws creates is read from its setup line, prefixed or not', () => {
  assert.equal(parseCreatedTopic(`Creating Pub/Sub topic: ${TOPIC}`), TOPIC);
  assert.equal(parseCreatedTopic(`[gmail-bot][gws] Creating Pub/Sub topic: ${TOPIC}`), TOPIC);
  assert.equal(parseCreatedTopic('Creating Pub/Sub subscription: projects/magician-gws/subscriptions/gws-gmail-watch-9ea955cc'), undefined);
  assert.equal(isWatchActiveLine('Gmail watch active (historyId: 4395, expires: 1785000000000)'), true);
  assert.equal(isWatchActiveLine('Listening for new emails...'), false);
});

test('a saved topic is reused only for the project the watcher runs against', () => {
  const saved = parseSavedWatchTopic(JSON.stringify({ topic: TOPIC, project: 'magician-gws', saved_at: 'x' }));
  assert.equal(reusableTopic(saved, 'magician-gws'), TOPIC);
  assert.equal(reusableTopic(saved, 'other-project'), undefined);
  assert.equal(reusableTopic(saved, undefined), undefined);
  assert.deepEqual(watchArgs(TOPIC), ['gmail', '+watch', '--format', 'json', '--topic', TOPIC]);
  assert.deepEqual(watchArgs(undefined), ['gmail', '+watch', '--format', 'json']);

  // A record whose project disagrees with its own topic name, or a broken file, is ignored.
  assert.equal(parseSavedWatchTopic(JSON.stringify({ topic: TOPIC, project: 'other-project' })), undefined);
  assert.equal(parseSavedWatchTopic('{not json'), undefined);
  assert.equal(parseSavedWatchTopic(JSON.stringify({ topic: 'gws-gmail-watch-9ea955cc', project: 'magician-gws' })), undefined);
});

test('only a not-found error for the saved topic forgets it', () => {
  assert.equal(isMissingTopicError(TOPIC_NOT_FOUND, TOPIC), true);
  assert.equal(isMissingTopicError(`Failed to create Pub/Sub subscription: {"error": ${TOPIC_NOT_FOUND}}`, TOPIC), true);
  assert.equal(isMissingTopicError(`Failed to create Pub/Sub subscription: 404 Not Found`, TOPIC), true);

  // Another resource, permission, quota and network errors keep it.
  assert.equal(isMissingTopicError('{"code":404,"message":"Requested entity was not found."}', TOPIC), false);
  assert.equal(isMissingTopicError(`{"code":403,"status":"PERMISSION_DENIED","message":"User not authorized to perform this action on ${TOPIC}"}`, TOPIC), false);
  assert.equal(isMissingTopicError('{"code":429,"status":"RESOURCE_EXHAUSTED","message":"Quota exceeded"}', TOPIC), false);
  assert.equal(isMissingTopicError('error sending request: dns error', TOPIC), false);

  // A run that reached an active watch, or never used a saved topic, keeps the file.
  assert.equal(shouldForgetTopic({ reusedTopic: TOPIC, watchActive: false, errorText: TOPIC_NOT_FOUND }), true);
  assert.equal(shouldForgetTopic({ reusedTopic: TOPIC, watchActive: true, errorText: TOPIC_NOT_FOUND }), false);
  assert.equal(shouldForgetTopic({ reusedTopic: undefined, watchActive: false, errorText: TOPIC_NOT_FOUND }), false);
});

test('the topic is saved atomically, owner-only, beside the gws credentials', (t) => {
  const configDir = mkdtempSync(join(tmpdir(), 'gmail-topic-'));
  t.after(() => rmSync(configDir, { recursive: true, force: true }));
  saveWatchTopic(configDir, TOPIC, new Date('2026-10-27T00:00:00Z'));
  const path = join(configDir, WATCH_TOPIC_STATE_FILE);
  assert.equal(statSync(path).mode & 0o777, 0o600);
  assert.deepEqual(JSON.parse(readFileSync(path, 'utf8')), {
    topic: TOPIC,
    project: 'magician-gws',
    saved_at: '2026-10-27T00:00:00.000Z',
  });
  assert.deepEqual(readSavedWatchTopic(configDir)?.topic, TOPIC);
});

/* ---------- through the adapter, with a stand-in `gws` ---------- */

/**
 * A fake `gws gmail +watch`: records its arguments, then either sets up a
 * new topic and stays up, or (handed a topic) fails the way FAKE_GWS_FAILURE says.
 */
const FAKE_GWS = `#!/usr/bin/env node
const { appendFileSync } = require('node:fs');
const args = process.argv.slice(2);
appendFileSync(process.env.FAKE_GWS_ARGS, JSON.stringify(args) + '\\n');
const topicIndex = args.indexOf('--topic');
if (topicIndex < 0) {
  process.stderr.write('Creating Pub/Sub topic: ${TOPIC}\\n');
  process.stderr.write('Creating Pub/Sub subscription: projects/magician-gws/subscriptions/gws-gmail-watch-9ea955cc\\n');
} else if (process.env.FAKE_GWS_FAILURE) {
  process.stdout.write(JSON.stringify({ error: JSON.parse(process.env.FAKE_GWS_FAILURE) }, null, 2) + '\\n');
  process.exit(1);
}
process.stderr.write('Gmail watch active (historyId: 4395, expires: 1785000000000)\\n');
setInterval(() => {}, 1000);
`;

const ORIGINAL_ENV = { ...process.env };

function adapterWithFakeGws(t, { failure } = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'gmail-watch-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const gwsBinary = join(dir, 'gws.cjs');
  writeFileSync(gwsBinary, FAKE_GWS);
  chmodSync(gwsBinary, 0o755);
  const configDir = join(dir, 'profile');
  const argsFile = join(dir, 'args.jsonl');
  writeFileSync(argsFile, '');

  const env = { GOOGLE_WORKSPACE_PROJECT_ID: 'magician-gws', FAKE_GWS_ARGS: argsFile, FAKE_GWS_FAILURE: failure };
  const original = Object.fromEntries(Object.keys(env).map((key) => [key, ORIGINAL_ENV[key]]));
  const apply = (values) => {
    for (const [key, value] of Object.entries(values)) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
  };
  apply(env);
  t.after(() => apply(original));

  const adapter = new GmailAdapter({ gwsBinary, configDir });
  // Observe each exit instead of restarting the watcher.
  const exits = [];
  let onExit = () => {};
  adapter.resilient.handleExit = async (code) => { exits.push(code); onExit(); };
  t.after(() => adapter.stop());
  const exited = () => new Promise((resolve) => { onExit = resolve; });
  const calls = () => readFileSync(argsFile, 'utf8').trim().split('\n').filter(Boolean).map((line) => JSON.parse(line));
  const statePath = join(configDir, WATCH_TOPIC_STATE_FILE);
  return { adapter, configDir, statePath, calls, exited, exits };
}

async function waitFor(predicate, timeoutMs = 5000) {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() > deadline) throw new Error('timed out waiting');
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}

test('a first run saves the topic gws created once the watch is active, and the next run reuses it', async (t) => {
  const { adapter, statePath, calls } = adapterWithFakeGws(t);
  adapter.spawnWatcher();
  await waitFor(() => existsSync(statePath));
  assert.equal(JSON.parse(readFileSync(statePath, 'utf8')).topic, TOPIC);
  assert.deepEqual(calls()[0], ['gmail', '+watch', '--format', 'json']);
  const first = adapter.watchProcess;
  const closed = new Promise((resolve) => first.once('close', resolve));
  await adapter.stop();
  await closed;

  adapter.stopping = false;
  adapter.spawnWatcher();
  await waitFor(() => calls().length === 2);
  assert.deepEqual(calls()[1], ['gmail', '+watch', '--format', 'json', '--topic', TOPIC]);
});

test('a saved topic Google no longer has is forgotten; other failures keep it', async (t) => {
  const missing = adapterWithFakeGws(t, { failure: TOPIC_NOT_FOUND });
  saveWatchTopic(missing.configDir, TOPIC);
  let exited = missing.exited();
  missing.adapter.spawnWatcher();
  await exited;
  assert.deepEqual(missing.calls()[0].slice(-2), ['--topic', TOPIC]);
  assert.equal(existsSync(missing.statePath), false);

  const denied = adapterWithFakeGws(t, {
    failure: JSON.stringify({ code: 403, status: 'PERMISSION_DENIED', message: `User not authorized to perform this action on ${TOPIC}` }),
  });
  saveWatchTopic(denied.configDir, TOPIC);
  exited = denied.exited();
  denied.adapter.spawnWatcher();
  await exited;
  assert.equal(existsSync(denied.statePath), true);
});
