package ai.magicbeans.magdroid.tasks

import java.io.File
import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The monitor wire contract, against the canonical fixtures.
 *
 * These are the same files under `magician/tests/fixtures/monitors` that the
 * Rust `monitor_contract_fixtures` suite and the web `monitorContract.test.ts`
 * load. Until now no mobile client read them, so the contract was pinned on two
 * of its four consumers and this one was only ever checked by hand.
 *
 * The reason to assert against shipped fixtures rather than JSON written here
 * is the failure they catch: a field renamed on the wire still decodes into a
 * default, silently, and a test carrying its own copy of the old spelling stays
 * green while the client reads nothing. That is exactly how twenty-one tutor
 * fields decoded as absent for the life of that feature.
 */
class MonitorContractFixtureTest {

    private val json = Json { ignoreUnknownKeys = true; isLenient = true }

    private fun fixture(name: String): String = File(fixtureRoot(), name).readText()

    @Test
    fun `the canonical spec decodes whole`() {
        val spec = json.decodeFromString(MonitorSpec.serializer(), fixture("monitor_spec_v1.json"))

        assertEquals(1, spec.schemaVersion)
        assertTrue(spec.objective.startsWith("Watch the Acme Robotics pricing page"))
        assertEquals(
            listOf("acme robotics pricing", "acme robotics plan tiers"),
            spec.querySeeds,
        )
        assertEquals(listOf("https://acme-robotics.example/pricing"), spec.sources.urls)
        assertEquals(listOf("acme-robotics.example"), spec.sources.domains)
        assertTrue(spec.sources.authenticatedSources.isEmpty())
        assertEquals(listOf("pricing tables", "plan tiers", "usage limits"), spec.includeRules)
        assertEquals(listOf("blog posts", "career pages"), spec.excludeRules)
        assertEquals("balanced", spec.matchMode)
        assertEquals("material_changes", spec.notificationPolicy)
        assertFalse(spec.notifyInitialBaseline)
    }

    @Test
    fun `a list page decodes with its paging fields`() {
        val page = json.decodeFromString(
            MonitorListPage.serializer(),
            fixture("monitor_list_page_v1.json"),
        )
        assertTrue("the fixture page should carry monitors", page.items.isNotEmpty())
        val first = page.items.first()
        assertTrue(first.taskId.isNotBlank())
        assertTrue(first.objective.isNotBlank())
        // Every summary field the list rows read. A default here is a row that
        // renders "never ran" for a monitor that has been running for weeks.
        assertTrue(first.cadenceSummary.isNotBlank())
        assertTrue(first.state.isNotBlank())
        assertTrue(first.lastRunStatus.isNotBlank())
        assertTrue(first.health.isNotBlank())
    }

    /**
     * All three run outcomes. `degraded` is the one worth having: a partial
     * scan that decoded as a clean one would report a monitor as healthy when
     * it could not reach half its sources.
     */
    @Test
    fun `every run result shape decodes`() {
        val changed = run("monitor_run_result_v1_changed.json")
        assertEquals("changed", changed.status)
        assertTrue(changed.completeScan)
        assertEquals(12, changed.counts.scanned)
        assertEquals(1, changed.counts.new)
        assertEquals(1, changed.counts.updated)
        assertEquals(10, changed.counts.unchanged)
        assertTrue(changed.findings.isNotEmpty())
        assertNotNull(changed.changeFingerprint)

        val finding = changed.findings.first()
        assertTrue(finding.stableKey.isNotBlank())
        assertTrue(finding.title.isNotBlank())
        assertTrue(finding.whyItMatters.isNotBlank())
        assertTrue(finding.evidence.isNotEmpty())
        assertTrue(finding.evidence.first().value.isNotBlank())

        val unchanged = run("monitor_run_result_v1_unchanged.json")
        assertEquals("unchanged", unchanged.status)

        val degraded = run("monitor_run_result_v1_degraded.json")
        assertEquals("degraded", degraded.status)
        // A degraded run is the one that must not read as complete.
        assertFalse(degraded.completeScan)
        assertTrue(
            "a degraded run should name a source that did not come back",
            degraded.sourceOutcomes.any { it.status != "ok" || !it.complete },
        )
    }

    @Test
    fun `an update detail decodes with its notification block`() {
        val update = json.decodeFromString(
            MonitorUpdate.serializer(),
            fixture("monitor_update_detail_v1.json"),
        )
        assertTrue(update.updateId.isNotBlank())
        assertTrue(update.monitorTaskId.isNotBlank())
        assertTrue(update.executionId.isNotBlank())
        assertTrue(update.headline.isNotBlank())
        assertTrue(update.status.isNotBlank())
        // The notification block decides whether the owner was told. Defaulting
        // it silently would show "not notified" against an update that pushed.
        assertTrue(update.notification.policy.isNotBlank())
        assertTrue(update.notification.dedupeKey.isNotBlank())
    }

    /**
     * The golden scenarios describe runs step by step. This client does not
     * replay them — that is the server's suite — but the specs inside them are
     * the same contract, and a scenario that stops decoding here means the
     * shape moved under all four consumers.
     */
    @Test
    fun `every golden scenario carries a decodable policy`() {
        val scenarios = File(fixtureRoot(), "golden")
            .listFiles { file -> file.extension == "json" }
            ?.sortedBy { it.name }
            .orEmpty()
        assertTrue("no golden scenarios found — did the directory move?", scenarios.isNotEmpty())

        scenarios.forEach { file ->
            val root = json.parseToJsonElement(file.readText())
            val obj = root as? kotlinx.serialization.json.JsonObject
                ?: error("${file.name}: not an object")
            assertNotNull("${file.name}: names itself", obj["name"])
            assertNotNull("${file.name}: declares a notification policy", obj["notification_policy"])
            assertNotNull("${file.name}: has steps", obj["steps"])
        }
    }

    /**
     * Walk up to the repository root. A unit test's working directory is the
     * module, and hard-coding the number of parents breaks the moment it moves.
     */
    private fun fixtureRoot(): File {
        var directory: File? = File(System.getProperty("user.dir") ?: ".").absoluteFile
        while (directory != null) {
            val candidate = File(directory, "magician/tests/fixtures/monitors")
            if (candidate.isDirectory) return candidate
            directory = directory.parentFile
        }
        error("could not locate the monitor fixtures from ${System.getProperty("user.dir")}")
    }

    private fun run(name: String) =
        json.decodeFromString(MonitorRun.serializer(), fixture(name))
}
