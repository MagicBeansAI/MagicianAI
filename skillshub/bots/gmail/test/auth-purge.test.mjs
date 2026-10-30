import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { GmailAdapter } from '../dist/adapter.js';

const CREDENTIAL_FILES = ['credentials.enc', 'credentials.json', 'token_cache.json'];
const VALID_STATUS = {
  auth_method: 'oauth2',
  storage: 'encrypted',
  encryption_valid: true,
  has_refresh_token: true,
  token_valid: true,
  scopes: [
    'https://www.googleapis.com/auth/gmail.modify',
    'https://www.googleapis.com/auth/pubsub',
  ],
};

class NeedsAuthExit extends Error {
  constructor(code) {
    super(`process.exit(${code})`);
    this.code = code;
  }
}

/** Turn the needs-auth `process.exit` into a rejection the test can observe. */
function trapExit(t) {
  t.mock.method(process, 'exit', (code) => {
    throw new NeedsAuthExit(code);
  });
}

/** An adapter whose `gws auth status` answers with `probe`, over real credential files. */
function adapterWithProbe(t, probe) {
  delete process.env.MAGICIAN_BOT_AUTH_SIDECAR_PATH;
  delete process.env.GWS_EXPECTED_EMAIL;
  const configDir = mkdtempSync(join(tmpdir(), 'gmail-auth-'));
  t.after(() => rmSync(configDir, { recursive: true, force: true }));
  for (const name of CREDENTIAL_FILES) writeFileSync(join(configDir, name), 'stored-secret');
  const adapter = new GmailAdapter({ gwsBinary: '/unused-test-gws', configDir });
  adapter.execGws = async (args) => {
    assert.deepEqual(args, ['auth', 'status', '--format', 'json']);
    return probe();
  };
  const remaining = () => CREDENTIAL_FILES.filter((name) => existsSync(join(configDir, name)));
  return { adapter, remaining };
}

test('a re-check that finds the credentials valid never purges them, even when a purge is due', async (t) => {
  trapExit(t);
  const { adapter, remaining } = adapterWithProbe(t, () => JSON.stringify(VALID_STATUS));
  await adapter.ensureAuth(true);
  assert.deepEqual(remaining(), CREDENTIAL_FILES);
});

test('credentials the re-check could not judge are kept: a refresh that failed on the network or a failing gws', async (t) => {
  trapExit(t);
  const probes = [
    () => JSON.stringify({ ...VALID_STATUS, token_valid: false, token_error: 'error sending request: dns error' }),
    () => { throw new Error('gws auth status timed out'); },
  ];
  for (const probe of probes) {
    const { adapter, remaining } = adapterWithProbe(t, probe);
    await assert.rejects(adapter.ensureAuth(true), NeedsAuthExit);
    assert.deepEqual(remaining(), CREDENTIAL_FILES);
  }
});

test('a refresh token Google rejected is purged once a purge is due, and only then', async (t) => {
  trapExit(t);
  const rejected = () => JSON.stringify({ ...VALID_STATUS, token_valid: false, token_error: 'invalid_grant: Token has been expired or revoked.' });

  const early = adapterWithProbe(t, rejected);
  await assert.rejects(early.adapter.ensureAuth(false), NeedsAuthExit);
  assert.deepEqual(early.remaining(), CREDENTIAL_FILES);

  const due = adapterWithProbe(t, rejected);
  await assert.rejects(due.adapter.ensureAuth(true), NeedsAuthExit);
  assert.deepEqual(due.remaining(), []);
});

test('giving up on repeated auth exits asks for sign-in only when the re-check finds the credentials at fault', async (t) => {
  trapExit(t);
  const exitedWith = (code) => (error) => error instanceof NeedsAuthExit && error.code === code;

  const accepted = adapterWithProbe(t, () => JSON.stringify(VALID_STATUS));
  await assert.rejects(accepted.adapter.giveUpOnAuthExits(), exitedWith(70));
  assert.deepEqual(accepted.remaining(), CREDENTIAL_FILES);

  const rejected = adapterWithProbe(t, () => JSON.stringify({ ...VALID_STATUS, has_refresh_token: false }));
  await assert.rejects(rejected.adapter.giveUpOnAuthExits(), exitedWith(79));
  assert.deepEqual(rejected.remaining(), CREDENTIAL_FILES);
});
