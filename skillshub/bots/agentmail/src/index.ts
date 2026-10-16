/**
 * AgentMail envoy bot — receive inbound emails, forward to magician via the
 * shared ChannelRuntime, and reply back out as an email.
 *
 * Inbound `message.received` webhooks for the inbox the `AGENT_MAIL_KEY` is
 * scoped to (magican@agentmail.to) are parsed by `AgentMailAdapter` and handed
 * to `ChannelRuntime`, which enrolls the sender, runs the magician chat turn,
 * and routes the envoy's reply back through `adapter.sendText` — sent out as a
 * threaded reply email. Authorization (the owner allowlist) is enforced
 * downstream by the magician agentmail-processing skill, NOT in the adapter.
 */
import { ChannelRuntime, MagicianClient } from "@magician/bot-sdk";
import type { ChannelIdentity } from "@magician/bot-sdk";

import { AgentMailAdapter } from "./adapter.js";

function requiredEnv(name: string): string {
  const value = process.env[name]?.trim();
  if (!value) {
    throw new Error(`Missing required environment variable: ${name}`);
  }
  return value;
}

function optionalEnv(name: string): string | undefined {
  const value = process.env[name]?.trim();
  return value ? value : undefined;
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

const adapter = new AgentMailAdapter({
  apiKey: requiredEnv("AGENT_MAIL_KEY"),
  webhookPort: Number(process.env.WEBHOOK_PORT?.trim() || "3011"),
  webhookUrl: optionalEnv("AGENTMAIL_WEBHOOK_URL"),
  webhookSecret: optionalEnv("AGENTMAIL_WEBHOOK_SECRET"),
});
const magician = new MagicianClient({
  baseUrl: process.env.MAGICIAN_URL?.trim() || "http://127.0.0.1:3002",
  bearerToken: requiredEnv("MAGICIAN_BEARER_TOKEN"),
});
const runtime = new ChannelRuntime({
  channelType: "agentmail",
  adapter,
  magician,
  enableRealtime: optionalBooleanEnv("MAGICIAN_REALTIME_ENABLED", true),
  connectedMessage: (identity: ChannelIdentity) =>
    `Connected. You're corresponding as ${identity.principal}. Reply to this email to begin.`,
});

let shutdownPromise: Promise<void> | undefined;

async function shutdown(signal: string): Promise<void> {
  if (!shutdownPromise) {
    console.info(`[agentmail-bot] shutting down on ${signal}`);
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
console.info("[agentmail-bot] started");
