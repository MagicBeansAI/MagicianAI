import type { IncomingMessage, Server } from "node:http";
import { parseEmailMailbox } from "@magician/bot-sdk";

import type {
  ChannelAdapter,
  ChannelAdapterHandlers,
  ChannelSendTextOptions,
  TaskStatusUpdateRender,
  ToolCallExecutedRender,
} from "@magician/bot-sdk";
import { AgentMailClient } from "agentmail";
import express from "express";
import { Webhook, WebhookVerificationError } from "svix";

const SELF_REGISTER_CLIENT_ID = "magician-presto-inbox";
const PREVIEW_LIMIT = 200;
// Served path for the inbound webhook. Distinct from the kapso bot's `/webhook`
// so both bots can share one public host (e.g. a single Cloudflare Tunnel):
//   https://<host>/agentmail-webhook  →  this bot
//   https://<host>/webhook            →  the kapso bot
const WEBHOOK_PATH = "/agentmail-webhook";

export interface AgentMailAdapterOptions {
  apiKey: string;
  webhookPort: number;
  webhookUrl?: string | undefined;
  webhookSecret?: string | undefined;
}

/**
 * Per-sender context captured from the inbound `message.received` event so the
 * envoy's reply threads back onto the same email conversation. Keyed by the
 * lower-cased sender email (the adapter target). The AgentMail JS SDK's
 * `reply(inboxId, messageId, ...)` keeps the reply in the same thread; we fall
 * back to a fresh `send` with a `Re:` subject if we never saw an inbound for a
 * target (e.g. an outbound nudge initiated by the runtime).
 */
interface ReplyContext {
  inboxId: string;
  messageId: string;
  threadId?: string;
  subject?: string;
}

export class AgentMailAdapter implements ChannelAdapter<string> {
  private handlers?: ChannelAdapterHandlers<string>;
  private app: express.Express | undefined;
  private server: Server | undefined;
  private keepaliveTimer: ReturnType<typeof setInterval> | undefined;
  private readonly client: AgentMailClient;
  private readonly apiKey: string;
  private readonly webhookPort: number;
  private readonly webhookUrl: string | undefined;
  private webhookSecret: string | undefined;
  // Last inbound message per sender, for threaded replies.
  private readonly replyContexts = new Map<string, ReplyContext>();

  constructor(options: AgentMailAdapterOptions) {
    this.apiKey = options.apiKey;
    this.webhookPort = options.webhookPort;
    this.webhookUrl = options.webhookUrl?.trim() || undefined;
    this.webhookSecret = options.webhookSecret?.trim() || undefined;
    this.client = new AgentMailClient({ apiKey: options.apiKey });
  }

  async start(handlers: ChannelAdapterHandlers<string>): Promise<void> {
    if (!this.webhookSecret && !this.webhookUrl) {
      throw new Error("AgentMail requires AGENTMAIL_WEBHOOK_SECRET or AGENTMAIL_WEBHOOK_URL for verified webhooks");
    }
    this.handlers = handlers;

    this.app = express();
    // Capture the raw body so svix signature verification works. We still
    // get the parsed JSON on req.body via the `verify` callback's buffer.
    this.app.use(
      express.json({
        verify: (req: IncomingMessage & { rawBody?: Buffer }, _res, buf) => {
          req.rawBody = Buffer.from(buf);
        },
      }),
    );

    this.app.post(
      WEBHOOK_PATH,
      (req: express.Request & { rawBody?: Buffer }, res: express.Response) => {
        const rawBody =
          req.rawBody ?? Buffer.from(JSON.stringify(req.body ?? {}));
        if (!this.verifySignature(req, rawBody)) {
          // Registration may still be obtaining the secret. Ask the provider
          // to retry; no unverified payload may populate reply contexts or
          // become a channelVerified sender.
          res.sendStatus(this.webhookSecret ? 401 : 503);
          return;
        }
        // Ack fast, then handle async (mirrors the kapso bot's pattern).
        res.sendStatus(200);
        void this.handleEvent(req.body);
      },
    );

    this.app.get(WEBHOOK_PATH, (_req, res) => {
      res.send("AgentMail webhook receiver is running.");
    });

    await new Promise<void>((resolve, reject) => {
      this.server = this.app!.listen(this.webhookPort, (error?: Error) => {
        if (error) {
          this.server = undefined;
          this.app = undefined;
          reject(error);
          return;
        }
        // A webhook-only runtime can otherwise fall off the active-handle
        // set after startup, making Node exit 0 even though the server
        // should remain resident.
        this.server?.ref?.();
        this.keepaliveTimer ??= setInterval(() => {}, 60 * 60 * 1000);
        console.info(
          `[agentmail-bot] webhook server listening on port ${this.webhookPort}` +
            (this.webhookSecret
              ? " (signature verification ENABLED)"
              : " (waiting for a signing secret; events are refused)"),
        );
        resolve();
      });
    });

    await this.selfRegisterWebhook();
    if (!this.webhookSecret) {
      await this.stop();
      throw new Error("AgentMail webhook registration supplied no signing secret; set AGENTMAIL_WEBHOOK_SECRET before restarting");
    }
    console.info("[agentmail-bot] webhook receiver ready");
  }

  async stop(): Promise<void> {
    if (this.keepaliveTimer) {
      clearInterval(this.keepaliveTimer);
      this.keepaliveTimer = undefined;
    }
    if (this.server) {
      await new Promise<void>((resolve, reject) => {
        this.server!.close((err) => (err ? reject(err) : resolve()));
      });
      this.server = undefined;
      this.app = undefined;
      console.info("[agentmail-bot] webhook server stopped");
    }
  }

  resolveRealtimeTarget(channelAddress: string): string {
    return channelAddress;
  }

  /**
   * Send the envoy's reply back out as an EMAIL to `target` from the inbox the
   * `AGENT_MAIL_KEY` is scoped to (magican@agentmail.to). When we have the
   * inbound message context, use `reply()` so it stays in the same thread;
   * otherwise fall back to a fresh `send()` with a `Re:` subject. Errors are
   * logged but never thrown — a failed email send must not crash the bot.
   */
  async sendText(target: string, text: string, options?: ChannelSendTextOptions): Promise<{ accepted: boolean }> {
    const body = options?.preserveExactText ? text : text?.trim();
    if (!body?.trim()) return { accepted: false };

    const ctx = this.replyContexts.get(target.toLowerCase());
    try {
      if (ctx) {
        const receipt = await this.client.inboxes.messages.reply(ctx.inboxId, ctx.messageId, {
          text: body,
          // A tracked reply may not inherit Reply-To/CC/BCC recipients from
          // the source email after the host bound its intended audience.
          ...(options?.preserveExactText ? { to: [target], cc: [], bcc: [] } : {}),
        }, options?.preserveExactText ? {
          maxRetries: 0,
          // The SDK clears its own timeout when headers arrive. This signal
          // remains active while it reads either a success or an error body.
          abortSignal: AbortSignal.timeout(30_000),
        } : undefined);
        // A retryable HTTP error can arrive after the provider has accepted
        // the mail. The outer receipt protocol owns recovery; a second SDK
        // POST would be a second email under the same dispatch grant.
        return { accepted: typeof receipt?.message_id === "string" && receipt.message_id.length > 0 };
      }
      // No inbound context for this target — best-effort fresh send. We need an
      // inbox id; without an inbound event we don't have one, so this path only
      // works if the runtime ever targets an address we have never heard from.
      // In practice the envoy only replies to senders we just received from, so
      // `ctx` is set. Log loudly if we hit this, since the send below has no
      // inbox to send from.
      console.warn(
        "[agentmail-bot] sendText: no inbound context for target — cannot thread",
        { target },
      );
    } catch (error) {
      console.error("[agentmail-bot] sendText failed", {
        target,
        threaded: Boolean(ctx),
        error: error instanceof Error ? error.message : error,
      });
    }
    return { accepted: false };
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

  /* ---------- webhook handling ---------- */

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  private async handleEvent(body: any): Promise<void> {
    try {
      const eventType = body?.event_type ?? body?.type;
      if (eventType === "message.received") {
        await this.handleMessageReceived(body);
        return;
      }
      // Log other event types too — useful for confirming the pipe and for
      // understanding what AgentMail delivers (sent/delivered/bounced…).
      console.info("[agentmail-bot] event received (ignored)", {
        eventType: eventType ?? "<unknown>",
        raw: truncate(JSON.stringify(body)),
      });
    } catch (error) {
      console.error("[agentmail-bot] event handler error", error);
    }
  }

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  private async handleMessageReceived(body: any): Promise<void> {
    const message = body?.message ?? {};

    // SDK fields are snake_case (from/subject/message_id/thread_id/inbox_id).
    // The raw `from_` alias is also accepted for hand-crafted test payloads.
    const rawFrom = message.from ?? message.from_;
    const senderEmail = parseEmailMailbox(rawFrom);
    if (!senderEmail) {
      console.warn("[agentmail-bot] message.received with no parseable sender", {
        from: truncate(rawFrom, 120),
      });
      return;
    }

    const messageId = message.message_id ?? message.id;
    const threadId = message.thread_id;
    const inboxId = message.inbox_id ?? body?.inbox_id;
    const displayName = parseDisplayName(rawFrom);

    // text/html are omitted by AgentMail when the payload exceeds 1 MB.
    const text = extractBody(message);

    console.info("[agentmail-bot] message.received", {
      from: senderEmail,
      subject: truncate(message.subject, 120),
      messageId,
      threadId,
      inboxId,
      preview: text ? truncate(text) : "[body omitted, >1MB]",
    });

    // Capture context so the reply threads. Keyed lower-cased to match the
    // adapter target (which is the lower-cased sender email).
    if (messageId && inboxId) {
      this.replyContexts.set(senderEmail, {
        inboxId,
        messageId,
        ...(threadId ? { threadId } : {}),
        ...(message.subject ? { subject: String(message.subject) } : {}),
      });
    } else {
      console.warn(
        "[agentmail-bot] message.received missing inbox_id/message_id — reply cannot thread",
        { inboxId, messageId },
      );
    }

    if (!text) {
      // Nothing to forward (body omitted / empty). The log above is the record.
      return;
    }

    // SENDER AUTHENTICITY: AgentMail DROPS mail whose auth (SPF/DKIM/DMARC)
    // explicitly fails, and labels soft/missing-auth mail `unauthenticated` (it
    // exposes no raw SPF/DKIM fields — only that label). So a spoofable `from`
    // arrives either dropped upstream or carrying `unauthenticated`. We forward
    // `channelVerified = !unauthenticated` so the backend's owner routing denies
    // owner-trust to an unauthenticated sender (routing it to the envoy as a
    // guest) even when the `from` is on the owner allowlist — closing the
    // email-spoofing hole. The owner allowlist itself is still enforced
    // downstream (the adapter doesn't hold it); this only supplies the auth bit.
    const labels: string[] = Array.isArray(message.labels)
      ? message.labels.map((l: unknown) => String(l).toLowerCase())
      : [];
    const channelVerified = !labels.includes("unauthenticated");

    await this.handlers?.onTextMessage({
      target: senderEmail,
      channelAddress: senderEmail,
      text,
      channelVerified,
      ...(displayName ? { displayName } : {}),
    });
  }

  /**
   * Verify the svix signature on a raw request body. AgentMail delivers
   * webhooks via svix with `svix-id` / `svix-timestamp` / `svix-signature`
   * headers signed by the per-webhook secret (whsec_...). Returns true when
   * the request is allowed to proceed.
   */
  private verifySignature(req: IncomingMessage, rawBody: Buffer): boolean {
    if (!this.webhookSecret) {
      return false;
    }
    try {
      const wh = new Webhook(this.webhookSecret);
      const headers = {
        "svix-id": String(req.headers["svix-id"] ?? ""),
        "svix-timestamp": String(req.headers["svix-timestamp"] ?? ""),
        "svix-signature": String(req.headers["svix-signature"] ?? ""),
      };
      wh.verify(rawBody.toString("utf8"), headers);
      return true;
    } catch (error) {
      if (error instanceof WebhookVerificationError) {
        console.warn("[agentmail-bot] rejected: invalid webhook signature");
      } else {
        console.error("[agentmail-bot] signature verification error", error);
      }
      return false;
    }
  }

  /* ---------- self-register ---------- */

  private async selfRegisterWebhook(): Promise<void> {
    if (!this.webhookUrl) {
      console.info(
        "[agentmail-bot] AGENTMAIL_WEBHOOK_URL not set — skipping self-register. " +
          `Register a webhook manually pointing at <public-url>${WEBHOOK_PATH} ` +
          `for event "message.received" (e.g. via an ngrok / magictunnel URL).`,
      );
      return;
    }

    const url = `${this.webhookUrl.replace(/\/+$/, "")}${WEBHOOK_PATH}`;
    try {
      // agentmail@0.0.46 CreateWebhookRequest is snake_case:
      //   { url, event_types, inbox_ids?, client_id? }.
      // `client_id` makes the registration idempotent (re-running the bot
      // updates the same webhook instead of creating duplicates). The API
      // key is inbox-scoped to magican@agentmail.to, so no inbox_ids filter
      // is needed — the webhook only fires for that inbox.
      const webhook = await this.client.webhooks.create({
        url,
        event_types: ["message.received"],
        client_id: SELF_REGISTER_CLIENT_ID,
      });
      // The provider returns the secret for this registration. Keep explicit
      // operator configuration when present; never print the returned secret.
      this.webhookSecret ??= webhook.secret?.trim() || undefined;
      console.info("[agentmail-bot] webhook registered", {
        url,
        webhookId: webhook.webhook_id,
        eventTypes: webhook.event_types,
      });
    } catch (error) {
      console.error("[agentmail-bot] webhook self-register failed", {
        url,
        error: error instanceof Error ? error.message : error,
      });
    }
  }
}

/* ---------- helpers ---------- */

function truncate(value: unknown, limit = PREVIEW_LIMIT): string {
  if (value == null) return "";
  const text = String(value).replace(/\s+/g, " ").trim();
  return text.length > limit ? `${text.slice(0, limit)}…` : text;
}

/**
 * Pull a human display name out of a `from` header of the form
 * "Display Name <user@host>". Returns undefined for a bare address.
 */
function parseDisplayName(rawFrom: unknown): string | undefined {
  if (rawFrom == null) return undefined;
  const raw = String(rawFrom).trim();
  const angle = raw.indexOf("<");
  if (angle <= 0) return undefined;
  const name = raw.slice(0, angle).trim().replace(/^"|"$/g, "").trim();
  return name ? name : undefined;
}

/**
 * Best body to forward: prefer plain text, fall back to a naive HTML→text
 * strip, then to the preview. Returns undefined when nothing usable exists
 * (e.g. AgentMail omitted the body for a >1MB payload).
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
function extractBody(message: any): string | undefined {
  const text = message?.text;
  if (typeof text === "string" && text.trim()) return text.trim();
  const html = message?.html;
  if (typeof html === "string" && html.trim()) {
    return htmlToText(html);
  }
  const preview = message?.preview;
  if (typeof preview === "string" && preview.trim()) return preview.trim();
  return undefined;
}

function htmlToText(html: string): string {
  return html
    .replace(/<style[\s\S]*?<\/style>/gi, " ")
    .replace(/<script[\s\S]*?<\/script>/gi, " ")
    .replace(/<br\s*\/?>(?=)/gi, "\n")
    .replace(/<\/(p|div|tr|h[1-6]|li)>/gi, "\n")
    .replace(/<[^>]+>/g, " ")
    .replace(/&nbsp;/gi, " ")
    .replace(/&amp;/gi, "&")
    .replace(/&lt;/gi, "<")
    .replace(/&gt;/gi, ">")
    .replace(/&quot;/gi, '"')
    .replace(/&#39;/gi, "'")
    .replace(/[ \t]+/g, " ")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}
