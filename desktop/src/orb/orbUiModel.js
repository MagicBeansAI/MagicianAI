/** Pure UI decisions shared by the Svelte surfaces and their provider-free tests. */

/** @param {boolean} active @param {boolean} reducedMotion */
export function frameIntervalMs(active, reducedMotion) {
  if (reducedMotion) return 200;
  return active ? 1000 / 60 : 100;
}

/** @param {number} now @param {number} lastDraw @param {boolean} active @param {boolean} reducedMotion */
export function frameIsDue(now, lastDraw, active, reducedMotion) {
  return now - lastDraw >= frameIntervalMs(active, reducedMotion) - 0.5;
}

/** @param {number} level @param {number} perFrameFactor @param {number} elapsedMs */
export function decayedLevel(level, perFrameFactor, elapsedMs) {
  return level * Math.pow(perFrameFactor, Math.max(0, elapsedMs) / (1000 / 60));
}

/** @param {string | null} phase @param {string | null} paletteKey */
export function fallbackShouldAnimate(phase, paletteKey) {
  return phase !== null && phase !== "ended" && paletteKey !== "graphite";
}

/** @typedef {{ energy: number, tempo: number, breath: number, turbulence: number }} OrbMotionProfile */
/** @type {Readonly<OrbMotionProfile>} */
const STILL_MOTION = Object.freeze({ energy: 0, tempo: 0, breath: 0, turbulence: 0 });
/** @type {Readonly<Record<string, Readonly<OrbMotionProfile>>>} */
const ORB_MOTION_PROFILES = Object.freeze({
  armed: Object.freeze({ energy: 0.34, tempo: 0.34, breath: 0.82, turbulence: 0.34 }),
  heard: Object.freeze({ energy: 0.96, tempo: 1.24, breath: 0.72, turbulence: 0.82 }),
  listening: Object.freeze({ energy: 0.7, tempo: 0.72, breath: 0.56, turbulence: 0.7 }),
  thinking: Object.freeze({ energy: 0.84, tempo: 0.96, breath: 0.38, turbulence: 1 }),
  speaking: Object.freeze({ energy: 1, tempo: 1.34, breath: 0.68, turbulence: 0.76 }),
});

/**
 * Phase colors remain the source of lifecycle truth; this profile only gives
 * each truthful phase a distinct organic motion vocabulary.
 * @param {string | null | undefined} phase
 * @returns {{ energy: number, tempo: number, breath: number, turbulence: number }}
 */
export function orbMotionProfile(phase) {
  if (phase == null) return STILL_MOTION;
  return ORB_MOTION_PROFILES[phase] ?? STILL_MOTION;
}

/**
 * @typedef {{ role: "user" | "assistant", speaker_name: string, text: string, final_caption: boolean }} Caption
 * @param {Caption[]} captions
 * @param {Caption} caption
 * @returns {Caption[]}
 */
export function reduceCaptions(captions, caption) {
  const prior = captions.at(-1);
  if (prior && prior.role === caption.role && prior.speaker_name === caption.speaker_name && !prior.final_caption) {
    return [...captions.slice(0, -1), caption].slice(-3);
  }
  return [...captions, caption].slice(-3);
}

/**
 * Remove only the latest unfinished caption for a speaker. Finalized history
 * remains visible while a VAD boundary with no usable transcript disappears.
 * @param {Caption[]} captions
 * @param {"user" | "assistant"} role
 * @returns {Caption[]}
 */
export function clearUnfinishedCaption(captions, role) {
  const index = captions.findLastIndex(caption => caption.role === role && !caption.final_caption);
  if (index < 0) return captions;
  return [...captions.slice(0, index), ...captions.slice(index + 1)];
}

/** @template {{ revision: number }} T @param {T} current @param {T} candidate @returns {T} */
export function latestRevision(current, candidate) {
  return candidate.revision >= current.revision ? candidate : current;
}

/**
 * @param {Caption | null} caption
 * @returns {string}
 */
export function captionAnnouncement(caption) {
  return caption ? `${caption.speaker_name}: ${caption.text}` : "";
}

/**
 * Keep the empty-caption copy truthful to microphone ownership. Armed means
 * only the local wake spotter is active; conversational audio starts later.
 * @param {string} state
 * @param {string} status
 * @returns {string}
 */
export function orbInvitation(state, status) {
  if (state === "hold_ready") return "Hold Left ⌥ to talk. A Live connection stays ready between holds.";
  if (state === "armed" && status.startsWith("Hold ")) return "Hold Left ⌥ to talk. Double-tap it for Quick Automate.";
  if (state === "armed") return `${status} to start, or tap the Orb.`;
  if (state === "heard" || state === "connecting") return "Opening the conversation…";
  if (state === "thinking") return "Thinking about that…";
  if (state === "speaking") return "Speaking…";
  if (["off", "voice_busy", "paused", "disarming", "ended", "recoverable_error"].includes(state)) return status;
  return "Say what’s on your mind. I’m listening.";
}

/**
 * Resolve pointer click cardinality without coupling the native presentation
 * contract to Svelte timing. A single click expands or toggles radial actions;
 * the second click in the platform double-click sequence advances the native
 * presentation cycle.
 * @param {number} detail
 * @param {boolean} expanded
 * @returns {"cycle_presentation" | "expand_controls" | "toggle_controls"}
 */
export function orbTapIntent(detail, expanded) {
  if (detail >= 2) return "cycle_presentation";
  return expanded ? "toggle_controls" : "expand_controls";
}

/**
 * Advance through the three attention-cost surfaces. Resting is passive,
 * Expanded exposes quick controls/captions, and Spotlight owns a centered
 * conversation. Settling is already moving toward Resting, so another double
 * tap completes that direction rather than reversing the animation mid-flight.
 * @param {"hidden" | "resting" | "expanded" | "spotlight" | "settling"} mode
 * @returns {"orb_expand" | "orb_spotlight" | "orb_collapse"}
 */
export function orbCycleCommand(mode) {
  if (mode === "resting" || mode === "hidden") return "orb_expand";
  if (mode === "expanded") return "orb_spotlight";
  return "orb_collapse";
}

/**
 * @typedef {{ key: string, metaKey: boolean, ctrlKey: boolean, altKey: boolean, shiftKey: boolean }} ShortcutEventLike
 * @typedef {{ kind: "navigate" | "cancel" | "clear" | "modifier" | "error" | "record", value?: string }} ShortcutIntent
 * @param {ShortcutEventLike} event
 * @returns {ShortcutIntent}
 */
export function shortcutIntent(event) {
  const hasPrimaryModifier = event.metaKey || event.ctrlKey || event.altKey;
  if (event.key === "Tab" && !hasPrimaryModifier) return { kind: "navigate" };
  if (event.key === "Escape") return { kind: "cancel" };
  if (event.key === "Backspace" || event.key === "Delete") return { kind: "clear" };
  if (["Meta", "Control", "Alt", "Shift"].includes(event.key)) return { kind: "modifier" };
  if (!hasPrimaryModifier) return { kind: "error" };

  const modifiers = [
    event.metaKey ? "CmdOrCtrl" : "",
    event.ctrlKey && !event.metaKey ? "Control" : "",
    event.altKey ? "Alt" : "",
    event.shiftKey ? "Shift" : "",
  ].filter(Boolean);
  /** @type {Record<string, string>} */
  const aliases = {
    " ": "Space",
    ArrowUp: "ArrowUp",
    ArrowDown: "ArrowDown",
    ArrowLeft: "ArrowLeft",
    ArrowRight: "ArrowRight",
    Enter: "Enter",
  };
  const key = aliases[event.key] ?? (event.key.length === 1 ? event.key.toUpperCase() : event.key);
  return { kind: "record", value: [...modifiers, key].join("+") };
}
