package ai.magicbeans.magdroid.vibedev

import android.content.Context
import android.content.Intent
import java.net.URI
import java.net.URLDecoder
import java.net.URLEncoder
import java.nio.charset.StandardCharsets
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * `GET /api/magician/v2/work/live`: the owner's long-running work ranked once
 * on the server (`work_summary`) — coding runs, VibeDev crews and Work tasks.
 * The summary notification features [featured] and counts the rest.
 */
@Serializable
data class WorkLive(
    val featured: WorkItem? = null,
    /** At most two, by rank after [featured]. */
    val next: List<WorkItem> = emptyList(),
    val counts: WorkCounts = WorkCounts(),
    /** At most 50, ranked. */
    val items: List<WorkItem> = emptyList(),
    /** Followed item ids (`run:…`, `crew:…`, `task:…`), at most three. */
    val followed: List<String> = emptyList(),
    @SerialName("generated_at") val generatedAt: String? = null,
)

/** `working` includes paused items. */
@Serializable
data class WorkCounts(
    @SerialName("needs_you") val needsYou: Int = 0,
    val working: Int = 0,
    val failed: Int = 0,
    val done: Int = 0,
)

/** What the owner can do about an item: `approve_diff` or `answer`, by id. */
@Serializable
data class WorkAction(val kind: String = "", val id: String = "")

@Serializable
data class WorkItem(
    /** `run:<id>`, `crew:<id>` or `task:<id>`. */
    val id: String,
    /** `coding_run | crew | work_task`. */
    val kind: String = "",
    val title: String = "",
    /** The project name; empty for a task. */
    val context: String? = null,
    /** `needs_you | working | paused | failed | stopped | done`. */
    val state: String = "working",
    val step: String? = null,
    @SerialName("by_owner") val byOwner: Boolean = false,
    @SerialName("from_phone") val fromPhone: Boolean = false,
    val followed: Boolean = false,
    @SerialName("started_at") val startedAt: String? = null,
    @SerialName("updated_at") val updatedAt: String? = null,
    @SerialName("waiting_since") val waitingSince: String? = null,
    @SerialName("spent_usd") val spentUsd: Double? = null,
    @SerialName("budget_usd") val budgetUsd: Double? = null,
    val action: WorkAction? = null,
    @SerialName("deep_link") val deepLink: String? = null,
    @SerialName("web_path") val webPath: String? = null,
    @SerialName("project_id") val projectId: String? = null,
    @SerialName("run_id") val runId: String? = null,
    @SerialName("live_task_id") val liveTaskId: String? = null,
    @SerialName("crew_id") val crewId: String? = null,
    @SerialName("task_id") val taskId: String? = null,
)

/** One line of the summary: the featured item or one of the next two. */
data class WorkSummaryLine(val id: String, val kind: String, val state: String, val title: String)

/** The work-item id a screen follows: `run:<id>`, `crew:<id>`, `task:<id>`. */
object WorkItemIds {
    fun run(runId: String) = "run:$runId"
    fun crew(crewId: String) = "crew:$crewId"
    fun task(taskId: String) = "task:$taskId"
}

internal fun workStateLabel(state: String): String = VibeDevRunState.from(state).label

internal fun workStateEnded(state: String): Boolean = state in setOf("failed", "stopped", "done")

internal fun workKindLabel(kind: String): String = when (kind) {
    "coding_run" -> "Coding run"
    "crew" -> "VibeDev crew"
    "work_task" -> "Task"
    else -> "Work"
}

/**
 * The link a work notification opens. Runs and crews keep their `magican://vibe`
 * link; a Work task opens its detail through `magican://task/<id>`, the task
 * link the app already routes (the Contract's `magican://work?task=…` has no
 * intent filter here).
 */
fun workItemLink(itemId: String?, deepLink: String?): String {
    val link = deepLink?.trim().orEmpty()
    val taskId = itemId?.trim()?.takeIf { it.startsWith("task:") }?.removePrefix("task:")?.takeIf(String::isNotBlank)
        ?: link.takeIf { it.startsWith("magican://work") }?.let { queryParameter(it, "task") }
    if (taskId != null) {
        return "magican://task/" + URLEncoder.encode(taskId, StandardCharsets.UTF_8.name()).replace("+", "%20")
    }
    return link.takeIf { it.startsWith("magican://") && !it.startsWith("magican://work") } ?: CodingRunsNotifier.DEEP_LINK
}

private fun queryParameter(raw: String, key: String): String? = runCatching {
    URI(raw).rawQuery.orEmpty().split('&').firstNotNullOfOrNull { part ->
        val pair = part.split('=', limit = 2)
        if (pair[0] == key) URLDecoder.decode(pair.getOrElse(1) { "" }, StandardCharsets.UTF_8.name()) else null
    }
}.getOrNull()?.trim()?.takeIf(String::isNotEmpty)

/**
 * The notification buttons and route requests the app answers in its
 * `WorkActionReceiver`: explicit-package broadcasts, as the task notifier's
 * route requests are, because the bridge cannot name app classes.
 */
object WorkActions {
    const val ACTION_APPROVE = "ai.magicbeans.magdroid.action.WORK_APPROVE"
    const val ACTION_FOLLOW = "ai.magicbeans.magdroid.action.WORK_FOLLOW"
    const val ACTION_UNFOLLOW = "ai.magicbeans.magdroid.action.WORK_UNFOLLOW"
    const val ACTION_REGISTER_ROUTE = "ai.magicbeans.magdroid.action.REGISTER_WORK_ITEM_PUSH"
    const val ACTION_REMOVE_ROUTE = "ai.magicbeans.magdroid.action.REMOVE_WORK_ITEM_PUSH"
    const val EXTRA_ITEM_ID = "item_id"
    const val EXTRA_ACTION_ID = "action_id"
    const val EXTRA_DEEP_LINK = "deep_link"

    /** Approve a waiting diff by its approval id, then re-read the summary. */
    suspend fun approve(context: Context, approvalId: String, itemId: String?, deepLink: String?) {
        val app = context.applicationContext
        val outcome = runCatching {
            VibeDevConnection.source(app).answerDiffApproval(emptyList(), approve = true, approvalId = approvalId)
        }
        if (outcome.isSuccess) {
            CodingRunsNotifier.clearNeedsYou(app)
        } else {
            val message = outcome.exceptionOrNull()?.message ?: "Open it to review the change."
            CodingRunsNotifier.postNotice(app, "Couldn't approve the change", message, workItemLink(itemId, deepLink))
        }
        CodingRunsNotifier.refresh(app)
    }

    /** Follow from a notification; a refusal (the limit) says why. */
    suspend fun follow(context: Context, itemId: String, deepLink: String?) {
        val app = context.applicationContext
        WorkFollows.follow(app, itemId).exceptionOrNull()?.let { error ->
            CodingRunsNotifier.postNotice(app, "Couldn't follow", error.message ?: "Try again from the app.", workItemLink(itemId, deepLink))
        }
    }

    suspend fun unfollow(context: Context, itemId: String) {
        WorkFollows.unfollow(context.applicationContext, itemId)
    }
}

/**
 * The items this scope follows, as the server last said. Each one gets its own
 * ongoing notification ([FollowedWorkNotifier]) through a `work_item` push
 * route the app registers with this phone's FCM token; this store asks for
 * the route when an id joins and for its removal when one leaves.
 */
object WorkFollows {
    private const val PREFERENCES = "magican_work_follows_v1"
    private const val FOLLOWED = "followed"
    const val MAX = 3
    const val FOLLOW_LIMIT_MESSAGE = "You can follow up to 3 items. Unfollow one first."
    private val state = MutableStateFlow<Set<String>>(emptySet())
    @Volatile private var loaded = false
    private val lock = Any()

    /** The followed ids, for Follow / Unfollow buttons. */
    fun followed(context: Context): StateFlow<Set<String>> {
        load(context.applicationContext)
        return state.asStateFlow()
    }

    fun ids(context: Context): Set<String> {
        load(context.applicationContext)
        return state.value
    }

    fun isFollowed(context: Context, itemId: String): Boolean = itemId in ids(context)

    /** Follow [itemId]; the summary is re-read so its Follow button goes. */
    suspend fun follow(context: Context, itemId: String): Result<Unit> = runCatching {
        val app = context.applicationContext
        val ids = VibeDevConnection.source(app).follow(itemId)
        sync(app, ids + itemId)
        CodingRunsNotifier.refresh(app)
    }

    /** Unfollow [itemId]: its card comes down now, its route after. */
    suspend fun unfollow(context: Context, itemId: String): Result<Unit> = runCatching {
        val app = context.applicationContext
        val ids = VibeDevConnection.source(app).unfollow(itemId)
        sync(app, ids - itemId)
        FollowedWorkNotifier.cancel(app, itemId)
        CodingRunsNotifier.refresh(app)
    }

    /**
     * Adopt the server's list: routes for ids that joined, removals for ids
     * that left (an ended item leaves ten minutes after it ends).
     */
    fun sync(context: Context, ids: Collection<String>) {
        val app = context.applicationContext
        val next = ids.map(String::trim).filterTo(linkedSetOf(), String::isNotEmpty)
        val (added, removed) = synchronized(lock) {
            load(app)
            val previous = state.value
            if (previous == next) return
            app.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).edit().putStringSet(FOLLOWED, next).apply()
            state.value = next
            (next - previous) to (previous - next)
        }
        added.forEach { route(app, it, register = true) }
        removed.forEach { route(app, it, register = false) }
    }

    /** The active machine changed: its follows (and routes) belong to it. */
    fun reset(context: Context) {
        val app = context.applicationContext
        synchronized(lock) {
            app.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).edit().remove(FOLLOWED).apply()
            state.value = emptySet()
            loaded = true
        }
    }

    private fun load(app: Context) {
        if (loaded) return
        synchronized(lock) {
            if (loaded) return
            state.value = app.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
                .getStringSet(FOLLOWED, null).orEmpty().toSet()
            loaded = true
        }
    }

    private fun route(app: Context, itemId: String, register: Boolean) {
        app.sendBroadcast(
            Intent(if (register) WorkActions.ACTION_REGISTER_ROUTE else WorkActions.ACTION_REMOVE_ROUTE)
                .setPackage(app.packageName)
                .putExtra(WorkActions.EXTRA_ITEM_ID, itemId),
        )
    }
}
