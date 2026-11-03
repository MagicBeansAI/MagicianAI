package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.chat.CardTimeText
import ai.magicbeans.magdroid.chat.CardTimes
import android.content.Context
import android.text.format.DateFormat
import android.text.format.DateUtils
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.PlainTooltip
import androidx.compose.material3.Text
import androidx.compose.material3.TooltipBox
import androidx.compose.material3.TooltipDefaults
import androidx.compose.material3.rememberTooltipState
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import java.text.SimpleDateFormat
import java.time.ZoneId
import java.util.Date
import java.util.Locale

/*
 * A chat card's times line: "Created Oct 31, 14:28 · Updated 2 min. ago".
 * The rules are [CardTimeText] (bridge); this file only formats with the
 * platform, so the locale and the 24-hour setting are the phone's.
 */

internal object CardTimeFormat {
    /** The time alone when it is today, else month, day and time. */
    fun absolute(context: Context, atMillis: Long, nowMillis: Long): String = format(
        CardTimeText.absoluteSkeleton(atMillis, nowMillis, ZoneId.systemDefault(), DateFormat.is24HourFormat(context)),
        atMillis,
    )

    /** The full date-time with seconds, for the long-press tooltip. */
    fun exact(context: Context, atMillis: Long): String =
        format(CardTimeText.exactSkeleton(DateFormat.is24HourFormat(context)), atMillis)

    /** "2 min. ago", "3 hr. ago" (the platform's abbreviated phrase); "just now" under a minute. */
    fun relative(atMillis: Long, nowMillis: Long): String =
        CardTimeText.relativeOrJustNow(atMillis, nowMillis) {
            DateUtils.getRelativeTimeSpanString(
                it, nowMillis, DateUtils.MINUTE_IN_MILLIS, DateUtils.FORMAT_ABBREV_RELATIVE,
            ).toString()
        }

    private fun format(skeleton: String, atMillis: Long): String {
        val locale = Locale.getDefault()
        return SimpleDateFormat(DateFormat.getBestDateTimePattern(locale, skeleton), locale).format(Date(atMillis))
    }
}

/**
 * `<startLabel> <absolute> · Updated <relative>`; a part whose time is
 * unknown is left out, and nothing is drawn when both are. A long press shows
 * the exact updated date-time (the start's when there is no update).
 * [nowMillis] is the card's own clock, so the relative part stays current.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun CardTimesLine(
    times: CardTimes,
    nowMillis: Long,
    startLabel: String,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    val line = CardTimeText.line(
        times,
        nowMillis,
        startLabel,
        absolute = { CardTimeFormat.absolute(context, it, nowMillis) },
        relative = { CardTimeFormat.relative(it, nowMillis) },
    ) ?: return
    val exact = times.updatedAtMs?.let { "Updated ${CardTimeFormat.exact(context, it)}" }
        ?: times.startedAtMs?.let { "$startLabel ${CardTimeFormat.exact(context, it)}" }
    TooltipBox(
        positionProvider = TooltipDefaults.rememberPlainTooltipPositionProvider(),
        tooltip = { PlainTooltip { Text(exact.orEmpty()) } },
        state = rememberTooltipState(),
        modifier = modifier,
    ) {
        Text(line, style = livelyStepMetaStyle(Muted), maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}
