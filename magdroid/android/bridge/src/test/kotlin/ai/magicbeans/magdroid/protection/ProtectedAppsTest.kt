package ai.magicbeans.magdroid.protection

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

private class FakePrefs : ProtectionPrefs {
    var packages: Set<String>? = null
    var seededFlag = false
    override fun packagesOrNull(): Set<String>? = packages
    override fun writePackages(packages: Set<String>) {
        this.packages = packages
    }
    override fun seeded(): Boolean = seededFlag
    override fun markSeeded() {
        seededFlag = true
    }
}

class ProtectedAppsTest {

    @Test
    fun first_run_seeds_the_authenticators() {
        val prefs = FakePrefs()
        val apps = ProtectedApps(prefs)
        assertTrue(prefs.seededFlag)
        assertEquals(ProtectedApps.SEED, apps.packages.value)
        assertTrue(apps.isProtected("com.beemdevelopment.aegis"))
    }

    /**
     * The seed-once contract: an owner who unprotects everything has made a
     * decision, and a restart must not overrule it by re-seeding.
     */
    @Test
    fun an_emptied_list_stays_empty_across_restarts() {
        val prefs = FakePrefs()
        val first = ProtectedApps(prefs)
        ProtectedApps.SEED.forEach { first.setProtected(it, false) }
        assertEquals(emptySet<String>(), first.packages.value)

        val restarted = ProtectedApps(prefs)
        assertEquals(emptySet<String>(), restarted.packages.value)
        assertFalse(restarted.isProtected("com.beemdevelopment.aegis"))
    }

    @Test
    fun toggling_persists_through_the_prefs() {
        val prefs = FakePrefs()
        val apps = ProtectedApps(prefs)
        apps.setProtected("com.bank.app", true)
        assertTrue(apps.isProtected("com.bank.app"))
        assertTrue(prefs.packages!!.contains("com.bank.app"))

        apps.setProtected("com.bank.app", false)
        assertFalse(apps.isProtected("com.bank.app"))
        assertFalse(prefs.packages!!.contains("com.bank.app"))
    }

    @Test
    fun a_null_foreground_is_never_protected() {
        // No active window means nothing to protect — the gate must not turn
        // "unknown foreground" into a blanket refusal of every tool.
        val apps = ProtectedApps(FakePrefs())
        assertFalse(apps.isProtected(null))
    }
}
