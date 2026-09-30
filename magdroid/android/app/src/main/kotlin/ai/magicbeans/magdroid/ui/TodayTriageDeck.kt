package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.today.AttentionDeliveryBinding
import ai.magicbeans.magdroid.today.TodayDeckAction
import ai.magicbeans.magdroid.today.TodayDeckCard
import ai.magicbeans.magdroid.today.TodayDeckLane
import ai.magicbeans.magdroid.today.TodayDeckTab
import ai.magicbeans.magdroid.today.TodayReadingRoomMode
import ai.magicbeans.magdroid.today.TodayUiState
import ai.magicbeans.magdroid.today.deckNeedsMore
import android.provider.Settings
import android.view.HapticFeedbackConstants
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.spring
import androidx.compose.animation.core.tween
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.awaitTouchSlopOrCancellation
import androidx.compose.foundation.gestures.drag
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChange
import androidx.compose.ui.input.pointer.util.VelocityTracker
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.launch
import kotlin.math.abs

// ------------------------------------------------------------------ Pure rules

/** Horizontal commit distance in dp (web 95px). */
internal const val DECK_SWIPE_DP = 95f
/** How far a release's velocity projects the card, like iOS predictedEndTranslation. */
internal const val DECK_PROJECTION_SECONDS = 0.2f

/**
 * Release decision for the top deck card. The current offset decides first,
 * then the velocity-projected end. Only horizontal travel commits: web's
 * swipe-up Seen is a button here, because the card fills most of a phone
 * viewport and an upward page scroll starting on it must never triage.
 * Returns null to spring back.
 */
internal fun resolveDeckRelease(dx: Float, dy: Float, projectedDx: Float, projectedDy: Float): TodayDeckAction? {
    val horizontal = when {
        abs(dx) > DECK_SWIPE_DP -> dx
        abs(projectedDx) > DECK_SWIPE_DP -> projectedDx
        else -> 0f
    }
    if (horizontal > DECK_SWIPE_DP) return TodayDeckAction.Useful
    if (horizontal < -DECK_SWIPE_DP) return TodayDeckAction.Dismiss
    return null
}

internal data class DeckStampOpacity(val useful: Float, val dismiss: Float, val acknowledged: Float)

/** Ink stamps fade in with the drag (dp offsets). ACKNOWLEDGED only shows for the Seen button. */
internal fun deckStampOpacity(dx: Float, @Suppress("UNUSED_PARAMETER") dy: Float): DeckStampOpacity = DeckStampOpacity(
    useful = ((dx - 25f) / 75f).coerceIn(0f, 1f),
    dismiss = ((-dx - 25f) / 75f).coerceIn(0f, 1f),
    acknowledged = 0f,
)

internal fun deckRotationDegrees(dx: Float): Float = dx * 0.07f

/**
 * Only a sideways-dominant drag belongs to the card; any vertical-dominant
 * drag, up or down, scrolls the page.
 */
internal fun deckClaimsDrag(overSlopX: Float, overSlopY: Float): Boolean =
    abs(overSlopX) > abs(overSlopY)

internal fun deckEmptyTitle(triagedInTab: Int): String =
    if (triagedInTab > 0) "All Dispatches Cleared" else "No Dispatches in This Stack"

internal fun deckEmptyMessage(tab: TodayDeckTab, triagedInTab: Int): String =
    if (triagedInTab > 0) "You've triaged all $triagedInTab items in this stack."
    else when (tab) {
        TodayDeckTab.All -> "There are currently no cards in today's deck."
        TodayDeckTab.ForYou -> "There are currently no For You cards in today's deck."
        TodayDeckTab.Worth -> "There are currently no Worth a Look cards in today's deck."
    }

// ------------------------------------------------------------------ Reading room header

@Composable
internal fun ReadingRoomHeader(mode: TodayReadingRoomMode, deckCountLabel: String, onMode: (TodayReadingRoomMode) -> Unit) {
    // Title and a compact switch share one row (web layout); the switch
    // hugs its labels on the right instead of spanning a row of its own.
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            FitText("Reading Room", style = newsSerif(22.sp, FontWeight.Bold), minSize = 16.sp, modifier = Modifier.weight(1f).semantics { heading() })
            Row(
                Modifier.background(Soft, RoundedCornerShape(8.dp)).padding(2.dp),
                horizontalArrangement = Arrangement.spacedBy(2.dp),
            ) {
                SegmentButton("🃏 Brief $deckCountLabel", mode == TodayReadingRoomMode.Deck, Modifier) {
                    onMode(TodayReadingRoomMode.Deck)
                }
                SegmentButton("📰 Broadsheet", mode == TodayReadingRoomMode.Broadsheet, Modifier) {
                    onMode(TodayReadingRoomMode.Broadsheet)
                }
            }
        }
        Hairline()
    }
}

@Composable
private fun SegmentButton(label: String, selected: Boolean, modifier: Modifier, onClick: () -> Unit) {
    Box(
        modifier
            .background(if (selected) Panel else Color.Transparent, RoundedCornerShape(7.dp))
            .then(if (selected) Modifier.border(1.dp, BorderSoft, RoundedCornerShape(7.dp)) else Modifier)
            .clickable(onClick = onClick)
            .semantics { this.selected = selected }
            .padding(horizontal = 9.dp, vertical = 6.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(label, color = if (selected) Ink else Secondary, fontSize = 12.sp,
            fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal, maxLines = 1)
    }
}

// ------------------------------------------------------------------ Deck

private val DECK_CARD_HEIGHT = 350.dp

/**
 * The Morning Brief: a Bumble-style stack of channel follow-ups and
 * worth-a-look cards. Swipe right Useful, left Dismiss, up Seen; tap opens
 * the card's full detail flow.
 */
@Composable
internal fun TodayTriageDeck(
    state: TodayUiState,
    onAction: (TodayDeckCard, TodayDeckAction) -> Boolean,
    onPrimaryCommitted: (TodayDeckCard) -> Unit,
    onOpenCard: (TodayDeckCard) -> Unit,
    onOpenUrl: (String?) -> Unit,
    onOpenBroadsheet: () -> Unit,
    onReviewAgain: () -> Unit,
    onLoadMore: () -> Unit,
    onImpression: (AttentionDeliveryBinding) -> Unit,
) {
    var tab by rememberSaveable { mutableStateOf(TodayDeckTab.All) }
    val stack = state.deckStack(tab)
    val triagedInTab = state.deckTriagedCount(tab)

    LaunchedEffect(stack.size, state.hasMoreFollowUps, state.hasMoreResurfacing) {
        if (deckNeedsMore(stack.size, state.hasMoreFollowUps, state.hasMoreResurfacing)) onLoadMore()
    }

    Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            TodayDeckTab.entries.forEach { option ->
                DeckTabChip(option.label, state.deckStack(option).size, tab == option) { tab = option }
            }
        }
        // One line per distinct failure: an unreachable host fails both lanes identically.
        listOfNotNull(
            state.sectionErrors["message_followups"]?.takeIf { tab != TodayDeckTab.Worth },
            state.sectionErrors["worth_a_look"]?.takeIf { tab != TodayDeckTab.ForYou },
        ).distinct().forEach { TodayInlineError(it) }
        if (stack.isEmpty()) {
            DeckEmpty(tab, triagedInTab, onOpenBroadsheet, onReviewAgain)
        } else {
            val top = stack.first()
            var fling by remember { mutableStateOf<((TodayDeckAction) -> Unit)?>(null) }
            Box(Modifier.fillMaxWidth().height(DECK_CARD_HEIGHT + 24.dp)) {
                stack.getOrNull(2)?.let { DeckPreviewCard(it, depth = 2) }
                stack.getOrNull(1)?.let { DeckPreviewCard(it, depth = 1) }
                key(top.id) {
                    DeckTopCard(
                        card = top,
                        onCommit = { action -> onAction(top, action) },
                        onPrimaryCommitted = onPrimaryCommitted,
                        onOpen = { onOpenCard(top) },
                        onOpenUrl = onOpenUrl,
                        onImpression = onImpression,
                        registerFling = { fling = it },
                    )
                }
            }
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                DeckButton("✕ Dismiss", Danger, Modifier.weight(1f)) { fling?.invoke(TodayDeckAction.Dismiss) }
                DeckButton("Seen", TodayInfo, Modifier.weight(.8f)) { fling?.invoke(TodayDeckAction.Acknowledge) }
                DeckButton("✓ Useful", TodaySuccess, Modifier.weight(1f)) { fling?.invoke(TodayDeckAction.Useful) }
                Button(
                    onClick = { fling?.invoke(TodayDeckAction.Primary) },
                    shape = MagicanButtonShape,
                    colors = ButtonDefaults.buttonColors(containerColor = Coral, contentColor = OnAccent),
                    contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 6.dp, vertical = 8.dp),
                    modifier = Modifier.weight(1f),
                ) { Text("⚡ ${top.primaryLabel}", fontSize = 12.sp, maxLines = 1, fontWeight = FontWeight.SemiBold) }
            }
        }
    }
}

@Composable
private fun DeckTabChip(label: String, remaining: Int, selected: Boolean, onClick: () -> Unit) {
    Row(
        Modifier
            .background(if (selected) Coral else Panel, MagicanButtonShape)
            .border(1.dp, if (selected) Coral else BorderSoft, MagicanButtonShape)
            .clickable(onClick = onClick)
            .semantics { this.selected = selected }
            .padding(horizontal = 12.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Text(label, color = if (selected) OnAccent else Ink, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
        if (remaining > 0) {
            Text(
                "$remaining",
                modifier = Modifier
                    .background(if (selected) OnAccent.copy(alpha = .22f) else Coral.copy(alpha = .14f), RoundedCornerShape(4.dp))
                    .padding(horizontal = 6.dp, vertical = 1.dp),
                color = if (selected) OnAccent else Coral, fontSize = 10.sp, fontWeight = FontWeight.Bold,
            )
        }
    }
}

@Composable
private fun DeckButton(label: String, color: Color, modifier: Modifier, onClick: () -> Unit) {
    OutlinedButton(
        onClick = onClick,
        shape = MagicanButtonShape,
        border = BorderStroke(1.dp, color.copy(alpha = .5f)),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 4.dp, vertical = 8.dp),
        modifier = modifier,
    ) { Text(label, color = color, fontSize = 12.sp, maxLines = 1, fontWeight = FontWeight.SemiBold) }
}

@Composable
private fun DeckPreviewCard(card: TodayDeckCard, depth: Int) {
    val scale = if (depth == 1) .96f else .92f
    Column(
        Modifier
            .fillMaxWidth()
            .height(DECK_CARD_HEIGHT)
            .offset(y = (12 * depth).dp)
            .graphicsLayer { scaleX = scale; scaleY = scale; transformOrigin = androidx.compose.ui.graphics.TransformOrigin(.5f, 1f) }
            .background(Panel, RoundedCornerShape(12.dp))
            .border(1.dp, BorderSoft, RoundedCornerShape(12.dp))
            .padding(18.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        NewsKicker(card.category, color = deckLaneColor(card.lane).copy(alpha = .7f))
        Text(card.title, style = newsSerif(18.sp, FontWeight.SemiBold, color = Secondary), maxLines = 2, overflow = TextOverflow.Ellipsis)
    }
}

internal fun deckLaneColor(lane: TodayDeckLane): Color = when (lane) {
    TodayDeckLane.Dispatch -> Coral
    TodayDeckLane.ReadingRoom -> TodayDiscovery
}

@Composable
private fun DeckTopCard(
    card: TodayDeckCard,
    onCommit: (TodayDeckAction) -> Boolean,
    onPrimaryCommitted: (TodayDeckCard) -> Unit,
    onOpen: () -> Unit,
    onOpenUrl: (String?) -> Unit,
    onImpression: (AttentionDeliveryBinding) -> Unit,
    registerFling: (((TodayDeckAction) -> Unit)?) -> Unit,
) {
    val density = LocalDensity.current
    val context = LocalContext.current
    val view = LocalView.current
    val scope = rememberCoroutineScope()
    val reduceMotion = remember {
        runCatching { Settings.Global.getFloat(context.contentResolver, Settings.Global.ANIMATOR_DURATION_SCALE, 1f) == 0f }
            .getOrDefault(false)
    }
    val offsetX = remember { Animatable(0f) }
    val offsetY = remember { Animatable(0f) }
    val fade = remember { Animatable(1f) }
    var committing by remember { mutableStateOf<TodayDeckAction?>(null) }

    BoxWithConstraints(Modifier.fillMaxWidth().height(DECK_CARD_HEIGHT)) {
        val flyX = with(density) { maxWidth.toPx() } * 1.2f
        val flyUp = with(density) { (-450).dp.toPx() }

        fun fling(action: TodayDeckAction) {
            if (committing != null) return
            committing = action
            view.performHapticFeedback(HapticFeedbackConstants.KEYBOARD_TAP)
            scope.launch {
                if (reduceMotion) {
                    fade.animateTo(0f, tween(180))
                } else {
                    val exit = tween<Float>(260)
                    val (targetX, targetY) = when (action) {
                        TodayDeckAction.Useful, TodayDeckAction.Primary -> flyX to offsetY.value * .5f
                        TodayDeckAction.Dismiss -> -flyX to offsetY.value * .5f
                        TodayDeckAction.Acknowledge -> offsetX.value * .4f to flyUp
                    }
                    launch { offsetY.animateTo(targetY, exit) }
                    offsetX.animateTo(targetX, exit)
                }
                val accepted = onCommit(action)
                if (accepted) {
                    if (action == TodayDeckAction.Primary) onPrimaryCommitted(card)
                } else {
                    // Locked by an in-flight mutation: nothing happened, so the card returns.
                    offsetX.snapTo(0f); offsetY.snapTo(0f); fade.snapTo(1f)
                    committing = null
                }
            }
        }
        LaunchedEffect(card.id) { registerFling(::fling) }

        val dxDp = offsetX.value / density.density
        val dyDp = offsetY.value / density.density
        val stamps = when (committing) {
            TodayDeckAction.Useful -> DeckStampOpacity(1f, 0f, 0f)
            TodayDeckAction.Dismiss -> DeckStampOpacity(0f, 1f, 0f)
            TodayDeckAction.Acknowledge -> DeckStampOpacity(0f, 0f, 1f)
            else -> deckStampOpacity(dxDp, dyDp)
        }
        val binding = card.followUp?.deliveryBinding ?: card.worth?.deliveryBinding

        Column(
            Modifier
                .fillMaxSize()
                .graphicsLayer {
                    translationX = offsetX.value
                    translationY = offsetY.value
                    rotationZ = if (reduceMotion) 0f else deckRotationDegrees(dxDp)
                    alpha = fade.value
                }
                .attentionImpression(binding, onImpression)
                .shadow(8.dp, RoundedCornerShape(12.dp), clip = false)
                .background(Panel, RoundedCornerShape(12.dp))
                .border(1.dp, BorderSoft, RoundedCornerShape(12.dp))
                .semantics {
                    customActions = listOf(
                        CustomAccessibilityAction("Useful") { fling(TodayDeckAction.Useful); true },
                        CustomAccessibilityAction("Dismiss") { fling(TodayDeckAction.Dismiss); true },
                        CustomAccessibilityAction("Seen") { fling(TodayDeckAction.Acknowledge); true },
                        CustomAccessibilityAction(card.primaryLabel) { fling(TodayDeckAction.Primary); true },
                    )
                }
                .pointerInput(card.id) {
                    val tracker = VelocityTracker()
                    awaitEachGesture {
                        val down = awaitFirstDown(requireUnconsumed = false)
                        if (committing != null) return@awaitEachGesture
                        tracker.resetTracking()
                        tracker.addPosition(down.uptimeMillis, down.position)
                        var claimed = false
                        var dx = 0f; var dy = 0f
                        val start = awaitTouchSlopOrCancellation(down.id) { change, overSlop ->
                            if (deckClaimsDrag(overSlop.x, overSlop.y)) {
                                change.consume(); claimed = true
                                dx = overSlop.x; dy = overSlop.y
                            }
                        } ?: return@awaitEachGesture
                        if (!claimed) return@awaitEachGesture
                        scope.launch { offsetX.snapTo(dx); offsetY.snapTo(dy) }
                        drag(start.id) { change ->
                            val delta = change.positionChange()
                            dx += delta.x; dy += delta.y
                            change.consume()
                            tracker.addPosition(change.uptimeMillis, change.position)
                            scope.launch { offsetX.snapTo(dx); offsetY.snapTo(dy) }
                        }
                        val velocity = tracker.calculateVelocity()
                        val px = density.density
                        val action = resolveDeckRelease(
                            dx / px, dy / px,
                            (dx + velocity.x * DECK_PROJECTION_SECONDS) / px,
                            (dy + velocity.y * DECK_PROJECTION_SECONDS) / px,
                        )
                        if (action != null) fling(action) else scope.launch {
                            val back = spring<Float>(dampingRatio = Spring.DampingRatioMediumBouncy, stiffness = Spring.StiffnessMediumLow)
                            launch { offsetY.animateTo(0f, back) }
                            offsetX.animateTo(0f, back)
                        }
                    }
                }
                .clickable(enabled = committing == null, onClick = onOpen)
                .padding(18.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Box(Modifier.fillMaxWidth()) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    val laneColor = deckLaneColor(card.lane)
                    Text(
                        card.category,
                        modifier = Modifier.background(laneColor.copy(alpha = .1f), RoundedCornerShape(4.dp)).padding(horizontal = 6.dp, vertical = 2.dp),
                        color = laneColor, fontFamily = newsMono(), fontSize = 10.sp, fontWeight = FontWeight.Bold,
                        maxLines = 1, overflow = TextOverflow.Ellipsis, letterSpacing = .8.sp,
                    )
                    card.sender?.let { Text(it, color = Muted, fontSize = 11.sp, maxLines = 1, overflow = TextOverflow.Ellipsis) }
                }
            }
            Text(card.title, style = newsSerif(23.sp, FontWeight.SemiBold, lineHeight = 28.sp), maxLines = 3, overflow = TextOverflow.Ellipsis)
            Text(card.summary, color = Secondary, fontSize = 14.sp, lineHeight = 20.sp, maxLines = 6, overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f, fill = false))
            Spacer(Modifier.weight(1f))
            val followUp = card.followUp
            followUp?.openUrl?.takeIf(String::isNotBlank)?.let { url ->
                Text(
                    "View thread in ${followUp.provider.ifBlank { "app" }} ↗",
                    color = Coral, fontSize = 13.sp, fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.clickable { onOpenUrl(url) }.padding(vertical = 4.dp),
                )
            }
            Text("Tap for details · swipe to triage", color = Muted, fontSize = 10.sp, modifier = Modifier.fillMaxWidth(), textAlign = TextAlign.Center)
        }
        // Ink stamps sit above the card content and move with it.
        Box(
            Modifier.fillMaxSize().graphicsLayer {
                translationX = offsetX.value
                translationY = offsetY.value
                rotationZ = if (reduceMotion) 0f else deckRotationDegrees(dxDp)
                alpha = fade.value
            },
        ) {
            DeckStamp("USEFUL", TodaySuccess, 14f, stamps.useful, Modifier.align(Alignment.TopStart).padding(18.dp).padding(top = 30.dp))
            DeckStamp("DISMISS", Danger, -14f, stamps.dismiss, Modifier.align(Alignment.TopEnd).padding(18.dp).padding(top = 30.dp))
            DeckStamp("ACKNOWLEDGED", TodayInfo, 0f, stamps.acknowledged, Modifier.align(Alignment.TopCenter).padding(top = 64.dp))
        }
    }
}

@Composable
private fun DeckStamp(label: String, color: Color, rotation: Float, opacity: Float, modifier: Modifier) {
    if (opacity <= 0f) return
    Text(
        label,
        modifier = modifier
            .graphicsLayer { rotationZ = rotation; alpha = opacity }
            .background(color.copy(alpha = .08f), RoundedCornerShape(6.dp))
            .border(3.dp, color, RoundedCornerShape(6.dp))
            .padding(horizontal = 10.dp, vertical = 4.dp),
        style = newsSerif(26.sp, FontWeight.ExtraBold, color = color, letterSpacing = 1.5.sp),
    )
}

@Composable
private fun DeckEmpty(tab: TodayDeckTab, triagedInTab: Int, onOpenBroadsheet: () -> Unit, onReviewAgain: () -> Unit) {
    Column(
        Modifier.fillMaxWidth()
            .background(Panel, RoundedCornerShape(12.dp))
            .border(1.dp, BorderSoft, RoundedCornerShape(12.dp))
            .padding(horizontal = 20.dp, vertical = 28.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text("☕", fontSize = 34.sp)
        Text(deckEmptyTitle(triagedInTab), style = newsSerif(21.sp, FontWeight.Bold), textAlign = TextAlign.Center)
        Text(deckEmptyMessage(tab, triagedInTab), color = Secondary, fontSize = 13.sp, textAlign = TextAlign.Center)
        Spacer(Modifier.size(4.dp))
        Button(onClick = onOpenBroadsheet, shape = MagicanButtonShape, colors = ButtonDefaults.buttonColors(containerColor = Coral, contentColor = OnAccent)) {
            Text("📰 Open Broadsheet View")
        }
        if (triagedInTab > 0) OutlinedButton(onClick = onReviewAgain, shape = MagicanButtonShape) { Text("↻ Review Again", color = Ink) }
    }
}
