import { ChannelRuntime, MagicianClient } from "@magician/bot-sdk";
import type { ChannelIdentity } from "@magician/bot-sdk";

import { KapsoAdapter } from "./adapter.js";

const DEFAULT_CONTROL_PREFIX = "/magic";
const LEGACY_CONTROL_PREFIX = "@magic";

function requiredEnv(name: string): string {
  const value = process.env[name]?.trim();
  if (!value) {
    throw new Error(`Missing required environment variable: ${name}`);
  }
  return value;
}

function optionalBooleanEnv(name: string, defaultValue: boolean): boolean {
  const value = process.env[name]?.trim().toLowerCase();
  if (!value) {
    return defaultValue;
  }

  if (["1", "true", "yes", "on"].includes(value)) {
    return true;
  }

  if (["0", "false", "no", "off"].includes(value)) {
    return false;
  }

  throw new Error(`Invalid boolean value for ${name}: ${process.env[name]}`);
}

function uniqueNonEmpty(values: readonly string[]): string[] {
  const out: string[] = [];
  for (const value of values) {
    const trimmed = value.trim();
    if (trimmed && !out.includes(trimmed)) {
      out.push(trimmed);
    }
  }
  return out;
}

const fallbackTemplateName = process.env.KAPSO_FALLBACK_TEMPLATE_NAME?.trim();
const fallbackTemplateLang = process.env.KAPSO_FALLBACK_TEMPLATE_LANG?.trim();
const adapter = new KapsoAdapter({
  apiKey: requiredEnv("KAPSO_API_KEY"),
  phoneNumberId: requiredEnv("KAPSO_PHONE_NUMBER_ID"),
  webhookSecret: requiredEnv("KAPSO_WEBHOOK_SECRET"),
  webhookPort: Number(process.env.WEBHOOK_PORT?.trim() || "3010"),
  ...(fallbackTemplateName ? { fallbackTemplateName } : {}),
  ...(fallbackTemplateLang ? { fallbackTemplateLang } : {}),
});
const magician = new MagicianClient({
  baseUrl: process.env.MAGICIAN_URL?.trim() || "http://127.0.0.1:3002",
  bearerToken: requiredEnv("MAGICIAN_BEARER_TOKEN"),
});
const controlPrefix =
  process.env.MAGICIAN_CONTROL_PREFIX?.trim() || DEFAULT_CONTROL_PREFIX;
const controlPrefixes = uniqueNonEmpty([
  controlPrefix,
  DEFAULT_CONTROL_PREFIX,
  LEGACY_CONTROL_PREFIX,
]);
const runtime = new ChannelRuntime({
  channelType: "kapso",
  adapter,
  magician,
  enableRealtime: optionalBooleanEnv("MAGICIAN_REALTIME_ENABLED", true),
  controlPrefix,
  controlPrefixes: controlPrefixes.slice(1),
  nonControlMessageBehavior: "dispatch",
  channelThreading: "per-address",
  nonControlMessageDispatch: {
    sourceSurface: "kapso-envoy-chat",
    allowOutsideWindowTemplate: false,
  },
  connectedMessage: (identity: ChannelIdentity) =>
    `Connected. Send ${controlPrefix} followed by a request to enter private assistant mode.`,
});

let shutdownPromise: Promise<void> | undefined;

async function shutdown(signal: string): Promise<void> {
  if (!shutdownPromise) {
    console.info(`[kapso-bot] shutting down on ${signal}`);
    shutdownPromise = runtime.stop();
  }

  await shutdownPromise;
}

process.once("SIGINT", () => {
  void shutdown("SIGINT");
});

process.once("SIGTERM", () => {
  void shutdown("SIGTERM");
});

// A webhook bot must survive a single message's processing failure. The webhook
// handler already try/catches, but a stray UNHANDLED REJECTION from a downstream
// async path — the realtime stream, the Kapso client, or a failing magician
// dispatch — would otherwise crash the process (Node ≥15 exits on
// unhandledRejection) and the supervisor would restart the bot on every test
// message. Log it and keep serving. (uncaughtException is intentionally left to
// crash+restart: an uncaught throw can leave the process in an undefined state.)
process.on("unhandledRejection", (reason) => {
  console.error("[kapso-bot] unhandledRejection (kept alive)", reason);
});

await runtime.start();
console.info("[kapso-bot] started");
