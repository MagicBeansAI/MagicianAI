package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.today.TodayAction
import androidx.compose.animation.core.animate
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.Orientation
import androidx.compose.foundation.gestures.draggable
import androidx.compose.foundation.gestures.rememberDraggableState
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlin.math.max
import kotlin.math.min

/** An action displayed behind a Today card's leading or trailing edge. */
internal data class TodaySwipeAction(
    val id: String,
    val title: String,
    val icon: ImageVector,
    val color: Color,
    val run: () -> Unit,
)

/** Pure release decisions keep gesture thresholds deterministic and unit-testable. */
internal enum class TodaySwipeResolution {
    Closed,
    LeadingOpen,
    TrailingOpen,
    CommitLeading,
    CommitTrailing,
}

internal data class TodayActionPresentation(
    val inline: List<TodayAction>,
    val overflow: List<TodayAction>,
)

/** Every executable action appears exactly once; only presentation is bounded. */
internal fun partitionTodayActions(
    actions: List<TodayAction>,
    inlineLimit: Int = 2,
): TodayActionPresentation {
    val boundedLimit = inlineLimit.coerceAtLeast(0)
    return TodayActionPresentation(
        inline = actions.take(boundedLimit),
        overflow = actions.drop(boundedLimit),
    )
}

internal fun resolveTodaySwipeRelease(
    rawOffset: Float,
    projectedOffset: Float,
    cardWidth: Float,
    leadingRevealWidth: Float,
    trailingRevealWidth: Float,
    commitOvershoot: Float,
): TodaySwipeResolution {
    val trailingThreshold = max(trailingRevealWidth + commitOvershoot, cardWidth * 0.5f)
    val leadingThreshold = max(leadingRevealWidth + commitOvershoot, cardWidth * 0.5f)
    return when {
        trailingRevealWidth > 0f &&
            (rawOffset <= -trailingThreshold || projectedOffset < -trailingThreshold) ->
            TodaySwipeResolution.CommitTrailing
        leadingRevealWidth > 0f &&
            (rawOffset >= leadingThreshold || projectedOffset > leadingThreshold) ->
            TodaySwipeResolution.CommitLeading
        trailingRevealWidth > 0f && projectedOffset < -(trailingRevealWidth * 0.35f) ->
            TodaySwipeResolution.TrailingOpen
        leadingRevealWidth > 0f && projectedOffset > leadingRevealWidth * 0.35f ->
            TodaySwipeResolution.LeadingOpen
        else -> TodaySwipeResolution.Closed
    }
}

internal fun todaySwipeVisibleOffset(
    rawOffset: Float,
    cardWidth: Float,
    leadingRevealWidth: Float,
    trailingRevealWidth: Float,
    fallbackTravel: Float = 160f,
): Float {
    val cap = if (cardWidth > 0f) {
        cardWidth * 0.92f
    } else {
        max(leadingRevealWidth, trailingRevealWidth) + fallbackTravel
    }
    return when {
        rawOffset < -trailingRevealWidth && trailingRevealWidth > 0f ->
            max(-cap, -trailingRevealWidth - ((-trailingRevealWidth) - rawOffset) * 0.55f)
        rawOffset > leadingRevealWidth && leadingRevealWidth > 0f ->
            min(cap, leadingRevealWidth + (rawOffset - leadingRevealWidth) * 0.55f)
        trailingRevealWidth == 0f && rawOffset < 0f -> 0f
        leadingRevealWidth == 0f && rawOffset > 0f -> 0f
        else -> rawOffset
    }
}

internal fun isTodaySwipeRailExposed(restingOffset: Float, leading: Boolean): Boolean =
    if (leading) restingOffset > 0f else restingOffset < 0f

/**
 * Mail-style Today swipe behavior shared by core, follow-up, and resurfacing cards.
 *
 * A short swipe leaves a labeled, tappable rail open. A long or fast swipe commits
 * the first action on that edge. State is keyed by the durable card identity so a
 * removed row can never donate its open/dismissed position to its successor.
 */
@Composable
internal fun TodaySwipeActionCard(
    itemId: String,
    leadingActions: List<TodaySwipeAction>,
    trailingActions: List<TodaySwipeAction>,
    enabled: Boolean,
    modifier: Modifier = Modifier,
    content: @Composable () -> Unit,
) {
    val density = LocalDensity.current
    val actionWidthPx = with(density) { 72.dp.toPx() }
    val commitOvershootPx = with(density) { 96.dp.toPx() }
    val fallbackTravelPx = with(density) { 160.dp.toPx() }
    val leadingRevealWidth = actionWidthPx * leadingActions.size
    val trailingRevealWidth = actionWidthPx * trailingActions.size
    val scope = rememberCoroutineScope()
    var cardWidth by remember(itemId) { mutableFloatStateOf(0f) }
    var restingOffset by remember(itemId) { mutableFloatStateOf(0f) }
    var rawOffset by remember(itemId) { mutableFloatStateOf(0f) }
    var renderedOffset by remember(itemId) { mutableFloatStateOf(0f) }
    var dragging by remember(itemId) { mutableStateOf(false) }
    var translationLayerActive by remember(itemId) { mutableStateOf(false) }
    var animationJob by remember(itemId) { mutableStateOf<Job?>(null) }
    val actionSignature = remember(leadingActions, trailingActions) {
        (leadingActions.map(TodaySwipeAction::id) + "|" + trailingActions.map(TodaySwipeAction::id)).joinToString()
    }

    fun settle(target: Float) {
        animationJob?.cancel()
        translationLayerActive = true
        restingOffset = target
        rawOffset = target
        val start = renderedOffset
        animationJob = scope.launch {
            animate(start, target, animationSpec = tween(durationMillis = 220)) { value, _ ->
                renderedOffset = value
            }
            translationLayerActive = target != 0f
        }
    }

    fun invoke(action: TodaySwipeAction) {
        settle(0f)
        action.run()
    }

    LaunchedEffect(itemId, actionSignature, enabled) {
        if (!enabled || renderedOffset != 0f) {
            animationJob?.cancel()
            restingOffset = 0f
            rawOffset = 0f
            renderedOffset = 0f
            translationLayerActive = false
        }
    }

    val dragState = rememberDraggableState { delta ->
        rawOffset += delta
        renderedOffset = todaySwipeVisibleOffset(
            rawOffset = rawOffset,
            cardWidth = cardWidth,
            leadingRevealWidth = leadingRevealWidth,
            trailingRevealWidth = trailingRevealWidth,
            fallbackTravel = fallbackTravelPx,
        )
    }

    val swipeClipModifier = if (translationLayerActive) {
        Modifier.clip(RoundedCornerShape(13.dp))
    } else {
        Modifier
    }
    Box(
        modifier = modifier
            .fillMaxWidth()
            .then(swipeClipModifier)
            .onSizeChanged { cardWidth = it.width.toFloat() }
            .draggable(
                state = dragState,
                orientation = Orientation.Horizontal,
                enabled = enabled && (leadingActions.isNotEmpty() || trailingActions.isNotEmpty()),
                onDragStarted = {
                    animationJob?.cancel()
                    translationLayerActive = true
                    dragging = true
                    rawOffset = restingOffset
                    renderedOffset = restingOffset
                },
                onDragStopped = { velocity ->
                    dragging = false
                    val resolution = resolveTodaySwipeRelease(
                        rawOffset = rawOffset,
                        projectedOffset = rawOffset + velocity * 0.2f,
                        cardWidth = cardWidth,
                        leadingRevealWidth = leadingRevealWidth,
                        trailingRevealWidth = trailingRevealWidth,
                        commitOvershoot = commitOvershootPx,
                    )
                    when (resolution) {
                        TodaySwipeResolution.CommitLeading -> leadingActions.firstOrNull()?.let(::invoke)
                        TodaySwipeResolution.CommitTrailing -> trailingActions.firstOrNull()?.let(::invoke)
                        TodaySwipeResolution.LeadingOpen -> settle(leadingRevealWidth)
                        TodaySwipeResolution.TrailingOpen -> settle(-trailingRevealWidth)
                        TodaySwipeResolution.Closed -> settle(0f)
                    }
                },
            ),
    ) {
        if (translationLayerActive) {
            Row(Modifier.matchParentSize()) {
                leadingActions.forEach { action ->
                    TodaySwipeActionButton(
                        action = action,
                        enabled = enabled,
                        exposed = isTodaySwipeRailExposed(restingOffset, leading = true),
                    ) { invoke(action) }
                }
                Spacer(Modifier.weight(1f))
                trailingActions.forEach { action ->
                    TodaySwipeActionButton(
                        action = action,
                        enabled = enabled,
                        exposed = isTodaySwipeRailExposed(restingOffset, leading = false),
                    ) { invoke(action) }
                }
            }

            FullSwipePanel(
                action = trailingActions.firstOrNull(),
                visibleWidthPx = (-renderedOffset).takeIf { it > trailingRevealWidth },
                armed = rawOffset <= -max(trailingRevealWidth + commitOvershootPx, cardWidth * 0.5f),
                trailing = true,
            )
            FullSwipePanel(
                action = leadingActions.firstOrNull(),
                visibleWidthPx = renderedOffset.takeIf { it > leadingRevealWidth },
                armed = rawOffset >= max(leadingRevealWidth + commitOvershootPx, cardWidth * 0.5f),
                trailing = false,
            )
        }

        val translationModifier = if (translationLayerActive) {
            Modifier.graphicsLayer { translationX = renderedOffset }
        } else {
            Modifier
        }
        Box(Modifier.fillMaxWidth().then(translationModifier)) {
            content()
            if (!dragging && restingOffset != 0f) {
                Box(
                    Modifier
                        .matchParentSize()
                        .clickable(
                            interactionSource = remember { MutableInteractionSource() },
                            indication = null,
                            onClick = { settle(0f) },
                        ),
                )
            }
        }
    }
}

@Composable
private fun TodaySwipeActionButton(action: TodaySwipeAction, enabled: Boolean, exposed: Boolean, run: () -> Unit) {
    val foreground = chatContrastingTextColor(action.color)
    val actionModifier = Modifier
        .width(72.dp)
        .fillMaxHeight()
        .background(action.color)
        .clickable(enabled = enabled && exposed, onClick = run)
        .let { if (exposed) it else it.clearAndSetSemantics { } }
    Column(
        modifier = actionModifier,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Spacer(Modifier.weight(1f))
        Icon(action.icon, contentDescription = null, tint = foreground)
        Text(
            action.title,
            color = foreground,
            fontSize = 10.sp,
            fontWeight = FontWeight.SemiBold,
            lineHeight = 12.sp,
            textAlign = TextAlign.Center,
        )
        Spacer(Modifier.weight(1f))
    }
}

@Composable
private fun BoxScope.FullSwipePanel(
    action: TodaySwipeAction?,
    visibleWidthPx: Float?,
    armed: Boolean,
    trailing: Boolean,
) {
    if (action == null || visibleWidthPx == null) return
    val foreground = chatContrastingTextColor(action.color)
    val width = with(LocalDensity.current) { visibleWidthPx.toDp() }
    Box(
        modifier = Modifier
            .align(if (trailing) Alignment.CenterEnd else Alignment.CenterStart)
            .fillMaxHeight()
            .width(width.coerceAtLeast(72.dp))
            .background(action.color.copy(alpha = if (armed) 1f else 0.85f))
            .clearAndSetSemantics { },
        contentAlignment = if (trailing) Alignment.CenterStart else Alignment.CenterEnd,
    ) {
        Column(
            modifier = Modifier.width(72.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Icon(action.icon, contentDescription = null, tint = foreground)
            Text(action.title, color = foreground, fontSize = 10.sp, fontWeight = FontWeight.SemiBold)
        }
    }
}
