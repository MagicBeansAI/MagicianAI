import { type ChildProcess, execFile, spawn } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import { basename, resolve } from "node:path";
import { createInterface } from "node:readline";
import { prepareEnvoyReply } from "./envoy-reply.js";
import { appendBounded, isGoogleAuthRejection } from "./auth-rejection.js";
import {
  forgetWatchTopic,
  isWatchActiveLine,
  parseCreatedTopic,
  readSavedWatchTopic,
  reusableTopic,
  saveWatchTopic,
  shouldForgetTopic,
  watchArgs,
} from "./watch-topic.js";

import {
  exitNeedsAuth,
  ResilientProcess,
  parseEmailMailbox,
} from "@magician/bot-sdk";
import type {
  ChannelAdapter,
  ChannelAdapterHandlers,
  TaskStatusUpdateRender,
  ToolCallExecutedRender,
} from "@magician/bot-sdk";

/**
 * Incoming email shape from `gws gmail +watch --json`.
 * We parse defensively — not all fields may be present.
 */
interface GwsWatchMessage {
  id?: string;
  threadId?: string;
  from?: string;
  to?: string;
  subject?: string;
  body?: string;
  snippet?: string;
  date?: string;
  messageId?: string;
  [key: string]: unknown;
}

interface GwsErrorPayload {
  error?: {
    code?: number;
    message?: string;
    reason?: string;
  };
}

export interface GmailAdapterOptions {
  /** Absolute path to the `gws` binary. */
  gwsBinary: string;
  /**
   * Optional config directory for GOOGLE_WORKSPACE_CLI_CONFIG_DIR.
   * Enables multi-account support — each bot instance uses a different dir.
   */
  configDir?: string | undefined;
}

interface GoogleClientSecretPayload {
  installed?: { project_id?: string };
  web?: { project_id?: string };
  project_id?: string;
}

const REQUIRED_GWS_SCOPES = [
  "https://www.googleapis.com/auth/gmail.modify",
  "https://www.googleapis.com/auth/pubsub",
];

/**
 * Token errors meaning Google refused the stored refresh token itself, so
 * the credentials cannot recover without a new sign-in. Anything else (a
 * timeout, DNS, a 5xx) says nothing about the token and must not purge it.
 */
const REJECTED_REFRESH_TOKEN = /invalid_grant|revoked|invalid_client|unauthorized_client/i;

/**
 * Exit code when the watcher keeps failing for a non-auth reason. Any code
 * but `EX_NEEDS_AUTH` (79) reaches the supervisor as an unexpected exit; 70
 * is `EX_SOFTWARE` from BSD `sysexits.h`.
 */
const EX_WATCH_FAILING = 70;

interface AuthProblem {
  reason: string;
  /** The probe proved the stored credentials can never work again. */
  credentialsUnusable: boolean;
}


export class GmailAdapter implements ChannelAdapter<string> {
  private handlers?: ChannelAdapterHandlers<string>;
  private watchProcess: ChildProcess | undefined;
  private stopping = false;
  private readonly gwsBinary: string;
  private readonly configDir: string | undefined;
  private readonly cloudSdkConfigDir: string | undefined;
  private readonly projectId: string | undefined;
  private readonly profileName: string;
  private readonly expectedAccount: string | undefined;
  private readonly resilient: ResilientProcess;
  private watchJsonBuffer = "";
  /** This watcher run saw Google reject the credentials it sent. */
  private watchAuthRejected = false;
  private watchStderrTail = "";
  /** Consecutive watcher runs that ended after Google rejected the credentials. */
  private authRejectedRuns = 0;
  /** Saved Pub/Sub topic this watcher run was handed with `--topic`. */
  private watchReusedTopic: string | undefined;
  /** Topic gws said it created on this run; saved once the watch is active. */
  private watchCreatedTopic: string | undefined;
  private watchActive = false;
  private watchErrorText = "";

  constructor(options: GmailAdapterOptions) {
    this.gwsBinary = options.gwsBinary;
    this.configDir = expandTilde(options.configDir);
    this.cloudSdkConfigDir = this.configDir
      ? resolve(this.configDir, "cloudsdk")
      : undefined;
    if (this.cloudSdkConfigDir) {
      mkdirSync(this.cloudSdkConfigDir, { recursive: true });
    }
    this.projectId = resolveProjectId(this.configDir);
    this.profileName =
      process.env.GWS_PROFILE_LABEL?.trim() ||
      (options.configDir
        ? basename(options.configDir).replace(/^gws-/, "").toUpperCase()
        : "DEFAULT");
    this.expectedAccount = process.env.GWS_EXPECTED_EMAIL?.trim() || undefined;
    this.resilient = new ResilientProcess({
      label: "[gmail-bot]",
      // gws exits 2 on missing/expired/invalid credentials. 5 is its
      // "internal error" (Pub/Sub pull timeouts, token-cache write races),
      // which a plain watcher restart recovers from.
      authFailureCodes: [2],
      // Mid-watch auth failure: re-probe `gws auth status` before asking for
      // OAuth. A failing probe exits with EX_NEEDS_AUTH so the supervisor
      // surfaces the escalation (the operator clicks Authenticate); a passing
      // one just restarts the watcher. `shouldPurge` only permits deleting
      // credentials the probe proves unusable — never ones it accepts.
      onReauth: async (shouldPurge) => {
        console.error(
          `[gmail-bot] auth failure mid-watch — re-checking credentials before asking for 'Authenticate'`,
        );
        await this.ensureAuth(shouldPurge);
      },
      // `gws` exit 5 and other non-auth failures restart with a backoff. A
      // watcher that keeps dying soon after start (e.g. a missing Pub/Sub
      // topic or permission, or the Gmail API disabled) cannot fix itself:
      // exit non-zero, which is not the needs-auth code, so Bot Control shows
      // the bot failing with this reason instead of it restarting silently.
      onPersistentFailure: (detail) => {
        console.error(
          `[gmail-bot] gws gmail +watch keeps failing (${detail}); exiting so the supervisor surfaces it. ` +
            "Check the watch's Pub/Sub topic and permissions, and that the Gmail API is enabled.",
        );
        process.exit(EX_WATCH_FAILING);
      },
      // Reached when the watcher keeps exiting with code 2 for the whole
      // retry window while every re-check passed (a failing re-check exits
      // needs-auth earlier, from onReauth).
      onGiveUp: () => {
        void this.giveUpOnAuthExits();
      },
    });

    if (this.projectId) {
      console.info(`[gmail-bot] using Google Workspace project ${this.projectId}`);
    }
  }

  async start(handlers: ChannelAdapterHandlers<string>): Promise<void> {
    this.handlers = handlers;
    this.stopping = false;
    await this.ensureAuth();
    this.spawnWatcher();
  }

  async stop(): Promise<void> {
    this.stopping = true;

    if (this.watchProcess) {
      const proc = this.watchProcess;
      this.watchProcess = undefined;
      proc.kill("SIGTERM");
    }
  }

  async sendText(target: string, text: string): Promise<{ accepted: boolean }> {
    if (target) {
      await this.execGws(["gmail", "+reply", "--message-id", target, "--body", text]);
      return { accepted: true };
    } else {
      console.info("[gmail-bot] sendText called without target; dropping message");
    }
    return { accepted: false };
  }

  prepareEnvoyTextDelivery(target: string, text: string) {
    return prepareEnvoyReply((args) => this.execGws(args), target, text);
  }

  async sendToolCallExecuted(
    target: string,
    executed: ToolCallExecutedRender,
  ): Promise<void> {
    await this.sendText(target, executed.text);
  }

  async sendTaskStatusUpdate(
    target: string,
    update: TaskStatusUpdateRender,
  ): Promise<void> {
    await this.sendText(target, update.text);
  }

  /* ---------- auth ---------- */

  /**
   * Probe `gws auth status` and either return cleanly (auth is valid) or exit
   * the process with `EX_NEEDS_AUTH` so the magician supervisor surfaces a
   * needs-auth escalation in the UI. We never launch the OAuth flow inline
   * anymore — the operator triggers it explicitly via "Authenticate" in Bot
   * Control, which runs `gws auth login` out of process and then restarts
   * the bot.
   *
   * `purgeIfUnusable` deletes the stored credentials only after the probe
   * has run and shown they cannot work (undecryptable, or a refresh token
   * Google rejected). Credentials the probe accepts are never deleted, nor
   * are ones it could not judge (e.g. `gws` itself failing or a network
   * error while refreshing): those may hold a valid refresh token.
   */
  private async ensureAuth(purgeIfUnusable = false): Promise<void> {
    const problem = await this.probeAuth();
    if (!problem) {
      console.info("[gmail-bot] gws auth verified");
      return;
    }

    const { reason } = problem;
    if (purgeIfUnusable && problem.credentialsUnusable) {
      console.error(`[gmail-bot] purging unusable stored credentials for ${this.profileName}: ${reason}`);
      this.purgeStoredCredentials();
    }
    console.error(`[gmail-bot] auth required for ${this.profileName}${this.expectedAccount ? ` (${this.expectedAccount})` : ""}: ${reason}`);
    console.error(
      `[gmail-bot] click 'Authenticate' in Bot Control to run OAuth (supervisor will restart bot on success)`,
    );
    // Account-mismatch errors look like "authenticated as foo@…; expected bar@…".
    // Capture the actual signed-in account so the attention bar can show
    // both sides instead of just "needs auth".
    const mismatchMatch = /authenticated as ([^;]+); expected /.exec(reason);
    const currentAccount = mismatchMatch?.[1]?.trim() ?? undefined;
    exitNeedsAuth({
      provider: "google_workspace",
      profileLabel: this.profileName,
      expectedAccount: this.expectedAccount,
      currentAccount,
      detail: reason,
    });
  }

  /**
   * The watcher kept exiting with an auth code for the whole retry window.
   * Needs-auth only when the probe finds the credentials at fault; when it
   * accepts them, a new sign-in cannot help, and parking the bot on an
   * 'Authenticate' prompt would hide the real failure. Exit through the
   * persistent-failure code instead, with the reason in the log.
   */
  private async giveUpOnAuthExits(): Promise<void> {
    // Exits with EX_NEEDS_AUTH when the probe finds a problem.
    await this.ensureAuth(false);
    console.error(
      "[gmail-bot] gws gmail +watch kept exiting with an auth error (code 2) for the whole retry window " +
        "while 'gws auth status' accepts the credentials; exiting so the supervisor surfaces it. " +
        "Re-authenticating will not fix this — check the watch's Pub/Sub topic and permissions.",
    );
    process.exit(EX_WATCH_FAILING);
  }

  /** Google rejected the credentials on two watcher runs in a row. */
  private exitGoogleRejectedCredentials(): never {
    const reason =
      "Google rejected the stored credentials (UNAUTHENTICATED) although 'gws auth status' accepts them; sign in again";
    console.error(`[gmail-bot] auth required for ${this.profileName}${this.expectedAccount ? ` (${this.expectedAccount})` : ""}: ${reason}`);
    exitNeedsAuth({
      provider: "google_workspace",
      profileLabel: this.profileName,
      expectedAccount: this.expectedAccount,
      detail: reason,
    });
  }

  /** `null` when `gws auth status` reports usable credentials. */
  private async probeAuth(): Promise<AuthProblem | null> {
    let status: Record<string, unknown>;
    try {
      const statusJson = await this.execGws(["auth", "status", "--format", "json"]);
      status = JSON.parse(statusJson) as Record<string, unknown>;
    } catch (error) {
      return { reason: errorReason(error), credentialsUnusable: false };
    }

    if (status.auth_method === "none" || status.storage === "none") {
      return { reason: "no credentials stored", credentialsUnusable: false };
    }
    if (status.encryption_valid === false) {
      return {
        reason: "encrypted credentials could not be decrypted",
        credentialsUnusable: true,
      };
    }
    if (status.has_refresh_token === false) {
      return { reason: "no refresh token stored", credentialsUnusable: false };
    }
    if (status.token_valid === false) {
      const tokenError =
        typeof status.token_error === "string" && status.token_error.trim()
          ? status.token_error.trim()
          : "token invalid";
      return {
        reason: tokenError,
        credentialsUnusable: REJECTED_REFRESH_TOKEN.test(tokenError),
      };
    }
    if (Array.isArray(status.scopes)) {
      const grantedScopes = status.scopes.filter((scope): scope is string => typeof scope === "string");
      const missingScopes = REQUIRED_GWS_SCOPES.filter((scope) => !grantedScopes.includes(scope));
      if (missingScopes.length > 0) {
        return {
          reason: `missing required scopes: ${missingScopes.join(", ")}`,
          credentialsUnusable: false,
        };
      }
    } else {
      console.info("[gmail-bot] gws auth status did not report scopes; skipping scope preflight");
    }
    try {
      this.assertExpectedAccount(status);
    } catch (error) {
      return { reason: errorReason(error), credentialsUnusable: false };
    }
    return null;
  }

  /* ---------- gws gmail +watch child process ---------- */

  private spawnWatcher(): void {
    if (this.stopping) {
      return;
    }

    // Reuse the topic an earlier run created (see watch-topic.ts): without
    // `--topic`, gws creates and leaks a new Pub/Sub topic on every start.
    const topic = this.configDir
      ? reusableTopic(readSavedWatchTopic(this.configDir), this.projectId)
      : undefined;
    const args = watchArgs(topic);
    console.info(`[gmail-bot] spawning gws ${args.join(" ")}`);

    const proc = spawn(this.gwsBinary, args, {
      stdio: ["ignore", "pipe", "pipe"],
      env: { ...process.env, ...this.gwsEnv() },
    });

    this.watchProcess = proc;
    this.watchJsonBuffer = "";
    this.watchAuthRejected = false;
    this.watchStderrTail = "";
    this.watchReusedTopic = topic;
    this.watchCreatedTopic = undefined;
    this.watchActive = false;
    this.watchErrorText = "";
    // A watcher that stays up long enough ends the failure streak, so exit-2s
    // days apart are not counted as consecutive (see `healthyRunMs`).
    this.resilient.recordStart();

    const rl = createInterface({ input: proc.stdout! });
    rl.on("line", (line: string) => {
      this.observeWatchSetupLine(line);
      void this.handleWatchLine(line);
    });
    // gws reports its Pub/Sub setup on either stream.
    createInterface({ input: proc.stderr! }).on("line", (line: string) => {
      this.observeWatchSetupLine(line);
    });

    proc.stderr?.on("data", (chunk: Buffer) => {
      const text = chunk.toString();
      this.watchStderrTail = appendBounded(this.watchStderrTail, text);
      if (isGoogleAuthRejection(this.watchStderrTail)) {
        this.watchAuthRejected = true;
      }
      const msg = text.trim();
      if (msg) {
        logChildProcessStderr("[gmail-bot][gws]", msg);
      }
    });

    proc.on("close", (code, signal) => {
      this.watchProcess = undefined;
      if (this.stopping) {
        console.info("[gmail-bot] watch process exited after stop");
        return;
      }
      // A saved topic someone deleted fails every start the same way: drop
      // it so the restart below lets gws create a fresh one.
      const reusedTopic = this.watchReusedTopic;
      if (
        this.configDir &&
        shouldForgetTopic({
          reusedTopic,
          watchActive: this.watchActive,
          errorText: this.watchErrorText + this.watchStderrTail,
        })
      ) {
        console.error(
          `[gmail-bot] saved Pub/Sub topic ${reusedTopic} no longer exists; forgetting it so the next start creates a new one`,
        );
        forgetWatchTopic(this.configDir);
      }
      // Google refusing the credentials is not a crash a restart can fix,
      // and `gws auth status` may still accept them (it only checks them
      // locally). Two runs in a row ending that way mean a new sign-in is
      // needed: surface it instead of restarting into the same rejection.
      this.authRejectedRuns = this.watchAuthRejected ? this.authRejectedRuns + 1 : 0;
      if (this.authRejectedRuns >= 2) {
        this.exitGoogleRejectedCredentials();
      }
      void this.resilient.handleExit(code, signal, () => this.spawnWatcher());
    });

    proc.on("error", (err) => {
      console.error("[gmail-bot] watch process error", err);
    });
  }

  /**
   * Track the watcher's Pub/Sub setup: remember a topic gws creates, and
   * save it for later runs once the Gmail watch is active on it.
   */
  private observeWatchSetupLine(line: string): void {
    const created = parseCreatedTopic(line);
    if (created) {
      this.watchCreatedTopic = created;
      return;
    }
    if (this.watchActive || !isWatchActiveLine(line)) {
      return;
    }
    this.watchActive = true;
    if (!this.watchCreatedTopic || !this.configDir) {
      return;
    }
    try {
      saveWatchTopic(this.configDir, this.watchCreatedTopic);
      console.info(`[gmail-bot] saved Pub/Sub topic ${this.watchCreatedTopic} for later watcher runs`);
    } catch (error) {
      console.error(`[gmail-bot] failed to save Pub/Sub topic ${this.watchCreatedTopic}: ${errorReason(error)}`);
    }
  }

  private async handleWatchLine(raw: string): Promise<void> {
    const payload = this.consumeWatchPayload(raw);
    if (!payload) {
      return;
    }

    if (isGwsErrorPayload(payload)) {
      const code = payload.error.code ?? "unknown";
      const reason = payload.error.reason ?? "unknown";
      const message = payload.error.message ?? "unknown error";
      const errorText = JSON.stringify(payload.error);
      this.watchErrorText = appendBounded(this.watchErrorText, `${errorText}\n`);
      if (isGoogleAuthRejection(errorText)) {
        this.watchAuthRejected = true;
      }
      console.error(
        `[gmail-bot] gws watch error (${code}/${reason}): ${message}`,
      );
      return;
    }

    const parsed = payload as GwsWatchMessage;
    const from = typeof parsed.from === "string" ? parsed.from.trim() : null;
    if (!from) {
      console.debug("[gmail-bot] message without from field, skipping");
      return;
    }

    this.resilient.recordSuccess();

    const channelAddress = parseEmailMailbox(from);
    if (!channelAddress) {
      console.warn("[gmail-bot] message has no unambiguous sender mailbox; refusing attribution");
      return;
    }

    const target =
      (typeof parsed.id === "string" ? parsed.id.trim() : null) ||
      "";
    if (!target) return; // A thread id is not a message id for Gmail replies.

    const subject = typeof parsed.subject === "string" ? parsed.subject.trim() : "";
    const body =
      (typeof parsed.body === "string" ? parsed.body.trim() : null) ||
      (typeof parsed.snippet === "string" ? parsed.snippet.trim() : null) ||
      "";

    const parts: string[] = [];
    if (subject) {
      parts.push(`Subject: ${subject}`);
    }
    if (body) {
      parts.push(body);
    }

    const text = parts.join("\n\n");
    if (!text) {
      console.debug("[gmail-bot] empty email content, skipping");
      return;
    }

    await this.handlers?.onTextMessage({
      target,
      channelAddress,
      text,
      ...(from !== channelAddress ? { displayName: from } : {}),
    });
  }

  private consumeWatchPayload(raw: string): GwsWatchMessage | GwsErrorPayload | null {
    const trimmed = raw.trim();
    if (!trimmed) {
      return null;
    }

    const candidate = this.watchJsonBuffer
      ? `${this.watchJsonBuffer}\n${trimmed}`
      : trimmed;

    try {
      const parsed = JSON.parse(candidate) as GwsWatchMessage | GwsErrorPayload;
      this.watchJsonBuffer = "";
      return parsed;
    } catch {
      if (
        this.watchJsonBuffer ||
        trimmed === "{" ||
        trimmed === "}" ||
        trimmed.startsWith("{") ||
        trimmed.startsWith("\"")
      ) {
        this.watchJsonBuffer = candidate;
        if (this.watchJsonBuffer.length > 64_000) {
          console.info("[gmail-bot] discarding oversized gws watch JSON buffer");
          this.watchJsonBuffer = "";
        }
        return null;
      }

      console.info("[gmail-bot] non-JSON line from gws watch:", trimmed);
      return null;
    }
  }

  /* ---------- gws exec helpers ---------- */

  private gwsEnv(): Record<string, string> {
    const env: Record<string, string> = {};

    if (this.configDir) {
      env.GOOGLE_WORKSPACE_CLI_CONFIG_DIR = this.configDir;
    }

    if (this.cloudSdkConfigDir) {
      env.CLOUDSDK_CONFIG = this.cloudSdkConfigDir;
    }

    if (this.projectId) {
      env.GOOGLE_WORKSPACE_PROJECT_ID = this.projectId;
    }

    return env;
  }

  /** Run a gws command and return its stdout. */
  private execGws(args: string[]): Promise<string> {
    return new Promise((resolve, reject) => {
      execFile(
        this.gwsBinary,
        args,
        { timeout: 30_000, env: { ...process.env, ...this.gwsEnv() } },
        (error, stdout) => {
          if (error) {
            reject(error);
          } else {
            resolve(stdout);
          }
        },
      );
    });
  }

  private assertExpectedAccount(status: Record<string, unknown>): void {
    if (!this.expectedAccount) {
      return;
    }

    const user =
      typeof status.user === "string" && status.user.trim()
        ? status.user.trim()
        : undefined;
    if (!user) {
      console.info(
        `[gmail-bot] gws auth status did not report user; skipping expected-account preflight for ${this.profileName}`,
      );
      return;
    }
    if (user.toLowerCase() !== this.expectedAccount.toLowerCase()) {
      throw new Error(`authenticated as ${user}; expected ${this.expectedAccount}`);
    }
  }

  private purgeStoredCredentials(): void {
    if (!this.configDir || !existsSync(this.configDir)) {
      return;
    }

    for (const filename of ["credentials.enc", "credentials.json", "token_cache.json"]) {
      try {
        rmSync(resolve(this.configDir, filename), { force: true });
      } catch {
        /* best effort */
      }
    }
  }
}

/* ---------- helpers ---------- */

/**
 * Extract a bare email address from a "Display Name <email@example.com>" string.
 * If no angle brackets are present, returns the input trimmed.
 */
/** Expand leading `~` or `~/` to the user's home directory. */
function expandTilde(value: string | undefined): string | undefined {
  if (!value) return value;
  if (value === "~") return process.env.HOME || value;
  if (value.startsWith("~/")) {
    return (process.env.HOME || "~") + value.slice(1);
  }
  return value;
}

function resolveProjectId(configDir: string | undefined): string | undefined {
  const envProjectId = process.env.GOOGLE_WORKSPACE_PROJECT_ID?.trim();
  if (envProjectId) {
    return envProjectId;
  }

  if (!configDir) {
    return undefined;
  }

  const clientSecretPath = resolve(configDir, "client_secret.json");
  if (!existsSync(clientSecretPath)) {
    return undefined;
  }

  try {
    const raw = readFileSync(clientSecretPath, "utf8");
    const parsed = JSON.parse(raw) as GoogleClientSecretPayload;
    return parsed.installed?.project_id || parsed.web?.project_id || parsed.project_id;
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    console.info(`[gmail-bot] failed to read project_id from ${clientSecretPath}: ${reason}`);
    return undefined;
  }
}

function logChildProcessStderr(label: string, message: string): void {
  const trimmed = message.trim();
  if (!trimmed) {
    return;
  }

  if (isErrorLikeProcessMessage(trimmed)) {
    console.error(`${label} ${trimmed}`);
    return;
  }

  console.info(`${label} ${trimmed}`);
}

function isErrorLikeProcessMessage(message: string): boolean {
  const lower = message.toLowerCase();
  return [
    /\berror\b/,
    /\bfailed\b/,
    /\bfatal\b/,
    /\bexception\b/,
    /\bpanic\b/,
    /\btraceback\b/,
    /\bdenied\b/,
    /\binvalid\b/,
    /\bunauthori[sz]ed\b/,
    /\bforbidden\b/,
    /\bpermission\b/,
    /\btimeout\b/,
    /\bunable\b/,
    /\bmissing\b/,
    /\bexited with code\b/,
  ].some((pattern) => pattern.test(lower));
}

function errorReason(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function isGwsErrorPayload(
  payload: GwsWatchMessage | GwsErrorPayload,
): payload is GwsErrorPayload & { error: NonNullable<GwsErrorPayload["error"]> } {
  return typeof payload === "object" && payload !== null && "error" in payload && !!payload.error;
}
