package ai.magicbeans.magdroid.voice

/**
 * How long an armed microphone stays armed.
 *
 * A port of `AmbientArm.swift`, one rule shared by the notification's Extend
 * button, the service's own leash, and any setting that chooses an initial
 * window. Each extension adds a deliberately legible thirty minutes; the last
 * one may add less when it lands near the ceiling.
 *
 * iOS caps the window at eight hours because a Live Activity cannot stay on
 * screen past that, and a microphone must never outlive its only external Stop
 * control. Android's ongoing notification has no such limit — it lives as long
 * as the foreground service does — so the cap here is not forced by the
 * platform. It is kept anyway: the reason a leash exists is that somebody arms
 * a microphone and then stops thinking about it, and that is true on both
 * phones.
 */
object AmbientExtensionPolicy {

    const val INCREMENT_MS: Long = 30 * 60 * 1000

    /** The initial window, matching iOS's default arm. */
    const val DEFAULT_WINDOW_MS: Long = 30 * 60 * 1000

    const val MAXIMUM_WINDOW_MS: Long = 8 * 60 * 60 * 1000

    /**
     * The next expiry, or null when the window is already at its ceiling.
     *
     * Null rather than the unchanged value, so a caller can tell "extended" from
     * "cannot extend" and say so instead of appearing to have added time.
     */
    fun extendedExpiry(armedAtMs: Long, currentExpiryMs: Long): Long? {
        val ceiling = armedAtMs + MAXIMUM_WINDOW_MS
        val candidate = minOf(currentExpiryMs + INCREMENT_MS, ceiling)
        return if (candidate > currentExpiryMs) candidate else null
    }

    /** Whether the window has run out at [nowMs]. */
    fun hasExpired(expiryMs: Long, nowMs: Long): Boolean = nowMs >= expiryMs

    /** What is left, floored at zero so a lapsed window never reads as negative. */
    fun remainingMs(expiryMs: Long, nowMs: Long): Long = (expiryMs - nowMs).coerceAtLeast(0)
}

/**
 * One armed window.
 *
 * Held as a value so the service can persist and restore it: a process death
 * with the microphone still running must not lose the expiry, or the leash
 * silently becomes indefinite — which is the failure the whole thing exists to
 * prevent.
 */
data class AmbientArm(val armedAtMs: Long, val expiresAtMs: Long) {

    fun extended(): AmbientArm? =
        AmbientExtensionPolicy.extendedExpiry(armedAtMs, expiresAtMs)?.let { copy(expiresAtMs = it) }

    fun hasExpired(nowMs: Long): Boolean = AmbientExtensionPolicy.hasExpired(expiresAtMs, nowMs)

    fun remainingMs(nowMs: Long): Long = AmbientExtensionPolicy.remainingMs(expiresAtMs, nowMs)

    /** True when another extension would add nothing. */
    val atCeiling: Boolean get() = extended() == null

    companion object {
        fun armedAt(
            nowMs: Long,
            windowMs: Long = AmbientExtensionPolicy.DEFAULT_WINDOW_MS,
        ): AmbientArm = AmbientArm(nowMs, nowMs + windowMs.coerceAtMost(AmbientExtensionPolicy.MAXIMUM_WINDOW_MS))
    }
}

/**
 * How long is left, said plainly.
 *
 * Minutes rather than a clock time: the owner is deciding whether to extend,
 * and "24 minutes left" answers that where "until 15:42" makes them do the
 * arithmetic.
 */
fun formatRemaining(remainingMs: Long): String {
    if (remainingMs <= 0) return "expiring"
    val totalMinutes = ((remainingMs + 59_999) / 60_000).toInt()
    if (totalMinutes < 60) return "$totalMinutes min left"
    val hours = totalMinutes / 60
    val minutes = totalMinutes % 60
    return if (minutes == 0) "${hours}h left" else "${hours}h ${minutes}m left"
}

/**
 * The armed window, across a process death.
 *
 * A foreground service can be killed and restarted with the microphone still
 * wanted. Without this the leash would be re-armed from scratch on every
 * restart, silently handing back time the owner had already spent — and a
 * window that keeps renewing itself is not a leash.
 */
object AmbientArmStore {

    private const val PREFS = "magdroid_ambient_arm_v1"
    private const val KEY_ARMED_AT = "armed_at_ms"
    private const val KEY_EXPIRES_AT = "expires_at_ms"

    fun save(context: android.content.Context, arm: AmbientArm?) {
        if (arm == null) {
            clear(context)
            return
        }
        prefs(context).edit()
            .putLong(KEY_ARMED_AT, arm.armedAtMs)
            .putLong(KEY_EXPIRES_AT, arm.expiresAtMs)
            .apply()
    }

    fun load(context: android.content.Context): AmbientArm? {
        val store = prefs(context)
        val armedAt = store.getLong(KEY_ARMED_AT, 0L)
        val expiresAt = store.getLong(KEY_EXPIRES_AT, 0L)
        if (armedAt <= 0L || expiresAt <= armedAt) return null
        return AmbientArm(armedAt, expiresAt)
    }

    fun clear(context: android.content.Context) {
        prefs(context).edit().remove(KEY_ARMED_AT).remove(KEY_EXPIRES_AT).apply()
    }

    private fun prefs(context: android.content.Context) =
        context.applicationContext.getSharedPreferences(PREFS, android.content.Context.MODE_PRIVATE)
}
