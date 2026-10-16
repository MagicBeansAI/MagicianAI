import { dirname, resolve } from "node:path";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { ChannelRuntime, MagicianClient } from "@magician/bot-sdk";
import type { ChannelIdentity } from "@magician/bot-sdk";

import { GmailAdapter } from "./adapter.js";

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

// Resolve gws binary: local node_modules first, then the scoped workspace root
function resolveGwsBinary(): string {
  const envBinary = process.env.GWS_BINARY?.trim();
  if (envBinary) {
    return envBinary;
  }

  const localBin = resolve(packageRoot, "node_modules", ".bin", "gws");
  if (existsSync(localBin)) {
    return localBin;
  }

  return resolve(packageRoot, "..", "..", "node_modules", ".bin", "gws");
}

const gwsBinary = resolveGwsBinary();
const configDir = process.env.GWS_CONFIG_DIR?.trim() || undefined;

const adapter = new GmailAdapter({ gwsBinary, configDir });
const magician = new MagicianClient({
  baseUrl: process.env.MAGICIAN_URL?.trim() || "http://127.0.0.1:3002",
  bearerToken: requiredEnv("MAGICIAN_BEARER_TOKEN"),
});
const runtime = new ChannelRuntime({
  channelType: "gmail",
  adapter,
  magician,
  enableRealtime: optionalBooleanEnv("MAGICIAN_REALTIME_ENABLED", true),
  connectedMessage: (identity: ChannelIdentity) =>
    `Connected. You're chatting as ${identity.principal}. Send an email to begin.`,
});

let shutdownPromise: Promise<void> | undefined;

async function shutdown(signal: string): Promise<void> {
  if (!shutdownPromise) {
    console.info(`[gmail-bot] shutting down on ${signal}`);
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
console.info("[gmail-bot] started");
