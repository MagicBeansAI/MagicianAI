import assert from 'node:assert/strict';
import test from 'node:test';
import { appendBounded, isGoogleAuthRejection } from '../dist/auth-rejection.js';

test('Google rejecting the sent credentials is an auth rejection', () => {
  // The Pub/Sub error the watcher logged when its stored token was unusable.
  const pubsub = `{
    "error": {
      "code": 401,
      "message": "Request had invalid authentication credentials. Expected OAuth 2 access token, login cookie or other valid authentication credential.",
      "status": "UNAUTHENTICATED",
      "details": [{ "reason": "ACCESS_TOKEN_TYPE_UNSUPPORTED" }]
    }
  }`;
  assert.equal(isGoogleAuthRejection(pubsub), true);
  assert.equal(isGoogleAuthRejection('"status":"UNAUTHENTICATED"'), true);
});

test('permission, quota and missing-resource errors are not auth rejections', () => {
  assert.equal(isGoogleAuthRejection('{"error":{"code":403,"status":"PERMISSION_DENIED","reason":"IAM_PERMISSION_DENIED"}}'), false);
  assert.equal(isGoogleAuthRejection('Caller does not have required permission to use project magician-gws.'), false);
  assert.equal(isGoogleAuthRejection('[gmail-bot] gws watch error (404/unknown): Requested entity was not found.'), false);
});

test('the stderr tail stays bounded and keeps the newest text', () => {
  const tail = appendBounded('a'.repeat(10), 'bcd', 8);
  assert.equal(tail.length, 8);
  assert.ok(tail.endsWith('bcd'));
});
