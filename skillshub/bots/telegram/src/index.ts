import { ChannelRuntime, MagicianClient } from "@magician/bot-sdk";
import type { ChannelIdentity } from "@magician/bot-sdk";
import { Telegraf } from "telegraf";

import { TelegramAdapter } from "./adapter.js";

const DEFAULT_CONTROL_PREFIX = "/magic";
const LEGACY_CONTROL_PREFIX = "@magic";

function requiredEnv(name: string): string {
  const value = process.env[name]?.trim();
  if (!value) {
    throw new Error(`Missing required environment variable: ${name}`);
  }
  return value;
}

function optionalBooleanEnv(
  name: string,
  defaultValue: boolean,
): boolean {
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

function commandNameForControlPrefix(prefix: string): string | undefined {
  if (!prefix.startsWith("/")) {
    return undefined;
  }
  const command = prefix.slice(1).split(/\s+/, 1)[0]?.trim().toLowerCase();
  return command || undefined;
}

const controlPrefix =
  process.env.MAGICIAN_CONTROL_PREFIX?.trim() || DEFAULT_CONTROL_PREFIX;
const controlPrefixes = uniqueNonEmpty([
  controlPrefix,
  DEFAULT_CONTROL_PREFIX,
  LEGACY_CONTROL_PREFIX,
]);
const controlCommandNames = controlPrefixes
  .map(commandNameForControlPrefix)
  .filter((value): value is string => value !== undefined);

const bot = new Telegraf(requiredEnv("TELEGRAM_TOKEN"));
const adapter = new TelegramAdapter(bot, {
  dropPendingUpdates: optionalBooleanEnv(
    "TELEGRAM_DROP_PENDING_UPDATES",
    false,
  ),
  allowedCommandNames: controlCommandNames,
});
const magician = new MagicianClient({
  baseUrl: process.env.MAGICIAN_URL?.trim() || "http://127.0.0.1:3002",
  bearerToken: requiredEnv("MAGICIAN_BEARER_TOKEN"),
});
const runtime = new ChannelRuntime({
  channelType: "telegram",
  adapter,
  magician,
  enableRealtime: optionalBooleanEnv("MAGICIAN_REALTIME_ENABLED", true),
  controlPrefix,
  controlPrefixes: controlPrefixes.slice(1),
  nonControlMessageBehavior: "dispatch",
  channelThreading: "per-address",
  nonControlMessageDispatch: {
    sourceSurface: "telegram-envoy-chat",
  },
  connectedMessage: (identity: ChannelIdentity) =>
    `Connected. Send ${controlPrefix} followed by a request to enter private assistant mode.`,
});

let shutdownPromise: Promise<void> | undefined;

async function shutdown(signal: string): Promise<void> {
  if (!shutdownPromise) {
    console.info(`[telegram-bot] shutting down on ${signal}`);
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

await runtime.start();
console.info("[telegram-bot] started");
