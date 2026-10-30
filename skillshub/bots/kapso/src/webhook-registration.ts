import { randomBytes } from "node:crypto";

/**
 * Kapso number-webhook self-registration.
 *
 * The public webhook URL follows this machine's tunnel hostname
 * (MAGICIAN_WEBHOOK_HOST), so a machine move or a fresh setup re-points the
 * number on the next bot start instead of stranding it on a dead hostname.
 * The provider's registration is authoritative for the signing secret.
 */

export const KAPSO_PLATFORM_BASE_URL = "https://api.kapso.ai/platform/v1";
const RECEIVED_EVENT = "whatsapp.message.received";
const KAPSO_WEBHOOK_PATH = "/webhook";
const REQUEST_TIMEOUT_MS = 20_000;
const PER_PAGE = 100;
const MAX_PAGES = 20;

export interface KapsoWebhookRecord {
  id: string;
  url?: string;
  kind?: string | null;
  active?: boolean | null;
  buffer_enabled?: boolean | null;
  events?: string[] | null;
  payload_version?: string | null;
  secret_key?: string | null;
}

export interface EnsureKapsoWebhookOptions {
  apiKey: string;
  phoneNumberId: string;
  /** Full public URL Kapso should deliver to, e.g. https://webhook-<machine>.<zone>/webhook. */
  url: string;
  /** Operator-configured secret; used only when a new registration needs one. */
  configuredSecret?: string | undefined;
  baseUrl?: string | undefined;
}

export interface KapsoWebhookRegistration {
  webhookId: string;
  /** Signing secret of the registration this receiver answers for. Never log it. */
  secret: string;
  created: boolean;
  updated: boolean;
  deactivated: Array<{ id: string; url: string }>;
}

/**
 * KAPSO_WEBHOOK_URL when set, else https://${MAGICIAN_WEBHOOK_HOST}/webhook.
 * The path matches the receiver this bot serves.
 */
export function resolveKapsoWebhookUrl(
  env: Record<string, string | undefined>,
): string | undefined {
  const explicit = env.KAPSO_WEBHOOK_URL?.trim();
  if (explicit) return explicit;
  const host = env.MAGICIAN_WEBHOOK_HOST?.trim()
    .replace(/^https?:\/\//i, "")
    .replace(/\/.*$/, "");
  return host ? `https://${host}${KAPSO_WEBHOOK_PATH}` : undefined;
}

interface NormalizedUrl {
  host: string;
  path: string;
  key: string;
}

function normalizeUrl(value: unknown): NormalizedUrl | undefined {
  if (typeof value !== "string" || !value.trim()) return undefined;
  try {
    const parsed = new URL(value.trim());
    const path = parsed.pathname.replace(/\/+$/, "") || "/";
    const host = parsed.hostname.toLowerCase();
    return { host, path, key: `${parsed.protocol}//${parsed.host.toLowerCase()}${path}` };
  } catch {
    return undefined;
  }
}

/** The zone the machine hostnames share: webhook-<machine>.<zone> -> <zone>. */
function parentDomain(host: string): string | undefined {
  const labels = host.split(".");
  return labels.length >= 3 ? labels.slice(1).join(".") : undefined;
}

class KapsoPlatform {
  constructor(
    private readonly apiKey: string,
    private readonly baseUrl: string,
  ) {}

  async request(method: string, path: string, body?: unknown): Promise<Response> {
    const response = await fetch(`${this.baseUrl}${path}`, {
      method,
      headers: {
        "X-API-Key": this.apiKey,
        Accept: "application/json",
        ...(body === undefined ? {} : { "Content-Type": "application/json" }),
      },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
      redirect: "error",
    });
    // Status only: an error body is provider text and may echo request fields.
    if (!response.ok) throw new Error(`Kapso ${method} webhooks failed (${response.status})`);
    return response;
  }
}

function recordOf(value: unknown): KapsoWebhookRecord | undefined {
  if (!value || typeof value !== "object") return undefined;
  const record = value as Record<string, unknown>;
  return typeof record.id === "string" && record.id ? (record as unknown as KapsoWebhookRecord) : undefined;
}

async function listWebhooks(platform: KapsoPlatform, collection: string): Promise<KapsoWebhookRecord[]> {
  const out: KapsoWebhookRecord[] = [];
  for (let page = 1; page <= MAX_PAGES; page += 1) {
    const response = await platform.request("GET", `${collection}?per_page=${PER_PAGE}&page=${page}`);
    const body: unknown = await response.json();
    const data = (body as { data?: unknown } | null)?.data;
    if (!Array.isArray(data)) throw new Error("Kapso webhook list response has no data array");
    for (const item of data) {
      const record = recordOf(item);
      if (record) out.push(record);
    }
    const totalPages = Number(response.headers.get("X-Total-Pages"));
    if (Number.isFinite(totalPages) && totalPages > 0 ? page >= totalPages : data.length < PER_PAGE) break;
  }
  return out;
}

async function dataOf(response: Response): Promise<KapsoWebhookRecord | undefined> {
  const body: unknown = await response.json().catch(() => undefined);
  return recordOf((body as { data?: unknown } | null)?.data);
}

function generateSecret(): string {
  return randomBytes(32).toString("hex");
}

/**
 * Make the phone number deliver to `url`, unbuffered (the receiver reads one
 * `body.message` per request), and return that registration's signing secret.
 * Other active hooks for the number that point at `/webhook` on a sibling host
 * of the same zone (another Magician machine) are deactivated: one machine
 * answers a number, otherwise every reply goes out twice. Throws when the
 * number's own registration cannot be ensured; a failed deactivation is logged.
 */
export async function ensureKapsoWebhook(
  options: EnsureKapsoWebhookOptions,
): Promise<KapsoWebhookRegistration> {
  const ours = normalizeUrl(options.url);
  if (!ours) throw new Error("Kapso webhook URL is not a valid URL");
  const platform = new KapsoPlatform(options.apiKey, (options.baseUrl ?? KAPSO_PLATFORM_BASE_URL).replace(/\/+$/, ""));
  const collection = `/whatsapp/phone_numbers/${encodeURIComponent(options.phoneNumberId)}/webhooks`;
  const configured = options.configuredSecret?.trim() || undefined;

  // Raw Meta forwarders are someone else's integration; only Kapso-format
  // hooks are ours to adopt or retire.
  const hooks = (await listWebhooks(platform, collection)).filter((hook) => hook.kind !== "meta");
  const mine = hooks.filter((hook) => normalizeUrl(hook.url)?.key === ours.key);
  let primary = mine.find((hook) => hook.active === true) ?? mine[0];
  let created = false;
  let updated = false;

  if (primary) {
    const patch: Record<string, unknown> = {};
    if (primary.active !== true) patch.active = true;
    if (primary.buffer_enabled) patch.buffer_enabled = false;
    const events = Array.isArray(primary.events) ? primary.events : [];
    if (!events.includes(RECEIVED_EVENT)) patch.events = [...events, RECEIVED_EVENT];
    if (primary.payload_version && primary.payload_version !== "v2") patch.payload_version = "v2";
    if (!primary.secret_key?.trim()) patch.secret_key = configured ?? generateSecret();
    if (Object.keys(patch).length > 0) {
      const response = await platform.request(
        "PATCH",
        `${collection}/${encodeURIComponent(primary.id)}`,
        { whatsapp_webhook: patch },
      );
      const confirmed = await dataOf(response);
      if (confirmed && (confirmed.active === false || confirmed.buffer_enabled === true)) {
        throw new Error("Kapso did not confirm an active, unbuffered webhook");
      }
      // A response that omits or blanks the secret keeps the one already known.
      const secretKey = confirmed?.secret_key?.trim() || (patch.secret_key as string | undefined) || primary.secret_key;
      primary = { ...primary, ...patch, ...(confirmed ?? {}), secret_key: secretKey ?? null } as KapsoWebhookRecord;
      updated = true;
    }
  } else {
    // Kapso never generates a secret: the creator supplies it.
    const secretKey = configured ?? generateSecret();
    const response = await platform.request("POST", collection, {
      whatsapp_webhook: {
        url: options.url,
        kind: "kapso",
        events: [RECEIVED_EVENT],
        active: true,
        buffer_enabled: false,
        payload_version: "v2",
        secret_key: secretKey,
      },
    });
    const confirmed = await dataOf(response);
    if (!confirmed) throw new Error("Kapso did not return the created webhook");
    primary = { ...confirmed, secret_key: confirmed.secret_key?.trim() || secretKey };
    created = true;
  }

  const secret = primary.secret_key?.trim();
  if (!secret) throw new Error("Kapso webhook registration has no signing secret");

  const parent = parentDomain(ours.host);
  const deactivated: Array<{ id: string; url: string }> = [];
  for (const hook of hooks) {
    if (hook.id === primary.id || hook.active !== true) continue;
    const other = normalizeUrl(hook.url);
    if (!other) continue;
    const duplicate = other.key === ours.key;
    const sibling = !!parent && other.host !== ours.host && other.host.endsWith(`.${parent}`)
      && (other.path === KAPSO_WEBHOOK_PATH || other.path === ours.path);
    if (!duplicate && !sibling) continue;
    const url = hook.url ?? "";
    try {
      await platform.request("PATCH", `${collection}/${encodeURIComponent(hook.id)}`, {
        whatsapp_webhook: { active: false },
      });
      deactivated.push({ id: hook.id, url });
      console.info("[kapso-bot] deactivated another Magician webhook for this number", {
        webhookId: hook.id,
        url,
        reason: duplicate ? "duplicate of this machine's URL" : "another machine's host",
      });
    } catch (error) {
      console.warn("[kapso-bot] could not deactivate another webhook for this number", {
        webhookId: hook.id,
        url,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }

  return { webhookId: primary.id, secret, created, updated, deactivated };
}
