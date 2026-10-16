/**
 * needs-auth signalling for bot adapters.
 *
 * Magician's bot supervisor surfaces "needs authentication" as a list-item
 * escalation in the attention bar so the operator can click "Authenticate"
 * to recover. The contract between adapter and supervisor:
 *
 *   1. Adapter probes its auth state on start (and on mid-watch failure).
 *   2. If auth is missing/expired, adapter calls `writeNeedsAuthSidecar(...)`
 *      then `process.exit(EX_NEEDS_AUTH)` (79).
 *   3. Supervisor reads the sidecar to populate `BotAuthSnapshot` (provider,
 *      profile, expected account, current account, detail). The bot is
 *      parked in `Failed` state with no auto-restart.
 *   4. Operator clicks "Authenticate" in the UI bot card → supervisor
 *      dispatches the provider-appropriate login flow and clears the
 *      sidecar; the bot restarts and probes again.
 *
 * Adapters should NEVER spawn an interactive login flow inline — that ambushes
 * the operator with browser/QR popups every time the bot restarts. The
 * supervisor owns when login runs; the adapter just signals the need.
 */

import { writeFileSync, mkdirSync } from "node:fs";
import { dirname } from "node:path";

/**
 * Exit code that signals "auth required" to the magician supervisor. Matches
 * `EX_NEEDS_AUTH` in `magician/src/magician_v2/bots/mod.rs`. 79 is unassigned
 * in BSD `sysexits.h` (which defines 64..=78) so no real CLI tool returns it
 * by accident.
 */
export const EX_NEEDS_AUTH = 79;

/** Stable provider identifiers the supervisor recognises for dispatch. */
export type BotAuthProvider =
  | "google_workspace"
  | "telegram_self"
  | "whatsapp_web"
  | "kapso"
  | (string & {}); // Allow future providers without changing the SDK.

export interface NeedsAuthSidecar {
  /** Stable provider id — drives `start_auth` dispatch in the supervisor. */
  provider: BotAuthProvider;
  /** Human-readable account/profile label shown in the attention bar row. */
  profileLabel?: string | null | undefined;
  /** Account the operator should sign in as (e.g. an email). */
  expectedAccount?: string | null | undefined;
  /** Account currently signed in, if any (set for "wrong account" cases). */
  currentAccount?: string | null | undefined;
  /** Short reason the probe failed; shown as subtitle fallback. */
  detail?: string | null | undefined;
}

/**
 * Write the needs-auth sidecar to the path the magician supervisor injected
 * via `MAGICIAN_BOT_AUTH_SIDECAR_PATH`. Returns `true` if the sidecar was
 * written, `false` if the env var is missing (which happens when the adapter
 * is run outside the supervisor — e.g. local dev). Adapters should treat
 * `false` as "supervisor isn't watching, continue with whatever the local
 * fallback is" and still `process.exit(EX_NEEDS_AUTH)` so the parent shell
 * sees the same signal.
 */
export function writeNeedsAuthSidecar(sidecar: NeedsAuthSidecar): boolean {
  const sidecarPath = process.env.MAGICIAN_BOT_AUTH_SIDECAR_PATH?.trim();
  if (!sidecarPath) {
    return false;
  }
  try {
    mkdirSync(dirname(sidecarPath), { recursive: true });
    const payload = {
      provider: sidecar.provider,
      profile_label: sidecar.profileLabel ?? null,
      expected_account: sidecar.expectedAccount ?? null,
      current_account: sidecar.currentAccount ?? null,
      detail: sidecar.detail ?? null,
      written_at_ms: Date.now(),
    };
    writeFileSync(sidecarPath, JSON.stringify(payload, null, 2), "utf8");
    return true;
  } catch (error) {
    console.error(
      `[bot-sdk] failed to write needs-auth sidecar at ${sidecarPath}:`,
      error,
    );
    return false;
  }
}

/**
 * Shortcut: write the sidecar then `process.exit(EX_NEEDS_AUTH)`. The vast
 * majority of adapters want exactly this combination — write + exit — so
 * the helper keeps both calls in one place and makes the "do not pass GO"
 * exit intent visible at the call site.
 */
export function exitNeedsAuth(sidecar: NeedsAuthSidecar): never {
  writeNeedsAuthSidecar(sidecar);
  process.exit(EX_NEEDS_AUTH);
}
