package ai.magicbeans.magdroid.chat

import ai.magicbeans.magdroid.attention.AttentionItem
import ai.magicbeans.magdroid.attention.askIds
import ai.magicbeans.magdroid.attention.askKey
import ai.magicbeans.magdroid.attention.openTaskId
import ai.magicbeans.magdroid.attention.originChatSessionId
import ai.magicbeans.magdroid.tasks.PlanApprovalActions

/*
 * Pure rules for the rows stacked on and above the chat composer (web
 * `ChatPanel` / `FloatingComposer` / `ComposerAskDock` / `PlannerDock`, iOS
 * `ComposerView` top rows): which asks the ask dock shows and how it hands a
 * secret off, which tasks the planner dock lists, the queue row's label, the
 * auto-send band's fill, the presence state and the archived read-only state.
 * No Compose, no networking.
 */

/** One question the conversation is waiting on, as the ask dock shows it. */
data class ComposerAskTarget(val messageId: String, val card: EscalationCard)

/**
 * One ask the dock can show: an escalation card in this transcript, or an
 * Attention feed row — this conversation's (by `ui_thread_id`, or by a task
 * the conversation started: Relentless and quality-controller asks carry a
 * task and no thread, and never appear in the transcript) or any other open one.
 */
sealed interface ComposerAsk {
    val key: String

    data class Transcript(val target: ComposerAskTarget) : ComposerAsk {
        override val key: String get() = target.messageId
    }

    data class Feed(val item: AttentionItem, override val key: String = "feed:${item.askKey}") : ComposerAsk
}

object ComposerAskDock {
    /**
     * Kinds answered in the focused surface (Attention) rather than inline:
     * secrets, grants and diff reviews (web `HANDOFF`), plus forms.
     */
    private val HANDOFF: Map<String, String> = mapOf(
        "password" to "Enter it securely →",
        "otp" to "Enter it securely →",
        "tool_authorization" to "Review and decide →",
        "sandbox_override" to "Review and decide →",
        "diff_approval" to "Review the changes →",
        "form" to "Answer in Attention →",
    )

    /** The open asks of the transcript, oldest first: unresolved escalation cards. */
    fun targets(messages: List<ChatMessage>): List<ComposerAskTarget> = messages.mapNotNull { message ->
        val card = message.escalation
        if (message.kind == MessageKind.Escalation && card != null && !card.resolved) {
            ComposerAskTarget(message.id, card)
        } else null
    }

    /** Tasks this conversation started or reported on: its task cards. */
    fun taskIds(messages: List<ChatMessage>): Set<String> = messages.mapNotNull { message ->
        (message.task?.taskId ?: message.escalation?.taskId)?.trim()?.takeIf { it.isNotEmpty() && it != "unknown-task" }
    }.toSet()

    /**
     * Keep transcript order, replacing a duplicate with the canonical feed
     * contract: it carries the full approval preview, grants and form schema.
     * Match every known alias so one pause is never asked twice.
     * [settled] hides asks answered on another in-place surface before the
     * transcript hears about it.
     */
    fun merged(
        messages: List<ChatMessage>,
        feedAsks: List<AttentionItem>,
        settled: Set<String> = emptySet(),
    ): List<ComposerAsk> {
        val transcript = targets(messages).filterNot { it.card.correlationId?.let(settled::contains) == true }
        val feed = ai.magicbeans.magdroid.attention.AskSurfaces.dedupe(
            feedAsks.filter { item -> item.askIds.none(settled::contains) },
        ).toMutableList()
        val seen = mutableSetOf<String>()
        val result = mutableListOf<ComposerAsk>()
        for (target in transcript) {
            val id = target.card.correlationId?.trim()?.takeIf(String::isNotEmpty)
            if (id != null && id in seen) continue
            val canonical = feed.firstOrNull { id != null && id in it.askIds }
            if (canonical != null) {
                feed.remove(canonical)
                seen += canonical.askIds
                result += ComposerAsk.Feed(canonical, key = target.messageId)
            } else {
                id?.let(seen::add)
                result += ComposerAsk.Transcript(target)
            }
        }
        result += feed.filter { it.askIds.none(seen::contains) }.map { ComposerAsk.Feed(it) }
        return result
    }

    /**
     * Whether an ask belongs to this conversation. A transcript card does, and
     * so does the feed row that replaced one; another feed row when [here]
     * names it (`null`: every one does).
     */
    fun isHere(ask: ComposerAsk, here: Set<String>?): Boolean = when (ask) {
        is ComposerAsk.Transcript -> true
        is ComposerAsk.Feed -> here == null || ask.item.askKey in here || ask.key != "feed:${ask.item.askKey}"
    }

    /**
     * Where an ask from another conversation came from, for the dock's origin
     * line (web `ComposerAskDock`): `From <chat> · <agent>`, `From task “<title>”`,
     * else the task id or `background run`.
     */
    fun originLabel(
        item: AttentionItem,
        chatTitles: Map<String, String>,
        taskTitles: Map<String, String> = emptyMap(),
    ): String {
        val hitl = item.metadata?.hitlRequest
        val schema = item.metadata?.inputSchema ?: hitl?.inputSchema
        fun String?.clean() = this?.trim()?.takeIf(String::isNotEmpty)
        val chatId = item.originChatSessionId
        val taskId = item.openTaskId
        val source = when {
            chatId != null -> chatTitles[chatId] ?: "Chat ${chatId.take(8)}"
            taskId != null -> taskTitles[taskId]?.let { "task “$it”" } ?: "task ${taskId.take(13)}"
            else -> "background run"
        }
        val origin = schema?.contextString("session")?.takeIf { it.startsWith("run:") }
            ?.let { "Run ${it.removePrefix("run:").take(8)} · $source" } ?: source
        val agent = (schema?.contextString("agent_id") ?: hitl?.scope?.agentId.clean() ?: item.agentId.clean())
            ?.let { " · $it" }.orEmpty()
        return "From $origin$agent"
    }

    /** The active ask survives a new arrival; it moves on only when it leaves. */
    fun activeKey(keys: List<String>, current: String?): String? =
        if (current != null && current in keys) current else keys.firstOrNull()

    /** Previous / next, wrapping. */
    fun stepKey(keys: List<String>, current: String?, delta: Int): String? {
        if (keys.isEmpty()) return null
        val index = keys.indexOf(current).coerceAtLeast(0)
        return keys[Math.floorMod(index + delta, keys.size)]
    }

    /** `2 of 3`, only with more than one ask. */
    fun positionOf(keys: List<String>, current: String?): String? {
        if (keys.size < 2) return null
        val index = keys.indexOf(current)
        return if (index < 0) null else "${index + 1} of ${keys.size}"
    }

    /** The hand-off label for an ask answered elsewhere; null when it is answered inline. */
    fun handoff(card: EscalationCard): String? {
        if (card.requiresReview) return "Review and decide →"
        if (card.allowOther) return "Answer in Attention →"
        val kind = card.inputType?.trim()?.lowercase()
        kind?.let { HANDOFF[it] }?.let { return it }
        card.escalationType?.trim()?.lowercase()?.let { HANDOFF[it] }?.let { return it }
        return if (card.sensitive != null) HANDOFF["password"] else null
    }

    /** A plan approval: Review plan · Reject · Approve · Approve & Run. */
    fun isPlanApproval(card: EscalationCard): Boolean =
        PlanApprovalActions.isPlanApproval(card.escalationType, card.inputType?.trim()?.lowercase())

    /** The active ask survives a new arrival; it moves on only when it leaves. */
    fun activeId(targets: List<ComposerAskTarget>, current: String?): String? =
        activeKey(targets.map { it.messageId }, current)

    /** Previous / next, wrapping. */
    fun step(targets: List<ComposerAskTarget>, current: String?, delta: Int): String? =
        stepKey(targets.map { it.messageId }, current, delta)

    /** `2 of 3`, only with more than one ask. */
    fun position(targets: List<ComposerAskTarget>, current: String?): String? =
        positionOf(targets.map { it.messageId }, current)
}

/** A task whose drafted plan waits on the reader, for the planner dock. */
data class PlannerDockTask(val taskId: String, val title: String, val status: String, val updatedAtMs: Long?)

object PlannerDock {
    /**
     * Each task's latest card in the transcript whose (store-reconciled)
     * status is a drafted plan, newest first. The dock re-checks each with the
     * task store before it offers anything.
     */
    fun candidates(messages: List<ChatMessage>): List<PlannerDockTask> {
        val latest = linkedMapOf<String, TaskStatusCard>()
        messages.forEach { message ->
            val task = message.task ?: return@forEach
            if (message.kind != MessageKind.TaskStatus || task.taskId == "unknown-task") return@forEach
            latest.remove(task.taskId)
            latest[task.taskId] = task
        }
        return latest.values.reversed().mapNotNull { task ->
            val status = (task.storeStatus ?: task.status).lowercase()
            if (status != "draft") return@mapNotNull null
            PlannerDockTask(
                taskId = task.taskId,
                title = TaskTitleRules.labelOrShortIdTitle(task.title, task.taskId),
                status = status,
                updatedAtMs = task.updatedAtMs,
            )
        }
    }

    /** `1 item`, `3 items across 2 tasks`. */
    fun countLabel(items: Int, tasks: Int): String {
        val base = "$items ${if (items == 1) "item" else "items"}"
        return if (tasks > 1) "$base across $tasks tasks" else base
    }
}

/** The composer's queue row and auto-send band. */
object ComposerRows {
    /** `#1 · 2m ago`, or `#1` without a time. */
    fun queueItemLabel(index: Int, queuedAtMillis: Long?, nowMillis: Long): String {
        val time = queuedAtMillis?.let { relative(it, nowMillis) }
        return listOfNotNull("#${index + 1}", time).joinToString(" · ")
    }

    fun relative(atMillis: Long, nowMillis: Long): String {
        val seconds = ((nowMillis - atMillis) / 1000).coerceAtLeast(0)
        return when {
            seconds < 45 -> "just now"
            seconds < 3_600 -> "${(seconds + 30) / 60}m ago"
            seconds < 86_400 -> "${seconds / 3_600}h ago"
            else -> "${seconds / 86_400}d ago"
        }
    }

    /** The countdown a dictated draft sends after (`ChatViewModel.armAutoSend`). */
    const val AUTO_SEND_SECONDS = 3

    /** The auto-send bar's fill after [remaining] seconds: drains from 1 to 0. */
    fun autoSendFraction(remaining: Int, total: Int = AUTO_SEND_SECONDS): Float =
        if (total <= 0) 0f else (remaining.coerceIn(0, total).toFloat() / total)
}

/**
 * The agent's presence beside the chat session title (web `PresenceOrb`,
 * iOS `ChatPresenceState`): idle, thinking, working, or needs you.
 */
enum class ChatPresence { Idle, Thinking, Working, NeedsYou }

object ChatPresenceRules {
    /** Precedence: needs-you > working > thinking > idle (web `derivePresence`). */
    fun state(hasPendingAsk: Boolean, streaming: Boolean, toolsRunning: Boolean, liveTurn: Boolean): ChatPresence = when {
        hasPendingAsk -> ChatPresence.NeedsYou
        streaming || toolsRunning -> ChatPresence.Working
        liveTurn -> ChatPresence.Thinking
        else -> ChatPresence.Idle
    }

    /**
     * From the chat's own state, as iOS `ChatView.presence` reads it: an open
     * ask needs you; reply text arriving, a live tool step or a server-side
     * run is working; a turn in flight with nothing to show yet is thinking.
     */
    fun derive(messages: List<ChatMessage>, sending: Boolean, serverRunning: Boolean): ChatPresence {
        val last = messages.lastOrNull()?.takeIf { !it.fromUser && it.streaming }
        val streaming = last != null && last.text.isNotBlank()
        val toolsRunning = serverRunning || (last != null && (last.activity.isNotEmpty() || last.activityRows.isNotEmpty()))
        return state(
            hasPendingAsk = ComposerAskDock.targets(messages).isNotEmpty(),
            streaming = streaming,
            toolsRunning = toolsRunning,
            liveTurn = sending || last != null,
        )
    }

    fun label(presence: ChatPresence, assistant: String = "Magician"): String = when (presence) {
        ChatPresence.Idle -> "$assistant is idle"
        ChatPresence.Thinking -> "$assistant is thinking"
        ChatPresence.Working -> "$assistant is working"
        ChatPresence.NeedsYou -> "$assistant needs you"
    }
}

object ChatReadOnly {
    /** An archived conversation is read-only: the composer becomes a "Return to active" band. */
    fun isArchived(session: SessionSummary?): Boolean = session?.status.equals("archived", ignoreCase = true)

    const val BAND_TEXT = "Viewing archived conversation"
}
