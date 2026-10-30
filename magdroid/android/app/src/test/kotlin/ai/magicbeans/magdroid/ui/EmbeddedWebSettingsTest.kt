package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.access.PairedMachine
import ai.magicbeans.magdroid.access.RemoteMachine
import ai.magicbeans.magdroid.access.RemoteMachineRegistry
import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Settings → Magican settings: which page it opens, and what stays inside it. */
class EmbeddedWebSettingsTest {
    private val origin = "https://ui.studio.example"
    private val settings = "$origin/settings?embed=1&host=android"

    private val active = PairedMachine("https://api.studio.example", "api.studio.example", active = true)

    @Test
    fun `the active machine's registry ui origin wins over the connection origin`() {
        val registry = RemoteMachineRegistry(
            self = "studio",
            machines = listOf(RemoteMachine("studio", "https://api.studio.example", "$origin/", isSelf = true)),
        )
        assertEquals(origin, webSettingsOrigin(listOf(active), registry, "https://api.studio.example"))
    }

    @Test
    fun `the machine that answered stands in when the registry does not name the active origin`() {
        val registry = RemoteMachineRegistry(
            machines = listOf(RemoteMachine("elsewhere", "https://api.other.example", origin, isSelf = true)),
        )
        assertEquals(origin, webSettingsOrigin(listOf(active), registry, "https://api.studio.example"))
    }

    @Test
    fun `without a registry the connection's own origin is used`() {
        assertEquals("https://api.studio.example", webSettingsOrigin(listOf(active), null, "https://api.studio.example/"))
        assertEquals("http://192.168.1.20:8443", webSettingsOrigin(emptyList(), RemoteMachineRegistry(), "http://192.168.1.20:8443"))
        assertNull(webSettingsOrigin(emptyList(), null, null))
        assertNull(webSettingsOrigin(emptyList(), null, "ftp://files.example"))
    }

    @Test
    fun `the page is the embedded android settings route`() {
        assertEquals(settings, webSettingsUrl("$origin/"))
    }

    @Test
    fun `settings and sign-in pages on the machine stay in the page`() {
        listOf(
            settings,
            "$origin/settings",
            "$origin/settings/",
            "$origin/settings#budgets",
            "$origin/login?redirectTo=%2Fsettings",
            "$origin/cdn-cgi/access/authorized?token=x",
            "https://ui.studio.example:443/settings",
        ).forEach { target ->
            assertEquals(target, WebSettingsNavigation.Stay, webSettingsNavigation(target, origin, settings, isRedirect = false))
        }
    }

    @Test
    fun `other app pages and external links open in the browser`() {
        listOf(
            "$origin/",
            "$origin/tasks",
            "$origin/settingsfoo",
            "https://ui.studio.example:8443/settings",
            "http://ui.studio.example/settings",
            "https://github.com/magicbeans",
            "mailto:owner@example.com",
        ).forEach { target ->
            assertEquals(target, WebSettingsNavigation.External, webSettingsNavigation(target, origin, settings, isRedirect = false))
        }
    }

    @Test
    fun `a sign-in on another origin finishes in the page`() {
        // Cloudflare Access redirects to its own login page…
        assertEquals(
            WebSettingsNavigation.Stay,
            webSettingsNavigation("https://team.cloudflareaccess.com/cdn-cgi/access/login", origin, settings, isRedirect = true),
        )
        // …whose identity-provider buttons are ordinary links.
        assertEquals(
            WebSettingsNavigation.Stay,
            webSettingsNavigation(
                "https://accounts.google.com/o/oauth2/auth",
                origin,
                "https://team.cloudflareaccess.com/cdn-cgi/access/login",
                isRedirect = false,
            ),
        )
    }

    @Test
    fun `a client-side route away from settings is sent to the browser`() {
        assertTrue(webSettingsLeavesInPlace("$origin/tasks", settings, origin))
        assertFalse(webSettingsLeavesInPlace("$origin/settings#voice", settings, origin))
        // Leaving the sign-in page (its own redirect) is not a link the owner followed.
        assertFalse(webSettingsLeavesInPlace("$origin/", "$origin/login", origin))
        assertFalse(webSettingsLeavesInPlace("$origin/tasks", null, origin))
        assertFalse(webSettingsLeavesInPlace("https://github.com/x", settings, origin))
    }

    @Test
    fun `the page never sends the phone's credentials or exposes native code`() {
        val root = File(requireNotNull(System.getProperty("user.dir")))
        val project = generateSequence(root) { it.parentFile }
            .firstOrNull { File(it, "app/src/main/kotlin").isDirectory }
            ?: File(root, "magdroid/android")
        val page = File(project, "app/src/main/kotlin/ai/magicbeans/magdroid/ui/EmbeddedWebSettingsPage.kt").readText()
        assertFalse(page.contains("addJavascriptInterface"))
        assertFalse(page.contains("MagicianAccess.headers"))
        assertFalse(page.contains("additionalHttpHeaders"))
        assertTrue(page.contains("settings.allowFileAccess = false"))
        assertTrue(page.contains("CookieManager.getInstance().flush()"))
    }
}
