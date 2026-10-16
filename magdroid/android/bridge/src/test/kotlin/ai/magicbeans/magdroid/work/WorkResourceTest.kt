package ai.magicbeans.magdroid.work

import ai.magicbeans.magdroid.access.MagicianConnectionSnapshot
import io.ktor.client.HttpClient
import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.*
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.coroutines.test.*
import org.junit.Assert.*
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class WorkResourceTest {
    @Test fun programRosterBoundsOverviewRequestsWithoutDroppingMembers() = runBlocking {
        Dispatchers.setMain(Dispatchers.Unconfined)
        val active = java.util.concurrent.atomic.AtomicInteger()
        val maximum = java.util.concurrent.atomic.AtomicInteger()
        val started = java.util.concurrent.atomic.AtomicInteger()
        val release = CompletableDeferred<Unit>()
        val resources = List(12) {
            WorkResource({ WorkRepository(MagicianConnectionSnapshot("https://work.test", emptyMap()), HttpClient(MockEngine {
                val now = active.incrementAndGet(); maximum.updateAndGet { maxOf(it, now) }; started.incrementAndGet()
                try { release.await(); respond("{\"context\":{\"program\":{\"relative_path\":\"program.md\"}}}", HttpStatusCode.OK, headersOf(HttpHeaders.ContentType, "application/json")) }
                finally { active.decrementAndGet() }
            })) }, { true }, emptyFlow())
        }
        try {
            resources.forEachIndexed { index, resource -> resource.load("/api/magician/v2/agents/member-$index/harness-overview") }
            withTimeout(5000) { while (started.get() < 4) delay(1) }
            assertEquals(4, started.get()); release.complete(Unit)
            withTimeout(5000) { while (resources.any { it.state.value.loading }) delay(1) }
            assertEquals(4, maximum.get()); assertEquals(12, started.get())
            assertTrue(resources.all { (it.state.value.value as? kotlinx.serialization.json.JsonObject)?.get("context") != null })
        } finally { release.complete(Unit); resources.forEach { it.close() }; Dispatchers.resetMain() }
    }

    @Test fun crewLoadRetainsHeaderOnlyVersionForTheNextWrite() = runTest {
        Dispatchers.setMain(UnconfinedTestDispatcher(testScheduler))
        try {
            val engine = MockEngine { request ->
                if (request.method == HttpMethod.Patch) assertEquals("header-version", request.headers[HttpHeaders.IfMatch])
                respond("""{"definition":{"agent_id":"writer","name":"Writer"}}""", HttpStatusCode.OK,
                    headersOf(HttpHeaders.ContentType to listOf("application/json"), HttpHeaders.ETag to listOf("header-version")))
            }
            val resource = WorkResource({ WorkRepository(MagicianConnectionSnapshot("https://work.test", emptyMap()), HttpClient(engine)) }, { true }, emptyFlow())
            try {
                resource.load("/api/magician/v2/agents/writer")
                withContext(Dispatchers.Default) { while (resource.state.value.loading) delay(1) }
                val value = resource.state.value.value as kotlinx.serialization.json.JsonObject
                assertEquals("header-version", value.text("etag"))
                assertNotNull(resource.perform("/api/magician/v2/agents/writer", resource.state.value.identity, WorkMethod.Patch, kotlinx.serialization.json.JsonObject(emptyMap()), value.text("etag")))
            } finally { resource.close() }
        } finally { Dispatchers.resetMain() }
    }
    @Test fun latest_filter_loads_after_an_in_flight_write_succeeds_or_fails() = runTest {
        Dispatchers.setMain(UnconfinedTestDispatcher(testScheduler))
        try {
            for (status in listOf(HttpStatusCode.OK, HttpStatusCode.Conflict)) {
                val started = CompletableDeferred<Unit>()
                val release = CompletableDeferred<Unit>()
                val engine = MockEngine { request ->
                    if (request.method == HttpMethod.Post) {
                        started.complete(Unit); release.await()
                        respond("{}", status, headersOf(HttpHeaders.ContentType, "application/json"))
                    } else respond("""{"filter":"${request.url.parameters["q"].orEmpty()}"}""",
                        HttpStatusCode.OK, headersOf(HttpHeaders.ContentType, "application/json"))
                }
                val resource = WorkResource({ WorkRepository(MagicianConnectionSnapshot("https://work.test", emptyMap()), HttpClient(engine)) }, { true }, emptyFlow())
                try {
                    resource.load("/api/magician/v2/relentless/catalog?q=original")
                    withContext(Dispatchers.Default) { while (resource.state.value.loading) delay(1) }
                    assertEquals("original", (resource.state.value.value as kotlinx.serialization.json.JsonObject).text("filter"))
                    val identity = resource.state.value.identity
                    val write = async { resource.perform("/api/magician/v2/relentless/runs/run/cancel", identity) }
                    started.await()
                    assertTrue(resource.state.value.busy)
                    resource.load("/api/magician/v2/relentless/catalog?q=intermediate")
                    resource.load("/api/magician/v2/relentless/catalog?q=latest")
                    resource.refresh()
                    release.complete(Unit)
                    assertEquals(status == HttpStatusCode.OK, write.await() != null)
                    withContext(Dispatchers.Default) { while (resource.state.value.loading) delay(1) }
                    assertEquals("latest", (resource.state.value.value as kotlinx.serialization.json.JsonObject).text("filter"))
                    assertTrue(resource.state.value.identity > identity)
                    assertFalse(resource.state.value.busy)
                    assertEquals(status != HttpStatusCode.OK, resource.state.value.error != null)
                } finally { resource.close() }
            }
        } finally { Dispatchers.resetMain() }
    }
}
