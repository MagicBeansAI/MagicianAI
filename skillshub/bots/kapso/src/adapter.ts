import { chunkExactText } from "@magician/bot-sdk";
import type { Server } from "node:http";

import type {
  ChannelAdapter,
  ChannelAdapterHandlers,
  ChannelSendTextOptions,
  CriticalRequestAlertCard,
  TaskStatusUpdateRender,
  ToolCallExecutedRender,
} from "@magician/bot-sdk";
import { WhatsAppClient } from "@kapso/whatsapp-cloud-api";
import express from "express";

import { verifyKapsoWebhookSignature } from "./webhook-signature.js";

const WHATSAPP_TEXT_LIMIT = 4_000;
const PROVIDER_REQUEST_TIMEOUT_MS = 30_000;
// WhatsApp caps a template body (static text + substituted variables) at ~1024
// characters; keep a margin for any fixed text wrapping the {{1}} variable.
const TEMPLATE_PARAM_LIMIT = 1_000;
// Meta error code for "re-engagement message" — sending outside the 24-hour
// customer-service window. The SDK also classifies it as this category.
const REENGAGEMENT_ERROR_CODE = 131047;

export interface KapsoAdapterOptions {
  apiKey: string;
  phoneNumberId: string;
  webhookPort: number;
  /** Kapso dashboard webhook secret used to authenticate every POST body. */
  webhookSecret: string;
  /**
   * Pre-approved WhatsApp message template used to reply when the 24-hour
   * customer-service window has closed. Outside that window Meta rejects
   * free-form text ("Cannot send non-template messages outside the 24-hour
   * window"), so the reply is re-sent through this template instead. The
   * template must have a single `{{1}}` body variable — the reply text is
   * passed there. When unset, an outside-window reply is logged and dropped
   * (the bot keeps running rather than crashing).
   */
  fallbackTemplateName?: string;
  /** BCP-47 language code of the fallback template (e.g. "en_US", "en"). Default "en_US". */
  fallbackTemplateLang?: string;
}

export class KapsoAdapter implements ChannelAdapter<string> {
  private handlers?: ChannelAdapterHandlers<string>;
  private app: express.Express | undefined;
  private server: Server | undefined;
  private keepaliveTimer: ReturnType<typeof setInterval> | undefined;
  private readonly client: WhatsAppClient;
  private readonly phoneNumberId: string;
  private readonly webhookPort: number;
  private readonly webhookSecret: string;
  private readonly fallbackTemplateName: string | undefined;
  private readonly fallbackTemplateLang: string;

  constructor(options: KapsoAdapterOptions) {
    this.phoneNumberId = options.phoneNumberId;
    this.webhookPort = options.webhookPort;
    this.webhookSecret = options.webhookSecret.trim();
    if (!this.webhookSecret) {
      throw new Error("Kapso webhook secret must not be empty");
    }
    this.fallbackTemplateName = options.fallbackTemplateName?.trim() || undefined;
    this.fallbackTemplateLang = options.fallbackTemplateLang?.trim() || "en_US";

    this.client = new WhatsAppClient({
      baseUrl: "https://api.kapso.ai/meta/whatsapp",
      kapsoApiKey: options.apiKey,
      // The provider SDK has no deadline. Keep the abort signal active through
      // response-body consumption as well as the initial fetch. A timeout is
      // an unknown send outcome, never permission to repeat the POST.
      fetch: (url, init) => {
        const deadline = AbortSignal.timeout(PROVIDER_REQUEST_TIMEOUT_MS);
        return globalThis.fetch(url, {
          ...init,
          signal: init?.signal ? AbortSignal.any([init.signal, deadline]) : deadline,
        });
      },
    });
  }

  async start(handlers: ChannelAdapterHandlers<string>): Promise<void> {
    this.handlers = handlers;

    this.app = express();
    this.app.post(
      "/webhook",
      // Authentication must cover the bytes Kapso actually sent. Keep the
      // payload as a Buffer and reject compressed bodies rather than verifying
      // a transparently inflated representation.
      express.raw({ type: "application/json", inflate: false }),
      (req, res) => {
        const rawBody = Buffer.isBuffer(req.body) ? req.body : undefined;
        const signature = req.get("X-Webhook-Signature");
        if (
          !rawBody ||
          !verifyKapsoWebhookSignature(rawBody, signature, this.webhookSecret)
        ) {
          // This is an anonymous public endpoint. Do not turn invalid requests
          // into an unbounded log-amplification primitive.
          res.sendStatus(401);
          return;
        }

        let body: unknown;
        try {
          body = JSON.parse(rawBody.toString("utf8"));
        } catch {
          res.sendStatus(400);
          return;
        }

        res.sendStatus(200);
        void this.handleWebhook(body);
      },
    );

    this.app.get("/webhook", (_req, res) => {
      res.send("Kapso webhook endpoint is running.");
    });

    await new Promise<void>((resolve) => {
      this.server = this.app!.listen(this.webhookPort, () => {
        // Kapso's webhook-only runtime can otherwise fall off the active-handle
        // set after startup, which makes Node exit 0 even though the webhook
        // server should remain resident.
        this.server?.ref?.();
        this.keepaliveTimer ??= setInterval(() => {}, 60 * 60 * 1000);
        console.info(
          `[kapso-bot] webhook server listening on port ${this.webhookPort}`,
        );
        resolve();
      });
    });
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
      console.info("[kapso-bot] webhook server stopped");
    }
  }

  resolveRealtimeTarget(channelAddress: string): string {
    return channelAddress;
  }

  async sendText(
    target: string,
    text: string,
    options?: ChannelSendTextOptions,
  ): Promise<{ accepted: boolean }> {
    // A template changes the exact body already recorded for a tracked reply.
    // It needs its own prepared act, so this dispatch cannot substitute one.
    const allowOutsideWindowTemplate = !options?.preserveExactText &&
      (options?.allowOutsideWindowTemplate ?? true);
    const chunks = options?.preserveExactText ? chunkExactText(text, WHATSAPP_TEXT_LIMIT) : chunkWhatsAppText(text);
    for (const chunk of chunks) {
      try {
        const receipt = await this.client.messages.sendText({
          phoneNumberId: this.phoneNumberId,
          to: target,
          body: chunk,
        });
        // The SDK accepts arbitrary successful HTTP bodies. Every tracked
        // chunk needs an actual provider message id before the complete reply
        // can be acknowledged as accepted in Claims Review.
        if (options?.preserveExactText && (!Array.isArray(receipt?.messages)
          || typeof receipt.messages[0]?.id !== "string" || !receipt.messages[0].id.trim())) {
          return { accepted: false };
        }
      } catch (error) {
        if (isReengagementWindowError(error)) {
          if (!allowOutsideWindowTemplate) {
            console.info(
              "[kapso-bot] ordinary chat reply outside the 24-hour window - template fallback suppressed",
              { target },
            );
            return { accepted: false };
          }
          // The 24-hour customer-service window has closed, so Meta rejects
          // free-form text. Re-send the whole reply once through a pre-approved
          // template (the only message type allowed now) and stop — the
          // remaining chunks would hit the same wall, and each template send
          // opens a fresh billable conversation.
          await this.sendOutsideWindowFallback(target, text);
          return { accepted: false };
        }
        // A single send failure must never crash the webhook bot: an unhandled
        // rejection would exit the process and the supervisor would restart it
        // on every message. Log and drop this reply.
        console.error("[kapso-bot] sendText failed", { target, error });
        return { accepted: false };
      }
    }
    return { accepted: true };
  }

  /**
   * Reply when the 24-hour window has closed, by re-sending the text through the
   * configured fallback template. Never throws — any failure is logged so the
   * webhook bot stays resident.
   */
  private async sendOutsideWindowFallback(
    target: string,
    text: string,
  ): Promise<void> {
    const templateName = this.fallbackTemplateName;
    if (!templateName) {
      console.warn(
        "[kapso-bot] reply outside the 24-hour window and no fallback template configured (set KAPSO_FALLBACK_TEMPLATE_NAME to a pre-approved template) — reply dropped",
        { target },
      );
      return;
    }

    const language = { code: this.fallbackTemplateLang };

    // The template may carry a single {{1}} body variable (filled with the
    // reply text) or be a static acknowledgment (no variables). We can't tell
    // which from the name, so try passing the reply text first; if Meta rejects
    // it as a parameter mismatch, resend the template with no parameters.
    try {
      await this.client.messages.sendTemplate({
        phoneNumberId: this.phoneNumberId,
        to: target,
        template: {
          name: templateName,
          language,
          components: [
            {
              type: "body",
              parameters: [{ type: "text", text: sanitizeTemplateParam(text) }],
            },
          ],
        },
      });
      console.info("[kapso-bot] reply sent via fallback template (outside 24h window)", {
        target,
        template: templateName,
      });
      return;
    } catch (error) {
      if (!isTemplateParamMismatchError(error)) {
        console.error("[kapso-bot] fallback template send failed", {
          target,
          template: templateName,
          error,
        });
        return;
      }
      console.info(
        "[kapso-bot] fallback template takes no body variable — retrying as a static template",
        { target, template: templateName },
      );
    }

    try {
      await this.client.messages.sendTemplate({
        phoneNumberId: this.phoneNumberId,
        to: target,
        template: { name: templateName, language },
      });
      console.info("[kapso-bot] static fallback template sent (outside 24h window)", {
        target,
        template: templateName,
      });
    } catch (error) {
      console.error("[kapso-bot] static fallback template send failed", {
        target,
        template: templateName,
        error,
      });
    }
  }

  /**
   * A critical-request alert (secure HITL plan §6.1). Unlike `sendText`,
   * which swallows failures to keep the webhook bot resident, this throws:
   * the runtime reports the provider's real answer and the backend records
   * it. Inside the 24-hour window the card goes as an interactive CTA
   * (the secure link is a button); outside it Meta accepts only a
   * pre-approved template, so the text goes through the configured
   * fallback template with the alert as its body variable — and without a
   * template the send is refused honestly rather than dropped.
   */
  async sendCriticalAlert(
    target: string,
    alert: CriticalRequestAlertCard,
  ): Promise<{ providerMessageId?: string }> {
    try {
      const response = alert.open_url
        ? await this.client.messages.sendInteractiveCtaUrl({
            phoneNumberId: this.phoneNumberId,
            to: target,
            bodyText: alert.text,
            parameters: { displayText: "Open secure request", url: alert.open_url },
          })
        : await this.client.messages.sendText({
            phoneNumberId: this.phoneNumberId,
            to: target,
            body: alert.text,
          });
      return providerMessageId(response);
    } catch (error) {
      if (!isReengagementWindowError(error)) {
        throw error;
      }
    }
    const templateName = this.fallbackTemplateName;
    if (!templateName) {
      throw new Error(
        "outside the 24-hour window and no fallback template is configured (KAPSO_FALLBACK_TEMPLATE_NAME)",
      );
    }
    const response = await this.client.messages.sendTemplate({
      phoneNumberId: this.phoneNumberId,
      to: target,
      template: {
        name: templateName,
        language: { code: this.fallbackTemplateLang },
        components: [
          {
            type: "body",
            parameters: [{ type: "text", text: sanitizeTemplateParam(alert.text) }],
          },
        ],
      },
    });
    return providerMessageId(response);
  }

  /**
   * WhatsApp messages cannot be edited: a card that was sent recently gets
   * a one-line follow-up so the owner does not act on a stale alert. A
   * failure here is logged, never thrown — the link already resolves to
   * the completed state.
   */
  async retireCriticalAlert(
    target: string,
    sent: { providerMessageId?: string; outcome: string },
  ): Promise<void> {
    const note = sent.outcome === "responded"
      ? "That request has been answered — nothing more to do."
      : `That request is no longer open (${sent.outcome}).`;
    try {
      await this.client.messages.sendText({
        phoneNumberId: this.phoneNumberId,
        to: target,
        body: note,
        ...(sent.providerMessageId ? { contextMessageId: sent.providerMessageId } : {}),
      });
    } catch (error) {
      console.info("[kapso-bot] could not retire a critical alert", { target, error });
    }
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
  private async handleWebhook(body: any): Promise<void> {
    try {
      const message = body?.message;
      if (!message) {
        console.info("[kapso-bot] webhook received with no message field", JSON.stringify(body).slice(0, 200));
        return;
      }

      console.info("[kapso-bot] webhook received", {
        type: message.type,
        direction: message.kapso?.direction,
        phone: body.conversation?.phone_number,
        content: message.kapso?.content?.slice?.(0, 100) ?? message.text?.body?.slice?.(0, 100),
      });

      // Only handle inbound messages
      if (message.kapso?.direction !== "inbound") return;

      const senderPhone = body.conversation?.phone_number;
      if (!senderPhone) return;

      const displayName = firstNonEmptyString([
        body.conversation?.kapso?.contact_name,
        body.conversation?.contact_name,
        body.conversation?.profile_name,
        body.conversation?.name,
        body.contact?.name,
        body.contact?.profile_name,
        body.profile?.name,
        message.from?.name,
      ]);

      // Handle text messages
      if (message.type === "text") {
        const text = message.text?.body?.trim();
        if (!text) return;
        await this.handleTextMessage(senderPhone, text, displayName);
        return;
      }

      // Handle media messages — forward the text description
      if (message.kapso?.content) {
        const content = String(message.kapso.content).trim();
        if (content) {
          await this.handleTextMessage(senderPhone, content, displayName);
          return;
        }
      }

      // Handle audio with transcription
      if (message.type === "audio" && message.kapso?.transcript?.text) {
        const transcript = String(message.kapso.transcript.text).trim();
        if (transcript) {
          await this.handleTextMessage(senderPhone, transcript, displayName);
        }
      }
    } catch (error) {
      console.error("[kapso-bot] webhook handler error", error);
    }
  }

  private async handleTextMessage(
    senderPhone: string,
    text: string,
    displayName: string | undefined,
  ): Promise<void> {
    await this.handlers?.onTextMessage({
      target: senderPhone,
      channelAddress: senderPhone,
      text,
      ...(displayName ? { displayName } : {}),
    });
  }

}

/* ---------- helpers ---------- */

/**
 * True when a send failed because the 24-hour customer-service window has
 * closed (Meta then only accepts pre-approved templates). Duck-typed across the
 * SDK's classification (`category`), Meta's numeric code, and the message text
 * so it survives error wrapping and SDK version drift.
 */
function isReengagementWindowError(error: unknown): boolean {
  if (!error || typeof error !== "object") return false;
  const e = error as { category?: unknown; code?: unknown; message?: unknown };
  if (e.category === "reengagementWindow") return true;
  if (e.code === REENGAGEMENT_ERROR_CODE) return true;
  const message = typeof e.message === "string" ? e.message.toLowerCase() : "";
  return message.includes("24-hour window") || message.includes("re-engagement");
}

function firstNonEmptyString(values: unknown[]): string | undefined {
  for (const value of values) {
    if (typeof value !== "string") continue;
    const trimmed = value.trim();
    if (trimmed) return trimmed;
  }
  return undefined;
}

/**
 * True when a template send failed because the parameters we passed don't match
 * the template's defined variables — typically sending a body parameter to a
 * static template that has none (Meta code 132000) or a format mismatch (132012).
 * Used to retry the send with no parameters.
 */
function providerMessageId(response: { messages?: Array<{ id?: string }> }): { providerMessageId?: string } {
  const id = response.messages?.[0]?.id;
  return id ? { providerMessageId: id } : {};
}

function isTemplateParamMismatchError(error: unknown): boolean {
  if (!error || typeof error !== "object") return false;
  const e = error as { code?: unknown; message?: unknown };
  if (e.code === 132000 || e.code === 132012) return true;
  const message = typeof e.message === "string" ? e.message.toLowerCase() : "";
  return (
    message.includes("number of parameters") ||
    message.includes("parameters does not match") ||
    message.includes("parameter format mismatch")
  );
}

/**
 * WhatsApp template body parameters reject newlines, tabs, and runs of more than
 * four spaces, and have a hard length cap. Collapse all whitespace to single
 * spaces and truncate so the re-engagement template is accepted. Free-form
 * formatting is lost — this is the outside-24h-window fallback path only.
 */
function sanitizeTemplateParam(text: string): string {
  const collapsed = text.replace(/\s+/g, " ").trim();
  if (collapsed.length <= TEMPLATE_PARAM_LIMIT) return collapsed;
  return collapsed.slice(0, TEMPLATE_PARAM_LIMIT - 1).trimEnd() + "…";
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
