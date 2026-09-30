<!--
  /draw-overlay — inert Personal Tutor drawing surface.

  This route is loaded by a separate transparent Tauri window that ignores
  cursor events. It is intentionally not the chat HUD: users can dismiss the
  HUD after submitting a tutor prompt while this surface remains available
  for highlights/arrows emitted later by the agent.
-->
<script lang="ts">
	import { PRODUCT_NAME } from '$lib/presentationIdentity';
	import { browser } from '$app/environment';
	import { onDestroy, onMount } from 'svelte';
	import {
		appendCurrentScopeQuery,
		installScopedApiFetch,
		scopedRequestHeaders
	} from '$lib/stores/scopeIdentityStore';
	import { chatStore } from '$lib/stores/chatStore';
	import {
		cancelTutorNarration,
		speakTutorNarration
	} from '$lib/media/tts/tutorNarration';
	import { tutorPrimitiveRegistry } from '$lib/tutor/primitiveRegistry';
	import {
		renderRecipe,
		estimateTextMetrics,
		type DrawCommand,
		type MeasureTextFn,
		type RecipeShape
	} from '$lib/tutor/recipeInterpreter';
	import {
		isSemanticBoxType,
		semanticBoxLabelLayout
	} from '$lib/tutor/semanticBoxLabelLayout';
	import {
		buildDeeperRequest,
		canRequestDeeper,
		postDeeperRequest
	} from '$lib/tutor/deeperRequest';

	type TutorCanvasMode = 'screen_overlay' | 'blackboard';
	type TutorOverlayRail = 'tutor' | 'copilot';

	interface TutorShape {
		id?: string;
		type: string;
		x?: number;
		y?: number;
		w?: number;
		h?: number;
		x1?: number;
		y1?: number;
		x2?: number;
		y2?: number;
		cx?: number;
		cy?: number;
		r?: number;
		/** Elliptical radii; absent means circular. */
		rx?: number;
		ry?: number;
		radius?: number;
		size?: number;
		start_angle?: number;
		end_angle?: number;
		from_x?: number;
		from_y?: number;
		to_x?: number;
		to_y?: number;
		points?: Array<[number, number] | { x?: number; y?: number }> | string;
		d?: string;
		path?: string;
		control_x?: number;
		control_y?: number;
		control1_x?: number;
		control1_y?: number;
		control2_x?: number;
		control2_y?: number;
		c1x?: number;
		c1y?: number;
		c2x?: number;
		c2y?: number;
		color?: string;
		fill?: string;
		opacity?: number;
		stroke_width?: number;
		font_size?: number;
		label?: string;
		text?: string;
		formula?: string;
		side?: 'left' | 'right' | string;
		orientation?: 'left' | 'right' | string;
		group_id?: string;
		z_index?: number;
		duration_ms?: number;
		delay_ms?: number;
		animate?: boolean;
		style?: Record<string, unknown>;
		ttl_ms?: number;
		persist?: boolean;
		created_at_ms?: number;
		reveal_id?: string;
		storyboard_step_id?: string;
		reveal_order?: number;
		tutor_step_label?: string;
		step_label?: string;
		narration?: string;
		wait_for_voice?: boolean;
		clear_previous?: boolean;
		persist_until_step?: string;
		canvas_mode?: TutorCanvasMode | string;
		canvasMode?: TutorCanvasMode | string;
		__renderLabelOffsetX?: number;
	}

	interface TutorTextLabelCandidate {
		index: number;
		x: number;
		y: number;
		width: number;
	}

	interface TutorStepBubble {
		revealId?: string;
		order?: number;
		label: string;
		narration?: string;
		waitForVoice?: boolean;
	}

	interface TutorOverlayStatusPayload {
		status?: string;
		session_id?: string | null;
		sessionId?: string | null;
		canvas_mode?: TutorCanvasMode | string | null;
		canvasMode?: TutorCanvasMode | string | null;
		rail?: TutorOverlayRail | string | null;
	}

	interface TutorOverlayDismissPayload {
		session_id?: string | null;
		sessionId?: string | null;
	}

	interface TutorControlRect {
		x: number;
		y: number;
		width: number;
		height: number;
	}

	interface CopilotUserActionPayload {
		session_id?: string | null;
		sessionId?: string | null;
		model_x?: number;
		model_y?: number;
		screen_x?: number;
		screen_y?: number;
	}

	let unlistenDraw: (() => void) | null = null;
	let unlistenReplay: (() => void) | null = null;
	let unlistenDeeper: (() => void) | null = null;
	let unlistenKeepShowing: (() => void) | null = null;
	let unlistenDismiss: (() => void) | null = null;
	let unlistenCopilotUserAction: (() => void) | null = null;
	let unlistenStatus: (() => void) | null = null;
	let pendingDrainTimer: ReturnType<typeof setInterval> | null = null;
	let shapeExpiryTimer: ReturnType<typeof setTimeout> | null = null;
	let replayReadyTimer: ReturnType<typeof setTimeout> | null = null;
	let narrationFlushTimer: ReturnType<typeof setTimeout> | null = null;
	let removeVisibilityListener: (() => void) | null = null;
	let shapes = $state<TutorShape[]>([]);
	let visibleShapes = $derived(applyTextLabelSpacing(sortShapesForDisplay(shapes)));
	let currentStep = $state<TutorStepBubble | null>(null);
	let tutorStatus = $state<'idle' | 'working'>('idle');
	let canvasMode = $state<TutorCanvasMode>('screen_overlay');
	let overlayRail = $state<TutorOverlayRail>('tutor');
	let activeTutorSessionId = $state<string | null>(null);
	let hasRenderedTutorContent = $state(false);
	let replayGeneration = $state(0);
	let replayReady = $state(false);
	let pendingNarrationCount = $state(0);
	let speakingStepKey = $state<string | null>(null);
	let coordinateWidth = $state(2048);
	let coordinateHeight = $state(1152);
	let overlayExpiresAtMs = $state<number | null>(null);
	let lastPublishedControlRegions = '';
	const drawnShapeIds = new Set<string>();
	const narratedStepKeys = new Set<string>();
	const queuedNarrationKeys = new Set<string>();
	const pendingStoryShapes = new Map<string, TutorShape[]>();
	const pendingStorySteps = new Map<string, TutorStepBubble>();
	let localShapeSequence = 0;
	let narrationEpoch = 0;
	let narrationTail: Promise<void> = Promise.resolve();
	const DEFAULT_DRAW_COLOR = '#00f5ff';
	const DEFAULT_FILL_COLOR = 'rgba(0, 245, 255, 0.22)';
	// Phase 3 flag: render supported tutor primitives via the data-driven recipe
	// interpreter (the same JSON recipes that drive iOS) instead of the native
	// per-type SVG path. Now the PRIMARY path (parity verified): a shape renders
	// via a loaded recipe when one exists, else falls back to the native SVG
	// renderer (kept as the offline / no-recipe safety net). Default ON; set the
	// env var VITE_USE_RECIPE_INTERPRETER=false to force the native path.
	const USE_RECIPE_INTERPRETER = import.meta.env.VITE_USE_RECIPE_INTERPRETER !== 'false';
	const MODEL_COORDINATE_MAX_SIDE = 2048;
	const MIN_SHAPE_TTL_MS = 20000;
	const DEFAULT_SHAPE_TTL_MS = 60000;
	const DEFAULT_DRAW_DURATION_MS = 1600;
	const MIN_DRAW_DURATION_MS = 1300;
	const MAX_DRAW_DURATION_MS = 3600;
	const MIN_STROKE_WIDTH = 4;
	const REPLAY_BUTTON_WIDTH = 118;
	const REPLAY_BUTTON_HEIGHT = 34;
	const REPLAY_BUTTON_RIGHT = 26;
	const REPLAY_BUTTON_BOTTOM = 26;
	const DISMISS_BUTTON_WIDTH = 128;
	const KEEP_SHOWING_BUTTON_WIDTH = 154;
	// Dynamic on desktop: currentControlRegions reports the rendered rectangle
	// to the native host. Rust deliberately has no mirrored fallback constant.
	const DEEPER_BUTTON_WIDTH = 150;
	const CONTROL_BUTTON_GAP = 10;
	// Must match `.draw-label`'s font-family, or every measurement is of a font
	// the overlay never renders.
	const LABEL_FONT_FAMILY =
		"Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";

	let labelMeasureCtx: CanvasRenderingContext2D | null = null;

	/**
	 * Measures label text with the real font so recipes can size a box around
	 * glyphs rather than a character count. Falls back to the estimate when
	 * there is no canvas — SSR, or a browser that refuses the 2D context — so a
	 * missing measurement degrades to the old geometry instead of a zero-width
	 * box.
	 */
	const measureLabelText: MeasureTextFn = (text, fontSize) => {
		if (!browser) return estimateTextMetrics(text, fontSize);
		if (!labelMeasureCtx) {
			labelMeasureCtx = document.createElement('canvas').getContext('2d');
		}
		if (!labelMeasureCtx) return estimateTextMetrics(text, fontSize);
		labelMeasureCtx.font = `${fontSize}px ${LABEL_FONT_FAMILY}`;
		const m = labelMeasureCtx.measureText(text);
		// `actualBoundingBox*` is per-string ink; fall back to the font-wide
		// ascent when a browser omits it, and to the estimate's ratio last.
		const ascent =
			m.actualBoundingBoxAscent ?? m.fontBoundingBoxAscent ?? fontSize * 0.78;
		const descent =
			m.actualBoundingBoxDescent ?? m.fontBoundingBoxDescent ?? fontSize * 0.22;
		return { width: m.width, height: ascent + descent, ascent };
	};

	const MIN_TEXT_LABEL_HORIZONTAL_GAP = 18;
	const TEXT_LABEL_ROW_THRESHOLD = 26;
	const TEXT_LABEL_EDGE_PADDING = 12;
	let overlayHasContent = $derived(visibleShapes.length > 0 || pendingNarrationCount > 0);
	let tutorPreparingContent = $derived(
		tutorStatus === 'working' && !hasRenderedTutorContent && !overlayHasContent
	);
	let showDimLayer = $derived(tutorPreparingContent || overlayHasContent);
	let blackboardActive = $derived(showDimLayer && canvasMode === 'blackboard');
	let spotlightCutoutShapes = $derived(visibleShapes.filter(isSpotlightCutoutShape));
	let copilotSpotlightActive = $derived(
		showDimLayer &&
			canvasMode === 'screen_overlay' &&
			overlayRail === 'copilot' &&
			spotlightCutoutShapes.length > 0
	);
	let showDismissControl = $derived(tutorPreparingContent || overlayHasContent);
	let playbackControlsReady = $derived(
		replayReady &&
			visibleShapes.length > 0 &&
			pendingNarrationCount === 0 &&
			!speakingStepKey &&
			(tutorStatus !== 'working' || overlayRail === 'tutor')
	);
	let showReplayControl = $derived(playbackControlsReady);
	let showKeepShowingControl = $derived(
		playbackControlsReady && overlayExpiresAtMs !== null
	);
	let showWorkingStatusBubble = $derived(tutorPreparingContent);
	// Offered once a step has been taught — during replay above all, which is
	// when the user has seen the whole lesson and knows which part did not land.
	// Withheld while a draw is still animating: deepening a step mid-teach is a
	// race against the explanation that might still answer it.
	let showDeeperControl = $derived(
		playbackControlsReady && canRequestDeeper(currentStep, activeTutorSessionId)
	);
	let dismissButtonX = $derived(
		Math.max(
			12,
			coordinateWidth -
				DISMISS_BUTTON_WIDTH -
				REPLAY_BUTTON_RIGHT -
				(showReplayControl ? REPLAY_BUTTON_WIDTH + CONTROL_BUTTON_GAP : 0)
		)
	);
	let deeperButtonX = $derived(
		Math.max(
			12,
			coordinateWidth -
				DEEPER_BUTTON_WIDTH -
				REPLAY_BUTTON_RIGHT -
				(showKeepShowingControl ? KEEP_SHOWING_BUTTON_WIDTH + CONTROL_BUTTON_GAP : 0) -
				(showDismissControl ? DISMISS_BUTTON_WIDTH + CONTROL_BUTTON_GAP : 0) -
				(showReplayControl ? REPLAY_BUTTON_WIDTH + CONTROL_BUTTON_GAP : 0)
		)
	);
	let keepShowingButtonX = $derived(
		Math.max(
			12,
			coordinateWidth -
				KEEP_SHOWING_BUTTON_WIDTH -
				REPLAY_BUTTON_RIGHT -
				(showDismissControl ? DISMISS_BUTTON_WIDTH + CONTROL_BUTTON_GAP : 0) -
				(showReplayControl ? REPLAY_BUTTON_WIDTH + CONTROL_BUTTON_GAP : 0)
		)
	);
	let replayButtonX = $derived(Math.max(12, coordinateWidth - REPLAY_BUTTON_WIDTH - REPLAY_BUTTON_RIGHT));
	let replayButtonY = $derived(Math.max(12, coordinateHeight - REPLAY_BUTTON_HEIGHT - REPLAY_BUTTON_BOTTOM));
	let currentStepSpeaking = $derived(
		currentStep ? speakingStepKey === stepNarrationKey(currentStep) : false
	);

	$effect(() => {
		const regions = currentControlRegions();
		const serialized = JSON.stringify(regions);
		if (serialized === lastPublishedControlRegions) return;
		lastPublishedControlRegions = serialized;
		void publishControlRegions(regions);
	});

	function refreshCoordinateSpace() {
		if (typeof window === 'undefined') return;
		const pixelWidth = Math.max(1, window.innerWidth * window.devicePixelRatio);
		const pixelHeight = Math.max(1, window.innerHeight * window.devicePixelRatio);
		const largestSide = Math.max(pixelWidth, pixelHeight);
		const scale =
			largestSide > MODEL_COORDINATE_MAX_SIDE ? MODEL_COORDINATE_MAX_SIDE / largestSide : 1;
		coordinateWidth = pixelWidth * scale;
		coordinateHeight = pixelHeight * scale;
	}

	function shapeColor(shape: TutorShape) {
		return neonizeColor(stringOr(shape.style?.stroke) || stringOr(shape.color));
	}

	function shapeFill(shape: TutorShape, fallback = 'transparent') {
		const explicit = stringOr(shape.style?.fill) || stringOr(shape.fill);
		if (explicit) return explicit;
		return fallback;
	}

	function shapeOpacity(shape: TutorShape, fallback = 1) {
		const value = numberOr(shape.style?.opacity, Number.NaN);
		if (Number.isFinite(value)) return value;
		return numberOr(shape.opacity, fallback);
	}

	function strokeWidth(shape: TutorShape, fallback = 4) {
		const value = numberOr(shape.style?.stroke_width, Number.NaN);
		if (Number.isFinite(value)) return Math.max(MIN_STROKE_WIDTH, value);
		return Math.max(MIN_STROKE_WIDTH, numberOr(shape.stroke_width, fallback));
	}

	function drawDurationMs(shape: TutorShape, fallback = DEFAULT_DRAW_DURATION_MS) {
		const raw = Number.isFinite(shape.duration_ms) ? Number(shape.duration_ms) : fallback;
		return Math.min(MAX_DRAW_DURATION_MS, Math.max(MIN_DRAW_DURATION_MS, raw));
	}

	function shapeAnimationStyle(shape: TutorShape, fallback = DEFAULT_DRAW_DURATION_MS) {
		const duration = drawDurationMs(shape, fallback);
		const labelDelay = Math.round(Math.max(760, duration * 0.68));
		const arrowDelay = Math.round(Math.max(820, duration * 0.72));
		const color = shapeColor(shape);
		return `--draw-duration: ${duration}ms; --label-delay: ${labelDelay}ms; --arrow-delay: ${arrowDelay}ms; --draw-glow: ${hexAlpha(color, 0.82)}; --draw-glow-soft: ${hexAlpha(color, 0.46)};`;
	}

	function clearReplayReadyTimer() {
		if (replayReadyTimer) {
			clearTimeout(replayReadyTimer);
			replayReadyTimer = null;
		}
	}

	function clearNarrationFlushTimer() {
		if (narrationFlushTimer) {
			clearTimeout(narrationFlushTimer);
			narrationFlushTimer = null;
		}
	}

	function updatePendingNarrationCount() {
		pendingNarrationCount = Array.from(pendingStoryShapes.values()).reduce(
			(total, list) => total + list.length,
			0
		);
	}

	function clearPendingStoryboards() {
		clearNarrationFlushTimer();
		pendingStoryShapes.clear();
		pendingStorySteps.clear();
		queuedNarrationKeys.clear();
		updatePendingNarrationCount();
	}

	function scheduleReplayReady(list = shapes) {
		clearReplayReadyTimer();
		replayReady = false;
		if (list.length === 0) return;
		const maxDuration = Math.max(
			...list.map((shape) => drawDurationMs(shape, DEFAULT_DRAW_DURATION_MS)),
			DEFAULT_DRAW_DURATION_MS
		);
		replayReadyTimer = setTimeout(() => {
			replayReady = shapes.length > 0;
		}, Math.min(MAX_DRAW_DURATION_MS + 500, maxDuration + 540));
	}

	function replayCurrentShapes() {
		if (visibleShapes.length === 0) return;
		const now = Date.now();
		const replayShapes = visibleShapes.map((shape) => ({
			...shape,
			id: `${shape.id || 'shape'}-replay-${replayGeneration + 1}-${++localShapeSequence}`,
			created_at_ms: now
		}));
		resetNarrationQueue();
		replayGeneration += 1;
		clearPendingStoryboards();
		shapes = [];
		overlayExpiresAtMs = null;
		currentStep = null;
		replayReady = false;
		const immediateShapes: TutorShape[] = [];
		for (const shape of replayShapes) {
			const step = stepBubbleFromShape(shape);
			if (step && stepNarrationText(step)) {
				addPendingNarratedShape(shape, step);
			} else {
				immediateShapes.push(shape);
			}
		}
		if (immediateShapes.length > 0) {
			shapes = immediateShapes;
			extendOverlayExpiry(immediateShapes);
		}
		scheduleShapeExpiry();
		if (immediateShapes.length > 0) scheduleReplayReady(immediateShapes);
		if (!currentStep) refreshCurrentStepFromShapes(immediateShapes, false);
	}

	function keepCurrentOverlayShowing() {
		if (overlayExpiresAtMs === null) return;
		overlayExpiresAtMs = null;
		scheduleShapeExpiry();
	}

	function currentControlRegions() {
		const actionRects = currentCopilotActionRegions();
		const dismissRect: TutorControlRect | null = showDismissControl
			? {
					x: dismissButtonX,
					y: replayButtonY,
					width: DISMISS_BUTTON_WIDTH,
					height: REPLAY_BUTTON_HEIGHT
				}
			: null;
		const replayRect: TutorControlRect | null = showReplayControl
			? {
					x: replayButtonX,
					y: replayButtonY,
					width: REPLAY_BUTTON_WIDTH,
					height: REPLAY_BUTTON_HEIGHT
				}
			: null;
		const keepShowingRect: TutorControlRect | null = showKeepShowingControl
			? {
					x: keepShowingButtonX,
					y: replayButtonY,
					width: KEEP_SHOWING_BUTTON_WIDTH,
					height: REPLAY_BUTTON_HEIGHT
				}
			: null;
		const deeperRect: TutorControlRect | null = showDeeperControl
			? {
					x: deeperButtonX,
					y: replayButtonY,
					width: DEEPER_BUTTON_WIDTH,
					height: REPLAY_BUTTON_HEIGHT
				}
			: null;
		return {
			dismissRect,
			keepShowingRect,
			replayRect,
			deeperRect,
			actionRects
		};
	}

	function currentCopilotActionRegions(): TutorControlRect[] {
		if (overlayRail !== 'copilot' || tutorStatus !== 'working' || canvasMode !== 'screen_overlay') {
			return [];
		}
		const activeStepKey = currentStep ? stepNarrationKey(currentStep) : null;
		const regions: TutorControlRect[] = [];
		for (const shape of visibleShapes) {
			const step = stepBubbleFromShape(shape);
			if (activeStepKey && step && stepNarrationKey(step) !== activeStepKey) continue;
			const rect = shapeActionRegion(shape);
			if (rect) regions.push(rect);
			if (regions.length >= 8) break;
		}
		if (regions.length > 0 || !activeStepKey) return regions;
		for (const shape of visibleShapes) {
			const rect = shapeActionRegion(shape);
			if (rect) regions.push(rect);
			if (regions.length >= 8) break;
		}
		return regions;
	}

	function shapeActionRegion(shape: TutorShape): TutorControlRect | null {
		if (isRectLike(shape) || isSpotlightCutoutShape(shape)) {
			const padding = 12;
			const x = Math.max(0, numberOr(shape.x) - padding);
			const y = Math.max(0, numberOr(shape.y) - padding);
			const width = Math.max(24, numberOr(shape.w) + padding * 2);
			const height = Math.max(24, numberOr(shape.h) + padding * 2);
			return {
				x,
				y,
				width: Math.min(coordinateWidth - x, width),
				height: Math.min(coordinateHeight - y, height)
			};
		}
		if (isArrowLike(shape) || isLineLike(shape)) {
			const x1 = segmentStartX(shape);
			const y1 = segmentStartY(shape);
			const x2 = segmentEndX(shape);
			const y2 = segmentEndY(shape);
			const padding = 26;
			const x = Math.max(0, Math.min(x1, x2) - padding);
			const y = Math.max(0, Math.min(y1, y2) - padding);
			const width = Math.max(36, Math.abs(x2 - x1) + padding * 2);
			const height = Math.max(36, Math.abs(y2 - y1) + padding * 2);
			return {
				x,
				y,
				width: Math.min(coordinateWidth - x, width),
				height: Math.min(coordinateHeight - y, height)
			};
		}
		if (Number.isFinite(shape.cx) || Number.isFinite(shape.x)) {
			const cx = numberAny(shape, ['cx', 'x']);
			const cy = numberAny(shape, ['cy', 'y']);
			const radius = Math.max(28, numberAny(shape, ['r', 'radius', 'size'], 28));
			const x = Math.max(0, cx - radius);
			const y = Math.max(0, cy - radius);
			return {
				x,
				y,
				width: Math.min(coordinateWidth - x, radius * 2),
				height: Math.min(coordinateHeight - y, radius * 2)
			};
		}
		return null;
	}

	async function publishControlRegions(regions: {
		dismissRect: TutorControlRect | null;
		keepShowingRect: TutorControlRect | null;
		replayRect: TutorControlRect | null;
		deeperRect: TutorControlRect | null;
		actionRects: TutorControlRect[];
	}) {
		if (typeof window === 'undefined' || !('__TAURI_INTERNALS__' in window)) return;
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			await invoke('set_draw_overlay_control_regions', regions);
		} catch (error) {
			console.warn('[draw-overlay] set_draw_overlay_control_regions failed:', error);
		}
	}

	/**
	 * Ask the tutor to decompose the step currently on screen.
	 *
	 * Reachable two ways on purpose. On desktop the overlay is click-through, so
	 * the Tauri host hit-tests the button's geometry and emits
	 * `overlay-explain-deeper-request`. In a browser `/draw-overlay` is an
	 * ordinary page with no host, so the same handler is bound to a real click.
	 */
	async function requestExplainDeeper() {
		const request = buildDeeperRequest(currentStep, activeTutorSessionId);
		// null means the guard and the builder disagreed. The control is hidden
		// under the same conditions, so doing nothing is the honest response
		// rather than throwing inside a click handler.
		if (!request) return;
		const params = appendCurrentScopeQuery();
		const endpoint = `/api/magician/v2/chat/sessions/${encodeURIComponent(request.sessionId)}/messages?${params.toString()}`;
		try {
			await postDeeperRequest(
				endpoint,
				request,
				scopedRequestHeaders({ 'Content-Type': 'application/json' })
			);
		} catch (error) {
			console.warn('[draw-overlay] explain-deeper request failed:', error);
		}
	}

	async function postCopilotUserAction(payload: CopilotUserActionPayload | null | undefined) {
		const sessionId = payloadSessionId(payload) || activeTutorSessionId;
		if (!sessionId || overlayRail !== 'copilot') return;
		const step = currentStep;
		const params = appendCurrentScopeQuery();
		const endpoint = `/api/magician/v2/chat/sessions/${encodeURIComponent(sessionId)}/tutor/user-action?${params.toString()}`;
		try {
			await fetch(endpoint, {
				method: 'POST',
				headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
				body: JSON.stringify({
					storyboard_step_id: step?.revealId,
					storyboard_step_label: step?.label,
					target: step?.label,
					evidence: 'User clicked inside the active App Copilot highlighted region.'
				})
			});
		} catch (error) {
			console.warn('[draw-overlay] post copilot user action failed:', error);
		}
	}

	function payloadSessionId(
		payload?: TutorOverlayStatusPayload | TutorOverlayDismissPayload | CopilotUserActionPayload | null
	) {
		const raw = payload?.session_id ?? payload?.sessionId ?? null;
		if (typeof raw !== 'string') return null;
		const trimmed = raw.trim();
		return trimmed.length > 0 ? trimmed : null;
	}

	function normalizeCanvasMode(value: unknown): TutorCanvasMode | null {
		if (typeof value !== 'string') return null;
		const trimmed = value.trim();
		if (trimmed === 'blackboard') return 'blackboard';
		if (trimmed === 'screen_overlay') return 'screen_overlay';
		return null;
	}

	function normalizeOverlayRail(value: unknown): TutorOverlayRail | null {
		if (typeof value !== 'string') return null;
		const trimmed = value.trim();
		if (trimmed === 'tutor') return 'tutor';
		if (trimmed === 'copilot') return 'copilot';
		return null;
	}

	function payloadCanvasMode(payload?: TutorOverlayStatusPayload | null) {
		return normalizeCanvasMode(payload?.canvas_mode ?? payload?.canvasMode ?? null);
	}

	function payloadOverlayRail(payload?: TutorOverlayStatusPayload | null) {
		return normalizeOverlayRail(payload?.rail ?? null);
	}

	function shapeCanvasMode(shape?: TutorShape | null) {
		return normalizeCanvasMode(shape?.canvas_mode ?? shape?.canvasMode ?? null);
	}

	function setTutorStatus(
		nextStatus: 'idle' | 'working',
		sessionId: string | null = null,
		options: {
			clearSessionOnIdle?: boolean;
			canvasMode?: TutorCanvasMode | null;
			rail?: TutorOverlayRail | null;
		} = {}
	) {
		const clearSessionOnIdle = options.clearSessionOnIdle ?? true;
		const previousSessionId = activeTutorSessionId;
		if (
			nextStatus === 'working' &&
			(!sessionId || sessionId !== previousSessionId) &&
			shapes.length === 0 &&
			pendingNarrationCount === 0
		) {
			hasRenderedTutorContent = false;
		}
		if (sessionId) activeTutorSessionId = sessionId;
		if (options.canvasMode) canvasMode = options.canvasMode;
		if (options.rail) overlayRail = options.rail;
		tutorStatus = nextStatus;
		if (nextStatus !== 'working' && clearSessionOnIdle) {
			activeTutorSessionId = null;
			if (shapes.length === 0 && pendingNarrationCount === 0) {
				canvasMode = 'screen_overlay';
				overlayRail = 'tutor';
			}
		}
	}

	function applyTutorStatusPayload(payload: TutorOverlayStatusPayload | string | null | undefined) {
		if (!payload) return;
		if (typeof payload === 'string') {
			setTutorStatus(payload === 'working' ? 'working' : 'idle');
			return;
		}
		const nextStatus = payload.status === 'working' ? 'working' : 'idle';
		const sessionId = payloadSessionId(payload);
		const nextCanvasMode = payloadCanvasMode(payload);
		const nextRail = payloadOverlayRail(payload);
		if (
			nextStatus === 'idle' &&
			sessionId &&
			activeTutorSessionId &&
			sessionId !== activeTutorSessionId
		) {
			return;
		}
		setTutorStatus(nextStatus, sessionId, { canvasMode: nextCanvasMode, rail: nextRail });
	}

	function clearVisibleTutorOverlay(
		cancelReason: 'user' | 'replaced' = 'user',
		options: { resetCanvasMode?: boolean } = {}
	) {
		resetNarrationQueue(cancelReason);
		clearPendingStoryboards();
		drawnShapeIds.clear();
		shapes = [];
		overlayExpiresAtMs = null;
		currentStep = null;
		replayReady = false;
		scheduleShapeExpiry();
		clearReplayReadyTimer();
		if (options.resetCanvasMode === true) {
			canvasMode = 'screen_overlay';
			overlayRail = 'tutor';
		}
		if (cancelReason === 'user') {
			hasRenderedTutorContent = false;
		}
	}

	async function notifyTutorOverlayIdle(sessionId: string | null) {
		if (typeof window === 'undefined' || !('__TAURI_INTERNALS__' in window)) return;
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			await invoke('show_tutor_overlay_status', {
				status: 'idle',
				sessionId,
				canvasMode,
				rail: overlayRail
			});
		} catch (error) {
			console.warn('[draw-overlay] show_tutor_overlay_status idle failed:', error);
		}
	}

	async function dismissTutorOverlay(payload?: TutorOverlayDismissPayload | null) {
		const sessionId = payloadSessionId(payload) ?? activeTutorSessionId;
		clearVisibleTutorOverlay('user', { resetCanvasMode: true });
		setTutorStatus('idle', sessionId, { clearSessionOnIdle: true });
		await notifyTutorOverlayIdle(sessionId);
		if (sessionId) {
			const cancelled = await chatStore.cancelChatRun(sessionId);
			if (!cancelled) {
				console.debug('[draw-overlay] no active tutor chat run to cancel', { sessionId });
			}
		}
	}

	function neonizeColor(value?: string) {
		const raw = value?.trim();
		if (!raw) return DEFAULT_DRAW_COLOR;
		const named = NEON_COLOR_MAP[raw.toLowerCase()];
		if (named) return named;
		const parsed = parseHexColor(raw);
		if (!parsed) return DEFAULT_DRAW_COLOR;
		const hue = rgbHue(parsed.r, parsed.g, parsed.b);
		if (hue < 28 || hue >= 335) return '#ff3d8b';
		if (hue < 55) return '#ff7a18';
		if (hue < 82) return '#fff700';
		if (hue < 155) return '#39ff14';
		if (hue < 200) return '#00ffd1';
		if (hue < 245) return '#00e5ff';
		if (hue < 290) return '#b967ff';
		return '#ff3df2';
	}

	const NEON_COLOR_MAP: Record<string, string> = {
		orange: '#ff7a18',
		amber: '#ffb000',
		yellow: '#fff700',
		blue: '#00e5ff',
		cyan: '#00f5ff',
		teal: '#00ffd1',
		green: '#39ff14',
		lime: '#b6ff00',
		red: '#ff1744',
		pink: '#ff3df2',
		magenta: '#ff3df2',
		purple: '#b967ff',
		violet: '#b967ff',
		white: '#f8fbff',
		black: '#00f5ff'
	};

	function parseHexColor(value: string) {
		const match = value.match(/^#([0-9a-f]{3}|[0-9a-f]{6})$/i);
		if (!match) return null;
		const hex = match[1];
		const full =
			hex.length === 3
				? hex
						.split('')
						.map((char) => `${char}${char}`)
						.join('')
				: hex;
		return {
			r: Number.parseInt(full.slice(0, 2), 16),
			g: Number.parseInt(full.slice(2, 4), 16),
			b: Number.parseInt(full.slice(4, 6), 16)
		};
	}

	function rgbHue(r: number, g: number, b: number) {
		const red = r / 255;
		const green = g / 255;
		const blue = b / 255;
		const max = Math.max(red, green, blue);
		const min = Math.min(red, green, blue);
		const delta = max - min;
		if (delta === 0) return 0;
		let hue = 0;
		if (max === red) hue = ((green - blue) / delta) % 6;
		else if (max === green) hue = (blue - red) / delta + 2;
		else hue = (red - green) / delta + 4;
		return (hue * 60 + 360) % 360;
	}

	function hexAlpha(hex: string, alpha: number) {
		const parsed = parseHexColor(hex);
		if (!parsed) return `rgba(0, 245, 255, ${alpha})`;
		return `rgba(${parsed.r}, ${parsed.g}, ${parsed.b}, ${alpha})`;
	}

	function stringOr(value: unknown) {
		return typeof value === 'string' && value.trim() ? value : undefined;
	}

	function numberOr(value: unknown, fallback = 0) {
		return typeof value === 'number' && Number.isFinite(value) ? Number(value) : fallback;
	}

	function numberAny(shape: TutorShape, keys: Array<keyof TutorShape>, fallback = 0) {
		for (const key of keys) {
			const value = shape[key];
			if (typeof value === 'number' && Number.isFinite(value)) return value;
		}
		return fallback;
	}

	// Paint layer for shapes that tie on z_index AND reveal_order. An opaque
	// background emitted after a label used to paint straight over it, because
	// nothing separated fills from text — the model controls emission order and
	// has no reason to think about occlusion. An explicit `z_index` still wins;
	// this only decides ties, so deliberate layering is untouched.
	const PAINT_LAYER_FILL = 0;
	const PAINT_LAYER_STROKE = 1;
	const PAINT_LAYER_TEXT = 2;

	function paintLayer(shape: TutorShape): number {
		if (shapeText(shape).trim()) return PAINT_LAYER_TEXT;
		if (shape.fill || isRectLike(shape) || shape.type === 'area_fill' || shape.type === 'highlight')
			return PAINT_LAYER_FILL;
		return PAINT_LAYER_STROKE;
	}

	function sortShapesForDisplay(list: TutorShape[]) {
		return [...list].sort(
			(a, b) =>
				numberOr(a.z_index) - numberOr(b.z_index) ||
				numberOr(a.reveal_order) - numberOr(b.reveal_order) ||
				paintLayer(a) - paintLayer(b)
		);
	}

	function applyTextLabelSpacing(list: TutorShape[]) {
		const candidates = list
			.map((shape, index) => textLabelCandidate(shape, index))
			.filter((candidate): candidate is TutorTextLabelCandidate => candidate !== null)
			.sort((a, b) => a.y - b.y || a.x - b.x || a.index - b.index);
		if (candidates.length <= 1) return list;

		const rows: Array<{ y: number; items: TutorTextLabelCandidate[] }> = [];
		for (const candidate of candidates) {
			const row = rows.find((entry) => Math.abs(entry.y - candidate.y) <= TEXT_LABEL_ROW_THRESHOLD);
			if (!row) {
				rows.push({ y: candidate.y, items: [candidate] });
				continue;
			}
			row.items.push(candidate);
			row.y =
				row.items.reduce((total, item) => total + item.y, 0) / Math.max(1, row.items.length);
		}

		const offsets = new Map<number, number>();
		const maxLabelX = Math.max(TEXT_LABEL_EDGE_PADDING, coordinateWidth - TEXT_LABEL_EDGE_PADDING);
		for (const row of rows) {
			row.items.sort((a, b) => a.x - b.x || a.index - b.index);
			let cursor = -Infinity;
			for (const candidate of row.items) {
				let adjustedX = Math.max(candidate.x, cursor + MIN_TEXT_LABEL_HORIZONTAL_GAP);
				const maxStartX = Math.max(
					TEXT_LABEL_EDGE_PADDING,
					maxLabelX - Math.min(candidate.width, maxLabelX - TEXT_LABEL_EDGE_PADDING)
				);
				adjustedX = Math.min(adjustedX, maxStartX);
				const offset = Math.max(0, adjustedX - candidate.x);
				if (offset > 0) offsets.set(candidate.index, offset);
				cursor = adjustedX + candidate.width;
			}
		}

		if (offsets.size === 0) return list;
		return list.map((shape, index) => {
			const offset = offsets.get(index);
			return offset ? { ...shape, __renderLabelOffsetX: offset } : shape;
		});
	}

	function textLabelCandidate(shape: TutorShape, index: number): TutorTextLabelCandidate | null {
		const text = shapeText(shape).trim();
		if (!text) return null;
		// Semantic box labels are contained by their own box and do not participate
		// in the external-label collision pass.
		if (isSemanticBoxType(shape.type)) return null;
		const anchor = textLabelAnchor(shape);
		if (!anchor) return null;
		return {
			index,
			x: anchor.x,
			y: anchor.y,
			width: estimateTextLabelWidth(shape, text)
		};
	}

	function textLabelAnchor(shape: TutorShape) {
		if (isArrowLike(shape) || isLineLike(shape) || shape.type === 'square_on_segment') {
			return { x: segmentEndX(shape), y: segmentEndY(shape) - 10 };
		}
		if (isPathLike(shape)) return pathLabelAnchor(shape);
		if (isRectLike(shape)) return { x: rawLabelX(shape), y: rawLabelY(shape) - 10 };
		if (shape.type === 'right_angle_marker') return { x: rawLabelX(shape), y: rawLabelY(shape) - 8 };
		if (isHandwritingLike(shape)) return { x: rawLabelX(shape), y: rawLabelY(shape) };
		if (isTextLike(shape)) return { x: rawLabelX(shape), y: rawLabelY(shape) };
		return null;
	}

	function estimateTextLabelWidth(shape: TutorShape, text: string) {
		if (isHandwritingLike(shape)) {
			const fontSize = handwritingFontSize(shape);
			return Math.min(900, Math.max(44, Array.from(text).length * fontSize * 0.72));
		}
		const fontSize = shape.type === 'formula' ? 22 : 16;
		if (shape.type !== 'formula') {
			// Plain labels render in `.draw-label`, whose family is exactly what
			// `measureLabelText` measures against — so this width can be real
			// rather than `len * fontSize * 0.6`. It decides how far the spacing
			// pass advances its cursor, so an underestimate is an overlap and an
			// overestimate is a needless nudge. The clamps are kept: they bound
			// how far a label may be displaced, which is a separate policy from
			// how wide it is.
			return Math.min(620, Math.max(28, measureLabelText(text, fontSize).width));
		}
		// Formula and handwriting draw in other fonts, which this canvas is not
		// configured for; measuring them against Inter would be a confident
		// wrong answer, so they keep the ratio estimate.
		return Math.min(620, Math.max(28, Array.from(text).length * fontSize * 0.66));
	}

	function shapeText(shape: TutorShape) {
		return shape.text || shape.formula || shape.label || '';
	}

	function shapeStepLabel(shape: TutorShape) {
		return shape.tutor_step_label || shape.step_label || shape.label || shape.text || shape.reveal_id || '';
	}

	function stepBubbleFromShape(shape: TutorShape): TutorStepBubble | null {
		const label = shapeStepLabel(shape).trim();
		const narration = (shape.narration || '').trim();
		if (!label && !narration) return null;
		return {
			revealId: shape.storyboard_step_id || shape.reveal_id,
			order: Number.isFinite(shape.reveal_order) ? Number(shape.reveal_order) : undefined,
			label: label || `Step ${shape.reveal_order ?? ''}`.trim(),
			narration: narration || undefined,
			waitForVoice: shape.wait_for_voice === true
		};
	}

	function stepNarrationKey(step: TutorStepBubble) {
		return [
			step.revealId ?? '',
			step.order ?? '',
			step.label.trim(),
			step.narration?.trim() ?? ''
		].join('|');
	}

	function stepNarrationText(step: TutorStepBubble) {
		const narration = step.narration?.trim();
		if (narration) return narration;
		return step.waitForVoice ? step.label.trim() : '';
	}

	function resetNarrationQueue(cancelReason: 'user' | 'replaced' = 'replaced') {
		narrationEpoch += 1;
		narrationTail = Promise.resolve();
		speakingStepKey = null;
		narratedStepKeys.clear();
		queuedNarrationKeys.clear();
		cancelTutorNarration(cancelReason);
	}

	function queueTutorNarration(
		step: TutorStepBubble,
		options: { force?: boolean; revealPending?: boolean } = {}
	) {
		const text = stepNarrationText(step);
		if (!text) return;
		const key = stepNarrationKey(step);
		if (!options.force && (narratedStepKeys.has(key) || queuedNarrationKeys.has(key))) return;
		queuedNarrationKeys.add(key);
		narratedStepKeys.add(key);
		const epoch = narrationEpoch;
		narrationTail = narrationTail
			.catch(() => undefined)
			.then(async () => {
				if (epoch !== narrationEpoch) {
					queuedNarrationKeys.delete(key);
					return;
				}
				let playbackStarted = false;
				let status: 'completed' | 'cancelled' | 'error' | 'skipped' = 'error';
				const revealAtPlaybackStart = () => {
					if (playbackStarted || epoch !== narrationEpoch) return;
					playbackStarted = true;
					currentStep = step;
					if (options.revealPending) {
						revealPendingShapesForStep(step);
					} else {
						restartShapesForStep(step);
					}
					speakingStepKey = key;
				};
				try {
					status = await speakTutorNarration({
						stepId: key,
						text,
						onStart: revealAtPlaybackStart
					});
				} finally {
					queuedNarrationKeys.delete(key);
					if (!playbackStarted && epoch === narrationEpoch && status !== 'cancelled') {
						revealAtPlaybackStart();
					}
					if (epoch === narrationEpoch && speakingStepKey === key) {
						speakingStepKey = null;
						extendOverlayExpiryAfterNarration();
					}
				}
			});
	}

	function schedulePendingNarrationFlush() {
		if (narrationFlushTimer) return;
		narrationFlushTimer = setTimeout(flushPendingNarrations, 80);
	}

	function flushPendingNarrations() {
		narrationFlushTimer = null;
		const entries = Array.from(pendingStorySteps.entries())
			.filter(([key]) => pendingStoryShapes.has(key))
			.sort((a, b) => numberOr(a[1].order, Number.MAX_SAFE_INTEGER) - numberOr(b[1].order, Number.MAX_SAFE_INTEGER));
		for (const [key, step] of entries) {
			if (queuedNarrationKeys.has(key)) continue;
			if (narratedStepKeys.has(key)) {
				revealPendingShapesForStep(step);
				continue;
			}
			queueTutorNarration(step, { revealPending: true });
		}
	}

	function addPendingNarratedShape(shape: TutorShape, step: TutorStepBubble) {
		const key = stepNarrationKey(step);
		const current = pendingStoryShapes.get(key) ?? [];
		pendingStoryShapes.set(key, [...current, shape]);
		pendingStorySteps.set(key, step);
		updatePendingNarrationCount();
		if (speakingStepKey === key || (narratedStepKeys.has(key) && !queuedNarrationKeys.has(key))) {
			revealPendingShapesForStep(step);
			return;
		}
		schedulePendingNarrationFlush();
	}

	function clearRenderedShapesForNewStep() {
		shapes = [];
		overlayExpiresAtMs = null;
		currentStep = null;
		replayReady = false;
		clearReplayReadyTimer();
		scheduleShapeExpiry();
	}

	function revealPendingShapesForStep(step: TutorStepBubble) {
		const key = stepNarrationKey(step);
		const pending = pendingStoryShapes.get(key) ?? [];
		if (pending.length === 0) {
			restartShapesForStep(step);
			return;
		}
		pendingStoryShapes.delete(key);
		pendingStorySteps.delete(key);
		updatePendingNarrationCount();
		let nextShapes = shapes;
		const now = Date.now();
		const shouldClearPrevious = pending.some((shape) => shape.clear_previous === true);
		if (shouldClearPrevious) {
			clearRenderedShapesForNewStep();
			nextShapes = [];
		}
		for (const shape of pending) {
			if (!shouldClearPrevious) {
				nextShapes = clearPersistedShapesForIncomingStep(nextShapes, shape);
			}
			nextShapes = [...nextShapes, { ...shape, created_at_ms: now }];
		}
		shapes = nextShapes;
		currentStep = step;
		if (nextShapes.length > 0) extendOverlayExpiry(nextShapes);
		scheduleShapeExpiry();
		scheduleReplayReady(nextShapes);
	}

	function updateCurrentStepFromShape(shape: TutorShape) {
		const bubble = stepBubbleFromShape(shape);
		if (bubble) {
			if (stepNarrationText(bubble)) {
				addPendingNarratedShape(shape, bubble);
			} else {
				currentStep = bubble;
			}
		}
	}

	function refreshCurrentStepFromShapes(list = shapes, includeNarrated = true) {
		const sorted = sortShapesForDisplay(list);
		for (let index = sorted.length - 1; index >= 0; index -= 1) {
			const bubble = stepBubbleFromShape(sorted[index]);
			if (bubble) {
				if (!includeNarrated && stepNarrationText(bubble)) continue;
				currentStep = bubble;
				return;
			}
		}
		currentStep = null;
	}

	function restartShapesForStep(step: TutorStepBubble) {
		const targetKey = stepNarrationKey(step);
		const now = Date.now();
		let changed = false;
		const restarted = shapes.map((shape) => {
			const shapeStep = stepBubbleFromShape(shape);
			if (!shapeStep || stepNarrationKey(shapeStep) !== targetKey) return shape;
			changed = true;
			return { ...shape, created_at_ms: now };
		});
		if (!changed) return;
		shapes = restarted;
		extendOverlayExpiry(restarted);
		scheduleShapeExpiry();
		scheduleReplayReady(restarted);
	}

	function roundedRectPath(shape: TutorShape) {
		const x = numberOr(shape.x);
		const y = numberOr(shape.y);
		const w = Math.max(0, numberOr(shape.w));
		const h = Math.max(0, numberOr(shape.h));
		const r = Math.min(8, w / 2, h / 2);
		if (w <= 0 || h <= 0) return '';
		return [
			`M ${x + r} ${y}`,
			`H ${x + w - r}`,
			`Q ${x + w} ${y} ${x + w} ${y + r}`,
			`V ${y + h - r}`,
			`Q ${x + w} ${y + h} ${x + w - r} ${y + h}`,
			`H ${x + r}`,
			`Q ${x} ${y + h} ${x} ${y + h - r}`,
			`V ${y + r}`,
			`Q ${x} ${y} ${x + r} ${y}`,
			'Z'
		].join(' ');
	}

	function isSpotlightCutoutShape(shape: TutorShape) {
		if (isPolygonLike(shape)) return Boolean(pointsString(shape));
		if (!['highlight', 'rect', 'spotlight', 'mask', 'code_highlight'].includes(shape.type)) {
			return false;
		}
		return numberOr(shape.w) > 0 && numberOr(shape.h) > 0;
	}

	function spotlightPadding(shape: TutorShape) {
		return shape.type === 'code_highlight' ? 6 : 8;
	}

	function spotlightRectX(shape: TutorShape) {
		return Math.max(0, numberOr(shape.x) - spotlightPadding(shape));
	}

	function spotlightRectY(shape: TutorShape) {
		return Math.max(0, numberOr(shape.y) - spotlightPadding(shape));
	}

	function spotlightRectWidth(shape: TutorShape) {
		return Math.max(0, numberOr(shape.w) + spotlightPadding(shape) * 2);
	}

	function spotlightRectHeight(shape: TutorShape) {
		return Math.max(0, numberOr(shape.h) + spotlightPadding(shape) * 2);
	}

	function spotlightRectRadius(shape: TutorShape) {
		const radius = numberAny(shape, ['radius', 'r'], shape.type === 'code_highlight' ? 7 : 12);
		return Math.max(4, Math.min(22, radius + 2));
	}

	function segmentStartX(shape: TutorShape) {
		return numberAny(shape, ['from_x', 'x1']);
	}

	function segmentStartY(shape: TutorShape) {
		return numberAny(shape, ['from_y', 'y1']);
	}

	function segmentEndX(shape: TutorShape) {
		return numberAny(shape, ['to_x', 'x2']);
	}

	function segmentEndY(shape: TutorShape) {
		return numberAny(shape, ['to_y', 'y2']);
	}

	function arrowHeadPoints(shape: TutorShape) {
		const fromX = segmentStartX(shape);
		const fromY = segmentStartY(shape);
		const toX = segmentEndX(shape);
		const toY = segmentEndY(shape);
		const angle = Math.atan2(toY - fromY, toX - fromX);
		const length = 16;
		const width = 11;
		const baseX = toX - Math.cos(angle) * length;
		const baseY = toY - Math.sin(angle) * length;
		const normalX = Math.cos(angle + Math.PI / 2);
		const normalY = Math.sin(angle + Math.PI / 2);
		const p1 = `${toX},${toY}`;
		const p2 = `${baseX + normalX * width * 0.5},${baseY + normalY * width * 0.5}`;
		const p3 = `${baseX - normalX * width * 0.5},${baseY - normalY * width * 0.5}`;
		return `${p1} ${p2} ${p3}`;
	}

	function safeSvgPathData(value: unknown) {
		if (typeof value !== 'string') return '';
		const trimmed = value.trim();
		if (!trimmed || trimmed.length > 8000) return '';
		return /^[MmZzLlHhVvCcSsQqTtAa0-9,.\-+\sEe]+$/.test(trimmed) ? trimmed : '';
	}

	function pathData(shape: TutorShape) {
		if (shape.type === 'path') return safeSvgPathData(shape.d) || safeSvgPathData(shape.path);
		if (shape.type === 'curve') return curvePath(shape);
		if (shape.type === 'freehand') return smoothPointsPath(shape);
		return '';
	}

	function curvePath(shape: TutorShape) {
		const fromX = segmentStartX(shape);
		const fromY = segmentStartY(shape);
		const toX = segmentEndX(shape);
		const toY = segmentEndY(shape);
		const c1x = numberAny(shape, ['control1_x', 'c1x', 'control_x'], Number.NaN);
		const c1y = numberAny(shape, ['control1_y', 'c1y', 'control_y'], Number.NaN);
		const c2x = numberAny(shape, ['control2_x', 'c2x'], Number.NaN);
		const c2y = numberAny(shape, ['control2_y', 'c2y'], Number.NaN);
		if (![fromX, fromY, toX, toY, c1x, c1y].every(Number.isFinite)) return '';
		if (Number.isFinite(c2x) && Number.isFinite(c2y)) {
			return `M ${fromX} ${fromY} C ${c1x} ${c1y} ${c2x} ${c2y} ${toX} ${toY}`;
		}
		return `M ${fromX} ${fromY} Q ${c1x} ${c1y} ${toX} ${toY}`;
	}

	function shapePoints(shape: TutorShape) {
		if (typeof shape.points === 'string') {
			return shape.points
				.trim()
				.split(/\s+/)
				.map((pair) => pair.split(',').map(Number))
				.filter((pair) => pair.length >= 2 && pair.every(Number.isFinite))
				.map(([x, y]) => ({ x, y }));
		}
		if (!Array.isArray(shape.points)) return [];
		return shape.points
			.map((point) => {
				if (Array.isArray(point)) return { x: numberOr(point[0], Number.NaN), y: numberOr(point[1], Number.NaN) };
				return { x: numberOr(point.x, Number.NaN), y: numberOr(point.y, Number.NaN) };
			})
			.filter((point) => Number.isFinite(point.x) && Number.isFinite(point.y));
	}

	function smoothPointsPath(shape: TutorShape) {
		const points = shapePoints(shape);
		if (points.length === 0) return '';
		if (points.length === 1) return `M ${points[0].x} ${points[0].y}`;
		if (points.length === 2) return `M ${points[0].x} ${points[0].y} L ${points[1].x} ${points[1].y}`;
		const segments = [`M ${points[0].x} ${points[0].y}`];
		for (let index = 0; index < points.length - 1; index += 1) {
			const previous = points[Math.max(0, index - 1)];
			const current = points[index];
			const next = points[index + 1];
			const afterNext = points[Math.min(points.length - 1, index + 2)];
			const c1x = current.x + (next.x - previous.x) / 6;
			const c1y = current.y + (next.y - previous.y) / 6;
			const c2x = next.x - (afterNext.x - current.x) / 6;
			const c2y = next.y - (afterNext.y - current.y) / 6;
			segments.push(`C ${c1x} ${c1y} ${c2x} ${c2y} ${next.x} ${next.y}`);
		}
		return segments.join(' ');
	}

	function pathLabelAnchor(shape: TutorShape) {
		const points = shapePoints(shape);
		if (points.length > 0) {
			const point = points[Math.min(points.length - 1, Math.max(0, Math.floor(points.length * 0.75)))];
			return { x: point.x, y: point.y - 10 };
		}
		if (Number.isFinite(shape.to_x) || Number.isFinite(shape.x2)) {
			return { x: segmentEndX(shape), y: segmentEndY(shape) - 10 };
		}
		if (Number.isFinite(shape.x) && Number.isFinite(shape.y)) {
			return { x: numberOr(shape.x), y: numberOr(shape.y) - 10 };
		}
		return { x: 0, y: 0 };
	}

	function pointsString(shape: TutorShape) {
		return shapePoints(shape)
			.map((point) => `${point.x},${point.y}`)
			.join(' ');
	}

	function squareOnSegmentPoints(shape: TutorShape) {
		const x1 = segmentStartX(shape);
		const y1 = segmentStartY(shape);
		const x2 = segmentEndX(shape);
		const y2 = segmentEndY(shape);
		const dx = x2 - x1;
		const dy = y2 - y1;
		const length = Math.hypot(dx, dy);
		if (!length) return '';
		const side = shape.side || shape.orientation || 'left';
		const sign = side === 'right' || side === '-1' ? -1 : 1;
		const nx = (-dy / length) * sign;
		const ny = (dx / length) * sign;
		return [
			`${x1},${y1}`,
			`${x2},${y2}`,
			`${x2 + nx * length},${y2 + ny * length}`,
			`${x1 + nx * length},${y1 + ny * length}`
		].join(' ');
	}

	// --- Recipe interpreter bridge (Phase 3) -------------------------------
	// The overlay draws in space coordinates inside an SVG viewBox, so the
	// interpreter's projection is identity here (the viewBox handles scaling).
	// Colors reuse the page's neon palette; fills get an alpha via hexAlpha.

	const recipeStyling = {
		mapColor: (raw: string) => neonizeColor(raw),
		applyOpacity: (color: string, opacity: number) => hexAlpha(color, opacity)
	};

	function toRecipeShape(shape: TutorShape): RecipeShape {
		return {
			type: shape.type,
			x: shape.x,
			y: shape.y,
			w: shape.w,
			h: shape.h,
			x1: shape.x1,
			y1: shape.y1,
			x2: shape.x2,
			y2: shape.y2,
			from_x: shape.from_x,
			from_y: shape.from_y,
			to_x: shape.to_x,
			to_y: shape.to_y,
			cx: shape.cx,
			cy: shape.cy,
			r: shape.r,
			// Elliptical radii. This function is a hand-copied whitelist, so a
			// field missing here is dropped silently on the way into the
			// interpreter — which is exactly how a cone base asking for 150x38
			// arrived as `r: undefined` and fell through to the recipe's own
			// `size: 36` default.
			rx: shape.rx,
			ry: shape.ry,
			size: shape.size,
			start_angle: shape.start_angle,
			end_angle: shape.end_angle,
			stroke_width: shape.stroke_width,
			opacity: shape.opacity,
			font_size: shape.font_size,
			c1x: shape.c1x,
			c1y: shape.c1y,
			c2x: shape.c2x,
			c2y: shape.c2y,
			points: shape.points,
			d: shape.d,
			// Forward the RAW `text` only (not shapeText's label/formula fallback):
			// the iOS interpreter resolves the recipe's `text` token against
			// shape.text alone, so matching that keeps web+iOS renders identical.
			text: shape.text,
			color: stringOr(shape.style?.stroke) || stringOr(shape.color),
			fill: stringOr(shape.style?.fill) || stringOr(shape.fill),
			side: shape.side,
			orientation: shape.orientation
		};
	}

	// Whether the recipe path should render this shape (flag on + a recipe
	// exists). When false the template uses the native per-type SVG path.
	function shapeUsesRecipe(shape: TutorShape) {
		return USE_RECIPE_INTERPRETER && tutorPrimitiveRegistry.isSupported(shape.type);
	}

	// Draw commands for a shape, at full progress (the SVG draw-on animation is
	// applied via CSS on the produced elements, matching the native path).
	/** Width-only measurer for in-box label wrapping. */
	const semanticBoxMeasure = (text: string, fontSize: number) =>
		measureLabelText(text, fontSize).width;

	function recipeCommands(shape: TutorShape): DrawCommand[] {
		const recipe = tutorPrimitiveRegistry.recipe(shape.type);
		if (!recipe) return [];
		return renderRecipe({
			recipe,
			shape: toRecipeShape(shape),
			project: (p) => p,
			progress: 1,
			styling: recipeStyling,
			measureText: measureLabelText
		});
	}

	// An SVG path `d` for a stroke/polyline/polygon/arc/bezier command.
	function recipeCommandPath(command: DrawCommand): string {
		if (command.kind !== 'path') return '';
		const points = command.points;
		if (points.length === 0) return '';
		if (command.op === 'bezier') {
			// points = [from, c1, (c2?), to]
			const from = points[0];
			if (points.length >= 4) {
				const [, c1, c2, to] = points;
				return `M ${from.x} ${from.y} C ${c1.x} ${c1.y} ${c2.x} ${c2.y} ${to.x} ${to.y}`;
			}
			const c1 = points[1];
			const to = points[points.length - 1];
			return `M ${from.x} ${from.y} Q ${c1.x} ${c1.y} ${to.x} ${to.y}`;
		}
		const segments = [`M ${points[0].x} ${points[0].y}`];
		for (const p of points.slice(1)) segments.push(`L ${p.x} ${p.y}`);
		if (command.closed) segments.push('Z');
		return segments.join(' ');
	}

	function recipeArrowHeadPoints(command: Extract<DrawCommand, { kind: 'arrowhead' }>) {
		const { from, to } = command;
		const angle = Math.atan2(to.y - from.y, to.x - from.x);
		const length = Math.max(12, command.style.width * 4);
		const p1x = to.x - length * Math.cos(angle - Math.PI / 6);
		const p1y = to.y - length * Math.sin(angle - Math.PI / 6);
		const p2x = to.x - length * Math.cos(angle + Math.PI / 6);
		const p2y = to.y - length * Math.sin(angle + Math.PI / 6);
		return `${p1x},${p1y} ${to.x},${to.y} ${p2x},${p2y}`;
	}

	function rightAnglePath(shape: TutorShape) {
		const x = numberAny(shape, ['x', 'cx']);
		const y = numberAny(shape, ['y', 'cy']);
		const size = Math.max(8, numberOr(shape.size, 28));
		return [`M ${x + size} ${y}`, `L ${x + size} ${y + size}`, `L ${x} ${y + size}`].join(' ');
	}

	function angleMarkerPath(shape: TutorShape) {
		const cx = numberAny(shape, ['cx', 'x']);
		const cy = numberAny(shape, ['cy', 'y']);
		const r = Math.max(8, numberAny(shape, ['r', 'radius', 'size'], 36));
		// Elliptical when asked; identical to the old circular output otherwise.
		const rx = Math.max(1, numberAny(shape, ['rx'], r));
		const ry = Math.max(1, numberAny(shape, ['ry'], r));
		const startDeg = numberOr(shape.start_angle, 0);
		const endDeg = numberOr(shape.end_angle, 90);
		const sweep = Math.abs(endDeg - startDeg);
		// A full turn puts the start and end at the SAME point, and an SVG `A`
		// between identical points renders NOTHING at all — so a closed base
		// circle drawn this way vanished rather than looking wrong. Two
		// half-turns are the standard way to express a whole ellipse.
		if (sweep >= 360) {
			const left = cx - rx;
			const right = cx + rx;
			return `M ${left} ${cy} A ${rx} ${ry} 0 1 1 ${right} ${cy} A ${rx} ${ry} 0 1 1 ${left} ${cy} Z`;
		}
		const start = (startDeg * Math.PI) / 180;
		const end = (endDeg * Math.PI) / 180;
		const startX = cx + Math.cos(start) * rx;
		const startY = cy + Math.sin(start) * ry;
		const endX = cx + Math.cos(end) * rx;
		const endY = cy + Math.sin(end) * ry;
		const largeArc = sweep > 180 ? 1 : 0;
		return `M ${startX} ${startY} A ${rx} ${ry} 0 ${largeArc} 1 ${endX} ${endY}`;
	}

	function rawLabelX(shape: TutorShape) {
		if (Number.isFinite(shape.x)) return numberOr(shape.x);
		if (Number.isFinite(shape.cx)) return numberOr(shape.cx);
		if (Number.isFinite(shape.to_x) || Number.isFinite(shape.x2)) return segmentEndX(shape);
		if (Number.isFinite(shape.from_x) || Number.isFinite(shape.x1)) return segmentStartX(shape);
		return 0;
	}

	function labelX(shape: TutorShape) {
		return labelOffsetX(shape, rawLabelX(shape));
	}

	function labelOffsetX(shape: TutorShape, baseX: number) {
		return baseX + numberOr(shape.__renderLabelOffsetX, 0);
	}

	function rawLabelY(shape: TutorShape) {
		if (Number.isFinite(shape.y)) return numberOr(shape.y);
		if (Number.isFinite(shape.cy)) return numberOr(shape.cy);
		if (Number.isFinite(shape.to_y) || Number.isFinite(shape.y2)) return segmentEndY(shape) - 10;
		if (Number.isFinite(shape.from_y) || Number.isFinite(shape.y1)) return segmentStartY(shape) - 10;
		return 0;
	}

	function labelY(shape: TutorShape) {
		return rawLabelY(shape);
	}

	function isRectLike(shape: TutorShape) {
		if (shape.type === 'area_fill') return !pointsString(shape);
		return [
			'highlight',
			'rect',
			'mask',
			'spotlight',
			'free_body_body',
			'code_highlight',
			'stack_frame',
			'heap_object',
			'state_box',
			'flow_node',
			'memory_cell'
		].includes(shape.type);
	}

	function isArrowLike(shape: TutorShape) {
		return ['arrow', 'vector_arrow', 'force_arrow', 'component_vector', 'pointer_arrow', 'flow_edge'].includes(
			shape.type
		);
	}

	function isLineLike(shape: TutorShape) {
		return ['line', 'axis', 'trajectory', 'field_line', 'measurement_tick'].includes(shape.type);
	}

	function isPathLike(shape: TutorShape) {
		return ['path', 'curve', 'freehand'].includes(shape.type);
	}

	function isHandwritingLike(shape: TutorShape) {
		return ['handwriting', 'cursive_text'].includes(shape.type);
	}

	function handwritingFontSize(shape: TutorShape) {
		return Math.max(34, Math.min(180, numberAny(shape, ['font_size', 'size'], 92)));
	}

	function isPolygonLike(shape: TutorShape) {
		return shape.type === 'polygon' || (shape.type === 'area_fill' && Boolean(pointsString(shape)));
	}

	function isTextLike(shape: TutorShape) {
		return ['label', 'callout', 'formula', 'side_label', 'unit_label', 'timeline_tick'].includes(shape.type);
	}

	function isMarkerLike(shape: TutorShape) {
		return ['angle_marker', 'perpendicular_marker', 'parallel_marker', 'arc'].includes(shape.type);
	}

	function shapeTtlMs(shape: TutorShape) {
		if (shape.persist === true) return null;
		const ttl = Number.isFinite(shape.ttl_ms) ? Number(shape.ttl_ms) : DEFAULT_SHAPE_TTL_MS;
		if (ttl <= 0) return null;
		return Math.max(MIN_SHAPE_TTL_MS, ttl);
	}

	function extendOverlayExpiry(list = shapes) {
		let ttlMs = 0;
		let drawMs = 0;
		let hasPersistentShape = false;
		for (const shape of list) {
			const shapeTtl = shapeTtlMs(shape);
			if (shapeTtl === null) {
				hasPersistentShape = true;
			} else {
				ttlMs = Math.max(ttlMs, shapeTtl);
			}
			drawMs = Math.max(drawMs, drawDurationMs(shape, DEFAULT_DRAW_DURATION_MS));
		}
		overlayExpiresAtMs = !hasPersistentShape && ttlMs > 0 ? Date.now() + drawMs + ttlMs : null;
	}

	function extendOverlayExpiryAfterNarration(list = shapes) {
		let ttlMs = 0;
		for (const shape of list) {
			const shapeTtl = shapeTtlMs(shape);
			if (shapeTtl === null) return;
			ttlMs = Math.max(ttlMs, shapeTtl);
		}
		if (ttlMs <= 0) return;
		const narrationExpiry = Date.now() + ttlMs;
		overlayExpiresAtMs =
			overlayExpiresAtMs === null ? narrationExpiry : Math.max(overlayExpiresAtMs, narrationExpiry);
		scheduleShapeExpiry();
	}

	function pruneExpiredShapes() {
		if (overlayExpiresAtMs !== null && overlayExpiresAtMs <= Date.now()) {
			shapes = [];
			overlayExpiresAtMs = null;
			currentStep = null;
			replayReady = false;
			resetNarrationQueue();
		}
		scheduleShapeExpiry();
	}

	function scheduleShapeExpiry() {
		if (shapeExpiryTimer) {
			clearTimeout(shapeExpiryTimer);
			shapeExpiryTimer = null;
		}
		if (overlayExpiresAtMs === null) return;
		shapeExpiryTimer = setTimeout(pruneExpiredShapes, Math.max(0, overlayExpiresAtMs - Date.now() + 16));
	}

	function applyTutorShapes(incoming: TutorShape | TutorShape[]) {
		const list = Array.isArray(incoming) ? incoming : [incoming];
		let nextShapes = shapes;
			let changed = false;
			let pendingChanged = false;
			for (const rawShape of list) {
				if (!rawShape || !rawShape.type) continue;
				const incomingCanvasMode = shapeCanvasMode(rawShape);
				if (incomingCanvasMode) canvasMode = incomingCanvasMode;
				if (rawShape.type === 'clear') {
					clearVisibleTutorOverlay('replaced', { resetCanvasMode: false });
					if (incomingCanvasMode) canvasMode = incomingCanvasMode;
					nextShapes = shapes;
					changed = true;
					continue;
			}
			const id = rawShape.id || `local-draw-${Date.now()}-${++localShapeSequence}`;
			if (drawnShapeIds.has(id)) continue;
			const shape = { ...rawShape, id, created_at_ms: Date.now() };
			hasRenderedTutorContent = true;
			const step = stepBubbleFromShape(shape);
			const narratedText = step ? stepNarrationText(step) : '';
			drawnShapeIds.add(id);
			if (step && narratedText) {
				addPendingNarratedShape(shape, step);
				pendingChanged = true;
				continue;
			}
			if (shape.clear_previous === true) {
				resetNarrationQueue();
				clearPendingStoryboards();
				nextShapes = [];
			} else {
				nextShapes = clearPersistedShapesForIncomingStep(nextShapes, shape);
			}
			nextShapes = [...nextShapes, shape];
			updateCurrentStepFromShape(shape);
			changed = true;
		}
		if (changed) {
			shapes = nextShapes;
			if (nextShapes.length > 0) extendOverlayExpiry(nextShapes);
			if (!currentStep) refreshCurrentStepFromShapes(nextShapes, false);
			scheduleShapeExpiry();
			scheduleReplayReady(nextShapes);
		}
		if (pendingChanged && !changed) {
			scheduleShapeExpiry();
		}
	}

	function stepIdentityKeys(shape: TutorShape) {
		return new Set(
			[
				shape.reveal_id,
				shape.storyboard_step_id,
				shape.tutor_step_label,
				shape.step_label,
				shape.label,
				shape.text,
				shape.formula
			]
				.map((value) => (typeof value === 'string' ? value.trim() : ''))
				.filter(Boolean)
		);
	}

	function clearPersistedShapesForIncomingStep(existingShapes: TutorShape[], incoming: TutorShape) {
		const stepKeys = stepIdentityKeys(incoming);
		if (stepKeys.size === 0) return existingShapes;
		return existingShapes.filter((shape) => {
			const persistUntil = shape.persist_until_step?.trim();
			if (!persistUntil || !stepKeys.has(persistUntil)) return true;
			if (shape.id) drawnShapeIds.delete(shape.id);
			return false;
		});
	}

	onMount(() => {
		const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
		installScopedApiFetch();
		// Fetch the merged scoped recipe set so the interpreter path (behind
		// USE_RECIPE_INTERPRETER) can render server-authored primitives. A
		// failure is non-fatal — the native path renders regardless.
		void tutorPrimitiveRegistry.refresh();
		refreshCoordinateSpace();
		window.addEventListener('resize', refreshCoordinateSpace);
		if (!isTauri) return;

		void (async () => {
			const { listen } = await import('@tauri-apps/api/event');
			const { invoke } = await import('@tauri-apps/api/core');
			async function pullPendingDrawShapes() {
				try {
					const pending = await invoke<TutorShape[]>('take_overlay_draw_shapes');
					if (pending?.length) applyTutorShapes(pending);
				} catch (err) {
					console.warn('[draw-overlay] take_overlay_draw_shapes failed:', err);
				}
			}
			async function pullTutorOverlayStatus() {
				try {
					const pendingStatus = await invoke<TutorOverlayStatusPayload | string | null>(
						'take_tutor_overlay_status'
					);
					applyTutorStatusPayload(pendingStatus);
				} catch (err) {
					console.warn('[draw-overlay] take_tutor_overlay_status failed:', err);
				}
			}
			unlistenDraw = await listen<TutorShape>('overlay-draw-shape', (event) => {
				applyTutorShapes(event.payload);
			});
			unlistenDeeper = await listen('overlay-explain-deeper-request', () => {
				void requestExplainDeeper();
			});
			unlistenReplay = await listen('overlay-replay-request', () => {
				replayCurrentShapes();
			});
			unlistenKeepShowing = await listen('overlay-keep-showing-request', () => {
				keepCurrentOverlayShowing();
			});
			unlistenDismiss = await listen<TutorOverlayDismissPayload>('overlay-dismiss-request', (event) => {
				void dismissTutorOverlay(event.payload);
			});
			unlistenCopilotUserAction = await listen<CopilotUserActionPayload>(
				'overlay-copilot-user-action',
				(event) => {
					void postCopilotUserAction(event.payload);
				}
			);
			unlistenStatus = await listen<TutorOverlayStatusPayload>('tutor-overlay-status', (event) => {
				applyTutorStatusPayload(event.payload);
			});
			await pullTutorOverlayStatus();
			await pullPendingDrawShapes();
			pendingDrainTimer = setInterval(() => {
				void pullPendingDrawShapes();
			}, 500);
			const onVisibilityChange = () => {
				if (!document.hidden) void pullPendingDrawShapes();
			};
			document.addEventListener('visibilitychange', onVisibilityChange);
			removeVisibilityListener = () => {
				document.removeEventListener('visibilitychange', onVisibilityChange);
			};
		})();
	});

	onDestroy(() => {
		unlistenDraw?.();
		unlistenReplay?.();
		unlistenDeeper?.();
		unlistenKeepShowing?.();
		unlistenDismiss?.();
		unlistenCopilotUserAction?.();
		unlistenStatus?.();
		if (pendingDrainTimer) clearInterval(pendingDrainTimer);
		if (shapeExpiryTimer) clearTimeout(shapeExpiryTimer);
		clearReplayReadyTimer();
		clearNarrationFlushTimer();
		resetNarrationQueue();
		clearPendingStoryboards();
		removeVisibilityListener?.();
		if (typeof window !== 'undefined') {
			window.removeEventListener('resize', refreshCoordinateSpace);
		}
	});
</script>

<svelte:head>
	<title>{PRODUCT_NAME} Tutor Overlay</title>
</svelte:head>

{#if showDimLayer}
	{#if copilotSpotlightActive}
		<svg
			class="spotlight-dim-layer"
			viewBox={`0 0 ${coordinateWidth} ${coordinateHeight}`}
			preserveAspectRatio="none"
			aria-hidden="true"
		>
			<defs>
				<mask id="copilot-spotlight-mask">
					<rect x="0" y="0" width={coordinateWidth} height={coordinateHeight} fill="white" />
					{#each spotlightCutoutShapes as shape (shape.id + ':spotlight:' + (shape.created_at_ms ?? 0))}
						{#if isPolygonLike(shape)}
							<polygon points={pointsString(shape)} fill="black" />
						{:else}
							<rect
								x={spotlightRectX(shape)}
								y={spotlightRectY(shape)}
								width={spotlightRectWidth(shape)}
								height={spotlightRectHeight(shape)}
								rx={spotlightRectRadius(shape)}
								ry={spotlightRectRadius(shape)}
								fill="black"
							/>
						{/if}
					{/each}
				</mask>
			</defs>
			<rect
				x="0"
				y="0"
				width={coordinateWidth}
				height={coordinateHeight}
				class="spotlight-dim-layer__shade"
				mask="url(#copilot-spotlight-mask)"
			/>
		</svg>
	{:else}
		<div class="dim-layer" class:dim-layer--blackboard={blackboardActive} aria-hidden="true"></div>
	{/if}
	{#if blackboardActive}
		<div class="blackboard-layer" aria-hidden="true"></div>
	{/if}
	<div class="aurora-edge-layer" aria-hidden="true">
		<span class="aurora-edge aurora-edge--top"></span>
		<span class="aurora-edge aurora-edge--right"></span>
		<span class="aurora-edge aurora-edge--bottom"></span>
		<span class="aurora-edge aurora-edge--left"></span>
	</div>
{/if}

{#if showWorkingStatusBubble}
	<div class="status-bubble" class:status-bubble--with-controls={showDismissControl} aria-hidden="true">
		<div class="status-pulse"></div>
		<span>{canvasMode === 'blackboard' ? 'Please wait, preparing blackboard' : 'Please wait, creating explanation'}</span>
	</div>
{/if}

<svg
	class="draw-layer"
	class:draw-layer--blackboard={canvasMode === 'blackboard'}
	viewBox={`0 0 ${coordinateWidth} ${coordinateHeight}`}
	preserveAspectRatio="none"
	aria-hidden="true"
>
	{#each visibleShapes as shape (shape.id + ':' + (shape.created_at_ms ?? 0) + ':' + replayGeneration)}
		{#if shapeUsesRecipe(shape)}
			{#each recipeCommands(shape) as command, commandIndex (commandIndex)}
				{#if command.kind === 'rect'}
					<rect
						class="draw-stroke"
						x={command.x}
						y={command.y}
						width={command.width}
						height={command.height}
						rx={command.radius}
						ry={command.radius}
						fill={command.style.fill ? command.style.fillColor : 'transparent'}
						stroke={command.style.doStroke ? command.style.strokeColor : 'none'}
						stroke-width={Math.max(1, command.style.width)}
						stroke-dasharray={command.style.dashed
							? `${command.style.width * 2} ${command.style.width * 1.5}`
							: undefined}
						pathLength="1"
						style={shapeAnimationStyle(shape, 1700)}
					/>
				{:else if command.kind === 'circle'}
					<circle
						class="draw-stroke"
						cx={command.cx}
						cy={command.cy}
						r={command.r}
						fill={command.style.fill ? command.style.fillColor : 'transparent'}
						stroke={command.style.doStroke ? command.style.strokeColor : 'none'}
						stroke-width={Math.max(1, command.style.width)}
						stroke-dasharray={command.style.dashed
							? `${command.style.width * 2} ${command.style.width * 1.5}`
							: undefined}
						pathLength="1"
						style={shapeAnimationStyle(shape, 1600)}
					/>
				{:else if command.kind === 'arrowhead'}
					<polygon
						class="arrow-head"
						points={recipeArrowHeadPoints(command)}
						fill={command.style.strokeColor}
						style="{shapeAnimationStyle(shape, 1500)} transform-origin: {command.to.x}px {command.to.y}px;"
					/>
				{:else if command.kind === 'label'}
					<text
						class="draw-label"
						class:handwriting-primitive={command.cursive}
						class:cursive-primitive={command.cursive}
						x={command.at.x + numberOr(shape.__renderLabelOffsetX, 0)}
						y={command.at.y}
						fill={command.color}
						text-anchor={command.anchor === 'center'
							? 'middle'
							: command.anchor === 'trailing'
								? 'end'
								: 'start'}
						font-weight="700"
						font-size={command.cursive ? command.fontSize : 16}
						style={shapeAnimationStyle(shape, 1500)}
					>
						{command.text}
					</text>
				{:else if command.kind === 'path'}
					<path
						class="draw-stroke"
						d={recipeCommandPath(command)}
						fill={command.style.fill ? command.style.fillColor : 'transparent'}
						stroke={command.style.doStroke ? command.style.strokeColor : 'none'}
						stroke-width={Math.max(1, command.style.width)}
						stroke-linecap="round"
						stroke-linejoin="round"
						stroke-dasharray={command.style.dashed
							? `${command.style.width * 2} ${command.style.width * 1.5}`
							: undefined}
						pathLength="1"
						style={shapeAnimationStyle(shape, 1600)}
					/>
				{/if}
			{/each}
		{:else if isRectLike(shape)}
			{@const semanticLabel = semanticBoxLabelLayout(shape, shapeText(shape), semanticBoxMeasure)}
			<path
				class="draw-stroke highlight-stroke"
				d={roundedRectPath(shape)}
				fill={shapeFill(
					shape,
					shape.type === 'area_fill' || shape.type === 'spotlight' || shape.type === 'mask'
						? DEFAULT_FILL_COLOR
						: 'transparent'
				)}
				fill-opacity={shapeOpacity(
					shape,
					shape.type === 'area_fill' || shape.type === 'spotlight' || shape.type === 'mask' ? 0.22 : 1
				)}
				stroke={shapeColor(shape)}
				stroke-width={strokeWidth(shape, shape.type === 'area_fill' ? 2 : 4)}
				pathLength="1"
				style={shapeAnimationStyle(shape, 1700)}
			/>
			{#if semanticLabel}
				<text
					class="draw-label semantic-box-label"
					x={semanticLabel.x}
					y={semanticLabel.centerY}
					fill={shapeColor(shape)}
					font-weight="700"
					font-size="16"
					text-anchor="middle"
					dominant-baseline="middle"
					style={shapeAnimationStyle(shape, 1700)}
				>
					{#each semanticLabel.lines as line, index}
						<tspan
							x={semanticLabel.x}
							y={semanticLabel.centerY + (index - (semanticLabel.lines.length - 1) / 2) * semanticLabel.lineHeight}
						>
							{line}
						</tspan>
					{/each}
				</text>
			{:else if shapeText(shape)}
				<text
					class="draw-label"
					x={labelX(shape)}
					y={labelY(shape) - 10}
					fill={shapeColor(shape)}
					font-weight="700"
					font-size="16"
					style={shapeAnimationStyle(shape, 1700)}
				>
					{shapeText(shape)}
				</text>
			{/if}
		{:else if isArrowLike(shape)}
			<line
				class="draw-stroke arrow-stroke"
				x1={segmentStartX(shape)}
				y1={segmentStartY(shape)}
				x2={segmentEndX(shape)}
				y2={segmentEndY(shape)}
				stroke={shapeColor(shape)}
				stroke-width={strokeWidth(shape, 4)}
				stroke-linecap="round"
				pathLength="1"
				style={shapeAnimationStyle(shape, 1500)}
			/>
			<polygon
				class="arrow-head"
				points={arrowHeadPoints(shape)}
				fill={shapeColor(shape)}
				style="{shapeAnimationStyle(shape, 1500)} transform-origin: {segmentEndX(shape)}px {segmentEndY(shape)}px;"
			/>
			{#if shapeText(shape)}
				<text
					class="draw-label"
					x={labelOffsetX(shape, segmentEndX(shape))}
					y={segmentEndY(shape) - 10}
					fill={shapeColor(shape)}
					font-weight="700"
					font-size="16"
					style={shapeAnimationStyle(shape, 1500)}
				>
					{shapeText(shape)}
				</text>
			{/if}
		{:else if isLineLike(shape)}
			<line
				class="draw-stroke"
				x1={segmentStartX(shape)}
				y1={segmentStartY(shape)}
				x2={segmentEndX(shape)}
				y2={segmentEndY(shape)}
				stroke={shapeColor(shape)}
				stroke-width={strokeWidth(shape, 3)}
				stroke-linecap="round"
				pathLength="1"
				style={shapeAnimationStyle(shape, 1500)}
			/>
			{#if shapeText(shape)}
				<text
					class="draw-label"
					x={labelOffsetX(shape, segmentEndX(shape))}
					y={segmentEndY(shape) - 10}
					fill={shapeColor(shape)}
					font-weight="700"
					font-size="16"
					style={shapeAnimationStyle(shape, 1500)}
				>
					{shapeText(shape)}
				</text>
			{/if}
		{:else if isPathLike(shape)}
			<path
				class="draw-stroke"
				d={pathData(shape)}
				fill={shapeFill(shape)}
				fill-opacity={shapeOpacity(shape, shapeFill(shape) === 'transparent' ? 1 : 0.16)}
				stroke={shapeColor(shape)}
				stroke-width={strokeWidth(shape, 5)}
				stroke-linecap="round"
				stroke-linejoin="round"
				pathLength="1"
				style={shapeAnimationStyle(shape, 1900)}
			/>
			{#if shapeText(shape)}
				<text
					class="draw-label"
					x={labelX(shape)}
					y={labelY(shape) - 10}
					fill={shapeColor(shape)}
					font-weight="700"
					font-size="16"
					style={shapeAnimationStyle(shape, 1900)}
				>
					{shapeText(shape)}
				</text>
			{/if}
		{:else if isPolygonLike(shape)}
			<polygon
				class="draw-stroke"
				points={pointsString(shape)}
				fill={shapeFill(shape, shape.type === 'area_fill' ? DEFAULT_FILL_COLOR : 'transparent')}
				fill-opacity={shapeOpacity(shape, shape.type === 'area_fill' ? 0.22 : 1)}
				stroke={shapeColor(shape)}
				stroke-width={strokeWidth(shape, 3)}
				pathLength="1"
				style={shapeAnimationStyle(shape, 1700)}
			/>
		{:else if shape.type === 'square_on_segment'}
			<polygon
				class="draw-stroke"
				points={squareOnSegmentPoints(shape)}
				fill={shapeFill(shape, DEFAULT_FILL_COLOR)}
				fill-opacity={shapeOpacity(shape, 0.14)}
				stroke={shapeColor(shape)}
				stroke-width={strokeWidth(shape, 3)}
				pathLength="1"
				style={shapeAnimationStyle(shape, 1700)}
			/>
			{#if shapeText(shape)}
				<text
					class="draw-label"
					x={labelOffsetX(shape, segmentEndX(shape))}
					y={segmentEndY(shape) - 10}
					fill={shapeColor(shape)}
					font-weight="700"
					font-size="16"
					style={shapeAnimationStyle(shape, 1700)}
				>
					{shapeText(shape)}
				</text>
			{/if}
		{:else if shape.type === 'circle'}
			<circle
				class="draw-stroke"
				cx={numberAny(shape, ['cx', 'x'])}
				cy={numberAny(shape, ['cy', 'y'])}
				r={numberAny(shape, ['r', 'radius'])}
				fill={shapeFill(shape)}
				fill-opacity={shapeOpacity(shape, shapeFill(shape) === 'transparent' ? 1 : 0.16)}
				stroke={shapeColor(shape)}
				stroke-width={strokeWidth(shape, 3)}
				pathLength="1"
				style={shapeAnimationStyle(shape, 1600)}
			/>
		{:else if shape.type === 'right_angle_marker'}
			<path
				class="draw-stroke"
				d={rightAnglePath(shape)}
				fill="transparent"
				stroke={shapeColor(shape)}
				stroke-width={strokeWidth(shape, 4)}
				stroke-linecap="round"
				stroke-linejoin="round"
				pathLength="1"
				style={shapeAnimationStyle(shape, 1500)}
			/>
			{#if shapeText(shape)}
				<text
					class="draw-label"
					x={labelX(shape)}
					y={labelY(shape) - 8}
					fill={shapeColor(shape)}
					font-weight="700"
					font-size="16"
					style={shapeAnimationStyle(shape, 1500)}
				>
					{shapeText(shape)}
				</text>
			{/if}
		{:else if isMarkerLike(shape)}
			<path
				class="draw-stroke"
				d={angleMarkerPath(shape)}
				fill="transparent"
				stroke={shapeColor(shape)}
				stroke-width={strokeWidth(shape, 3)}
				stroke-linecap="round"
				pathLength="1"
				style={shapeAnimationStyle(shape, 1500)}
			/>
		{:else if isHandwritingLike(shape)}
			<text
				class="draw-label handwriting-primitive"
				class:cursive-primitive={shape.type === 'cursive_text'}
				x={labelX(shape)}
				y={labelY(shape)}
				fill={shapeColor(shape)}
				font-size={handwritingFontSize(shape)}
				style={shapeAnimationStyle(shape, 1700)}
			>
				{shapeText(shape)}
			</text>
		{:else if isTextLike(shape)}
			<text
				class="draw-label text-primitive"
				x={labelX(shape)}
				y={labelY(shape)}
				fill={shapeColor(shape)}
				font-weight={shape.type === 'formula' ? '800' : '700'}
				font-size={shape.type === 'formula' ? '22' : '16'}
				style={shapeAnimationStyle(shape, 1400)}
			>
				{shapeText(shape)}
			</text>
		{/if}
	{/each}
	{#if showDeeperControl}
		<!-- svelte-ignore a11y_click_events_have_key_events -->
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<g
			class="replay-control deeper-control"
			transform={`translate(${deeperButtonX}, ${replayButtonY})`}
			role="button"
			tabindex="0"
			aria-label="Explain this deeper"
			onclick={() => void requestExplainDeeper()}
			onkeydown={(event) => {
				if (event.key === 'Enter' || event.key === ' ') {
					event.preventDefault();
					void requestExplainDeeper();
				}
			}}
		>
			<rect
				class="replay-control__panel deeper-control__panel"
				width={DEEPER_BUTTON_WIDTH}
				height={REPLAY_BUTTON_HEIGHT}
				rx="17"
			/>
			<path
				class="replay-control__icon deeper-control__icon"
				d="M24 7 V19 M19 14 L24 19 L29 14 M17 24 H31"
				fill="none"
			/>
			<text class="replay-control__label" x="40" y="22">Explain deeper</text>
		</g>
	{/if}
	{#if showKeepShowingControl}
		<g
			class="replay-control keep-showing-control"
			transform={`translate(${keepShowingButtonX}, ${replayButtonY})`}
			aria-hidden="true"
		>
			<rect
				class="replay-control__panel keep-showing-control__panel"
				width={KEEP_SHOWING_BUTTON_WIDTH}
				height={REPLAY_BUTTON_HEIGHT}
				rx="17"
			/>
			<path
				class="replay-control__icon keep-showing-control__icon"
				d="M18 8 H30 M24 8 V22 M20 22 H28"
				fill="none"
			/>
			<text class="replay-control__label" x="40" y="22">Keep showing</text>
		</g>
	{/if}
	{#if showDismissControl}
		<g
			class="replay-control dismiss-control"
			transform={`translate(${dismissButtonX}, ${replayButtonY})`}
			aria-hidden="true"
		>
			<rect
				class="replay-control__panel dismiss-control__panel"
				width={DISMISS_BUTTON_WIDTH}
				height={REPLAY_BUTTON_HEIGHT}
				rx="17"
			/>
			<path
				class="replay-control__icon dismiss-control__icon"
				d="M19 10 L29 20 M29 10 L19 20"
				fill="none"
			/>
			<text class="replay-control__label" x="40" y="22">Dismiss</text>
		</g>
	{/if}
	{#if showReplayControl}
		<g
			class="replay-control"
			transform={`translate(${replayButtonX}, ${replayButtonY})`}
			aria-hidden="true"
		>
			<rect
				class="replay-control__panel"
				width={REPLAY_BUTTON_WIDTH}
				height={REPLAY_BUTTON_HEIGHT}
				rx="17"
			/>
			<path
				class="replay-control__icon"
				d="M23 11 A7 7 0 1 1 19 5.4 M19 5.4 H26 M19 5.4 V12"
				fill="none"
			/>
			<text class="replay-control__label" x="39" y="22">Replay</text>
		</g>
	{/if}
</svg>

{#if currentStep}
	<div class="step-bubble" class:step-bubble--blackboard={canvasMode === 'blackboard'} aria-hidden="true">
		<div class="step-kicker">
			{#if currentStep.order !== undefined}
				Step {currentStep.order}
			{:else}
				Tutor
			{/if}
			{#if currentStep.waitForVoice}
				<span>voice</span>
			{/if}
			{#if currentStepSpeaking}
				<span>speaking</span>
			{/if}
		</div>
		<div class="step-title">{currentStep.label}</div>
		{#if currentStep.narration}
			<div class="step-narration">{currentStep.narration}</div>
		{/if}
	</div>
{/if}

<style>
	@font-face {
		font-family: 'Tutor Playwrite US Trad';
		src: url('/fonts/tutor/PlaywriteUSTrad.ttf') format('truetype');
		font-weight: 100 400;
		font-style: normal;
		font-display: swap;
	}

	:global(html),
	:global(body) {
		width: 100%;
		height: 100%;
		margin: 0;
		padding: 0;
		overflow: hidden;
		background: transparent !important;
		pointer-events: none;
	}

	:global(body::before),
	:global(body::after) {
		content: none !important;
		display: none !important;
	}

	:global(body) {
		color-scheme: light dark;
	}

	:global(body > div) {
		background: transparent !important;
		pointer-events: none;
	}

		.draw-layer {
			position: fixed;
			inset: 0;
			width: 100vw;
		height: 100vh;
		pointer-events: none;
		background: transparent;
			z-index: 1;
		}

		.draw-layer--blackboard {
			filter: saturate(1.12) contrast(1.06);
		}

		.dim-layer {
		position: fixed;
		inset: 0;
		z-index: 0;
		pointer-events: none;
		background:
			radial-gradient(circle at 50% 24%, rgba(15, 23, 42, 0.08), rgba(2, 6, 23, 0.24)),
			rgba(2, 6, 23, 0.22);
		backdrop-filter: saturate(0.78) brightness(0.82);
			animation: dim-layer-in 220ms ease-out forwards;
		}

		.dim-layer--blackboard {
			background:
				radial-gradient(circle at 50% 28%, rgba(15, 23, 42, 0.18), rgba(0, 0, 0, 0.58)),
				linear-gradient(135deg, rgba(4, 14, 24, 0.78), rgba(5, 16, 15, 0.72)),
				rgba(0, 0, 0, 0.52);
			backdrop-filter: saturate(0.7) brightness(0.58);
		}

		.spotlight-dim-layer {
			position: fixed;
			inset: 0;
			z-index: 0;
			width: 100vw;
			height: 100vh;
			pointer-events: none;
			animation: dim-layer-in 220ms ease-out forwards;
		}

		.spotlight-dim-layer__shade {
			fill: rgba(2, 6, 23, 0.54);
		}

		.blackboard-layer {
			position: fixed;
			inset: 0;
			z-index: 0;
			pointer-events: none;
			background:
				linear-gradient(rgba(255, 255, 255, 0.035) 1px, transparent 1px),
				linear-gradient(90deg, rgba(255, 255, 255, 0.028) 1px, transparent 1px),
				radial-gradient(circle at 18% 24%, rgba(0, 245, 255, 0.09), transparent 34%),
				radial-gradient(circle at 82% 72%, rgba(57, 255, 20, 0.07), transparent 36%),
				rgba(1, 13, 13, 0.38);
			background-size:
				72px 72px,
				72px 72px,
				100% 100%,
				100% 100%,
				100% 100%;
			box-shadow: inset 0 0 120px rgba(0, 0, 0, 0.46);
			opacity: 0;
			animation: blackboard-layer-in 260ms ease-out forwards;
		}

	.aurora-edge-layer {
		position: fixed;
		inset: 0;
		z-index: 0;
		overflow: hidden;
		pointer-events: none;
		opacity: 0;
		animation: aurora-layer-in 420ms ease-out forwards;
	}

	.aurora-edge {
		position: absolute;
		display: block;
		pointer-events: none;
		filter: blur(18px) saturate(1.35);
		mix-blend-mode: screen;
		background-size: 260% 260%;
		animation:
			aurora-edge-flow 9s ease-in-out infinite alternate,
			aurora-edge-breathe 5.6s ease-in-out infinite;
	}

	.aurora-edge--top,
	.aurora-edge--bottom {
		left: -8vw;
		right: -8vw;
		height: 92px;
		background:
			radial-gradient(circle at 18% 56%, rgba(57, 255, 20, 0.18), transparent 28%),
			radial-gradient(circle at 46% 34%, rgba(0, 245, 255, 0.22), transparent 32%),
			radial-gradient(circle at 76% 58%, rgba(255, 61, 242, 0.18), transparent 30%),
			linear-gradient(90deg, transparent, rgba(0, 245, 255, 0.18), rgba(255, 247, 0, 0.1), transparent);
	}

	.aurora-edge--top {
		top: -36px;
		mask-image: linear-gradient(to bottom, #000 0%, rgba(0, 0, 0, 0.82) 38%, transparent 100%);
	}

	.aurora-edge--bottom {
		bottom: -36px;
		animation-delay: -2.8s, -1.2s;
		mask-image: linear-gradient(to top, #000 0%, rgba(0, 0, 0, 0.82) 38%, transparent 100%);
	}

	.aurora-edge--left,
	.aurora-edge--right {
		top: -8vh;
		bottom: -8vh;
		width: 82px;
		background:
			radial-gradient(circle at 52% 20%, rgba(0, 245, 255, 0.18), transparent 29%),
			radial-gradient(circle at 36% 48%, rgba(255, 61, 139, 0.16), transparent 32%),
			radial-gradient(circle at 54% 76%, rgba(57, 255, 20, 0.14), transparent 30%),
			linear-gradient(180deg, transparent, rgba(185, 103, 255, 0.16), rgba(0, 255, 209, 0.12), transparent);
	}

	.aurora-edge--left {
		left: -34px;
		animation-delay: -1.6s, -0.6s;
		mask-image: linear-gradient(to right, #000 0%, rgba(0, 0, 0, 0.76) 42%, transparent 100%);
	}

	.aurora-edge--right {
		right: -34px;
		animation-delay: -4.1s, -2s;
		mask-image: linear-gradient(to left, #000 0%, rgba(0, 0, 0, 0.76) 42%, transparent 100%);
	}

	.status-bubble {
		position: fixed;
		right: max(24px, env(safe-area-inset-right));
		bottom: max(24px, env(safe-area-inset-bottom));
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 11px 14px;
		border: 1px solid rgba(0, 245, 255, 0.42);
		border-radius: 999px;
		background: rgba(4, 8, 18, 0.8);
		box-shadow:
			0 16px 42px rgba(0, 0, 0, 0.4),
			0 0 18px rgba(0, 245, 255, 0.28),
			inset 0 1px 0 rgba(255, 255, 255, 0.16);
		backdrop-filter: blur(14px) saturate(1.18);
		color: #eaffff;
		font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI',
			sans-serif;
		font-size: 13px;
		font-weight: 800;
		text-shadow:
			0 0 12px rgba(0, 245, 255, 0.58),
			0 1px 4px rgba(0, 0, 0, 0.95);
		pointer-events: none;
		animation: status-bubble-in 240ms ease-out forwards;
		z-index: 3;
	}

	.status-bubble--with-controls {
		bottom: max(76px, calc(env(safe-area-inset-bottom) + 76px));
	}

	.status-pulse {
		width: 8px;
		height: 8px;
		border-radius: 999px;
		background: #39ff14;
		box-shadow:
			0 0 0 4px rgba(57, 255, 20, 0.16),
			0 0 14px rgba(57, 255, 20, 0.86);
		animation: status-pulse 1.2s ease-in-out infinite;
	}

		.step-bubble {
		position: fixed;
		top: max(18px, env(safe-area-inset-top));
		left: 50%;
		transform: translateX(-50%);
		max-width: min(560px, calc(100vw - 32px));
		padding: 10px 14px 11px;
		border: 1px solid rgba(255, 255, 255, 0.28);
		border-radius: 14px;
		background: color-mix(in srgb, rgba(7, 10, 18, 0.92) 92%, transparent);
		box-shadow:
			0 18px 52px rgba(0, 0, 0, 0.44),
			0 0 22px rgba(0, 245, 255, 0.28),
			0 0 46px rgba(255, 61, 242, 0.16),
			inset 0 1px 0 rgba(255, 255, 255, 0.16);
		backdrop-filter: blur(16px) saturate(1.26);
		color: #fff8ef;
		font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI',
			sans-serif;
		line-height: 1.25;
		pointer-events: none;
		animation: step-bubble-in 260ms ease-out forwards;
			z-index: 2;
		}

		.step-bubble--blackboard {
			border-color: rgba(182, 255, 0, 0.36);
			background: rgba(2, 13, 12, 0.93);
			box-shadow:
				0 18px 52px rgba(0, 0, 0, 0.5),
				0 0 22px rgba(57, 255, 20, 0.2),
				0 0 46px rgba(0, 245, 255, 0.16),
				inset 0 1px 0 rgba(255, 255, 255, 0.14);
		}

	.step-kicker {
		display: flex;
		align-items: center;
		gap: 8px;
		color: #00f5ff;
		font-size: 11px;
		font-weight: 800;
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}

	.step-kicker span {
		border-radius: 999px;
		border: 1px solid rgba(0, 245, 255, 0.48);
		padding: 1px 6px;
		color: #b6ff00;
		font-size: 10px;
		letter-spacing: 0.06em;
	}

	.step-title {
		margin-top: 3px;
		color: #f8fbff;
		font-size: 15px;
		font-weight: 800;
		text-shadow:
			0 0 12px rgba(0, 245, 255, 0.42),
			0 1px 4px rgba(0, 0, 0, 0.95);
	}

	.step-narration {
		margin-top: 4px;
		color: rgba(224, 255, 251, 0.9);
		font-size: 13px;
		font-weight: 600;
	}

	.draw-stroke {
		stroke-dasharray: 1;
		stroke-dashoffset: 1;
		filter:
			drop-shadow(0 0 3px rgba(255, 255, 255, 0.88))
			drop-shadow(0 0 10px var(--draw-glow, rgba(0, 245, 255, 0.82)))
			drop-shadow(0 0 22px var(--draw-glow-soft, rgba(0, 245, 255, 0.46)))
			drop-shadow(0 2px 7px rgba(0, 0, 0, 0.72));
		animation: draw-stroke var(--draw-duration, 1600ms) cubic-bezier(0.45, 0, 0.15, 1) forwards;
	}

	.arrow-head {
		opacity: 0;
		transform: scale(0.62);
		filter:
			drop-shadow(0 0 4px rgba(255, 255, 255, 0.86))
			drop-shadow(0 0 10px var(--draw-glow, rgba(0, 245, 255, 0.82)))
			drop-shadow(0 0 22px var(--draw-glow-soft, rgba(0, 245, 255, 0.46)))
			drop-shadow(0 2px 7px rgba(0, 0, 0, 0.72));
		animation: arrow-head-in 320ms cubic-bezier(0.2, 0, 0.2, 1) var(--arrow-delay, 1150ms) forwards;
	}

	.draw-label {
		opacity: 0;
		font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI',
			sans-serif;
		stroke: rgba(2, 6, 23, 0.96);
		stroke-width: 4px;
		stroke-linejoin: round;
		paint-order: stroke fill;
		filter:
			drop-shadow(0 0 3px rgba(255, 255, 255, 0.9))
			drop-shadow(0 0 9px var(--draw-glow, rgba(0, 245, 255, 0.82)))
			drop-shadow(0 0 20px var(--draw-glow-soft, rgba(0, 245, 255, 0.46)))
			drop-shadow(0 2px 5px rgba(0, 0, 0, 0.82));
		text-shadow:
			0 1px 5px rgba(0, 0, 0, 0.9),
			0 0 2px rgba(0, 0, 0, 1);
		animation: label-in 360ms ease-out var(--label-delay, 980ms) forwards;
	}

	.handwriting-primitive {
		font-family: 'Snell Roundhand', 'Apple Chancery', 'Bradley Hand', 'Segoe Script', cursive;
		font-weight: 600;
		stroke-width: 2.4px;
		letter-spacing: 0.02em;
		filter:
			drop-shadow(0 0 4px rgba(255, 255, 255, 0.84))
			drop-shadow(0 0 12px var(--draw-glow, rgba(0, 245, 255, 0.82)))
			drop-shadow(0 0 26px var(--draw-glow-soft, rgba(0, 245, 255, 0.46)))
			drop-shadow(0 2px 6px rgba(0, 0, 0, 0.82));
	}

	.cursive-primitive {
		font-family: 'Tutor Playwrite US Trad', 'Brush Script MT', 'Brush Script', cursive;
		font-weight: 400;
		letter-spacing: 0;
	}

	.deeper-control {
		/* On desktop the window ignores cursor events and the Tauri host
		   hit-tests this geometry instead; enabling pointer events here is what
		   makes the same control work in a plain browser, where no host exists. */
		pointer-events: auto;
		cursor: pointer;
	}

	.replay-control {
		opacity: 0;
		filter:
			drop-shadow(0 10px 22px rgba(0, 0, 0, 0.52))
			drop-shadow(0 0 18px rgba(0, 245, 255, 0.78))
			drop-shadow(0 0 34px rgba(0, 245, 255, 0.34));
		animation: replay-control-in 240ms ease-out forwards;
	}

	.replay-control__panel {
		fill: rgba(191, 255, 255, 0.97);
		stroke: rgba(0, 245, 255, 0.96);
		stroke-width: 1.5;
	}

	.dismiss-control__panel {
		fill: rgba(255, 205, 231, 0.98);
		stroke: rgba(255, 61, 139, 0.98);
	}

	.keep-showing-control__panel {
		fill: rgba(216, 255, 231, 0.98);
		stroke: rgba(25, 255, 155, 0.98);
	}

	.replay-control__icon {
		stroke: #032b34;
		stroke-width: 2.2;
		stroke-linecap: round;
		stroke-linejoin: round;
		filter: drop-shadow(0 0 5px rgba(255, 255, 255, 0.95));
	}

	.dismiss-control__icon {
		stroke: #3b061b;
		filter: drop-shadow(0 0 5px rgba(255, 255, 255, 0.95));
	}

	.keep-showing-control__icon {
		stroke: #043119;
		filter: drop-shadow(0 0 5px rgba(255, 255, 255, 0.95));
	}

	.replay-control__label {
		fill: #03141a;
		font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI',
			sans-serif;
		font-size: 13px;
		font-weight: 850;
		letter-spacing: 0;
		stroke: rgba(255, 255, 255, 0.78);
		stroke-width: 1.8px;
		paint-order: stroke fill;
	}

	@keyframes draw-stroke {
		from {
			stroke-dashoffset: 1;
		}
		to {
			stroke-dashoffset: 0;
		}
	}

	@keyframes arrow-head-in {
		from {
			opacity: 0;
			transform: scale(0.62);
		}
		to {
			opacity: 1;
			transform: scale(1);
		}
	}

	@keyframes label-in {
		from {
			opacity: 0;
			transform: translateY(3px);
		}
		to {
			opacity: 1;
			transform: translateY(0);
		}
	}

	@keyframes dim-layer-in {
		from {
			opacity: 0;
		}
		to {
			opacity: 1;
		}
	}

	@keyframes aurora-layer-in {
		from {
			opacity: 0;
		}
		to {
			opacity: 0.9;
		}
	}

	@keyframes blackboard-layer-in {
		from {
			opacity: 0;
		}
		to {
			opacity: 1;
		}
	}

	@keyframes aurora-edge-flow {
		from {
			background-position:
				0% 46%,
				18% 26%,
				64% 48%,
				0% 50%;
			transform: translate3d(-1.5%, 0, 0) scale(1);
		}
		to {
			background-position:
				96% 56%,
				76% 34%,
				24% 62%,
				100% 50%;
			transform: translate3d(1.5%, 0, 0) scale(1.03);
		}
	}

	@keyframes aurora-edge-breathe {
		0%,
		100% {
			opacity: 0.62;
		}
		50% {
			opacity: 0.86;
		}
	}

	@keyframes status-bubble-in {
		from {
			opacity: 0;
			transform: translateY(6px);
		}
		to {
			opacity: 1;
			transform: translateY(0);
		}
	}

	@keyframes status-pulse {
		0%,
		100% {
			opacity: 0.78;
			transform: scale(0.92);
		}
		50% {
			opacity: 1;
			transform: scale(1.08);
		}
	}

	@keyframes replay-control-in {
		from {
			opacity: 0;
		}
		to {
			opacity: 1;
		}
	}

	@keyframes step-bubble-in {
		from {
			opacity: 0;
			transform: translate(-50%, -6px);
		}
		to {
			opacity: 1;
			transform: translate(-50%, 0);
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.draw-stroke,
		.arrow-head,
		.draw-label,
		.step-bubble,
		.status-bubble,
		.status-pulse,
		.aurora-edge-layer,
		.aurora-edge,
		.replay-control {
			animation-duration: 1ms;
			animation-delay: 0ms;
		}
	}
</style>
