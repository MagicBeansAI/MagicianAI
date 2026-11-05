import assert from 'node:assert/strict';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { GmailAdapter } from '../dist/adapter.js';

// What Pub/Sub answered the business account's watcher on 2026-11-03 while
// Gmail accepted the same credentials; a fresh token cache cleared it.
const PUBSUB_REFUSAL = JSON.stringify({
  code: 400,
  message:
    'Failed to create Pub/Sub subscription: {"error":{"code":401,"message":"Request had invalid authentication credentials.","status":"UNAUTHENTICATED","details":[{"reason":"ACCESS_TOKEN_TYPE_UNSUPPORTED"}]}}',
  reason: 'pubsubError',
});

/**
 * A fake `gws`. `gmail users getProfile` succeeds unless FAKE_GMAIL_REJECTS.
 * `gmail +watch` is refused while a token cache exists (or always, with
 * FAKE_PUBSUB_ALWAYS_REFUSES), else goes active and stays up.
 */
const FAKE_GWS = `#!/usr/bin/env node
const { existsSync } = require('node:fs');
const { join } = require('node:path');
const args = process.argv.slice(2);
if (args[1] === 'users') {
  if (process.env.FAKE_GMAIL_REJECTS) {
    process.stdout.write(JSON.stringify({ error: { code: 401, status: 'UNAUTHENTICATED' } }) + '\\n');
    process.exit(1);
  }
  process.stdout.write(JSON.stringify({ emailAddress: 'owner@example.com' }) + '\\n');
  process.exit(0);
}
const cache = join(process.env.GOOGLE_WORKSPACE_CLI_CONFIG_DIR, 'token_cache.json');
if (process.env.FAKE_PUBSUB_ALWAYS_REFUSES || existsSync(cache)) {
  process.stdout.write(JSON.stringify({ error: ${PUBSUB_REFUSAL} }, null, 2) + '\\n');
  process.exit(1);
}
process.stderr.write('Gmail watch active (historyId: 4395, expires: 1785000000000)\\n');
setInterval(() => {}, 1000);
`;

function adapterWithStaleCache(t, env) {
  const dir = mkdtempSync(join(tmpdir(), 'gmail-rejection-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const gwsBinary = join(dir, 'gws.cjs');
  writeFileSync(gwsBinary, FAKE_GWS);
  chmodSync(gwsBinary, 0o755);
  const configDir = join(dir, 'profile');
  mkdirSync(configDir, { recursive: true });
  writeFileSync(join(configDir, 'token_cache.json'), 'stale');

  const values = { FAKE_GMAIL_REJECTS: undefined, FAKE_PUBSUB_ALWAYS_REFUSES: undefined, MAGICIAN_BOT_AUTH_SIDECAR_PATH: undefined, ...env };
  const saved = Object.fromEntries(Object.keys(values).map((key) => [key, process.env[key]]));
  const apply = (entries) => {
    for (const [key, value] of Object.entries(entries)) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
  };
  apply(values);
  t.after(() => apply(saved));

  const adapter = new GmailAdapter({ gwsBinary, configDir });
  t.after(() => adapter.stop());
  // Each refused run closes here instead of restarting through the backoff;
  // the test spawns the next run itself.
  let closes = 0;
  adapter.resilient.handleExit = async () => { closes += 1; };

  // A mocked exit returns, so record what an exit would have left behind.
  const exits = [];
  const cacheKeptAtExit = [];
  t.mock.method(process, 'exit', (code) => {
    exits.push(code);
    cacheKeptAtExit.push(existsSync(join(configDir, 'token_cache.json')));
  });
  const cacheFiles = () => readdirSync(configDir).filter((name) => name.startsWith('token_cache'));
  return { adapter, configDir, exits, cacheKeptAtExit, cacheFiles, closes: () => closes };
}

async function waitFor(predicate, timeoutMs = 5000) {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() > deadline) throw new Error('timed out waiting');
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}

test('a Pub/Sub refusal Gmail does not share sets the token cache aside once and the watch recovers', async (t) => {
  const { adapter, configDir, exits, cacheFiles, closes } = adapterWithStaleCache(t, {});
  adapter.spawnWatcher();
  await waitFor(() => closes() === 1);
  assert.deepEqual(exits, [], 'one refused run only restarts the watcher');
  adapter.spawnWatcher();
  await waitFor(() => adapter.watchActive);
  assert.deepEqual(exits, []);
  assert.equal(existsSync(join(configDir, 'token_cache.json')), false);
  assert.match(cacheFiles().join(), /^token_cache\.rejected-\d{8}T\d{6}Z\.json$/, 'the old cache is kept, renamed');
});

test('a refusal that survives a fresh token reports a failing watch, not a sign-in', async (t) => {
  const { adapter, exits, closes } = adapterWithStaleCache(t, { FAKE_PUBSUB_ALWAYS_REFUSES: '1' });
  adapter.spawnWatcher();
  await waitFor(() => closes() === 1);
  adapter.spawnWatcher();
  // Set aside and respawned internally; that run is refused too.
  await waitFor(() => closes() === 2);
  adapter.spawnWatcher();
  await waitFor(() => exits.length > 0);
  assert.equal(exits[0], 70);
});

test('Pub/Sub and Gmail both refusing the credentials asks for a sign-in and keeps the cache', async (t) => {
  const { adapter, exits, cacheKeptAtExit, closes } = adapterWithStaleCache(t, { FAKE_GMAIL_REJECTS: '1' });
  adapter.spawnWatcher();
  await waitFor(() => closes() === 1);
  adapter.spawnWatcher();
  await waitFor(() => exits.length > 0);
  assert.equal(exits[0], 79);
  assert.equal(cacheKeptAtExit[0], true);
});
