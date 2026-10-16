(function() {
    // ONLY render in the top frame to avoid multiple cursors in iframes
    if (window !== window.top) return;

    console.log('[Magicutor Overlay] Script starting...', window.location.href);

    // Prevent multiple injections
    if (window.__magicutorOverlayInitialized) {
        console.log('[Magicutor Overlay] Already initialized, skipping');
        return;
    }
    window.__magicutorOverlayInitialized = true;

    // SVG Icons
    const ICONS = {
        cursor: `<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><path d="M3.8 5.4 Q3.2 3.2 5.4 3.8 L15.5 6.5 Q17.7 7.1 15.5 7.8 L8.4 10.3 Q9.8 9.8 9.3 11.2 L7.8 15.5 Q7.1 17.7 6.5 15.5 L3.8 5.4 Z"/></svg>`,
        click: '🖱️', type: '⌨️', scroll: '↕️', search: '🔍', wait: '⏳', success: '✓', error: '✗', locked: '🔒',
        thinking: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10" /><path d="M12 6v6l4 2" /></svg>`
    };
    const RESUME_SUPPRESSION_MS = 3000;

    // CSS Styles
    const OVERLAY_CSS = `
    :host {
      --cursor-fill: #111111; --cursor-stroke: #ffffff; --cursor-hue: rgba(186, 198, 255, 0.42);
      --spotlight-dim: rgba(0, 0, 0, 0.12); --spotlight-glow: rgba(99, 102, 241, 0.15);
      --ripple-color: rgba(99, 102, 241, 0.4); --success-color: #22c55e; --error-color: #ef4444;
      --bubble-bg: #ffffff; --bubble-border: #e5e7eb; --bubble-text: #111827;
      --bubble-shadow: 0 4px 6px -1px rgba(0, 0, 0, 0.1);
    }
    .overlay-container { position: fixed; top: 0; left: 0; width: 100vw; height: 100vh; pointer-events: none; z-index: 2147483647; overflow: hidden; font-family: system-ui, sans-serif; opacity: 0; transition: opacity 0.4s ease; }
    .overlay-container.visible { opacity: 1; }
    
    /* Aurora Border - Top & Left edges (Teal/Cyan) */
    .overlay-container::before {
        content: ''; position: absolute; inset: 0; pointer-events: none;
        background:
            linear-gradient(to bottom, rgba(45,212,191,0.7) 0%, rgba(45,212,191,0.3) 2%, rgba(45,212,191,0) 8%),
            linear-gradient(to right, rgba(45,212,191,0.6) 0%, rgba(45,212,191,0.2) 2%, rgba(45,212,191,0) 6%),
            radial-gradient(ellipse 50% 40% at 0% 0%, rgba(45,212,191,0.8) 0%, rgba(45,212,191,0.3) 40%, transparent 70%);
        filter: blur(8px);
        animation: aurora-pulse-1 2.5s ease-in-out infinite;
    }

    /* Aurora Border - Bottom & Right edges (Violet) */
    .overlay-container::after {
        content: ''; position: absolute; inset: 0; pointer-events: none;
        background:
            linear-gradient(to top, rgba(167,139,250,0.7) 0%, rgba(167,139,250,0.3) 2%, rgba(167,139,250,0) 8%),
            linear-gradient(to left, rgba(167,139,250,0.6) 0%, rgba(167,139,250,0.2) 2%, rgba(167,139,250,0) 6%),
            radial-gradient(ellipse 50% 40% at 100% 100%, rgba(167,139,250,0.8) 0%, rgba(167,139,250,0.3) 40%, transparent 70%);
        filter: blur(8px);
        animation: aurora-pulse-2 3s ease-in-out infinite;
    }

    .spotlight-layer { display: none; }
    
    .cursor { position: absolute; top: 0; left: 0; width: 22px; height: 22px; transform: translate(var(--x, 0px), var(--y, 0px)); will-change: transform; }
    .cursor::before { content: ''; position: absolute; left: -8.5px; top: -8.5px; width: 34px; height: 34px; border-radius: 50%; background: radial-gradient(circle, rgba(236, 240, 255, 0.55) 0%, var(--cursor-hue) 40%, rgba(176, 168, 255, 0.16) 62%, transparent 78%); z-index: -1; }
    .cursor svg { width: 100%; height: 100%; fill: var(--cursor-fill); stroke: var(--cursor-stroke); stroke-width: 3.4; stroke-linejoin: round; stroke-linecap: round; paint-order: stroke fill; }
    .cursor.error { animation: shake 0.4s both; }
    
    .thought-bubble { display: none !important; }
    .thought-bubble.visible { opacity: 1; scale: 1; }
    .thought-bubble.success { background: #DCFCE7; border-color: #4ADE80; color: #166534; }
    .thought-bubble.error { background: #FEE2E2; border-color: #F87171; color: #991B1B; }
    .thought-bubble .bubble-content { display: flex; align-items: center; gap: 6px; }
    .thought-bubble .bubble-content.slide-in { animation: bubble-slide-in 0.25s ease-out forwards; }
    .thought-bubble .bubble-content.slide-out { animation: bubble-slide-out 0.2s ease-in forwards; }
    @keyframes bubble-slide-in { 0% { transform: translateY(-100%); opacity: 0; } 100% { transform: translateY(0); opacity: 1; } }
    @keyframes bubble-slide-out { 0% { transform: translateY(0); opacity: 1; } 100% { transform: translateY(100%); opacity: 0; } }
    
    .ripple { position: absolute; border-radius: 50%; background: var(--ripple-color); transform: translate(-50%, -50%) scale(0); animation: ripple-anim 0.6s ease-out forwards; }
    .particle { position: absolute; width: 4px; height: 4px; background: var(--cursor-fill); border-radius: 50%; }
    .ghost-char { position: absolute; font-size: 14px; font-weight: bold; color: var(--cursor-fill); opacity: 0; }
    
    @keyframes ripple-anim { to { transform: translate(-50%, -50%) scale(4); opacity: 0; } }
    @keyframes shake { 10%, 90% { transform: translate(calc(var(--x) - 1px), var(--y)); } 20%, 80% { transform: translate(calc(var(--x) + 2px), var(--y)); } 50% { transform: translate(calc(var(--x) - 4px), var(--y)); } }
    @keyframes float-up { 0% { transform: translateY(0); opacity: 1; } 100% { transform: translateY(-20px); opacity: 0; } }
    /* Pulsating border glow - breathing effect */
    @keyframes aurora-pulse-1 {
        0%, 100% { opacity: 0.5; filter: blur(8px) brightness(1); }
        50% { opacity: 1; filter: blur(4px) brightness(1.4); }
    }
    @keyframes aurora-pulse-2 {
        0%, 100% { opacity: 0.5; filter: blur(8px) brightness(1); }
        50% { opacity: 1; filter: blur(4px) brightness(1.4); }
    }

    /* Closing animation - aurora gathers to center */
    .overlay-container.closing::before,
    .overlay-container.closing::after {
        animation: aurora-gather 0.8s ease-in-out forwards !important;
    }
    @keyframes aurora-gather {
        0% { opacity: 1; transform: scale(1); filter: blur(8px); }
        100% { opacity: 0; transform: scale(0.1); filter: blur(2px); }
    }

    /* Closing orb - appears at center when aurora gathers */
    .closing-orb {
        position: fixed;
        top: 50%;
        left: 50%;
        width: 80px;
        height: 80px;
        transform: translate(-50%, -50%) scale(0);
        border-radius: 50%;
        opacity: 0;
        pointer-events: none;
        z-index: 10;
    }
    .closing-orb.success {
        background: radial-gradient(circle, rgba(34, 197, 94, 0.9) 0%, rgba(34, 197, 94, 0.4) 50%, transparent 70%);
        box-shadow: 0 0 60px rgba(34, 197, 94, 0.8), 0 0 120px rgba(34, 197, 94, 0.4);
    }
    .closing-orb.error {
        background: radial-gradient(circle, rgba(239, 68, 68, 0.9) 0%, rgba(239, 68, 68, 0.4) 50%, transparent 70%);
        box-shadow: 0 0 60px rgba(239, 68, 68, 0.8), 0 0 120px rgba(239, 68, 68, 0.4);
    }
    .closing-orb.active {
        animation: orb-appear 0.6s ease-out forwards, orb-pulse 0.4s ease-in-out 0.6s 2, orb-fade 0.5s ease-out 1.4s forwards;
    }
    @keyframes orb-appear {
        0% { opacity: 0; transform: translate(-50%, -50%) scale(0); }
        100% { opacity: 1; transform: translate(-50%, -50%) scale(1); }
    }
    @keyframes orb-pulse {
        0%, 100% { transform: translate(-50%, -50%) scale(1); }
        50% { transform: translate(-50%, -50%) scale(1.2); }
    }
    @keyframes orb-fade {
        0% { opacity: 1; transform: translate(-50%, -50%) scale(1); }
        100% { opacity: 0; transform: translate(-50%, -50%) scale(0.5); }
    }

    /* Floating Status Panel */
    .status-panel {
        position: fixed;
        bottom: 20px;
        left: 50%;
        transform: translateX(-50%) translateY(100px);
        background: linear-gradient(135deg, rgba(139, 92, 246, 0.25) 0%, rgba(167, 139, 250, 0.2) 100%);
        backdrop-filter: blur(16px);
        border: 1px solid rgba(167, 139, 250, 0.4);
        border-radius: 12px;
        padding: 10px 16px;
        display: flex;
        align-items: center;
        gap: 8px;
        box-shadow: 0 8px 32px rgba(139, 92, 246, 0.2), 0 0 0 1px rgba(255,255,255,0.08) inset;
        pointer-events: none;
        opacity: 0;
        transition: transform 0.4s cubic-bezier(0.34, 1.56, 0.64, 1), opacity 0.3s ease;
        z-index: 10;
        min-width: 0;
        max-width: 400px;
    }
    .status-panel.visible {
        transform: translateX(-50%) translateY(0);
        opacity: 1;
        pointer-events: auto;
    }
    .status-panel .stop-btn {
        background: linear-gradient(135deg, #EF4444 0%, #DC2626 100%);
        border: none;
        border-radius: 6px;
        padding: 8px;
        color: white;
        font-size: 12px;
        font-weight: 600;
        cursor: pointer;
        transition: all 0.2s ease;
        display: flex;
        align-items: center;
        justify-content: center;
    }
    .status-panel .stop-btn:hover {
        background: linear-gradient(135deg, #F87171 0%, #EF4444 100%);
        transform: scale(1.05);
    }
    .status-panel .stop-btn:active {
        transform: scale(0.98);
    }
    .status-panel .stop-btn .stop-icon {
        width: 11px;
        height: 11px;
        background: white;
        border-radius: 2px;
    }
    .status-panel .resume-btn {
        background: linear-gradient(135deg, #16a34a 0%, #15803d 100%);
        border: none;
        border-radius: 6px;
        padding: 6px 12px;
        color: white;
        font-size: 12px;
        font-weight: 600;
        cursor: pointer;
        transition: all 0.2s ease;
        display: none;
    }
    .status-panel .resume-btn:hover {
        background: linear-gradient(135deg, #22c55e 0%, #16a34a 100%);
        transform: scale(1.05);
    }
    .status-panel .resume-btn:active {
        transform: scale(0.98);
    }
    .status-panel .cancel-btn {
        background: linear-gradient(135deg, #EF4444 0%, #DC2626 100%);
        border: none;
        border-radius: 6px;
        padding: 6px 12px;
        color: white;
        font-size: 12px;
        font-weight: 600;
        cursor: pointer;
        transition: all 0.2s ease;
        display: none;
    }
    .status-panel .cancel-btn:hover {
        background: linear-gradient(135deg, #F87171 0%, #EF4444 100%);
        transform: scale(1.05);
    }
    .status-panel .done-btn {
        background: linear-gradient(135deg, #16a34a 0%, #15803d 100%);
        border: none; border-radius: 6px; padding: 6px 12px;
        color: white; font-size: 12px; font-weight: 600;
        cursor: pointer; transition: all 0.2s ease; display: none;
    }
    .status-panel .done-btn:hover {
        background: linear-gradient(135deg, #22c55e 0%, #16a34a 100%);
        transform: scale(1.05);
    }
    .status-panel .keep-trying-btn {
        background: linear-gradient(135deg, #F59E0B 0%, #D97706 100%);
        border: none; border-radius: 6px; padding: 6px 12px;
        color: white; font-size: 12px; font-weight: 600;
        cursor: pointer; transition: all 0.2s ease; display: none;
    }
    .status-panel .keep-trying-btn:hover {
        background: linear-gradient(135deg, #FBBF24 0%, #F59E0B 100%);
        transform: scale(1.05);
    }
    .status-panel.paused {
        border-color: rgba(245, 158, 11, 0.5);
        background: linear-gradient(135deg, rgba(245, 158, 11, 0.25) 0%, rgba(217, 119, 6, 0.2) 100%);
    }
    .status-panel.success {
        border-color: rgba(34, 197, 94, 0.5);
    }
    .status-panel.error {
        border-color: rgba(239, 68, 68, 0.5);
    }
    `;

    // Physics engine classes
    class Spring {
        constructor(config) {
            Object.assign(this, config);
            this.x = this.y = this.vx = this.vy = this.targetX = this.targetY = 0;
        }
        setTarget(x, y) { this.targetX = x; this.targetY = y; }
        snapTo(x, y) { this.x = this.targetX = x; this.y = this.targetY = y; this.vx = this.vy = 0; }
        update(dt) {
            const timeStep = Math.min(dt, 0.05);
            this.vx += ((-this.stiffness * (this.x - this.targetX)) + (-this.damping * this.vx)) / this.mass * timeStep;
            this.vy += ((-this.stiffness * (this.y - this.targetY)) + (-this.damping * this.vy)) / this.mass * timeStep;
            this.x += this.vx * timeStep; this.y += this.vy * timeStep;
            return Math.abs(this.vx) < 0.1 && Math.abs(this.vy) < 0.1 && Math.abs(this.x - this.targetX) < 0.1 && Math.abs(this.y - this.targetY) < 0.1;
        }
    }

    class BezierPath {
        constructor(startX, startY, endX, endY) {
            this.start = {x:startX, y:startY}; this.end = {x:endX, y:endY};
            const dist = Math.hypot(endX-startX, endY-startY);
            const var_ = Math.min(30, dist * 0.2) * (Math.random() - 0.5) * 2;
            const perp = {x: -(endY-startY)/dist, y: (endX-startX)/dist};
            this.c1 = {x: startX + (endX-startX)*0.25 + perp.x*var_, y: startY + (endY-startY)*0.25 + perp.y*var_};
            this.c2 = {x: startX + (endX-startX)*0.75 + perp.x*var_, y: startY + (endY-startY)*0.75 + perp.y*var_};
        }
        getPoint(t) {
            const i=1-t, i2=i*i, i3=i2*i, t2=t*t, t3=t2*t;
            return {x: i3*this.start.x + 3*i2*t*this.c1.x + 3*i*t2*this.c2.x + t3*this.end.x, y: i3*this.start.y + 3*i2*t*this.c1.y + 3*i*t2*this.c2.y + t3*this.end.y};
        }
    }

    class ParticleSystem {
        constructor(c) { this.c = c; this.p = []; }
        spawn(x,y,count=5,color='#6366F1') {
            for(let i=0; i<count; i++) {
                const el = document.createElement('div'); el.className='particle'; el.style.left=`${x}px`; el.style.top=`${y}px`; el.style.backgroundColor=color;
                const a = Math.random()*Math.PI*2, s = 2+Math.random()*3;
                this.p.push({el, vx:Math.cos(a)*s, vy:Math.sin(a)*s, life:1, decay:0.02+Math.random()*0.02});
                this.c.appendChild(el);
            }
        }
        update() {
            for(let i=this.p.length-1; i>=0; i--) {
                const p = this.p[i]; p.life -= p.decay;
                if (p.life<=0) { p.el.remove(); this.p.splice(i,1); continue; }
                const x = parseFloat(p.el.style.left), y = parseFloat(p.el.style.top);
                p.el.style.left=`${x+p.vx}px`; p.el.style.top=`${y+p.vy}px`; p.el.style.opacity=p.life; p.vy+=0.1;
            }
        }
    }

    class VisualOverlay {
        constructor() {
            this.isVisible = this.isAnimating = this.debugMode = false;
            this.spring = new Spring({mass:1, stiffness:170, damping:26});
            this.path = null; this.lastTime = 0;
            this.currentExecutionId = null;
            this.panelHideTimeout = null;
            this._resumeInFlight = false;
            this._resumeSuppressUntil = 0;
            this.actionInProgress = false;
            this.cursorPlaced = false;
            this.reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
            this.moveStoppedTimeout = null;
            this.lastBubbleText = null;
            this.lastBubbleUpdateTime = 0;

            this.handleMessage = this.handleMessage.bind(this);
            this.animate = this.animate.bind(this);
            this.handleMouseMove = this.handleMouseMove.bind(this);
            this.handleClick = this.handleClick.bind(this);
            this.handleStopClick = this.handleStopClick.bind(this);

            this.init();
        }
        init() {
            this.root = document.createElement('div'); this.root.id = 'magicutor-overlay-host';
            this.root.style.cssText = 'position:fixed; top:0; left:0; width:100vw; height:100vh; z-index:2147483647; pointer-events:none;';
            try { this.shadow = this.root.attachShadow({mode:'closed'}); } catch(e) { this.shadow = this.root; }
            const style = document.createElement('style'); style.textContent = OVERLAY_CSS; this.shadow.appendChild(style);
            this.container = document.createElement('div'); this.container.className = 'overlay-container';
            this.spotlight = document.createElement('div'); this.spotlight.className = 'spotlight-layer'; this.container.appendChild(this.spotlight);
            this.cursor = document.createElement('div'); this.cursor.className = 'cursor'; this.cursor.innerHTML = ICONS.cursor; this.container.appendChild(this.cursor);
            this.bubble = document.createElement('div'); this.bubble.className = 'thought-bubble'; this.container.appendChild(this.bubble);

            // Create floating status panel
            this.statusPanel = document.createElement('div');
            this.statusPanel.className = 'status-panel';
            this.statusPanel.innerHTML = `
                <button class="resume-btn">Resume</button>
                <button class="done-btn">Done</button>
                <button class="keep-trying-btn">Keep Trying</button>
                <button class="cancel-btn">Cancel</button>
                <button class="stop-btn" aria-label="Stop automation"><div class="stop-icon"></div></button>
            `;
            this.statusPanel.querySelector('.stop-btn').addEventListener('click', this.handleStopClick);
            this.statusPanel.querySelector('.resume-btn').addEventListener('click', () => this.handleResumeClick());
            this.statusPanel.querySelector('.cancel-btn').addEventListener('click', () => this.handleCancelClick());
            this.statusPanel.querySelector('.done-btn').addEventListener('click', () => this.handleDoneClick());
            this.statusPanel.querySelector('.keep-trying-btn').addEventListener('click', () => this.handleKeepTryingClick());
            this.container.appendChild(this.statusPanel);

            // Create closing orb for completion animation
            this.closingOrb = document.createElement('div');
            this.closingOrb.className = 'closing-orb';
            this.container.appendChild(this.closingOrb);

            this.shadow.appendChild(this.container);
            (document.body || document.documentElement).appendChild(this.root);
            this.particles = new ParticleSystem(this.container);
            try {
                if (chrome && chrome.runtime && chrome.runtime.onMessage) {
                    chrome.runtime.onMessage.addListener(this.handleMessage);

                    // Check for active automation (restore panel after page reload)
                    chrome.runtime.sendMessage({ action: 'check_active_panel' }, (response) => {
                        if (response && response.active && response.executionId) {
                            this.showStatusPanel({
                                executionId: response.executionId,
                                status: response.status || 'running',
                                text: response.text || 'Running...'
                            });
                        }
                    });

                    // Load persistent debug mode from storage (user setting)
                    // This is separate from "Enable Visual Overlay" which controls overlay during automation
                    chrome.storage.local.get(['debugOverlayEnabled'], (result) => {
                        if (result.debugOverlayEnabled) {
                            this.setDebugMode(true);
                        }
                    });

                    // Listen for debug-page execution registration (page → content script → background)
                    // This bridges the gap when the debug page creates executions directly via HTTP
                    window.addEventListener('message', (event) => {
                        if (event.source !== window) return;
                        if (event.data?.type === 'magicutor_register_execution' && event.data.executionId) {
                            chrome.runtime.sendMessage({
                                action: 'register_debug_execution',
                                executionId: event.data.executionId
                            }, (response) => {
                                if (chrome.runtime.lastError) return;
                            });
                        }
                    });
                }
            } catch(e) {}
            console.log('[VisualOverlay] Initialized');
        }
        startAnimation() { if(!this.isAnimating) { this.isAnimating=true; this.lastTime=performance.now(); requestAnimationFrame(t=>this.animate(t)); } }
        ensureCursorAlive() {
            if (!this.cursorPlaced) {
                const x = Math.max(72, Math.round(window.innerWidth * 0.28));
                const y = Math.max(88, Math.round(window.innerHeight * 0.34));
                this.spring.snapTo(x, y);
                this.cursor.style.setProperty('--x', `${x}px`);
                this.cursor.style.setProperty('--y', `${y}px`);
                this.cursorPlaced = true;
            }
            this.startAnimation();
        }
        handleMessage(m, s, r) {
            if (m.action?.startsWith('overlay_')) {
                if (m.action === 'overlay_preview') this.showPreview(m);
                else if (m.action === 'overlay_complete') this.showComplete(m);
                else if (m.action === 'overlay_error') this.showError(m);
                else if (m.action === 'overlay_hide') this.hide();
                else if (m.action === 'overlay_debug') this.setDebugMode(m.enabled);
                else if (m.action === 'overlay_debug_mode') this.setDebugMode(m.enabled); // Alias for persistent debug mode
                else if (m.action === 'overlay_status') this.updateStatusPanel(m);
                else if (m.action === 'overlay_panel_show') this.showStatusPanel(m);
                else if (m.action === 'overlay_panel_hide') this.hideStatusPanel();
                r({status: 'ok'});
            }
        }
        // Status Panel Methods
        showStatusPanel({ executionId, status, text }) {
            this.currentExecutionId = executionId || null;
            this._resumeInFlight = false;
            this._resumeSuppressUntil = 0;
            this.updateStatusPanel({ status: status || 'running', text: text || 'Running...' });
            this.statusPanel.classList.add('visible');
            this.isVisible = true;
            this.container.classList.add('visible');
            this.ensureCursorAlive();
            if (this.panelHideTimeout) {
                clearTimeout(this.panelHideTimeout);
                this.panelHideTimeout = null;
            }
        }
        updateStatusPanel({ status, text, executionId, localResumeTransition = false }) {
            // Auto-show panel if it received a status update but isn't visible yet.
            // This self-heals when overlay_panel_show failed (e.g. debug-page timing race).
            if (!this.statusPanel.classList.contains('visible') && status) {
                this.statusPanel.classList.add('visible');
                this.container.classList.add('visible');
                this.isVisible = true;
                this.ensureCursorAlive();
            }

            // Set executionId if provided (needed when overlay_panel_show was missed)
            if (executionId && !this.currentExecutionId) {
                this.currentExecutionId = executionId;
            }

            const stopBtn = this.statusPanel.querySelector('.stop-btn');
            const resumeBtn = this.statusPanel.querySelector('.resume-btn');
            const cancelBtn = this.statusPanel.querySelector('.cancel-btn');
            const doneBtn = this.statusPanel.querySelector('.done-btn');
            const keepTryingBtn = this.statusPanel.querySelector('.keep-trying-btn');
            const isResumablePauseStatus = status === 'paused'
                || status === 'waitinguser'
                || status === 'waiting_user';
            const isInputBlockedStatus = status === 'waiting_user_input';
            const isWaitingChildrenStatus = status === 'waitingchildren'
                || status === 'waiting_children';

            // Suppress transient "paused" updates immediately after Resume click.
            if (isResumablePauseStatus && this._resumeInFlight && Date.now() < this._resumeSuppressUntil) {
                return;
            }
            if (this._resumeInFlight && !localResumeTransition) {
                if (!isResumablePauseStatus) {
                    this._resumeInFlight = false;
                    this._resumeSuppressUntil = 0;
                } else if (Date.now() >= this._resumeSuppressUntil) {
                    this._resumeInFlight = false;
                    this._resumeSuppressUntil = 0;
                }
            }

            this.statusPanel.classList.remove('success', 'error', 'paused');

            // Escalation pause: agent is stuck (CannotProceed / LoopDetected) and needs user decision.
            if (status === 'escalation') {
                this.statusPanel.classList.add('paused');
                stopBtn.style.display = 'none';
                resumeBtn.style.display = 'none';
                doneBtn.style.display = 'block';
                keepTryingBtn.style.display = 'block';
                cancelBtn.style.display = 'block';
            } else if (isInputBlockedStatus) {
                this.statusPanel.classList.add('paused');
                stopBtn.style.display = 'none';
                resumeBtn.style.display = 'none';
                cancelBtn.style.display = 'block';
                doneBtn.style.display = 'none';
                keepTryingBtn.style.display = 'none';
            } else if (isResumablePauseStatus) {
                this.statusPanel.classList.add('paused');
                stopBtn.style.display = 'none';
                resumeBtn.style.display = 'block';
                cancelBtn.style.display = 'block';
                doneBtn.style.display = 'none';
                keepTryingBtn.style.display = 'none';
            } else if (isWaitingChildrenStatus) {
                this.statusPanel.classList.add('paused');
                stopBtn.style.display = 'none';
                resumeBtn.style.display = 'none';
                cancelBtn.style.display = 'block';
                doneBtn.style.display = 'none';
                keepTryingBtn.style.display = 'none';
            } else if (status === 'running' || status === 'working') {
                stopBtn.style.display = 'flex';
                resumeBtn.style.display = 'none';
                cancelBtn.style.display = 'none';
                doneBtn.style.display = 'none';
                keepTryingBtn.style.display = 'none';
            } else if (status === 'success' || status === 'completed') {
                this.statusPanel.classList.add('success');
                stopBtn.style.display = 'none';
                resumeBtn.style.display = 'none';
                cancelBtn.style.display = 'none';
                doneBtn.style.display = 'none';
                keepTryingBtn.style.display = 'none';
                this.hideStatusPanel();
                this.startClosingAnimation('success');
            } else if (status === 'error' || status === 'failed') {
                this.statusPanel.classList.add('error');
                stopBtn.style.display = 'none';
                resumeBtn.style.display = 'none';
                cancelBtn.style.display = 'none';
                doneBtn.style.display = 'none';
                keepTryingBtn.style.display = 'none';
                this.hideStatusPanel();
                this.startClosingAnimation('error');
            } else {
                stopBtn.style.display = 'none';
                resumeBtn.style.display = 'none';
                cancelBtn.style.display = 'none';
                doneBtn.style.display = 'none';
                keepTryingBtn.style.display = 'none';
            }

            this.statusPanel.dataset.status = status || '';
        }
        hideStatusPanel() {
            this.statusPanel.classList.remove('visible');
            this.currentExecutionId = null;
            if (this.panelHideTimeout) {
                clearTimeout(this.panelHideTimeout);
                this.panelHideTimeout = null;
            }
        }
        scheduleHidePanel(delay) {
            if (this.panelHideTimeout) clearTimeout(this.panelHideTimeout);
            this.panelHideTimeout = setTimeout(() => {
                this.hideStatusPanel();
            }, delay);
        }
        startClosingAnimation(type) {
            // Cancel any pending hide
            if (this.panelHideTimeout) {
                clearTimeout(this.panelHideTimeout);
                this.panelHideTimeout = null;
            }

            // Show status for a moment before starting animation
            setTimeout(() => {
                // Start aurora gathering animation
                this.container.classList.add('closing');

                // Show closing orb with appropriate color
                this.closingOrb.classList.remove('success', 'error', 'active');
                this.closingOrb.classList.add(type, 'active');

                // Timeline:
                // 0ms: Aurora starts gathering (0.8s animation)
                // 600ms: Orb appears (orb-appear 0.6s)
                // 1200ms: Orb pulses (orb-pulse 0.4s x2 = 0.8s)
                // 2000ms: Orb fades (orb-fade 0.5s)
                // 2500ms: Hide panel and overlay

                setTimeout(() => {
                    // Hide status panel (slide down)
                    this.hideStatusPanel();

                    // Clean up animation classes
                    setTimeout(() => {
                        this.container.classList.remove('closing');
                        this.closingOrb.classList.remove('success', 'error', 'active');
                        // Now hide the full overlay
                        this.hide();
                    }, 400); // After panel slide-out animation
                }, 2000); // After orb animation completes
            }, 1000); // Show status for 1 second before animation
        }
        async handleStopClick() {
            if (!this.currentExecutionId) {
                console.warn('[VisualOverlay] No execution ID to stop');
                return;
            }
            const stopBtn = this.statusPanel.querySelector('.stop-btn');
            stopBtn.disabled = true;
            stopBtn.setAttribute('aria-busy', 'true');

            try {
                const response = await chrome.runtime.sendMessage({
                    action: 'stop_automation',
                    executionId: this.currentExecutionId,
                    closeWindows: true
                });
                if (response?.error) {
                    throw new Error(response.error);
                }
                this.updateStatusPanel({ status: 'success', text: 'Stopped' });
            } catch (e) {
                console.error('[VisualOverlay] Stop failed:', e);
                this.updateStatusPanel({ status: 'error', text: 'Stop failed' });
            } finally {
                stopBtn.disabled = false;
                stopBtn.removeAttribute('aria-busy');
            }
        }
        async handleResumeClick() {
            if (!this.currentExecutionId) {
                console.warn('[VisualOverlay] No execution ID to resume');
                return;
            }
            const resumeBtn = this.statusPanel.querySelector('.resume-btn');
            this._resumeInFlight = true;
            this._resumeSuppressUntil = Date.now() + RESUME_SUPPRESSION_MS;
            resumeBtn.disabled = true;
            resumeBtn.textContent = 'Resuming...';

            try {
                await chrome.runtime.sendMessage({
                    action: 'continue_automation',
                    executionId: this.currentExecutionId
                });
                this.updateStatusPanel({ status: 'running', text: 'Resumed...', localResumeTransition: true });
            } catch (e) {
                console.error('[VisualOverlay] Resume failed:', e);
                this._resumeInFlight = false;
                this._resumeSuppressUntil = 0;
                this.updateStatusPanel({ status: 'error', text: 'Resume failed' });
            } finally {
                resumeBtn.disabled = false;
                resumeBtn.textContent = 'Resume';
            }
        }
        async handleDoneClick() {
            if (!this.currentExecutionId) return;
            const doneBtn = this.statusPanel.querySelector('.done-btn');
            this._resumeInFlight = true;
            this._resumeSuppressUntil = Date.now() + RESUME_SUPPRESSION_MS;
            doneBtn.disabled = true;
            doneBtn.textContent = 'Completing...';
            try {
                await chrome.runtime.sendMessage({
                    action: 'escalation_done',
                    executionId: this.currentExecutionId
                });
                this.updateStatusPanel({ status: 'running', text: 'Resumed...', localResumeTransition: true });
            } catch (e) {
                console.error('[VisualOverlay] Done failed:', e);
                this._resumeInFlight = false;
                this._resumeSuppressUntil = 0;
                this.updateStatusPanel({ status: 'error', text: 'Done failed' });
            } finally {
                doneBtn.disabled = false;
                doneBtn.textContent = 'Done';
            }
        }
        async handleKeepTryingClick() {
            if (!this.currentExecutionId) return;
            const keepTryingBtn = this.statusPanel.querySelector('.keep-trying-btn');
            this._resumeInFlight = true;
            this._resumeSuppressUntil = Date.now() + RESUME_SUPPRESSION_MS;
            keepTryingBtn.disabled = true;
            keepTryingBtn.textContent = 'Retrying...';
            try {
                await chrome.runtime.sendMessage({
                    action: 'escalation_keep_trying',
                    executionId: this.currentExecutionId
                });
                this.updateStatusPanel({ status: 'running', text: 'Retrying...', localResumeTransition: true });
            } catch (e) {
                console.error('[VisualOverlay] Keep Trying failed:', e);
                this._resumeInFlight = false;
                this._resumeSuppressUntil = 0;
                this.updateStatusPanel({ status: 'error', text: 'Retry failed' });
            } finally {
                keepTryingBtn.disabled = false;
                keepTryingBtn.textContent = 'Keep Trying';
            }
        }
        async handleCancelClick() {
            if (!this.currentExecutionId) {
                console.warn('[VisualOverlay] No execution ID to cancel');
                return;
            }
            const cancelBtn = this.statusPanel.querySelector('.cancel-btn');
            this._resumeInFlight = false;
            this._resumeSuppressUntil = 0;
            cancelBtn.disabled = true;
            cancelBtn.textContent = 'Cancelling...';

            try {
                const response = await chrome.runtime.sendMessage({
                    action: 'cancel_automation',
                    executionId: this.currentExecutionId
                });
                if (response?.error) {
                    throw new Error(response.error);
                }
                this.updateStatusPanel({ status: 'error', text: 'Cancelled' });
            } catch (e) {
                console.error('[VisualOverlay] Cancel failed:', e);
                this.updateStatusPanel({ status: 'error', text: 'Cancel failed' });
            } finally {
                cancelBtn.disabled = false;
                cancelBtn.textContent = 'Cancel';
            }
        }
        setDebugMode(enabled) {
            this.debugMode = enabled;
            if (enabled) {
                console.log('[VisualOverlay] Debug ON');
                this.isVisible = true; this.container.classList.add('visible');
                this.ensureCursorAlive();
                this.updateBubble('default', {text: 'Debug'});
                document.addEventListener('mousemove', this.handleMouseMove);
                document.addEventListener('click', this.handleClick);
                this.startAnimation();
            } else {
                console.log('[VisualOverlay] Debug OFF');
                this.hide();
                document.removeEventListener('mousemove', this.handleMouseMove);
                document.removeEventListener('click', this.handleClick);
            }
        }
        handleMouseMove(e) {
            if(this.debugMode) {
                this.spring.setTarget(e.clientX, e.clientY);
                // Only show coordinates if no action in progress
                if (!this.actionInProgress) {
                    this.updateBubble('debug', {text: `X:${e.clientX} Y:${e.clientY}`});
                    // Reset timeout - when mouse stops, show default text
                    if (this.moveStoppedTimeout) clearTimeout(this.moveStoppedTimeout);
                    this.moveStoppedTimeout = setTimeout(() => {
                        if (!this.actionInProgress) {
                            this.updateBubble('default', {text: this.debugMode ? 'Debug' : 'Idling'});
                        }
                    }, 500);
                }
            }
        }
        handleClick(e) { if(this.debugMode) { this.createRipple(e.clientX, e.clientY, true); this.particles.spawn(e.clientX, e.clientY, 8, '#22c55e'); } }
        showPreview(d) {
            const {type, target, params, duration=300} = d;
            if (d.executionId && !this.currentExecutionId) {
                this.currentExecutionId = d.executionId;
            }
            const tx = target.rect.x + target.rect.width/2, ty = target.rect.y + target.rect.height/2;
            if(!this.isVisible) { this.isVisible=true; this.container.classList.add('visible'); this.spring.snapTo(tx-100, ty-100); }
            this.cursorPlaced = true;
            this.actionInProgress = true;
            if (this.moveStoppedTimeout) { clearTimeout(this.moveStoppedTimeout); this.moveStoppedTimeout = null; }
            this.startAnimation();
            this.path = new BezierPath(this.spring.x, this.spring.y, tx, ty);
            this.pathStart = performance.now(); this.pathDur = duration;
            this.updateBubble(type, params);
            if(type==='type' && params.text) this.startGhostTyping(params.text, tx, ty);
        }
        _setBubbleContent(iconHtml, text, animate = false) {
            this.bubble.textContent = '';
            this.bubble.classList.remove('visible', 'success', 'error');
        }
        updateBubble(t, p = {}) {
            this.bubble.textContent = '';
            this.bubble.classList.remove('visible', 'success', 'error');
            this.lastBubbleText = null;
            this.lastBubbleUpdateTime = performance.now();
        }
        startGhostTyping(text, x, y) {
            // Text overlays are intentionally disabled so screenshots only show
            // the cursor sprite, not automation-generated words.
        }
        showComplete(d) {
            this.actionInProgress = false;
            if(d.success) {
                this.createRipple(this.spring.x, this.spring.y, true);
                this.particles.spawn(this.spring.x, this.spring.y, 8, '#22c55e');
                // After brief success display, return to default state
                setTimeout(() => {
                    if (!this.actionInProgress) {
                        this.updateBubble('default', {text: this.debugMode ? 'Debug' : 'Idling'});
                    }
                }, 800);
            }
        }
        showError(d) {
            this.actionInProgress = false;
            this.cursor.classList.add('error');
            setTimeout(() => this.cursor.classList.remove('error'), 500);
            // After brief error display, return to default state
            setTimeout(() => {
                if (!this.actionInProgress) {
                    this.updateBubble('default', {text: this.debugMode ? 'Debug' : 'Idling'});
                }
            }, 1500);
        }
        createRipple(x, y, s) {
            const r = document.createElement('div'); r.className=`ripple ${s?'success':''}`; r.style.left=`${x}px`; r.style.top=`${y}px`;
            this.container.appendChild(r); setTimeout(()=>r.remove(), 600);
        }
        hide() {
            // Don't hide if debug mode is enabled (persistent debug from settings)
            if (this.debugMode) {
                this.bubble.classList.remove('visible');
                return;
            }
            this.isVisible=false; this.container.classList.remove('visible'); this.spotlight.classList.remove('active'); this.bubble.classList.remove('visible');
        }
        animate(t) {
            if(!this.isAnimating) return;
            const dt = (t - this.lastTime)/1000; this.lastTime = t;
            if(this.path) {
                const prog = Math.min((t - this.pathStart)/this.pathDur, 1);
                const p = this.path.getPoint(1 - Math.pow(1-prog, 4));
                this.spring.setTarget(p.x, p.y); if(prog>=1) this.path=null;
            }
            const settled = this.spring.update(dt); this.particles.update();
            if(!this.isVisible && settled && this.particles.p.length===0) { this.isAnimating=false; return; }
            let drawX = this.spring.x, drawY = this.spring.y;
            if (!this.path && !this.reduceMotion) {
                drawX += Math.sin(t / 900) * 4;
                drawY += Math.cos(t / 1100) * 6;
            }
            this.cursor.style.setProperty('--x', `${drawX}px`); this.cursor.style.setProperty('--y', `${drawY}px`);
            this.spotlight.style.setProperty('--x', `${drawX}px`); this.spotlight.style.setProperty('--y', `${drawY}px`);
            const flipH = this.spring.x > window.innerWidth - 250, flipV = this.spring.y > window.innerHeight - 100;
            this.bubble.style.setProperty('--bubble-x', `${flipH?this.spring.x-180:this.spring.x+24}px`);
            this.bubble.style.setProperty('--bubble-y', `${flipV?this.spring.y-60:this.spring.y+24}px`);
            requestAnimationFrame(t=>this.animate(t));
        }
    }
    new VisualOverlay();
})();
