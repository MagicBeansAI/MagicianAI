import { createHash } from "node:crypto";
import { parseEmailMailbox, type ProviderDeliveryReceipt } from "@magician/bot-sdk";

export const DELIVERY_EVENT_TYPES = [
  "message.delivered", "message.bounced", "message.complained",
] as const;

function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Provider delivery event is not an object.");
  }
  return value as Record<string, unknown>;
}

function field(row: Record<string, unknown>, key: string): string {
  const value = row[key];
  if (typeof value !== "string" || !value.trim() || /[\u0000-\u001f]/u.test(value)) {
    throw new Error(`Provider delivery event has no usable ${key}.`);
  }
  return value.trim();
}

export function isDeliveryEvent(body: unknown): boolean {
  if (!body || typeof body !== "object") return false;
  return DELIVERY_EVENT_TYPES.some(type => (body as Record<string, unknown>).event_type === type);
}

/** Decode the documented AgentMail payload. Unknown bounce classes are
 * refused rather than guessed into a permanent suppression. */
export function deliveryReceipts(body: unknown, rawPayload: string): ProviderDeliveryReceipt[] {
  const event = object(body);
  const type = field(event, "event_type");
  const eventId = field(event, "event_id");
  let state: ProviderDeliveryReceipt["state"];
  let payload: Record<string, unknown>;
  if (type === "message.delivered") {
    payload = object(event.delivery);
    state = "delivered";
  } else if (type === "message.complained") {
    payload = object(event.complaint);
    state = "complained";
  } else if (type === "message.bounced") {
    payload = object(event.bounce);
    if (payload.type === "Permanent") state = "hard_bounce";
    else if (payload.type === "Transient") state = "soft_bounce";
    else throw new Error("Provider bounce class is unknown; no suppression may be inferred.");
  } else {
    throw new Error("Unsupported provider delivery event.");
  }
  const messageId = field(payload, "message_id");
  const inboxId = field(payload, "inbox_id");
  const observedAt = field(payload, "timestamp");
  if (!Number.isFinite(Date.parse(observedAt))) throw new Error("Provider observation time is invalid.");
  if (!Array.isArray(payload.recipients) || !payload.recipients.length) {
    throw new Error("Provider delivery event has no recipients.");
  }
  const recipients = payload.recipients.map(value => {
    const raw = type === "message.bounced" ? field(object(value), "address") : value;
    const identity = typeof raw === "string" ? parseEmailMailbox(raw) : null;
    if (!identity) throw new Error("Provider delivery event has an invalid recipient.");
    return identity;
  });
  const payloadHash = createHash("sha256").update(rawPayload).digest("hex");
  return [...new Set(recipients)].map(identity => ({
    provider: "agentmail",
    // A message sent to two recipients carries two independent outcomes.
    // Keep this key stable across events/states, and preserve the original
    // message id separately for scoped send correlation.
    provider_message_id: createHash("sha256").update(JSON.stringify([inboxId, messageId, identity])).digest("hex"),
    sent_message_id: messageId,
    identity,
    state,
    observed_at: observedAt,
    payload_ref: `agentmail:event:${encodeURIComponent(eventId)}:sha256:${payloadHash}`,
    source: "provider",
    raw_payload: rawPayload,
  }));
}
