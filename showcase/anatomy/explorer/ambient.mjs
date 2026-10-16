// Injected into the standalone explorer by build.mjs. A separate, low-resolution
// shader canvas lets the expensive model renderer keep sleeping while sealed.
export function createAmbientBackground({ THREE, stage, still, lite, disabled }) {
  const idle = { setTheme() {}, setExpanded() {}, resize() {}, dispose() {}, get state() { return { enabled: false, frames: 0 }; } };
  if (disabled) return idle;
  const canvas = document.createElement('canvas');
  canvas.id = 'ambientBackground';
  canvas.className = 'ambient-background';
  canvas.setAttribute('aria-hidden', 'true');
  document.body.insertBefore(canvas, stage);
  let renderer;
  try {
    renderer = new THREE.WebGLRenderer({ canvas, antialias: true, powerPreference: 'low-power' });
  } catch {
    canvas.remove();
    return idle; // the existing solid theme background remains available
  }
  renderer.setPixelRatio(lite ? 1 : Math.min(window.devicePixelRatio||1,2));
  let shaderFailed = false;
  renderer.debug.onShaderError = () => { shaderFailed = true; };
  const palettes = {
    paper: [0xf4f4f1, 0x52616e, 0xc59c55, 0xd68a16],
    sorbet: [0xfff9f4, 0x9f64ce, 0xffb584, 0xed638f],
    night: [0x0a0a0f, 0x42687c, 0x1ba398, 0x9cf5de],
    blueprint: [0x0b2340, 0x5987b4, 0x74b4e5, 0xd2f0ff]
  };
  const keys = Object.keys(palettes);
  const uniforms = {
    uResolution: { value: new THREE.Vector2(1, 1) },
    uPixelRatio: { value: renderer.getPixelRatio() },
    tFlow: { value: null },
    uTime: { value: 0 }, uMotion: { value: still.matches ? 0 : 1 },
    uPointer: { value: new THREE.Vector3(0, 0, 0) },
    uStyle: { value: new THREE.Vector4(1, 0, 0, 0) },
    uBg: { value: new THREE.Color(palettes.paper[0]) },
    uTrace: { value: new THREE.Color(palettes.paper[1]) },
    uGlow: { value: new THREE.Color(palettes.paper[2]) },
    uPulse: { value: new THREE.Color(palettes.paper[3]) }
  };
  const material = new THREE.ShaderMaterial({
    uniforms, depthTest: false, depthWrite: false, extensions: { derivatives: true },
    vertexShader: `varying vec2 vUv;
      void main() { vUv = uv; gl_Position = vec4(position.xy, 0.0, 1.0); }`,
    fragmentShader: `
      precision highp float;
      varying vec2 vUv;
      uniform vec2 uResolution;
      uniform float uTime, uMotion;
      uniform vec3 uPointer;
      uniform vec4 uStyle;
      uniform vec3 uBg, uTrace, uGlow, uPulse;
      uniform sampler2D tFlow;
      vec3 paper(vec2 p, float reveal) {
        float t = uTime*0.1*uMotion;
        vec2 q = mat2(0.94,-0.34,0.34,0.94)*p;
        // Magnetic ink filaments bend around the pointer, with gold charges.
        q.y += reveal*0.1*sin((p.x-uPointer.x)*12.0);
        float field = q.y+0.14*sin(q.x*3.0+t)+0.055*sin(q.x*7.0-t);
        float lanes = field*34.0;
        float d = abs(fract(lanes)-0.5)/max(fwidth(lanes),0.0001);
        float strand = 1.0-smoothstep(0.2,1.35,d);
        float fine = 1.0-smoothstep(0.4,1.0,d);
        float charge = pow(max(0.0,sin(q.x*15.0+floor(lanes)*1.7-t*4.0)),24.0);
        vec3 col = mix(uBg,uTrace,strand*(0.018+reveal*0.46));
        col = mix(col,uGlow,strand*reveal*(0.2+0.2*sin(field*18.0+t)));
        return mix(col,uPulse,fine*charge*reveal*0.78);
      }
      vec3 sorbet(vec2 p, float reveal) {
        float t = uTime*0.08*uMotion;
        vec2 flow = p+vec2(sin(p.y*3.0+t),cos(p.x*2.6-t))*0.16;
        flow += (p-uPointer.xy)*reveal*0.22;
        float field = flow.y*3.1+sin(flow.x*3.7+t)*0.7;
        field += 0.24*sin(flow.x*8.0-flow.y*2.0-t);
        float ribbons = pow(0.5+0.5*sin(field*7.0),5.0);
        float edge = abs(fract(field*1.114)-0.5)/max(fwidth(field*1.114),0.0001);
        float silk = 1.0-smoothstep(0.35,1.4,edge);
        vec3 tint = mix(uTrace,uGlow,0.5+0.5*sin(field*2.0+flow.x*3.0-t));
        tint = mix(tint,uPulse,0.28+0.25*sin(flow.x*4.0+field));
        vec3 col = mix(uBg,tint,ribbons*(0.025+reveal*0.62));
        return mix(col,tint,silk*(0.02+reveal*0.45));
      }
      float noise(vec2 p) {
        vec2 cell = floor(p), f = fract(p);
        f = f*f*(3.0-2.0*f);
        vec4 h = fract(sin(vec4(dot(cell,vec2(127.1,311.7)),
          dot(cell+vec2(1.0,0.0),vec2(127.1,311.7)),
          dot(cell+vec2(0.0,1.0),vec2(127.1,311.7)),
          dot(cell+1.0,vec2(127.1,311.7))))*43758.5453);
        return mix(mix(h.x,h.y,f.x),mix(h.z,h.w,f.x),f.y);
      }
      float fbm(vec2 p) {
        float n = 0.0, amplitude = 0.55;
        for (int i=0;i<3;i++) {
          n += noise(p)*amplitude;
          p = mat2(1.6,-1.2,1.2,1.6)*p+3.7;
          amplitude *= 0.5;
        }
        return n;
      }
      vec3 night(vec2 p, float reveal) {
        vec3 flow = texture2D(tFlow,vUv).rgb;
        float wake = smoothstep(0.025,0.65,flow.b);
        float reach = min(1.0,reveal*0.75+wake*0.75);
        if (reach<0.002) return uBg;
        float t = uTime*0.12*uMotion;
        vec2 q = p*4.0+(flow.rg-128.0/255.0)*3.0;
        vec2 warp = vec2(fbm(q+vec2(t,0.0)),fbm(q+vec2(7.3,-t)));
        float field = fbm(q+warp*2.8+vec2(t,-t*0.7));
        float cloud = pow(smoothstep(0.18,0.82,field),1.8);
        float wisp = pow(max(0.0,1.0-abs(field-0.48)*3.5),12.0);
        vec3 light = uGlow*(cloud*0.12+wisp*0.035);
        light += uPulse*wisp*wisp*0.045;
        return uBg+light*reach;
      }
      void main() {
        vec2 p = (vUv - 0.5) * uResolution / uResolution.y;
        vec2 delta = (p-uPointer.xy)*uResolution.y;
        float haloRadius = clamp(uResolution.y*0.24,150.0,240.0);
        float reveal = exp(-dot(delta,delta)/(haloRadius*haloRadius))*uPointer.z;
        float margins = 0.25 + 0.75 * smoothstep(0.15, 0.7, length(p * vec2(0.8, 1.0)));
        float quiet = smoothstep(0.0, 0.13, vUv.y) * smoothstep(0.0, 0.12, 1.0-vUv.y);
        vec3 col = uBg;
        // Each theme owns its geometry; weights crossfade only during a change.
        if (uStyle.x > 0.001) col += (paper(p,reveal)-uBg)*uStyle.x;
        if (uStyle.y > 0.001) col += (sorbet(p,reveal)-uBg)*uStyle.y;
        if (uStyle.z > 0.001) col += (night(p,reveal)-uBg)*uStyle.z;
        col = mix(uBg,col,margins*quiet);
        gl_FragColor = vec4(col,1.0);
        #include <colorspace_fragment>
      }`
  });
  const scene = new THREE.Scene();
  const geometry = new THREE.PlaneGeometry(2, 2);
  const backdrop = new THREE.Mesh(geometry, material);
  backdrop.frustumCulled = false;
  backdrop.renderOrder = -1;
  scene.add(backdrop);
  const camera = new THREE.Camera();

  // A small feedback texture advects cursor velocity and dye. It gives Night
  // a fading wake instead of moving a fixed decorative pattern under a mask.
  const flowTargets = [0,1].map(()=>new THREE.WebGLRenderTarget(1,1,{
    minFilter:THREE.LinearFilter,magFilter:THREE.LinearFilter,
    type:THREE.UnsignedByteType,depthBuffer:false,stencilBuffer:false
  }));
  const flowUniforms = {
    ...uniforms, tSource:{value:null}, uDelta:{value:0}, uReset:{value:1},
    uFrom:{value:new THREE.Vector2()}, uVelocity:{value:new THREE.Vector2()}
  };
  const flowMaterial = new THREE.ShaderMaterial({
    uniforms:flowUniforms, depthTest:false,depthWrite:false,
    vertexShader:material.vertexShader,
    fragmentShader:`
      varying vec2 vUv;
      uniform sampler2D tSource;
      uniform vec2 uResolution,uFrom,uVelocity;
      uniform vec3 uPointer;
      uniform float uDelta,uTime,uReset;
      void main() {
        const float zero = 128.0/255.0;
        if (uReset>0.5) { gl_FragColor=vec4(zero,zero,0.0,1.0); return; }
        vec2 aspect = vec2(uResolution.x/uResolution.y,1.0);
        vec2 p = (vUv-0.5)*aspect;
        vec2 velocity = (texture2D(tSource,vUv).rg-zero)*1.2;
        // A smooth divergence-free field carries and curls the deposited dye.
        float a = p.x*5.0+p.y*4.0+uTime*0.22;
        float b = p.x*3.0-p.y*6.0-uTime*0.17;
        vec2 current = vec2(4.0*cos(a)-3.0*cos(b),-5.0*cos(a)-1.5*cos(b))*0.006;
        vec2 uv = vUv-(velocity+current)*uDelta/aspect;
        vec3 advected = texture2D(tSource,clamp(uv,0.0,1.0)).rgb;
        velocity = (advected.rg-zero)*1.2*exp(-uDelta*2.8);
        vec2 ab = uPointer.xy-uFrom;
        vec2 nearPoint = uFrom+ab*clamp(dot(p-uFrom,ab)/max(dot(ab,ab),0.000001),0.0,1.0);
        vec2 delta = p-nearPoint;
        float touch = exp(-dot(delta,delta)/0.0045)*uPointer.z;
        velocity += uVelocity*touch*uDelta*3.5;
        velocity += vec2(-delta.y,delta.x)*touch*uDelta*1.8;
        velocity = clamp(velocity,vec2(-0.55),vec2(0.55));
        float dye = max(0.0,advected.b*exp(-uDelta*2.1)-uDelta*0.06);
        dye += touch*(0.18+min(length(uVelocity)*0.45,1.0))*uDelta*5.0;
        gl_FragColor = vec4(velocity/1.2+zero,clamp(dye,0.0,1.0),1.0);
      }`
  });
  const flowScene = new THREE.Scene();
  const flowQuad = new THREE.Mesh(geometry,flowMaterial);
  flowQuad.frustumCulled = false;
  flowScene.add(flowQuad);
  let flowIndex = 0, resetFlow = true, simulationFrames = 0;
  uniforms.tFlow.value = flowTargets[flowIndex].texture;

  // Blueprint: independent electric flecks travel through a coherent current.
  // There are no connections, wire forms, tiled cells or rigid geometry.
  const particleCount = lite ? 180 : 580;
  const particleSeeds = new Float32Array(particleCount*4);
  let seed = 1741;
  const random = () => { seed = (Math.imul(seed,1664525)+1013904223)>>>0; return seed/4294967296; };
  for (let i=0;i<particleSeeds.length;i++) particleSeeds[i] = random();
  const particleGeometry = new THREE.BufferGeometry();
  particleGeometry.setAttribute('position',new THREE.BufferAttribute(new Float32Array(particleCount*3),3));
  particleGeometry.setAttribute('aSeed',new THREE.BufferAttribute(particleSeeds,4));
  const particleMaterial = new THREE.ShaderMaterial({
    uniforms,transparent:true,depthTest:false,depthWrite:false,
    vertexShader:`
      attribute vec4 aSeed;
      uniform vec2 uResolution;
      uniform vec3 uPointer;
      uniform float uTime,uMotion,uPixelRatio;
      varying float vReveal,vBrightness,vWidth;
      varying vec2 vDirection;
      vec2 flow(float time) {
        float span = uResolution.x/uResolution.y+0.4;
        float depth = 0.35+aSeed.z*0.65;
        float x = mod(aSeed.x*span+time*0.11*depth,span)-span*0.5;
        float y = (aSeed.y-0.5)*1.12;
        y += 0.075*sin(x*3.2+y*5.0+time*0.09);
        y += 0.035*sin(x*6.1-y*7.0-time*0.12);
        vec2 p = vec2(x,y), delta = p-uPointer.xy;
        float pull = exp(-dot(delta,delta)/0.032)*uPointer.z;
        p += vec2(-delta.y,delta.x)*pull*0.65;
        p += delta*pull*0.35;
        return p;
      }
      void main() {
        float t = uTime*uMotion;
        vec2 p = flow(t), previous = flow(t-0.3);
        vec2 delta = (p-uPointer.xy)*uResolution.y;
        float radius = clamp(uResolution.y*0.24,150.0,240.0);
        vReveal = exp(-dot(delta,delta)/(radius*radius))*uPointer.z;
        vDirection = normalize(p-previous+vec2(0.00001,0.0));
        float quiet = smoothstep(0.0,0.12,p.y+0.5)*smoothstep(0.0,0.12,0.5-p.y);
        vBrightness = (0.55+aSeed.w*0.45)*quiet;
        vWidth = 0.25+aSeed.z*0.16;
        gl_Position = vec4(p*vec2(uResolution.y/uResolution.x,1.0)*2.0,0.0,1.0);
        gl_PointSize = (4.0+aSeed.z*4.0+vReveal*9.0)*uPixelRatio;
      }`,
    fragmentShader:`
      uniform vec3 uGlow,uPulse;
      uniform vec4 uStyle;
      varying float vReveal,vBrightness,vWidth;
      varying vec2 vDirection;
      void main() {
        vec2 p = (gl_PointCoord-0.5)*2.0;
        // Point coordinates run downward; the current's coordinates run upward.
        p.y = -p.y;
        vec2 q = vec2(dot(p,vDirection),dot(p,vec2(-vDirection.y,vDirection.x)));
        float d = length((q-vec2(0.32,0.0))/vec2(0.2,vWidth*0.5));
        float core = exp(-d*d*3.0);
        float along = (q.x+0.13)/0.6, across = q.y/vWidth;
        float tail = exp(-along*along-across*across*5.0);
        tail *= 1.0-smoothstep(0.65,1.0,length(p));
        float alpha = (core+tail*0.46)*(0.012+vReveal*0.88)*vBrightness*uStyle.w;
        gl_FragColor = vec4(mix(uGlow,uPulse,core),alpha);
        #include <colorspace_fragment>
      }`
  });
  const particles = new THREE.Points(particleGeometry,particleMaterial);
  particles.frustumCulled = false;
  scene.add(particles);
  renderer.info.autoReset = false;
  const pointer = new THREE.Vector3(0, 0, 0);
  const pointerBefore = new THREE.Vector3();
  const targetStyle = new THREE.Vector4(1, 0, 0, 0);
  const targetColors = palettes.paper.map(hex => new THREE.Color(hex));
  let theme = 'paper', themed = false, expanded = false, disposed = false, frame = 0;
  let lastTick = 0, lastDraw = 0, elapsed = 0, drawn = 0, dirty = true;
  let width = 0, height = 0;

  function schedule() {
    if (!frame && !disposed && !expanded && !document.hidden) frame = requestAnimationFrame(draw);
  }
  function draw(t) {
    frame = 0;
    if (disposed || expanded || document.hidden) return;
    const dt = lastTick ? Math.min(t-lastTick,100) : 16;
    lastTick = t;
    // Read the current media state before the final static draw as well: the
    // browser may update matches before delivering its change event.
    uniforms.uMotion.value = still.matches ? 0 : 1;
    if (still.matches) pointer.z = 0;
    if (!still.matches) elapsed += dt/1000;
    if (dirty || still.matches || t-lastDraw >= (lite ? 1000/18 : 1000/30)) {
      const blend = still.matches ? 1 : 1-Math.exp(-Math.max(16,t-lastDraw)/70);
      pointerBefore.copy(uniforms.uPointer.value);
      uniforms.uStyle.value.lerp(targetStyle,blend);
      ['uBg','uTrace','uGlow','uPulse'].forEach((key,i) => uniforms[key].value.lerp(targetColors[i],blend));
      uniforms.uPointer.value.lerp(pointer,blend);
      uniforms.uTime.value = elapsed;
      renderer.info.reset();
      if (uniforms.uStyle.value.z>0.001) {
        const step = Math.min(0.08,Math.max(0.001,(t-lastDraw)/1000));
        flowUniforms.uDelta.value = step;
        flowUniforms.uReset.value = resetFlow||lite||still.matches ? 1 : 0;
        const current = uniforms.uPointer.value;
        flowUniforms.uFrom.value.set(pointerBefore.z>0.01?pointerBefore.x:current.x,pointerBefore.z>0.01?pointerBefore.y:current.y);
        flowUniforms.uVelocity.value.set(current.x-flowUniforms.uFrom.value.x,current.y-flowUniforms.uFrom.value.y).divideScalar(step).clampLength(0,2.5);
        flowUniforms.tSource.value = flowTargets[flowIndex].texture;
        renderer.setRenderTarget(flowTargets[1-flowIndex]);
        renderer.render(flowScene,camera);
        renderer.setRenderTarget(null);
        flowIndex = 1-flowIndex;
        uniforms.tFlow.value = flowTargets[flowIndex].texture;
        resetFlow = false; simulationFrames++;
      }
      particles.visible = uniforms.uStyle.value.w>0.001;
      renderer.render(scene,camera);
      if (shaderFailed) { dispose(); return; }
      lastDraw = t; dirty = false; drawn++;
    }
    if (!still.matches) schedule();
  }
  function resize(w, h) {
    w = Math.max(1,w); h = Math.max(1,h);
    if (width === w && height === h) return;
    width = w; height = h;
    renderer.setSize(w,h,false);
    uniforms.uResolution.value.set(w,h);
    const scale = Math.min(1,(lite?256:512)/Math.max(w,h));
    flowTargets.forEach(target=>target.setSize(Math.max(1,Math.round(w*scale)),Math.max(1,Math.round(h*scale))));
    resetFlow = true;
    dirty = true; schedule();
  }
  function setTheme(id) {
    if (id!==theme) resetFlow = true;
    theme = keys.includes(id) ? id : 'paper';
    targetStyle.set(...keys.map(key => key === theme ? 1 : 0));
    palettes[theme].forEach((hex,i) => targetColors[i].set(hex));
    if (!themed || expanded || still.matches) {
      uniforms.uStyle.value.copy(targetStyle);
      ['uBg','uTrace','uGlow','uPulse'].forEach((key,i) => uniforms[key].value.copy(targetColors[i]));
    }
    themed = true;
    dirty = true; schedule();
  }
  function setExpanded(value) {
    expanded = value;
    canvas.style.opacity = value ? '0' : '1';
    canvas.dataset.expanded = String(value);
    if (value) { cancelAnimationFrame(frame); frame = 0; resetFlow = true; }
    else { lastTick = 0; dirty = true; schedule(); }
  }
  const move = e => {
    if (lite || still.matches || expanded || e.pointerType === 'touch') return;
    pointer.set((e.clientX-width/2)/height,(height/2-e.clientY)/height,1);
  };
  const leave = () => { pointer.z = 0; };
  const visibility = () => {
    cancelAnimationFrame(frame); frame = 0; lastTick = 0;
    if (document.hidden) { pointer.z = 0; uniforms.uPointer.value.z = 0; resetFlow = true; }
    if (!document.hidden) { dirty = true; schedule(); }
  };
  const motion = () => {
    uniforms.uMotion.value = still.matches ? 0 : 1;
    if (still.matches) { pointer.z = 0; uniforms.uPointer.value.z = 0; }
    setTheme(theme);
  };
  function dispose() {
    if (disposed) return;
    disposed = true; cancelAnimationFrame(frame);
    window.removeEventListener('pointermove',move);
    window.removeEventListener('blur',leave);
    document.documentElement.removeEventListener('pointerleave',leave);
    document.removeEventListener('visibilitychange',visibility);
    still.removeEventListener('change',motion);
    window.removeEventListener('pagehide',pagehide);
    [geometry,particleGeometry].forEach(g=>g.dispose());
    [material,flowMaterial,particleMaterial].forEach(m=>m.dispose());
    flowTargets.forEach(target=>target.dispose());
    renderer.dispose(); canvas.remove();
  }
  const pagehide = e => { if (!e.persisted) dispose(); };
  window.addEventListener('pointermove',move,{ passive:true });
  window.addEventListener('blur',leave);
  document.documentElement.addEventListener('pointerleave',leave);
  document.addEventListener('visibilitychange',visibility);
  still.addEventListener('change',motion);
  window.addEventListener('pagehide',pagehide);
  resize(window.innerWidth,window.innerHeight);
  return { resize,setTheme,setExpanded,dispose,
    get state() { return { enabled:!disposed,theme,pattern:{paper:'magnetic-filaments',sorbet:'iridescent-ribbons',night:'fluid-light',blueprint:'particle-current'}[theme],expanded,frames:drawn,motion:!still.matches,lite,particles:particleCount,simulationFrames,drawCalls:renderer.info.render.calls,pointerReveal:uniforms.uPointer.value.z,shaderFailed }; }
  };
}
