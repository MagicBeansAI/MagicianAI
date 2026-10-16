package ai.magicbeans.magdroid.voice

import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.BatteryManager
import android.os.PowerManager

/** Why ambient listening was refused, or warned about. */
enum class AmbientPowerBlock(val message: String, val shortLabel: String) {
    BatterySaver(
        "Battery Saver is on, so Magican stopped listening.",
        "Battery Saver — higher battery use",
    ),
    BatteryLow(
        "Battery is low, so Magican stopped listening.",
        "Battery low",
    ),
}

/** Whether a window may open, given the power conditions at that instant. */
sealed class AmbientPowerAdmission {
    object Allowed : AmbientPowerAdmission()

    /** Open it, and carry the reason for the whole window. */
    data class AllowedWithWarning(val block: AmbientPowerBlock) : AmbientPowerAdmission()

    data class Refused(val block: AmbientPowerBlock) : AmbientPowerAdmission()

    val warning: AmbientPowerBlock? get() = (this as? AllowedWithWarning)?.block
    val refusal: AmbientPowerBlock? get() = (this as? Refused)?.block
    val permitted: Boolean get() = this !is Refused
}

/**
 * The battery rails for always-on listening.
 *
 * Ported from iOS's `AmbientPowerMonitor`, including the decisions it argues for
 * rather than only the thresholds. Android had none of this: the wake service
 * would happily keep a microphone open at 3%.
 *
 * Split from the service for the reason iOS split it — these are decisions with
 * wrong answers that ship silently. A rail written as the obvious comparison
 * refuses every window and the symptom is a feature that "sometimes doesn't
 * arm", which is close to unreportable.
 */
object AmbientPowerMonitor {

    /** The floor, named so the number appears once. */
    const val BATTERY_FLOOR: Float = 0.20f

    /**
     * Everything the decisions are made from.
     *
     * One value so a test can place a window on any battery it likes without
     * contriving device state.
     */
    data class Readings(
        val batterySaver: Boolean,
        /** 0..1, or negative when the level cannot be read. */
        val batteryLevel: Float,
        val isCharging: Boolean,
    ) {
        companion object {
            /** A phone with nothing to complain about. */
            val healthy = Readings(batterySaver = false, batteryLevel = 1f, isCharging = false)
        }
    }

    /**
     * Whether a window may open on these readings.
     *
     * **Battery Saver warns rather than refuses.** Refusing would make ambient
     * listening unusable for anyone who lives in Battery Saver — it is sticky,
     * often on for days — and the cost is the owner's to accept. So the window
     * opens and says so, for as long as it is open.
     *
     * **The floor still refuses.** A window opened at 8% is a microphone that
     * will outlive the phone, and unlike Battery Saver there is nothing useful
     * to say about it that anybody can act on while it runs.
     *
     * Refusal is checked first because it is the stronger answer: a phone both
     * in Battery Saver and below the floor is refused, not warned.
     *
     * **Charging is exempt from the floor, and that is a decision.** Read
     * literally, "below 20%" ends the window of a phone sitting on a charger at
     * 15% and climbing — for a feature whose premise is a phone nobody is
     * holding, which is very often a phone that is plugged in. The rail exists
     * to stop an armed microphone draining a battery towards nothing; a battery
     * that is filling is not that.
     *
     * **An unreadable level is allowed.** Android reports no level before the
     * first battery broadcast, and `-1 < 0.20` is true — a rail that trusts it
     * refuses everything, immediately and permanently. The safe direction for a
     * refusal is not to fire.
     */
    fun admit(readings: Readings): AmbientPowerAdmission {
        val known = readings.batteryLevel >= 0f
        if (known && !readings.isCharging && readings.batteryLevel < BATTERY_FLOOR) {
            return AmbientPowerAdmission.Refused(AmbientPowerBlock.BatteryLow)
        }
        if (readings.batterySaver) {
            return AmbientPowerAdmission.AllowedWithWarning(AmbientPowerBlock.BatterySaver)
        }
        return AmbientPowerAdmission.Allowed
    }

    /**
     * Whether an already-open window must now close.
     *
     * Only the floor closes one. Battery Saver switching on mid-window is
     * carried as a warning rather than ending a window somebody is speaking
     * into — interrupting a sentence to report a setting is worse than the
     * battery it saves.
     */
    fun mustClose(readings: Readings): AmbientPowerBlock? {
        val known = readings.batteryLevel >= 0f
        return if (known && !readings.isCharging && readings.batteryLevel < BATTERY_FLOOR) {
            AmbientPowerBlock.BatteryLow
        } else {
            null
        }
    }

    /** Read the phone, tolerating everything it declines to tell us. */
    fun read(context: Context): Readings {
        val power = context.getSystemService(PowerManager::class.java)
        val saver = runCatching { power?.isPowerSaveMode == true }.getOrDefault(false)

        // A sticky broadcast, so this is a read rather than a subscription. A
        // null means nothing has been broadcast yet, which is exactly the
        // unknown the floor is written to tolerate.
        val status = runCatching {
            context.registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
        }.getOrNull()

        val level = status?.getIntExtra(BatteryManager.EXTRA_LEVEL, -1) ?: -1
        val scale = status?.getIntExtra(BatteryManager.EXTRA_SCALE, -1) ?: -1
        val fraction = if (level >= 0 && scale > 0) level.toFloat() / scale.toFloat() else -1f

        val plugged = status?.getIntExtra(BatteryManager.EXTRA_PLUGGED, 0) ?: 0
        val chargeStatus = status?.getIntExtra(BatteryManager.EXTRA_STATUS, -1) ?: -1
        val charging = plugged != 0 ||
            chargeStatus == BatteryManager.BATTERY_STATUS_CHARGING ||
            chargeStatus == BatteryManager.BATTERY_STATUS_FULL

        return Readings(batterySaver = saver, batteryLevel = fraction, isCharging = charging)
    }
}
