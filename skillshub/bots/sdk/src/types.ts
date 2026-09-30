export type JsonPrimitive = string | number | boolean | null;
export type JsonValue = JsonPrimitive | JsonObject | JsonValue[];

export interface JsonObject {
  [key: string]: JsonValue;
}

export interface ChannelAddressIdentity {
  channelType: string;
  channelAddress: string;
}

export interface ChannelIdentity extends ChannelAddressIdentity {
  principal: string;
  /** Informational scope resolved from the bearer; never serialized as request authority. */
  workspace?: string;
  /** Stable UI thread requested for this channel message. Public chat bots use
   *  this to keep ordinary chat and owner-control chat out of `general`. */
  uiThreadId?: string;
  /** Whether the transport authenticated the sender (per-message). Surfaced to
   *  the chat active-session call as `channel_verified`; omitted ⇒ the backend
   *  treats the sender as verified, so channels that don't set it are
   *  unaffected. The agentmail adapter sets `false` for `unauthenticated` mail. */
  channelVerified?: boolean;
  /** Whether this message explicitly invoked the owner-control surface. When
   *  false, owner allowlist routing is suppressed and the backend routes the
   *  sender like a guest/envoy conversation. Omitted preserves legacy routing. */
  controlIntent?: boolean;
}

export interface EnrollRequest extends ChannelAddressIdentity {
  displayName?: string;
}

export type EnrollmentStatus =
  | {
      status: "enrolled";
      principal: string;
    }
  | {
      status: "pending";
      code: string | null;
    }
  | {
      status: "unknown";
    };

export type EnrollResult = Exclude<EnrollmentStatus, { status: "unknown" }>;

export type ChatMessageDirection = "user" | "assistant" | "system";
export type ChatSessionStatus = "active" | "archived";

export interface ChatChannel {
  channel_type: string;
  address?: string;
}

export interface ChatSession {
  id: string;
  principal: string;
  workspace: string;
  agent_id: string;
  title: string | null;
  origin_channel: ChatChannel;
  status: ChatSessionStatus;
  created_at: number;
  updated_at: number;
}

export interface ChatTextMessageContent {
  type: "text";
  text: string;
}

export interface StructuredResponsePresentation {
  schema: "magician.structured_response";
  version: 1;
  plain_text: string;
}

export type ChatMessagePresentation = StructuredResponsePresentation;

export interface ToolCallExecutedContent {
  type: "tool_call_executed";
  tool_name: string;
  summary: string;
}

export interface TaskStatusUpdateContent {
  type: "task_status_update";
  task_id: string;
  status: string;
  summary?: string;
}

export interface PackProgressOutputFile {
  type: "file";
  source: { type: "session_output" } | { type: "task_output"; task_id: string };
  relative_path: string;
  display_name: string;
  mime_type: string;
  absolute_path?: string;
  label?: string;
  size: number;
}

export interface PackProgressContent {
  type: "pack_progress";
  pack_name: string;
  execution_id: string;
  status: string;
  summary?: string;
  tool_call_id?: string;
  output_files?: PackProgressOutputFile[];
}

export interface EscalationContent {
  type: "escalation";
  question?: string;
  summary?: string;
  /** The pause's input type (`text`, `password`, `otp`, `choice`, …). */
  input_type?: string;
  /**
   * The typed schema. `sensitive` is the backend's value-free classification
   * when the ask collects a secret (P3): a channel relays a notice for such an
   * ask, never the question, and never invites the value into the transcript.
   */
  input_schema?: {
    sensitive?: {
      kind?: "login_identifier" | "password" | "otp" | "other";
      fields?: Array<{ id: string; kind: string }>;
      one_time?: boolean;
      collection_deadline_ms?: number;
    };
    request_type?: string;
    [key: string]: unknown;
  };
}

export interface RichToolResultContent {
  type: "rich_tool_result";
  tool_name: string;
  summary: string;
  tool_call_id?: string;
  content_blocks?: unknown[];
}

export interface AttachmentContent {
  type: "attachment";
  filename: string;
  mime_type: string;
  size: number;
  label?: string;
  absolute_path?: string;
}

export interface EscalationResolvedContent {
  type: "escalation_resolved";
  execution_id: string;
  summary: string;
  task_id?: string;
}

export type ChatMessageContent =
  | ChatTextMessageContent
  | ToolCallExecutedContent
  | TaskStatusUpdateContent
  | PackProgressContent
  | EscalationContent
  | RichToolResultContent
  | AttachmentContent
  | EscalationResolvedContent;

export interface ChatMessage {
  id: string;
  session_id: string;
  direction: ChatMessageDirection;
  content: ChatMessageContent;
  presentation?: ChatMessagePresentation;
  created_at: number;
  chat_turn_id?: string;
  source_surface?: string;
  presence_session_id?: string;
}

export interface ChatSessionDetail {
  session: ChatSession;
  messages: ChatMessage[];
}

export interface ChatResponse {
  user_message?: ChatMessage;
  assistant_message?: ChatMessage;
  tool_executed?: ChatMessage[];
  session_title?: string;
  /**
   * Present when the inbound message could not dispatch immediately because
   * the session had an in-flight turn. The backend enqueued it in the
   * per-session pending-message queue; the caller should NOT treat the
   * response as an agent reply. Position is 1-indexed.
   */
  queued?: QueuedMessageReceipt;
  /** True when the turn was cancelled by `/stop` / `cancel_chat_run`. */
  cancelled?: boolean;
  /** Pending-queue depth at the moment the turn settled. `0` when the
   *  queue is empty (no drain needed); `> 0` when the SDK should fetch
   *  the queue and replay. `undefined` when the response was the
   *  queued-receipt path (no turn ran). */
  pending_queue_depth?: number;
  /** Deterministic public-chat admission notice for queue/overload/fallback
   *  responses. Normal LLM replies leave this absent. */
  public_chat_notice?: PublicChatNotice;
}

export interface PublicChatNotice {
  kind: "queued" | "overload" | "fallback_notice";
  template_allowed: boolean;
  reason?: string;
}

/** A pending-replay message held in the per-session queue. Mirror of the
 *  Rust `QueuedMessage` struct. */
export interface QueuedMessage {
  id: string;
  session_id: string;
  text?: string;
  attachment_ids?: string[];
  profile_override?: string;
  source_surface?: string;
  presence_session_id?: string;
  sender_display_name?: string;
  channel?: string;
  channel_address?: string;
  queued_at: number;
}

/** Receipt returned to the caller when a message was enqueued instead of
 *  dispatched. `position` is 1-indexed. `dropped_oldest` is set when the
 *  enqueue pushed the queue past the per-session cap and the oldest
 *  message was discarded. */
export interface QueuedMessageReceipt {
  id: string;
  position: number;
  dropped_oldest?: QueuedMessage;
}

export interface ListQueuedMessagesResponse {
  queued: QueuedMessage[];
  session_id: string;
}

export interface CancelChatRunResponse {
  cancelled: boolean;
  session_id: string;
}

export interface DeleteQueuedMessageResponse {
  deleted: boolean;
  session_id: string;
  message_id: string;
}

export interface ClearQueuedMessagesResponse {
  cleared: number;
  session_id: string;
}

export interface ChannelTextMessage<TTarget = string, TContext = unknown> {
  target: TTarget;
  channelAddress: string;
  text: string;
  displayName?: string;
  context?: TContext;
  /** Whether the transport authenticated the sender. Adapters whose `from` is
   *  spoofable (e.g. agentmail) set `false` for unauthenticated messages so the
   *  backend denies owner-trust to a spoofed sender. Omitted ⇒ verified. */
  channelVerified?: boolean;
}

export interface ChannelConnectEvent<TTarget = string, TContext = unknown> {
  target: TTarget;
  channelAddress: string;
  displayName?: string;
  context?: TContext;
}

export interface ChannelAdapterHandlers<TTarget = string, TContext = unknown> {
  onConnectRequest?(message: ChannelConnectEvent<TTarget, TContext>): Promise<void>;
  onTextMessage(message: ChannelTextMessage<TTarget, TContext>): Promise<void>;
}

export interface ToolCallExecutedRender {
  sessionId: string;
  messageId: string;
  toolName: string;
  summary: string;
  text: string;
}

export interface TaskStatusUpdateRender {
  sessionId: string;
  messageId: string;
  taskId: string;
  status: string;
  summary?: string;
  text: string;
}

/**
 * One file the channel runtime should attach to a chat reply. Channels that
 * implement `sendFile` should fetch from `url` with `download_headers` (the
 * magician backend serves the bytes) and upload through the
 * channel-native media API. `mime_type` is the source-of-truth content type;
 * `display_name` is what the channel should show as the filename / caption
 * fallback. `size` is best-effort — channels that need it for upload limits
 * may either trust it or HEAD the URL.
 */
export interface ChannelFileAttachment {
  url: string;
  /** Authorization for this backend download. Never append it to the URL or forward it to the channel. */
  download_headers: Readonly<Record<string, string>>;
  mime_type: string;
  display_name: string;
  size?: number;
  /** Optional caption text to attach with the file (e.g., the pack summary). */
  caption?: string;
  /** Source content classification ("image" | "video" | "audio" | "pdf" | "other"). */
  kind: "image" | "video" | "audio" | "pdf" | "other";
}

export interface ChannelAdapter<TTarget = string, TContext = unknown> {
  start(handlers: ChannelAdapterHandlers<TTarget, TContext>): Promise<void>;
  stop?(): Promise<void>;
  resolveRealtimeTarget?(channelAddress: string): TTarget | null;
  /** Read-only preparation for transports whose opaque reply target differs
   * from the recipient address. The returned send closes over the exact text
   * and resolved recipient; it is invoked only after the host grants dispatch. */
  prepareEnvoyTextDelivery?(target: TTarget, text: string): Promise<{
    channelAddress: string;
    send(): Promise<{ accepted: boolean }>;
  }>;
  /** Explicit acceptance covers all text chunks. Suppression, fallback to
   * different text, partial sends and swallowed errors must not return true.
   * Legacy void-returning adapters remain usable but cannot attest Envoy sends. */
  sendText(
    target: TTarget,
    text: string,
    options?: ChannelSendTextOptions,
  ): Promise<void | { accepted: boolean }>;
  sendToolCallExecuted?(
    target: TTarget,
    executed: ToolCallExecutedRender,
  ): Promise<void>;
  sendTaskStatusUpdate?(
    target: TTarget,
    update: TaskStatusUpdateRender,
  ): Promise<void>;
  /**
   * Optional: send a file (image / video / audio / pdf / other document)
   * to the channel. When implemented, the SDK uses this for capability-pack
   * `output_files` (image gen results, video gen, deep-research PDFs, etc.).
   * When absent, the SDK falls back to a text message containing a clickable
   * URL — better than silence but worse than a real attachment.
   */
  sendFile?(target: TTarget, file: ChannelFileAttachment): Promise<void>;
  /**
   * Optional: send a critical-request alert natively (a button for the
   * secure link, say) and return the provider's message id when it has
   * one. When absent, the SDK sends `alert.text` through `sendText`.
   */
  sendCriticalAlert?(
    target: TTarget,
    alert: CriticalRequestAlertCard,
  ): Promise<{ providerMessageId?: string } | void>;
  /**
   * Optional: retire an alert that was sent — edit it, or follow it up with
   * a one-line note — once the request resolved. When absent, nothing is
   * sent: the link resolves to the completed state on its own.
   */
  retireCriticalAlert?(
    target: TTarget,
    sent: { providerMessageId?: string; outcome: string },
  ): Promise<void>;
}

export interface EnvoyDeliveryBinding {
  attempt_id: string;
  channel_type: string;
  channel_address: string;
  payload_sha256: string;
}

export interface ChannelSendTextOptions {
  allowOutsideWindowTemplate?: boolean;
  /** A prepared Envoy payload must survive adapter formatting and chunking. */
  preserveExactText?: boolean;
}

export interface RawRealtimeEvent<TData = unknown> {
  event_type: string;
  data: TData;
}

export interface ChatMessageReceivedEventData {
  session_id: string;
  message: ChatMessage;
  origin_channel?: ChatChannel;
  /** Backend principal (scope owner). Used to build chat-session output URLs. */
  principal?: string;
  /** Backend workspace name. Used to build chat-session output URLs. */
  workspace?: string;
}

export interface ChatMessageReceivedEvent
  extends RawRealtimeEvent<ChatMessageReceivedEventData> {
  event_type: "ChatMessageReceived";
}

/**
 * The value-free card a critical-request alert carries (secure HITL plan
 * §6.1). `text` is the one sentence every channel sends; the structured
 * fields let an adapter add a native button. Never a prompt, an option, an
 * answer or a token — and never the owner's address, which arrives only
 * through the claim.
 */
export interface CriticalRequestAlertCard {
  kind: "request" | "test";
  service_alias: string;
  reason: string;
  text: string;
  deadline_ms?: number;
  open_url?: string;
}

export interface CriticalRequestAlertEventData {
  delivery_id: string;
  correlation_id: string;
  channel_type: string;
  kind: "request" | "test";
  alert: CriticalRequestAlertCard;
  revision: number;
  deadline_ms?: number;
  principal?: string;
  workspace?: string;
  timestamp: number;
}

export interface CriticalRequestAlertEvent
  extends RawRealtimeEvent<CriticalRequestAlertEventData> {
  event_type: "CriticalRequestAlert";
}

export interface CriticalRequestRetiredEventData {
  delivery_id: string;
  correlation_id: string;
  channel_type: string;
  outcome: string;
  principal?: string;
  workspace?: string;
  timestamp: number;
}

export interface CriticalRequestRetiredEvent
  extends RawRealtimeEvent<CriticalRequestRetiredEventData> {
  event_type: "CriticalRequestRetired";
}

export type MagicianRealtimeEvent =
  | ChatMessageReceivedEvent
  | CriticalRequestAlertEvent
  | CriticalRequestRetiredEvent
  | RawRealtimeEvent;

/** What a claim hands the channel's bot: the owner address for one delivery. */
export interface CriticalDeliveryClaim {
  delivery_id: string;
  correlation_id: string;
  kind: "request" | "test";
  channel_type: string;
  address: string;
  alert: CriticalRequestAlertCard;
  deadline_ms?: number;
}

export interface CriticalDeliveryReport {
  status: "provider_accepted" | "confirmed_delivered" | "failed";
  provider_message_id?: string;
  reason?: string;
  /** The connection generation that CLAIMED this delivery. One bot name can be
   *  two processes; only the connection that claimed may say what the provider
   *  did with the send, so the runtime echoes the generation it claimed with. */
  connection_generation?: string;
}

export type ChannelThreadingMode = "default" | "per-address";

export interface ChannelRuntimeOptions<TTarget = string, TContext = unknown> {
  channelType: string;
  adapter: ChannelAdapter<TTarget, TContext>;
  magician: MagicianClientLike;
  enableRealtime?: boolean;
  /** Optional prefix that turns an inbound channel message into an owner-control
   *  message (for example "@magic do this"). Without the prefix, behavior is
   *  controlled by `nonControlMessageBehavior`. */
  controlPrefix?: string;
  /** Optional additional control prefixes accepted alongside
   *  `controlPrefix`. Useful for platform-native command aliases such as
   *  Telegram `/magic` while keeping legacy `@magic` compatibility. */
  controlPrefixes?: readonly string[];
  /** What to do with inbound messages that do not start with `controlPrefix`.
   *  `dispatch` sends them as ordinary channel conversation messages with
   *  `control_intent=false`; `ignore` drops them before enrollment/session
   *  creation so the channel remains observational. Default: `dispatch`. */
  nonControlMessageBehavior?: "dispatch" | "ignore";
  /** Threading policy for channel chat bots. `per-address` requests
   *  `ext:<channel>:<address>` for ordinary chat and
   *  `ext:<channel>:<address>:magic` for explicit owner-control messages.
   *  Default keeps legacy backend thread selection. */
  channelThreading?: ChannelThreadingMode;
  /** Optional send policy for non-control messages. Used by agent-owned
   *  surfaces such as Kapso to keep ordinary conversation in a cheap,
   *  chat-only profile while preserving the normal session/thread ledger. */
  nonControlMessageDispatch?: ChannelMessageDispatchOptions;
  pendingApprovalMessage?: string;
  connectedMessage?:
    | string
    | ((identity: ChannelIdentity) => string);
}

export interface ChannelMessageDispatchOptions {
  profile?: string;
  sourceSurface?: string;
  presenceSessionId?: string;
  senderDisplayName?: string;
  allowOutsideWindowTemplate?: boolean;
}

export interface MagicianClientLike {
  /** Resolve the workspace bound to the bearer. Channel runtimes call this
   * before accepting traffic so local configuration can never select scope. */
  resolveBearerScope(): Promise<{ workspace: string }>;
  enroll(request: EnrollRequest): Promise<EnrollResult>;
  getActiveSession(identity: ChannelIdentity): Promise<ChatSessionDetail>;
  newSession(identity: ChannelIdentity): Promise<ChatSession>;
  sendMessage(
    sessionId: string,
    identity: ChannelIdentity,
    text: string,
    options?: ChannelMessageDispatchOptions,
  ): Promise<ChatResponse>;
  reportEnvoyDelivery?(
    sessionId: string,
    messageId: string,
    phase: "begin" | "provider_accepted" | "unknown",
    binding?: EnvoyDeliveryBinding,
  ): Promise<{ tracked: boolean; send: boolean; status: string }>;
  /** Phase 1 — cancel the in-flight chat turn for `sessionId`. */
  cancelChatRun(
    sessionId: string,
    identity: ChannelIdentity,
  ): Promise<CancelChatRunResponse>;
  /** Phase 2 — list pending-replay messages for `sessionId` in FIFO order. */
  listQueuedMessages(
    sessionId: string,
    identity: ChannelIdentity,
  ): Promise<ListQueuedMessagesResponse>;
  /** Phase 2 — remove one queued message by id. Idempotent. */
  deleteQueuedMessage(
    sessionId: string,
    messageId: string,
    identity: ChannelIdentity,
  ): Promise<DeleteQueuedMessageResponse>;
  /** Phase 2 — clear the entire pending-replay queue for `sessionId`. */
  clearQueuedMessages(
    sessionId: string,
    identity: ChannelIdentity,
  ): Promise<ClearQueuedMessagesResponse>;
  /** Headers for downloading a Magician-owned resource without query credentials. */
  resourceHeaders?(): Readonly<Record<string, string>>;
  connectRealtime?(
    listener: (event: MagicianRealtimeEvent) => void | Promise<void>,
  ): MagicianRealtimeConnectionLike;
  /** P5 — claim one critical-request delivery for this bot's channel type. */
  claimCriticalDelivery?(
    deliveryId: string,
    channelType: string,
    connectionGeneration: string,
  ): Promise<CriticalDeliveryClaim>;
  /** P5 — report the provider's answer for a claimed delivery. */
  reportCriticalDelivery?(
    deliveryId: string,
    report: CriticalDeliveryReport,
  ): Promise<void>;
}

export interface MagicianRealtimeConnectionLike {
  close(code?: number, reason?: string): void;
  /** Optional: called once when the socket closes for any reason, so the
   *  runtime can reconnect and start a new connection generation. */
  onClose?(listener: () => void): void;
}
