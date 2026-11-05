import { readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

/**
 * `gws gmail +watch` without `--topic` creates a new Pub/Sub topic (and a
 * subscription) on every start and never deletes the topic. The supervisor
 * restarts the bot often, so a shared project fills up towards Pub/Sub's
 * 10,000-topics cap, after which every Gmail watcher fails. The bot keeps the
 * topic its first successful run created in the account's gws profile dir and
 * passes it back with `--topic`: gws then only creates a subscription and
 * renews the 7-day Gmail watch.
 *
 * A `--topic` run still creates a new subscription on every start, and none
 * is ever deleted (137 had piled up on four topics by 2026-11-03). So the
 * subscription the first run creates is saved beside the topic, and later
 * runs pass it with `--subscription`. That skips gws's setup, including the
 * Gmail watch renewal, so the bot renews the watch itself (`users.watch` on
 * the saved topic) before each such run and daily while it runs.
 */
export const WATCH_TOPIC_STATE_FILE = "gmail-watch-topic.json";

export interface SavedWatchTopic {
  topic: string;
  project: string;
  /** The subscription a `--topic` run created on `topic`, reused with `--subscription`. */
  subscription?: string;
  saved_at: string;
}

const TOPIC_NAME = /^projects\/([^/\s]+)\/topics\/([^/\s]+)$/;
const SUBSCRIPTION_NAME = /^projects\/([^/\s]+)\/subscriptions\/([^/\s]+)$/;
const CREATED_TOPIC_LINE = /Creating Pub\/Sub topic:\s*(projects\/[^/\s]+\/topics\/[^/\s"']+)/;
const CREATED_SUBSCRIPTION_LINE =
  /Creating Pub\/Sub subscription:\s*(projects\/[^/\s]+\/subscriptions\/[^/\s"']+)/;
const WATCH_ACTIVE_LINE = /Gmail watch active\b/;

/**
 * Google's not-found verdict. Permission, quota and network errors never
 * match, so they keep the saved topic.
 */
const NOT_FOUND = /NOT_FOUND|"code"\s*:\s*404\b|\(404\b|\b404 Not Found\b|Resource not found|entity was not found/i;

/** The topic gws announced it is creating, from one line of its output. */
export function parseCreatedTopic(line: string): string | undefined {
  return line.match(CREATED_TOPIC_LINE)?.[1];
}

/** The subscription gws announced it is creating, from one line of its output. */
export function parseCreatedSubscription(line: string): string | undefined {
  return line.match(CREATED_SUBSCRIPTION_LINE)?.[1];
}

/** gws prints this once the Pub/Sub setup and the Gmail `users.watch` call succeeded. */
export function isWatchActiveLine(line: string): boolean {
  return WATCH_ACTIVE_LINE.test(line);
}

/** The project segment of a full `projects/<p>/topics/<name>` topic name. */
export function topicProject(topic: string): string | undefined {
  return topic.match(TOPIC_NAME)?.[1];
}

export function parseSavedWatchTopic(raw: string): SavedWatchTopic | undefined {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return undefined;
  }
  if (typeof parsed !== "object" || parsed === null) return undefined;
  const { topic, project, subscription, saved_at } = parsed as Record<string, unknown>;
  if (typeof topic !== "string" || typeof project !== "string") return undefined;
  if (topicProject(topic) !== project) return undefined;
  const state: SavedWatchTopic = { topic, project, saved_at: typeof saved_at === "string" ? saved_at : "" };
  // A subscription from another project is dropped; the topic is still usable.
  if (typeof subscription === "string" && subscription.match(SUBSCRIPTION_NAME)?.[1] === project) {
    state.subscription = subscription;
  }
  return state;
}

/**
 * The saved topic, when it belongs to the project this watcher runs against.
 * With no resolved project there is nothing to check it against, so gws
 * creates its own.
 */
export function reusableTopic(
  saved: SavedWatchTopic | undefined,
  projectId: string | undefined,
): string | undefined {
  if (!saved || !projectId) return undefined;
  return saved.project === projectId ? saved.topic : undefined;
}

/** The saved subscription, under the same rule as {@link reusableTopic}. */
export function reusableSubscription(
  saved: SavedWatchTopic | undefined,
  projectId: string | undefined,
): string | undefined {
  return reusableTopic(saved, projectId) ? saved?.subscription : undefined;
}

/**
 * Arguments for `gws`: an existing subscription when there is one (gws then
 * skips all setup), else the saved topic, else nothing (gws creates both).
 */
export function watchArgs(topic: string | undefined, subscription?: string): string[] {
  const args = ["gmail", "+watch", "--format", "json"];
  if (subscription) return [...args, "--subscription", subscription];
  return topic ? [...args, "--topic", topic] : args;
}

/** Arguments that renew the 7-day Gmail watch on `topic`, which `--subscription` runs skip. */
export function renewWatchArgs(topic: string): string[] {
  return [
    "gmail", "users", "watch",
    "--params", JSON.stringify({ userId: "me" }),
    "--json", JSON.stringify({ topicName: topic }),
  ];
}

/** A `--subscription` run failed because Google no longer has that subscription. */
export function isMissingSubscriptionError(text: string, subscription: string): boolean {
  if (!NOT_FOUND.test(text)) return false;
  const shortName = subscription.match(SUBSCRIPTION_NAME)?.[2];
  return text.includes(subscription) || (!!shortName && text.includes(shortName));
}

/**
 * A run that was handed `topic` failed because the topic no longer exists.
 * Pub/Sub's error names the topic by its short name; gws may also wrap it as
 * a failed subscription create, whose only missing resource is the topic.
 */
export function isMissingTopicError(text: string, topic: string): boolean {
  if (!NOT_FOUND.test(text)) return false;
  const shortName = topic.match(TOPIC_NAME)?.[2];
  return (
    text.includes(topic) ||
    (!!shortName && text.includes(shortName)) ||
    /Failed to create Pub\/Sub subscription/i.test(text)
  );
}

/**
 * Forget the saved topic only when this run used it, never got as far as an
 * active watch, and Google said the topic is gone.
 */
export function shouldForgetTopic(run: {
  reusedTopic: string | undefined;
  watchActive: boolean;
  errorText: string;
}): boolean {
  return (
    !!run.reusedTopic &&
    !run.watchActive &&
    isMissingTopicError(run.errorText, run.reusedTopic)
  );
}

/* ---------- state file in the gws profile dir ---------- */

export function watchTopicStatePath(configDir: string): string {
  return resolve(configDir, WATCH_TOPIC_STATE_FILE);
}

export function readSavedWatchTopic(configDir: string): SavedWatchTopic | undefined {
  let raw: string;
  try {
    raw = readFileSync(watchTopicStatePath(configDir), "utf8");
  } catch {
    return undefined;
  }
  return parseSavedWatchTopic(raw);
}

/** Write atomically (temp file + rename), readable only by the owner. */
export function saveWatchTopic(
  configDir: string,
  topic: string,
  now: Date = new Date(),
  subscription?: string,
): SavedWatchTopic | undefined {
  const project = topicProject(topic);
  if (!project) return undefined;
  const state: SavedWatchTopic = { topic, project, saved_at: now.toISOString() };
  if (subscription?.match(SUBSCRIPTION_NAME)?.[1] === project) {
    state.subscription = subscription;
  }
  const path = watchTopicStatePath(configDir);
  const tmp = `${path}.${process.pid}.tmp`;
  try {
    writeFileSync(tmp, `${JSON.stringify(state, null, 2)}\n`, { mode: 0o600 });
    renameSync(tmp, path);
  } catch (error) {
    rmSync(tmp, { force: true });
    throw error;
  }
  return state;
}

export function forgetWatchTopic(configDir: string): void {
  rmSync(watchTopicStatePath(configDir), { force: true });
}

/** Keep the saved topic, drop its subscription, so the next run creates one. */
export function forgetWatchSubscription(configDir: string): void {
  const saved = readSavedWatchTopic(configDir);
  if (saved?.subscription) {
    saveWatchTopic(configDir, saved.topic);
  }
}
