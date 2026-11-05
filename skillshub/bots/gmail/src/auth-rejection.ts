/**
 * Google's own verdict that the credentials `gws` sent are not usable:
 * an `UNAUTHENTICATED` status, a 401, or a token Google cannot read
 * (`ACCESS_TOKEN_TYPE_UNSUPPORTED`). `gws auth status` only checks the
 * stored credentials locally and can still accept them, so this is the
 * signal that a new sign-in is needed. A permission or quota error
 * (`PERMISSION_DENIED`, 403) is not: signing in again would not fix it.
 */
const GOOGLE_AUTH_REJECTION =
  /"status"\s*:\s*"UNAUTHENTICATED"|ACCESS_TOKEN_TYPE_UNSUPPORTED|invalid authentication credentials|"code"\s*:\s*401\b/i;

export function isGoogleAuthRejection(text: string): boolean {
  return GOOGLE_AUTH_REJECTION.test(text);
}

/** Outcome of one plain Gmail call made with the watcher's credentials. */
export type GmailProbe = { ok: true } | { ok: false; text: string };

/**
 * The watcher's Pub/Sub setup was refused twice in a row; a plain Gmail call
 * with the same credentials decides who is at fault. Gmail refusing them too
 * means a new sign-in is needed. Gmail accepting them means a new sign-in
 * cannot help (the watch's cached token, or the account's Google Cloud access),
 * and neither can a probe that failed for another reason prove them bad.
 */
export function repeatedRejectionNeedsSignIn(gmail: GmailProbe): boolean {
  return !gmail.ok && isGoogleAuthRejection(gmail.text);
}

/** Bounded tail of a watcher's stderr, kept only to classify its exit. */
export function appendBounded(tail: string, chunk: string, limit = 16 * 1024): string {
  const next = tail + chunk;
  return next.length > limit ? next.slice(next.length - limit) : next;
}
