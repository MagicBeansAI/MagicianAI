package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.chat.ChatAppDestination
import ai.magicbeans.magdroid.chat.ChatLinkTarget
import ai.magicbeans.magdroid.chat.ChatLinks
import ai.magicbeans.magdroid.chat.ChatLocalState
import ai.magicbeans.magdroid.chat.ThreadDirectory
import ai.magicbeans.magdroid.work.*
import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.access.MagicianConnectionSnapshot
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

internal val LocalChatOutputSession = staticCompositionLocalOf<String?> { null }

internal enum class ChatContextPage(val title: String) {
    Tasks("Tasks"), Settings("Thread settings"), Files("Files"), Models("Models")
}

internal data class ChatContextTarget(val sessionId: String, val threadId: String?, val page: ChatContextPage, val connection: MagicianConnectionSnapshot?)

/** Same session inventory and canonical accounting rollup as the web. */
@Composable
internal fun ChatSessionDetails(sessionId: String, page: ChatContextPage, connection: MagicianConnectionSnapshot?, onDismiss: () -> Unit) {
    val context = LocalContext.current
    var files by remember(sessionId, connection) { mutableStateOf<JsonObject?>(null) }
    var usage by remember(sessionId, connection) { mutableStateOf<JsonObject?>(null) }
    var models by remember(sessionId, connection) { mutableStateOf<JsonObject?>(null) }
    var error by remember(sessionId, connection) { mutableStateOf<String?>(null) }
    var refreshing by remember(sessionId, connection) { mutableStateOf(false) }
    var refresh by remember(sessionId, connection) { mutableIntStateOf(0) }
    var pendingDelete by remember(sessionId, connection) { mutableStateOf<JsonObject?>(null) }
    var deleting by remember(sessionId, connection) { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    LaunchedEffect(sessionId, connection, refresh, page, lifecycle) {
        files = null; usage = null; models = null; error = null; refreshing = true
        val client = runCatching { connection?.let { WorkRepository.usingConnection(it) } ?: throw IllegalStateException("Connect to your Magician in Settings.") }.getOrElse { error = it.message; refreshing = false; return@LaunchedEffect }
        try {
            if (!client.isCurrentConnection(context)) { onDismiss(); return@LaunchedEffect }
            // Files load on open/refresh; accounting polls only in Models.
            if (page == ChatContextPage.Files) {
                try {
                    val value = client.readJson("/api/magician/v2/chat/sessions/${WorkRepository.segment(sessionId)}/assets").jsonObject
                    if (client.isCurrentConnection(context)) files = value
                } catch (e: CancellationException) { throw e } catch (e: Exception) { error = e.message }
                refreshing = false
                return@LaunchedEffect
            }
            lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
                while (true) {
                    if (!client.isCurrentConnection(context)) { onDismiss(); break }
                    try {
                        val body = buildJsonObject { put("sql", ChatLocalState.usageSql(sessionId)) }
                        val value = client.postJson("/api/magician/v2/analytics/llm_calls/query", body).jsonObject
                        val modelUsage = client.postJson("/api/magician/v2/analytics/llm_calls/query", buildJsonObject { put("sql", ChatLocalState.usageSql(sessionId, byModel = true)) }).jsonObject
                        if (client.isCurrentConnection(context)) { usage = value; models = modelUsage; error = null }
                    } catch (e: CancellationException) { throw e } catch (e: Exception) { error = e.message }
                    if (!client.isCurrentConnection(context)) { files = null; usage = null; models = null; error = "Connection changed. Reopen this chat."; break }
                    refreshing = false
                    delay(15000)
                }
            }
        } finally { client.close(); refreshing = false }
    }
    AlertDialog(onDismissRequest = { pendingDelete = null; onDismiss() }, title = { Text(page.title) },
        text = { Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState())) {
            if (refreshing) LinearProgressIndicator(Modifier.fillMaxWidth())
            error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            if (page == ChatContextPage.Models) {
                Text("Whole chat", style = MaterialTheme.typography.titleSmall)
                val columns = usage?.get("columns")?.jsonArray?.map { it.jsonPrimitive.content }.orEmpty()
                val row = usage?.get("rows")?.jsonArray?.firstOrNull()?.jsonArray
                val values = columns.mapIndexed { index, key -> key to row?.getOrNull(index) }.toMap()
                listOf("calls" to "Calls", "input_tokens" to "Input tokens", "output_tokens" to "Output tokens",
                    "cache_read_tokens" to "Cache read tokens", "cache_creation_tokens" to "Cache write tokens", "cost_usd" to "Cost (USD)", "unpriced_calls" to "Calls without a price").forEach { (key, label) ->
                    val value = values[key]?.takeUnless { it == JsonNull }?.jsonPrimitive?.content ?: "Unavailable"
                    Text("$label: $value")
                }
                Spacer(Modifier.height(12.dp)); Text("Models", style = MaterialTheme.typography.titleSmall)
                val modelColumns = models?.get("columns")?.jsonArray?.map { it.jsonPrimitive.content }.orEmpty()
                val modelRows = models?.get("rows")?.jsonArray
                if (modelRows?.isEmpty() == true) Text("No model usage recorded yet.")
                modelRows?.forEach { modelRow ->
                    val modelValues = modelColumns.mapIndexed { index, key -> key to modelRow.jsonArray.getOrNull(index) }.toMap()
                    fun value(key: String) = modelValues[key]?.takeUnless { it == JsonNull }?.jsonPrimitive?.content ?: "Not reported"
                    Text("${value("model")} · ${value("provider")}", style = MaterialTheme.typography.titleSmall)
                    Text("${value("calls")} calls · Input: ${value("input_tokens")} · Output: ${value("output_tokens")}", style = MaterialTheme.typography.bodySmall)
                    Text("Cache read: ${value("cache_read_tokens")} · Cache write: ${value("cache_creation_tokens")}", style = MaterialTheme.typography.bodySmall)
                    Text("Cost (USD): ${value("cost_usd")} · Unpriced calls: ${value("unpriced_calls")}", style = MaterialTheme.typography.bodySmall)
                    HorizontalDivider()
                }
            }
            if (page == ChatContextPage.Files) {
                Spacer(Modifier.height(12.dp)); Text("Files", style = MaterialTheme.typography.titleSmall)
                files?.get("files")?.jsonArray?.let { entries ->
                    val sizes = entries.mapNotNull { it.jsonObject["size"]?.jsonPrimitive?.longOrNull?.takeIf { size -> size >= 0 } }
                    val total = sizes.fold(0L) { sum, size -> if (sum > Long.MAX_VALUE - size) Long.MAX_VALUE else sum + size }
                    val unknown = entries.size - sizes.size
                    val partial = (files?.get("unavailable_files")?.jsonPrimitive?.intOrNull ?: 0) > 0 || files?.get("unavailable_tasks")?.jsonArray?.isNotEmpty() == true
                    Text("${entries.size} ${if (entries.size == 1) "file" else "files"} · ${if (unknown > 0 || partial) "Known size" else "Total"}: ${android.text.format.Formatter.formatShortFileSize(context, total)}" +
                        if (unknown > 0) " · $unknown ${if (unknown == 1) "size" else "sizes"} unavailable" else "", style = MaterialTheme.typography.bodySmall)
                }
                files?.get("files")?.jsonArray?.forEach { item ->
                    val file = item.jsonObject
                    val path = file["path"]?.jsonPrimitive?.content ?: return@forEach
                    val name = file["name"]?.jsonPrimitive?.content ?: path
                    val source = file["source"]?.jsonPrimitive?.content
                    val mime = file["mime_type"]?.takeUnless { it == JsonNull }?.jsonPrimitive?.content
                    val route = if (source == "task") "/api/magician/v3/tasks/${WorkRepository.segment(file["task_id"]!!.jsonPrimitive.content)}/outputs/"
                        else "/api/magician/v2/chat/sessions/${WorkRepository.segment(sessionId)}/outputs/"
                    val url = connection?.baseUrl?.trimEnd('/') + route + path.split('/').joinToString("/") { WorkRepository.segment(it) }
                    TextButton(enabled = connection != null, onClick = { scope.launch {
                        if (MagicianAccess.connectionSnapshot(context) == connection) AuthenticatedArtifacts.open(context, url, name, mime, expectedConnection = connection)
                    } }) { Text("$name · ${source ?: "chat"}") }
                    listOf("task_id" to "Task", "execution_id" to "Execution", "session_id" to "Chat").forEach { (key, label) ->
                        file[key]?.takeUnless { it == JsonNull }?.jsonPrimitive?.content?.let { Text("$label: $it", style = MaterialTheme.typography.bodySmall) }
                    }
                    val rawDate = file["created_at"]?.takeUnless { it == JsonNull }?.jsonPrimitive?.content
                    val date = rawDate?.let { raw -> runCatching {
                        val instant = raw.toLongOrNull()?.let(java.time.Instant::ofEpochMilli) ?: java.time.Instant.parse(raw)
                        java.time.format.DateTimeFormatter.ofLocalizedDateTime(java.time.format.FormatStyle.MEDIUM).withZone(java.time.ZoneId.systemDefault()).format(instant)
                    }.getOrNull() }
                    Text(date ?: "Date unavailable", style = MaterialTheme.typography.bodySmall)
                    val size = file["size"]?.jsonPrimitive?.longOrNull?.takeIf { it >= 0 }
                    Text(size?.let { android.text.format.Formatter.formatShortFileSize(context, it) } ?: "Size unavailable", style = MaterialTheme.typography.bodySmall)
                    TextButton(enabled = !deleting && !refreshing && connection != null, onClick = { pendingDelete = file }) {
                        Text("Delete $name", color = MaterialTheme.colorScheme.error)
                    }
                    HorizontalDivider()
                }
                if (files?.get("files")?.jsonArray?.isEmpty() == true) Text("No files in this chat yet.")
                if ((files?.get("unavailable_files")?.jsonPrimitive?.intOrNull ?: 0) > 0 || files?.get("unavailable_tasks")?.jsonArray?.isNotEmpty() == true)
                    Text("Some files are unavailable. Refresh to retry.")
            }
        } }, confirmButton = { TextButton(onClick = { pendingDelete = null; onDismiss() }) { Text("Done") } },
        dismissButton = { TextButton(enabled = !deleting, onClick = { refresh++ }) { Text("Refresh") } })
    pendingDelete?.let { file ->
        AlertDialog(onDismissRequest = { pendingDelete = null },
            title = { Text("Delete file from this chat?") },
            text = { Text("Any chat copy will be permanently removed and existing chat links may stop working. Any task original stays available under Work.") },
            confirmButton = { TextButton(onClick = {
                pendingDelete = null
                val id = file["id"]?.jsonPrimitive?.content
                if (!deleting && id != null && connection != null && connection == MagicianAccess.connectionSnapshot(context)) {
                    deleting = true
                    scope.launch {
                        val client = WorkRepository.usingConnection(connection)
                        try {
                            if (!client.isCurrentConnection(context)) return@launch
                            client.deleteJson("/api/magician/v2/chat/sessions/${WorkRepository.segment(sessionId)}/assets/${WorkRepository.segment(id)}")
                            if (client.isCurrentConnection(context)) refresh++
                        } catch (e: CancellationException) { throw e }
                        catch (e: Exception) { if (client.isCurrentConnection(context)) error = e.message }
                        finally { client.close(); deleting = false }
                    }
                }
            }) { Text("Delete file", color = MaterialTheme.colorScheme.error) } },
            dismissButton = { TextButton(onClick = { pendingDelete = null }) { Text("Cancel") } })
    }
}

/** Thread-scoped name and durable context, distinct from app Settings. */
@Composable
internal fun ChatThreadSettings(threadId: String, connection: MagicianConnectionSnapshot?, onDismiss: () -> Unit, onSaved: () -> Unit) {
    val context = LocalContext.current
    var name by remember(threadId, connection) { mutableStateOf("") }
    var memory by remember(threadId, connection) { mutableStateOf("") }
    var originalName by remember(threadId, connection) { mutableStateOf("") }
    var originalMemory by remember(threadId, connection) { mutableStateOf("") }
    var loaded by remember(threadId, connection) { mutableStateOf(false) }
    var saving by remember(threadId, connection) { mutableStateOf(false) }
    var error by remember(threadId, connection) { mutableStateOf<String?>(null) }
    var saved by remember(threadId, connection) { mutableStateOf(false) }
    var revision by remember(threadId, connection) { mutableIntStateOf(0) }
    val scope = rememberCoroutineScope()
    val path = "/api/magician/v2/ui-threads/${WorkRepository.segment(threadId)}"
    LaunchedEffect(threadId, connection, revision) {
        loaded = false; error = null
        val client = runCatching { connection?.let { WorkRepository.usingConnection(it) } ?: throw IllegalStateException("Connect to your Magician in Settings.") }.getOrElse { error = it.message; return@LaunchedEffect }
        try {
            if (!client.isCurrentConnection(context)) { onDismiss(); return@LaunchedEffect }
            val thread = client.readJson(path).jsonObject["thread"]?.jsonObject ?: throw IllegalStateException("Thread response is missing")
            if (client.isCurrentConnection(context)) {
                name = thread["name"]?.jsonPrimitive?.contentOrNull ?: threadId
                memory = thread["memory_text"]?.jsonPrimitive?.contentOrNull.orEmpty()
                originalName = name; originalMemory = memory
                loaded = true
            }
        } catch (e: CancellationException) { throw e }
        catch (e: Exception) { if (client.isCurrentConnection(context)) error = e.message }
        finally { client.close() }
    }
    AlertDialog(onDismissRequest = onDismiss, title = { Text("Thread settings") }, text = {
        Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text("Saved name and context for #${originalName.ifBlank { ThreadDirectory.label(threadId).removePrefix("#") }}.", style = MaterialTheme.typography.bodySmall)
            if (!loaded && error == null) LinearProgressIndicator(Modifier.fillMaxWidth())
            error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            if (!loaded && error != null) TextButton(onClick = { revision++ }) { Text("Retry") }
            OutlinedTextField(value = name, onValueChange = { name = it; saved = false }, label = { Text("Name") }, enabled = loaded && !saving, modifier = Modifier.fillMaxWidth())
            OutlinedTextField(value = memory, onValueChange = { memory = it; saved = false }, label = { Text("Thread memory") }, minLines = 4, enabled = loaded && !saving, modifier = Modifier.fillMaxWidth())
            if (saved) Text("Saved")
        }
    }, confirmButton = {
        TextButton(enabled = loaded && !saving && connection != null && ChatLocalState.threadSettingsPatch(threadId, originalName, originalMemory, name, memory).isNotEmpty(), onClick = {
            if (connection == null || connection != MagicianAccess.connectionSnapshot(context)) return@TextButton
            saving = true; error = null
            val submittedName = name.trim().ifBlank { threadId }
            val submittedMemory = memory
            val patch = ChatLocalState.threadSettingsPatch(threadId, originalName, originalMemory, name, memory)
            scope.launch {
                val client = WorkRepository.usingConnection(connection)
                try {
                    if (!client.isCurrentConnection(context)) return@launch
                    client.putJson(path, patch)
                    if (client.isCurrentConnection(context)) {
                        originalName = submittedName; originalMemory = submittedMemory
                        saved = true; onSaved()
                    }
                } catch (e: CancellationException) { throw e }
                catch (e: Exception) { if (client.isCurrentConnection(context)) error = e.message }
                finally { client.close(); saving = false }
            }
        }) { Text(if (saving) "Saving…" else "Save") }
    }, dismissButton = { TextButton(onClick = onDismiss) { Text("Done") } })
}

/**
 * The one opener for every link tapped in chat (Markdown replies, structured
 * blocks). [ChatLinks.classify] — the twin of web `classifyRef` and iOS
 * `classifyActivityLink` — decides what a link is; this decides what happens:
 *
 * - note links → the native Notes screen;
 * - task outputs and `/api/magician/…` resources → downloaded with the device
 *   bearer and opened through the app's FileProvider ([AuthenticatedArtifacts]);
 * - host files (absolute paths, `file://`, workspace-relative) → open-file on
 *   the paired machine through the chat session;
 * - chat links (`/chat?session=…`, `/t/<thread>[/chat][?session=…]`) → that
 *   conversation, else the thread's latest ([ChatDeepLinks]); task, Work,
 *   Attention, Today, Observe, Settings, Notes and VibeDev routes → in-app
 *   navigation ([ChatLinks.appDestination]); any other app route → the
 *   machine's web page in the browser;
 * - http(s), mailto, tel, `magican://` → the system.
 *
 * A tap it cannot open shows "Can't open link" instead of being dropped.
 */
@Composable
internal fun rememberChatOutputHandler(fallback: androidx.compose.ui.platform.UriHandler, session: String? = LocalChatOutputSession.current): androidx.compose.ui.platform.UriHandler {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val connection = MagicianAccess.connectionSnapshot(context)
    return remember(session, context, fallback, connection) { object : androidx.compose.ui.platform.UriHandler {
        fun cannotOpen(detail: String? = null) {
            android.widget.Toast.makeText(context, detail?.let { "$CANNOT_OPEN_LINK: $it" } ?: CANNOT_OPEN_LINK, android.widget.Toast.LENGTH_SHORT).show()
        }

        fun view(url: String) {
            try {
                context.startActivity(android.content.Intent(android.content.Intent.ACTION_VIEW, android.net.Uri.parse(url))
                    .addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK))
            } catch (e: android.content.ActivityNotFoundException) { cannotOpen() } catch (e: RuntimeException) { cannotOpen() }
        }

        fun openAuthenticated(path: String, filename: String?) {
            val base = MagicianAccess.baseUrl(context).ifBlank { return cannotOpen("no paired machine") }
            scope.launch { AuthenticatedArtifacts.open(context, base + path, filename?.takeIf(String::isNotBlank), null, connection) }
        }

        fun openOnHost(path: String) {
            if (session == null) return cannotOpen("this file is not attached to a chat")
            scope.launch {
                if (connection == null || connection != MagicianAccess.connectionSnapshot(context)) { cannotOpen(); return@launch }
                val client = WorkRepository.usingConnection(connection)
                try {
                    client.postJson("/api/magician/v2/chat/sessions/${WorkRepository.segment(session)}/outputs/open-file",
                        buildJsonObject { put("absolute_path", path) })
                    if (client.isCurrentConnection(context)) android.widget.Toast.makeText(context, "Opened on your Magician host", android.widget.Toast.LENGTH_SHORT).show()
                } catch (e: CancellationException) { throw e } catch (e: Exception) {
                    if (client.isCurrentConnection(context)) cannotOpen(e.message)
                } finally { client.close() }
            }
        }

        fun openRoute(route: String) {
            fun web() {
                val base = MagicianAccess.baseUrl(context).ifBlank { return cannotOpen("no paired machine") }
                view(base + route)
            }
            // A `/t/<key>` link may carry the thread's slug (it follows the
            // name) or an old one; open by the canonical id the server maps it to.
            fun resolvingThread(key: String, then: (String) -> Unit) {
                ThreadDirectory.canonicalId(key)?.let { return then(it) }
                val snapshot = connection ?: return then(key)
                scope.launch {
                    val client = WorkRepository.usingConnection(snapshot)
                    val id = try { ThreadDirectory.resolve(key) { path -> client.readJson(path) } } finally { client.close() }
                    if (client.isCurrentConnection(context)) then(id)
                }
            }
            when (val destination = ChatLinks.appDestination(route)) {
                is ChatAppDestination.Chat -> {
                    val threadKey = destination.threadId
                    if (destination.sessionId != null || threadKey == null) ChatDeepLinks.openChat(destination.sessionId, threadKey)
                    else resolvingThread(threadKey) { id -> ChatDeepLinks.openChat(null, id) }
                }
                ChatAppDestination.History -> ChatDeepLinks.openHistory()
                is ChatAppDestination.Work -> {
                    fun accept(target: String) { if (!TaskDeepLinks.accept(android.net.Uri.parse(target))) web() }
                    val threadKey = ChatLinks.threadRouteKey(destination.route)
                    if (threadKey == null) accept(destination.route)
                    else resolvingThread(threadKey) { id -> accept(ChatLinks.withThreadId(destination.route, id)) }
                }
                is ChatAppDestination.Attention -> {
                    AttentionDeepLinks.request(destination.itemId)
                    AppShortcutLinks.request(AppShortcutTarget.OpenAttention)
                }
                ChatAppDestination.Today -> AppShortcutLinks.request(AppShortcutTarget.OpenToday)
                is ChatAppDestination.Observe ->
                    AppShortcutLinks.request(AppShortcutTarget.OpenObserve(ObservePane.fromDeepLink(destination.route)))
                ChatAppDestination.Settings -> ChatDeepLinks.openSettings()
                ChatAppDestination.Notes -> ChatDeepLinks.openNotes()
                ChatAppDestination.VibeDev -> AppShortcutLinks.request(AppShortcutTarget.OpenVibeDev)
                is ChatAppDestination.Web -> web()
            }
        }

        override fun openUri(uri: String) {
            if (NoteLinks.accept(context, uri)) return
            val origin = MagicianAccess.baseUrl(context).takeIf(String::isNotBlank)
            when (val target = ChatLinks.classify(uri, origin)) {
                is ChatLinkTarget.External -> view(target.url)
                is ChatLinkTarget.TaskOutput -> openAuthenticated(target.apiPath, target.filename)
                is ChatLinkTarget.ServerResource -> openAuthenticated(target.path, null)
                is ChatLinkTarget.HostFile -> openOnHost(target.path)
                is ChatLinkTarget.AppRoute -> openRoute(target.route)
                is ChatLinkTarget.Unknown -> cannotOpen()
            }
        }
    } }
}

private const val CANNOT_OPEN_LINK = "Can't open link"
