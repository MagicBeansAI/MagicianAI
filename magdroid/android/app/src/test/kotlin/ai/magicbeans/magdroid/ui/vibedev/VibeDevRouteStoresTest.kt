package ai.magicbeans.magdroid.ui.vibedev

import ai.magicbeans.magdroid.vibedev.VibeDevChangedFile
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotSame
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

class VibeDevRouteStoresTest {
    private class Probe : ViewModel() {
        var cleared = false
        override fun onCleared() { cleared = true }
    }

    private fun VibeDevChrome.probe(route: VibeDevRoute): Probe =
        ViewModelProvider(
            owner(route)!!,
            object : ViewModelProvider.Factory {
                @Suppress("UNCHECKED_CAST")
                override fun <T : ViewModel> create(modelClass: Class<T>): T = Probe() as T
            },
        )[Probe::class.java]

    private val run = VibeDevRoute.Run("r1", "Search")
    private val diff = VibeDevRoute.Diff("r1", null, listOf(VibeDevChangedFile("a.kt")), "a.kt")

    @Test
    fun `a popped route's view model is cleared and the rest are kept`() {
        val stores = VibeDevRouteStores()
        val chrome = VibeDevChrome(stores)
        val home = chrome.probe(VibeDevRoute.Home)
        chrome.push(run)
        val runModel = chrome.probe(run)
        chrome.push(diff)
        val diffModel = chrome.probe(diff)

        assertTrue(chrome.back())
        assertTrue("the diff and its patches go", diffModel.cleared)
        assertFalse(runModel.cleared)
        assertFalse(home.cleared)
        assertSame("the run screen gets its model back", runModel, chrome.probe(run))
        assertEquals(setOf("home", "run:r1"), stores.keys)
    }

    @Test
    fun `a retitled run keeps its view model`() {
        val chrome = VibeDevChrome(VibeDevRouteStores())
        chrome.push(run)
        val model = chrome.probe(run)
        chrome.retitle(run, "Search, faster")
        assertSame(model, chrome.probe(chrome.top))
        assertFalse(model.cleared)
    }

    @Test
    fun `reset keeps home, and a machine switch clears everything`() {
        val stores = VibeDevRouteStores()
        val chrome = VibeDevChrome(stores)
        val home = chrome.probe(VibeDevRoute.Home)
        chrome.push(run)
        val runModel = chrome.probe(run)

        chrome.reset()
        assertTrue(runModel.cleared)
        assertFalse(home.cleared)

        val before = chrome.connection
        chrome.resetConnection()
        assertTrue("Now read the old machine", home.cleared)
        assertEquals(before + 1, chrome.connection)
        assertNotSame(home, chrome.probe(VibeDevRoute.Home))
    }

    @Test
    fun `a rebuilt shell starts at home and drops the old routes`() {
        val stores = VibeDevRouteStores()
        val first = VibeDevChrome(stores)
        first.push(run)
        val runModel = first.probe(run)
        val home = first.probe(VibeDevRoute.Home)
        // The activity was recreated: the stores survive, the stack does not.
        VibeDevChrome(stores)
        assertTrue(runModel.cleared)
        assertFalse(home.cleared)
    }
}
