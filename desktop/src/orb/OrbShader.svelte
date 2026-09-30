<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { paletteFor, type AuroraPalette } from "./auroraPalettes";
  import {
    fallbackShouldAnimate,
    frameIntervalMs,
    frameIsDue,
    orbMotionProfile,
  } from "./orbUiModel.js";

  interface Props {
    paletteKey: string | null;
    phase: string | null;
    inputLevel: number;
    outputLevel: number;
    reducedMotion?: boolean;
  }

  let { paletteKey, phase, inputLevel, outputLevel, reducedMotion = false }: Props = $props();
  let canvas: HTMLCanvasElement;
  let fallback = $state(false);
  let frame = 0;
  let idleTimer: ReturnType<typeof setTimeout> | undefined;
  let observer: ResizeObserver | null = null;
  let gl: WebGL2RenderingContext | null = null;
  let program: WebGLProgram | null = null;
  let targetKey = "graphite";
  const fallbackActive = $derived(fallbackShouldAnimate(phase, paletteKey));
  const fallbackMotion = $derived(orbMotionProfile(phase));

  function handleContextLost(event: Event) {
    event.preventDefault();
    fallback = true;
    cancelAnimationFrame(frame);
    if (idleTimer) clearTimeout(idleTimer);
  }

  const vertexSource = `#version 300 es
    in vec2 aPosition;
    out vec2 vUv;
    void main() {
      vUv = aPosition * .5 + .5;
      gl_Position = vec4(aPosition, 0., 1.);
    }
  `;

  const fragmentSource = `#version 300 es
    precision highp float;
    in vec2 vUv;
    out vec4 fragColor;
    uniform vec2 uResolution;
    uniform float uTime;
    uniform float uLevel;
    uniform float uSpeaking;
    uniform float uEnergy;
    uniform float uTempo;
    uniform float uBreath;
    uniform float uTurbulence;
    uniform float uMix;
    uniform vec3 uFrom[4];
    uniform vec3 uTo[4];
    uniform vec4 uFromMeta;
    uniform vec4 uToMeta;

    float hash(vec2 p) {
      p = fract(p * vec2(123.34, 456.21));
      p += dot(p, p + 45.32);
      return fract(p.x * p.y);
    }
    float noise(vec2 p) {
      vec2 i = floor(p), f = fract(p);
      f = f * f * (3. - 2. * f);
      return mix(mix(hash(i), hash(i + vec2(1,0)), f.x),
                 mix(hash(i + vec2(0,1)), hash(i + vec2(1,1)), f.x), f.y);
    }
    float fbm(vec2 p) {
      float value = 0., amplitude = .52;
      mat2 turn = mat2(.80, -.60, .60, .80);
      for (int i = 0; i < 4; i++) {
        value += amplitude * noise(p);
        p = turn * p * 2.03 + 7.13;
        amplitude *= .5;
      }
      return value;
    }
    vec3 palette(int i) { return mix(uFrom[i], uTo[i], smoothstep(0., 1., uMix)); }

    void main() {
      vec2 p = (vUv * 2. - 1.);
      p.x *= uResolution.x / max(uResolution.y, 1.);
      float metaHalo = mix(uFromMeta.x, uToMeta.x, uMix);
      float metaRim = mix(uFromMeta.y, uToMeta.y, uMix);
      float seed = mix(uFromMeta.z, uToMeta.z, uMix);
      float still = mix(uFromMeta.w, uToMeta.w, uMix);
      float angle = atan(p.y, p.x);
      float motionGate = step(.001, uEnergy + uBreath + uTurbulence);
      float t = uTime * max(uTempo, .08) * motionGate + seed;
      vec2 direction = vec2(cos(angle), sin(angle));
      float n = fbm(direction * 1.72 + vec2(t * .31, -t * .23));
      float fine = fbm(p * 3.35 - vec2(t * .38, t * .31));
      float breathWave = sin(t * 1.18 + sin(t * .31) * .64);
      float lobe = sin(angle * 3. + t * .72 + n * 2.4);
      lobe += .56 * sin(angle * 5. - t * .48 + fine * 1.8);
      lobe += .28 * sin(angle * 7. + t * .27);
      lobe /= 1.84;
      float intakeWave = .5 + .5 * sin(angle * 3. - uTime * 5.4 + fine * 2.2);
      float emissionWave = .5 + .5 * sin(angle * 5. + length(p) * 24. - uTime * 7.6);
      float intake = uLevel * intakeWave;
      float emission = uSpeaking * emissionWave;
      float displacement = still * (
        (n - .5) * (.062 + .046 * uTurbulence)
        + lobe * (.026 + .046 * uEnergy)
        + breathWave * .025 * uBreath
      );
      displacement += intake * .07 + emission * .048;
      float radius = .69 + displacement + uLevel * .018 + uSpeaking * .026;
      float distanceToBody = length(p) - radius;
      float pixel = 2. / max(min(uResolution.x, uResolution.y), 1.);
      float body = 1. - smoothstep(-pixel, pixel * 1.45, distanceToBody);
      float angular = .5 + .5 * sin(angle * 1.62 + fine * 4.8 + t * .86);
      vec3 color = mix(palette(0), palette(1), angular);
      float aurora = smoothstep(.36, .88, fbm(p * 2.6 + vec2(-t * .22, t * .29)));
      color = mix(color, palette(2), aurora * (.42 + .18 * uEnergy));
      float sphereZ = sqrt(max(0., 1. - pow(length(p) / max(radius, .01), 2.)));
      float rim = pow(1. - sphereZ, 2.15) * metaRim;
      float inner = pow(max(sphereZ, 0.), 1.55) * (.18 + uLevel * .2 + uSpeaking * .12);
      float highlight = pow(max(0., dot(normalize(p + vec2(.0001)), normalize(vec2(-.72, .68))) * .5 + .5), 7.);
      color += palette(3) * (rim + inner);
      color += palette(2) * emission * (.16 + .3 * emissionWave);
      color += palette(3) * highlight * sphereZ * (.08 + .12 * uEnergy);
      float halo = exp(-max(distanceToBody, 0.) * 11.) * metaHalo;
      halo *= .86 + (.5 + .5 * breathWave) * .18 * uBreath + uLevel * .58 + uSpeaking * .78;
      float outerRing = exp(-abs(distanceToBody - .052) * 42.) * emission * .38;
      vec3 rgb = color * body + palette(3) * (halo * .35 + outerRing);
      float alpha = max(body, halo * .42 + outerRing);
      float sparkle = pow(max(0., noise(p * 19. + t) - .885), 7.) * body * (7. + uEnergy * 4.);
      rgb += vec3(sparkle);
      fragColor = vec4(rgb * alpha, clamp(alpha, 0., 1.));
    }
  `;

  function compile(context: WebGL2RenderingContext, type: number, source: string): WebGLShader {
    const shader = context.createShader(type);
    if (!shader) throw new Error("WebGL shader allocation failed");
    context.shaderSource(shader, source);
    context.compileShader(shader);
    if (!context.getShaderParameter(shader, context.COMPILE_STATUS)) {
      throw new Error(context.getShaderInfoLog(shader) ?? "WebGL shader compilation failed");
    }
    return shader;
  }

  function setPalette(prefix: "uFrom" | "uTo", palette: AuroraPalette) {
    if (!gl || !program) return;
    const colors = [palette.coreA, palette.coreB, palette.accent, palette.halo];
    colors.forEach((color, index) =>
      gl!.uniform3fv(
        gl!.getUniformLocation(program!, `${prefix}[${index}]`),
        new Float32Array(color),
      ),
    );
    gl.uniform4f(
      gl.getUniformLocation(program, `${prefix}Meta`),
      palette.haloStrength,
      palette.rimStrength,
      palette.seed,
      palette.seed === 0 ? 0 : 1,
    );
  }

  function resize() {
    if (!canvas || !gl) return;
    const bounds = canvas.getBoundingClientRect();
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const width = Math.max(1, Math.round(bounds.width * dpr));
    const height = Math.max(1, Math.round(bounds.height * dpr));
    if (canvas.width !== width || canvas.height !== height) {
      canvas.width = width;
      canvas.height = height;
      gl.viewport(0, 0, width, height);
    }
  }

  onMount(() => {
    canvas.addEventListener("webglcontextlost", handleContextLost);
    try {
      gl = canvas.getContext("webgl2", {
        alpha: true,
        antialias: true,
        premultipliedAlpha: true,
        powerPreference: "low-power",
      });
      if (!gl) throw new Error("WebGL2 unavailable");
      const vertex = compile(gl, gl.VERTEX_SHADER, vertexSource);
      const fragment = compile(gl, gl.FRAGMENT_SHADER, fragmentSource);
      program = gl.createProgram();
      if (!program) throw new Error("WebGL program allocation failed");
      gl.attachShader(program, vertex);
      gl.attachShader(program, fragment);
      gl.linkProgram(program);
      if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
        throw new Error(gl.getProgramInfoLog(program) ?? "WebGL link failed");
      }
      const buffer = gl.createBuffer();
      gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
      gl.bufferData(
        gl.ARRAY_BUFFER,
        new Float32Array([-1, -1, 1, -1, -1, 1, -1, 1, 1, -1, 1, 1]),
        gl.STATIC_DRAW,
      );
      gl.useProgram(program);
      const position = gl.getAttribLocation(program, "aPosition");
      gl.enableVertexAttribArray(position);
      gl.vertexAttribPointer(position, 2, gl.FLOAT, false, 0, 0);
      gl.enable(gl.BLEND);
      gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);

      let from = paletteFor(targetKey);
      let to = from;
      let renderedKey = targetKey;
      let transitionStarted = performance.now() - 520;
      let lastDraw = 0;
      let renderedMotion = orbMotionProfile(phase);
      const draw = (now: number) => {
        if (fallback) return;
        const active = ![null, "armed", "ended"].includes(phase);
        const interval = frameIntervalMs(active, reducedMotion);
        if (!frameIsDue(now, lastDraw, active, reducedMotion)) {
          frame = requestAnimationFrame(draw);
          return;
        }
        const elapsed = lastDraw ? now - lastDraw : 1000 / 60;
        lastDraw = now;
        resize();
        if (!gl || !program) return;
        if (renderedKey !== targetKey) {
          from = to;
          to = paletteFor(targetKey);
          renderedKey = targetKey;
          transitionStarted = now;
        }
        const mix = reducedMotion ? 1 : Math.min(1, (now - transitionStarted) / 520);
        const targetMotion = reducedMotion ? orbMotionProfile(null) : orbMotionProfile(phase);
        const motionEase = reducedMotion ? 1 : Math.min(1, Math.max(0, elapsed) / 180);
        renderedMotion = {
          energy: renderedMotion.energy + (targetMotion.energy - renderedMotion.energy) * motionEase,
          tempo: renderedMotion.tempo + (targetMotion.tempo - renderedMotion.tempo) * motionEase,
          breath: renderedMotion.breath + (targetMotion.breath - renderedMotion.breath) * motionEase,
          turbulence: renderedMotion.turbulence
            + (targetMotion.turbulence - renderedMotion.turbulence) * motionEase,
        };
        setPalette("uFrom", from);
        setPalette("uTo", to);
        gl.uniform1f(gl.getUniformLocation(program, "uMix"), mix);
        gl.uniform1f(gl.getUniformLocation(program, "uTime"), reducedMotion ? 0 : now / 1000);
        gl.uniform1f(gl.getUniformLocation(program, "uLevel"), inputLevel);
        gl.uniform1f(gl.getUniformLocation(program, "uSpeaking"), outputLevel);
        gl.uniform1f(gl.getUniformLocation(program, "uEnergy"), renderedMotion.energy);
        gl.uniform1f(gl.getUniformLocation(program, "uTempo"), renderedMotion.tempo);
        gl.uniform1f(gl.getUniformLocation(program, "uBreath"), renderedMotion.breath);
        gl.uniform1f(gl.getUniformLocation(program, "uTurbulence"), renderedMotion.turbulence);
        gl.uniform2f(gl.getUniformLocation(program, "uResolution"), canvas.width, canvas.height);
        gl.clearColor(0, 0, 0, 0);
        gl.clear(gl.COLOR_BUFFER_BIT);
        gl.drawArrays(gl.TRIANGLES, 0, 6);
        if (active && !reducedMotion) {
          frame = requestAnimationFrame(draw);
        } else {
          idleTimer = setTimeout(() => {
            frame = requestAnimationFrame(draw);
          }, interval);
        }
      };
      observer = new ResizeObserver(resize);
      observer.observe(canvas);
      frame = requestAnimationFrame(draw);
    } catch (error) {
      console.warn("Orb shader unavailable; using layered CSS fallback", error);
      fallback = true;
    }
  });

  $effect(() => {
    targetKey = paletteKey ?? "graphite";
  });

  onDestroy(() => {
    cancelAnimationFrame(frame);
    if (idleTimer) clearTimeout(idleTimer);
    observer?.disconnect();
    canvas?.removeEventListener("webglcontextlost", handleContextLost);
  });
</script>

<div
  class:shader-fallback={fallback}
  class:fallback-active={fallbackActive}
  class="orb-renderer"
  data-palette={paletteKey ?? "graphite"}
  data-phase={phase ?? "off"}
  style={`--orb-level:${Math.max(inputLevel, outputLevel)};--orb-bloom:${1 + fallbackMotion.energy * 0.07}`}
  aria-hidden="true"
>
  <canvas bind:this={canvas}></canvas>
  {#if fallback}<div class="fallback-core"></div>{/if}
</div>

<style>
  .orb-renderer, canvas { width: 100%; height: 100%; display: block; }
  .orb-renderer { position: relative; filter: saturate(1.14) contrast(1.025); contain: strict; }
  .shader-fallback .fallback-core {
    position: absolute; inset: 7%; overflow: hidden;
    border-radius: 44% 56% 52% 48% / 55% 43% 57% 45%;
    background: conic-gradient(from 25deg, #8d61fa, #466ff4, #ee70cd, #8d61fa);
    box-shadow: 0 0 calc(24px + 28px * var(--orb-level)) #8063ff70,
      inset -18px -22px 32px #201834a0, inset 10px 9px 22px #fff5;
    will-change: transform, border-radius;
  }
  .fallback-core::before,
  .fallback-core::after {
    content: ""; position: absolute; inset: -28%; border-radius: 42% 58% 63% 37%;
    background: conic-gradient(from 140deg, transparent 0 14%, #fff5 24%, transparent 38% 56%, #fff3 70%, transparent 82%);
    mix-blend-mode: screen; opacity: .58;
  }
  .fallback-core::after { inset: 18%; opacity: .34; filter: blur(7px); }
  .fallback-active .fallback-core { animation: fallback-breathe 4.8s ease-in-out infinite alternate; }
  .fallback-active .fallback-core::before { animation: fallback-flow 7.2s linear infinite; }
  .fallback-active .fallback-core::after { animation: fallback-flow 5.4s linear infinite reverse; }
  .orb-renderer[data-phase="armed"] .fallback-core { animation-duration: 6.4s; }
  .orb-renderer[data-phase="heard"] .fallback-core { animation-duration: 1.15s; }
  .orb-renderer[data-phase="thinking"] .fallback-core { animation-duration: 2.7s; }
  .orb-renderer[data-phase="speaking"] .fallback-core { animation-duration: .92s; }
  .orb-renderer[data-palette="armed_ember"] .fallback-core {
    background: conic-gradient(from 25deg, #ff784f, #8f52ff, #ffc26f, #ff784f);
    box-shadow: 0 0 calc(24px + 28px * var(--orb-level)) #ff714d66, inset -18px -22px 32px #30172ca0, inset 10px 9px 22px #fff5;
  }
  .orb-renderer[data-palette="violet_surge"] .fallback-core {
    background: conic-gradient(from 25deg, #9c6dff, #5271ff, #ff74d3, #9c6dff);
  }
  .orb-renderer[data-palette="calm_aurora"] .fallback-core {
    background: conic-gradient(from 25deg, #5bd9c5, #657cff, #ad73ee, #5bd9c5);
  }
  .orb-renderer[data-palette="amber_thinking"] .fallback-core {
    background: conic-gradient(from 25deg, #ffb54c, #ec6f5e, #7d54d9, #ffb54c);
  }
  .orb-renderer[data-palette="teal_speaking"] .fallback-core {
    background: conic-gradient(from 25deg, #35e2c1, #298cd9, #81f2c3, #35e2c1);
  }
  .orb-renderer[data-palette="graphite"] .fallback-core {
    background: conic-gradient(from 25deg, #676274, #353442, #8a8294, #676274);
    box-shadow: 0 0 calc(16px + 18px * var(--orb-level)) #77708048, inset -18px -22px 32px #17151ca0, inset 10px 9px 22px #fff3;
  }
  @keyframes fallback-breathe {
    0% { transform: scale(.94) rotate(-3deg); border-radius: 44% 56% 52% 48% / 55% 43% 57% 45%; }
    48% { transform: scale(var(--orb-bloom)) rotate(3deg); border-radius: 58% 42% 45% 55% / 43% 57% 47% 53%; }
    100% { transform: scale(.97) rotate(8deg); border-radius: 49% 51% 61% 39% / 58% 41% 59% 42%; }
  }
  @keyframes fallback-flow { to { transform: rotate(360deg) scale(1.08); } }
  @media (prefers-reduced-motion: reduce) { .shader-fallback .fallback-core { animation: none; } }
</style>
