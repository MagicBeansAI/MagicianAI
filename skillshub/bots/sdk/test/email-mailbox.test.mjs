import assert from 'node:assert/strict';
import test from 'node:test';
import { parseEmailMailbox } from '../dist/index.js';

test('single mailboxes are canonical while quoted display-name addresses never become identity', () => {
  for (const value of [
    'Actual.Person+tag@Example.COM',
    'Actual Person <Actual.Person+tag@Example.COM>',
    '"Owner <owner@example.com>" <Actual.Person+tag@Example.COM>',
    '"Last, First \\"Nickname\\"" <Actual.Person+tag@Example.COM>',
    '<Actual.Person+tag@Example.COM>',
  ]) assert.equal(parseEmailMailbox(value), 'actual.person+tag@example.com', value);
});

test('ambiguous, malformed and unsupported mailbox syntax never supplies a principal', () => {
  for (const value of [
    undefined, null, 42, {}, '', 'a@example.com, b@example.com',
    'Owner <owner@example.com>, Attacker <attacker@example.net>',
    '"Name" <one@example.com> trailing', 'Name <one@example.com> <two@example.com>',
    '"Owner <owner@example.com> <attacker@example.net>',
    'team: owner@example.com;', 'owner@example.com\r\nBcc: attacker@example.net',
    'a..b@example.com', '.a@example.com', 'a@-example.com', 'a@example..com',
    'a@example.com (comment)', '"quoted@local"@example.com',
  ]) assert.equal(parseEmailMailbox(value), undefined, String(value));
});
