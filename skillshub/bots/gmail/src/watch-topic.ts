import { readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

/**
 * `gws gmail +watch` without `--topic` creates a new Pub/Sub topic (and a
 * subscription) on every start and never deletes the topic. The supervisor
 * restarts the bot often, so a shared project fills up towards Pub/Sub's
 * 10,000-topics cap, after which every Gmail watcher fails. The bot keeps the
 * topic its first successful run created in the account's gws profile dir and
 * passes it back with `--topic`: gws then only creates a subscription (which
 * expires after 31 idle days) and still renews the 7-day Gmail watch.
 * `--subscription` is not used because it skips that renewal.
 */
export const WATCH_TOPIC_STATE_FILE = "gmail-watch-topic.json";

export interface SavedWatchTopic {
  topic: string;
  project: string;
  saved_at: string;
}

const TOPIC_NAME = /^projects\/([^/\s]+)\/topics\/([^/\s]+)$/;
const CREATED_TOPIC_LINE = /Creating Pub\/Sub topic:\s*(projects\/[^/\s]+\/topics\/[^/\s"']+)/;
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
  const { topic, project, saved_at } = parsed as Record<string, unknown>;
  if (typeof topic !== "string" || typeof project !== "string") return undefined;
  if (topicProject(topic) !== project) return undefined;
  return { topic, project, saved_at: typeof saved_at === "string" ? saved_at : "" };
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

/** Arguments for `gws`, reusing `topic` when there is one. */
export function watchArgs(topic: string | undefined): string[] {
  const args = ["gmail", "+watch", "--format", "json"];
  return topic ? [...args, "--topic", topic] : args;
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
): SavedWatchTopic | undefined {
  const project = topicProject(topic);
  if (!project) return undefined;
  const state: SavedWatchTopic = { topic, project, saved_at: now.toISOString() };
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
