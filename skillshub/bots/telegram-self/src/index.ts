import { dirname, resolve } from "node:path";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { ChannelRuntime, MagicianClient } from "@magician/bot-sdk";
import type { ChannelIdentity } from "@magician/bot-sdk";

import { TelegramSelfAdapter } from "./adapter.js";

function requiredEnv(name: string): string {
  const value = process.env[name]?.trim();
  if (!value) throw new Error(`Missing required environment variable: ${name}`);
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

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

// Resolve tgcli binary: local node_modules first, then the scoped workspace root
function resolveTgcliBinary(): string {
  const envBinary = process.env.TGCLI_BINARY?.trim();
  if (envBinary) {
    return envBinary;
  }

  const localBin = resolve(packageRoot, "node_modules", ".bin", "tgcli");
  if (existsSync(localBin)) {
    return localBin;
  }

  return resolve(packageRoot, "..", "..", "node_modules", ".bin", "tgcli");
}

const tgcliBinary = resolveTgcliBinary();
const store = process.env.TGCLI_STORE?.trim() || undefined;
const apiId = process.env.TGCLI_API_ID?.trim() || undefined;
const apiHash = process.env.TGCLI_API_HASH?.trim() || undefined;

const adapter = new TelegramSelfAdapter({ tgcliBinary, store, apiId, apiHash });
const magician = new MagicianClient({
  baseUrl: process.env.MAGICIAN_URL?.trim() || "http://127.0.0.1:3002",
  bearerToken: requiredEnv("MAGICIAN_BEARER_TOKEN"),
});
const runtime = new ChannelRuntime({
  channelType: "telegram-self",
  adapter,
  magician,
  enableRealtime: optionalBooleanEnv("MAGICIAN_REALTIME_ENABLED", true),
  connectedMessage: (identity: ChannelIdentity) =>
    `Connected. You're chatting as ${identity.principal}. Send a message to Saved Messages to begin.`,
});

let shutdownPromise: Promise<void> | undefined;

async function shutdown(signal: string): Promise<void> {
  if (!shutdownPromise) {
    console.info(`[telegram-self-bot] shutting down on ${signal}`);
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
console.info("[telegram-self-bot] started");
