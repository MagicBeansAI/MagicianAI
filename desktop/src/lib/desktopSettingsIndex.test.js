import assert from "node:assert/strict";
import test from "node:test";

import {
  DESKTOP_SETTINGS_CATEGORIES,
  cardMatches,
  categoryForHash,
  normalizeSettingsQuery,
} from "./desktopSettingsIndex.js";

const card = (id) =>
  DESKTOP_SETTINGS_CATEGORIES.flatMap((category) => category.cards).find((entry) => entry.id === id);

test("category and card ids are unique and never collide", () => {
  const categoryIds = DESKTOP_SETTINGS_CATEGORIES.map((category) => category.id);
  const cardIds = DESKTOP_SETTINGS_CATEGORIES.flatMap((category) => category.cards.map((entry) => entry.id));
  assert.equal(new Set(categoryIds).size, categoryIds.length);
  assert.equal(new Set(cardIds).size, cardIds.length);
  assert.deepEqual(cardIds.filter((id) => categoryIds.includes(id)), []);
});

test("search matches titles, keywords and rendered text, requiring every word", () => {
  assert.equal(normalizeSettingsQuery("  Wake   WORD "), "wake word");
  assert.equal(cardMatches(card("ambient-orb"), ""), true);
  assert.equal(cardMatches(card("ambient-orb"), "wake word"), true);
  assert.equal(cardMatches(card("general-options"), "launch login"), true);
  assert.equal(cardMatches(card("updates"), "orb"), false);
  assert.equal(cardMatches(card("network"), "8443", "Remote route on port 8443"), true);
});

test("hashes map to their category", () => {
  assert.equal(categoryForHash("#orb"), "orb");
  assert.equal(categoryForHash("#contextual-assist"), "orb");
  assert.equal(categoryForHash("#environment"), "keys");
  assert.equal(categoryForHash("#nope"), null);
  assert.equal(categoryForHash(""), null);
});
