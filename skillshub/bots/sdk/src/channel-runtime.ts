import { createHash, randomUUID } from "node:crypto";
import {
  buildTaskStatusUpdateRender,
  buildToolCallExecutedRender,
} from "./formatters.js";
import type {
  ChannelFileAttachment,
  EnvoyDeliveryBinding,
  ChannelIdentity,
  ChannelMessageDispatchOptions,
  ChannelRuntimeOptions,
  ChannelSendTextOptions,
  ChannelThreadingMode,
  ChatMessage,
  ChatMessageReceivedEvent,
  ChatResponse,
  CriticalRequestAlertEvent,
  CriticalRequestRetiredEvent,
  EnrollResult,
  EscalationContent,
  MagicianRealtimeEvent,
  QueuedMessageReceipt,
} from "./types.js";

const DEFAULT_PENDING_APPROVAL_MESSAGE =
  "Pending approval. An admin will connect you shortly.";
const DEFAULT_CONNECTED_MESSAGE = (identity: ChannelIdentity): string =>
  `Connected. You're chatting as ${identity.principal}.`;
const MAX_DELIVERED_MESSAGE_IDS = 2_048;
const ENVOY_RECEIPT_RETRY_MS = 30_000;
type EnvoyReceipt = {
  sessionId: string;
  messageId: string;
  binding: EnvoyDeliveryBinding;
  // A reserved slot cannot be reported while the adapter is still running.
  phase: "provider_accepted" | "unknown" | null;
};
const DEFAULT_NON_CONTROL_MESSAGE_BEHAVIOR = "dispatch";
const DEFAULT_CHANNEL_THREADING: ChannelThreadingMode = "default";
const STRUCTURED_RESPONSE_SCHEMA = "magician.structured_response";
const STRUCTURED_RESPONSE_VERSION = 1;
const STRUCTURED_RESPONSE_MAX_BYTES = 32_768;

/** Hard cap on automatic queue drains per `handleTextMessage` invocation.
 *  Matches the backend `MAX_QUEUED_PER_SESSION` so a maxed-out queue can
 *  fully drain in a single drain loop. Defensive bound against runaway
 *  drains if the backend cap ever raises. */
const DRAIN_MAX_ITERATIONS = 5;

function disallowedControlCharactersInText(text: string): boolean {
  for (const ch of text) {
    const code = ch.codePointAt(0);
    if (code === undefined) continue;
    if (code <= 0x08) return true;
    if (code === 0x0b || code === 0x0c) return true;
    if (code >= 0x0e && code <= 0x1f) return true;
    if (code >= 0x7f && code <= 0x9f) return true;
  }
  return false;
}

function normalizedStructuredText(text: string): string {
  return text.trim().replace(/\s+/g, " ");
}

function canonicalTextForPresentation(message: ChatMessage): string | null {
  switch (message.content.type) {
    case "text":
      return typeof message.content.text === "string" ? message.content.text : null;
    case "tool_call_executed":
      return typeof message.content.summary === "string" ? message.content.summary : null;
    case "rich_tool_result": {
      if (typeof message.content.summary === "string" && message.content.summary.trim()) {
        return message.content.summary;
      }
      const textBlock = message.content.content_blocks?.find((block) => {
        if (typeof block !== "object" || block === null || Array.isArray(block)) return false;
        const candidate = block as { type?: unknown; text?: unknown };
        return candidate.type === "text" && typeof candidate.text === "string" && candidate.text.trim() !== "";
      }) as { text?: string } | undefined;
      return textBlock?.text ?? null;
    }
    case "attachment":
      return typeof message.content.filename === "string" ? message.content.filename : null;
    case "task_status_update":
      return typeof message.content.summary === "string"
        ? message.content.summary
        : typeof message.content.status === "string"
          ? message.content.status
          : null;
    case "escalation":
      return typeof message.content.question === "string"
        ? message.content.question
        : typeof message.content.summary === "string"
          ? message.content.summary
          : null;
    case "escalation_resolved":
      return typeof message.content.summary === "string" ? message.content.summary : null;
    default:
      return null;
  }
}

function plainTextFromPresentation(message: ChatMessage): string | null {
  const presentation = (message as { presentation?: unknown }).presentation;
  if (typeof presentation !== "object" || presentation === null) {
    return null;
  }

  const candidate = presentation as {
    schema?: unknown;
    version?: unknown;
    plain_text?: unknown;
  };
  if (candidate.schema !== STRUCTURED_RESPONSE_SCHEMA) return null;
  if (candidate.version !== STRUCTURED_RESPONSE_VERSION) return null;
  if (typeof candidate.plain_text !== "string") return null;

  const text = candidate.plain_text;
  if (text.trim() === "") return null;
  if (new TextEncoder().encode(text).length > STRUCTURED_RESPONSE_MAX_BYTES) return null;
  if (disallowedControlCharactersInText(text)) return null;

  const canonical = canonicalTextForPresentation(message);
  if (canonical === null || normalizedStructuredText(canonical) !== normalizedStructuredText(text)) {
    return null;
  }
  return text;
}

/** Parsed slash command. `name` is lowercase, no leading slash. `args` is
 *  the trimmed remainder of the line after the command word. Returns
 *  `null` for non-slash messages. Only matches when the entire trimmed
 *  message starts with `/`; "/stop trying" is treated as a slash command
 *  with args "trying", "Please /stop" is not (the slash is mid-message). */
function parseSlashCommand(text: string): { name: string; args: string } | null {
  const trimmed = text.trim();
  if (!trimmed.startsWith("/")) return null;
  const space = trimmed.indexOf(" ");
  const parseName = (raw: string): string => {
    const mention = raw.indexOf("@");
    return (mention === -1 ? raw : raw.slice(0, mention)).toLowerCase();
  };
  if (space === -1) {
    return { name: parseName(trimmed.slice(1)), args: "" };
  }
  return {
    name: parseName(trimmed.slice(1, space)),
    args: trimmed.slice(space + 1).trim(),
  };
}

function normalizeControlPrefixes(
  prefix: string | undefined,
  aliases: readonly string[] | undefined,
): string[] {
  const out: string[] = [];
  for (const candidate of [prefix, ...(aliases ?? [])]) {
    const trimmed = candidate?.trim();
    if (!trimmed || out.includes(trimmed)) {
      continue;
    }
    out.push(trimmed);
  }
  return out;
}

function parseControlInvocation(
  text: string,
  prefix: string,
): { invoked: true; text: string } | { invoked: false } {
  const trimmedStart = text.trimStart();
  const lower = trimmedStart.toLowerCase();
  const normalizedPrefix = prefix.toLowerCase();
  if (!lower.startsWith(normalizedPrefix)) {
    return { invoked: false };
  }

  let rest = trimmedStart.slice(prefix.length);
  if (prefix.startsWith("/") && rest.startsWith("@")) {
    const mention = rest.match(/^@[^\s:,]+/);
    if (mention) {
      rest = rest.slice(mention[0].length);
    }
  }
  const boundary = rest[0];
  if (
    boundary !== undefined
    && !/\s/.test(boundary)
    && boundary !== ":"
    && boundary !== ","
  ) {
    return { invoked: false };
  }

  let stripped = rest.trimStart();
  if (stripped.startsWith(":") || stripped.startsWith(",")) {
    stripped = stripped.slice(1).trimStart();
  }

  return { invoked: true, text: stripped.trim() };
}

function parseControlInvocationForPrefixes(
  text: string,
  prefixes: readonly string[],
): { invoked: true; text: string } | { invoked: false } {
  for (const prefix of prefixes) {
    const control = parseControlInvocation(text, prefix);
    if (control.invoked) {
      return control;
    }
  }
  return { invoked: false };
}

function channelScopedThreadId(
  channelType: string,
  channelAddress: string,
): string | undefined {
  const channel = channelType.trim();
  const address = channelAddress.trim();
  if (!channel || !address || channel.toLowerCase() === "web") {
    return undefined;
  }
  return `ext:${channel}:${address}`;
}

function channelScopedThreadIdForIntent(
  channelType: string,
  channelAddress: string,
  controlIntent: boolean | undefined,
): string | undefined {
  const base = channelScopedThreadId(channelType, channelAddress);
  if (!base) {
    return undefined;
  }
  return controlIntent === true ? `${base}:magic` : base;
}

/** Render the "your message was queued" hint sent back to the user when
 *  their inbound message landed on the pending-replay queue. */
function renderQueuedHint(receipt: QueuedMessageReceipt): string {
  const lines = [
    `Queued (#${receipt.position}). Type /stop to cancel the current run.`,
  ];
  if (receipt.dropped_oldest) {
    lines.push(
      "Older queued message dropped — keep your turns short during agent work.",
    );
  }
  return lines.join("\n");
}

function deliveryOptionsForDispatch(
  dispatchOptions: ChannelMessageDispatchOptions | undefined,
): ChannelSendTextOptions | undefined {
  if (dispatchOptions?.allowOutsideWindowTemplate === undefined) {
    return undefined;
  }

  return {
    allowOutsideWindowTemplate: dispatchOptions.allowOutsideWindowTemplate,
  };
}

function dispatchOptionsWithSenderDisplayName(
  dispatchOptions: ChannelMessageDispatchOptions | undefined,
  displayName: string | undefined,
): ChannelMessageDispatchOptions | undefined {
  const senderDisplayName = displayName?.trim();
  if (!senderDisplayName) {
    return dispatchOptions;
  }

  return {
    ...(dispatchOptions ?? {}),
    senderDisplayName,
  };
}

interface SessionBinding<TTarget> {
  target: TTarget;
  sessionId: string;
}

function isChatMessageReceivedEvent(
  event: MagicianRealtimeEvent,
): event is ChatMessageReceivedEvent {
  return event.event_type === "ChatMessageReceived";
}

function isCriticalRequestAlertEvent(
  event: MagicianRealtimeEvent,
): event is CriticalRequestAlertEvent {
  return event.event_type === "CriticalRequestAlert";
}

function isCriticalRequestRetiredEvent(
  event: MagicianRealtimeEvent,
): event is CriticalRequestRetiredEvent {
  return event.event_type === "CriticalRequestRetired";
}

/** How long a lost realtime socket waits before the runtime reconnects. */
const REALTIME_RECONNECT_DELAYS_MS = [1_000, 5_000, 15_000, 30_000];
/** Sent alerts remembered for retirement (delivery id → what was sent). */
const MAX_SENT_ALERTS = 512;
/** A card older than this is not annotated on retirement: the link already
 *  resolves to the completed state, and a day-old follow-up is noise (and,
 *  on WhatsApp, outside the 24-hour window anyway). */
const MAX_RETIRE_AGE_MS = 24 * 60 * 60 * 1000;

export class ChannelRuntime<TTarget = string, TContext = unknown> {
  private readonly channelType: string;
  private readonly adapter: ChannelRuntimeOptions<TTarget, TContext>["adapter"];
  private readonly magician: ChannelRuntimeOptions<TTarget, TContext>["magician"];
  private workspace = "";
  private readonly pendingApprovalMessage: string;
  private readonly connectedMessage:
    | string
    | ((identity: ChannelIdentity) => string);
  private readonly enableRealtime: boolean;
  private readonly controlPrefixes: string[];
  private readonly nonControlMessageBehavior: "dispatch" | "ignore";
  private readonly channelThreading: ChannelThreadingMode;
  private readonly nonControlMessageDispatch:
    | ChannelMessageDispatchOptions
    | undefined;
  private readonly sessionBindings = new Map<string, SessionBinding<TTarget>>();
  private readonly deliveredMessageIds = new Set<string>();
  private readonly activeDeliveries = new Set<string>();
  private readonly envoyAttempts = new Map<string, string>();
  private readonly envoyReceipts = new Map<string, EnvoyReceipt>();
  private readonly activeReceiptReports = new Map<string, Promise<void>>();
  private envoyReceiptTimer: ReturnType<typeof setTimeout> | undefined;
  private retryingEnvoyReceipts = false;
  /**
   * Sessions with a sensitive ask open on this channel, by session id → the
   * time the ask's own window closes. While one is open an inbound reply is
   * answered with the notice and never forwarded (plan §6): the only thing a
   * channel can do with a secret typed into it is leak it. Cleared when the
   * ask resolves or its window closes.
   */
  private readonly openSensitiveAsks = new Map<string, number>();
  private realtimeScopeKey: string | undefined;
  private realtimeConnection: {
    close(code?: number, reason?: string): void;
    onClose?(listener: () => void): void;
  } | undefined;
  /**
   * Counts realtime connections this process opened. A critical-request
   * claim carries the current generation so the backend binds the delivery
   * to *this* socket; a delivery claimed under an older generation is the
   * backend's to retire, not ours to resend.
   *
   * The counter alone is not an identity: every process of this channel starts
   * at 0, so two of them (an orphaned bot beside its replacement, a dev bot
   * beside the runtime-spawned one) claimed under the identical generation and
   * the backend could not tell them apart. `connectionGeneration` therefore
   * reports the counter joined to a per-process token.
   */
  private realtimeGeneration = 0;
  /** Distinguishes this process's generations from another process's. */
  private readonly processToken = Math.random().toString(36).slice(2, 10);
  private realtimeReconnectAttempt = 0;
  private realtimeReconnectTimer: ReturnType<typeof setTimeout> | undefined;
  private stopped = false;
  /** delivery id → the target, provider message id and time of an alert we sent. */
  private readonly sentAlerts = new Map<
    string,
    { target: TTarget; providerMessageId?: string; sentAt: number }
  >();

  constructor(options: ChannelRuntimeOptions<TTarget, TContext>) {
    this.channelType = options.channelType;
    this.adapter = options.adapter;
    this.magician = options.magician;
    this.pendingApprovalMessage =
      options.pendingApprovalMessage ?? DEFAULT_PENDING_APPROVAL_MESSAGE;
    this.connectedMessage =
      options.connectedMessage ?? DEFAULT_CONNECTED_MESSAGE;
    this.enableRealtime = options.enableRealtime ?? false;
    this.controlPrefixes = normalizeControlPrefixes(
      options.controlPrefix,
      options.controlPrefixes,
    );
    this.nonControlMessageBehavior =
      options.nonControlMessageBehavior ?? DEFAULT_NON_CONTROL_MESSAGE_BEHAVIOR;
    this.channelThreading =
      options.channelThreading ?? DEFAULT_CHANNEL_THREADING;
    this.nonControlMessageDispatch = options.nonControlMessageDispatch;
  }

  async start(): Promise<void> {
    const authoritativeScope = await this.magician.resolveBearerScope();
    this.workspace = authoritativeScope.workspace.trim();
    if (!this.workspace) {
      throw new Error("The Magician bearer did not resolve to a workspace.");
    }

    // BEFORE the adapter, not after. A long-polling adapter's `start` does not
    // return until the bot stops — Telegraf's `launch()` awaits its polling
    // loop — so anything sequenced after it never runs at all. The realtime
    // feed opened there was unreachable for exactly that reason, and a bot
    // that never opens the feed receives no critical-request alert and claims
    // no delivery, silently. The socket is authenticated by the bearer alone
    // and needs nothing from the adapter, so it belongs here.
    this.stopped = false;
    this.connectRealtimeFor(`bearer::${this.workspace}`);

    await this.adapter.start({
      onConnectRequest: async (event) => {
        await this.handleConnectRequest(
          event.target,
          event.channelAddress,
          event.displayName,
        );
      },
      onTextMessage: async (message) => {
        await this.handleTextMessage(message.target, message.channelAddress, {
          text: message.text,
          ...(message.displayName ? { displayName: message.displayName } : {}),
          ...(message.channelVerified !== undefined
            ? { channelVerified: message.channelVerified }
            : {}),
        });
      },
    });

  }

  async stop(): Promise<void> {
    this.stopped = true;
    clearTimeout(this.envoyReceiptTimer);
    this.envoyReceiptTimer = undefined;
    if (this.realtimeReconnectTimer) {
      clearTimeout(this.realtimeReconnectTimer);
      this.realtimeReconnectTimer = undefined;
    }
    this.realtimeConnection?.close();
    this.realtimeConnection = undefined;
    this.realtimeScopeKey = undefined;
    await this.adapter.stop?.();
  }

  /** The current realtime connection generation (exported for tests). */
  get connectionGeneration(): string {
    return `${this.processToken}-${this.realtimeGeneration}`;
  }

  private async handleConnectRequest(
    target: TTarget,
    channelAddress: string,
    displayName?: string,
  ): Promise<void> {
    const enrollment = await this.magician.enroll({
      channelType: this.channelType,
      channelAddress,
      ...(displayName ? { displayName } : {}),
    });

    if (enrollment.status === "pending") {
      await this.sendText(target, this.pendingApprovalMessage);
      return;
    }

    const identity = this.buildIdentity(channelAddress, enrollment);
    this.ensureRealtime(identity);
    await this.sendText(target, this.renderConnectedMessage(identity));
  }

  private async handleTextMessage(
    target: TTarget,
    channelAddress: string,
    input: {
      text: string;
      displayName?: string;
      channelVerified?: boolean;
    },
  ): Promise<void> {
    let text = input.text;
    let controlIntent: boolean | undefined;
    let dispatchOptions: ChannelMessageDispatchOptions | undefined;
    let deliveryOptions: ChannelSendTextOptions | undefined;

    if (this.controlPrefixes.length > 0) {
      const control = parseControlInvocationForPrefixes(
        input.text,
        this.controlPrefixes,
      );
      if (control.invoked) {
        text = control.text;
        controlIntent = true;
      } else if (this.nonControlMessageBehavior === "ignore") {
        return;
      } else {
        controlIntent = false;
        dispatchOptions = this.nonControlMessageDispatch;
        deliveryOptions = deliveryOptionsForDispatch(dispatchOptions);
      }
    }

    const enrollment = await this.magician.enroll({
      channelType: this.channelType,
      channelAddress,
      ...(input.displayName ? { displayName: input.displayName } : {}),
    });

    if (enrollment.status === "pending") {
      await this.sendText(target, this.pendingApprovalMessage, deliveryOptions);
      return;
    }

    const identity = this.buildIdentity(channelAddress, enrollment);
    // Per-message sender-auth signal (only set by adapters with a spoofable
    // `from`, e.g. agentmail). Flows to getActiveSession → `channel_verified`.
    if (input.channelVerified !== undefined) {
      identity.channelVerified = input.channelVerified;
    }
    if (controlIntent !== undefined) {
      identity.controlIntent = controlIntent;
    }
    const uiThreadId = this.uiThreadIdFor(channelAddress, controlIntent);
    if (uiThreadId) {
      identity.uiThreadId = uiThreadId;
    }
    this.ensureRealtime(identity);

    if (controlIntent === true && !text) {
      await this.sendText(target, this.renderConnectedMessage(identity));
      return;
    }

    const slash = parseSlashCommand(text);

    // Handle /start — trigger connect flow (welcome message)
    if (slash?.name === "start") {
      await this.sendText(
        target,
        this.renderConnectedMessage(identity),
        deliveryOptions,
      );
      return;
    }

    // Handle /new — archive current session and start fresh
    if (slash?.name === "new") {
      const session = await this.magician.newSession(identity);
      this.rememberSession(target, session.id, identity);
      await this.sendText(target, "New conversation started.", deliveryOptions);
      return;
    }

    const sessionDetail = await this.magician.getActiveSession(identity);
    this.rememberSession(target, sessionDetail.session.id, identity);
    const sessionId = sessionDetail.session.id;
    // Before any branch that could relay this message: a hold this process
    // never saw (a restart, a redeploy) is recovered from the session itself.
    this.rearmSensitiveAskFromSession(sessionId, sessionDetail.messages ?? []);

    // Phase 3 — chat-run control commands. These bypass the normal
    // message-dispatch path and reach the backend's chat-run / queue
    // APIs directly, so they work *while* an agent turn is in flight
    // (which is exactly when users want them).
    if (slash?.name === "stop") {
      // The run that raised the sensitive ask is being cancelled, so the hold
      // it armed goes with it — otherwise the notice's own advice ("stop the
      // run") left the channel refusing every message until the ask's window
      // ran out.
      this.openSensitiveAsks.delete(sessionId);
      const result = await this.magician.cancelChatRun(sessionId, identity);
      await this.sendText(
        target,
        result.cancelled
          ? "Stopped."
          : "Nothing to stop — no chat turn is currently in flight.",
        deliveryOptions,
      );
      return;
    }
    // `/clearqueue`, `/queue-clear`, AND `/queue clear` all clear the
    // queue. The natural `/queue clear` form needs to be handled BEFORE
    // the bare `/queue` list branch because the parser splits on the
    // first space and would otherwise route both to the list path.
    const isClearQueue =
      slash?.name === "clearqueue"
      || slash?.name === "queue-clear"
      || (slash?.name === "queue" && slash.args.toLowerCase() === "clear");
    if (isClearQueue) {
      const result = await this.magician.clearQueuedMessages(sessionId, identity);
      await this.sendText(
        target,
        result.cleared === 0
          ? "Queue was already empty."
          : `Cleared ${result.cleared} queued message${result.cleared === 1 ? "" : "s"}.`,
        deliveryOptions,
      );
      return;
    }
    if (slash?.name === "queue") {
      const list = await this.magician.listQueuedMessages(sessionId, identity);
      if (list.queued.length === 0) {
        await this.sendText(target, "No messages queued.", deliveryOptions);
      } else {
        const lines = list.queued.map((msg, idx) => {
          const preview = (msg.text ?? "(attachments only)").slice(0, 80);
          return `${idx + 1}. ${preview}`;
        });
        await this.sendText(
          target,
          `${list.queued.length} queued:\n${lines.join("\n")}`,
          deliveryOptions,
        );
      }
      return;
    }

    // A reply on this channel while a sensitive ask is open is refused with
    // the same notice; it is never accepted as the answer, never forwarded
    // into the transcript. `/stop` above still cancels the run.
    if (this.sensitiveAskIsOpen(sessionId)) {
      await this.sendText(target, SENSITIVE_REPLY_REFUSAL, deliveryOptions);
      return;
    }

    const messageDispatchOptions = dispatchOptionsWithSenderDisplayName(
      dispatchOptions,
      input.displayName,
    );

    const response = await this.magician.sendMessage(
      sessionId,
      identity,
      text,
      messageDispatchOptions,
    );

    // Backend held the message in the per-session pending queue because
    // a turn was already in flight. Surface the receipt; do NOT replay
    // here — the drain happens at the end of the current handler when
    // the in-flight turn settles.
    if (response.queued) {
      await this.sendText(target, renderQueuedHint(response.queued), deliveryOptions);
      return;
    }

    const responseDeliveryOptions =
      response.public_chat_notice?.template_allowed === undefined
        ? deliveryOptions
        : {
            ...deliveryOptions,
            allowOutsideWindowTemplate:
              response.public_chat_notice.template_allowed,
          };

    if (response.assistant_message) {
      await this.deliverChatMessage(
        target,
        sessionId,
        response.assistant_message,
        responseDeliveryOptions,
      );
    }

    for (const executed of response.tool_executed ?? []) {
      await this.deliverChatMessage(
        target,
        sessionId,
        executed,
        responseDeliveryOptions,
      );
    }

    // Drain only when the backend's drain-hint says the queue is
    // non-empty. Saves one GET per turn for the common case of a
    // session that never queued anything. `undefined` means "old
    // backend or unknown" — be conservative and check anyway.
    const depth = response.pending_queue_depth;
    if (depth === undefined || depth > 0) {
      await this.drainPendingQueue(sessionId, identity, target);
    }
  }

  /** Drain the per-session pending-message queue after a turn settles.
   *  Pops the oldest queued message, replays it through `sendMessage`,
   *  delivers the assistant response, and loops until either the queue
   *  is empty or the drain hits `DRAIN_MAX_ITERATIONS`. */
  private async drainPendingQueue(
    sessionId: string,
    identity: ChannelIdentity,
    target: TTarget,
  ): Promise<void> {
    for (let i = 0; i < DRAIN_MAX_ITERATIONS; i++) {
      const list = await this.magician.listQueuedMessages(sessionId, identity);
      const next = list.queued[0];
      if (!next) return;

      // Delete from the backend queue BEFORE replaying so a crash mid-replay
      // doesn't leave the same message stuck at head of queue forever.
      // The trade-off (and bounded risk): if the replay sendMessage fails,
      // the user's message is silently lost. Tracked as an open issue; for
      // now a `warn` log is emitted on send failure so it's visible.
      await this.magician.deleteQueuedMessage(sessionId, next.id, identity);

      const replayText = next.text;
      if (!replayText) {
        // Attachments-only replay isn't wired yet — skip rather than crash.
        continue;
      }
      const replayDispatchOptions: ChannelMessageDispatchOptions = {
        ...(next.profile_override ? { profile: next.profile_override } : {}),
        ...(next.source_surface
          ? { sourceSurface: next.source_surface }
          : {}),
        ...(next.presence_session_id
          ? { presenceSessionId: next.presence_session_id }
          : {}),
        ...(next.sender_display_name
          ? { senderDisplayName: next.sender_display_name }
          : {}),
      };
      if (
        next.source_surface
        && this.nonControlMessageDispatch?.sourceSurface === next.source_surface
        && this.nonControlMessageDispatch.allowOutsideWindowTemplate !== undefined
      ) {
        replayDispatchOptions.allowOutsideWindowTemplate =
          this.nonControlMessageDispatch.allowOutsideWindowTemplate;
      }
      const replayDeliveryOptions =
        deliveryOptionsForDispatch(replayDispatchOptions);

      let response: ChatResponse;
      try {
        response = await this.magician.sendMessage(
          sessionId,
          identity,
          replayText,
          replayDispatchOptions,
        );
      } catch (err) {
        console.warn(
          `[bot-sdk] drain replay failed for queued message ${next.id}: ${
            err instanceof Error ? err.message : String(err)
          }`,
        );
        return;
      }

      if (response.queued) {
        // Shouldn't normally happen — we just drained the slot. Be
        // defensive: surface the receipt and stop the drain so we don't
        // spin on a session that re-enqueues every replay.
        await this.sendText(
          target,
          renderQueuedHint(response.queued),
          replayDeliveryOptions,
        );
        return;
      }

      const responseDeliveryOptions =
        response.public_chat_notice?.template_allowed === undefined
          ? replayDeliveryOptions
          : {
              ...replayDeliveryOptions,
              allowOutsideWindowTemplate:
                response.public_chat_notice.template_allowed,
            };

      if (response.assistant_message) {
        await this.deliverChatMessage(
          target,
          sessionId,
          response.assistant_message,
          responseDeliveryOptions,
        );
      }
      for (const executed of response.tool_executed ?? []) {
        await this.deliverChatMessage(
          target,
          sessionId,
          executed,
          responseDeliveryOptions,
        );
      }
    }
  }

  private async handleRealtimeEvent(event: MagicianRealtimeEvent): Promise<void> {
    if (isCriticalRequestAlertEvent(event)) {
      await this.handleCriticalRequestAlert(event);
      return;
    }
    if (isCriticalRequestRetiredEvent(event)) {
      await this.handleCriticalRequestRetired(event);
      return;
    }
    if (!isChatMessageReceivedEvent(event)) {
      return;
    }

    const binding = this.sessionBindings.get(event.data.session_id);
    if (binding) {
      await this.deliverChatMessage(
        binding.target,
        binding.sessionId,
        event.data.message,
        this.deliveryOptionsForMessage(event.data.message),
      );
      return;
    }

    const originChannel = event.data.origin_channel;
    if (
      !originChannel
      || originChannel.channel_type !== this.channelType
      || !originChannel.address
      || !this.adapter.resolveRealtimeTarget
    ) {
      return;
    }

    const target = this.adapter.resolveRealtimeTarget(originChannel.address);
    if (target == null) {
      return;
    }

    this.sessionBindings.set(event.data.session_id, {
      target,
      sessionId: event.data.session_id,
    });

    await this.deliverChatMessage(
      target,
      event.data.session_id,
      event.data.message,
      this.deliveryOptionsForMessage(event.data.message),
    );
  }

  private async sendText(
    target: TTarget,
    text: string,
    options?: ChannelSendTextOptions,
  ): Promise<void | { accepted: boolean }> {
    return this.adapter.sendText(target, text, options);
  }

  private deliveryOptionsForMessage(
    message: ChatMessage,
  ): ChannelSendTextOptions | undefined {
    const dispatchOptions = this.nonControlMessageDispatch;
    if (
      !message.source_surface
      || !dispatchOptions
      || !dispatchOptions.sourceSurface
    ) {
      return undefined;
    }

    if (
      message.source_surface
      === `${dispatchOptions.sourceSurface}:public-chat-notice`
    ) {
      return { allowOutsideWindowTemplate: true };
    }

    if (dispatchOptions.sourceSurface !== message.source_surface) {
      return undefined;
    }

    return deliveryOptionsForDispatch(dispatchOptions);
  }

  private sensitiveAskIsOpen(sessionId: string): boolean {
    const expiresAt = this.openSensitiveAsks.get(sessionId);
    if (expiresAt === undefined) return false;
    if (expiresAt <= Date.now()) {
      this.openSensitiveAsks.delete(sessionId);
      return false;
    }
    return true;
  }

  /**
   * Re-arm the hold from the session's own transcript when this process has no
   * memory of it.
   *
   * The hold is the only thing stopping a channel from taking a secret into a
   * durable transcript, and it lived in one process's memory: a restart
   * (crash, redeploy, host reboot) forgot every open ask while the pause was
   * still open for its window, and the owner — who had already seen the notice
   * — could then send the password on WhatsApp and have it accepted as an
   * ordinary message. The realtime feed is live-only, so nothing re-delivers
   * the ask. The session's last escalation row is the server's own record of
   * it, and it is already in hand on every inbound message.
   */
  private rearmSensitiveAskFromSession(sessionId: string, messages: ChatMessage[]): void {
    if (this.openSensitiveAsks.has(sessionId)) return;
    for (let index = messages.length - 1; index >= 0; index -= 1) {
      const content = messages[index]?.content;
      if (content?.type === "escalation_resolved") return;
      if (content?.type !== "escalation") continue;
      // The newest ask decides: sensitive arms the hold, ordinary leaves it off.
      // A RECOVERED hold is armed only while the ask's own collection window is
      // still open, and expires with it — never with the "just delivered"
      // fallback, which would restart a fresh fifteen minutes from the same
      // stale row on every message, on every restart, for ever (no
      // `escalation_resolved` row is written for an agentic resolution, so the
      // row stays the newest one after the owner answers in the UI).
      const deadline = content.input_schema?.sensitive?.collection_deadline_ms;
      if (sensitiveAskNotice(content) && typeof deadline === "number" && deadline > Date.now()) {
        this.openSensitiveAsks.set(sessionId, sensitiveAskExpiry(content, Date.now()));
      }
      return;
    }
  }

  private async deliverChatMessage(
    target: TTarget, sessionId: string, message: ChatMessage,
    deliveryOptions?: ChannelSendTextOptions,
  ): Promise<void> {
    if (message.session_id !== sessionId) throw new Error("Message belongs to another session");
    const key = `${sessionId}:${message.id}`;
    if (this.activeDeliveries.has(key)) return;
    this.activeDeliveries.add(key);
    try {
      // The body already left. Recover only its receipt, even after the normal
      // delivered-message cache has evicted this message.
      if (this.envoyReceipts.has(key)) { await this.flushEnvoyReceipt(key); return; }
      await this.deliverChatMessageOnce(target, sessionId, message, deliveryOptions);
    }
    finally { this.activeDeliveries.delete(key); }
  }

  private async reportEnvoyPhase(
    sessionId: string, messageId: string, phase: "begin" | "provider_accepted" | "unknown",
    binding: EnvoyDeliveryBinding,
  ) {
    for (let attempt = 0; ; attempt += 1) {
      try { return await this.magician.reportEnvoyDelivery!(sessionId, messageId, phase, binding); }
      catch (error) {
        const status = (error as { status?: number })?.status;
        if (attempt >= 2 || (status && status >= 400 && status < 500 && status !== 408 && status !== 429)) throw error;
        await new Promise((resolve) => setTimeout(resolve, 100 * (attempt + 1)));
      }
    }
  }

  private flushEnvoyReceipt(key: string): Promise<void> {
    const active = this.activeReceiptReports.get(key);
    if (active) return active;
    const receipt = this.envoyReceipts.get(key);
    if (!receipt?.phase) return Promise.resolve();
    const phase = receipt.phase;
    const reporting = (async () => {
      try {
        await this.reportEnvoyPhase(receipt.sessionId, receipt.messageId, phase, receipt.binding);
        this.envoyReceipts.delete(key);
        this.rememberDelivered(receipt.messageId);
      } finally {
        this.activeReceiptReports.delete(key);
        this.scheduleEnvoyReceiptRetry();
      }
    })();
    this.activeReceiptReports.set(key, reporting);
    return reporting;
  }

  private scheduleEnvoyReceiptRetry(): void {
    if (this.stopped || this.retryingEnvoyReceipts || this.envoyReceiptTimer || ![...this.envoyReceipts.values()].some((receipt) => receipt.phase)) return;
    this.envoyReceiptTimer = setTimeout(() => {
      this.envoyReceiptTimer = undefined;
      void this.retryPendingEnvoyReceipts();
    }, ENVOY_RECEIPT_RETRY_MS);
    this.envoyReceiptTimer.unref?.();
  }

  private async retryPendingEnvoyReceipts(): Promise<void> {
    if (this.retryingEnvoyReceipts) return;
    this.retryingEnvoyReceipts = true;
    try {
      for (const key of [...this.envoyReceipts.keys()]) {
        if (this.stopped) break;
        await this.flushEnvoyReceipt(key).catch(() => {});
      }
    } finally {
      this.retryingEnvoyReceipts = false;
      this.scheduleEnvoyReceiptRetry();
    }
  }

  private async deliverChatMessageOnce(
    target: TTarget,
    sessionId: string,
    message: ChatMessage,
    deliveryOptions?: ChannelSendTextOptions,
  ): Promise<void> {
    if (message.direction === "user" || this.hasDelivered(message.id)) {
      return;
    }

    this.rememberDelivered(message.id);
    const presentationText = plainTextFromPresentation(message);

    switch (message.content.type) {
      case "text": {
        // Prepare/lease before sending. The server uses the stored session to
        // distinguish Envoy from owner chat. Never turn generated words into a
        // sent statement, nor re-send an already accepted/uncertain attempt.
        const report = this.magician.reportEnvoyDelivery?.bind(this.magician);
        let tracked = false;
        let prepared: Awaited<ReturnType<NonNullable<typeof this.adapter.prepareEnvoyTextDelivery>>> | undefined;
        if (report && message.direction === "assistant" && this.adapter.prepareEnvoyTextDelivery) {
          try { prepared = await this.adapter.prepareEnvoyTextDelivery(target, message.content.text); }
          catch (error) { this.deliveredMessageIds.delete(message.id); throw error; }
        }
        const key = `${sessionId}:${message.id}`;
        const attemptId = this.envoyAttempts.get(key) ?? randomUUID();
        const binding: EnvoyDeliveryBinding = {
          attempt_id: attemptId, channel_type: this.channelType,
          channel_address: prepared?.channelAddress ?? String(target),
          payload_sha256: createHash("sha256").update(message.content.text, "utf8").digest("hex"),
        };
        if (report && message.direction === "assistant") {
          this.envoyAttempts.set(key, attemptId);
          if (this.envoyAttempts.size > MAX_DELIVERED_MESSAGE_IDS) {
            this.envoyAttempts.delete(this.envoyAttempts.keys().next().value!);
          }
          try {
            const grant = await this.reportEnvoyPhase(sessionId, message.id, "begin", binding);
            tracked = grant.tracked;
            if (!grant.send) { this.envoyAttempts.delete(key); return; }
            if (tracked) {
              if (this.envoyReceipts.size >= MAX_DELIVERED_MESSAGE_IDS) throw new Error("Envoy receipt recovery queue is full; retry after the service recovers");
              this.envoyReceipts.set(key, { sessionId, messageId: message.id, binding, phase: null });
            }
          } catch (error) {
            this.deliveredMessageIds.delete(message.id);
            throw error;
          }
        }
        // Consume the resumable Begin identity BEFORE any external send. Cache
        // eviction/replay must never reuse this grant after entering the adapter.
        this.envoyAttempts.delete(key);
        let accepted = false;
        try {
          const receipt = tracked && prepared ? await prepared.send() : await this.sendText(
            target,
            tracked ? message.content.text : (presentationText ?? message.content.text),
            tracked ? { ...deliveryOptions, preserveExactText: true } : deliveryOptions,
          );
          accepted = receipt?.accepted === true;
        } catch (error) {
          // Some adapters send in chunks. A rejection cannot prove no chunk
          // left, so retain unknown instead of granting an automatic resend.
          if (tracked) {
            this.envoyReceipts.get(key)!.phase = "unknown";
            await this.flushEnvoyReceipt(key).catch(() => {});
          }
          throw error;
        }
        if (tracked) {
          this.envoyReceipts.get(key)!.phase = accepted ? "provider_accepted" : "unknown";
          await this.flushEnvoyReceipt(key);
        }
        return;
      }

      case "tool_call_executed": {
        const rendered = buildToolCallExecutedRender(
          sessionId,
          message.id,
          message.content.tool_name,
          message.content.summary,
        );
        const fallbackText = presentationText ?? rendered.text;

        if (this.adapter.sendToolCallExecuted) {
          await this.adapter.sendToolCallExecuted(target, rendered);
          return;
        }

        await this.sendText(target, fallbackText, deliveryOptions);
        return;
      }

      case "task_status_update": {
        const rendered = buildTaskStatusUpdateRender(
          sessionId,
          message.id,
          message.content.task_id,
          message.content.status,
          message.content.summary,
        );
        const fallbackText = presentationText ?? rendered.text;

        if (this.adapter.sendTaskStatusUpdate) {
          await this.adapter.sendTaskStatusUpdate(target, rendered);
          return;
        }

        await this.sendText(target, fallbackText, deliveryOptions);
        return;
      }

      case "escalation_resolved":
        this.openSensitiveAsks.delete(sessionId);
        // fall through: the resolution text relays like any other summary.
      case "rich_tool_result":
      case "attachment": {
        const canonical = canonicalTextForPresentation(message);
        const plain = presentationText ?? canonical;
        if (plain?.trim()) await this.sendText(target, plain, deliveryOptions);
        return;
      }

      case "escalation": {
        // A secret is never solicited into a channel transcript (plan §6): the
        // question that asks for it is replaced by a notice that says what is
        // needed and where to enter it, and says not to send it here. The
        // backend's spec decides, with the typed `password`/`otp` widgets and
        // the built-in secure browser asks kept for messages announced before
        // the spec existed.
        // A newly delivered ask supersedes the previous one for this session:
        // an ORDINARY question after a sensitive one must lift the hold, or the
        // channel stays refused for the sensitive ask's whole window (up to a
        // day for an agentic pause) even though the owner answered it in the UI
        // — no `escalation_resolved` row is produced for an agentic resolution.
        this.openSensitiveAsks.delete(sessionId);
        const secretNotice = sensitiveAskNotice(message.content);
        if (secretNotice) {
          this.openSensitiveAsks.set(sessionId, sensitiveAskExpiry(message.content, Date.now()));
          await this.sendText(target, secretNotice, deliveryOptions);
          return;
        }
        const escalationText = typeof message.content.question === "string"
          ? message.content.question.trim()
          : null;
        const escalationSummary = typeof message.content.summary === "string"
          ? message.content.summary.trim()
          : null;
        const fallback = escalationText && escalationText.length > 0
          ? escalationText
          : escalationSummary && escalationSummary.length > 0
            ? escalationSummary
            : null;
        const plain = presentationText ?? fallback;
        if (!plain) {
          return;
        }
        await this.sendText(target, plain, deliveryOptions);
        return;
      }

      case "pack_progress": {
        // Chat-spawned capability-pack progress events. Web UI shows them
        // as one updating card with inline file previews on terminal.
        // External channels deliver only the terminal summary plus any
        // generated files (image gen → photo, video gen → video, deep
        // research → document, etc.) so users see one rich message per
        // pack call, not the per-update stream.
        const isTerminalStatus = ["completed", "failed", "cancelled"].includes(
          message.content.status ?? "",
        );
        if (!isTerminalStatus) {
          return;
        }
        const summary = message.content.summary ?? message.content.status;
        const captionLine = `${message.content.pack_name}: ${summary}`;
        const outputFiles = Array.isArray(message.content.output_files)
          ? message.content.output_files
          : [];

        // Always send the textual summary first so the user has context
        // even if file uploads fail.
        await this.sendText(target, captionLine, deliveryOptions);

        if (outputFiles.length === 0 || !this.adapter.sendFile || !this.magician.resourceHeaders) {
          // A raw backend URL is not a capability and must never carry the
          // bearer in its query. The summary already tells the user the file
          // exists; they can open Magician when this channel cannot upload it.
          if (this.adapter.sendFile == null && outputFiles.length > 0) {
            for (const file of outputFiles) {
              await this.sendText(
                target,
                `${file.display_name ?? file.relative_path ?? "Output file"} is available in Magician.`,
                deliveryOptions,
              );
            }
          }
          return;
        }

        for (const file of outputFiles) {
          const url = this.buildSessionOutputUrl(sessionId, file);
          if (!url) continue;
          const mime = (file.mime_type ?? "application/octet-stream") as string;
          let kind: ChannelFileAttachment["kind"] = "other";
          if (mime.startsWith("image/")) kind = "image";
          else if (mime.startsWith("video/")) kind = "video";
          else if (mime.startsWith("audio/")) kind = "audio";
          else if (mime === "application/pdf") kind = "pdf";
          const attachment: ChannelFileAttachment = {
            url,
            download_headers: this.magician.resourceHeaders(),
            mime_type: mime,
            display_name: file.display_name ?? file.relative_path ?? "file",
            caption: captionLine,
            kind,
          };
          if (typeof file.size === "number") attachment.size = file.size;
          try {
            await this.adapter.sendFile(target, attachment);
          } catch (error) {
            console.warn(
              `[channel-runtime] sendFile failed for ${file.display_name ?? file.relative_path}; falling back to URL text`,
              error,
            );
            await this.sendText(target, url, deliveryOptions);
          }
        }
        return;
      }

      default:
        {
          const fallbackText = ((
            message.content as {
              text?: unknown;
              summary?: unknown;
              tool_name?: unknown;
              task_id?: unknown;
            }
          ));
          const summary = typeof fallbackText.summary === "string"
            ? fallbackText.summary
            : undefined;
          const text = typeof fallbackText.text === "string"
            ? fallbackText.text
            : summary;
          const plain = text?.trim();
          if (!plain) {
            return;
          }
          await this.sendText(target, plain, deliveryOptions);
        }
        return;
    }
  }

  private buildIdentity(
    channelAddress: string,
    enrollment: EnrollResult,
  ): ChannelIdentity {
    if (enrollment.status !== "enrolled") {
      throw new Error(
        `Expected enrolled identity for ${this.channelType}:${channelAddress}`,
      );
    }

    return {
      principal: enrollment.principal,
      channelType: this.channelType,
      channelAddress,
      workspace: this.workspace,
    };
  }

  private renderConnectedMessage(identity: ChannelIdentity): string {
    if (typeof this.connectedMessage === "function") {
      return this.connectedMessage(identity);
    }

    return this.connectedMessage;
  }

  private uiThreadIdFor(
    channelAddress: string,
    controlIntent: boolean | undefined,
  ): string | undefined {
    if (this.channelThreading !== "per-address") {
      return undefined;
    }
    return channelScopedThreadIdForIntent(
      this.channelType,
      channelAddress,
      controlIntent,
    );
  }

  private ensureRealtime(identity: ChannelIdentity): void {
    // The socket is scoped by the bearer, not by the identity that
    // happened to write: one connection per workspace serves every sender
    // (and the critical-request alerts that name no sender at all).
    this.connectRealtimeFor(`bearer::${identity.workspace ?? this.workspace}`);
  }

  private connectRealtimeFor(scopeKey: string): void {
    if (!this.enableRealtime || !this.magician.connectRealtime || this.stopped) {
      return;
    }
    if (this.realtimeConnection && this.realtimeScopeKey === scopeKey) {
      return;
    }

    this.realtimeConnection?.close(1000, "scope changed");

    try {
      const connection = this.magician.connectRealtime(async (event) => {
        await this.handleRealtimeEvent(event);
      });
      this.realtimeGeneration += 1;
      this.realtimeConnection = connection;
      this.realtimeScopeKey = scopeKey;
      const generation = this.realtimeGeneration;
      connection.onClose?.(() => {
        // Only the connection that is still current reconnects; a socket
        // we replaced on purpose is not a loss.
        if (this.stopped || this.realtimeGeneration !== generation) {
          return;
        }
        this.realtimeConnection = undefined;
        this.realtimeScopeKey = undefined;
        this.scheduleRealtimeReconnect(scopeKey);
      });
      this.realtimeReconnectAttempt = 0;
    } catch (error) {
      this.realtimeConnection = undefined;
      this.realtimeScopeKey = undefined;
      console.info("[channel-runtime] failed to connect realtime", error);
      this.scheduleRealtimeReconnect(scopeKey);
    }
  }

  private scheduleRealtimeReconnect(scopeKey: string): void {
    if (this.stopped || this.realtimeReconnectTimer) {
      return;
    }
    const delay = REALTIME_RECONNECT_DELAYS_MS[
      Math.min(this.realtimeReconnectAttempt, REALTIME_RECONNECT_DELAYS_MS.length - 1)
    ];
    this.realtimeReconnectAttempt += 1;
    this.realtimeReconnectTimer = setTimeout(() => {
      this.realtimeReconnectTimer = undefined;
      this.connectRealtimeFor(scopeKey);
    }, delay);
    // A pending reconnect must not keep a stopping process alive.
    (this.realtimeReconnectTimer as { unref?: () => void }).unref?.();
  }

  /**
   * A critical-request alert offered to this channel (secure HITL plan
   * §6.1): claim it — the backend hands over the owner address for exactly
   * this send and binds the delivery to this bot and socket — send the
   * value-free card, and report what the provider said. The event carries
   * no address; a claim the backend refuses (another bot was first, the
   * request already resolved) is simply not ours.
   */
  private async handleCriticalRequestAlert(
    event: CriticalRequestAlertEvent,
  ): Promise<void> {
    const data = event.data;
    if (
      !data
      || data.channel_type !== this.channelType
      || !this.magician.claimCriticalDelivery
      || !this.magician.reportCriticalDelivery
      || !this.adapter.resolveRealtimeTarget
    ) {
      return;
    }
    // The generation this claim is made with. Every report for it echoes this
    // one, not whatever the runtime has reconnected to since: only the
    // connection that claimed may report the provider's answer.
    const claimedWith = this.connectionGeneration;
    let claim;
    try {
      claim = await this.magician.claimCriticalDelivery(
        data.delivery_id,
        this.channelType,
        claimedWith,
      );
    } catch (error) {
      console.info("[channel-runtime] critical alert not claimed", data.delivery_id, error);
      return;
    }
    const target = this.adapter.resolveRealtimeTarget(claim.address);
    if (target == null) {
      await this.reportCriticalDelivery(claim.delivery_id, {
        status: "failed",
        reason: "the owner address does not resolve to a channel target",
      }, claimedWith);
      return;
    }
    try {
      let providerMessageId: string | undefined;
      if (this.adapter.sendCriticalAlert) {
        const sent = await this.adapter.sendCriticalAlert(target, claim.alert);
        providerMessageId = sent?.providerMessageId;
      } else {
        await this.adapter.sendText(target, claim.alert.text, {
          allowOutsideWindowTemplate: true,
        });
      }
      this.rememberSentAlert(claim.delivery_id, {
        target,
        ...(providerMessageId ? { providerMessageId } : {}),
      });
      const refused = await this.reportCriticalDelivery(claim.delivery_id, {
        status: "provider_accepted",
        ...(providerMessageId ? { provider_message_id: providerMessageId } : {}),
      }, claimedWith);
      // A report refused as no longer reportable means the request closed
      // between our claim and our send (answered, expired, cancelled): the
      // card just sent is already stale, and the backend's retirement — if
      // it raced ahead of the send — found nothing to retire. Retire it now.
      if (refused === 409) {
        this.sentAlerts.delete(claim.delivery_id);
        try {
          await this.adapter.retireCriticalAlert?.(target, {
            ...(providerMessageId ? { providerMessageId } : {}),
            outcome: "superseded",
          });
        } catch (error) {
          console.info("[channel-runtime] could not retire a superseded alert", claim.delivery_id, error);
        }
      }
    } catch (error) {
      await this.reportCriticalDelivery(claim.delivery_id, {
        status: "failed",
        reason: describeSendFailure(error),
      }, claimedWith);
    }
  }

  /** Reports; returns the HTTP status of a refusal, or `null` when accepted. */
  private async reportCriticalDelivery(
    deliveryId: string,
    report: { status: "provider_accepted" | "confirmed_delivered" | "failed"; provider_message_id?: string; reason?: string },
    connectionGeneration?: string,
  ): Promise<number | null> {
    try {
      await this.magician.reportCriticalDelivery?.(deliveryId, {
        ...report,
        ...(connectionGeneration ? { connection_generation: connectionGeneration } : {}),
      });
      return null;
    } catch (error) {
      console.info("[channel-runtime] critical delivery report failed", deliveryId, error);
      const status = (error as { status?: unknown })?.status;
      return typeof status === "number" ? status : -1;
    }
  }

  private rememberSentAlert(
    deliveryId: string,
    sent: { target: TTarget; providerMessageId?: string },
  ): void {
    this.sentAlerts.set(deliveryId, { ...sent, sentAt: Date.now() });
    while (this.sentAlerts.size > MAX_SENT_ALERTS) {
      const oldest = this.sentAlerts.keys().next().value;
      if (oldest === undefined) {
        break;
      }
      this.sentAlerts.delete(oldest);
    }
  }

  /** The request behind an alert we sent resolved: edit or annotate it. */
  private async handleCriticalRequestRetired(
    event: CriticalRequestRetiredEvent,
  ): Promise<void> {
    const data = event.data;
    if (!data || data.channel_type !== this.channelType) {
      return;
    }
    const sent = this.sentAlerts.get(data.delivery_id);
    if (!sent) {
      return;
    }
    this.sentAlerts.delete(data.delivery_id);
    if (!this.adapter.retireCriticalAlert || Date.now() - sent.sentAt > MAX_RETIRE_AGE_MS) {
      return;
    }
    try {
      await this.adapter.retireCriticalAlert(sent.target, {
        ...(sent.providerMessageId ? { providerMessageId: sent.providerMessageId } : {}),
        outcome: data.outcome,
      });
    } catch (error) {
      console.info("[channel-runtime] could not retire a critical alert", data.delivery_id, error);
    }
  }

  private rememberSession(
    target: TTarget,
    sessionId: string,
    identity: ChannelIdentity,
  ): void {
    this.sessionBindings.set(sessionId, {
      target,
      sessionId,
    });
  }

  /**
   * Build the magician-served HTTP URL for a chat-session output file.
   * Returns `null` when the file's relative path or backend base URL is
   * missing. Authorization travels separately in `download_headers`.
   */
  private buildSessionOutputUrl(
    sessionId: string,
    file: { relative_path?: string; absolute_path?: string },
  ): string | null {
    const relative = file.relative_path?.trim();
    if (!relative) return null;
    const baseUrl = (this.magician as { baseUrl?: string }).baseUrl;
    if (!baseUrl) return null;
    const encodedPath = relative
      .split("/")
      .filter(Boolean)
      .map((segment) => encodeURIComponent(segment))
      .join("/");
    const path = `/api/magician/v2/chat/sessions/${encodeURIComponent(
      sessionId,
    )}/outputs/${encodedPath}`;
    return `${baseUrl.replace(/\/$/, "")}${path}`;
  }

  private hasDelivered(messageId: string): boolean {
    return this.deliveredMessageIds.has(messageId);
  }

  private rememberDelivered(messageId: string): void {
    this.deliveredMessageIds.add(messageId);
    if (this.deliveredMessageIds.size <= MAX_DELIVERED_MESSAGE_IDS) {
      return;
    }

    const oldest = this.deliveredMessageIds.values().next().value;
    if (oldest) {
      this.deliveredMessageIds.delete(oldest);
    }
  }
}

/** A bounded, value-free description of a send failure for the report. */
function describeSendFailure(error: unknown): string {
  const text = error instanceof Error ? `${error.name}: ${error.message}` : String(error);
  return text.length > 200 ? `${text.slice(0, 200)}…` : text;
}

/**
 * The notice a channel relays instead of an ask that collects a secret, or
 * `null` when the ask is ordinary.
 *
 * Exported for the SDK tests. What it names — a password, a code, sign-in
 * details — comes from the backend's classification; the value itself is
 * never requested here, and the reader is told so in words, because the only
 * thing a channel can do with a secret typed into it is leak it.
 */
/** How long a channel keeps refusing replies to a sensitive ask whose spec
 *  names no window of its own (a password ask waits on the request itself). */
export const SENSITIVE_ASK_REPLY_HOLD_MS = 15 * 60 * 1000;

/** What an inbound reply gets while a sensitive ask is open on the channel. */
export const SENSITIVE_REPLY_REFUSAL =
  "Magician is waiting for a private value and can't take it here. Enter it in the Magician app or web UI, or send /stop to cancel. This message was not sent on.";

/** The longest a spec's own window is honoured before the channel forwards
 *  again on its own (a resolution normally clears it well before). */
const SENSITIVE_ASK_REPLY_HOLD_CAP_MS = 24 * 60 * 60 * 1000;

/**
 * When the channel stops refusing replies for this ask: the spec's own
 * collection window when it names one, else a bounded hold from now.
 */
export function sensitiveAskExpiry(content: EscalationContent, nowMs: number): number {
  const deadline = content.input_schema?.sensitive?.collection_deadline_ms;
  if (typeof deadline === "number" && Number.isFinite(deadline) && deadline > nowMs) {
    return Math.min(deadline, nowMs + SENSITIVE_ASK_REPLY_HOLD_CAP_MS);
  }
  return nowMs + SENSITIVE_ASK_REPLY_HOLD_MS;
}

export function sensitiveAskNotice(content: EscalationContent): string | null {
  const spec = content.input_schema?.sensitive;
  const requestType = content.input_schema?.request_type ?? "";
  const inputType = content.input_type ?? "";
  const legacySecure = requestType === "secure_browser_input" || requestType === "secure_browser_confirm";
  if (!spec && !legacySecure && inputType !== "password" && inputType !== "otp") {
    return null;
  }
  const fieldKinds = (spec?.fields ?? []).map((field) => field.kind);
  const kind = spec?.kind ?? (inputType === "otp" ? "otp" : inputType === "password" ? "password" : null);
  const needs = kind === "otp" || fieldKinds.includes("otp")
    ? "a verification code"
    : fieldKinds.length > 1 || fieldKinds.includes("login_identifier")
      ? "your sign-in details"
      : kind === "password" || fieldKinds.includes("password") || legacySecure
        ? "your password"
        : "a private value";
  return `Magician needs ${needs} to continue. For your security, enter it in the Magician app or web UI — don't send it here.`;
}
