package ai.magicbeans.magdroid.chat

import java.time.Instant
import java.time.ZoneId

/*
 * When a chat card began and last changed: "Created Oct 31, 14:28 · Updated
 * 2 min. ago" on the live task card, "Started … · Updated …" on a turn's
 * Steps card. Pure: the times, which part shows, and which date-time
 * skeleton a part uses. The app formats with the platform (locale and the
 * 24-hour setting) and owns the clock.
 */

/** Epoch milliseconds; null when unknown, and the part that reads it is hidden. */
data class CardTimes(val startedAtMs: Long?, val updatedAtMs: Long?) {
    val isEmpty: Boolean get() = startedAtMs == null && updatedAtMs == null

    companion object {
        /**
         * The live task card. Created: the task record's `created_at` when
         * fetched, else the first status update the chat saw. Updated: the
         * later of the record's updated time ([TaskStoreRecord.updatedAtMs]:
         * last progress, else `updated_at`) and the latest status update.
         */
        fun forTask(task: TaskStatusCard, storeCreatedAtMs: Long? = null, storeUpdatedAtMs: Long? = null): CardTimes {
            val created = positive(storeCreatedAtMs) ?: LiveTaskCardState.epochMillis(task.startedAtMs?.toDouble())
            val updated = listOfNotNull(
                positive(storeUpdatedAtMs),
                LiveTaskCardState.epochMillis(task.updatedAtMs?.toDouble()),
            ).maxOrNull()
            return of(created, updated)
        }

        /**
         * A turn's Steps card. Started: the earliest row start or first event.
         * Updated: the latest row event or call end (start + duration).
         */
        fun forActivity(rows: List<ActivityRow>): CardTimes {
            val starts = rows.flatMap { listOfNotNull(it.startedAtMs, it.firstSeenAtMs) }
            val updates = rows.flatMap { row ->
                listOfNotNull(
                    row.updatedAtMs,
                    row.startedAtMs,
                    row.startedAtMs?.let { start -> row.durationMs?.let { start + it } },
                )
            }
            return of(millis(starts).minOrNull(), millis(updates).maxOrNull())
        }

        /** Row times are already epoch ms. */
        private fun millis(values: List<Double>): List<Long> =
            values.filter { it.isFinite() && it > 0 }.map { it.toLong() }

        /** An update never reads earlier than the start it follows. */
        private fun of(started: Long?, updated: Long?): CardTimes =
            CardTimes(started, if (started != null && updated != null) maxOf(started, updated) else updated)

        private fun positive(value: Long?): Long? = value?.takeIf { it > 0 }
    }
}

object CardTimeText {
    /** How often a card refreshes its relative "Updated" when nothing else ticks. */
    const val RELATIVE_TICK_MILLIS = 30_000L

    /** Below this an update reads "just now" (the platform would say "0 min. ago"). */
    const val JUST_NOW_MILLIS = 60_000L

    /**
     * `<startLabel> <absolute> · Updated <relative>`, each part only when its
     * time is known; null when neither is.
     */
    fun line(
        times: CardTimes,
        nowMillis: Long,
        startLabel: String,
        absolute: (Long) -> String,
        relative: (Long) -> String,
    ): String? {
        val parts = listOfNotNull(
            times.startedAtMs?.let { "$startLabel ${absolute(it)}" },
            times.updatedAtMs?.let { "Updated ${relativeOrJustNow(it, nowMillis, relative)}" },
        )
        return parts.takeIf { it.isNotEmpty() }?.joinToString(" · ")
    }

    /** "just now" under a minute (or for a time ahead of the clock), else the platform's phrase. */
    fun relativeOrJustNow(atMillis: Long, nowMillis: Long, relative: (Long) -> String): String =
        if (nowMillis - atMillis < JUST_NOW_MILLIS) "just now" else relative(atMillis)

    /**
     * The skeleton for an absolute time (`DateFormat.getBestDateTimePattern`):
     * the time only when it falls today in [zone], else month, day and time.
     */
    fun absoluteSkeleton(atMillis: Long, nowMillis: Long, zone: ZoneId, is24Hour: Boolean): String {
        val sameDay = Instant.ofEpochMilli(atMillis).atZone(zone).toLocalDate() ==
            Instant.ofEpochMilli(nowMillis).atZone(zone).toLocalDate()
        val time = if (is24Hour) "Hm" else "hma"
        return if (sameDay) time else "MMMd$time"
    }

    /** The long-press skeleton: the full date with seconds. */
    fun exactSkeleton(is24Hour: Boolean): String = if (is24Hour) "yMMMdHms" else "yMMMdhmsa"
}
