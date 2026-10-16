/**
 * ResilientProcess — shared retry/backoff/timeout logic for CLI-based bot adapters.
 *
 * Wraps a child process spawner with:
 * - Exponential backoff on failures (3s → 60s)
 * - Consecutive failure tracking with credential purge after N attempts
 * - 10-minute max retry window before giving up
 * - Auto-reset on successful output
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
}

export class ResilientProcess {
  private consecutiveFailures = 0;
  private firstFailureAt: number | null = null;

  private readonly label: string;
  private readonly maxRetriesBeforePurge: number;
  private readonly maxRetryDurationMs: number;
  private readonly authFailureCodes: Set<number>;
  private readonly onReauth: ((shouldPurge: boolean) => Promise<void>) | undefined;
  private readonly onGiveUp: (() => void) | undefined;

  constructor(options: ResilientProcessOptions) {
    this.label = options.label;
    this.maxRetriesBeforePurge = options.maxRetriesBeforePurge ?? 2;
    this.maxRetryDurationMs = options.maxRetryDurationMs ?? 10 * 60 * 1000;
    this.authFailureCodes = new Set(options.authFailureCodes ?? []);
    this.onReauth = options.onReauth;
    this.onGiveUp = options.onGiveUp;
  }

  /**
   * Call when the process produces successful output (e.g., a stdout line).
   * Resets all failure counters.
   */
  recordSuccess(): void {
    this.consecutiveFailures = 0;
    this.firstFailureAt = null;
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
    const isAuthFailure = code !== null && this.authFailureCodes.has(code);

    if (isAuthFailure) {
      this.consecutiveFailures++;
      if (!this.firstFailureAt) this.firstFailureAt = Date.now();

      // Check timeout
      const elapsed = Date.now() - this.firstFailureAt;
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
      // Non-auth exit — simple restart with short delay
      this.consecutiveFailures = 0;
      this.firstFailureAt = null;

      console.info(
        `${this.label} process exited (code=${code ?? "null"}, signal=${signal ?? "null"}); restarting in 3s`,
      );
      setTimeout(restart, 3_000);
    }
  }
}
