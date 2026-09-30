import assert from "node:assert/strict";
import test from "node:test";
import {
  captionAnnouncement,
  clearUnfinishedCaption,
  fallbackShouldAnimate,
  decayedLevel,
  frameIntervalMs,
  frameIsDue,
  latestRevision,
  orbCycleCommand,
  orbInvitation,
  orbMotionProfile,
  orbTapIntent,
  reduceCaptions,
  shortcutIntent,
} from "./orbUiModel.js";

test("renderer is capped at 60fps while active and 10fps at rest", () => {
  assert.equal(frameIntervalMs(true, false), 1000 / 60);
  assert.equal(frameIntervalMs(false, false), 100);
  assert.equal(frameIntervalMs(true, true), 200);
  assert.equal(frameIsDue(8, 0, true, false), false);
  assert.equal(frameIsDue(17, 0, true, false), true);
});

test("audio envelope decay is elapsed-time normalized", () => {
  const frame = 1000 / 60;
  const twoFramesAtOnce = decayedLevel(1, 0.84, frame * 2);
  const twoSeparateFrames = decayedLevel(decayedLevel(1, 0.84, frame), 0.84, frame);
  assert.ok(Math.abs(twoFramesAtOnce - twoSeparateFrames) < Number.EPSILON * 4);
});

test("armed and conversational phases stay alive while graphite stays still", () => {
  assert.equal(fallbackShouldAnimate("speaking", "teal_speaking"), true);
  assert.equal(fallbackShouldAnimate("speaking", "graphite"), false);
  assert.equal(fallbackShouldAnimate("armed", "armed_ember"), true);
  assert.equal(fallbackShouldAnimate("ended", "graphite"), false);
});

test("each truthful orb phase has a bounded and distinct organic motion profile", () => {
  const armed = orbMotionProfile("armed");
  const heard = orbMotionProfile("heard");
  const listening = orbMotionProfile("listening");
  const thinking = orbMotionProfile("thinking");
  const speaking = orbMotionProfile("speaking");

  assert.ok(armed.energy > 0, "wake-ready presence must visibly breathe");
  assert.ok(heard.energy > listening.energy, "wake acknowledgement blooms");
  assert.ok(thinking.turbulence > listening.turbulence, "thinking folds inward");
  assert.ok(speaking.tempo > listening.tempo, "speech has a quicker pulse");
  for (const profile of [armed, heard, listening, thinking, speaking]) {
    for (const value of Object.values(profile)) assert.ok(value >= 0 && value <= 1.34);
  }
  assert.deepEqual(orbMotionProfile("ended"), {
    energy: 0, tempo: 0, breath: 0, turbulence: 0,
  });
});

test("streaming captions replace their draft and preserve only three turns", () => {
  const first = reduceCaptions([], { role: "assistant", speaker_name: "Sam", text: "Hel", final_caption: false });
  const replaced = reduceCaptions(first, { role: "assistant", speaker_name: "Sam", text: "Hello", final_caption: true });
  assert.deepEqual(replaced, [{ role: "assistant", speaker_name: "Sam", text: "Hello", final_caption: true }]);
  const bounded = [
    { role: "user", speaker_name: "You", text: "one", final_caption: true },
    { role: "assistant", speaker_name: "Sam", text: "two", final_caption: true },
    { role: "user", speaker_name: "You", text: "three", final_caption: true },
  ];
  assert.equal(reduceCaptions(bounded, { role: "assistant", speaker_name: "Sam", text: "four", final_caption: true }).length, 3);
});

test("caption cleanup removes only the matching unfinished turn", () => {
  const captions = [
    { role: "user", speaker_name: "You", text: "kept", final_caption: true },
    { role: "assistant", speaker_name: "Sam", text: "also kept", final_caption: true },
    { role: "user", speaker_name: "You", text: "draft", final_caption: false },
  ];
  assert.deepEqual(clearUnfinishedCaption(captions, "user"), captions.slice(0, 2));
  assert.equal(clearUnfinishedCaption(captions, "assistant"), captions);
});

test("rehydration snapshots cannot overwrite newer subscribed events", () => {
  const event = { revision: 9, state: "speaking" };
  const staleGetter = { revision: 8, state: "armed" };
  assert.equal(latestRevision(event, staleGetter), event);
  const freshGetter = { revision: 10, state: "listening" };
  assert.equal(latestRevision(event, freshGetter), freshGetter);
});

test("final captions have explicit speaker announcements", () => {
  const caption = { role: "assistant", speaker_name: "Sam", text: "Ready.", final_caption: true };
  assert.equal(captionAnnouncement(caption), "Sam: Ready.");
  assert.equal(captionAnnouncement({ ...caption, role: "user", speaker_name: "You" }), "You: Ready.");
  assert.equal(captionAnnouncement(null), "");
});

test("armed orb never claims conversation capture is listening", () => {
  assert.equal(
    orbInvitation("armed", "Say “hey assistant”"),
    "Say “hey assistant” to start, or tap the Orb.",
  );
  assert.equal(
    orbInvitation("armed", "Hold Left ⌥ to talk"),
    "Hold Left ⌥ to talk. Double-tap it for Quick Automate.",
  );
  assert.equal(
    orbInvitation("hold_ready", "Hold Left ⌥ to talk"),
    "Hold Left ⌥ to talk. A Live connection stays ready between holds.",
  );
  assert.equal(orbInvitation("connecting", "Connecting"), "Opening the conversation…");
  assert.equal(
    orbInvitation("recoverable_error", "Wake microphone unavailable"),
    "Wake microphone unavailable",
  );
  assert.equal(
    orbInvitation("listening", "Listening"),
    "Say what’s on your mind. I’m listening.",
  );
});

test("orb taps expand controls while double-click cycles all presentations", () => {
  assert.equal(orbTapIntent(1, false), "expand_controls");
  assert.equal(orbTapIntent(1, true), "toggle_controls");
  assert.equal(orbTapIntent(2, false), "cycle_presentation");
  assert.equal(orbTapIntent(2, true), "cycle_presentation");
  assert.equal(orbCycleCommand("resting"), "orb_expand");
  assert.equal(orbCycleCommand("expanded"), "orb_spotlight");
  assert.equal(orbCycleCommand("spotlight"), "orb_collapse");
  assert.equal(orbCycleCommand("settling"), "orb_collapse");
});

test("shortcut recorder preserves keyboard navigation and rejects bare keys", () => {
  const event = (key, overrides = {}) => ({
    key, metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...overrides,
  });
  assert.deepEqual(shortcutIntent(event("Tab")), { kind: "navigate" });
  assert.deepEqual(shortcutIntent(event("k")), { kind: "error" });
  assert.deepEqual(shortcutIntent(event("Shift", { shiftKey: true })), { kind: "modifier" });
  assert.deepEqual(shortcutIntent(event(" ", { altKey: true })), { kind: "record", value: "Alt+Space" });
  assert.deepEqual(shortcutIntent(event("k", { metaKey: true, shiftKey: true })), {
    kind: "record", value: "CmdOrCtrl+Shift+K",
  });
});
