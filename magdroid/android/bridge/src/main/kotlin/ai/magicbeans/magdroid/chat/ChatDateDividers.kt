package ai.magicbeans.magdroid.chat

import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.time.temporal.ChronoUnit
import java.util.Locale

/**
 * Day labels for the chat transcript's date dividers and the floating date
 * pill: "Today", "Yesterday", a weekday within the last week, then a medium
 * date. Local calendar throughout. Android port of iOS `ChatDateDividers.swift`
 * (web `chat/dayLabel.ts`). Times are epoch milliseconds.
 */
object ChatDateDividers {
    private fun day(millis: Long, zone: ZoneId): LocalDate = Instant.ofEpochMilli(millis).atZone(zone).toLocalDate()

    /**
     * For timestamps in display order, the indices that open a new local day
     * (the first dated message always opens one). A message without a time
     * (null) never opens a day and does not break the run it sits in.
     */
    fun insertionIndices(timestamps: List<Long?>, zone: ZoneId = ZoneId.systemDefault()): List<Int> {
        val indices = mutableListOf<Int>()
        var previous: LocalDate? = null
        timestamps.forEachIndexed { index, timestamp ->
            if (timestamp == null) return@forEachIndexed
            val current = day(timestamp, zone)
            if (current != previous) indices.add(index)
            previous = current
        }
        return indices
    }

    /**
     * "Today", "Yesterday", the weekday for 2–6 days back, else a medium
     * date ("Sep 24, 2026"). A future day reads as its date.
     */
    fun label(
        dateMillis: Long,
        nowMillis: Long = System.currentTimeMillis(),
        zone: ZoneId = ZoneId.systemDefault(),
        locale: Locale = Locale.getDefault(),
    ): String {
        val date = day(dateMillis, zone)
        val days = ChronoUnit.DAYS.between(date, day(nowMillis, zone))
        if (days == 0L) return "Today"
        if (days == 1L) return "Yesterday"
        if (days in 2..6) return DateTimeFormatter.ofPattern("EEEE", locale).format(date)
        return DateTimeFormatter.ofLocalizedDate(FormatStyle.MEDIUM).withLocale(locale).format(date)
    }

    /** The divider label for each index that opens a day. */
    fun labels(
        timestamps: List<Long?>,
        nowMillis: Long = System.currentTimeMillis(),
        zone: ZoneId = ZoneId.systemDefault(),
        locale: Locale = Locale.getDefault(),
    ): Map<Int, String> = insertionIndices(timestamps, zone).mapNotNull { index ->
        timestamps[index]?.let { index to label(it, nowMillis, zone, locale) }
    }.toMap()

    /**
     * Epoch milliseconds (the wire's `created_at`), or seconds below 1e11, as
     * milliseconds. Null for zero, negative or non-finite values.
     */
    fun epochMillis(value: Double): Long? {
        if (!value.isFinite() || value <= 0) return null
        return if (value < 1e11) (value * 1_000).toLong() else value.toLong()
    }

    /**
     * The floating pill's day: the topmost visible dated message's day, or
     * null when none is dated or when a visible divider already shows that
     * day (the pill never doubles a divider). `visibleIndices` in any order.
     */
    fun floatingLabel(
        timestamps: List<Long?>,
        visibleIndices: Set<Int>,
        nowMillis: Long = System.currentTimeMillis(),
        zone: ZoneId = ZoneId.systemDefault(),
        locale: Locale = Locale.getDefault(),
    ): String? {
        val top = visibleIndices.sorted().firstOrNull { it in timestamps.indices && timestamps[it] != null } ?: return null
        val topLabel = label(timestamps[top]!!, nowMillis, zone, locale)
        val dividers = labels(timestamps, nowMillis, zone, locale)
        val visibleDividers = visibleIndices.mapNotNull { dividers[it] }
        return if (topLabel in visibleDividers) null else topLabel
    }
}
