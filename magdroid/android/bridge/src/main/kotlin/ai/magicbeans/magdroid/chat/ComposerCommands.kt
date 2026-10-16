package ai.magicbeans.magdroid.chat

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

/**
 * Composer slash commands.
 *
 * `/` at the start of the composer opens a command list styled like the `@`
 * picker: `/budget`, `/budget default`, `/engine`, `/run-engine`. Choosing a
 * command shows its options in the same picker; choosing an option runs it on
 * the client — never as a chat message. Contract:
 * `docs/plans/2026-10-03-slash-commands-and-run-budgets.md`. A Kotlin port of
 * `magios/Shared/ComposerCommands.swift`; keep the two in step.
 *
 * Everything here is pure: trigger, command tree, option rows, filtering, the
 * request bodies and the confirmation lines. The view model does the I/O.
 */
enum class ComposerCommandId(val wire: String) {
    Budget("budget"),
    BudgetDefault("budget default"),
    Engine("engine"),
    RunEngine("run-engine");

    val label: String get() = "/$wire"

    companion object {
        fun fromWire(name: String): ComposerCommandId? = entries.firstOrNull { it.wire == name }
    }
}

data class ComposerCommand(val id: ComposerCommandId, val detail: String) {
    val name: String get() = id.wire
    val label: String get() = id.label
}

/** A run's USD cap: a number, or switched off. */
sealed interface BudgetCap {
    data class Usd(val value: Double) : BudgetCap
    data object Unlimited : BudgetCap

    val label: String
        get() = when (this) {
            Unlimited -> "Unlimited"
            is Usd -> usdLabel(value)
        }

    val wire: JsonPrimitive
        get() = when (this) {
            Unlimited -> JsonPrimitive("unlimited")
            is Usd -> JsonPrimitive(value)
        }

    companion object {
        fun fromWire(element: JsonElement?): BudgetCap? {
            val primitive = element as? JsonPrimitive ?: return null
            if (primitive.isString) {
                val text = primitive.content
                if (text.equals("unlimited", ignoreCase = true)) return Unlimited
                return text.toDoubleOrNull()?.let { Usd(it) }
            }
            return primitive.doubleOrNull?.let { Usd(it) }
        }
    }
}

internal fun usdLabel(value: Double): String =
    if (value == Math.rint(value)) "$${value.toLong()}" else "$" + String.format(java.util.Locale.US, "%.2f", value)

/** `GET /api/magician/v2/budgets/defaults`. */
data class BudgetDefaults(val native: BudgetCap? = null, val harness: BudgetCap? = null) {
    companion object {
        fun decode(body: String): BudgetDefaults? = runCatching {
            val obj = Json.parseToJsonElement(body).jsonObject
            BudgetDefaults(
                native = BudgetCap.fromWire(obj["native_max_cost_usd"]),
                harness = BudgetCap.fromWire(obj["harness_max_cost_usd"]),
            )
        }.getOrNull()
    }
}

enum class BudgetRunKind(val wireKey: String, val title: String, val badge: String) {
    Harness("harness_max_cost_usd", "Harness runs", "Harness"),
    Native("native_max_cost_usd", "Native runs", "Native"),
}

/** One `POST /tasks/{id}/budget` grant. */
enum class BudgetGrant(val label: String, val detail: String) {
    Usd25("+$25", "Raise the run's cost cap"),
    Usd100("+$100", "Raise the run's cost cap"),
    Tokens1M("+1M tokens", "Raise the run's token allowance"),
    Iterations50("+50 iterations", "Raise the run's iteration allowance"),
    Unlimited("Unlimited", "Remove the cost cap and token meter for this run");

    fun body(): JsonObject = buildJsonObject {
        when (this@BudgetGrant) {
            Usd25 -> put("add_cost_usd", 25)
            Usd100 -> put("add_cost_usd", 100)
            Tokens1M -> put("add_tokens", 1_000_000)
            Iterations50 -> put("add_iterations", 50)
            Unlimited -> put("unlimited", true)
        }
    }
}

/** What picking a row does. */
sealed interface ComposerCommandAction {
    data object Back : ComposerCommandAction
    data class Open(val id: ComposerCommandId) : ComposerCommandAction
    /** `/engine`: flip between "this device" and "default everywhere". */
    data object ToggleEngineScope : ComposerCommandAction
    /** The per-device chat engine (and, [everywhere], `PUT /plane/chat-engine`). */
    data class SetChatEngine(val engine: String, val model: String, val everywhere: Boolean) : ComposerCommandAction
    /** `PUT /plane/engine`. */
    data class SetRunEngine(val engine: String, val model: String) : ComposerCommandAction
    data class PickBudgetTask(val taskId: String, val title: String) : ComposerCommandAction
    data class GrantBudget(val taskId: String, val title: String, val grant: BudgetGrant) : ComposerCommandAction
    /** `PUT /budgets/defaults`. */
    data class SetBudgetDefault(val kind: BudgetRunKind, val cap: BudgetCap) : ComposerCommandAction
    /** An informational row: nothing to pick. */
    data object None : ComposerCommandAction

    /** Runs on the server (or the device setting) and closes the picker. */
    val terminal: Boolean
        get() = this is SetChatEngine || this is SetRunEngine || this is GrantBudget || this is SetBudgetDefault
}

data class ComposerCommandRow(
    val id: String,
    /** The small uppercase chip, like the `@` picker's kind chip. */
    val badge: String,
    val label: String,
    val detail: String? = null,
    /** The current value (rendered with a check). */
    val current: Boolean = false,
    val action: ComposerCommandAction,
) {
    /** Always shown whatever the filter says. */
    internal val pinned: Boolean
        get() = action == ComposerCommandAction.Back ||
            action == ComposerCommandAction.ToggleEngineScope ||
            action == ComposerCommandAction.None
}

/** A task the transcript shows, from its task cards (oldest first). */
data class ComposerCommandTask(val taskId: String, val title: String, val status: String)

data class ComposerCommandEngine(val name: String, val models: List<String>)

/** Everything the option rows read. */
data class ComposerCommandContext(
    val engines: List<ComposerCommandEngine> = emptyList(),
    val deviceEngine: String = "magician",
    val deviceModel: String = "default",
    val runEngine: String? = null,
    val runModel: String? = null,
    val tasks: List<ComposerCommandTask> = emptyList(),
    val budgetDefaults: BudgetDefaults? = null,
    /** Why the defaults could not be read, when they could not. */
    val budgetDefaultsError: String? = null,
)

sealed interface ComposerCommandLevel {
    data class Command(val id: ComposerCommandId) : ComposerCommandLevel
    data class BudgetTask(val taskId: String, val title: String) : ComposerCommandLevel
}

/** Where the picker is. An empty [stack] is the command list. */
data class ComposerCommandState(
    val stack: List<ComposerCommandLevel> = emptyList(),
    /** `/engine` applies everywhere (also `PUT /plane/chat-engine`). */
    val engineEverywhere: Boolean = false,
    /**
     * The composer text when the current level opened (`/budget `). Typing
     * after it filters the options; deleting into it closes the level.
     */
    val prefix: String = "",
)

/** A state change plus the text the composer should now hold. */
data class ComposerCommandStep(val state: ComposerCommandState, val text: String)

object ComposerCommands {
    val all: List<ComposerCommand> = listOf(
        ComposerCommand(ComposerCommandId.Budget, "Give a running or paused task more budget"),
        ComposerCommand(ComposerCommandId.BudgetDefault, "Default cost caps for new runs"),
        ComposerCommand(ComposerCommandId.Engine, "Who answers chat on this device"),
        ComposerCommand(ComposerCommandId.RunEngine, "Who runs tasks"),
    )

    private val terminalStatuses = setOf(
        "complete", "completed", "done", "failed", "error", "cancelled", "canceled",
    )

    /** Statuses of a task with no run yet: nothing to budget. */
    private val notStartedStatuses = setOf(
        "created", "pending", "draft", "planning", "plan_ready", "ready", "rejected",
    )

    /**
     * The query after a leading `/`, or null. Only a composer that starts with
     * `/` (no leading space) and holds a single line opens the list.
     */
    fun trigger(text: String): String? =
        if (text.startsWith("/") && !text.contains('\n')) text.substring(1) else null

    /** Commands whose name contains the (trimmed) query, declared order. */
    fun matches(query: String): List<ComposerCommand> {
        val needle = query.trim().lowercase()
        if (needle.isEmpty()) return all
        return all.filter { it.name.contains(needle) }
    }

    /**
     * A composer holding exactly a command (`/budget`): sending it opens the
     * command's options instead of posting it to the model.
     */
    fun exactCommand(text: String): ComposerCommandId? {
        val trimmed = text.trim().lowercase()
        if (!trimmed.startsWith("/")) return null
        val name = trimmed.substring(1).split(' ').filter { it.isNotEmpty() }.joinToString(" ")
        return ComposerCommandId.fromWire(name)
    }

    fun open(id: ComposerCommandId): ComposerCommandStep {
        val prefix = "/${id.wire} "
        return ComposerCommandStep(ComposerCommandState(listOf(ComposerCommandLevel.Command(id)), false, prefix), prefix)
    }

    fun pickTask(taskId: String, title: String, state: ComposerCommandState): ComposerCommandStep =
        ComposerCommandStep(state.copy(stack = state.stack + ComposerCommandLevel.BudgetTask(taskId, title)), state.prefix)

    fun back(state: ComposerCommandState): ComposerCommandStep {
        val stack = state.stack.dropLast(1)
        if (stack.isEmpty()) return ComposerCommandStep(ComposerCommandState(), "/")
        return ComposerCommandStep(state.copy(stack = stack), state.prefix)
    }

    /** Keep the level while the text still starts with its prefix; otherwise close it. */
    fun sync(text: String, state: ComposerCommandState): ComposerCommandState {
        if (state.stack.isEmpty()) return state
        return if (text.lowercase().startsWith(state.prefix.lowercase())) state else ComposerCommandState()
    }

    /** The rows the picker shows for [text] in [state]. */
    fun rows(text: String, state: ComposerCommandState, context: ComposerCommandContext): List<ComposerCommandRow> {
        val level = state.stack.lastOrNull()
        if (level == null) {
            val query = trigger(text) ?: return emptyList()
            return matches(query).map { command ->
                ComposerCommandRow(
                    id = "command:${command.name}", badge = "Command",
                    label = command.label, detail = command.detail,
                    action = ComposerCommandAction.Open(command.id),
                )
            }
        }
        val query = if (text.length >= state.prefix.length) text.substring(state.prefix.length) else ""
        val back = ComposerCommandRow(
            id = "back", badge = "Back",
            label = if (state.stack.size > 1) "Back to tasks" else "Back to commands",
            action = ComposerCommandAction.Back,
        )
        return listOf(back) + filter(optionRows(level, state, context), query)
    }

    internal fun filter(rows: List<ComposerCommandRow>, query: String): List<ComposerCommandRow> {
        val needle = query.trim().lowercase()
        if (needle.isEmpty()) return rows
        return rows.filter { it.pinned || "${it.label} ${it.detail.orEmpty()}".lowercase().contains(needle) }
    }

    private fun optionRows(
        level: ComposerCommandLevel,
        state: ComposerCommandState,
        context: ComposerCommandContext,
    ): List<ComposerCommandRow> = when (level) {
        is ComposerCommandLevel.BudgetTask -> BudgetGrant.entries.map { grant ->
            ComposerCommandRow(
                id = "grant:${grant.name}", badge = "Budget", label = grant.label, detail = grant.detail,
                action = ComposerCommandAction.GrantBudget(level.taskId, level.title, grant),
            )
        }
        is ComposerCommandLevel.Command -> when (level.id) {
            ComposerCommandId.Engine -> {
                val scope = ComposerCommandRow(
                    id = "engine:scope", badge = "Scope",
                    label = if (state.engineEverywhere) "Default everywhere" else "This device",
                    detail = if (state.engineEverywhere) {
                        "Tap to apply to this device only"
                    } else {
                        "Tap to make the choice the default everywhere"
                    },
                    action = ComposerCommandAction.ToggleEngineScope,
                )
                listOf(scope) + engineRows(context).map { (engine, model) ->
                    ComposerCommandRow(
                        id = "engine:$engine:$model", badge = "Engine", label = engineLabel(engine, model),
                        current = engine == context.deviceEngine && model == context.deviceModel,
                        action = ComposerCommandAction.SetChatEngine(engine, model, state.engineEverywhere),
                    )
                }
            }
            ComposerCommandId.RunEngine -> engineRows(context).map { (engine, model) ->
                ComposerCommandRow(
                    id = "run:$engine:$model", badge = "Run", label = engineLabel(engine, model),
                    current = engine == context.runEngine && model == (context.runModel ?: "default"),
                    action = ComposerCommandAction.SetRunEngine(engine, model),
                )
            }
            ComposerCommandId.Budget -> {
                val tasks = budgetTasks(context.tasks)
                if (tasks.isEmpty()) {
                    listOf(
                        ComposerCommandRow(
                            id = "budget:none", badge = "Task",
                            label = "No running or paused tasks in this chat",
                            action = ComposerCommandAction.None,
                        ),
                    )
                } else {
                    tasks.map { task ->
                        ComposerCommandRow(
                            id = "task:${task.taskId}", badge = "Task", label = task.title,
                            detail = statusWord(task.status),
                            action = ComposerCommandAction.PickBudgetTask(task.taskId, task.title),
                        )
                    }
                }
            }
            ComposerCommandId.BudgetDefault -> buildList {
                context.budgetDefaultsError?.let { error ->
                    add(
                        ComposerCommandRow(
                            id = "defaults:error", badge = "Note",
                            label = "Current defaults unavailable", detail = error,
                            action = ComposerCommandAction.None,
                        ),
                    )
                }
                addAll(
                    defaultRows(
                        BudgetRunKind.Harness,
                        listOf(BudgetCap.Usd(50.0), BudgetCap.Usd(100.0), BudgetCap.Usd(250.0), BudgetCap.Unlimited),
                        context.budgetDefaults?.harness,
                    ),
                )
                addAll(
                    defaultRows(
                        BudgetRunKind.Native,
                        listOf(BudgetCap.Usd(5.0), BudgetCap.Usd(15.0), BudgetCap.Usd(50.0), BudgetCap.Unlimited),
                        context.budgetDefaults?.native,
                    ),
                )
            }
        }
    }

    private fun defaultRows(kind: BudgetRunKind, caps: List<BudgetCap>, current: BudgetCap?) = caps.map { cap ->
        ComposerCommandRow(
            id = "default:${kind.name.lowercase()}:${cap.label}", badge = kind.badge,
            label = cap.label, detail = kind.title, current = current == cap,
            action = ComposerCommandAction.SetBudgetDefault(kind, cap),
        )
    }

    /** Engine × model pairs, roster order. */
    private fun engineRows(context: ComposerCommandContext): List<Pair<String, String>> =
        context.engines.flatMap { engine ->
            engine.models.ifEmpty { listOf("default") }.map { engine.name to it }
        }

    fun engineLabel(engine: String, model: String): String =
        if (model == "default") engine else "$engine · $model"

    /** The chat's running/paused tasks, newest first, one row per task. */
    fun budgetTasks(tasks: List<ComposerCommandTask>): List<ComposerCommandTask> {
        val seen = HashSet<String>()
        return tasks.asReversed()
            .filter { it.taskId.isNotBlank() && it.taskId != "unknown-task" && seen.add(it.taskId) }
            .filter {
                val status = it.status.lowercase()
                status !in terminalStatuses && status !in notStartedStatuses
            }
    }

    private fun statusWord(status: String): String =
        status.replace('_', ' ').replaceFirstChar { it.uppercase() }

    // MARK: requests and confirmations

    /** `PUT /budgets/defaults` body: the changed cap, plus the other one when known. */
    fun defaultsBody(kind: BudgetRunKind, cap: BudgetCap, current: BudgetDefaults?): JsonObject = buildJsonObject {
        put(kind.wireKey, cap.wire)
        val other = if (kind == BudgetRunKind.Harness) BudgetRunKind.Native else BudgetRunKind.Harness
        val known = if (other == BudgetRunKind.Harness) current?.harness else current?.native
        known?.let { put(other.wireKey, it.wire) }
    }

    fun engineBody(engine: String, model: String): JsonObject = buildJsonObject {
        put("harness_engine", engine)
        put("harness_model", model)
    }

    fun chatEngineConfirmation(engine: String, model: String, everywhere: Boolean): String =
        if (everywhere) {
            "Chat engine is now ${engineLabel(engine, model)} everywhere"
        } else {
            "Chat engine on this device: ${engineLabel(engine, model)}"
        }

    fun runEngineConfirmation(engine: String, model: String): String = "Run engine: ${engineLabel(engine, model)}"

    fun grantConfirmation(grant: BudgetGrant, title: String, resumed: Boolean): String {
        val what = if (grant == BudgetGrant.Unlimited) {
            "Removed the budget cap on $title"
        } else {
            "Added ${grant.label.drop(1)} to $title"
        }
        return if (resumed) "$what — resumed" else what
    }

    fun defaultConfirmation(kind: BudgetRunKind, cap: BudgetCap): String = "${kind.title} default cap: ${cap.label}"

    /** Whether a `POST /tasks/{id}/budget` answer says the run was continued. */
    fun resumed(body: String): Boolean = runCatching {
        Json.parseToJsonElement(body).jsonObject["resumed"]?.jsonPrimitive?.booleanOrNull ?: false
    }.getOrDefault(false)

    /**
     * The server's own words for a failed call: `message`, else `error`, else
     * the body, else the status.
     */
    fun serverErrorMessage(status: Int, body: String?): String {
        val text = body?.trim().orEmpty()
        runCatching { Json.parseToJsonElement(text).jsonObject }.getOrNull()?.let { obj ->
            for (key in listOf("message", "error", "detail")) {
                val value = (obj[key] as? JsonPrimitive)?.takeIf { it.isString }?.contentOrNull
                if (!value.isNullOrBlank()) return value
            }
        }
        if (text.isNotEmpty()) return text.take(200)
        return if (status == 404) "Not available on this Magician (HTTP 404)" else "Request failed (HTTP $status)"
    }
}
