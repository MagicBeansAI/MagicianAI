package ai.magicbeans.magdroid.protection

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * The apps an agent may not look inside.
 *
 * The device is the trust boundary's owner: a check anywhere else leaves the
 * wire carrying pixels that should never have been captured. Every tool call
 * consults this set at the single dispatch seam in `McpToolHandler` — observe
 * and act tools are refused while a protected app is foregrounded, and the
 * notification tools drop entries whose *source* is protected, because a
 * banking OTP arrives while any app at all is in the foreground.
 *
 * Storage is behind [ProtectionPrefs] rather than SharedPreferences directly
 * so the seed-once contract is testable on the JVM.
 */
interface ProtectionPrefs {
    fun packagesOrNull(): Set<String>?
    fun writePackages(packages: Set<String>)
    fun seeded(): Boolean
    fun markSeeded()
}

class ProtectedApps(private val prefs: ProtectionPrefs) {

    private val _packages = MutableStateFlow(loadOrSeed())
    val packages: StateFlow<Set<String>> = _packages.asStateFlow()

    private fun loadOrSeed(): Set<String> {
        if (!prefs.seeded()) {
            // Seed exactly once. The marker survives even if the owner later
            // unprotects everything — an emptied list is a decision, and
            // re-seeding over it would overrule the owner on every launch.
            prefs.writePackages(SEED)
            prefs.markSeeded()
            return SEED
        }
        return prefs.packagesOrNull() ?: emptySet()
    }

    fun isProtected(packageName: String?): Boolean =
        packageName != null && packageName in _packages.value

    fun setProtected(packageName: String, protected: Boolean) {
        val next =
            if (protected) _packages.value + packageName else _packages.value - packageName
        prefs.writePackages(next)
        _packages.value = next
    }

    companion object {
        /**
         * One instance per process — non-negotiable, and review-earned. The
         * first cut constructed one instance in the bridge service and
         * another in the Settings page: both wrapped the same preferences
         * file, but membership lives in each instance's in-memory
         * [StateFlow], so a toggle in Settings never reached the live gate
         * until the process died. Protecting a banking app and getting
         * nothing until the next reboot is the exact inverse of fail-closed.
         * `get(context)` is the only production constructor; the public
         * constructor exists for JVM tests.
         */
        @Volatile
        private var instance: ProtectedApps? = null

        fun get(context: android.content.Context): ProtectedApps =
            instance ?: synchronized(this) {
                instance ?: ProtectedApps(SharedPreferencesProtectionPrefs(context))
                    .also { instance = it }
            }

        /**
         * Authenticator apps, protected out of the box. The parent plan's risk
         * table names "screenshots of banking / 2FA leaving the device" as the
         * risk this phase controls; banking apps cannot be enumerated in
         * advance, but the second factor's home can be. Owner-managed after
         * the seed: any of these can be unprotected in Settings.
         */
        val SEED: Set<String> = setOf(
            "com.google.android.apps.authenticator2", // Google Authenticator
            "com.azure.authenticator", // Microsoft Authenticator
            "com.beemdevelopment.aegis", // Aegis
            "com.authy.authy", // Twilio Authy
            "com.duosecurity.duomobile", // Duo Mobile
            "org.fedorahosted.freeotp", // FreeOTP
            "org.shadowice.flocke.andotp", // andOTP
            "com.twofasapp", // 2FAS
            "com.onepassword.android", // 1Password
        )
    }
}
