/**
 * ResilientProcess — shared retry/backoff/timeout logic for CLI-based bot adapters.
 *
 * Wraps a child process spawner with:
 * - Exponential backoff on auth failures (3s → 60s)
 * - Consecutive failure tracking with credential purge after N attempts
 * - 10-minute max retry window before giving up
 * - Exponential backoff on quick non-auth exits (3s → 5min), escalating via
 *   `onPersistentFailure` after `maxQuickExits` of them in a row
 * - Auto-reset on successful output, and after a run that stayed up for
 *   `healthyRunMs` (failures days apart are not "consecutive")
 */

export interface ResilientProcessOptions {
  /** Human-readable label for log messages, e.g. "[whatsapp-bot]" */
  label: string;

  /** Number of consecutive auth failures before purging credentials. Default: 2 */
  maxRetriesBeforePurge?: number;

  /** Max duration (ms) of continuous failures before giving up. Default: 10 minutes */
  maxRetryDurationMs?: number;

  /**
   * Exit codes that indicate an auth/session failure (trigger re-auth + purge flow).
   * All other non-zero codes trigger a simple restart.
   */
  authFailureCodes?: number[];

  /**
   * Called before restarting after an auth failure.
   * `shouldPurge` is true when failures exceed `maxRetriesBeforePurge`.
   */
  onReauth?: (shouldPurge: boolean) => Promise<void>;

  /** Called when retries are exhausted and the process gives up. */
  onGiveUp?: () => void;

  /**
   * A run that stayed up at least this long before exiting ends the failure
   * streak, so its exit counts as a first failure. Without it a quiet process
   * (no output to `recordSuccess` on) carried its count across days and the
   * Nth unrelated failure purged credentials. Needs `recordStart()`.
   * Default: 5 minutes
   */
  healthyRunMs?: number;

  /** Clock in epoch milliseconds; injectable for tests. Default: `Date.now` */
  now?: () => number;

  /**
   * Non-auth exits in a row, each from a run shorter than `healthyRunMs`,
   * before `onPersistentFailure` is called instead of another restart.
   * Default: 5
   */
  maxQuickExits?: number;

  /** Cap of the non-auth restart backoff (3 s, doubling). Default: 5 minutes */
  maxRestartDelayMs?: number;

  /**
   * Called instead of restarting once `maxQuickExits` quick non-auth exits
   * ran in a row: the process keeps dying soon after start for a reason a
   * restart does not fix (e.g. a missing permission). The adapter should
   * surface it — typically by exiting non-zero so the supervisor shows the
   * bot as failing. Unset: keep restarting at the capped backoff.
   */
  onPersistentFailure?: (detail: string) => void;
}

export class ResilientProcess {
  private consecutiveFailures = 0;
  private firstFailureAt: number | null = null;
  private runStartedAt: number | null = null;
  private quickExits = 0;

  private readonly label: string;
  private readonly maxRetriesBeforePurge: number;
  private readonly maxRetryDurationMs: number;
  private readonly authFailureCodes: Set<number>;
  private readonly onReauth: ((shouldPurge: boolean) => Promise<void>) | undefined;
  private readonly onGiveUp: (() => void) | undefined;
  private readonly healthyRunMs: number;
  private readonly now: () => number;
  private readonly maxQuickExits: number;
  private readonly maxRestartDelayMs: number;
  private readonly onPersistentFailure: ((detail: string) => void) | undefined;

  constructor(options: ResilientProcessOptions) {
    this.label = options.label;
    this.maxRetriesBeforePurge = options.maxRetriesBeforePurge ?? 2;
    this.maxRetryDurationMs = options.maxRetryDurationMs ?? 10 * 60 * 1000;
    this.authFailureCodes = new Set(options.authFailureCodes ?? []);
    this.onReauth = options.onReauth;
    this.onGiveUp = options.onGiveUp;
    this.healthyRunMs = options.healthyRunMs ?? 5 * 60 * 1000;
    this.now = options.now ?? (() => Date.now());
    this.maxQuickExits = options.maxQuickExits ?? 5;
    this.maxRestartDelayMs = options.maxRestartDelayMs ?? 5 * 60 * 1000;
    this.onPersistentFailure = options.onPersistentFailure;
  }

  /**
   * Call each time the process is (re)spawned. An exit after a run of at
   * least `healthyRunMs` resets the failure counters before it is counted.
   */
  recordStart(): void {
    this.runStartedAt = this.now();
  }

  /**
   * Call when the process produces successful output (e.g., a stdout line).
   * Resets all failure counters.
   */
  recordSuccess(): void {
    this.consecutiveFailures = 0;
    this.firstFailureAt = null;
    this.quickExits = 0;
  }

  /**
   * Call when the process exits. Returns the action to take.
   *
   * @param code - process exit code (null if killed by signal)
   * @param signal - signal that killed the process (null if exited normally)
   * @param restart - function to call to restart the process
   */
  async handleExit(
    code: number | null,
    signal: string | null,
    restart: () => void,
  ): Promise<void> {
    const now = this.now();
    if (this.runStartedAt !== null && now - this.runStartedAt >= this.healthyRunMs) {
      this.recordSuccess();
    }
    this.runStartedAt = null;

    const isAuthFailure = code !== null && this.authFailureCodes.has(code);

    if (isAuthFailure) {
      this.consecutiveFailures++;
      if (this.firstFailureAt === null) this.firstFailureAt = now;

      // Check timeout
      const elapsed = now - this.firstFailureAt;
      if (elapsed > this.maxRetryDurationMs) {
        console.error(
          `${this.label} giving up after ${Math.round(elapsed / 60_000)}min of auth failures — restart manually from bots page`,
        );
        this.onGiveUp?.();
        return;
      }

      const shouldPurge = this.consecutiveFailures > this.maxRetriesBeforePurge;
      const delay = Math.min(
        3_000 * Math.pow(2, this.consecutiveFailures - 1),
        60_000,
      );

      console.info(
        `${this.label} auth failure #${this.consecutiveFailures} (code=${code}); ` +
          `${shouldPurge ? "purging stale creds and " : ""}retrying in ${delay / 1000}s`,
      );

      setTimeout(async () => {
        try {
          if (this.onReauth) {
            await this.onReauth(shouldPurge);
          }
          restart();
        } catch (err) {
          console.error(`${this.label} re-auth failed, retrying in 30s`, err);
          setTimeout(restart, 30_000);
        }
      }, delay);
    } else {
      // Non-auth exit: restart, backing off while runs keep ending before
      // `healthyRunMs` (a healthy run or output reset the streak above), and
      // escalate once that streak shows a restart does not help.
      this.consecutiveFailures = 0;
      this.firstFailureAt = null;
      this.quickExits++;

      const exit = `code=${code ?? "null"}, signal=${signal ?? "null"}`;
      if (this.onPersistentFailure && this.quickExits >= this.maxQuickExits) {
        const detail =
          `${this.quickExits} exits in a row, each within ` +
          `${Math.round(this.healthyRunMs / 1000)}s of starting (last: ${exit})`;
        console.error(`${this.label} process keeps failing: ${detail}`);
        this.onPersistentFailure(detail);
        return;
      }

      const delay = Math.min(
        3_000 * Math.pow(2, this.quickExits - 1),
        this.maxRestartDelayMs,
      );
      console.info(
        `${this.label} process exited (${exit}); restarting in ${delay / 1000}s`,
      );
      setTimeout(restart, delay);
    }
  }
}
