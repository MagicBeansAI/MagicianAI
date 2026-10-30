package ai.magicbeans.magdroid.chat

import android.content.Context
import ai.magicbeans.magdroid.access.MagicianAccess
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.plugins.HttpTimeoutConfig
import io.ktor.client.plugins.timeout
import io.ktor.client.request.delete
import io.ktor.client.request.forms.formData
import io.ktor.client.request.forms.submitFormWithBinaryData
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.parameter
import io.ktor.client.request.patch
import io.ktor.client.request.post
import io.ktor.client.request.preparePost
import io.ktor.client.request.put
import io.ktor.client.request.setBody
import io.ktor.client.statement.bodyAsChannel
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.Headers
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.contentType
import ai.magicbeans.magdroid.net.CarriesFailure
import ai.magicbeans.magdroid.net.Failure
import ai.magicbeans.magdroid.net.FailureKind
import ai.magicbeans.magdroid.net.Failures
import io.ktor.http.isSuccess
import io.ktor.utils.io.readUTF8Line
import ai.magicbeans.magdroid.realtime.HubDelivery
import ai.magicbeans.magdroid.realtime.HubFilter
import ai.magicbeans.magdroid.realtime.RealtimeHub
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.channelFlow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.mapNotNull
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.CancellationException
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import java.net.ConnectException
import java.net.SocketTimeoutException
import java.net.URLEncoder
import java.net.UnknownHostException
import java.nio.channels.UnresolvedAddressException

/** Something the screen can show an owner, rather than a stack trace. */
/**
 * Why a chat call failed.
 *
 * Reads carry a classified [Failure]; the sentence-only constructor stays for
 * the refusals the server words itself.
 */
class ChatError(override val failure: Failure) :
    Exception(failure.headline), CarriesFailure {

    constructor(message: String, setupRequired: Boolean = false) : this(
        Failure(
            kind = FailureKind.Unknown,
            headline = message,
            detail = "",
            retryable = !setupRequired,
            setupRequired = setupRequired,
        ),
    )

    val setupRequired: Boolean get() = failure.setupRequired
}

/**
 * Talks to Magician's chat API.
 *
 * Kept free of Android UI types so it can be exercised without an emulator, and
 * free of state so the ViewModel owns what the screen shows. Every call carries
 * the Cloudflare Access headers and scope from [MagicianAccess] — the same
 * identity the bridge uses, because a phone should not authenticate two ways.
 */
class ChatRepository internal constructor(
    private val client: HttpClient,
    private val baseUrl: () -> String,
    private val headers: () -> Map<String, String>,
    private val scope: () -> Pair<String, String>,
    realtimeHub: RealtimeHub? = null,
    private val modeConnection: (() -> ai.magicbeans.magdroid.access.MagicianConnectionSnapshot?)? = null,
) {
    constructor(
        context: Context,
        modeConnection: () -> ai.magicbeans.magdroid.access.MagicianConnectionSnapshot? = { MagicianAccess.connectionSnapshot(context) },
    ) : this(
        client = chatHttpClient(),
        baseUrl = { MagicianAccess.baseUrl(context) },
        headers = { MagicianAccess.headers(context) },
        scope = { MagicianAccess.principal(context) to MagicianAccess.workspace(context) },
        realtimeHub = RealtimeHub.shared(context),
        modeConnection = modeConnection,
    )

    /** The app's one realtime connection; a private one only for tests built on a bare client. */
    private val hub: RealtimeHub by lazy { realtimeHub ?: RealtimeHub(client, baseUrl, headers) }

    private fun base(): String {
        return "${root()}/api/magician/v2"
    }

    /** Uses the same enrolled, scoped credential as ordinary chat. */
    suspend fun voiceRequest(path: String, body: JsonObject?): JsonObject {
        val workspace = scope().second
        val response = if (body == null) client.get("${base()}$path") { authorize(); parameter("workspace", workspace) }
        else client.post("${base()}$path") {
            authorize(); parameter("workspace", workspace); contentType(ContentType.Application.Json); setBody(body.toString())
        }
        if (!response.status.isSuccess()) throw failure(response.status, response.bodyAsText())
        return Json.parseToJsonElement(response.bodyAsText()).jsonObject
    }

    /** The existing owner-only mode endpoints; a null chat choice follows Settings. */
    suspend fun chatGodMode(sessionId: String, choice: Boolean? = null, update: Boolean = false): ChatGodModeState {
        val body = if (update) buildJsonObject {
            put("enabled", choice?.let { kotlinx.serialization.json.JsonPrimitive(it) } ?: kotlinx.serialization.json.JsonNull)
        } else null
        return chatJson.decodeFromJsonElement(ChatGodModeState.serializer(), executionModeRequest(
            "/chat/sessions/${encoded(sessionId)}/god-mode", body))
    }

    /** Sets this chat's auto-approve; `null` follows the Settings default. */
    suspend fun chatAutoApprove(sessionId: String, choice: Boolean?): ChatGodModeState {
        val body = buildJsonObject {
            put("auto_approve", choice?.let { kotlinx.serialization.json.JsonPrimitive(it) } ?: kotlinx.serialization.json.JsonNull)
        }
        return chatJson.decodeFromJsonElement(ChatGodModeState.serializer(), executionModeRequest(
            "/chat/sessions/${encoded(sessionId)}/god-mode", body))
    }

    suspend fun superGodMode(choice: SuperGodModeDuration? = null, update: Boolean = false): SuperGodModeState {
        val body = if (update) buildJsonObject {
            put("enabled", choice != null)
            choice?.let { put("duration", it.wire) }
        } else null
        return chatJson.decodeFromJsonElement(SuperGodModeState.serializer(), executionModeRequest(
            "/settings/super-god-mode", body))
    }

    private suspend fun executionModeRequest(path: String, body: JsonObject?): JsonObject {
        val snapshot = modeConnection?.invoke()
        if (modeConnection != null && snapshot == null) throw ChatError("Connect to your Magician in Settings.")
        val endpoint = snapshot?.baseUrl?.trimEnd('/')?.let { "$it/api/magician/v2" } ?: base()
        val authHeaders = snapshot?.headers ?: headers()
        val response = if (body == null) client.get("$endpoint$path") {
            authHeaders.forEach { (key,value) -> header(key,value) }
        } else client.put("$endpoint$path") {
            authHeaders.forEach { (key,value) -> header(key,value) }
            contentType(ContentType.Application.Json); setBody(body.toString())
        }
        if (!response.status.isSuccess()) throw failure(response.status, response.bodyAsText())
        return Json.parseToJsonElement(response.bodyAsText()).jsonObject
    }

    suspend fun queueSnapshot(sessionId: String): ChatQueueSnapshot {
        val body = voiceRequest("/chat/sessions/$sessionId/queue", null)
        return chatJson.decodeFromJsonElement(ChatQueueSnapshot.serializer(), body)
            .copy(turnState = ChatTurnStateReport.from(body, sessionId))
    }

    suspend fun enqueue(sessionId: String, request: SendMessageRequest): String {
        val body = chatRequestJson.encodeToJsonElement(SendMessageRequest.serializer(), request).jsonObject
        return voiceRequest("/chat/sessions/$sessionId/queue", body).getValue("queued").jsonObject.getValue("id").jsonPrimitive.content
    }

    suspend fun queueAction(sessionId: String, messageId: String, action: String) {
        voiceRequest("/chat/sessions/$sessionId/queue/$messageId/action", buildJsonObject { put("action", action) })
    }

    /** `DELETE /chat/sessions/{id}/queue`: drop every queued message. */
    suspend fun clearQueue(sessionId: String) {
        val response = client.delete("${base()}/chat/sessions/$sessionId/queue") { authorize() }
        if (!response.status.isSuccess()) throw failure(response.status, response.bodyAsText())
    }

    suspend fun removeQueued(sessionId: String, messageId: String) {
        val response = client.delete("${base()}/chat/sessions/$sessionId/queue/$messageId") { authorize() }
        if (!response.status.isSuccess()) throw failure(response.status, response.bodyAsText())
    }

    /**
     * Chat events that arrive without this device asking.
     *
     * The SSE a send opens carries that turn and closes with it, so anything
     * happening outside one — a message typed on the web, a background
     * execution finishing, activity produced between turns — was invisible.
     * These ride the app's one [RealtimeHub] connection, which reconnects
     * forever and resumes where the server can.
     *
     * A [ChatRealtimeEvent.Resync] means events may have been missed; the
     * screen re-reads its transcript by REST.
     */
    fun chatEvents(): Flow<ChatRealtimeEvent> = hub.subscribe(HubFilter.ALL).mapNotNull { delivery ->
        when (delivery) {
            is HubDelivery.Resync -> {
                // The badge's baseline is the Attention feed; a gap makes it
                // re-read that feed rather than trust a count that missed frames.
                PendingHitlTracker.resync()
                ChatRealtimeEvent.Resync(delivery.reason)
            }
            is HubDelivery.Event -> {
                // Every frame, before the chat filter: a HITL raised on
                // another surface still belongs in the badge, and the chat
                // parser only keeps what chat can draw.
                PendingHitlTracker.apply(delivery.event.root)
                val (principal, workspace) = scope()
                parseChatRealtimeEvent(delivery.event.root, principal, workspace)
            }
        }
    }

    /** Reopen the app's realtime connection, e.g. after pairing with another host. */
    fun restartRealtime() = hub.restart()

    private fun encoded(value: String): String = URLEncoder.encode(value, Charsets.UTF_8.name())

    private fun root(): String {
        val host = baseUrl().trimEnd('/')
        if (host.isEmpty()) throw ChatError("No Magician host configured yet.", setupRequired = true)
        return host
    }

    private fun io.ktor.client.request.HttpRequestBuilder.authorize() {
        headers().forEach { (name, value) -> header(name, value) }
    }

    /** Start a session, or resume the one already open. */
    suspend fun newSession(): String = createSession()

    /** Create a session in the requested UI thread and history lane. */
    suspend fun createSession(
        uiThreadId: String = "general",
        historyLane: ChatHistoryLane = ChatHistoryLane.Personal,
    ): String {
        val response = client.post("${base()}/chat/new") {
            authorize()
            parameter("ui_thread_id", uiThreadId)
            parameter("history_lane", historyLane.wire)
            contentType(ContentType.Application.Json)
            setBody("{}")
        }
        if (!response.status.isSuccess()) throw failure(response.status, response.bodyAsText())
        // The session comes back wrapped. Reading it as a bare session found no
        // id and refused every new conversation.
        val created = chatJson.decodeFromString(ChatSessionEnvelope.serializer(), response.bodyAsText())
        return created.session.identifier().ifBlank {
            throw ChatError("Magician created a session without an id.")
        }
    }

    suspend fun history(sessionId: String): List<ChatMessage> = conversation(sessionId).second

    /** Exact metadata matters for voice branches omitted from the history list. */
    suspend fun conversation(sessionId: String, target: OriginalAnswerLink? = null): Pair<SessionSummary, List<ChatMessage>> =
        conversationSnapshot(sessionId, target).let { it.summary to it.messages }

    /**
     * The conversation with the session's latest turn state.
     *
     * Read from the same response, so the transcript and the turn agree: a
     * turn the server is still running shows live over exactly this history.
     */
    suspend fun conversationSnapshot(sessionId: String, target: OriginalAnswerLink? = null): ChatConversation {
        val response = client.get("${base()}/chat/sessions/$sessionId") { authorize() }
        if (!response.status.isSuccess()) throw failure(response.status, response.bodyAsText())
        val root = chatJson.parseToJsonElement(response.bodyAsText()).jsonObject
        val detail = chatJson.decodeFromJsonElement(ChatSessionDetail.serializer(), root)
        val session = detail.session
        val metadata = SessionSummary(
            id = session.identifier(), title = session.title, uiThreadId = session.uiThreadId,
            status = session.status, isDefaultSession = session.isDefaultSession,
            internalVoice = session.internalVoice, historyLane = session.historyLane,
        )
        var messages = detail.messages
        var hasMore = resolveHistoryHasMore(detail.hasMore, messages.size)
        val cursors = mutableSetOf<String>()
        while (target != null && messages.none(target::matches) && hasMore) {
            val before = messages.firstOrNull()?.id ?: break
            check(cursors.add(before)) { "Could not locate the original answer. Please try again." }
            val page = olderMessages(sessionId, before, CHAT_EMBEDDED_HISTORY_LIMIT)
            messages = page.messages + messages
            hasMore = page.hasMore
        }
        val oldest = messages.firstOrNull()?.id
        return ChatConversation(
            summary = metadata,
            messages = coalesceTaskStatusMessages(
                messages.mapIndexed { index, dto -> dto.project(index).copy(linkedAnswerTarget = target?.matches(dto) == true) },
            ),
            turnState = ChatTurnStateReport.from(root, metadata.id.ifBlank { sessionId }),
            // Without a cursor there is nothing to page from.
            hasMore = hasMore && oldest != null,
            oldestMessageId = oldest,
        )
    }

    /** One page of messages older than [before], oldest first. */
    suspend fun olderMessages(sessionId: String, before: String, limit: Int = CHAT_OLDER_PAGE_LIMIT): OlderMessagesPage {
        val page = client.get("${base()}/chat/sessions/$sessionId/messages") {
            authorize(); parameter("limit", limit); parameter("before", before)
        }
        val body = page.bodyAsText()
        if (!page.status.isSuccess()) throw failure(page.status, body)
        return parseOlderMessagesPage(body, limit)
    }

    /** Remove the transcript while preserving the session itself. */
    suspend fun clearSession(sessionId: String) {
        val response = client.delete("${base()}/chat/sessions/$sessionId/messages") { authorize() }
        val body = response.bodyAsText()
        if (!response.status.isSuccess()) throw failure(response.status, body)
    }

    /** Move a session out of the active history lane without deleting it. */
    suspend fun archiveSession(sessionId: String) {
        setSessionArchived(sessionId, archived = true)
    }

    /** Archive or restore one session without changing its transcript. */
    suspend fun setSessionArchived(sessionId: String, archived: Boolean) {
        val response = client.patch("${base()}/chat/sessions/$sessionId") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(if (archived) """{"status":"archived"}""" else """{"status":"active"}""")
        }
        val body = response.bodyAsText()
        if (!response.status.isSuccess()) throw failure(response.status, body)
    }

    /** Permanently remove a session and all content owned by it. */
    suspend fun deleteSession(sessionId: String) {
        val response = client.delete("${base()}/chat/sessions/$sessionId") { authorize() }
        val body = response.bodyAsText()
        if (!response.status.isSuccess()) throw failure(response.status, body)
    }


    /**
     * One turn's activity rows, from the canonical projection.
     *
     * `GET /chat/sessions/{id}/turns/{turn}/events` is what the web's activity
     * card and iOS's steps section read — the same JSONL the realtime bus
     * feeds, served back as `{ events: [...] }`. Rows never ride the
     * `ExecutionPanelDelta` frame; this client once believed they did and its
     * "What happened" section never drew once.
     *
     * Empty on failure rather than throwing: a missing timeline should cost a
     * bubble its steps, not the conversation its transcript.
     */
    suspend fun turnActivity(sessionId: String, chatTurnId: String): List<ActivityRow> =
        chatTurnActivityRows(turnEvents(sessionId, chatTurnId))

    /** The raw events behind [turnActivity]; empty on failure, for the same reason. */
    private suspend fun turnEvents(sessionId: String, chatTurnId: String): List<JsonObject> = try {
        val response = client.get(
            "${base()}/chat/sessions/$sessionId/turns/$chatTurnId/events",
        ) {
            authorize()
            parameter("limit", TURN_EVENTS_LIMIT)
        }
        if (!response.status.isSuccess()) {
            emptyList()
        } else {
            val root = chatJson.parseToJsonElement(response.bodyAsText()) as? JsonObject
            (root?.get("events") as? kotlinx.serialization.json.JsonArray)
                ?.filterIsInstance<JsonObject>()
                .orEmpty()
        }
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (_: Throwable) {
        emptyList()
    }

    /** Read and losslessly reconstruct every authorized page behind a projected tool result. */
    suspend fun completeResult(hostSessionId: String?, row: ActivityRow): CompleteResult {
        val resultRef = row.resultRef?.takeIf { it.isNotBlank() }
            ?: throw ChatError("This complete result is no longer available.")
        val target = resolveCompleteResultReadTarget(
            owner = row.resultOwner,
            hostSessionId = hostSessionId,
            legacyTaskId = row.taskId,
            legacyExecutionId = row.executionId,
        ) ?: throw ChatError(
            "This complete result is no longer attached to a readable chat or task owner.",
        )
        return readCompleteResult(
            resultRef = resultRef,
            expectedContentHash = row.resultHash,
            readTarget = target,
        ) { requestBody ->
            val response = client.post("${root()}${target.path}") {
                authorize()
                contentType(ContentType.Application.Json)
                setBody(
                    chatRequestJson.encodeToString(
                        CompleteResultReadRequest.serializer(),
                        requestBody,
                    ),
                )
            }
            val body = response.bodyAsText()
            if (!response.status.isSuccess()) throw failure(response.status, body)
            val envelope = chatJson.decodeFromString(
                CompleteResultReadEnvelope.serializer(),
                body,
            )
            envelope.page ?: throw ChatError(
                envelope.error ?: "The complete result response was missing its page.",
            )
        }
    }

    /**
     * Live canonical activity for one turn.
     *
     * One REST read of `GET .../turns/{turn}/events` for what already
     * happened, then the turn's events off the app's [RealtimeHub] — not a held
     * NDJSON stream per turn. An event belongs to the turn when it names the
     * turn, or when it names a task or execution an earlier event of this turn
     * named: the server's chat fan-out maps a subscribed run's events to the
     * turn the same way, and those events do not carry the turn id themselves.
     *
     * The live subscription starts before the backfill so nothing falls between
     * them; the projection dedupes the overlap. A hub resync re-reads the REST
     * projection. Only changed row projections are published.
     */
    fun turnActivityStream(sessionId: String, chatTurnId: String): Flow<List<ActivityRow>> = channelFlow {
        val lock = Mutex()
        val events = mutableListOf<JsonObject>()
        var published: List<ActivityRow> = emptyList()
        val link = ChatTurnLink(chatTurnId)

        suspend fun absorb(batch: List<JsonObject>) {
            if (batch.isEmpty()) return
            lock.withLock {
                batch.forEach(link::learn)
                events.addAll(batch)
                val normalized = normalizedChatTurnEvents(events)
                events.clear()
                events.addAll(normalized)
                val rows = chatTurnActivityRows(events)
                if (rows != published) {
                    published = rows
                    send(rows)
                }
            }
        }

        launch(start = CoroutineStart.UNDISPATCHED) {
            hub.subscribe(HubFilter(predicate = link::owns)).collect { delivery ->
                when (delivery) {
                    is HubDelivery.Event -> absorb(listOf(delivery.event.root))
                    is HubDelivery.Resync -> absorb(turnEvents(sessionId, chatTurnId))
                }
            }
        }
        absorb(turnEvents(sessionId, chatTurnId))
    }

    /**
     * The agents and skills this session can address.
     *
     * Empty on failure rather than throwing: a missing catalog should cost the
     * `@` picker its entries, not block the conversation.
     */
    suspend fun referenceCatalog(sessionId: String): ReferenceCatalogResponse = runCatching {
        val response = client.get("${base()}/chat/sessions/$sessionId/reference-catalog") {
            authorize()
        }
        if (!response.status.isSuccess()) return ReferenceCatalogResponse()
        chatJson.decodeFromString(ReferenceCatalogResponse.serializer(), response.bodyAsText())
    }.getOrDefault(ReferenceCatalogResponse())

    /** An unavailable catalog is explicit; it never resets a saved choice. */
    suspend fun models(engine: String): ChatModelCatalog {
        require(engine in setOf("magician", "pi"))
        val response = client.get("${base()}/chat/models") {
            authorize()
            parameter("engine", engine)
        }
        check(response.status.isSuccess()) { "Model choices could not be loaded." }
        return chatJson.decodeFromString(ChatModelCatalog.serializer(), response.bodyAsText())
    }

    /**
     * The engine roster with the run engine it names (`current`, `run_model`),
     * for the `/run-engine` picker. Null on failure.
     */
    suspend fun engineRoster(): ChatHarnessRoster? = runCatching {
        val response = client.get("${base()}/plane/engines") { authorize() }
        if (!response.status.isSuccess()) return null
        chatJson.decodeFromString(ChatHarnessRoster.serializer(), response.bodyAsText())
    }.getOrNull()

    /**
     * One slash-command call (`/budget`, `/engine`, …): the response body, or a
     * [ChatError] carrying the server's own message (`message`, else `error`)
     * so a refusal or a missing route reads as the server put it.
     */
    suspend fun commandCall(method: String, path: String, body: JsonObject? = null): String {
        val url = "${base()}$path"
        val response = when (method) {
            "GET" -> client.get(url) { authorize() }
            "PUT" -> client.put(url) {
                authorize(); contentType(ContentType.Application.Json); setBody((body ?: JsonObject(emptyMap())).toString())
            }
            else -> client.post(url) {
                authorize(); contentType(ContentType.Application.Json); setBody((body ?: JsonObject(emptyMap())).toString())
            }
        }
        val text = response.bodyAsText()
        if (!response.status.isSuccess()) {
            throw ChatError(ComposerCommands.serverErrorMessage(response.status.value, text))
        }
        return text
    }

    suspend fun chatHarnesses(): List<ChatHarnessOption> = runCatching {
        val response = client.get("${base()}/plane/engines") { authorize() }
        if (!response.status.isSuccess()) return emptyList()
        chatJson.decodeFromString(ChatHarnessRoster.serializer(), response.bodyAsText())
            .engines.filter { it.installed }
    }.getOrDefault(emptyList())

    /** Sessions, newest first — what the history panel lists. */
    suspend fun workingSessionIds(ids: Set<String>): Set<String> {
        if (ids.isEmpty()) return emptySet()
        val response = client.get("${base()}/chat/activity") {
            authorize()
            parameter("session_ids", ids.joinToString(","))
            timeout { requestTimeoutMillis = 5_000 }
        }
        val body = response.bodyAsText()
        if (!response.status.isSuccess()) throw failure(response.status, body)
        return chatJson.decodeFromString(SessionActivityResponse.serializer(), body).workingSessionIds.toSet().intersect(ids)
    }

    suspend fun sessions(): List<SessionSummary> {
        val response = client.get("${base()}/chat/sessions") { authorize() }
        if (!response.status.isSuccess()) throw failure(response.status, response.bodyAsText())
        val body = response.bodyAsText()
        // The endpoint has returned both a bare array and an object over time.
        return runCatching {
            chatJson.decodeFromString(SessionList.serializer(), body).all()
        }.getOrElse {
            runCatching {
                chatJson.decodeFromString(
                    kotlinx.serialization.builtins.ListSerializer(SessionSummary.serializer()),
                    body,
                )
            }.getOrElse {
                throw ChatError(Failures.garbled("your chat sessions"))
            }
        }
    }

    /** One deterministic page for the Sessions history view. */
    suspend fun sessionsPage(
        historyLane: ChatHistoryLane? = null,
        uiThreadId: String? = null,
        limit: Int = 15,
        offset: Int = 0,
    ): HistoryPage<SessionSummary> {
        val response = client.get("${base()}/chat/sessions") {
            authorize()
            historyLane?.let { parameter("history_lane", it.wire) }
            uiThreadId?.let { parameter("ui_thread_id", it) }
            parameter("limit", limit)
            parameter("offset", offset)
        }
        if (!response.status.isSuccess()) throw failure(response.status, response.bodyAsText())
        val body = response.bodyAsText()
        val page = runCatching { chatJson.decodeFromString(SessionList.serializer(), body) }.getOrNull()
        if (page != null) {
            val items = page.all()
            return HistoryPage(
                items = items,
                total = page.total ?: items.size,
                limit = page.limit ?: limit,
                offset = page.offset ?: offset,
            )
        }
        val items = runCatching {
            chatJson.decodeFromString(
                kotlinx.serialization.builtins.ListSerializer(SessionSummary.serializer()),
                body,
            )
        }.getOrElse { throw ChatError("Session history response was invalid.") }
        return HistoryPage(items, items.size, limit, offset)
    }

    /**
     * Upgrade fallback for a device that predates last-session persistence.
     * Walk the Personal lane until an active row is found; treating only the
     * first page as the whole history can create a duplicate when that page is
     * filled with archived sessions.
     */
    suspend fun mostRecentActivePersonalSessionId(): String? {
        val limit = 50
        var offset = 0
        val seenSessionIds = mutableSetOf<String>()
        repeat(100) {
            val page = sessionsPage(
                historyLane = ChatHistoryLane.Personal,
                limit = limit,
                offset = offset,
            )
            mostRecentRestorableSession(page.items)?.let { return it }
            if (page.items.isEmpty()) return null
            val newIds = page.items.map(SessionSummary::identifier).filter(String::isNotBlank)
            val seenCount = seenSessionIds.size
            seenSessionIds.addAll(newIds)
            if (seenSessionIds.size == seenCount) {
                throw ChatError(Failures.garbled("your chat sessions"))
            }
            val nextOffset = offset + page.items.size
            // A legacy bare-array response has no authoritative total. A full
            // page may therefore have another page behind it; one harmless
            // empty-page probe is safer than manufacturing a duplicate chat.
            if (nextOffset >= page.total && page.items.size < limit) return null
            offset = nextOffset
        }
        throw ChatError(Failures.garbled("your chat sessions"))
    }

    /** One deterministic page for the Threads history view. */
    suspend fun threadsPage(
        historyLane: ChatHistoryLane,
        limit: Int = 15,
        offset: Int = 0,
    ): HistoryPage<UiThreadRecord> {
        val response = client.get("${base()}/ui-threads") {
            authorize()
            parameter("history_lane", historyLane.wire)
            parameter("limit", limit)
            parameter("offset", offset)
        }
        if (!response.status.isSuccess()) throw failure(response.status, response.bodyAsText())
        val decoded = runCatching {
            chatJson.decodeFromString(UiThreadListResponse.serializer(), response.bodyAsText())
        }.getOrElse { throw ChatError("Thread history response was invalid.") }
        return HistoryPage(decoded.threads, decoded.total, decoded.limit, decoded.offset)
    }

    /** Search sessions and threads together, independent of the browse filters. */
    suspend fun searchHistory(
        query: String,
        limit: Int = 15,
        offset: Int = 0,
    ): HistoryPage<HistorySearchItem> {
        val response = client.get("${base()}/history/search") {
            authorize()
            parameter("q", query)
            parameter("limit", limit)
            parameter("offset", offset)
        }
        if (!response.status.isSuccess()) throw failure(response.status, response.bodyAsText())
        val decoded = runCatching {
            chatJson.decodeFromString(HistorySearchResponse.serializer(), response.bodyAsText())
        }.getOrElse { throw ChatError("History search response was invalid.") }
        return HistoryPage(decoded.items, decoded.total, decoded.limit, decoded.offset)
    }

    /** Create a named thread; a session is created only when the thread opens. */
    suspend fun createThread(name: String): UiThreadRecord {
        val response = client.post("${base()}/ui-threads") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(buildJsonObject { put("name", name.trim()) }.toString())
        }
        if (!response.status.isSuccess()) throw failure(response.status, response.bodyAsText())
        return runCatching {
            chatJson.decodeFromString(UiThreadRecord.serializer(), response.bodyAsText())
        }.getOrElse { throw ChatError("Magician created a thread with an invalid response.") }
    }

    suspend fun setThreadArchived(threadId: String, archived: Boolean) {
        val response = client.patch("${base()}/ui-threads/${encoded(threadId)}") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(buildJsonObject { put("archived", archived) }.toString())
            // Archiving walks every session on the thread.
            timeout {
                requestTimeoutMillis = 600_000
                socketTimeoutMillis = 600_000
            }
        }
        if (!response.status.isSuccess()) throw failure(response.status, response.bodyAsText())
    }

    suspend fun deleteThread(threadId: String) {
        val response = client.delete("${base()}/ui-threads/${encoded(threadId)}") {
            authorize()
            timeout {
                requestTimeoutMillis = 600_000
                socketTimeoutMillis = 600_000
            }
        }
        if (!response.status.isSuccess()) throw failure(response.status, response.bodyAsText())
    }

    /**
     * Send a message and stream the reply.
     *
     * A cold `Flow` so the caller controls the lifetime: cancelling collection
     * cancels the request, which is what "stop generating" has to mean.
     */
    fun send(
        sessionId: String,
        text: String,
        /** Stable correlation owned by the caller before either stream starts. */
        chatTurnId: String,
        modelSelection: ChatModelSelection? = null,
        harnessEngine: String? = null,
        harnessModel: String? = null,
        attachmentIds: List<String> = emptyList(),
        /**
         * Which surface will draw what this turn produces.
         *
         * The backend routes tutor actions by this, so a turn that starts a
         * lesson has to name the canvas that is actually listening. Defaulted
         * for every ordinary turn, which draws nothing.
         */
        sourceSurface: String = "android",
        /** Whether the owner spoke this turn rather than typing it. */
        voiceOrigin: Boolean = false,
        /** `plan` or `accept_in_scope`. Null is ordinary Ask and is omitted. */
        mode: String? = null,
    ): Flow<ChatStreamEvent> = flow {
        val payload = chatRequestJson.encodeToString(
            SendMessageRequest.serializer(),
            SendMessageRequest(
                text = text,
                chatTurnId = chatTurnId,
                sourceSurface = sourceSurface,
                modelSelection = modelSelection,
                harnessEngine = harnessEngine,
                harnessModel = harnessModel.takeUnless { modelSelection != null },
                attachmentIds = attachmentIds,
                voiceOrigin = voiceOrigin,
                continueOnDisconnect = true,
                mode = mode,
            ),
        )
        client.preparePost("${base()}/chat/sessions/$sessionId/messages/stream") {
            authorize()
            // A pending human answer can outlive the ordinary five-minute
            // request budget. Collection cancellation still closes the stream.
            timeout {
                requestTimeoutMillis = HttpTimeoutConfig.INFINITE_TIMEOUT_MS
                socketTimeoutMillis = HttpTimeoutConfig.INFINITE_TIMEOUT_MS
            }
            contentType(ContentType.Application.Json)
            header("Accept", "text/event-stream")
            setBody(payload)
        }.execute { response ->
            if (!response.status.isSuccess()) throw failure(response.status, response.bodyAsText())

            val channel = response.bodyAsChannel()
            var event = "message"
            val data = StringBuilder()

            // The scoped response delivers frames before EOF and releases the
            // connection when collection is cancelled or a terminal frame lands.
            while (true) {
                val line = channel.readUTF8Line() ?: break
                when {
                    line.startsWith("event:") -> event = line.removePrefix("event:").trim()
                    line.startsWith("data:") -> data.append(line.removePrefix("data:").trim())
                    line.isBlank() -> {
                        if (data.isNotEmpty() || event != "message") {
                            val decoded = decodeStreamFrame(event, data.toString())
                            emit(decoded)
                            if (decoded is ChatStreamEvent.Done || decoded is ChatStreamEvent.Failed) return@execute
                        }
                        event = "message"
                        data.setLength(0)
                    }
                }
            }
            // A socket that ends without `done` must not leave the bubble
            // spinning. Marked dropped: the turn may still be running there.
            emit(ChatStreamEvent.Done(dropped = true))
        }
    }

    /**
     * Answer a paused execution.
     *
     * Returns null on success, or the reason it did not land. The endpoint
     * answers 200 with `accepted: false` for an escalation that was already
     * resolved, so the body decides, not the status code.
     */
    suspend fun respondToEscalation(
        correlationId: String,
        value: HitlResponseValue,
        inputType: String?,
        executionId: String?,
    ): String? {
        val response = client.post("${base()}/hitl/$correlationId/respond") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(
                chatRequestJson.encodeToString(
                    HitlRespondRequest.serializer(),
                    HitlRespondRequest(
                        value = value.toJson(),
                        inputType = inputType,
                        executionId = executionId,
                    ),
                ),
            )
        }
        val body = response.bodyAsText()
        if (!response.status.isSuccess()) return failure(response.status, body).message
        val result = runCatching {
            chatJson.decodeFromString(HitlRespondResponse.serializer(), body)
        }.getOrNull() ?: return null
        return if (result.accepted) null else result.refusal()
    }

    /**
     * Cancel the turn the session is running.
     *
     * Dropping the stream on this side only stops us listening: the turn keeps
     * running on the server, spending tokens on an answer nobody will see. Stop
     * has to reach the server to mean anything.
     */
    suspend fun cancelRun(sessionId: String) {
        client.delete("${base()}/chat/sessions/$sessionId/run") { authorize() }
    }

    /**
     * Upload one staged file and return the id a send refers to it by.
     *
     * Multipart with a single `file` part, which is what the handler reads. The
     * status is checked rather than only the body: an oversized file answers
     * 413, and treating that as "no id found" would report a generic failure
     * for something the owner can act on.
     */
    suspend fun uploadAttachment(
        sessionId: String,
        filename: String,
        mime: String,
        bytes: ByteArray,
    ): String {
        val response = client.submitFormWithBinaryData(
            url = "${base()}/chat/sessions/$sessionId/attachments",
            formData = formData {
                append(
                    "file",
                    bytes,
                    Headers.build {
                        append(HttpHeaders.ContentType, mime)
                        append(HttpHeaders.ContentDisposition, "filename=\"$filename\"")
                    },
                )
            },
        ) {
            authorize()
        }
        val body = response.bodyAsText()
        if (!response.status.isSuccess()) {
            throw ChatError(
                when (response.status.value) {
                    413 -> "That file is too large. The limit is 20 MB."
                    else -> failure(response.status, body).message ?: "The upload failed."
                },
            )
        }
        val uploaded = runCatching {
            chatJson.decodeFromString(AttachmentUploaded.serializer(), body)
        }.getOrNull()
        return uploaded?.attachmentId?.takeIf { it.isNotBlank() }
            ?: throw ChatError("Magician stored the file but did not return an id for it.")
    }

    /**
     * The Attention badge's inputs.
     *
     * Zeroes on any failure rather than throwing: a badge is an ambient hint,
     * and a chat session must not fail to load because a count could not be
     * fetched.
     */
    suspend fun attentionCounts(): AttentionCounts = runCatching {
        val response = client.get("${base()}/feed/attention") {
            authorize()
            parameter("limit", 1)
        }
        if (!response.status.isSuccess()) return AttentionCounts()
        chatJson.decodeFromString(AttentionFeed.serializer(), response.bodyAsText()).counts
    }.getOrDefault(AttentionCounts())

    /**
     * The account's voice preferences.
     *
     * Null when they cannot be read: a phone that cannot reach Magician should
     * keep using its own settings, not fall back to defaults and silently
     * unmute itself.
     */
    suspend fun mediaPreferences(): MediaPreferences? = try {
        val response = client.get("${base()}/media/preferences") { authorize() }
        if (!response.status.isSuccess()) null
        else chatJson.decodeFromString(MediaPreferences.serializer(), response.bodyAsText())
    } catch (cancel: kotlinx.coroutines.CancellationException) {
        throw cancel
    } catch (_: Exception) { null }

    suspend fun voicePersonaCatalog(): ai.magicbeans.magdroid.voice.RealtimeVoiceCatalog {
        val response = client.get("${base()}/media/providers") { authorize() }
        val body = response.bodyAsText()
        if (!response.status.isSuccess()) throw ChatError("Could not load voice models.")
        return chatJson.decodeFromString(ai.magicbeans.magdroid.voice.RealtimeVoiceCatalog.serializer(), body)
    }

    /** Patch only this binding, preserving concurrent changes on other clients. */
    suspend fun putVoicePreference(realtime: Boolean, bindingId: String, voice: String): MediaPreferences {
        val response = client.put("${base()}/media/preferences") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(buildJsonObject {
                put(if (realtime) "realtime_voices" else "tts_voices", buildJsonObject { put(bindingId, voice) })
            }.toString())
        }
        val body = response.bodyAsText()
        if (!response.status.isSuccess()) throw ChatError("Could not save voice. Your previous choice is still selected.")
        return chatJson.decodeFromString(MediaPreferences.serializer(), body)
    }

    /** Publish a preference change so the account's other devices see it. */
    suspend fun putMediaPreferences(autoSpeak: Boolean): Boolean = runCatching {
        val response = client.put("${base()}/media/preferences") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(
                chatRequestJson.encodeToString(
                    MediaPreferencesUpdate.serializer(),
                    MediaPreferencesUpdate(
                        autoSpeak = autoSpeak,
                    ),
                ),
            )
        }
        response.status.isSuccess()
    }.getOrDefault(false)

    /**
     * Whether Magician is answering, and what version.
     *
     * A reachability probe, not a diagnosis: the point is to tell "the phone
     * cannot get there" apart from "the request was refused", which is the
     * question anyone opening Settings after a failure is actually asking.
     */
    suspend fun healthStack(): ServiceHealthStack = runCatching {
        // Health is deliberately at the server root, not under the V2 API. It
        // already aggregates Magicutor and the desktop gateway, so probing
        // invented child paths would make healthy dependencies render offline.
        val response = client.get("${root()}/health") { authorize() }
        val body = response.bodyAsText()
        if (!response.status.isSuccess()) {
            val detail = when (response.status.value) {
                401, 403 -> "Access denied"
                404 -> "Not available on this host"
                in 500..599 -> "Service is unavailable"
                else -> "HTTP ${response.status.value}"
            }
            return ServiceHealthStack(
                magician = ServiceHealth(false, detail = detail),
                magicutor = ServiceHealth(false, detail = "Not reported"),
                desktop = ServiceHealth(false, detail = "Not reported"),
            )
        }
        runCatching { chatJson.decodeFromString(ServiceHealthBody.serializer(), body).stack() }
            .getOrElse {
                ServiceHealthStack(
                    magician = ServiceHealth(true, detail = "Reachable; status unreadable"),
                    magicutor = ServiceHealth(false, detail = "Not reported"),
                    desktop = ServiceHealth(false, detail = "Not reported"),
                )
            }
    }.getOrElse {
        if (it is CancellationException) throw it
        ServiceHealthStack(
            magician = ServiceHealth(false, detail = serviceHealthMessage(it)),
            magicutor = ServiceHealth(false, detail = "Not reported"),
            desktop = ServiceHealth(false, detail = "Not reported"),
        )
    }

    /** Release this repository's sockets when a short-lived settings surface leaves. */
    fun close() = client.close()

    private fun failure(status: HttpStatusCode, body: String): ChatError = when (status.value) {
        401, 403 -> ChatError(
            "Magician refused the request. Check the Cloudflare Access credentials on Setup.",
            setupRequired = true,
        )
        404 -> ChatError(
            Failure(
                kind = FailureKind.NotFound,
                headline = "That chat session no longer exists",
                detail = "It may have been deleted on another device.",
                retryable = false,
            ),
        )
        // Classified rather than pasted. This produced headlines like
        // "Magician answered 502: error code: 502" — the code twice, the
        // cause never, and no hint that the fix was to start the server.
        //
        // The body is not discarded: it moves to the detail line, where a
        // server that explained itself is still readable but no longer
        // outranks the sentence saying what to do.
        // The body is not discarded: it moves to the detail line, where a
        // server that explained itself is still readable but no longer
        // outranks the sentence saying what to do. A body that only echoes
        // the status — "error code: 502" — is dropped, because appending it
        // states the code a third time and explains nothing.
        else -> ChatError(
            Failures.ofStatus(status.value, "this conversation").let { classified ->
                val said = body.trim().take(160)
                if (said.addsSomethingTo(status.value)) {
                    classified.copy(detail = "${classified.detail} $said".trim())
                } else {
                    classified
                }
            },
        )
    }
}

/**
 * Whether a response body is worth showing beside an already-classified cause.
 *
 * A gateway's body is usually the status again in words — "error code: 502",
 * "502 Bad Gateway". Strip the digits and the boilerplate; if nothing is left,
 * the body was the code and the owner has already been told the code.
 */
internal fun String.addsSomethingTo(status: Int): Boolean {
    if (isBlank()) return false
    val remainder = lowercase()
        .replace(status.toString(), " ")
        .replace(Regex("[^a-z]+"), " ")
        .split(' ')
        .filter { it.isNotBlank() && it !in ECHOED }
    return remainder.isNotEmpty()
}

private fun chatHttpClient() = HttpClient(CIO) {
    install(io.ktor.client.plugins.websocket.WebSockets)
    install(HttpTimeout) {
        // A turn can think for a long time before its first token.
        requestTimeoutMillis = 300_000
        socketTimeoutMillis = 300_000
        connectTimeoutMillis = 20_000
    }
}

/** Words that carry no information once the status code is already stated. */
private val ECHOED = setOf(
    "error", "code", "status", "http", "bad", "gateway", "service",
    "unavailable", "timeout", "internal", "server", "the", "a", "an",
)

internal fun serviceHealthMessage(error: Throwable): String {
    val causes = generateSequence(error as Throwable?) { it.cause }.take(8).toList()
    return when {
        error is ChatError && error.setupRequired -> "Connection is not configured"
        causes.any { it is UnknownHostException || it is UnresolvedAddressException || it is ConnectException } ->
            "Magician is offline"
        causes.any { it is SocketTimeoutException || it.javaClass.simpleName.contains("Timeout", true) } ->
            "Connection timed out"
        else -> "Could not reach Magician"
    }
}

/**
 * How much of a turn's timeline one fetch asks for. The endpoint serves the
 * newest rows first-truncated (the tail of the JSONL), and two hundred events
 * comfortably covers any turn a phone screen will summarise into steps.
 */
private const val TURN_EVENTS_LIMIT = 200
