import { chunkExactText } from "@magician/bot-sdk";
import { createWriteStream, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { toFile as writeQrFile } from "qrcode";
import { sendWithServerAck } from "./server-ack.js";

import type {
  ChannelAdapter,
  ChannelAdapterHandlers,
  ChannelSendTextOptions,
  ChannelFileAttachment,
  TaskStatusUpdateRender,
  ToolCallExecutedRender,
} from "@magician/bot-sdk";

/* eslint-disable @typescript-eslint/no-explicit-any */
const wuLib: any = await import("@ibrahimwithi/wu-cli");
const createConnection: (...args: any[]) => Promise<any> = wuLib.createConnection;
const startListener: (...args: any[]) => void = wuLib.startListener;
const wuSendText: (...args: any[]) => Promise<any> = wuLib.sendText;
const wuSendMedia: (...args: any[]) => Promise<any> = wuLib.sendMedia;
const loadConfig: (...args: any[]) => any = wuLib.loadConfig;
/* eslint-enable @typescript-eslint/no-explicit-any */

const WHATSAPP_TEXT_LIMIT = 4_000;

/** Parsed message from the wu-cli listener callback. */
interface ParsedMessage {
  chatJid: string;
  senderJid: string | null;
  senderName: string | null;
  body: string | null;
  isFromMe: boolean;
}

export interface WhatsAppAdapterOptions {
  /** When true, keep the wu-cli connection alive but don't forward messages to Magician. */
  chatDisabled?: boolean;
}

export class WhatsAppAdapter implements ChannelAdapter<string> {
  private handlers?: ChannelAdapterHandlers<string>;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  private sock: any;
  private stopping = false;
  private connecting = false;
  private selfJid: string | undefined;
  private readonly selfJids = new Set<string>();
  private readonly chatDisabled: boolean;
  private reconnectTimer: ReturnType<typeof setTimeout> | undefined;

  constructor(options: WhatsAppAdapterOptions = {}) {
    this.chatDisabled = options.chatDisabled ?? false;
    if (this.chatDisabled) {
      console.info("[whatsapp-bot] chat disabled — connection kept alive for wu-cli tools only");
    }
  }

  async start(handlers: ChannelAdapterHandlers<string>): Promise<void> {
    this.handlers = handlers;
    this.stopping = false;
    await this.connect();
  }

  async stop(): Promise<void> {
    this.stopping = true;
    this.clearConnectionMarker();
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = undefined;
    }
    if (this.sock) {
      this.sock.end(undefined);
      this.sock = undefined;
    }
  }

  resolveRealtimeTarget(channelAddress: string): string {
    return channelAddress;
  }

  async sendText(target: string, text: string, options?: ChannelSendTextOptions): Promise<{ accepted: boolean }> {
    if (!this.sock) {
      console.info("[whatsapp-bot] cannot send — not connected");
      return { accepted: false };
    }
    const config = loadConfig();
    const sock = this.sock;
    const chunks = options?.preserveExactText ? chunkExactText(text, WHATSAPP_TEXT_LIMIT) : chunkWhatsAppText(text);
    for (const chunk of chunks) {
      const send = () => wuSendText(sock, target, chunk, config, {});
      if (options?.preserveExactText) {
        if (!await sendWithServerAck(sock.ws, send)) return { accepted: false };
      } else {
        await send();
      }
    }
    return { accepted: chunks.length > 0 };
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
    if (!this.sock) {
      console.info("[whatsapp-bot] cannot sendFile — not connected");
      return;
    }

    const tmpDir = mkdtempSync(join(tmpdir(), "wa-attach-"));
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

      const config = loadConfig();
      const opts = attachment.caption ? { caption: attachment.caption } : {};
      await wuSendMedia(this.sock, target, tmpPath, config, opts);
    } finally {
      try {
        rmSync(tmpDir, { recursive: true, force: true });
      } catch {
        /* ignore */
      }
    }
  }

  /* ---------- connection ---------- */

  /**
   * Create a single WhatsApp connection using the wu-cli library API.
   * Both the listener and sender share this socket — no connection conflicts.
   */
  private async connect(): Promise<void> {
    if (this.stopping || this.connecting) return;
    this.connecting = true;

    try {
      const wuHome = process.env.WU_HOME?.trim() || resolve(homedir(), ".wu");
      const authDir = resolve(wuHome, "auth");
      if (!existsSync(authDir)) {
        console.info(
          "[whatsapp-bot] no scoped auth state found at",
          authDir,
          "— waiting for wu-cli QR/pairing bootstrap",
        );
      }

      // Defense-in-depth: ensure `<wu_home>/config.yaml` declares
      // collection constraints. wu-cli's `loadConfig()` returns an
      // empty schema on ENOENT, leaving `constraints: undefined`,
      // which makes its listener's `shouldCollect(jid, config)` return
      // `false` for *every* JID — and silently drops every chat and
      // message during `messaging-history.set`. Contacts (which take
      // a different code path) still persist, so the failure mode is
      // particularly nasty: bot looks healthy, contacts table fills,
      // chats/messages stay at 0. The install-time fix is in
      // `skillshub/scripts/install_bot_bundles.py::seed_whatsapp_wu_config`,
      // but we self-heal here too in case the file was deleted, the
      // scope was provisioned through a different path, or wu-cli
      // ever changes its loadConfig defaults.
      ensureWuConstraintsConfig(wuHome);

      console.info("[whatsapp-bot] creating shared WhatsApp connection");

      const config = loadConfig();
      if (!(config as { constraints?: unknown }).constraints) {
        console.warn(
          "[whatsapp-bot] WARNING: wu-cli config has no `constraints` block — every chat and message will be silently dropped.",
          "Expected file:",
          resolve(wuHome, "config.yaml"),
        );
      }

      const { sock } = await createConnection({
        onQr: (qr: string) => {
          this.clearConnectionMarker();
          const qrFile = process.env.WHATSAPP_QR_FILE?.trim();
          if (qrFile) {
            mkdirSync(dirname(qrFile), { recursive: true });
            void writeQrFile(qrFile, qr, {
              type: "png",
              errorCorrectionLevel: "M",
              margin: 2,
              width: 512,
            })
              .then(() => console.info("[whatsapp-bot] pairing QR image ready"))
              .catch((error: unknown) => {
                console.error("[whatsapp-bot] failed to write pairing QR image", error);
              });
          }
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          import("qrcode-terminal").then((mod: any) => {
            console.info("[whatsapp-bot] Scan this QR code with WhatsApp:");
            mod.generate(qr, { small: true }, (output: string) => {
              process.stdout.write(output + "\n");
            });
          }).catch(() => {
            console.info("[whatsapp-bot] pairing QR is available through Bot Control");
          });
        },
        pairingPhone: process.env.WHATSAPP_PHONE?.trim() || undefined,
      });

      // Guard against stop() racing with createConnection()
      if (this.stopping) {
        sock.end(undefined);
        return;
      }

      this.sock = sock;

      this.refreshSelfJids(sock, wuHome, "connection");

      // Start listener on the SAME socket — no separate process
      startListener(sock, {
        config,
        onMessage: (parsed: ParsedMessage) => {
          void this.handleMessage(parsed);
        },
      });

      // Handle disconnection.
      //
      // On `close`, explicitly tear down the dying socket before
      // scheduling the reconnect. Without `sock.end()` + listener
      // cleanup, every flaky reconnect leaks an additional
      // EventEmitter registration on `sock.ev` — and the prior socket
      // can re-emit buffered events into a stale `onMessage` closure,
      // producing duplicate `handleMessage` calls (and duplicate
      // outbound replies) once the new socket finally takes over.
      sock.ev.on("connection.update", (update: { connection?: string }) => {
        if (update.connection === "close" && !this.stopping) {
          this.clearConnectionMarker();
          console.info("[whatsapp-bot] connection closed, reconnecting in 5s");
          try {
            sock.ev.removeAllListeners?.();
          } catch {
            /* baileys versions differ on availability — non-fatal */
          }
          try {
            sock.end?.(undefined);
          } catch {
            /* idempotent close — safe to swallow */
          }
          this.sock = undefined;
          this.reconnectTimer = setTimeout(() => void this.connect(), 5_000);
        } else if (update.connection === "open") {
          const qrFile = process.env.WHATSAPP_QR_FILE?.trim();
          if (qrFile) {
            try {
              rmSync(qrFile, { force: true });
            } catch {
              /* auth status remains authoritative if stale-file cleanup fails */
            }
          }
          const statusFile = process.env.WHATSAPP_STATUS_FILE?.trim();
          if (statusFile) {
            try {
              mkdirSync(dirname(statusFile), { recursive: true });
              writeFileSync(statusFile, new Date().toISOString() + "\n", { mode: 0o600 });
            } catch (error) {
              console.error("[whatsapp-bot] failed to publish connection status", error);
            }
          }
          console.info("[whatsapp-bot] connected and listening");
          // Always refresh self JID aliases on reconnect — account may have changed.
          this.refreshSelfJids(sock, wuHome, "connection.open");
        }
      });

      console.info("[whatsapp-bot] started");
    } finally {
      this.connecting = false;
    }
  }

  private clearConnectionMarker(): void {
    const statusFile = process.env.WHATSAPP_STATUS_FILE?.trim();
    if (!statusFile) return;
    try {
      rmSync(statusFile, { force: true });
    } catch {
      /* auth status falls back to needs-auth if the marker cannot be read */
    }
  }

  /* ---------- message handling ---------- */

  private async handleMessage(parsed: ParsedMessage): Promise<void> {
    if (this.chatDisabled) return;

    const jid = parsed.chatJid;
    if (!jid) return;

    if (this.selfJids.size === 0) return;
    // The package entrypoint disables inbound forwarding so personal self-chat
    // remains observation/history only. Keep the self-chat guard here as
    // defense-in-depth if that wiring is ever changed.
    //
    // The previous `|| parsed.isFromMe` fallback was wrong: WhatsApp's
    // `fromMe` flag is `true` for ANY outbound message, including
    // messages the user sends from their phone to other contacts. So
    // a DM to Alice would match (`chatJid = Alice`, `fromMe = true`),
    // the bot would forward Alice-bound text to the LLM, and post the
    // LLM response back into the user's self-chat. Net effect: every
    // outbound message anywhere triggered an LLM round-trip.
    //
    // Self-chat messages are always `fromMe = true` AND the chat JID
    // equals one of the account's own identifiers. Newer WhatsApp
    // sessions can report the same self thread as either the phone
    // JID (`...@s.whatsapp.net`) or the linked LID JID (`...@lid`),
    // so keep both aliases instead of falling back to `fromMe`.
    const normalizedChatJid = normalizeJid(jid);
    const isSelfChat = this.selfJids.has(normalizedChatJid);
    if (!isSelfChat) return;

    const text = parsed.body?.trim();
    if (!text) return;

    const displayName = parsed.senderName?.trim() || undefined;
    const replyTarget = this.selfJid ?? normalizedChatJid;

    await this.handlers?.onTextMessage({
      target: replyTarget,
      channelAddress: replyTarget,
      text,
      ...(displayName ? { displayName } : {}),
    });
  }

  private refreshSelfJids(sock: any, wuHome: string, source: string): void {
    const envJid = process.env.WHATSAPP_SELF_JID?.trim();
    const candidates = [
      envJid,
      stringValue(sock?.user?.id),
      stringValue(sock?.user?.lid),
      stringValue(sock?.authState?.creds?.me?.id),
      stringValue(sock?.authState?.creds?.me?.lid),
      ...readSelfJidCandidatesFromCreds(wuHome),
    ];

    // Compute the new set into a local first so a transient flaky
    // reconnect (sock.user briefly empty, creds.json unreadable) can't
    // clobber the previously-known state. We only commit if we
    // resolved at least one candidate. Prior code did a `clear()`
    // followed by population, which on a reconnect-with-no-candidates
    // dropped the bot into a state with `selfJids.size === 0` and
    // silently halted all incoming message handling until the next
    // reconnect produced values.
    const nextSelfJids = new Set<string>();
    for (const candidate of candidates) {
      if (!candidate) continue;
      nextSelfJids.add(normalizeJid(candidate));
    }
    if (nextSelfJids.size === 0) {
      console.info(
        `[whatsapp-bot] self JID refresh from ${source}: no candidates; keeping prior state`,
      );
      return;
    }

    this.selfJids.clear();
    for (const jid of nextSelfJids) {
      this.selfJids.add(jid);
    }
    const normalizedEnvJid = envJid ? normalizeJid(envJid) : undefined;
    this.selfJid =
      normalizedEnvJid ??
      Array.from(this.selfJids).find((jid) => jid.endsWith("@s.whatsapp.net")) ??
      Array.from(this.selfJids)[0];

    if (this.selfJid) {
      const aliasCount = Math.max(0, this.selfJids.size - 1);
      console.info(
        `[whatsapp-bot] self JID from ${source}: ${this.selfJid}`,
        aliasCount === 1 ? "(1 alias)" : `(${aliasCount} aliases)`,
      );
    } else {
      console.info("[whatsapp-bot] could not determine self JID");
    }
  }

}

/* ---------- helpers ---------- */

/**
 * Self-heal `<wu_home>/config.yaml` if it's missing the `constraints`
 * block wu-cli's listener depends on for chat/message collection.
 *
 * Behavior:
 * - File missing  → write the canonical default and log an info note.
 * - File exists, has `constraints:` line → leave alone.
 * - File exists, no `constraints:` line  → log a warning. We don't
 *   rewrite — operator may have intentionally narrowed collection.
 *
 * The check is intentionally substring-based ("constraints:" present
 * anywhere in the file) rather than YAML-parsed: this file imports
 * no YAML parser of its own, and the check only needs to discriminate
 * "operator has a constraints block" from "wu-cli's loadConfig will
 * silently fall back to undefined".
 */
function ensureWuConstraintsConfig(wuHome: string): void {
  const configPath = resolve(wuHome, "config.yaml");
  const canonicalDefault =
    "constraints:\n  default: full\n  chats: {}\n";
  if (!existsSync(configPath)) {
    try {
      mkdirSync(dirname(configPath), { recursive: true });
      writeFileSync(configPath, canonicalDefault, { encoding: "utf8" });
      console.info(
        "[whatsapp-bot] seeded missing wu config at",
        configPath,
        "(constraints: default=full) — required for chat/message collection",
      );
    } catch (err) {
      console.warn(
        "[whatsapp-bot] failed to seed wu config at",
        configPath,
        "—",
        err instanceof Error ? err.message : String(err),
      );
    }
    return;
  }
  // File exists; sanity-check that it carries a top-level constraints
  // block. wu-cli's loadConfig only reads `constraints` at the YAML
  // root, so we anchor at column 0 — a nested `constraints:` under a
  // parent mapping would still leave the loader returning undefined
  // and silent-drop every chat. Operators who customize the file
  // beyond this are on their own.
  try {
    const raw = readFileSync(configPath, "utf8");
    if (!/^constraints\s*:/m.test(raw)) {
      console.warn(
        "[whatsapp-bot] wu config at",
        configPath,
        "is missing a `constraints:` block — chat/message collection will be silently dropped.",
        "Add `constraints:\\n  default: full\\n  chats: {}` to fix.",
      );
    }
  } catch {
    /* read failure is non-fatal — wu-cli's own loadConfig will surface it */
  }
}

function readSelfJidCandidatesFromCreds(wuHome: string): string[] {
  const credsPath = resolve(wuHome, "auth", "creds.json");
  if (!existsSync(credsPath)) return [];
  try {
    const raw = readFileSync(credsPath, "utf8");
    const creds = JSON.parse(raw) as {
      me?: { id?: unknown; lid?: unknown; jid?: unknown };
      account?: { id?: unknown; lid?: unknown; jid?: unknown };
    };
    return [
      stringValue(creds.me?.id),
      stringValue(creds.me?.lid),
      stringValue(creds.me?.jid),
      stringValue(creds.account?.id),
      stringValue(creds.account?.lid),
      stringValue(creds.account?.jid),
    ].filter((candidate): candidate is string => Boolean(candidate));
  } catch (err) {
    // A corrupt or mid-write `creds.json` here means the bot would
    // silently run with no JID aliases and stop responding in
    // self-chat. Surface the failure so the operator can correlate
    // it with the "WhatsApp not ready"-style misdiagnoses SKILL.md
    // warns about, instead of swallowing it into an empty array.
    console.warn(
      "[whatsapp-bot] failed to read self-jid candidates from",
      credsPath,
      "—",
      err instanceof Error ? err.message : String(err),
    );
    return [];
  }
}

function stringValue(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

function normalizeJid(jid: string): string {
  const atIdx = jid.indexOf("@");
  if (atIdx === -1) {
    // Bare phone number → default domain. Strip a leading `+`
    // (E.164-style) so operators can paste their number from contacts
    // without the env value silently falling through to the
    // pass-through branch and missing the set-lookup against inbound
    // `…@s.whatsapp.net` JIDs.
    const stripped = jid.startsWith("+") ? jid.slice(1) : jid;
    return /^\d+$/.test(stripped) ? `${stripped}@s.whatsapp.net` : jid;
  }
  // Strip device suffix (e.g., 1234567890:12@s.whatsapp.net → 1234567890@s.whatsapp.net)
  const local = jid.slice(0, atIdx).split(":")[0] ?? "";
  const domain = jid.slice(atIdx);
  return `${local}${domain}`;
}

function chunkWhatsAppText(text: string): string[] {
  const trimmed = text.trim();
  if (!trimmed) return [];
  if (trimmed.length <= WHATSAPP_TEXT_LIMIT) return [trimmed];

  const chunks: string[] = [];
  let remaining = trimmed;
  while (remaining.length > WHATSAPP_TEXT_LIMIT) {
    let splitAt = remaining.lastIndexOf("\n", WHATSAPP_TEXT_LIMIT);
    if (splitAt <= 0) splitAt = remaining.lastIndexOf(" ", WHATSAPP_TEXT_LIMIT);
    if (splitAt <= 0) splitAt = WHATSAPP_TEXT_LIMIT;
    chunks.push(remaining.slice(0, splitAt).trim());
    remaining = remaining.slice(splitAt).trimStart();
  }
  if (remaining) chunks.push(remaining);
  return chunks;
}
