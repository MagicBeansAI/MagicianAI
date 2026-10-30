/**
 * The desktop Settings page's categories, cards and search keywords ("This
 * Mac" tab). Shared Magician settings live in web Settings, shown in the same
 * window's other tab. Mirrors unified-ui's `settingsIndex.ts`: a card stays
 * visible while searching when every query word appears in its title, its
 * keywords or its rendered text.
 */

/**
 * @typedef {{ id: string, title: string, keywords: string[] }} SettingsCard
 * @typedef {{ id: string, label: string, cards: SettingsCard[] }} SettingsCategory
 * @typedef {{ id: string, label: string, count: number }} SettingsNavCategory
 */

/** @type {SettingsCategory[]} */
export const DESKTOP_SETTINGS_CATEGORIES = [
  {
    id: "privacy",
    label: "Permissions & privacy",
    cards: [
      {
        id: "permissions",
        title: "Desktop permissions",
        keywords: ["accessibility", "microphone", "screen recording", "input monitoring", "full disk access", "automation", "permission"],
      },
      {
        id: "app-observation",
        title: "macOS app observation",
        keywords: ["apps", "observe", "pairing", "android", "approval", "macos"],
      },
      {
        id: "app-memory",
        title: "App memory contributions",
        keywords: ["memory", "apps", "promote", "contributions"],
      },
      {
        id: "imessage",
        title: "iMessage access",
        keywords: ["imessage", "messages", "sms", "full disk access"],
      },
      {
        id: "screen-capture",
        title: "Screen capture",
        keywords: ["screenshot", "screen", "capture", "region", "shortcut"],
      },
      {
        id: "audio-privacy",
        title: "Audio privacy and diagnostics",
        keywords: ["audio", "microphone", "recording", "playback", "privacy", "diagnostics", "speech"],
      },
    ],
  },
  {
    id: "orb",
    label: "Orb & assist",
    cards: [
      {
        id: "ambient-orb",
        title: "Ambient Orb",
        keywords: ["orb", "wake word", "voice", "realtime", "live", "thread", "conversation", "battery", "notch"],
      },
      {
        id: "contextual-assist",
        title: "Contextual Assist",
        keywords: ["selection", "writing", "rewrite", "option key", "personality", "assist", "popup"],
      },
    ],
  },
  {
    id: "engine",
    label: "Engine & connection",
    cards: [
      {
        id: "signed-in-scope",
        title: "Signed-in scope",
        keywords: ["account", "workspace", "sign in", "sign out", "scope", "login"],
      },
      {
        id: "network",
        title: "Network",
        keywords: ["ports", "port", "remote", "route", "gateway", "host", "tunnel", "connection"],
      },
      {
        id: "resources",
        title: "Resources",
        keywords: ["cpu", "memory", "ram", "container", "limits"],
      },
    ],
  },
  {
    id: "general",
    label: "General",
    cards: [
      {
        id: "general-options",
        title: "General",
        keywords: ["launch at login", "startup", "notifications", "sleep", "container image", "data folder", "overlay", "gesture", "shortcut", "double tap", "option"],
      },
      {
        id: "updates",
        title: "Updates",
        keywords: ["update", "version", "release", "upgrade"],
      },
      {
        id: "uninstall",
        title: "Uninstall",
        keywords: ["uninstall", "remove", "delete", "reset"],
      },
    ],
  },
  {
    id: "keys",
    label: "Keys & environment",
    cards: [
      {
        id: "key-mappings",
        title: "Key mappings",
        keywords: ["hotkeys", "shortcuts", "keys", "triggers", "commands", "keyboard"],
      },
      {
        id: "env-map",
        title: "Environment mapping",
        keywords: ["environment", "env", "variables", "mapping"],
      },
      {
        id: "environment",
        title: "Environment",
        keywords: ["environment", "env", "variables", "api key", "secrets", "credentials"],
      },
    ],
  },
];

/** @param {string} query */
export function normalizeSettingsQuery(query) {
  return query.trim().toLowerCase().replace(/\s+/g, " ");
}

/**
 * @param {SettingsCard} card
 * @param {string} query
 * @param {string} [renderedText]
 */
export function cardMatches(card, query, renderedText = "") {
  const normalized = normalizeSettingsQuery(query);
  if (!normalized) return true;
  const haystack = [card.title, ...card.keywords, renderedText].join(" ").toLowerCase();
  return normalized.split(" ").every((word) => haystack.includes(word));
}

/**
 * The category a URL hash or card id points at.
 * @param {string} hash
 * @returns {string | null}
 */
export function categoryForHash(hash) {
  const id = decodeURIComponent(hash.replace(/^#/, "")).trim();
  if (!id) return null;
  for (const category of DESKTOP_SETTINGS_CATEGORIES) {
    if (category.id === id || category.cards.some((card) => card.id === id)) return category.id;
  }
  return null;
}
