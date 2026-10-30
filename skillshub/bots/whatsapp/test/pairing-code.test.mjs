import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test, { after } from 'node:test';

import { createPairingCodeRequest, normalizePairingPhone, writePairingCodeFile } from '../dist/pairing-code.js';

const home = mkdtempSync(join(tmpdir(), 'whatsapp-pairing-'));
after(() => rmSync(home, { recursive: true, force: true }));

test('the phone reaches wu-cli only until one pairing code was issued', () => {
  const written = [];
  const request = createPairingCodeRequest({
    phone: '+1 (555) 000-1111',
    codeFile: join(home, 'code'),
    write: (file, code) => written.push({ file, code }),
    log: () => {},
  });
  // wu-cli reads `pairingPhone` twice per QR event (check, then request).
  assert.equal(request.phone(), '15550001111');
  assert.equal(request.phone(), '15550001111');
  request.onCode('ABCD1234');
  assert.equal(request.phone(), undefined, 'a later QR refresh must not request another code');
  assert.deepEqual(written, [{ file: join(home, 'code'), code: 'ABCD1234' }]);
});

test('without a usable phone the login stays on the QR', () => {
  for (const phone of [undefined, '', '12345', 'not a phone']) {
    const request = createPairingCodeRequest({ phone, codeFile: join(home, 'code'), write: () => {}, log: () => {} });
    assert.equal(request.phone(), undefined, String(phone));
  }
  assert.equal(normalizePairingPhone('+44 7700 900123'), '447700900123');
  assert.equal(normalizePairingPhone('+1234567890123456'), undefined);
});

test('the pairing code file is private and holds only the code', () => {
  const file = join(home, 'pairing', 'whatsapp.pairing-code');
  writePairingCodeFile(file, 'WXYZ5678');
  assert.equal(readFileSync(file, 'utf8'), 'WXYZ5678\n');
  assert.equal(statSync(file).mode & 0o777, 0o600);
});
