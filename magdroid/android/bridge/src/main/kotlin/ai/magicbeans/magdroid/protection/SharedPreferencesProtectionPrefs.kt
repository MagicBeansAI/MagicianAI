package ai.magicbeans.magdroid.protection

import android.content.Context

/** The production [ProtectionPrefs]: `magdroid.protection`, mode-private. */
class SharedPreferencesProtectionPrefs(context: Context) : ProtectionPrefs {

    private val store = context.applicationContext
        .getSharedPreferences("magdroid.protection", Context.MODE_PRIVATE)

    override fun packagesOrNull(): Set<String>? = store.getStringSet(PACKAGES, null)

    override fun writePackages(packages: Set<String>) {
        store.edit().putStringSet(PACKAGES, packages).apply()
    }

    override fun seeded(): Boolean = store.getBoolean(SEEDED, false)

    override fun markSeeded() {
        store.edit().putBoolean(SEEDED, true).apply()
    }

    private companion object {
        const val PACKAGES = "packages"
        const val SEEDED = "seeded"
    }
}
