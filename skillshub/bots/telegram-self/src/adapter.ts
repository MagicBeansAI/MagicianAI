import { chunkExactText } from "@magician/bot-sdk";
import { type ChildProcess, execFile, spawn } from "node:child_process";
import { createWriteStream, existsSync, mkdtempSync, rmSync } from "node:fs";
import { unlink } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { createInterface } from "node:readline";

import { exitNeedsAuth, ResilientProcess } from "@magician/bot-sdk";

import type {
  ChannelAdapter,
  ChannelAdapterHandlers,
  ChannelSendTextOptions,
  ChannelFileAttachment,
  TaskStatusUpdateRender,
  ToolCallExecutedRender,
} from "@magician/bot-sdk";

const TELEGRAM_TEXT_LIMIT = 4_096;

/**
 * Incoming message shape from `tgcli sync --follow --json`.
 * We parse defensively — not all fields may be present.
 */
interface TgcliSyncMessage {
  /** Chat/peer ID */
  chat_id?: number | string;
  /** Sender user ID */
  from_id?: number | string;
  /** The logged-in user's own ID */
  self_id?: number | string;
  /** Message text */
  text?: string;
  /** Display name of the sender */
  from_name?: string;
  /** Message ID */
  message_id?: number | string;
  /** Whether the message is outgoing (sent by self) */
  out?: boolean;
  /** Peer type: "user", "chat", "channel" */
  peer_type?: string;
  [key: string]: unknown;
}

export interface TelegramSelfAdapterOptions {
  /** Absolute path to the `tgcli` binary. */
  tgcliBinary: string;
  /** Optional TGCLI_STORE override for session data. */
  store?: string | undefined;
  /** Optional Telegram API ID. */
  apiId?: string | undefined;
  /** Optional Telegram API hash. */
  apiHash?: string | undefined;
}

export class TelegramSelfAdapter implements ChannelAdapter<string> {
  private handlers?: ChannelAdapterHandlers<string>;
  private syncProcess: ChildProcess | undefined;
  private stopping = false;
  private selfId: string | undefined;
  private readonly tgcliBinary: string;
  private readonly store: string | undefined;
  private readonly apiId: string | undefined;
  private readonly apiHash: string | undefined;
  private readonly resilient: ResilientProcess;

  constructor(options: TelegramSelfAdapterOptions) {
    this.tgcliBinary = options.tgcliBinary;
    this.store = options.store;
    this.apiId = options.apiId;
    this.apiHash = options.apiHash;
    this.resilient = new ResilientProcess({
      label: "[telegram-self-bot]",
      authFailureCodes: [], // tgcli doesn't have a specific auth exit code
      onReauth: (shouldPurge) => this.ensureAuth(shouldPurge),
    });
  }

  async start(handlers: ChannelAdapterHandlers<string>): Promise<void> {
    this.handlers = handlers;
    this.stopping = false;
    await this.ensureAuth();
    this.spawnSync();
  }

  async stop(): Promise<void> {
    this.stopping = true;

    if (this.syncProcess) {
      const proc = this.syncProcess;
      this.syncProcess = undefined;
      proc.kill("SIGTERM");
    }
  }

  async sendText(target: string, text: string, options?: ChannelSendTextOptions): Promise<{ accepted: boolean }> {
    const chunks = options?.preserveExactText ? chunkExactText(text, TELEGRAM_TEXT_LIMIT) : chunkTelegramText(text);
    for (const chunk of chunks) {
      await this.execTgcli(["send", "text", "--to", target, "--message", chunk]);
    }
    return { accepted: true };
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

  async sendFile(
    target: string,
    attachment: ChannelFileAttachment,
  ): Promise<void> {
    const tmpDir = mkdtempSync(join(tmpdir(), "tg-attach-"));
    const safeName = attachment.display_name.replace(/[^A-Za-z0-9._-]+/g, "_");
    const tmpPath = join(tmpDir, safeName || "file");

    try {
      const response = await fetch(attachment.url, { headers: attachment.download_headers });
      if (!response.ok || !response.body) {
        throw new Error(
          `download failed: ${response.status} ${response.statusText}`,
        );
      }
      await pipeline(
        Readable.fromWeb(response.body as never),
        createWriteStream(tmpPath),
      );

      const useSendPhoto = attachment.kind === "image";
      const args: string[] = [
        "send",
        useSendPhoto ? "photo" : "file",
        "--to",
        target,
        useSendPhoto ? "--photo" : "--file",
        tmpPath,
      ];
      if (attachment.caption) {
        args.push("--caption", attachment.caption);
      }

      await this.execTgcli(args, 120_000);
    } finally {
      await unlink(tmpPath).catch(() => {});
      try {
        rmSync(tmpDir, { recursive: true, force: true });
      } catch {
        /* ignore */
      }
    }
  }

  /* ---------- auth ---------- */

  /**
   * Probe tgcli auth state. On failure, signal "needs auth" to the magician
   * supervisor (sidecar + exit 79). We never launch `tgcli auth --qr` from
   * here — that would tie the QR display to the bot startup path and make
   * every restart re-show the QR. The operator triggers the QR flow
   * explicitly via the UI's "Authenticate" button, which restarts the
   * bot; the QR code surfaces in bot logs the same way it always has.
   */
  private async ensureAuth(forcePurge = false): Promise<void> {
    if (forcePurge) {
      const storeDir = this.store || resolve(homedir(), ".tgcli");
      if (existsSync(storeDir)) {
        console.info("[telegram-self-bot] purging stale tgcli session at", storeDir);
        rmSync(storeDir, { recursive: true, force: true });
      }
    }

    try {
      await this.execTgcli(["channels", "--limit", "1", "--json"]);
      console.info("[telegram-self-bot] tgcli auth verified");
      return;
    } catch (error) {
      const reason = error instanceof Error ? error.message : String(error);
      console.error(`[telegram-self-bot] auth required: ${reason}`);
      console.error(
        `[telegram-self-bot] click 'Authenticate' in Bot Control to start QR login`,
      );
      exitNeedsAuth({
        provider: "telegram_self",
        profileLabel: process.env.TGCLI_PROFILE_LABEL?.trim() || "telegram-self",
        detail: reason,
      });
    }
  }

  /* ---------- tgcli sync child process ---------- */

  private spawnSync(): void {
    if (this.stopping) {
      return;
    }

    console.info("[telegram-self-bot] spawning tgcli sync --follow --json");

    const proc = spawn(this.tgcliBinary, ["sync", "--follow", "--json"], {
      stdio: ["ignore", "pipe", "pipe"],
      env: { ...process.env, ...this.tgcliEnv() },
    });

    this.syncProcess = proc;

    const rl = createInterface({ input: proc.stdout! });
    rl.on("line", (line: string) => {
      this.resilient.recordSuccess();
      void this.handleSyncLine(line);
    });

    proc.stderr?.on("data", (chunk: Buffer) => {
      const msg = chunk.toString().trim();
      if (msg) {
        logChildProcessStderr("[telegram-self-bot][tgcli]", msg);
      }
    });

    proc.on("close", (code, signal) => {
      this.syncProcess = undefined;
      if (this.stopping) {
        console.info("[telegram-self-bot] sync process exited after stop");
        return;
      }
      void this.resilient.handleExit(code, signal, () => this.spawnSync());
    });

    proc.on("error", (err) => {
      console.error("[telegram-self-bot] sync process error", err);
    });
  }

  private async handleSyncLine(raw: string): Promise<void> {
    const trimmed = raw.trim();
    if (!trimmed) {
      return;
    }

    let parsed: TgcliSyncMessage;
    try {
      parsed = JSON.parse(trimmed) as TgcliSyncMessage;
    } catch {
      console.info("[telegram-self-bot] non-JSON line from tgcli sync:", trimmed);
      return;
    }

    // Track self ID from messages for self-chat detection
    if (parsed.self_id != null) {
      this.selfId = String(parsed.self_id);
    }

    // Only forward messages from "Saved Messages" (self-chat).
    // In Telegram, saved messages have the same chat_id as the user's own ID,
    // or the message is outgoing to self.
    if (!this.isSelfChat(parsed)) {
      return;
    }

    const text = typeof parsed.text === "string" ? parsed.text.trim() : null;
    if (!text) {
      return;
    }

    const chatId = String(parsed.chat_id ?? parsed.from_id ?? "self");
    const displayName =
      typeof parsed.from_name === "string"
        ? parsed.from_name.trim() || undefined
        : undefined;

    await this.handlers?.onTextMessage({
      target: chatId,
      channelAddress: chatId,
      text,
      ...(displayName ? { displayName } : {}),
    });
  }

  /**
   * Determine if a message is from "Saved Messages" (self-chat).
   * In Telegram, saved messages are where chat_id equals the user's own ID,
   * or the message is sent by self to self.
   */
  private isSelfChat(msg: TgcliSyncMessage): boolean {
    const chatId = msg.chat_id != null ? String(msg.chat_id) : null;
    const fromId = msg.from_id != null ? String(msg.from_id) : null;

    // If we know our self ID, check if chat_id matches (Saved Messages)
    if (this.selfId && chatId === this.selfId) {
      return true;
    }

    // If self_id is in the message and matches chat_id
    if (msg.self_id != null && chatId === String(msg.self_id)) {
      return true;
    }

    // Message is outgoing (from self) and peer is self
    if (msg.out === true && fromId != null && chatId === fromId) {
      return true;
    }

    // peer_type check: if the peer is "user" and chat_id matches from_id
    // (self-chat scenario)
    if (msg.peer_type === "user" && chatId != null && chatId === fromId) {
      return true;
    }

    return false;
  }

  /* ---------- tgcli exec helpers ---------- */

  /**
   * Build the extra env vars for tgcli commands.
   */
  private tgcliEnv(): Record<string, string> {
    const env: Record<string, string> = {};
    if (this.store) {
      env["TGCLI_STORE"] = this.store;
    }
    if (this.apiId) {
      env["TGCLI_API_ID"] = this.apiId;
    }
    if (this.apiHash) {
      env["TGCLI_API_HASH"] = this.apiHash;
    }
    return env;
  }

  private execTgcli(args: string[], timeoutMs = 30_000): Promise<string> {
    return new Promise((resolve, reject) => {
      execFile(
        this.tgcliBinary,
        args,
        { timeout: timeoutMs, env: { ...process.env, ...this.tgcliEnv() } },
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
}

/* ---------- helpers ---------- */

function chunkTelegramText(text: string): string[] {
  const trimmed = text.trim();
  if (!trimmed) {
    return [];
  }

  if (trimmed.length <= TELEGRAM_TEXT_LIMIT) {
    return [trimmed];
  }

  const chunks: string[] = [];
  let remaining = trimmed;

  while (remaining.length > TELEGRAM_TEXT_LIMIT) {
    let splitAt = remaining.lastIndexOf("\n", TELEGRAM_TEXT_LIMIT);
    if (splitAt <= 0) {
      splitAt = remaining.lastIndexOf(" ", TELEGRAM_TEXT_LIMIT);
    }
    if (splitAt <= 0) {
      splitAt = TELEGRAM_TEXT_LIMIT;
    }

    chunks.push(remaining.slice(0, splitAt).trim());
    remaining = remaining.slice(splitAt).trimStart();
  }

  if (remaining) {
    chunks.push(remaining);
  }

  return chunks;
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
