import type { EventEmitter } from "node:events";

const ACK_EVENT = "CB:ack,class:message";
type Ack = { tag?: string; attrs?: { class?: string; id?: string; error?: string } };
type SentMessage = { key?: { id?: string | null } } | undefined;

/** Baileys sendMessage resolves after a websocket write, before WhatsApp's
 * acknowledgement. Listen before sending so a fast ACK cannot be missed. */
export async function sendWithServerAck(
  ws: Pick<EventEmitter, "on" | "off"> | undefined,
  send: () => Promise<SentMessage>,
  timeoutMs = 30_000,
): Promise<boolean> {
  if (!ws?.on || !ws.off) return false;
  let messageId: string | undefined;
  let settled = false;
  const early = new Map<string, boolean>();
  let resolveAck!: (accepted: boolean) => void;
  const answer = new Promise<boolean>((resolve) => { resolveAck = resolve; });
  const cleanup = () => {
    clearTimeout(timer);
    ws.off(ACK_EVENT, onAck);
    ws.off("close", onClose);
    ws.off("error", onClose);
    early.clear();
  };
  const finish = (accepted: boolean) => {
    if (settled) return;
    settled = true;
    cleanup();
    resolveAck(accepted);
  };
  const onClose = () => finish(false);
  const onAck = (node: Ack) => {
    const id = node?.attrs?.id;
    if (node?.tag !== "ack" || node.attrs?.class !== "message" || !id || settled) return;
    const accepted = !node.attrs.error;
    if (messageId) {
      if (id === messageId) finish(accepted);
    } else {
      // Bound transient state while sendMessage has not returned its ID.
      if (early.size >= 256 && !early.has(id)) { finish(false); return; }
      early.set(id, accepted && early.get(id) !== false);
    }
  };
  const timer = setTimeout(() => finish(false), timeoutMs);
  ws.on(ACK_EVENT, onAck);
  ws.on("close", onClose);
  ws.on("error", onClose);
  try {
    // The deadline and socket-close path must also release a sendMessage that
    // has not settled. It may still write later, so this is unknown, never a
    // reason to retry the send. Promise.race observes a late rejection too.
    const result = await Promise.race([
      send().then(sent => ({ sent })),
      answer.then(accepted => ({ accepted })),
    ]);
    if ("accepted" in result) return result.accepted;
    messageId = result.sent?.key?.id || undefined;
    if (!messageId) finish(false);
    else if (early.has(messageId)) finish(early.get(messageId)!);
    return await answer;
  } finally {
    cleanup();
  }
}
