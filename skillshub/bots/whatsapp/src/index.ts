import { ChannelRuntime, MagicianClient } from "@magician/bot-sdk";

import { WhatsAppAdapter } from "./adapter.js";

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

const adapter = new WhatsAppAdapter({
  // Personal WhatsApp self-chat is observation/history only. Do not forward it
  // into Magician chat; Kapso is the explicit `@magic` control surface.
  chatDisabled: true,
});
const magician = new MagicianClient({
  baseUrl: process.env.MAGICIAN_URL?.trim() || "http://127.0.0.1:3002",
  bearerToken: requiredEnv("MAGICIAN_BEARER_TOKEN"),
});
const runtime = new ChannelRuntime({
  channelType: "whatsapp",
  adapter,
  magician,
  enableRealtime: optionalBooleanEnv("MAGICIAN_REALTIME_ENABLED", true),
});

let shutdownPromise: Promise<void> | undefined;

async function shutdown(signal: string): Promise<void> {
  if (!shutdownPromise) {
    console.info(`[whatsapp-bot] shutting down on ${signal}`);
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
console.info("[whatsapp-bot] started");
