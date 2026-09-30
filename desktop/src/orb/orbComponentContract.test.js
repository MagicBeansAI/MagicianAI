import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { JSDOM } from "jsdom";
import { compile } from "svelte/compiler";

const surfaceSource = readFileSync(new URL("./OrbSurface.svelte", import.meta.url), "utf8");
const shaderSource = readFileSync(new URL("./OrbShader.svelte", import.meta.url), "utf8");
const orbDragCapability = JSON.parse(
  readFileSync(new URL("../../src-tauri/capabilities/orb-drag.json", import.meta.url), "utf8"),
);
const settingsSource = readFileSync(new URL("../lib/Settings.svelte", import.meta.url), "utf8");
const modelUrl = new URL("./orbUiModel.js", import.meta.url).href;
const svelteClientUrl = new URL("../../node_modules/svelte/src/index-client.js", import.meta.url).href;

function dataModule(source) {
  return `data:text/javascript;base64,${Buffer.from(source).toString("base64")}`;
}

function resolveSvelteImports(source) {
  return source.replace(/(["'])(svelte(?:\/[^"']+)?)\1/g, (_match, quote, specifier) =>
    `${quote}${specifier === "svelte" ? svelteClientUrl : import.meta.resolve(specifier)}${quote}`,
  );
}

function compileModule(source, filename) {
  return resolveSvelteImports(compile(source, { filename, generate: "client" }).js.code);
}

function compileOrbForTest() {
  const paletteUrl = dataModule(`
    const palette = {
      coreA: [0.2, 0.2, 0.2], coreB: [0.4, 0.4, 0.4],
      accent: [0.6, 0.6, 0.6], halo: [0.8, 0.8, 0.8],
      haloStrength: 0.5, rimStrength: 0.5, seed: 0,
    };
    export const paletteFor = () => palette;
  `);
  const shader = shaderSource
    .replace('"./auroraPalettes"', JSON.stringify(paletteUrl))
    .replace('"./orbUiModel.js"', JSON.stringify(modelUrl));
  const shaderUrl = dataModule(compileModule(shader, "OrbShader.svelte"));

  const tauriUrl = dataModule(`
    export const invoke = (...args) => globalThis.__orbTauri.invoke(...args);
    export const listen = (...args) => globalThis.__orbTauri.listen(...args);
    export const getCurrentWindow = () => ({
      startDragging: (...args) => Promise.resolve(globalThis.__orbTauri.startDragging?.(...args)),
    });
  `);
  const surface = surfaceSource
    .replace('"@tauri-apps/api/core"', JSON.stringify(tauriUrl))
    .replace('"@tauri-apps/api/event"', JSON.stringify(tauriUrl))
    .replace('"@tauri-apps/api/window"', JSON.stringify(tauriUrl))
    .replace('"./OrbShader.svelte"', JSON.stringify(shaderUrl))
    .replace('"./orbUiModel.js"', JSON.stringify(modelUrl));
  return dataModule(compileModule(surface, "OrbSurface.svelte"));
}

function deferred() {
  let resolve;
  const promise = new Promise(done => resolve = done);
  return { promise, resolve };
}

async function waitFor(predicate, description) {
  const deadline = Date.now() + 1_000;
  while (!predicate()) {
    if (Date.now() > deadline) throw new Error(`Timed out waiting for ${description}`);
    await new Promise(resolve => setTimeout(resolve, 0));
  }
}

function installDom() {
  const dom = new JSDOM("<!doctype html><html><body><div id=app></div></body></html>", {
    pretendToBeVisual: true,
    url: "http://localhost/orb",
  });
  for (const key of [
    "window", "document", "navigator", "Node", "Element", "HTMLElement",
    "HTMLCanvasElement", "SVGElement", "Text", "Comment", "Event",
    "CustomEvent", "KeyboardEvent", "MutationObserver", "getComputedStyle",
  ]) {
    Object.defineProperty(globalThis, key, {
      configurable: true,
      value: key === "window" || key === "document" || key === "navigator"
        ? dom.window[key]
        : dom.window[key],
      writable: true,
    });
  }
  window.matchMedia = () => ({
    matches: false,
    addEventListener() {},
    removeEventListener() {},
  });
  globalThis.requestAnimationFrame = callback => setTimeout(() => callback(performance.now()), 0);
  globalThis.cancelAnimationFrame = handle => clearTimeout(handle);
  globalThis.ResizeObserver = class {
    observe() {}
    disconnect() {}
  };
  if (dom.window.HTMLMediaElement) {
    globalThis.HTMLMediaElement = dom.window.HTMLMediaElement;
  }
  HTMLCanvasElement.prototype.getContext = () => null;
  return dom;
}

test("orb components compile through the real Svelte compiler", () => {
  assert.doesNotThrow(() => compileOrbForTest());
});

test("mounted surface preserves event truth and repeat-safe live announcements", async () => {
  const dom = installDom();
  const snapshot = deferred();
  const presentation = deferred();
  const listeners = new Map();
  const invocations = [];
  const listenerCountsAtRead = [];
  globalThis.__orbTauri = {
    async listen(name, handler) {
      listeners.set(name, handler);
      return () => listeners.delete(name);
    },
    async invoke(name) {
      invocations.push(name);
      if (name === "get_orb_snapshot") {
        listenerCountsAtRead.push(listeners.size);
        return snapshot.promise;
      }
      if (name === "get_orb_presentation") {
        listenerCountsAtRead.push(listeners.size);
        return presentation.promise;
      }
      return undefined;
    },
  };

  const originalWarn = console.warn;
  console.warn = () => undefined;
  const { default: OrbSurface } = await import(compileOrbForTest());
  // Node selects Svelte's SSR export by default. Import the browser entry
  // explicitly after jsdom is installed so this exercises the real DOM mount.
  const { mount, unmount } = await import(svelteClientUrl);
  const target = document.getElementById("app");
  const instance = mount(OrbSurface, { target });

  try {
    await waitFor(
      () => invocations.includes("get_orb_snapshot") && invocations.includes("get_orb_presentation"),
      "revisioned initial reads",
    );
    assert.equal(
      invocations.includes("orb_start_conversation"),
      false,
      "mounting an armed orb must never start conversation capture",
    );
    assert.deepEqual(listenerCountsAtRead, [7, 7], "all listeners precede both initial reads");

    listeners.get("orb://phase")({
      payload: {
        revision: 9, state: "speaking", phase: "speaking", palette_key: "teal_speaking",
        status: "Speaking", window_open: true, paused_until_ms: null,
        cooldown_until_ms: null, leash_deadline_ms: null, cap_expired_during_wake: false,
      },
    });
    listeners.get("orb://presentation")({
      payload: {
        mode: "spotlight", duration_ms: 580, notch: true, docked: false,
        notch_width_points: 150, notch_height_points: 32, revision: 9,
      },
    });
    snapshot.resolve({
      revision: 8, state: "armed", phase: "armed", palette_key: "armed_ember",
      status: "Armed", window_open: true, paused_until_ms: null,
      cooldown_until_ms: null, leash_deadline_ms: null, cap_expired_during_wake: false,
    });
    presentation.resolve({
      mode: "resting", duration_ms: 260, notch: true, docked: true,
      notch_width_points: 150, notch_height_points: 32, revision: 8,
    });

    await waitFor(
      () => target.querySelector("main")?.classList.contains("spotlight"),
      "runtime spotlight presentation",
    );
    assert.ok(target.querySelector('[data-palette="teal_speaking"]'));
    assert.ok(target.querySelector("main").classList.contains("expanded"));

    listeners.get("orb://presentation")({
      payload: {
        mode: "resting", duration_ms: 260, notch: true, docked: true,
        notch_width_points: 150, notch_height_points: 32, revision: 10,
      },
    });
    await waitFor(
      () => target.querySelector("main")?.classList.contains("docked"),
      "runtime notch docking presentation",
    );
    assert.ok(target.querySelector("[data-notch-surface]"));
    assert.match(target.querySelector("main").getAttribute("style"), /--notch-depth: 32px/);

    const liveRegion = target.querySelector("[data-orb-announcement]");
    assert.ok(liveRegion, "the polite live region is mounted before captions arrive");
    listeners.get("orb://caption")({
      payload: { role: "assistant", speaker_name: "Aria", text: "Ready.", final_caption: true },
    });
    await waitFor(() => liveRegion.textContent === "Aria: Ready.", "first final caption");

    const mutations = [];
    const observer = new MutationObserver(records => mutations.push(...records));
    observer.observe(liveRegion, {
      childList: true,
      characterData: true,
      characterDataOldValue: true,
      subtree: true,
    });
    listeners.get("orb://caption")({
      payload: { role: "assistant", speaker_name: "Aria", text: "Ready.", final_caption: true },
    });
    await waitFor(
      () => mutations.some(record => record.oldValue === "")
        && liveRegion.textContent === "Aria: Ready.",
      "clear then restore mutations for an identical caption",
    );
    assert.equal(target.querySelector("[data-orb-announcement]"), liveRegion);
    assert.ok(mutations.some(record => record.oldValue === "Aria: Ready."));
    assert.equal(target.querySelectorAll('[role="alert"]').length, 0);
    observer.disconnect();

    listeners.get("orb://presentation")({
      payload: {
        mode: "expanded", duration_ms: 360, notch: true, docked: true,
        notch_width_points: 150, notch_height_points: 32, revision: 11,
      },
    });
    listeners.get("orb://caption")({
      payload: { role: "user", speaker_name: "You", text: "background", final_caption: false },
    });
    await waitFor(() => target.textContent.includes("background"), "unfinished user caption");
    listeners.get("orb://caption-clear")({ payload: "user" });
    await waitFor(
      () => !target.textContent.includes("background") && target.textContent.includes("Ready."),
      "silent unfinished-caption cleanup",
    );

    listeners.get("orb://phase")({
      payload: {
        revision: 12, state: "ended", phase: "ended", palette_key: "graphite",
        status: "Resting", window_open: false,
        paused_until_ms: null, cooldown_until_ms: null, leash_deadline_ms: null,
        cap_expired_during_wake: false,
      },
    });
    listeners.get("orb://ended")({
      payload: {
        reason: "user_disarm",
        message: "Resting",
      },
    });
    await waitFor(() => target.querySelector("[data-orb-terminal]"), "single terminal farewell");
    assert.equal(target.querySelector("[data-orb-status]"), null);
    assert.equal(
      target.querySelector("[data-orb-terminal]").textContent.trim(),
      "Resting",
    );
    assert.equal(
      target.textContent.split("Resting").length - 1,
      1,
      "the terminal message has exactly one visible owner",
    );
  } finally {
    await unmount(instance);
    console.warn = originalWarn;
    delete globalThis.__orbTauri;
    dom.window.close();
  }
});

test("renderer cadence, invisible window, radial actions, and settle styling stay connected", () => {
  assert.match(surfaceSource, /class:spotlight[\s\S]*class:settling=/);
  assert.match(surfaceSource, /data-radial-menu/);
  assert.match(surfaceSource, /@keyframes radial-emerge/);
  assert.match(surfaceSource, /orbTapIntent\(event\.detail, expanded\)/);
  assert.match(surfaceSource, /orbCycleCommand\(tapOriginMode \?\? presentation\.mode\)/);
  assert.match(surfaceSource, /double-click to cycle size/);
  assert.match(surfaceSource, /const canDrag = \$derived\(\["resting", "expanded"\]\.includes\(presentation\.mode\)\);/);
  assert.match(surfaceSource, /orb_reset_home/);
  assert.match(surfaceSource, />Home</);
  assert.match(surfaceSource, /getCurrentWindow\(\)\.startDragging\(\)/);
  assert.match(surfaceSource, /target\.closest\("button"\)/);
  assert.match(surfaceSource, /Let the Orb rest/);
  assert.match(surfaceSource, /Wake the Orb/);
  assert.doesNotMatch(surfaceSource, /"Disarm"|"Re-arm"/);
  assert.match(surfaceSource, /\.orb-scene[\s\S]*background: transparent/);
  assert.match(surfaceSource, /class:docked=\{presentation\.docked\}/);
  assert.match(surfaceSource, /data-notch-surface/);
  assert.match(surfaceSource, /--notch-depth/);
  assert.match(surfaceSource, /\.docked \.notch-surface/);
  assert.match(surfaceSource, /\.docked\.expanded:not\(\.spotlight\) \.notch-surface/);
  assert.match(surfaceSource, /main:not\(\.expanded\) \.state-pill[\s\S]*right: 44px;[\s\S]*top: 50%/);
  assert.match(surfaceSource, /\.notched:not\(\.expanded\)[\s\S]*--orb-x: calc\(100% - 38px\)/);
  assert.match(surfaceSource, /onpointerenter=\{/);
  assert.match(surfaceSource, /orb_set_edge_open/);
  assert.match(surfaceSource, /data-orb-intro/);
  assert.match(surfaceSource, /get_orb_intro/);
  assert.match(surfaceSource, /main:not\(\.expanded\) \.farewell[\s\S]*text-overflow: ellipsis/);
  assert.match(surfaceSource, /\.expanded:not\(\.spotlight\) \.farewell[\s\S]*left: 230px;[\s\S]*top: var\(--orb-center-y\);[\s\S]*text-align: left/);
  assert.match(surfaceSource, /\.spotlight \.farewell[\s\S]*left: 50%;[\s\S]*top: 338px;[\s\S]*text-align: center/);
  assert.match(surfaceSource, /\.spotlight \.conversation-cloud[\s\S]*translate: -50% 0/);
  assert.match(surfaceSource, /\.spotlight \.atmosphere[\s\S]*-webkit-mask-image: radial-gradient/);
  assert.doesNotMatch(surfaceSource, /class="glass"/);
  assert.match(shaderSource, /frameIsDue\(now, lastDraw, active, reducedMotion\)/);
  assert.match(shaderSource, /powerPreference: "low-power"/);
  assert.match(shaderSource, /uniform float uEnergy/);
  assert.match(shaderSource, /uniform float uTempo/);
  assert.match(shaderSource, /uniform float uBreath/);
  assert.match(shaderSource, /uniform float uTurbulence/);
  assert.match(shaderSource, /float radius = \.69 \+ displacement/);
  assert.match(shaderSource, /data-phase=\{phase \?\? "off"\}/);
  assert.match(surfaceSource, /@keyframes core-drift/);
  assert.match(surfaceSource, /@keyframes core-think/);
  assert.match(surfaceSource, /@keyframes core-speak/);
});

test("only the Ambient Orb window can request native dragging", () => {
  assert.deepEqual(orbDragCapability.windows, ["magician-notch-orb"]);
  assert.deepEqual(orbDragCapability.permissions, ["core:window:allow-start-dragging"]);
});

test("detaching changes only the notch silhouette, not Orb content geometry", () => {
  assert.match(surfaceSource, /main:not\(\.expanded\)[\s\S]*--orb-size: 32px/);
  assert.match(surfaceSource, /\.expanded:not\(\.spotlight\)[\s\S]*--orb-y: calc\(var\(--notch-depth\) \+ 44px\)/);
  assert.match(surfaceSource, /\.expanded:not\(\.spotlight\) \.conversation-cloud/);
  assert.doesNotMatch(
    surfaceSource,
    /\.docked(?:\.expanded:not\(\.spotlight\)|:not\(\.expanded\)) (?:\.state-pill|\.conversation-cloud|\.farewell|\.dark-halo)/,
  );
});

test("Orb settings own an independent cold-start wake switch", () => {
  assert.match(settingsSource, /bind:checked=\{config\.orb\.wake_enabled\}/);
  assert.match(settingsSource, /Let the wake phrase summon the Orb/);
  assert.match(settingsSource, /Keep the Orb ready/);
  assert.match(settingsSource, /Keep wake listening ready on battery power/);
  assert.doesNotMatch(settingsSource, /Keep the orb armed|Stay armed on battery power/);
  assert.match(settingsSource, /it never opens the web composer/);
});

test("desktop settings send shared configuration to the browser", () => {
  assert.match(settingsSource, /await invoke\("open_app_at", \{ path \}\)/);
  assert.match(settingsSource, /openWebSettings\("\/settings"\)/);
  assert.match(settingsSource, /openWebSettings\("\/settings\/model-routing"\)/);
  assert.match(settingsSource, /Open Web Settings/);
  assert.match(settingsSource, /Model Routing/);
  assert.match(settingsSource, /This \{desktopDeviceLabel\}/);
  assert.match(settingsSource, /\{#if isMacDesktop\}[\s\S]*<MacosAppPairing/);
  assert.match(settingsSource, /\{#if isMacDesktop\}[\s\S]*<h2>iMessage access<\/h2>/);
  assert.match(settingsSource, /bind:value=\{config\.network\.engine_base_url\}/);
  assert.match(settingsSource, /voice, notes/);
  assert.doesNotMatch(settingsSource, /<h2>Notes<\/h2>/);
  assert.doesNotMatch(settingsSource, /Audio Engines/);
  assert.doesNotMatch(settingsSource, /<h2>Workspace Storage<\/h2>/);
  assert.doesNotMatch(settingsSource, /save_media_preferences/);
  assert.match(settingsSource, /adoptSharedVoiceModeMirror/);
});
