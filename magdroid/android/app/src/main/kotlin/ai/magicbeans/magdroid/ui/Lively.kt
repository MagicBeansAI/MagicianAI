package ai.magicbeans.magdroid.ui

import android.provider.Settings
import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.scale
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.LocalIndication

/**
 * Shared style tokens for the lively chat surfaces (task card, transcript,
 * composer, step lists), ported from iOS `LivelyTokens.swift`. Every theme
 * reads the same radii; colours come from the active [Palette].
 */
object Lively {
    val chipRadius: Dp = 6.dp
    val cardRadius: Dp = 14.dp
    /** The composer card and the docks above it: 10 on every platform. */
    val composerRadius: Dp = 10.dp
    val bubbleRadius: Dp = 16.dp

    /** Minimum hit area for anything tappable (Material guidance). */
    val tapMin: Dp = 48.dp

    /**
     * Step lists (chat Steps panel, task card, task detail Run / Plan): the
     * step title, its secondary lines (target, timing, usage, notes) and each
     * row's vertical padding, as iOS `Lively.stepTitleSize` etc.
     */
    const val STEP_TITLE_SP = 12f
    const val STEP_META_SP = 10.5f
    val stepRowPadding: Dp = 6.dp

    val chipShape = RoundedCornerShape(chipRadius)
    val cardShape = RoundedCornerShape(cardRadius)
    val composerShape = RoundedCornerShape(composerRadius)
    val bubbleShape = RoundedCornerShape(bubbleRadius)
}

/** A task or step state, in the web card's status language. */
enum class StatusTone { Running, Waiting, Done, Failed, Partial, NotStarted, Cancelled }

/**
 * The dot colour for a tone: running is the theme's running blue (web
 * `--status-running`, never the accent), waiting and partial amber, done
 * green, failed red, not started the secondary accent, cancelled muted.
 */
fun statusToneColor(tone: StatusTone, palette: Palette = activePalette): Color = when (tone) {
    StatusTone.Running -> palette.running
    StatusTone.Waiting, StatusTone.Partial -> palette.warning
    StatusTone.Done -> palette.success
    StatusTone.Failed -> palette.danger
    StatusTone.NotStarted -> palette.secondaryAccentColor
    StatusTone.Cancelled -> palette.secondaryText
}

/**
 * Only live tones move (running breathes, waiting pulses), and nothing moves
 * when the device's animator duration scale is 0 (animations off).
 */
fun statusDotAnimates(tone: StatusTone, animatorDurationScale: Float): Boolean =
    animatorDurationScale > 0f && (tone == StatusTone.Running || tone == StatusTone.Waiting)

/** The device's `ANIMATOR_DURATION_SCALE`; 1 when it cannot be read. */
@Composable
fun rememberAnimatorDurationScale(): Float {
    val context = LocalContext.current
    return remember(context) {
        runCatching {
            Settings.Global.getFloat(context.contentResolver, Settings.Global.ANIMATOR_DURATION_SCALE, 1f)
        }.getOrDefault(1f)
    }
}

/** A task/step status dot. Stays a circle; everything else is a rounded square. */
@Composable
fun StatusDot(tone: StatusTone, modifier: Modifier = Modifier, size: Dp = 8.dp) {
    val color = statusToneColor(tone)
    val moving = statusDotAnimates(tone, rememberAnimatorDurationScale())
    if (!moving) {
        Box(modifier.size(size).background(color, CircleShape))
        return
    }
    val transition = rememberInfiniteTransition(label = "status-dot")
    val phase by transition.animateFloat(
        initialValue = 0f,
        targetValue = 1f,
        animationSpec = infiniteRepeatable(
            tween(if (tone == StatusTone.Running) 1600 else 1200, easing = FastOutSlowInEasing),
            RepeatMode.Reverse,
        ),
        label = "status-dot-phase",
    )
    if (tone == StatusTone.Running) {
        // Breathing: a soft glow behind a dot that grows by a quarter.
        Box(
            modifier
                .size(size)
                .drawBehind {
                    drawCircle(
                        color = color.copy(alpha = 0.35f * phase),
                        radius = this.size.minDimension / 2f * (1f + phase),
                        center = Offset(this.size.width / 2f, this.size.height / 2f),
                    )
                }
                .scale(1f + 0.25f * phase)
                .background(color, CircleShape),
        )
    } else {
        // Waiting: an opacity pulse.
        Box(
            modifier
                .size(size)
                .graphicsLayer { alpha = 1f - 0.55f * phase }
                .background(color, CircleShape),
        )
    }
}

/**
 * A compact chip button: rounded square (6 dp), 12 sp semibold label, control
 * fill (or accent when [prominent]), and a 48 dp hit area around a smaller chip.
 */
@Composable
fun LivelyChip(
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    prominent: Boolean = false,
    enabled: Boolean = true,
    content: @Composable () -> Unit,
) {
    val interaction = remember { MutableInteractionSource() }
    val pressed by interaction.collectIsPressedAsState()
    Box(
        modifier
            .defaultMinSize(minWidth = Lively.tapMin, minHeight = Lively.tapMin)
            .clickable(
                interactionSource = interaction,
                indication = LocalIndication.current,
                enabled = enabled,
                role = Role.Button,
                onClick = onClick,
            ),
        contentAlignment = Alignment.Center,
    ) {
        Surface(
            color = when {
                !prominent -> Control
                pressed -> AccentFillPressed
                else -> AccentFill
            },
            contentColor = if (prominent) OnAccent else Ink,
            shape = Lively.chipShape,
            border = if (prominent) null else BorderStroke(1.dp, ControlBorder),
            modifier = Modifier.graphicsLayer { alpha = if (enabled) 1f else 0.45f },
        ) {
            Box(Modifier.padding(horizontal = 10.dp, vertical = 5.dp)) { content() }
        }
    }
}

/** The label style a [LivelyChip] uses. */
@Composable
fun livelyChipTextStyle(prominent: Boolean = false): TextStyle = TextStyle(
    fontSize = 12.sp,
    fontWeight = FontWeight.SemiBold,
    fontFamily = LocalMagicanFontFamilies.current.body,
    color = if (prominent) OnAccent else Ink,
)

/** The wash stops for a palette, top-leading to bottom-trailing. */
fun washBrush(palette: Palette = activePalette): Brush =
    Brush.linearGradient(palette.washStops)

/** Paints the active theme's wash behind the content, full bleed. */
fun Modifier.themeWash(palette: Palette = activePalette): Modifier =
    this.background(washBrush(palette))

/** A step's title line (~12 sp). */
@Composable
fun livelyStepTitleStyle(color: Color = Ink): TextStyle = TextStyle(
    fontSize = Lively.STEP_TITLE_SP.sp,
    lineHeight = 16.sp,
    fontFamily = LocalMagicanFontFamilies.current.body,
    color = color,
)

/** A step's secondary line: target, timing, tokens / cost, notes (~10.5 sp, muted). */
@Composable
fun livelyStepMetaStyle(color: Color = Secondary): TextStyle = TextStyle(
    fontSize = Lively.STEP_META_SP.sp,
    lineHeight = 14.sp,
    fontFamily = LocalMagicanFontFamilies.current.body,
    color = color,
)

/** The hairline colour between step rows: the control border at 60%. */
fun livelyStepHairlineColor(palette: Palette = activePalette): Color =
    palette.controlBorder.copy(alpha = palette.controlBorder.alpha * 0.6f)

/**
 * The subtle 1 px rule between two rows of a step list, inset to the text
 * column (never under the leading status mark). Draw it between rows only,
 * never after the last.
 */
@Composable
fun LivelyStepDivider(startInset: Dp = 0.dp) {
    HorizontalDivider(
        modifier = Modifier.padding(start = startInset),
        thickness = Dp.Hairline,
        color = livelyStepHairlineColor(),
    )
}
