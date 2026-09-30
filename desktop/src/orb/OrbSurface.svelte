<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { onDestroy, onMount, tick } from "svelte";
  import OrbShader from "./OrbShader.svelte";
  import {
    captionAnnouncement,
    clearUnfinishedCaption,
    decayedLevel,
    frameIsDue,
    latestRevision,
    orbCycleCommand,
    orbInvitation,
    orbTapIntent,
    reduceCaptions,
  } from "./orbUiModel.js";

  interface OrbSnapshot {
    revision: number;
    state: string;
    phase: string | null;
    palette_key: string | null;
    status: string;
    window_open: boolean;
    paused_until_ms: number | null;
    cooldown_until_ms: number | null;
    leash_deadline_ms: number | null;
    cap_expired_during_wake: boolean;
  }
  interface Presentation {
    mode: "hidden" | "resting" | "expanded" | "spotlight" | "settling";
    duration_ms: number;
    notch: boolean;
    docked: boolean;
    notch_width_points: number;
    notch_height_points: number;
    revision: number;
  }
  interface Caption {
    role: "user" | "assistant";
    speaker_name: string;
    text: string;
    final_caption: boolean;
  }
  interface AudioLevel { channel: "input" | "output"; level: number; }
  interface Ended { message: string; reason: string; }

  const empty: OrbSnapshot = {
    revision: 0,
    state: "off",
    phase: null,
    palette_key: "graphite",
    status: "Resting",
    window_open: false,
    paused_until_ms: null,
    cooldown_until_ms: null,
    leash_deadline_ms: null,
    cap_expired_during_wake: false,
  };

  let snapshot = $state<OrbSnapshot>(empty);
  let presentation = $state<Presentation>({
    mode: "resting",
    duration_ms: 260,
    notch: false,
    docked: false,
    notch_width_points: 0,
    notch_height_points: 0,
    revision: 0,
  });
  let captions = $state<Caption[]>([]);
  let announcementText = $state("");
  let announcementGeneration = 0;
  let endedMessage = $state("");
  let inputLevel = $state(0);
  let outputLevel = $state(0);
  let commandBusy = $state(false);
  let commandError = $state("");
  let controlsOpen = $state(false);
  let now = $state(Date.now());
  let reducedMotion = $state(window.matchMedia("(prefers-reduced-motion: reduce)").matches);
  const unlisteners: UnlistenFn[] = [];
  let decayFrame = 0;
  let decayRunning = false;
  let lastDecayDraw = 0;
  let tapTimer: ReturnType<typeof setTimeout> | undefined;
  let tapOriginMode: Presentation["mode"] | undefined;
  let clock: ReturnType<typeof setInterval> | undefined;
  let stopMotionListener: (() => void) | undefined;
  interface IntroHint { keys: string; action: string }
  interface OrbIntro { visible: boolean; hints: IntroHint[] }
  let introVisible = $state(false);
  let introHints = $state<IntroHint[]>([]);
  let pointerInside = $state(false);
  let edgeRequested = false;
  let suppressOrbClick = false;
  let dragArm: { x: number; y: number; id: number } | null = null;

  const expanded = $derived(["expanded", "spotlight"].includes(presentation.mode));
  const detailsVisible = $derived(
    ["expanded", "spotlight", "settling"].includes(presentation.mode),
  );
  const spotlight = $derived(presentation.mode === "spotlight");
  const canDrag = $derived(["resting", "expanded"].includes(presentation.mode));
  const notchDepth = $derived(
    Math.max(presentation.notch_height_points || 0, presentation.notch ? 28 : 12),
  );
  const notchWidth = $derived(
    Math.max(presentation.notch_width_points || 0, presentation.notch ? 120 : 82),
  );
  const pausedRemaining = $derived(
    snapshot.paused_until_ms ? Math.max(0, snapshot.paused_until_ms - now) : 0,
  );
  const pauseLabel = $derived(
    pausedRemaining > 0 ? `Paused · ${Math.ceil(pausedRemaining / 60000)}m` : snapshot.status,
  );
  const activeMotion = $derived(![null, "armed", "ended"].includes(snapshot.phase));
  const pauseEligible = $derived([
    "armed", "heard", "connecting", "listening", "thinking", "speaking",
    "cooldown", "hold_ready", "recoverable_error", "paused",
  ].includes(snapshot.state));
  const canTalk = $derived(["armed", "cooldown"].includes(snapshot.state));
  const canDisarm = $derived(!["off", "ended", "disarming"].includes(snapshot.state));

  $effect(() => {
    const open = presentation.mode === "resting" && (pointerInside || activeMotion);
    if (open === edgeRequested) return;
    edgeRequested = open;
    void invoke("orb_set_edge_open", { open });
  });

  async function command(name: string) {
    if (commandBusy) return;
    commandBusy = true;
    commandError = "";
    try {
      await invoke(name);
    } catch (error) {
      commandError = error instanceof Error ? error.message : String(error);
    } finally {
      commandBusy = false;
    }
  }

  async function radialCommand(name: string) {
    controlsOpen = false;
    await command(name);
  }

  function pauseOrResume() {
    return radialCommand(snapshot.state === "paused" ? "orb_resume" : "orb_pause_one_hour");
  }

  function disarmOrRearm() {
    return radialCommand(canDisarm ? "orb_disarm" : "orb_rearm");
  }

  function armOrbDrag(event: PointerEvent) {
    if (!canDrag || event.button !== 0) return;
    dragArm = { x: event.screenX, y: event.screenY, id: event.pointerId };
    if (event.currentTarget instanceof Element) {
      event.currentTarget.setPointerCapture(event.pointerId);
    }
  }

  function followOrbDrag(event: PointerEvent) {
    if (!dragArm || event.pointerId !== dragArm.id) return;
    const dx = event.screenX - dragArm.x;
    const dy = event.screenY - dragArm.y;
    if (dx * dx + dy * dy < 25) return;
    dragArm = null;
    suppressOrbClick = true;
    void getCurrentWindow().startDragging().catch(error => {
      console.warn("Could not begin Ambient Orb drag", error);
    });
  }

  function endOrbDrag(event: PointerEvent) {
    if (dragArm?.id === event.pointerId) dragArm = null;
  }

  function beginSurfaceDrag(event: PointerEvent) {
    if (!canDrag || event.button !== 0) return;
    const target = event.target;
    // Orb/radial buttons retain click and double-click ownership. Every other
    // visible part of the minimal or expanded notchless surface is a handle.
    if (target instanceof Element && target.closest("button")) return;
    void getCurrentWindow().startDragging().catch(error => {
      console.warn("Could not begin Ambient Orb drag", error);
    });
  }

  function handleOrbClick(event: MouseEvent) {
    event.stopPropagation();
    if (suppressOrbClick) {
      suppressOrbClick = false;
      return;
    }
    const intent = orbTapIntent(event.detail, expanded);
    if (intent === "cycle_presentation") {
      if (tapTimer) clearTimeout(tapTimer);
      tapTimer = undefined;
      // Use the mode where the first click began. If the user's configured
      // double-click interval exceeds our single-click delay, the first action
      // may already have repainted the presentation before click #2 arrives;
      // cycling from the repainted mode would skip a state.
      const nextCommand = orbCycleCommand(tapOriginMode ?? presentation.mode);
      tapOriginMode = undefined;
      controlsOpen = nextCommand === "orb_expand";
      void command(nextCommand);
      return;
    }
    tapOriginMode = presentation.mode;
    if (tapTimer) clearTimeout(tapTimer);
    tapTimer = setTimeout(() => {
      tapTimer = undefined;
      if (intent === "expand_controls") {
        controlsOpen = true;
        void command("orb_expand");
      } else {
        controlsOpen = !controlsOpen;
      }
    }, 210);
  }

  async function announce(caption: Caption | null) {
    const generation = ++announcementGeneration;
    announcementText = "";
    if (!caption) return;
    await tick();
    if (generation === announcementGeneration) {
      announcementText = captionAnnouncement(caption);
    }
  }

  function pushCaption(caption: Caption) {
    captions = reduceCaptions(captions, caption);
    if (caption.final_caption) void announce(caption);
  }

  function ensureLevelDecay() {
    if (decayRunning) return;
    decayRunning = true;
    const decay = (timestamp: number) => {
      if (lastDecayDraw && !frameIsDue(timestamp, lastDecayDraw, true, false)) {
        decayFrame = requestAnimationFrame(decay);
        return;
      }
      const elapsed = lastDecayDraw ? timestamp - lastDecayDraw : 1000 / 60;
      lastDecayDraw = timestamp;
      inputLevel = decayedLevel(inputLevel, 0.84, elapsed);
      outputLevel = decayedLevel(outputLevel, 0.87, elapsed);
      if (inputLevel > 0.001 || outputLevel > 0.001) {
        decayFrame = requestAnimationFrame(decay);
      } else {
        inputLevel = 0;
        outputLevel = 0;
        decayRunning = false;
        lastDecayDraw = 0;
      }
    };
    decayFrame = requestAnimationFrame(decay);
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key !== "Escape") return;
    if (controlsOpen) {
      controlsOpen = false;
    } else if (expanded) {
      void command("orb_collapse");
    }
  }

  onMount(async () => {
    window.addEventListener("keydown", onKeydown);
    const motionQuery = window.matchMedia("(prefers-reduced-motion: reduce)");
    const syncMotion = () => {
      reducedMotion = motionQuery.matches;
      void invoke("orb_set_reduced_motion", { reduced: reducedMotion }).catch(() => undefined);
    };
    motionQuery.addEventListener("change", syncMotion);
    stopMotionListener = () => motionQuery.removeEventListener("change", syncMotion);
    syncMotion();

    const listeners = await Promise.all([
      listen<OrbSnapshot>("orb://phase", event => {
        const next = event.payload;
        if (latestRevision(snapshot, next) !== next) return;
        if (next.state === "heard" && snapshot.state !== "heard") {
          captions = [];
          void announce(null);
        }
        if (["disarming", "ended", "off"].includes(next.state)) void announce(null);
        if (next.state !== "ended" && next.state !== "disarming") endedMessage = "";
        snapshot = next;
      }),
      listen<Presentation>("orb://presentation", event => {
        const next = latestRevision(presentation, event.payload);
        if (next !== presentation && ["hidden", "resting", "settling"].includes(next.mode)) {
          controlsOpen = false;
        }
        presentation = next;
      }),
      listen<Caption>("orb://caption", event => pushCaption(event.payload)),
      listen<Caption["role"]>("orb://caption-clear", event => {
        captions = clearUnfinishedCaption(captions, event.payload);
      }),
      listen<AudioLevel>("orb://audio-level", event => {
        if (event.payload.channel === "input") inputLevel = Math.max(inputLevel, event.payload.level);
        else outputLevel = Math.max(outputLevel, event.payload.level);
        ensureLevelDecay();
      }),
      listen<Ended>("orb://ended", event => endedMessage = event.payload.message),
      listen<OrbIntro>("orb://intro", event => {
        introHints = event.payload.hints ?? [];
        introVisible = event.payload.visible && introHints.length > 0;
      }),
    ]);
    unlisteners.push(...listeners);

    const [initialSnapshot, initialPresentation] = await Promise.all([
      invoke<OrbSnapshot>("get_orb_snapshot").catch(() => empty),
      invoke<Presentation>("get_orb_presentation").catch(() => presentation),
    ]);
    snapshot = latestRevision(snapshot, initialSnapshot);
    presentation = latestRevision(presentation, initialPresentation);
    const intro = await invoke<OrbIntro>("get_orb_intro").catch(() => null);
    if (intro?.visible && intro.hints?.length) {
      introHints = intro.hints;
      introVisible = true;
    }
    clock = setInterval(() => now = Date.now(), 1000);
  });

  onDestroy(() => {
    window.removeEventListener("keydown", onKeydown);
    unlisteners.forEach(stop => stop());
    stopMotionListener?.();
    if (clock) clearInterval(clock);
    if (tapTimer) clearTimeout(tapTimer);
    cancelAnimationFrame(decayFrame);
  });
</script>

<svelte:head><title>Magican Orb</title></svelte:head>

<main
  class:expanded
  class:spotlight
  class:draggable={canDrag}
  class:settling={presentation.mode === "settling"}
  class:notched={presentation.notch}
  class:docked={presentation.docked}
  style={`--notch-depth: ${notchDepth}px; --notch-width: ${notchWidth}px;`}
  class:active-motion={activeMotion}
  class:phase-armed={snapshot.phase === "armed"}
  class:phase-heard={snapshot.phase === "heard"}
  class:phase-listening={snapshot.phase === "listening"}
  class:phase-thinking={snapshot.phase === "thinking"}
  class:phase-speaking={snapshot.phase === "speaking"}
  class:intro={introVisible && !expanded}
>
  <section
    class="orb-scene"
    aria-label="Magican ambient voice"
    onpointerdown={beginSurfaceDrag}
    onpointerenter={() => pointerInside = true}
    onpointerleave={() => pointerInside = false}
  >
    <div class="notch-surface" data-notch-surface aria-hidden="true"></div>
    <div class="atmosphere" aria-hidden="true"></div>

    <div class="orb-cluster">
      <div
        class:open={controlsOpen && expanded}
        class="radial-menu"
        data-radial-menu
        aria-hidden={!controlsOpen || !expanded}
      >
        <button
          class="radial-action talk-action"
          style="--x: 0px; --y: -108px; --delay: 0ms"
          title="Talk"
          aria-label="Start talking"
          tabindex={controlsOpen && expanded ? 0 : -1}
          disabled={commandBusy || !canTalk}
          onclick={(event) => { event.stopPropagation(); void radialCommand("orb_start_conversation"); }}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3Zm-6 9a6 6 0 0 0 12 0M12 18v3M9 21h6" /></svg>
          <span>Talk</span>
        </button>
        <button
          class="radial-action"
          style="--x: 94px; --y: -54px; --delay: 35ms"
          title={snapshot.state === "paused" ? "Resume" : "Pause for one hour"}
          aria-label={snapshot.state === "paused" ? "Resume orb" : "Pause orb for one hour"}
          tabindex={controlsOpen && expanded ? 0 : -1}
          disabled={commandBusy || !pauseEligible}
          onclick={(event) => { event.stopPropagation(); void pauseOrResume(); }}
        >
          {#if snapshot.state === "paused"}
            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="m9 6 9 6-9 6Z" /></svg>
            <span>Resume</span>
          {:else}
            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9 6v12M15 6v12" /></svg>
            <span>Pause</span>
          {/if}
        </button>
        <button
          class="radial-action"
          style="--x: 94px; --y: 54px; --delay: 70ms"
          title="Open Today"
          aria-label="Open Today"
          tabindex={controlsOpen && expanded ? 0 : -1}
          disabled={commandBusy}
          onclick={(event) => { event.stopPropagation(); void radialCommand("orb_open_app"); }}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 5h6v6H5ZM13 5h6v6h-6ZM5 13h6v6H5ZM13 13h6v6h-6Z" /></svg>
          <span>Today</span>
        </button>
        <button
          class="radial-action"
          style="--x: 0px; --y: 108px; --delay: 105ms"
          title="Settings"
          aria-label="Open orb settings"
          tabindex={controlsOpen && expanded ? 0 : -1}
          disabled={commandBusy}
          onclick={(event) => { event.stopPropagation(); void radialCommand("orb_open_settings"); }}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 7h14M8 7a2 2 0 1 0 0 .01M5 17h14M16 17a2 2 0 1 0 0 .01" /></svg>
          <span>Settings</span>
        </button>
        <button
          class="radial-action"
          style="--x: -94px; --y: 54px; --delay: 140ms"
          title="Settle to the top"
          aria-label="Collapse orb"
          tabindex={controlsOpen && expanded ? 0 : -1}
          disabled={commandBusy}
          onclick={(event) => { event.stopPropagation(); void radialCommand("orb_collapse"); }}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="m6 14 6-6 6 6" /></svg>
          <span>Settle</span>
        </button>
        <button
          class="radial-action"
          style="--x: -118px; --y: 0px; --delay: 210ms"
          title="Back to the top of the right edge"
          aria-label="Reset orb position"
          tabindex={controlsOpen && expanded ? 0 : -1}
          disabled={commandBusy}
          onclick={(event) => { event.stopPropagation(); void radialCommand("orb_reset_home"); }}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M14 5h5v5M19 5 10 14M5 10v9h9" /></svg>
          <span>Home</span>
        </button>
        <button
          class="radial-action"
          style="--x: -94px; --y: -54px; --delay: 175ms"
          title={canDisarm ? "Let the Orb rest" : "Wake the Orb"}
          aria-label={canDisarm ? "Let the Orb rest" : "Wake the Orb"}
          tabindex={controlsOpen && expanded ? 0 : -1}
          disabled={commandBusy}
          onclick={(event) => { event.stopPropagation(); void disarmOrRearm(); }}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3v8M7.2 6.8a7 7 0 1 0 9.6 0" /></svg>
          <span>{canDisarm ? "Rest" : "Wake"}</span>
        </button>
      </div>

      <button
        class="orb-button"
        class:controls-open={controlsOpen}
        aria-label={`${pauseLabel}. Tap for controls, drag to move, double-click to cycle size.`}
        aria-expanded={expanded}
        title="Tap for actions · Drag to move · Double-click to cycle size"
        onpointerdown={armOrbDrag}
        onpointermove={followOrbDrag}
        onpointerup={endOrbDrag}
        onpointercancel={endOrbDrag}
        onclick={handleOrbClick}
      >
        <span class="dark-halo" aria-hidden="true"></span>
        <span class="phase-ring ring-one" aria-hidden="true"></span>
        <span class="phase-ring ring-two" aria-hidden="true"></span>
        <span class="thinking-orbit" aria-hidden="true"><i></i><i></i><i></i></span>
        <span class="orb-wrap">
          <OrbShader
            paletteKey={snapshot.palette_key}
            phase={snapshot.phase}
            {inputLevel}
            {outputLevel}
            {reducedMotion}
          />
        </span>
      </button>

      {#if introVisible && !expanded}
        <div class="hotkey-card" data-orb-intro>
          {#each introHints as hint}
            <p><kbd>{hint.keys}</kbd><span>{hint.action}</span></p>
          {/each}
        </div>
      {:else if snapshot.phase !== "ended"}
        <div class="state-pill" data-orb-status aria-live="polite">
          <span class="state-glyph" aria-hidden="true"><i></i><i></i><i></i></span>
          <span>{snapshot.state === "paused" ? pauseLabel : snapshot.status}</span>
        </div>
      {/if}
    </div>

    {#if detailsVisible && snapshot.phase !== "ended"}
      <div class="conversation-cloud">
        <div class="identity-row">
          <span class="phase-dot"></span>
          <span class="eyebrow">MAGICAN · {snapshot.phase ?? "RESTING"}</span>
          {#if snapshot.leash_deadline_ms}<span class="leash">protected</span>{/if}
        </div>
        <h1>{spotlight ? "Here with you." : pauseLabel}</h1>
        <div class="caption-stack">
          {#if captions.length === 0}
            <p class="invitation">{orbInvitation(snapshot.state, snapshot.status)}</p>
          {:else}
            {#each captions as caption}
              <p class:user={caption.role === "user"} class="caption">
                <span>{caption.speaker_name}</span>{caption.text}
              </p>
            {/each}
          {/if}
        </div>
        <p class="gesture-hint">Drag the Orb to move it · tap it and choose Home to return to the right edge</p>
        {#if commandError}<p class="command-error" role="alert">{commandError}</p>{/if}
      </div>
    {/if}

    {#if snapshot.phase === "ended"}
      <div class="farewell" data-orb-terminal aria-live="polite">
        {endedMessage || snapshot.status}
      </div>
    {/if}
    <p class="sr-only" data-orb-announcement aria-live="polite" aria-atomic="true">{announcementText}</p>
  </section>
</main>

<style>
  :global(html.orb-surface),
  :global(body.orb-surface),
  :global(body.orb-surface #app) {
    width: 100%;
    height: 100%;
    min-height: 0;
    overflow: hidden;
    background: transparent !important;
  }

  main {
    width: 100%;
    height: 100%;
    color: #faf9ff;
    user-select: none;
    background: transparent;
    --orb-size: 54px;
    --orb-x: 16px;
    --orb-y: 10px;
    --orb-center-x: 43px;
    --orb-center-y: 37px;
    --orbit-radius: 34px;
    --phase: #a98aff;
  }
  main.phase-armed { --phase: #f2a765; }
  main.phase-heard { --phase: #b382ff; }
  main.phase-listening { --phase: #6ee7c5; }
  main.phase-thinking { --phase: #f5bf66; }
  main.phase-speaking { --phase: #61e7dd; }

  .orb-scene {
    position: relative;
    width: 100%;
    height: 100%;
    overflow: hidden;
    background: transparent;
  }
  .draggable .orb-scene,
  .draggable .state-pill,
  .draggable .conversation-cloud,
  .draggable .farewell { cursor: grab; }
  .draggable .orb-scene:active,
  .draggable .state-pill:active,
  .draggable .conversation-cloud:active,
  .draggable .farewell:active { cursor: grabbing; }
  .orb-button, .radial-action { cursor: pointer; }

  .notch-surface {
    position: absolute;
    z-index: 0;
    top: 50%;
    left: 42%;
    width: min(92%, 240px);
    height: 78%;
    border-radius: 999px;
    pointer-events: none;
    opacity: 0;
    transform: translate(-42%, -50%) scale(.92);
    transform-origin: 42% 50%;
    background:
      radial-gradient(ellipse 70% 90% at 28% 50%, color-mix(in srgb, var(--phase) 34%, transparent), transparent 70%),
      radial-gradient(ellipse 100% 120% at 46% 50%, rgba(12, 9, 20, .5), rgba(12, 9, 20, .16) 46%, transparent 76%);
    -webkit-mask-image: radial-gradient(ellipse 88% 80% at 40% 50%, #000 18%, transparent 76%);
    mask-image: radial-gradient(ellipse 88% 80% at 40% 50%, #000 18%, transparent 76%);
    transition:
      width .52s cubic-bezier(.16, 1, .3, 1),
      height .52s cubic-bezier(.16, 1, .3, 1),
      opacity .28s ease,
      transform .52s cubic-bezier(.16, 1, .3, 1);
  }
  .notch-surface::after { content: none; }
  .docked .notch-surface {
    opacity: 1;
    transform: translate(-42%, -50%) scale(1);
  }
  .docked.expanded:not(.spotlight) .notch-surface {
    left: 34%;
    width: 78%;
    height: 88%;
    transform: translate(-34%, -46%) scale(1);
    background:
      radial-gradient(ellipse 48% 62% at 24% 42%, color-mix(in srgb, var(--phase) 26%, transparent), transparent 70%),
      radial-gradient(ellipse 80% 72% at 42% 48%, rgba(10, 8, 18, .34), transparent 74%);
  }
  .active-motion.docked .notch-surface { animation: atmosphere-drift 5.2s ease-in-out infinite alternate; }

  .atmosphere {
    position: absolute;
    z-index: 1;
    inset: 0;
    pointer-events: none;
    opacity: 0;
    background:
      radial-gradient(circle at 24% 50%, color-mix(in srgb, var(--phase) 17%, transparent), transparent 34%),
      radial-gradient(ellipse at 68% 50%, rgba(8, 7, 14, .55), transparent 63%);
    transition: opacity .42s ease;
  }
  .expanded .atmosphere { opacity: 1; }
  .active-motion.expanded .atmosphere { animation: atmosphere-drift 5.2s ease-in-out infinite alternate; }
  .spotlight .atmosphere {
    inset: 4%;
    border-radius: 46%;
    opacity: .82;
    background:
      radial-gradient(circle at 50% 40%, color-mix(in srgb, var(--phase) 18%, transparent), transparent 31%),
      radial-gradient(circle at 50% 43%, rgba(5, 5, 10, .58), transparent 58%);
    -webkit-mask-image: radial-gradient(ellipse 72% 68% at 50% 44%, #000 0 38%, rgba(0, 0, 0, .82) 62%, transparent 100%);
    mask-image: radial-gradient(ellipse 72% 68% at 50% 44%, #000 0 38%, rgba(0, 0, 0, .82) 62%, transparent 100%);
  }

  .orb-cluster {
    position: absolute;
    z-index: 3;
    inset: 0;
    pointer-events: none;
  }
  .orb-button {
    position: absolute;
    left: var(--orb-x);
    top: var(--orb-y);
    width: var(--orb-size);
    height: var(--orb-size);
    padding: 0;
    border: 0;
    border-radius: 50%;
    color: inherit;
    background: transparent;
    pointer-events: auto;
    isolation: isolate;
    outline: none;
    filter: drop-shadow(0 7px 14px rgba(0, 0, 0, .5));
    transition:
      left .58s cubic-bezier(.16, 1, .3, 1),
      top .58s cubic-bezier(.16, 1, .3, 1),
      width .58s cubic-bezier(.16, 1, .3, 1),
      height .58s cubic-bezier(.16, 1, .3, 1),
      filter .3s ease;
  }
  .orb-button:focus-visible .dark-halo {
    box-shadow: 0 0 0 2px rgba(245, 242, 255, .9), 0 0 34px color-mix(in srgb, var(--phase) 65%, transparent);
  }
  .orb-button:hover { filter: drop-shadow(0 9px 22px rgba(0, 0, 0, .6)) brightness(1.06); }

  .dark-halo {
    position: absolute;
    inset: -20%;
    z-index: -3;
    border-radius: 50%;
    background: radial-gradient(circle, rgba(14, 11, 22, .42) 0 28%, rgba(8, 7, 14, .14) 50%, transparent 72%);
    transition: inset .5s ease, opacity .5s ease, transform .42s cubic-bezier(.16, 1, .3, 1);
  }
  .orb-button.controls-open .dark-halo { opacity: 1; transform: scale(1.1); }
  .orb-wrap {
    position: absolute;
    inset: 7%;
    display: block;
    border-radius: 50%;
    overflow: hidden;
    filter: drop-shadow(0 0 9px color-mix(in srgb, var(--phase) 48%, transparent));
    transform-origin: 50% 50%;
    will-change: transform, filter;
  }
  .phase-ring {
    position: absolute;
    inset: -4%;
    border: 1px solid color-mix(in srgb, var(--phase) 52%, transparent);
    border-radius: 50%;
    opacity: .2;
    pointer-events: none;
  }
  .ring-two { inset: -16%; opacity: .08; }

  .phase-armed .orb-button { animation: breathe 4.8s ease-in-out infinite; }
  .phase-armed .orb-wrap { animation: core-drift 7.6s ease-in-out infinite alternate; }
  .phase-heard .ring-one { animation: wake-bloom .9s cubic-bezier(.16, 1, .3, 1) infinite; }
  .phase-heard .ring-two { animation: wake-bloom .9s .18s cubic-bezier(.16, 1, .3, 1) infinite; }
  .phase-heard .orb-wrap { animation: core-wake 1.05s cubic-bezier(.16, 1, .3, 1) infinite; }
  .phase-listening .ring-one { animation: listen-ring 1.45s ease-out infinite; }
  .phase-listening .ring-two { animation: listen-ring 1.45s .48s ease-out infinite; }
  .phase-listening .orb-wrap { animation: core-listen 3.4s ease-in-out infinite; }
  .phase-thinking .orb-wrap { animation: core-think 2.8s ease-in-out infinite alternate; }
  .phase-speaking .ring-one { animation: speak-ripple .72s ease-out infinite; }
  .phase-speaking .ring-two { animation: speak-ripple .72s .24s ease-out infinite; }
  .phase-speaking .orb-wrap { animation: core-speak 1.18s ease-in-out infinite; }

  .thinking-orbit {
    position: absolute;
    inset: -12%;
    border-radius: 50%;
    opacity: 0;
    pointer-events: none;
  }
  .thinking-orbit i {
    position: absolute;
    left: 50%;
    top: -1px;
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: var(--phase);
    box-shadow: 0 0 9px var(--phase);
  }
  .thinking-orbit i:nth-child(2) { transform: rotate(120deg); transform-origin: 0 var(--orbit-radius); }
  .thinking-orbit i:nth-child(3) { transform: rotate(240deg); transform-origin: 0 var(--orbit-radius); }
  .phase-thinking .thinking-orbit { opacity: .88; animation: orbit 2.2s linear infinite; }

  .state-pill {
    position: absolute;
    box-sizing: border-box;
    left: 68px;
    top: 50%;
    max-width: 139px;
    min-height: 28px;
    padding: 7px 11px 7px 9px;
    border-radius: 999px;
    display: flex;
    align-items: center;
    gap: 7px;
    color: rgba(248, 246, 255, .94);
    text-shadow: 0 1px 2px rgba(0, 0, 0, .72);
    background: radial-gradient(ellipse at 16% 50%, color-mix(in srgb, var(--phase) 24%, transparent), rgba(14, 11, 22, .34) 72%);
    font-size: 10.5px;
    font-weight: 620;
    letter-spacing: .005em;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    transform: translateY(-50%);
    pointer-events: none;
    transition: opacity .3s ease, transform .42s ease;
  }
  .state-glyph { width: 13px; height: 12px; display: flex; align-items: center; justify-content: center; gap: 1.5px; flex: none; }
  .state-glyph i { width: 2px; height: 5px; border-radius: 2px; background: var(--phase); opacity: .75; }
  .phase-listening .state-glyph i,
  .phase-speaking .state-glyph i { animation: meter .65s ease-in-out infinite alternate; }
  .state-glyph i:nth-child(2) { height: 10px; animation-delay: -.22s !important; }
  .state-glyph i:nth-child(3) { height: 7px; animation-delay: -.38s !important; }

  /* Docking changes only the surrounding notch silhouette. The Orb's body,
     status, and controls belong to the presentation mode, so persisting a
     dragged placement cannot resize or realign them when `docked` flips. */
  main:not(.expanded) {
    --orb-size: 32px;
    --orb-x: calc(100% - 38px);
    --orb-y: calc(50% - 16px);
    --orb-center-x: calc(100% - 22px);
    --orb-center-y: 50%;
    --orbit-radius: 20px;
  }
  main:not(.expanded) .dark-halo { inset: -20%; }
  main:not(.expanded) .state-pill {
    left: auto;
    right: 44px;
    top: 50%;
    max-width: none;
    min-height: 22px;
    padding: 3px 8px 3px 7px;
    justify-content: flex-start;
    gap: 5px;
    border-radius: 999px;
    background: radial-gradient(ellipse at 18% 50%, color-mix(in srgb, var(--phase) 28%, transparent), rgba(12, 10, 18, .38) 70%);
    box-shadow: none;
    font-size: 9px;
    transform: translateY(-50%);
  }
  main:not(.expanded) .state-pill > span:last-child {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  main.intro,
  main.intro.notched {
    --orb-size: 224px;
    --orb-x: calc(50% - 112px);
    --orb-y: 48px;
    --orb-center-x: 50%;
    --orb-center-y: 160px;
    --orbit-radius: 139px;
  }
  main.intro .dark-halo { inset: -34%; }
  main.intro .notch-surface {
    left: 50%;
    top: 40%;
    right: auto;
    width: 86%;
    height: 72%;
    transform: translate(-50%, -48%);
    border-radius: 50%;
    -webkit-mask-image: radial-gradient(ellipse 72% 70% at 50% 42%, #000 0 28%, transparent 74%);
    mask-image: radial-gradient(ellipse 72% 70% at 50% 42%, #000 0 28%, transparent 74%);
    background:
      radial-gradient(circle at 50% 42%, color-mix(in srgb, var(--phase) 30%, transparent), transparent 48%),
      radial-gradient(circle at 50% 50%, rgba(10, 8, 18, .32), transparent 64%);
  }
  .hotkey-card {
    position: absolute;
    z-index: 4;
    left: 36px;
    right: 36px;
    top: 308px;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    pointer-events: none;
  }
  .hotkey-card p {
    margin: 0;
    display: flex;
    align-items: baseline;
    justify-content: center;
    gap: 10px;
    color: rgba(244, 240, 255, .94);
    font-size: 16px;
    font-weight: 560;
    letter-spacing: .01em;
    text-shadow: 0 1px 3px rgba(0, 0, 0, .75);
  }
  .hotkey-card kbd {
    font-family: inherit;
    font-weight: 720;
    color: white;
    white-space: nowrap;
  }
  .hotkey-card span { color: rgba(214, 206, 230, .82); white-space: nowrap; }

  main:not(.expanded) .state-glyph { width: 10px; height: 9px; }
  main:not(.expanded) .state-glyph i { width: 1.5px; height: 4px; }
  main:not(.expanded) .state-glyph i:nth-child(2) { height: 8px; }
  main:not(.expanded) .state-glyph i:nth-child(3) { height: 6px; }
  .notched:not(.expanded) {
    --orb-size: 32px;
    --orb-x: calc(100% - 38px);
    --orb-y: calc(50% - 16px);
    --orb-center-x: calc(100% - 22px);
    --orb-center-y: 50%;
    --orbit-radius: 20px;
  }
  .notched:not(.expanded) .state-pill {
    left: auto;
    right: 44px;
    top: 50%;
  }
  main.docked:not(.expanded):not(.intro) .notch-surface {
    left: 0;
    right: 0;
    top: 0;
    width: 100%;
    height: 100%;
    transform: none;
    border-radius: 26px 0 0 26px;
    -webkit-mask-image: none;
    mask-image: none;
    background:
      radial-gradient(ellipse 90% 140% at 92% 50%, color-mix(in srgb, var(--phase) 38%, transparent), transparent 68%),
      radial-gradient(ellipse 160% 120% at 100% 50%, rgba(12, 9, 20, .58), rgba(12, 9, 20, .14) 52%, transparent 80%);
  }

  .expanded {
    --orb-size: 148px;
    --orb-x: 43px;
    --orb-y: 76px;
    --orb-center-x: 117px;
    --orb-center-y: 150px;
    --orbit-radius: 92px;
  }
  .expanded .dark-halo { inset: -28%; }
  .expanded .state-pill {
    left: 67px;
    top: 232px;
    max-width: 152px;
    justify-content: center;
    transform: none;
  }
  .expanded:not(.spotlight) {
    --orb-y: calc(var(--notch-depth) + 44px);
    --orb-center-y: calc(var(--notch-depth) + 118px);
  }
  .expanded:not(.spotlight) .state-pill {
    top: calc(var(--notch-depth) + 200px);
  }

  .spotlight {
    --orb-size: 224px;
    --orb-x: calc(50% - 112px);
    --orb-y: 74px;
    --orb-center-x: 50%;
    --orb-center-y: 186px;
    --orbit-radius: 139px;
  }
  .spotlight .dark-halo { inset: -34%; }
  .spotlight .state-pill {
    left: 50%;
    top: 308px;
    max-width: 230px;
    transform: translateX(-50%);
  }

  .radial-menu {
    position: absolute;
    left: var(--orb-center-x);
    top: var(--orb-center-y);
    width: 0;
    height: 0;
    z-index: 8;
    pointer-events: none;
    transition: left .58s cubic-bezier(.16, 1, .3, 1), top .58s cubic-bezier(.16, 1, .3, 1);
  }
  .radial-action {
    position: absolute;
    left: 0;
    top: 0;
    width: 42px;
    height: 42px;
    padding: 0;
    border: 1px solid rgba(255, 255, 255, .1);
    border-radius: 50%;
    display: grid;
    place-items: center;
    color: rgba(247, 244, 255, .88);
    background: radial-gradient(circle at 35% 25%, rgba(47, 42, 65, .96), rgba(12, 10, 19, .94) 68%);
    box-shadow: 0 8px 22px rgba(0, 0, 0, .42), inset 0 1px rgba(255, 255, 255, .08);
    opacity: 0;
    transform: translate(-50%, -50%) scale(.18);
    pointer-events: none;
    transition: filter .18s ease, border-color .18s ease;
  }
  .radial-action svg { width: 18px; height: 18px; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; }
  .radial-action span {
    position: absolute;
    top: calc(100% + 5px);
    left: 50%;
    padding: 3px 6px;
    border-radius: 6px;
    color: #e9e4f3;
    background: rgba(7, 6, 12, .88);
    font-size: 8px;
    font-weight: 650;
    white-space: nowrap;
    opacity: 0;
    transform: translate(-50%, -2px);
    transition: opacity .15s ease, transform .15s ease;
  }
  .radial-action:hover:not(:disabled), .radial-action:focus-visible:not(:disabled) {
    filter: brightness(1.2);
    border-color: color-mix(in srgb, var(--phase) 55%, transparent);
    outline: none;
  }
  .radial-action:hover span, .radial-action:focus-visible span { opacity: 1; transform: translate(-50%, 0); }
  .radial-action:disabled { opacity: .26 !important; }
  .radial-menu.open { pointer-events: auto; }
  .radial-menu.open .radial-action {
    pointer-events: auto;
    animation: radial-emerge .48s var(--delay) cubic-bezier(.16, 1.15, .3, 1) forwards;
  }
  .talk-action {
    color: white;
    background: radial-gradient(circle at 35% 25%, color-mix(in srgb, var(--phase) 75%, #fff 5%), #4d358b 78%);
    box-shadow: 0 9px 25px color-mix(in srgb, var(--phase) 28%, transparent);
  }

  .conversation-cloud {
    position: absolute;
    box-sizing: border-box;
    z-index: 2;
    left: 230px;
    top: 48px;
    right: 14px;
    min-height: 204px;
    padding: 35px 24px 25px 31px;
    display: flex;
    flex-direction: column;
    justify-content: center;
    pointer-events: auto;
    background:
      radial-gradient(ellipse at 42% 48%, rgba(16, 14, 25, .88), rgba(8, 7, 13, .63) 56%, transparent 75%);
    mask-image: radial-gradient(ellipse at center, #000 52%, transparent 82%);
    animation: cloud-in .52s .08s cubic-bezier(.16, 1, .3, 1) both;
  }
  .expanded:not(.spotlight) .conversation-cloud {
    top: calc(var(--notch-depth) + 16px);
  }
  .identity-row { display: flex; align-items: center; gap: 7px; min-height: 16px; }
  .phase-dot { width: 6px; height: 6px; border-radius: 50%; background: var(--phase); box-shadow: 0 0 12px var(--phase); animation: pulse 2.2s ease-in-out infinite; }
  .eyebrow { color: rgba(205, 198, 222, .72); font-size: 8.5px; font-weight: 720; letter-spacing: .17em; text-transform: uppercase; }
  .leash { margin-left: auto; color: rgba(172, 164, 190, .58); font-size: 8px; }
  h1 { margin: 8px 0 9px; font-size: 23px; font-weight: 640; letter-spacing: -.035em; line-height: 1.04; text-shadow: 0 3px 20px rgba(0, 0, 0, .55); }
  .caption-stack { min-height: 72px; max-height: 92px; overflow: hidden; display: flex; flex-direction: column; justify-content: flex-end; gap: 5px; mask-image: linear-gradient(to bottom, transparent, #000 25%); }
  .caption, .invitation { color: rgba(229, 224, 238, .86); font-size: 11.5px; line-height: 1.42; overflow: hidden; display: -webkit-box; line-clamp: 2; -webkit-line-clamp: 2; -webkit-box-orient: vertical; }
  .caption span { margin-right: 7px; color: color-mix(in srgb, var(--phase) 74%, white); font-size: 8.5px; font-weight: 720; letter-spacing: .08em; text-transform: uppercase; }
  .caption.user { color: rgba(188, 181, 202, .78); }
  .invitation { color: rgba(187, 179, 202, .72); font-style: italic; }
  .gesture-hint { margin-top: 10px; color: rgba(171, 162, 187, .46); font-size: 8px; letter-spacing: .025em; }
  .command-error { margin-top: 6px; color: #f0aeba; font-size: 9px; line-height: 1.3; }

  .spotlight .conversation-cloud {
    left: 50%;
    right: auto;
    top: 338px;
    width: 430px;
    min-height: 145px;
    padding: 23px 54px;
    text-align: center;
    /* Keep centering independent from the entrance animation's `transform`.
       Previously cloud-in ended at `transform: none`, silently discarding the
       -50% correction and leaving all spotlight copy southeast of the Orb. */
    translate: -50% 0;
    transform: none;
    background: radial-gradient(ellipse, rgba(15, 13, 23, .84), rgba(7, 6, 12, .46) 54%, transparent 76%);
    -webkit-mask-image: radial-gradient(ellipse at center, #000 42%, rgba(0, 0, 0, .88) 60%, transparent 88%);
    mask-image: radial-gradient(ellipse at center, #000 42%, rgba(0, 0, 0, .88) 60%, transparent 88%);
  }
  .spotlight .identity-row { justify-content: center; }
  .spotlight .leash { margin-left: 0; }
  .spotlight h1 { font-size: 31px; }
  .spotlight .caption-stack { min-height: 46px; max-height: 62px; }

  .settling .conversation-cloud { animation: cloud-out .45s ease both; }
  .settling .state-pill { opacity: 0; }
  .notched:not(.docked):not(.expanded) .dark-halo {
    background: radial-gradient(circle, rgba(5, 5, 8, .98) 0 45%, rgba(4, 4, 7, .78) 60%, transparent 75%);
  }
  .farewell {
    position: absolute;
    left: 12px;
    right: 12px;
    bottom: 4px;
    z-index: 10;
    color: rgba(216, 209, 229, .82);
    font-size: 9px;
    line-height: 1.25;
    text-align: center;
    text-wrap: balance;
  }
  main:not(.expanded) .farewell {
    left: 52px;
    right: 10px;
    top: 50%;
    bottom: auto;
    overflow: hidden;
    text-align: left;
    text-overflow: ellipsis;
    text-wrap: nowrap;
    white-space: nowrap;
    transform: translateY(-50%);
  }
  .expanded:not(.spotlight) .farewell {
    left: 230px;
    right: 20px;
    top: var(--orb-center-y);
    bottom: auto;
    text-align: left;
    transform: translateY(-50%);
  }
  .spotlight .farewell {
    left: 50%;
    right: auto;
    top: 338px;
    bottom: auto;
    width: 430px;
    text-align: center;
    transform: translateX(-50%);
  }
  .notched:not(.expanded) .farewell {
    left: 46px;
    top: calc(var(--notch-depth) + 17px);
  }
  .sr-only { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; }

  @keyframes radial-emerge {
    from { opacity: 0; transform: translate(-50%, -50%) scale(.18) rotate(-16deg); }
    to { opacity: 1; transform: translate(calc(-50% + var(--x)), calc(-50% + var(--y))) scale(1) rotate(0); }
  }
  @keyframes notch-current {
    0%, 100% { opacity: .28; transform: scaleX(.72); }
    50% { opacity: .82; transform: scaleX(1.08); }
  }
  @keyframes breathe { 50% { transform: scale(1.045); } }
  @keyframes core-drift {
    0% { transform: scale(.955) rotate(-2deg); }
    52% { transform: scale(1.035) rotate(2.5deg); }
    100% { transform: scale(.98) rotate(5deg); }
  }
  @keyframes core-wake {
    0%, 100% { transform: scale(.91); }
    46% { transform: scale(1.085); }
  }
  @keyframes core-listen {
    0%, 100% { transform: scale(.97) rotate(-1.25deg); }
    45% { transform: scale(1.035, 1.055) rotate(1.25deg); }
  }
  @keyframes core-think {
    0% { transform: scale(1.035, .965) rotate(-3deg); }
    54% { transform: scale(.955, 1.04) rotate(1deg); }
    100% { transform: scale(1.015, .98) rotate(4deg); }
  }
  @keyframes core-speak {
    0%, 100% { transform: scale(.965); }
    48% { transform: scale(1.065); }
  }
  @keyframes wake-bloom { from { opacity: .72; transform: scale(.76); } to { opacity: 0; transform: scale(1.48); } }
  @keyframes listen-ring { from { opacity: .55; transform: scale(.86); } to { opacity: 0; transform: scale(1.34); } }
  @keyframes speak-ripple { 0% { opacity: .7; transform: scale(.9); } 60% { opacity: .16; } 100% { opacity: 0; transform: scale(1.48); } }
  @keyframes orbit { to { transform: rotate(360deg); } }
  @keyframes meter { to { transform: scaleY(.4); opacity: .42; } }
  @keyframes pulse { 50% { transform: scale(1.45); opacity: .58; } }
  @keyframes atmosphere-drift {
    from { transform: translate3d(-1.5%, 0, 0) scale(.98); }
    to { transform: translate3d(1.5%, -1%, 0) scale(1.03); }
  }
  @keyframes cloud-in { from { opacity: 0; transform: translateX(-14px) scale(.94); filter: blur(8px); } to { opacity: 1; transform: none; filter: blur(0); } }
  @keyframes cloud-out { to { opacity: 0; transform: scale(.84); filter: blur(8px); } }

  @media (prefers-reduced-motion: reduce) {
    *, *::before, *::after { animation: none !important; transition-duration: .01ms !important; }
  }
</style>
