package ai.magicbeans.magdroid.work

import ai.magicbeans.magdroid.access.MagicianAccess
import android.content.Context
import io.ktor.http.HttpMethod
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject

import kotlinx.serialization.json.buildJsonObject

private val workProgramReadGate = Semaphore(4)

data class WorkResourceState(
    val identity: Long = 0,
    val value: JsonElement? = null,
    val loading: Boolean = false,
    val busy: Boolean = false,
    val error: String? = null,
    val notice: String? = null,
)

enum class WorkMethod { Get, Post, Patch, Put }

/** Mounted native resource with cancellation, connection and late-response guards. */
class WorkResource(
    private val clientFactory: () -> WorkRepository,
    private val currentConnection: (WorkRepository) -> Boolean,
    private val connectionChanges: kotlinx.coroutines.flow.Flow<String>,
) : AutoCloseable {
    constructor(context: Context,
                clientFactory: () -> WorkRepository = { WorkRepository.connected(context) },
                currentConnection: (WorkRepository) -> Boolean = { it.isCurrentConnection(context) },
                connectionChanges: kotlinx.coroutines.flow.Flow<String> = MagicianAccess.activeMachine)
        : this(clientFactory, currentConnection, connectionChanges)
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val mutable = MutableStateFlow(WorkResourceState())
    val state: StateFlow<WorkResourceState> = mutable.asStateFlow()
    private var repository: WorkRepository? = null
    private var loadJob: Job? = null
    private var generation = 0L
    private var path = ""
    private var pendingPath: String? = null
    private var invalidated = false
    init {
        scope.launch {
            connectionChanges.collect {
                generation++; loadJob?.cancel(); repository?.close(); repository = null
                pendingPath = null
                mutable.value = WorkResourceState(identity = mutable.value.identity + 1)
                invalidated = true
                reportError("Connection changed. Reopen this work view.")
            }
        }
    }
    fun load(path: String) {
        if (mutable.value.busy) { pendingPath = path; return }
        pendingPath = null
        if (invalidated || repository?.let(currentConnection) == false) {
            invalidated = true; generation++; loadJob?.cancel(); repository?.close(); repository = null
            mutable.value = WorkResourceState(identity = mutable.value.identity + 1, error = "Connection changed. Reopen this work view.")
            return
        }
        loadJob?.cancel(); val ticket = ++generation
        if (this.path != path) mutable.value = WorkResourceState(identity = mutable.value.identity + 1)
        this.path = path
        mutable.value = mutable.value.copy(loading = true, error = null)
        repository?.close()
        val client = try { clientFactory() }
        catch (error: Exception) { mutable.value = mutable.value.copy(loading = false, error = error.message); return }
        repository = client
        loadJob = scope.launch {
            try {
                val (payload, etag) = if (path.endsWith("/harness-overview")) workProgramReadGate.withPermit {
                    if (!currentConnection(client)) throw CancellationException("Connection changed")
                    client.request(path)
                } else client.request(path)
                val value = if (Regex("^/api/magician/v2/agents/[^/?]+$").matches(path) && payload is JsonObject && payload.text("etag").isNullOrBlank() && !etag.isNullOrBlank()) payload.crewSet(listOf("etag"), kotlinx.serialization.json.JsonPrimitive(etag)) else payload
                if (ticket == generation && currentConnection(client)) {
                    mutable.value = mutable.value.copy(value = value, loading = false)
                }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) {
                if (ticket == generation && currentConnection(client)) {
                    mutable.value = mutable.value.copy(loading = false, error = error.message ?: "Work could not be loaded.")
                }
            }
        }
    }
    suspend fun file(path: String, expectedIdentity: Long): ByteArray? {
        val client = repository
        if (mutable.value.busy || client == null || expectedIdentity != mutable.value.identity || !currentConnection(client)) return null
        val ticket = generation; mutable.value = mutable.value.copy(busy = true, error = null, notice = null)
        try {
            val bytes = client.file(path)
            return bytes.takeIf { ticket == generation && expectedIdentity == mutable.value.identity && currentConnection(client) }
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { if (ticket == generation) reportError(error.message ?: "Artifact could not be loaded."); return null }
        finally { finishRequest(ticket) }
    }
    fun refresh() { (pendingPath ?: path).takeIf { it.isNotEmpty() }?.let(::load) }
    fun reportError(message: String) { mutable.value = mutable.value.copy(error = message) }
    suspend fun perform(path: String, expectedIdentity: Long, method: WorkMethod = WorkMethod.Post,
                        body: JsonObject? = buildJsonObject {}, etag: String? = null, successNotice: String? = "Saved"): JsonElement? {
        val client = repository
        if (mutable.value.busy || expectedIdentity != mutable.value.identity || client == null || !currentConnection(client)) {
            reportError("Connection changed. Refresh before continuing."); return null
        }
        val ticket = generation
        mutable.value = mutable.value.copy(busy = true, error = null, notice = null)
        try {
            val httpMethod = when (method) { WorkMethod.Get -> HttpMethod.Get; WorkMethod.Post -> HttpMethod.Post; WorkMethod.Patch -> HttpMethod.Patch; WorkMethod.Put -> HttpMethod.Put }
            val result = client.request(path, httpMethod, body, etag).first
            if (ticket != generation || expectedIdentity != mutable.value.identity || !currentConnection(client)) return null
            mutable.value = mutable.value.copy(notice = if (method == WorkMethod.Get) null else successNotice)
            return result
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) {
            if (ticket == generation && currentConnection(client)) reportError(error.message ?: "Work could not be updated.")
            return null
        } finally { finishRequest(ticket) }
    }
    /**
     * A typed write under the same busy, identity and connection guards as
     * [perform]. Null when the view moved on; otherwise the call's own result
     * or failure, left unreported so the screen decides how a typed failure
     * (a save conflict) reads.
     */
    suspend fun <T> write(expectedIdentity: Long, successNotice: String? = "Saved", block: suspend (WorkRepository) -> T): Result<T>? {
        val client = repository
        if (mutable.value.busy || expectedIdentity != mutable.value.identity || client == null || !currentConnection(client)) {
            reportError("Connection changed. Refresh before continuing."); return null
        }
        val ticket = generation
        mutable.value = mutable.value.copy(busy = true, error = null, notice = null)
        try {
            val result = block(client)
            if (ticket != generation || expectedIdentity != mutable.value.identity || !currentConnection(client)) return null
            mutable.value = mutable.value.copy(notice = successNotice)
            return Result.success(result)
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) {
            if (ticket != generation || !currentConnection(client)) return null
            return Result.failure(error)
        } finally { finishRequest(ticket) }
    }
    private fun finishRequest(ticket: Long) {
        if (ticket != generation) return
        mutable.value = mutable.value.copy(busy = false)
        pendingPath?.let { next ->
            val outcome = mutable.value
            load(next)
            if (!invalidated) mutable.value = mutable.value.copy(error = outcome.error ?: mutable.value.error, notice = outcome.notice ?: mutable.value.notice)
        }
    }
    override fun close() { generation++; pendingPath = null; scope.cancel(); repository?.close() }
}
