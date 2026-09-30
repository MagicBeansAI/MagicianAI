import { type ChildProcess, execFile, spawn } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import { basename, resolve } from "node:path";
import { createInterface } from "node:readline";
import { prepareEnvoyReply } from "./envoy-reply.js";

import {
  EX_NEEDS_AUTH,
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
      authFailureCodes: [5], // gws exits 5 on missing/invalid credentials
      // Mid-watch auth failure: bubble it up as a needs-auth exit instead of
      // re-running the OAuth flow inline (which used to pop a browser
      // window). The supervisor surfaces the escalation; the operator
      // clicks Authenticate to recover.
      onReauth: (shouldPurge) => {
        if (shouldPurge) {
          this.purgeStoredCredentials();
        }
        console.error(
          `[gmail-bot] auth failure mid-watch — exiting with code ${EX_NEEDS_AUTH} so supervisor can surface 'Authenticate' escalation`,
        );
        exitNeedsAuth({
          provider: "google_workspace",
          profileLabel: this.profileName,
          expectedAccount: this.expectedAccount,
          detail: "session expired or credentials revoked mid-watch",
        });
      },
      onGiveUp: () => {
        // Unreachable under the new flow (onReauth above terminates the
        // process before retries accumulate) but kept as a safety net.
        console.error(
          "[gmail-bot] persistent auth failure — exiting so the system can surface the problem",
        );
        exitNeedsAuth({
          provider: "google_workspace",
          profileLabel: this.profileName,
          expectedAccount: this.expectedAccount,
          detail: "persistent auth failure",
        });
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
   */
  private async ensureAuth(forcePurge = false): Promise<void> {
    if (forcePurge) {
      this.purgeStoredCredentials();
    }

    try {
      const statusJson = await this.execGws(["auth", "status", "--format", "json"]);
      const status = JSON.parse(statusJson) as Record<string, unknown>;
      if (status.auth_method === "none" || status.storage === "none") {
        throw new Error("no credentials stored");
      }
      if (status.encryption_valid === false) {
        throw new Error("encrypted credentials could not be decrypted");
      }
      if (status.has_refresh_token === false) {
        throw new Error("no refresh token stored");
      }
      if (status.token_valid === false) {
        const tokenError =
          typeof status.token_error === "string" && status.token_error.trim()
            ? status.token_error.trim()
            : "token invalid";
        throw new Error(tokenError);
      }
      if (Array.isArray(status.scopes)) {
        const grantedScopes = status.scopes.filter((scope): scope is string => typeof scope === "string");
        const missingScopes = REQUIRED_GWS_SCOPES.filter((scope) => !grantedScopes.includes(scope));
        if (missingScopes.length > 0) {
          throw new Error(`missing required scopes: ${missingScopes.join(", ")}`);
        }
      } else {
        console.info("[gmail-bot] gws auth status did not report scopes; skipping scope preflight");
      }
      this.assertExpectedAccount(status);
      console.info("[gmail-bot] gws auth verified");
    } catch (error) {
      const reason = error instanceof Error ? error.message : String(error);
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
  }

  /* ---------- gws gmail +watch child process ---------- */

  private spawnWatcher(): void {
    if (this.stopping) {
      return;
    }

    console.info("[gmail-bot] spawning gws gmail +watch --format json");

    const proc = spawn(this.gwsBinary, ["gmail", "+watch", "--format", "json"], {
      stdio: ["ignore", "pipe", "pipe"],
      env: { ...process.env, ...this.gwsEnv() },
    });

    this.watchProcess = proc;
    this.watchJsonBuffer = "";

    const rl = createInterface({ input: proc.stdout! });
    rl.on("line", (line: string) => {
      void this.handleWatchLine(line);
    });

    proc.stderr?.on("data", (chunk: Buffer) => {
      const msg = chunk.toString().trim();
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
      void this.resilient.handleExit(code, signal, () => this.spawnWatcher());
    });

    proc.on("error", (err) => {
      console.error("[gmail-bot] watch process error", err);
    });
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

function isGwsErrorPayload(
  payload: GwsWatchMessage | GwsErrorPayload,
): payload is GwsErrorPayload & { error: NonNullable<GwsErrorPayload["error"]> } {
  return typeof payload === "object" && payload !== null && "error" in payload && !!payload.error;
}
