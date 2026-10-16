package ai.magicbeans.magdroid.vibedev

import ai.magicbeans.magdroid.access.MagicianIdentity
import java.io.File
import java.nio.file.Files
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class VibeDevCacheTest {
    private fun cache(): Pair<VibeDevCache, File> {
        val dir = Files.createTempDirectory("vibedev-cache").toFile()
        dir.deleteOnExit()
        return VibeDevCache(File(dir, "machine")) to dir
    }

    @Test
    fun `now and run reads round-trip`() {
        val (store, _) = cache()
        val snapshot = VibeDevNowSnapshot(
            runs = listOf(
                VibeDevLiveRun("r1", "r1_live", "p1", "Shop", "Search", stateWire = "needs_you",
                    startedAt = "2026-10-13T09:12:00Z", costUsd = 0.5, approvalId = "ccp-1", lane = "API"),
                VibeDevLiveRun("r2", "r2", "p1", "Shop", "Form"),
            ),
            projects = listOf(
                VibeDevProject("p1", "Shop", trust = "trusted", crews = listOf(
                    VibeDevCrew("c1", "Goal", status = "running", lanes = listOf(VibeDevCrewLane("API", status = "done"))),
                )),
            ),
        )
        assertNull(store.loadNow())
        store.saveNow(snapshot, 1234)
        val loaded = store.loadNow()!!
        assertEquals(1234, loaded.savedAtMs)
        assertEquals(snapshot, loaded.value)
        assertEquals(VibeDevRunState.Working, loaded.value.runs[1].state)

        val turns = VibeDevRunTurns("r1", "p1", "Shop", "Search", "running", trusted = true, branch = "vibedev/x",
            turns = listOf(VibeDevRunTurn("r1", "Make it fast", engine = "codex", costUsd = 0.61, filesChanged = 2)))
        store.saveRun(turns, 99)
        assertEquals(turns, store.loadRun("r1")!!.value)
        assertNull(store.loadRun("other"))
    }

    @Test
    fun `a corrupt file reads as nothing`() {
        val (store, dir) = cache()
        store.saveNow(VibeDevNowSnapshot(), 1)
        File(File(dir, "machine"), "now.json").writeText("{not json")
        assertNull(store.loadNow())
    }

    @Test
    fun `only the newest run files are kept`() {
        val (store, dir) = cache()
        repeat(VibeDevCache.MAX_RUNS + 5) { index -> store.saveRun(VibeDevRunTurns("run$index"), index.toLong()) }
        val files = File(dir, "machine").listFiles { file -> file.name.startsWith("run-") }!!
        assertEquals(VibeDevCache.MAX_RUNS, files.size)
        assertTrue(File(dir, "machine").listFiles()!!.none { it.name.endsWith(".tmp") })
    }

    @Test
    fun `each machine and scope keeps its own folder`() {
        val home = MagicianIdentity("https://home.example.com", "owner", "default")
        val key = VibeDevCache.key(home)
        assertEquals(16, key.length)
        assertEquals(key, VibeDevCache.key(home.copy(origin = "https://home.example.com/")))
        assertNotEquals(key, VibeDevCache.key(home.copy(origin = "https://work.example.com")))
        assertNotEquals(key, VibeDevCache.key(home.copy(principal = "guest")))
        assertNotEquals(key, VibeDevCache.key(home.copy(workspace = "side")))
    }
}
