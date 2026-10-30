package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.voice.LiveVoiceEngine
import ai.magicbeans.magdroid.voice.PrimaryAgentWakeIdentityStore
import ai.magicbeans.magdroid.voice.primaryAgentDisplayName
import ai.magicbeans.magdroid.voice.RealtimeVoiceCaption
import ai.magicbeans.magdroid.voice.RealtimeVoiceState
import android.os.SystemClock
import androidx.compose.animation.Crossfade
import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.wrapContentHeight
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Mic
import androidx.compose.material.icons.filled.MicOff
import androidx.compose.material.icons.filled.PanTool
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.Hearing
import androidx.compose.material.icons.outlined.Keyboard
import androidx.compose.material.icons.outlined.Mic
import androidx.compose.material.icons.outlined.MicOff
import androidx.compose.material.icons.outlined.PanTool
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.minimumInteractiveComponentSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.rotate
import androidx.compose.ui.graphics.compositeOver
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import kotlin.math.PI
import kotlin.math.cos
import kotlin.math.min
import kotlin.math.sin

/**
 * The live-call card, the Android mirror of iOS `VoiceCallPanel`: an aurora
 * orb that breathes and swells with the microphone, the engine title with an
 * mm:ss timer, a status line that names the mic mode, a fixed rolling caption
 * viewport, the two-segment mic-mode control and one main mic button whose
 * shape follows the mode. A small red X in the header ends the call.
 *
 * [micLevel] is read only inside draw lambdas, so the ~16 Hz level updates
 * redraw the orb and ring without recomposing the card. [state] is expected
 * without its level (`withoutMicLevel()`).
 *
 * The full call surface can collapse into the text composer without ending
 * the call. Dictation remains unavailable while realtime owns the microphone.
 */
@Composable
internal fun LiveVoicePanel(
    voiceQueue: @Composable () -> Unit = {},
    state: RealtimeVoiceState,
    micLevel: () -> Float = { state.micLevel },
    playbackLevel: () -> Float = { state.playbackLevel },
    onMute: () -> Unit,
    onHoldStart: () -> Unit,
    onHoldEnd: () -> Unit,
    onSetPushToTalk: (Boolean) -> Unit,
    onEnd: () -> Unit,
    onTypeMessage: () -> Unit,
) {
    val context = LocalContext.current
    remember(context) { PrimaryAgentWakeIdentityStore.initialize(context) }
    val primaryAgent by PrimaryAgentWakeIdentityStore.identity.collectAsStateWithLifecycle()
    val assistantName = primaryAgentDisplayName(primaryAgent)
    val ready = state.phase == RealtimeVoiceState.Phase.Ready
    val animate = rememberAnimatorDurationScale() > 0f
    Surface(
        color = composerSurfaceColor(),
        shape = Lively.composerShape,
        border = BorderStroke(1.dp, BorderSoft),
        modifier = Modifier.fillMaxWidth().padding(horizontal = 10.dp, vertical = 7.dp),
    ) {
        Column(Modifier.fillMaxWidth()) {
            voiceQueue()
            Column(
                Modifier.fillMaxWidth().padding(12.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    LiveCallOrb(
                        palette = liveOrbPalette(state),
                        glyph = if (state.engine == LiveVoiceEngine.HandsFree) Icons.Outlined.Hearing else Icons.Filled.Mic,
                        micLevel = micLevel,
                        animate = animate,
                    )
                    Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            Text(
                                "${state.engine.label} voice",
                                color = Ink, fontSize = 14.sp, fontWeight = FontWeight.Bold,
                                maxLines = 1, overflow = TextOverflow.Ellipsis,
                                modifier = Modifier.weight(1f, fill = false),
                            )
                            LiveCallTimer(state.connectedAtMillis)
                        }
                        Text(
                            liveCallStatusText(state),
                            color = Secondary, fontSize = 12.sp, maxLines = 1, overflow = TextOverflow.Ellipsis,
                        )
                    }
                    IconButton(onClick = onTypeMessage, modifier = Modifier.size(34.dp)) {
                        Icon(Icons.Outlined.Keyboard, "Type a message", tint = Secondary, modifier = Modifier.size(19.dp))
                    }
                    LiveCallEndButton(onEnd)
                }

                state.error?.let { LiveCallErrorRow(it) }

                if (state.captions.isNotEmpty()) {
                    LiveCaptions(state.captions, assistantName)
                } else {
                    Text(
                        liveCallHintText(state),
                        color = Secondary, fontSize = 12.sp, textAlign = TextAlign.Center,
                        modifier = Modifier.fillMaxWidth().height(LIVE_CAPTION_VIEWPORT_DP.dp)
                            .wrapContentHeight(Alignment.CenterVertically),
                    )
                }

                Column(
                    Modifier.fillMaxWidth(),
                    horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    // Changeable while the call runs: a room that stopped being
                    // quiet must not mean ending the call and going to Settings.
                    MicModeControl(
                        pushToTalk = state.pushToTalk,
                        holdAvailable = state.holdToTalkAvailable,
                        enabled = ready,
                        onSetPushToTalk = onSetPushToTalk,
                    )
                    VoiceActivityWaveform(
                        active = ready, muted = state.muted, animate = animate,
                        micLevel = micLevel, playbackLevel = playbackLevel,
                    )
                    if (state.pushToTalk) {
                        HoldToTalkButton(
                            held = state.pushToTalkHeld,
                            enabled = ready,
                            onHoldStart = onHoldStart,
                            onHoldEnd = onHoldEnd,
                        )
                    } else {
                        OpenMicButton(muted = state.muted, enabled = ready, micLevel = micLevel, onMute = onMute)
                    }
                }
            }
        }
    }
}

/** The caption viewport's fixed height (iOS `VoiceCaptionViewportPolicy.height`). */
internal const val LIVE_CAPTION_VIEWPORT_DP = 72

/**
 * The status line names the mic mode, so the one control that changes it never
 * has to: "Open mic · Listening", "Hold to talk · Ready", and so on.
 */
internal fun liveCallStatusText(state: RealtimeVoiceState): String = when (state.phase) {
    RealtimeVoiceState.Phase.Connecting -> "Preparing…"
    RealtimeVoiceState.Phase.Reconnecting -> "Reconnecting…"
    RealtimeVoiceState.Phase.Ending -> "Ending call…"
    RealtimeVoiceState.Phase.Failed -> "Voice error"
    RealtimeVoiceState.Phase.Idle -> "Call ended"
    RealtimeVoiceState.Phase.Ready -> when {
        state.pushToTalk && state.pushToTalkHeld -> "Hold to talk · Sending"
        // Between "let me check…" and the answer the line is silent; say so
        // rather than inviting the next question.
        state.assistantWorking -> "Working…"
        state.pushToTalk -> "Hold to talk · Ready"
        state.muted -> "Open mic · Muted"
        else -> "Open mic · Listening"
    }
}

/** What the empty caption viewport says before anyone has spoken. */
internal fun liveCallHintText(state: RealtimeVoiceState): String = when (state.phase) {
    RealtimeVoiceState.Phase.Connecting -> "Setting up voice…"
    RealtimeVoiceState.Phase.Reconnecting -> "Restoring the voice connection…"
    else -> if (state.pushToTalk) "Hold the button and talk." else "Start talking. Captions will appear here."
}

/** The composer strip's label while the call is collapsed behind typed chat. */
internal fun liveCallStripLabel(state: RealtimeVoiceState): String = when (state.phase) {
    RealtimeVoiceState.Phase.Ready -> when {
        state.pushToTalk -> "Live · Hold to talk"
        state.muted -> "Live · Muted"
        else -> "Live · Open mic"
    }
    RealtimeVoiceState.Phase.Connecting -> "Connecting…"
    RealtimeVoiceState.Phase.Reconnecting -> "Reconnecting…"
    RealtimeVoiceState.Phase.Ending -> "Ending call…"
    else -> "Call ended"
}

/** `m:ss` from the call's first ready, on the [SystemClock.elapsedRealtime] clock. */
internal fun liveCallElapsedLabel(connectedAtMillis: Long?, nowMillis: Long): String {
    if (connectedAtMillis == null) return "0:00"
    val total = ((nowMillis - connectedAtMillis) / 1_000L).coerceAtLeast(0L)
    return "${total / 60}:${(total % 60).toString().padStart(2, '0')}"
}

/** Ticks once a second on its own, so only this text recomposes. */
@Composable
private fun LiveCallTimer(connectedAtMillis: Long?) {
    var now by remember { mutableLongStateOf(SystemClock.elapsedRealtime()) }
    LaunchedEffect(connectedAtMillis) {
        if (connectedAtMillis == null) return@LaunchedEffect
        while (true) {
            now = SystemClock.elapsedRealtime()
            delay((1_000L - (now - connectedAtMillis).mod(1_000L)).coerceIn(50L, 1_000L))
        }
    }
    Text(
        liveCallElapsedLabel(connectedAtMillis, now),
        color = Secondary, fontSize = 11.sp, fontWeight = FontWeight.Medium, fontFamily = LocalMagicanFontFamilies.current.mono,
        maxLines = 1,
    )
}

@Composable
private fun LiveCallEndButton(onEnd: () -> Unit) {
    val shape = RoundedCornerShape(8.dp)
    Box(
        Modifier
            .minimumInteractiveComponentSize()
            .clickable(role = Role.Button, onClickLabel = "End voice call", onClick = onEnd)
            .semantics { contentDescription = "End voice call" },
        contentAlignment = Alignment.Center,
    ) {
        Box(Modifier.size(34.dp).clip(shape).background(Danger, shape), contentAlignment = Alignment.Center) {
            Icon(Icons.Filled.Close, null, tint = chatContrastingTextColor(Danger), modifier = Modifier.size(18.dp))
        }
    }
}

@Composable
private fun LiveCallErrorRow(message: String) {
    val shape = RoundedCornerShape(10.dp)
    Row(
        Modifier.fillMaxWidth().clip(shape).background(Danger.copy(alpha = 0.12f), shape)
            .padding(horizontal = 10.dp, vertical = 8.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(Icons.Outlined.ErrorOutline, null, tint = Danger, modifier = Modifier.size(13.dp))
        Text(message, color = Danger, fontSize = 12.sp, lineHeight = 16.sp, maxLines = 2, overflow = TextOverflow.Ellipsis)
    }
}

/**
 * The recent captions in a fixed viewport that follows the newest line. The
 * durable transcript is the chat behind the panel; this is the glance.
 */
@Composable
private fun LiveCaptions(captions: List<RealtimeVoiceCaption>, assistantName: String) {
    val scroll = rememberScrollState()
    LaunchedEffect(scroll) {
        snapshotFlow { scroll.maxValue }.collectLatest { scroll.animateScrollTo(it) }
    }
    Column(
        Modifier.fillMaxWidth().height(LIVE_CAPTION_VIEWPORT_DP.dp).verticalScroll(scroll),
        verticalArrangement = Arrangement.spacedBy(5.dp),
    ) {
        captions.forEach { caption ->
            key(caption.id) {
                Text(
                    buildAnnotatedString {
                        withStyle(SpanStyle(
                            color = if (caption.fromUser) Coral else Secondary,
                            fontSize = 10.sp,
                            fontWeight = FontWeight.Bold,
                        )) { append(if (caption.fromUser) "You" else assistantName) }
                        append("  ")
                        append(caption.text)
                    },
                    color = Ink, fontSize = 13.sp, lineHeight = 17.sp,
                    modifier = Modifier.fillMaxWidth().alpha(if (caption.final) 1f else 0.72f),
                )
            }
        }
    }
}

/**
 * Two segments, same vocabulary on every platform: "Open mic" and "Hold to
 * talk". The selected one is the accent fill; tapping the other switches.
 */
@Composable
private fun MicModeControl(
    pushToTalk: Boolean,
    holdAvailable: Boolean,
    enabled: Boolean,
    onSetPushToTalk: (Boolean) -> Unit,
) {
    val shape = RoundedCornerShape(10.dp)
    Row(
        Modifier.clip(shape).border(1.dp, BorderSoft, shape).padding(3.dp).selectableGroup(),
        horizontalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        MicModeSegment(
            label = "Open mic",
            icon = Icons.Outlined.Mic,
            selected = !pushToTalk,
            enabled = enabled,
            onSelect = { onSetPushToTalk(false) },
        )
        MicModeSegment(
            label = "Hold to talk",
            icon = Icons.Outlined.PanTool,
            selected = pushToTalk,
            // A translation profile streams continuously; it refuses hold-to-talk.
            enabled = enabled && (holdAvailable || pushToTalk),
            unavailable = !holdAvailable && !pushToTalk,
            onSelect = { onSetPushToTalk(true) },
        )
    }
}

@Composable
private fun MicModeSegment(
    label: String,
    icon: ImageVector,
    selected: Boolean,
    enabled: Boolean,
    onSelect: () -> Unit,
    unavailable: Boolean = false,
) {
    val shape = RoundedCornerShape(7.dp)
    val foreground = if (selected) OnAccent else Secondary
    Row(
        Modifier
            .height(36.dp)
            .clip(shape)
            .background(if (selected) AccentFill else Color.Transparent, shape)
            .selectable(selected = selected, enabled = enabled, role = Role.Tab, onClick = { if (!selected) onSelect() })
            .semantics { if (unavailable) stateDescription = "Not available for this voice" }
            .alpha(if (enabled || selected) 1f else 0.5f)
            .padding(horizontal = 12.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(icon, null, tint = foreground, modifier = Modifier.size(15.dp))
        Text(
            label, color = foreground, fontSize = 12.sp,
            fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Medium, maxLines = 1,
        )
    }
}

/** Open mic: one round button, a ring that moves with the voice, and what a tap does. */
@Composable
private fun OpenMicButton(muted: Boolean, enabled: Boolean, micLevel: () -> Float, onMute: () -> Unit) {
    val palette = activePalette
    val fill = if (muted) palette.soft else palette.accentFill
    val content = if (muted) palette.text else palette.onAccent
    Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(2.dp)) {
        Box(
            Modifier.size(76.dp).drawBehind {
                if (muted) return@drawBehind
                // Read here, not in composition: a level change redraws only.
                val level = micLevel().coerceIn(0f, 1f)
                drawCircle(
                    color = palette.accent.copy(alpha = 0.18f + 0.5f * level),
                    radius = 31.dp.toPx() + level * 7.dp.toPx(),
                    style = Stroke(width = 2.dp.toPx()),
                )
            },
            contentAlignment = Alignment.Center,
        ) {
            Box(
                Modifier
                    .size(56.dp)
                    .clip(CircleShape)
                    // Paint disabled colours directly. A transient alpha layer
                    // on the clipped button can flash white on first GPU draw.
                    .background(if (enabled) fill else fill.copy(alpha = 0.5f).compositeOver(palette.soft), CircleShape)
                    .border(1.dp, if (muted) BorderSoft else Color.Transparent, CircleShape)
                    .clickable(
                        enabled = enabled,
                        role = Role.Button,
                        onClickLabel = if (muted) "Unmute microphone" else "Mute microphone",
                        onClick = onMute,
                    )
                    .semantics { contentDescription = if (muted) "Unmute microphone" else "Mute microphone" },
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    if (muted) Icons.Filled.MicOff else Icons.Filled.Mic,
                    null,
                    tint = content.copy(alpha = if (enabled) 1f else 0.5f),
                    modifier = Modifier.size(24.dp),
                )
            }
        }
        Text(
            if (muted) "Muted · tap to unmute" else "Listening · tap to mute",
            color = Secondary, fontSize = 11.sp,
        )
    }
}

@Composable
internal fun VoiceActivityWaveform(
    active: Boolean, muted: Boolean, animate: Boolean,
    micLevel: () -> Float, playbackLevel: () -> Float,
) {
    val samples = remember { mutableStateListOf<Float>().apply { repeat(25) { add(0f) } } }
    val accent = Coral
    LaunchedEffect(active, muted, animate) {
        while (true) {
            val level = if (active) maxOf(playbackLevel(), if (muted) 0f else micLevel()).coerceIn(0f, 1f) else 0f
            if (animate) { samples.removeAt(0); samples.add(level) }
            else { samples.indices.forEach { samples[it] = level } }
            delay(50)
        }
    }
    Canvas(Modifier.widthIn(min = 148.dp, max = 148.dp).height(24.dp).clearAndSetSemantics {}) {
        val gradient = Brush.verticalGradient(listOf(accent.copy(alpha = 0.4f), accent, accent.copy(alpha = 0.4f)))
        samples.forEachIndexed { index, value ->
            val height = 2.dp.toPx() + value * (size.height - 2.dp.toPx())
            drawRoundRect(
                brush = gradient, topLeft = Offset(index * size.width / 25, (size.height - height) / 2),
                size = androidx.compose.ui.geometry.Size(3.dp.toPx(), height),
                cornerRadius = androidx.compose.ui.geometry.CornerRadius(1.5.dp.toPx()),
            )
        }
    }
}

/** Hold to talk: a wide capsule that fills with the accent while held. */
@Composable
private fun HoldToTalkButton(held: Boolean, enabled: Boolean, onHoldStart: () -> Unit, onHoldEnd: () -> Unit) {
    val shape = RoundedCornerShape(percent = 50)
    val foreground = if (held) OnAccent else Ink
    Row(
        Modifier
            .widthIn(min = 220.dp)
            .height(52.dp)
            .clip(shape)
            .background(if (held) AccentFill else Soft, shape)
            .border(1.dp, if (held) Color.Transparent else BorderSoft, shape)
            .alpha(if (enabled) 1f else 0.5f)
            .semantics {
                role = Role.Button
                contentDescription = if (held) "Release to send" else "Hold to talk"
            }
            .pointerInput(enabled) {
                if (!enabled) return@pointerInput
                detectTapGestures(onPress = {
                    onHoldStart()
                    tryAwaitRelease()
                    onHoldEnd()
                })
            }
            .padding(horizontal = 22.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(if (held) Icons.Filled.PanTool else Icons.Outlined.PanTool, null, tint = foreground, modifier = Modifier.size(19.dp))
        Text(
            if (held) "Release to send" else "Hold to talk",
            color = foreground, fontSize = 14.sp, fontWeight = FontWeight.SemiBold,
        )
    }
}

/**
 * The aurora presets the iOS beacon draws (`AuroraPalette` in
 * `magios/Shared/AmbientOrbAppearance.swift`), by value: brand colours, not
 * theme tokens, so the orb reads the same on every theme and platform.
 */
internal enum class LiveOrbPalette(val stops: List<Color>, val halo: Color, val seed: Float) {
    VioletSurge(
        listOf(Color(0.62f, 0.35f, 1.00f), Color(1.00f, 0.42f, 0.78f), Color(0.62f, 0.35f, 1.00f)),
        Color(0.78f, 0.45f, 1.00f),
        2.1f,
    ),
    /** Its halo is clear: a dead call's glow goes out rather than dimming. */
    Graphite(listOf(Color(0.45f, 0.45f, 0.45f), Color(0.28f, 0.28f, 0.28f)), Color.Transparent, 0f),
    ArmedEmber(
        listOf(Color(0.36f, 0.30f, 0.52f), Color(0.20f, 0.17f, 0.30f)),
        Color(0.55f, 0.42f, 0.95f),
        0.9f,
    ),
    CalmAurora(
        listOf(Color(0.55f, 0.38f, 0.98f), Color(0.36f, 0.48f, 1.00f)),
        Color(0.60f, 0.50f, 1.00f),
        3.3f,
    ),
}

/**
 * Coarse, as iOS: connecting surges, a dead call goes graphite, a closed mic
 * (muted, or hold-to-talk not held) rests in ember, a live mic is aurora.
 */
internal fun liveOrbPalette(state: RealtimeVoiceState): LiveOrbPalette = when (state.phase) {
    RealtimeVoiceState.Phase.Connecting, RealtimeVoiceState.Phase.Reconnecting -> LiveOrbPalette.VioletSurge
    RealtimeVoiceState.Phase.Ready ->
        if (state.muted || (state.pushToTalk && !state.pushToTalkHeld)) LiveOrbPalette.ArmedEmber
        else LiveOrbPalette.CalmAurora
    else -> LiveOrbPalette.Graphite
}

/** 0.6 rad/s, as iOS. Five turns, so the 1.4x term also lands on a whole turn at the wrap. */
private val ORB_MORPH_SPAN = (10 * PI).toFloat()
private const val ORB_MORPH_MILLIS = 52_360

/**
 * The 44 dp call orb: a breathing halo under a morphing aurora blob that
 * swells and deepens its lobes with the microphone, and the engine glyph on
 * top. Animation values and the level are read in the draw phase only.
 * Animations off (animator scale 0) freezes the drift and the breath; the
 * level still moves the blob, because lobe depth is information.
 */
@Composable
private fun LiveCallOrb(palette: LiveOrbPalette, glyph: ImageVector, micLevel: () -> Float, animate: Boolean) {
    val transition = rememberInfiniteTransition(label = "live-orb")
    val breath = transition.animateFloat(
        initialValue = 0f, targetValue = 1f,
        animationSpec = infiniteRepeatable(tween(900, easing = FastOutSlowInEasing), RepeatMode.Reverse),
        label = "live-orb-breath",
    )
    val morph = transition.animateFloat(
        initialValue = 0f, targetValue = ORB_MORPH_SPAN,
        animationSpec = infiniteRepeatable(tween(ORB_MORPH_MILLIS, easing = LinearEasing), RepeatMode.Restart),
        label = "live-orb-morph",
    )
    val path = remember { Path() }
    Box(Modifier.size(44.dp).clearAndSetSemantics {}, contentAlignment = Alignment.Center) {
        Crossfade(targetState = palette, animationSpec = tween(if (animate) 350 else 0), label = "live-orb-palette") { current ->
            Canvas(Modifier.size(44.dp)) {
                val level = micLevel().coerceIn(0f, 1f)
                val b = if (animate) breath.value else 0.5f
                if (current.halo.alpha > 0f) {
                    val haloAlpha = if (animate) 0.4f + 0.45f * b else 0.5f
                    val haloRadius = 24.dp.toPx() * (if (animate) 0.9f + 0.25f * b else 1f)
                    drawCircle(
                        brush = Brush.radialGradient(
                            listOf(current.halo.copy(alpha = haloAlpha), current.halo.copy(alpha = 0f)),
                            center = center,
                            radius = haloRadius,
                        ),
                        radius = haloRadius,
                    )
                }
                val extent = 17.dp.toPx() * (1f + level * 0.16f)
                auroraBlobPath(
                    path, center, extent,
                    seed = current.seed,
                    morph = if (animate) morph.value else 0f,
                    amplitude = 0.10f + level * 0.15f,
                )
                rotate(220f, center) {
                    drawPath(
                        path,
                        Brush.sweepGradient(current.stops + current.stops.first(), center),
                        alpha = 0.82f + min(level, 0.5f) * 0.36f,
                    )
                }
            }
        }
        // White holds over every preset: the aurora body is never light.
        Icon(glyph, null, tint = Color.White, modifier = Modifier.size(14.dp))
    }
}

/**
 * iOS `AuroraBlobShape`: a circle whose radius undulates with a 2θ/3θ/5θ sine
 * mix at weights summing to 1, so [amplitude] is the worst-case excursion and
 * the base radius shrinks by (1 + amplitude) to keep every lobe inside
 * [halfExtent]. Rewrites [path] in place.
 */
internal fun auroraBlobPath(
    path: Path,
    center: Offset,
    halfExtent: Float,
    seed: Float,
    morph: Float,
    amplitude: Float,
    samples: Int = 96,
) {
    path.reset()
    val amp = amplitude.coerceIn(0f, 1f)
    val base = halfExtent / (1f + amp)
    for (index in 0 until samples) {
        val theta = index.toFloat() / samples * 2f * PI.toFloat()
        val wave = 0.55f * sin(2f * theta + seed) +
            0.30f * sin(3f * theta + seed * 1.7f + morph) +
            0.15f * sin(5f * theta - seed * 2.3f + morph * 1.4f)
        val radius = base * (1f + amp * wave.coerceIn(-1f, 1f))
        val x = center.x + cos(theta) * radius
        val y = center.y + sin(theta) * radius
        if (index == 0) path.moveTo(x, y) else path.lineTo(x, y)
    }
    path.close()
}

/** Keep call status and immediate mute/end controls reachable beside typed chat. */
@Composable
internal fun LiveCallComposerStrip(state: RealtimeVoiceState, onOpen: () -> Unit, onMute: () -> Unit, onEnd: () -> Unit) {
    Column {
        Row(Modifier.fillMaxWidth().height(30.dp).padding(start = 14.dp, end = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(liveCallStripLabel(state),
                color = Secondary, fontSize = 12.sp, maxLines = 1,
                modifier = Modifier.weight(1f).fillMaxHeight().wrapContentHeight(Alignment.CenterVertically)
                    .clickable(role = Role.Button, onClickLabel = "Open voice call controls", onClick = onOpen))
            IconButton(onClick = if (state.pushToTalk) onOpen else onMute, modifier = Modifier.size(36.dp)) {
                Icon(
                    when {
                        state.pushToTalk -> Icons.Outlined.PanTool
                        state.muted -> Icons.Outlined.MicOff
                        else -> Icons.Outlined.Mic
                    },
                    if (state.pushToTalk) "Open voice call controls" else if (state.muted) "Unmute microphone" else "Mute microphone",
                    tint = Secondary, modifier = Modifier.size(17.dp))
            }
            IconButton(onClick = onEnd, modifier = Modifier.size(36.dp)) {
                Icon(Icons.Filled.Close, "End voice call", tint = Danger, modifier = Modifier.size(17.dp))
            }
        }
        HorizontalDivider(thickness = 0.5.dp, color = Muted.copy(alpha = 0.2f))
    }
}
