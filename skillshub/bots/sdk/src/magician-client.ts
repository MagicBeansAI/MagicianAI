import type { EnvoyDeliveryBinding } from "./types.js";
import type {
  CancelChatRunResponse,
  ChannelAddressIdentity,
  ChannelIdentity,
  ChannelMessageDispatchOptions,
  ChatResponse,
  ChatSession,
  ChatSessionDetail,
  ClearQueuedMessagesResponse,
  CriticalDeliveryClaim,
  CriticalDeliveryReport,
  DeleteQueuedMessageResponse,
  EnrollRequest,
  EnrollResult,
  EnrollmentStatus,
  ListQueuedMessagesResponse,
  MagicianRealtimeEvent,
} from "./types.js";

type FetchLike = typeof fetch;

interface WebSocketLike {
  readonly readyState: number;
  addEventListener(
    type: "open" | "message" | "error" | "close",
    listener: EventListenerOrEventListenerObject,
  ): void;
  removeEventListener(
    type: "open" | "message" | "error" | "close",
    listener: EventListenerOrEventListenerObject,
  ): void;
  close(code?: number, reason?: string): void;
}

export interface MagicianClientOptions {
  baseUrl: string;
  /** Workspace-bound API token or session bearer used by every transport. */
  bearerToken: string;
  fetchImpl?: FetchLike;
  websocketFactory?: (url: string, protocols?: string[]) => WebSocketLike;
  defaultHeaders?: Record<string, string>;
}

interface EnrollResponseWire {
  enrolled: boolean;
  principal?: string | null;
  code?: string | null;
}

interface ChatSessionResponseWire {
  session: ChatSession;
}

interface ChatSessionListResponseWire {
  sessions: ChatSession[];
}

interface AuthSessionResponseWire {
  workspace: string;
}

const WS_CONNECTING = 0;
const WS_OPEN = 1;
const WS_CLOSED = 3;

function normalizeBaseUrl(baseUrl: string): string {
  return baseUrl.endsWith("/") ? baseUrl.slice(0, -1) : baseUrl;
}

function responseHeaders(
  defaultHeaders: Record<string, string>,
  bearerToken: string,
  initHeaders?: HeadersInit,
): HeadersInit {
  const headers = new Headers(initHeaders);
  headers.set("accept", "application/json");

  for (const [key, value] of Object.entries(defaultHeaders)) {
    headers.set(key, value);
  }

  headers.delete("X-Principal");
  headers.delete("X-Workspace");
  headers.set("Authorization", `Bearer ${bearerToken}`);

  return headers;
}

function toRealtimeUrl(baseUrl: string): string {
  const url = new URL("/api/magician/v2/realtime/ws", baseUrl);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  return url.toString();
}

async function parseEventData(data: unknown): Promise<string> {
  if (typeof data === "string") {
    return data;
  }

  if (data instanceof ArrayBuffer) {
    return new TextDecoder().decode(data);
  }

  if (ArrayBuffer.isView(data)) {
    return new TextDecoder().decode(
      data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength),
    );
  }

  if (typeof Blob !== "undefined" && data instanceof Blob) {
    return await data.text();
  }

  throw new Error("Unsupported WebSocket event payload");
}

function buildChatQuery(identity: ChannelIdentity): string {
  const params = new URLSearchParams({
    channel: identity.channelType,
    channel_address: identity.channelAddress,
  });

  if (identity.uiThreadId) {
    params.set("ui_thread_id", identity.uiThreadId);
  }

  // Only sent when an adapter set it (e.g. agentmail false for unauthenticated
  // mail); omitted ⇒ the backend treats the sender as verified.
  if (identity.channelVerified !== undefined) {
    params.set("channel_verified", String(identity.channelVerified));
  }

  // Only sent by prefix-gated control surfaces (for example Kapso `@magic`).
  // Omitted preserves legacy owner-vs-envoy routing.
  if (identity.controlIntent !== undefined) {
    params.set("control_intent", String(identity.controlIntent));
  }

  return params.toString();
}

function buildEnrollmentStatusQuery(identity: ChannelAddressIdentity): string {
  const params = new URLSearchParams({
    channel_type: identity.channelType,
    channel_address: identity.channelAddress,
  });

  return params.toString();
}

function defaultWebSocketFactory(url: string, protocols?: string[]): WebSocketLike {
  if (typeof WebSocket === "undefined") {
    throw new Error(
      "Global WebSocket is unavailable. Provide websocketFactory explicitly.",
    );
  }

  return new WebSocket(url, protocols);
}

export class MagicianHttpError extends Error {
  readonly status: number;
  readonly url: string;
  readonly responseBody: string;

  constructor(status: number, url: string, responseBody: string) {
    super(`Backend request failed (${status}) for ${url}`);
    this.name = "MagicianHttpError";
    this.status = status;
    this.url = url;
    this.responseBody = responseBody;
  }
}

export class MagicianRealtimeConnection {
  private readonly socket: WebSocketLike;

  constructor(socket: WebSocketLike) {
    this.socket = socket;
  }

  async waitForOpen(timeoutMs = 5_000): Promise<void> {
    if (this.socket.readyState === WS_OPEN) {
      return;
    }

    if (this.socket.readyState === WS_CLOSED) {
      throw new Error("Realtime socket is already closed.");
    }

    await new Promise<void>((resolve, reject) => {
      const onOpen = () => {
        cleanup();
        resolve();
      };

      const onError = () => {
        cleanup();
        reject(new Error("Realtime socket failed before opening."));
      };

      const onClose = () => {
        cleanup();
        reject(new Error("Realtime socket closed before opening."));
      };

      const timer = setTimeout(() => {
        cleanup();
        reject(new Error("Timed out waiting for realtime socket to open."));
      }, timeoutMs);

      const cleanup = () => {
        clearTimeout(timer);
        this.socket.removeEventListener("open", onOpen);
        this.socket.removeEventListener("error", onError);
        this.socket.removeEventListener("close", onClose);
      };

      this.socket.addEventListener("open", onOpen);
      this.socket.addEventListener("error", onError);
      this.socket.addEventListener("close", onClose);
    });
  }

  close(code?: number, reason?: string): void {
    this.socket.close(code, reason);
  }

  onClose(listener: () => void): void {
    const once = () => {
      this.socket.removeEventListener("close", once);
      listener();
    };
    this.socket.addEventListener("close", once);
  }
}

export class MagicianClient {
  private readonly baseUrl: string;
  private readonly fetchImpl: FetchLike;
  private readonly websocketFactory: (url: string, protocols?: string[]) => WebSocketLike;
  private readonly defaultHeaders: Record<string, string>;
  private readonly bearerToken: string;

  constructor(options: MagicianClientOptions) {
    this.baseUrl = normalizeBaseUrl(options.baseUrl);
    this.fetchImpl = options.fetchImpl ?? fetch;
    this.websocketFactory = options.websocketFactory ?? defaultWebSocketFactory;
    this.defaultHeaders = options.defaultHeaders ?? {};
    this.bearerToken = options.bearerToken.trim();
    if (!this.bearerToken) {
      throw new Error("MagicianClient requires a non-empty workspace-bound bearer token.");
    }
  }

  resourceHeaders(): Readonly<Record<string, string>> {
    return Object.freeze({ Authorization: `Bearer ${this.bearerToken}` });
  }

  async resolveBearerScope(): Promise<{ workspace: string }> {
    const response = await this.requestJson<AuthSessionResponseWire>(
      "/api/magician/v2/auth/session",
    );
    const workspace = response.workspace?.trim();
    if (!workspace) {
      throw new Error("Auth session response was missing the bearer workspace.");
    }
    return { workspace };
  }

  async enroll(request: EnrollRequest): Promise<EnrollResult> {
    const response = await this.requestJson<EnrollResponseWire>(
      "/api/magician/v2/chat/enroll",
      {
        method: "POST",
        headers: {
          "content-type": "application/json",
        },
        body: JSON.stringify({
          channel_type: request.channelType,
          channel_address: request.channelAddress,
          ...(request.displayName
            ? { display_name: request.displayName }
            : {}),
        }),
      },
    );

    if (response.enrolled) {
      if (!response.principal) {
        throw new Error("Enroll response was missing principal.");
      }

      return {
        status: "enrolled",
        principal: response.principal,
      };
    }

    return {
      status: "pending",
      code: response.code ?? null,
    };
  }

  async getEnrollmentStatus(
    identity: ChannelAddressIdentity,
  ): Promise<EnrollmentStatus> {
    const response = await this.requestJson<EnrollResponseWire>(
      `/api/magician/v2/chat/enroll/status?${buildEnrollmentStatusQuery(identity)}`,
    );

    if (response.enrolled) {
      if (!response.principal) {
        throw new Error("Enrollment status response was missing principal.");
      }

      return {
        status: "enrolled",
        principal: response.principal,
      };
    }

    if (response.code !== undefined && response.code !== null) {
      return {
        status: "pending",
        code: response.code,
      };
    }

    return { status: "unknown" };
  }

  async getActiveSession(identity: ChannelIdentity): Promise<ChatSessionDetail> {
    return this.requestJson<ChatSessionDetail>(
      `/api/magician/v2/chat/active?${buildChatQuery(identity)}`,
    );
  }

  async newSession(identity: ChannelIdentity): Promise<ChatSession> {
    const response = await this.requestJson<ChatSessionResponseWire>(
      `/api/magician/v2/chat/new?${buildChatQuery(identity)}`,
      {
        method: "POST",
      },
    );

    return response.session;
  }

  async listSessions(identity: ChannelIdentity): Promise<ChatSession[]> {
    const response = await this.requestJson<ChatSessionListResponseWire>(
      `/api/magician/v2/chat/sessions?${buildChatQuery(identity)}`,
    );

    return response.sessions;
  }

  async getSession(
    sessionId: string,
    identity: ChannelIdentity,
  ): Promise<ChatSessionDetail> {
    return this.requestJson<ChatSessionDetail>(
      `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}?${buildChatQuery(identity)}`,
    );
  }

  async sendMessage(
    sessionId: string,
    identity: ChannelIdentity,
    text: string,
    options?: ChannelMessageDispatchOptions,
  ): Promise<ChatResponse> {
    return this.requestJson<ChatResponse>(
      `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/messages?${buildChatQuery(identity)}`,
      {
        method: "POST",
        headers: {
          "content-type": "application/json",
        },
        body: JSON.stringify({
          text,
          ...(options?.profile ? { profile: options.profile } : {}),
          ...(options?.sourceSurface
            ? { source_surface: options.sourceSurface }
            : {}),
          ...(options?.presenceSessionId
            ? { presence_session_id: options.presenceSessionId }
            : {}),
          ...(options?.senderDisplayName
            ? { sender_display_name: options.senderDisplayName }
            : {}),
        }),
      },
    );
  }

  async reportEnvoyDelivery(sessionId: string, messageId: string, phase: "begin" | "provider_accepted" | "unknown", binding?: EnvoyDeliveryBinding):
    Promise<{ tracked: boolean; send: boolean; status: string }> {
    const grant = await this.requestJson<{ tracked: boolean; send: boolean; status: string }>(`/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/messages/${encodeURIComponent(messageId)}/envoy-delivery`, {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ phase, binding }),
      // This short acknowledgement must not hang the receipt recovery queue.
      // The signal covers both response headers and body consumption; retries
      // retain the same attempt binding and never repeat an external send.
      signal: AbortSignal.timeout(30_000),
    });
    if (!grant || typeof grant.tracked !== "boolean" || typeof grant.send !== "boolean" || typeof grant.status !== "string") {
      throw new Error("Invalid Envoy delivery grant; refusing to send without a definite answer");
    }
    return grant;
  }

  /**
   * Cancel the in-flight chat turn for `sessionId`. Drops partial output
   * (per backend decision #1). Does NOT clear the pending-message queue
   * — the next queued message drains as soon as the cancelled turn
   * settles. Returns `cancelled: false` when no turn is in flight.
   */
  async cancelChatRun(
    sessionId: string,
    identity: ChannelIdentity,
  ): Promise<CancelChatRunResponse> {
    return this.requestJson<CancelChatRunResponse>(
      `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/run?${buildChatQuery(identity)}`,
      { method: "DELETE" },
    );
  }

  /**
   * List pending-replay messages for `sessionId` in FIFO order. Drives the
   * SDK's drain loop and the unified-UI "N queued" indicator.
   */
  async listQueuedMessages(
    sessionId: string,
    identity: ChannelIdentity,
  ): Promise<ListQueuedMessagesResponse> {
    return this.requestJson<ListQueuedMessagesResponse>(
      `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/queue?${buildChatQuery(identity)}`,
    );
  }

  /**
   * Remove one queued message by id. Idempotent — returns
   * `deleted: false` when the id is unknown (already drained or never
   * enqueued).
   */
  async deleteQueuedMessage(
    sessionId: string,
    messageId: string,
    identity: ChannelIdentity,
  ): Promise<DeleteQueuedMessageResponse> {
    return this.requestJson<DeleteQueuedMessageResponse>(
      `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/queue/${encodeURIComponent(messageId)}?${buildChatQuery(identity)}`,
      { method: "DELETE" },
    );
  }

  /**
   * Clear the entire pending-replay queue for `sessionId`. Returns the
   * number of messages dropped.
   */
  async clearQueuedMessages(
    sessionId: string,
    identity: ChannelIdentity,
  ): Promise<ClearQueuedMessagesResponse> {
    return this.requestJson<ClearQueuedMessagesResponse>(
      `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/queue?${buildChatQuery(identity)}`,
      { method: "DELETE" },
    );
  }

  connectRealtime(
    listener: (event: MagicianRealtimeEvent) => void | Promise<void>,
  ): MagicianRealtimeConnection {
    // Two subprotocols, and the order matters. The runtime negotiates against
    // its own list (`magician-events-v2`) and echoes the one it selected; the
    // bearer rides as a second protocol because a browser cannot set an
    // Authorization header on a WebSocket, and the runtime reads it from here
    // without ever echoing it back. Offering the bearer alone leaves the
    // server with nothing it recognises, so it selects none and omits
    // `Sec-WebSocket-Protocol` from the 101 — and a strict client (Node's
    // built-in WebSocket included) must then fail the connection. That is a
    // silent failure: the socket never opens, so a bot receives no critical
    // request alert and claims no delivery.
    const socket = this.websocketFactory(toRealtimeUrl(this.baseUrl), [
      "magician-events-v2",
      `magician-bearer.${this.bearerToken}`,
    ]);
    socket.addEventListener("message", (event: Event) => {
      void this.handleRealtimeMessage(event as MessageEvent<unknown>, listener);
    });

    return new MagicianRealtimeConnection(socket);
  }

  private async handleRealtimeMessage(
    event: MessageEvent<unknown>,
    listener: (event: MagicianRealtimeEvent) => void | Promise<void>,
  ): Promise<void> {
    const payload = await parseEventData(event.data);
    const parsed = JSON.parse(payload) as MagicianRealtimeEvent;
    await listener(parsed);
  }

  /**
   * Claim one critical-request delivery (secure HITL plan §6.1). The backend
   * checks the bearer is this channel's bot and the delivery is still queued,
   * then hands over the owner address for exactly this send.
   */
  async claimCriticalDelivery(
    deliveryId: string,
    channelType: string,
    connectionGeneration: string,
  ): Promise<CriticalDeliveryClaim> {
    return this.requestJson<CriticalDeliveryClaim>(
      `/api/magician/v2/hitl/deliveries/${encodeURIComponent(deliveryId)}/claim`,
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          channel_type: channelType,
          connection_generation: connectionGeneration,
        }),
      },
    );
  }

  async reportCriticalDelivery(
    deliveryId: string,
    report: CriticalDeliveryReport,
  ): Promise<void> {
    await this.requestJson<{ delivery_id: string; state: string }>(
      `/api/magician/v2/hitl/deliveries/${encodeURIComponent(deliveryId)}/report`,
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(report),
      },
    );
  }

  private async requestJson<T>(
    path: string,
    init?: RequestInit,
  ): Promise<T> {
    const url = `${this.baseUrl}${path}`;
    const response = await this.fetchImpl(url, {
      ...init,
      headers: responseHeaders(this.defaultHeaders, this.bearerToken, init?.headers),
    });
    const responseText = await response.text();

    if (!response.ok) {
      throw new MagicianHttpError(response.status, url, responseText);
    }

    if (!responseText) {
      throw new Error(`The backend returned an empty response for ${url}`);
    }

    return JSON.parse(responseText) as T;
  }
}
