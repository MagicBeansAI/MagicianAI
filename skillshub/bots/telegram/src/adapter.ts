import { chunkExactText } from "@magician/bot-sdk";
import type {
  ChannelAdapter,
  ChannelAdapterHandlers,
  ChannelSendTextOptions,
  ChannelFileAttachment,
  CriticalRequestAlertCard,
  TaskStatusUpdateRender,
  ToolCallExecutedRender,
} from "@magician/bot-sdk";
import { Markup, Telegraf } from "telegraf";
import type { Context } from "telegraf";

import { chunkTelegramText, isRejectedButtonUrl } from "./callback-data.js";

const DEFAULT_PRIVATE_ONLY_MESSAGE = "Please message me in a private chat.";
const DEFAULT_UNKNOWN_COMMAND_MESSAGE =
  "Send a plain message to chat with your assistant.";
const TELEGRAM_ALLOWED_UPDATES = ["message"] as const;
const DEFAULT_SDK_COMMAND_NAMES = [
  "start",
  "new",
  "stop",
  "queue",
  "clearqueue",
  "queue-clear",
] as const;

export type TelegramTarget = number;
type TelegramRequestOptions = NonNullable<Parameters<Telegraf["telegram"]["callApi"]>[2]>;

export interface TelegramAdapterOptions {
  dropPendingUpdates?: boolean;
  privateOnlyMessage?: string;
  unknownCommandMessage?: string;
  allowedCommandNames?: readonly string[];
}

export class TelegramAdapter implements ChannelAdapter<TelegramTarget> {
  private handlers?: ChannelAdapterHandlers<TelegramTarget>;
  private readonly privateOnlyMessage: string;
  private readonly unknownCommandMessage: string;
  private readonly dropPendingUpdates: boolean;
  private readonly allowedCommandNames: Set<string>;

  constructor(
    private readonly bot: Telegraf,
    options: TelegramAdapterOptions = {},
  ) {
    this.privateOnlyMessage =
      options.privateOnlyMessage ?? DEFAULT_PRIVATE_ONLY_MESSAGE;
    this.unknownCommandMessage =
      options.unknownCommandMessage ?? DEFAULT_UNKNOWN_COMMAND_MESSAGE;
    this.dropPendingUpdates = options.dropPendingUpdates ?? false;
    this.allowedCommandNames = new Set([
      ...DEFAULT_SDK_COMMAND_NAMES,
      ...(options.allowedCommandNames ?? []),
    ].map(normalizeCommandName).filter(Boolean));
  }

  async start(
    handlers: ChannelAdapterHandlers<TelegramTarget>,
  ): Promise<void> {
    this.handlers = handlers;

    this.bot.catch((error) => {
      console.error("[telegram-bot] handler error", error);
    });

    this.bot.start(async (ctx) => {
      if (!isPrivateChat(ctx)) {
        await ctx.reply(this.privateOnlyMessage);
        return;
      }

      const displayName = formatDisplayName(ctx);
      await this.handlers?.onConnectRequest?.({
        target: ctx.chat.id,
        channelAddress: String(ctx.chat.id),
        ...(displayName ? { displayName } : {}),
      });
    });

    this.bot.on("text", async (ctx) => {
      if (!isPrivateChat(ctx)) {
        return;
      }

      const text = ctx.message.text.trim();
      if (!text) {
        return;
      }

      // SDK-handled commands pass through as text messages so
      // channel-runtime handles them consistently across all channels.
      // Unknown /commands still get rejected here.
      const commandName = parseTelegramCommandName(text);
      if (commandName && !this.allowedCommandNames.has(commandName)) {
        await ctx.reply(this.unknownCommandMessage);
        return;
      }

      const displayName = formatDisplayName(ctx);
      await this.handlers?.onTextMessage({
        target: ctx.chat.id,
        channelAddress: String(ctx.chat.id),
        text,
        ...(displayName ? { displayName } : {}),
      });
    });

    await this.bot.launch({
      dropPendingUpdates: this.dropPendingUpdates,
      allowedUpdates: [...TELEGRAM_ALLOWED_UPDATES],
    });
  }

  async stop(): Promise<void> {
    this.bot.stop("shutdown");
  }

  resolveRealtimeTarget(channelAddress: string): TelegramTarget | null {
    const parsed = Number.parseInt(channelAddress, 10);
    return Number.isFinite(parsed) ? parsed : null;
  }

  async sendText(target: TelegramTarget, text: string, options?: ChannelSendTextOptions): Promise<{ accepted: boolean }> {
    const chunks = options?.preserveExactText ? chunkExactText(text, 4_000) : chunkTelegramText(text);
    for (const chunk of chunks) {
      if (options?.preserveExactText) {
        // Telegraf's convenience method exposes no abort option and accepts
        // any `ok: true` result. Bound headers and body reading, and require a
        // real message in the intended chat before acknowledging this chunk.
        // Its types still name abort-controller's shim; the node-fetch
        // transport accepts Node's native AbortSignal (covered over HTTP).
        const receipt = await this.bot.telegram.callApi("sendMessage", {
          chat_id: target, text: chunk,
        }, { signal: AbortSignal.timeout(30_000) } as unknown as TelegramRequestOptions);
        if (!Number.isSafeInteger(receipt?.message_id) || receipt.message_id <= 0
          || receipt.chat?.id !== target) return { accepted: false };
      } else {
        await this.bot.telegram.sendMessage(target, chunk);
      }
    }
    return { accepted: true };
  }

  /**
   * A critical-request alert (secure HITL plan §6.1): the value-free card
   * with the secure link as an inline URL button. Throws on failure so the
   * runtime reports the provider's real answer.
   */
  async sendCriticalAlert(
    target: TelegramTarget,
    alert: CriticalRequestAlertCard,
  ): Promise<{ providerMessageId?: string }> {
    const extra = alert.open_url
      ? Markup.inlineKeyboard([Markup.button.url("Open secure request", alert.open_url)])
      : undefined;
    try {
      const sent = await this.bot.telegram.sendMessage(target, alert.text, extra);
      return { providerMessageId: String(sent.message_id) };
    } catch (error) {
      // Telegram validates an inline keyboard button's URL and refuses a
      // non-public one ("Bad Request: ... is invalid: Wrong HTTP URL"), which
      // failed the ENTIRE delivery — the owner was told nothing at all because
      // a decoration could not be drawn. The card's own text already carries
      // the same link, so send it without the button rather than lose the
      // alert. Any other failure still throws, so the runtime reports the
      // provider's real answer.
      if (!extra || !isRejectedButtonUrl(error)) throw error;
      console.warn(
        "[telegram-bot] Telegram refused the secure-link button URL; sending the alert without it",
        { reason: error instanceof Error ? error.message : String(error) },
      );
      const sent = await this.bot.telegram.sendMessage(target, alert.text);
      return { providerMessageId: String(sent.message_id) };
    }
  }

  /** The request resolved: edit the card so a stale alert is not acted on. */
  async retireCriticalAlert(
    target: TelegramTarget,
    sent: { providerMessageId?: string; outcome: string },
  ): Promise<void> {
    const note = sent.outcome === "responded"
      ? "Magician: that request has been answered — nothing more to do."
      : `Magician: that request is no longer open (${sent.outcome}).`;
    const messageId = Number.parseInt(sent.providerMessageId ?? "", 10);
    if (Number.isFinite(messageId)) {
      await this.bot.telegram.editMessageText(target, messageId, undefined, note);
      return;
    }
    await this.sendText(target, note);
  }

  async sendToolCallExecuted(
    target: TelegramTarget,
    executed: ToolCallExecutedRender,
  ): Promise<void> {
    await this.sendText(target, executed.text);
  }

  async sendTaskStatusUpdate(
    target: TelegramTarget,
    update: TaskStatusUpdateRender,
  ): Promise<void> {
    await this.sendText(target, update.text);
  }

  async sendFile(
    target: TelegramTarget,
    attachment: ChannelFileAttachment,
  ): Promise<void> {
    const response = await fetch(attachment.url, { headers: attachment.download_headers });
    if (!response.ok) {
      throw new Error(
        `download failed: ${response.status} ${response.statusText}`,
      );
    }
    const buffer = Buffer.from(await response.arrayBuffer());
    const filename = attachment.display_name || "file";
    const source = { source: buffer, filename };
    const extra = attachment.caption ? { caption: attachment.caption } : undefined;

    switch (attachment.kind) {
      case "image":
        await this.bot.telegram.sendPhoto(target, source, extra);
        return;
      case "video":
        await this.bot.telegram.sendVideo(target, source, extra);
        return;
      case "audio":
        await this.bot.telegram.sendAudio(target, source, extra);
        return;
      case "pdf":
      case "other":
      default:
        await this.bot.telegram.sendDocument(target, source, extra);
        return;
    }
  }
}

function isPrivateChat(ctx: Context): boolean {
  return ctx.chat?.type === "private";
}

function normalizeCommandName(name: string): string {
  const trimmed = name.trim().toLowerCase();
  return trimmed.startsWith("/") ? trimmed.slice(1) : trimmed;
}

function parseTelegramCommandName(text: string): string | null {
  const trimmed = text.trim();
  if (!trimmed.startsWith("/")) {
    return null;
  }

  const command = trimmed.split(/\s+/, 1)[0]?.slice(1) ?? "";
  const mention = command.indexOf("@");
  return normalizeCommandName(mention === -1 ? command : command.slice(0, mention));
}

function formatDisplayName(ctx: Context): string | undefined {
  const firstName = ctx.from?.first_name?.trim();
  const lastName = ctx.from?.last_name?.trim();
  const fullName = [firstName, lastName].filter(Boolean).join(" ").trim();

  if (fullName) {
    return fullName;
  }

  const username = ctx.from?.username?.trim();
  if (username) {
    return `@${username}`;
  }

  return undefined;
}
