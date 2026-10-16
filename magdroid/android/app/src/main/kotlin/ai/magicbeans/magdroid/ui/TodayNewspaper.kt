package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.today.AttentionDeliveryBinding
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.boundsInWindow
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay

/*
 * Shared newspaper vocabulary for the Morning Edition Today page: the
 * Newsreader serif, mono kickers, hairlines, section banners and the
 * on-screen impression rule. Colours come only from theme tokens.
 */

internal val TodaySuccess: Color get() = activePalette.success
internal val TodayWarning: Color get() = activePalette.warning
internal val TodayDiscovery: Color get() = activePalette.discovery
internal val TodayInfo: Color get() = activePalette.info

internal fun newsSerif(
    size: TextUnit,
    weight: FontWeight = FontWeight.SemiBold,
    italic: Boolean = false,
    color: Color = Ink,
    lineHeight: TextUnit = TextUnit.Unspecified,
    letterSpacing: TextUnit = 0.sp,
): TextStyle = TextStyle(
    fontFamily = NewsreaderFamily,
    fontSize = size,
    fontWeight = weight,
    fontStyle = if (italic) FontStyle.Italic else FontStyle.Normal,
    color = color,
    lineHeight = lineHeight,
    letterSpacing = letterSpacing,
)

@Composable
internal fun newsMono(): FontFamily = LocalMagicanFontFamilies.current.mono

/** Mono caps dateline / kicker. */
@Composable
internal fun NewsKicker(text: String, color: Color = Muted, modifier: Modifier = Modifier, size: TextUnit = 10.sp, maxLines: Int = 1) {
    Text(
        text.uppercase(),
        modifier = modifier,
        color = color,
        fontFamily = newsMono(),
        fontSize = size,
        fontWeight = FontWeight.SemiBold,
        letterSpacing = 1.sp,
        maxLines = maxLines,
        overflow = TextOverflow.Ellipsis,
    )
}

@Composable
internal fun Hairline(modifier: Modifier = Modifier, color: Color = BorderSoft) {
    Box(modifier.fillMaxWidth().height(1.dp).background(color))
}

/** Two 1dp rules in the text colour, 3dp apart — the masthead's double rule. */
@Composable
internal fun DoubleRule(modifier: Modifier = Modifier) {
    Column(modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(3.dp)) {
        Box(Modifier.fillMaxWidth().height(1.dp).background(Ink))
        Box(Modifier.fillMaxWidth().height(1.dp).background(Ink))
    }
}

/** `§ n` banner: marker, serif title, italic sub and an optional trailing action. */
@Composable
internal fun NewsSectionBanner(
    marker: String?,
    title: String,
    subtitle: String?,
    modifier: Modifier = Modifier,
    trailing: (@Composable RowScope.() -> Unit)? = null,
) {
    Column(modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Row(Modifier.weight(1f), verticalAlignment = Alignment.Bottom) {
                marker?.let {
                    Text(it, style = newsSerif(20.sp, FontWeight.Bold, color = Coral))
                    Spacer(Modifier.size(8.dp))
                }
                FitText(title, style = newsSerif(21.sp, FontWeight.Bold, lineHeight = 25.sp), modifier = Modifier.weight(1f, fill = false))
            }
            trailing?.invoke(this)
        }
        subtitle?.let { Text(it, style = newsSerif(13.sp, FontWeight.Normal, italic = true, color = Secondary)) }
        Hairline(Modifier.padding(top = 4.dp))
    }
}

/**
 * One-line text that steps its font size down until it fits its width, never
 * below [minSize]. Section heads use it so long titles ("Special Reports &
 * Briefings") stay on one line on narrow phones and at large system font
 * scales. Drawing waits for the fitted size, so no oversized frame flashes.
 */
@Composable
internal fun FitText(
    text: String,
    style: TextStyle,
    modifier: Modifier = Modifier,
    minSize: TextUnit = 14.sp,
    color: Color = Color.Unspecified,
) {
    var size by remember(text, style) { mutableStateOf(style.fontSize) }
    var ready by remember(text, style) { mutableStateOf(false) }
    Text(
        text,
        modifier = modifier.drawWithContent { if (ready) drawContent() },
        style = style.copy(fontSize = size, lineHeight = if (style.lineHeight.isSp) style.lineHeight * (size.value / style.fontSize.value) else style.lineHeight),
        color = color,
        maxLines = 1,
        softWrap = false,
        overflow = TextOverflow.Ellipsis,
        onTextLayout = { layout ->
            if (layout.hasVisualOverflow && size.value > minSize.value) {
                size = (size.value * 0.92f).coerceAtLeast(minSize.value).sp
            } else {
                ready = true
            }
        },
    )
}

/** Inline section error with an optional Retry, used wherever a read can fail. */
@Composable
internal fun TodayInlineError(message: String, onRetry: (() -> Unit)? = null) {
    Row(
        Modifier.fillMaxWidth().background(Danger.copy(alpha = .08f), RoundedCornerShape(9.dp)).padding(horizontal = 9.dp, vertical = 4.dp),
        horizontalArrangement = Arrangement.spacedBy(7.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(Icons.Outlined.Close, null, tint = Danger, modifier = Modifier.size(15.dp))
        Text(message, color = Danger, fontSize = 11.sp, modifier = Modifier.weight(1f).padding(vertical = 5.dp))
        onRetry?.let { TextButton(onClick = it) { Text("Retry", color = Danger, fontSize = 12.sp) } }
    }
}

/**
 * A card counts as seen once at least half of it is inside the window. A
 * LazyColumn only places on-screen rows, and precomposed or scrolled-away rows
 * report empty clipped bounds, so off-screen cards never start the timer.
 */
internal fun isImpressionVisible(clippedHeight: Float, clippedWidth: Float, fullHeight: Int): Boolean =
    fullHeight > 0 && clippedWidth > 0f && clippedHeight >= fullHeight * 0.5f

/**
 * Records the delivery impression after the policy's visible duration, and
 * only while the card stays on screen; scrolling it away restarts the clock.
 */
@Composable
internal fun Modifier.attentionImpression(
    binding: AttentionDeliveryBinding?,
    record: (AttentionDeliveryBinding) -> Unit,
): Modifier {
    if (binding == null) return this
    var visible by remember(binding.identity) { mutableStateOf(false) }
    LaunchedEffect(binding.identity, visible) {
        if (!visible) return@LaunchedEffect
        delay(binding.minVisibleMs.toLong())
        record(binding)
    }
    return onGloballyPositioned { coordinates ->
        val bounds = coordinates.boundsInWindow()
        visible = coordinates.isAttached &&
            isImpressionVisible(bounds.height, bounds.width, coordinates.size.height)
    }
}
