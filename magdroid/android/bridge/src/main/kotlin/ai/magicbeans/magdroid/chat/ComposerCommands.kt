package ai.magicbeans.magdroid.chat

import ai.magicbeans.magdroid.preferences.HarnessCredentials
import ai.magicbeans.magdroid.voice.ComposerVoiceMode
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
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
 * picker: `/budget`, `/budget default`, `/engine`, `/run-engine`,
 * `/voice-mode`. Choosing a command shows its options in the same picker;
 * choosing an option runs it on the client — never as a chat message.
 * `/engine` and `/run-engine` are engine → model pickers (the composer's model
 * dropdown and Settings → Agentic flows). Contract:
 * `docs/archive/plans/2026-10-03-slash-commands-and-run-budgets.md`, extended by
 * `docs/plans/2026-10-30-slash-engine-model-voice-design.md`. A Kotlin port of
 * `magios/Shared/ComposerCommands.swift` (and web `composerCommands.ts`); keep
 * the rows, labels and notes in step.
 *
 * Everything here is pure: trigger, command tree, option rows, filtering, the
 * request bodies and the confirmation lines. The view model does the I/O.
 */
enum class ComposerCommandId(val wire: String) {
    Budget("budget"),
    BudgetDefault("budget default"),
    Engine("engine"),
    RunEngine("run-engine"),
    VoiceMode("voice-mode");

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

/**
 * `GET /api/magician/v2/llm/routing`, read for the `agentic_decision`
 * operation: Magician's own loop has no per-run model, every decision uses the
 * model mapped to that operation (web `EnginesPanel` "loop model").
 */
data class LoopModelRouting(
    val models: List<ChatModelChoice> = emptyList(),
    val selection: ChatModelSelection? = null,
    val error: String? = null,
) {
    @Serializable
    internal data class Overview(
        val models: List<ChatModelChoice> = emptyList(),
        val profiles: List<Profile> = emptyList(),
        val operations: List<Operation> = emptyList(),
    )

    @Serializable
    internal data class Profile(
        val name: String = "",
        @SerialName("model_selection") val modelSelection: ChatModelSelection? = null,
    )

    @Serializable
    internal data class Operation(
        val operation: String = "",
        @SerialName("effective_profile") val effectiveProfile: String? = null,
    )

    companion object {
        const val operation = "agentic_decision"

        fun decode(body: String): LoopModelRouting {
            val overview = runCatching { chatJson.decodeFromString(Overview.serializer(), body) }.getOrNull()
                ?: return LoopModelRouting(error = "Model routing could not be read.")
            val mapping = overview.operations.firstOrNull { it.operation == operation }
                ?: return LoopModelRouting(overview.models, error = "The agentic decision mapping is not in the model catalog.")
            val route = mapping.effectiveProfile
            // Web `selectionForRoute`: the profile's saved selection, else the model whose choices name the route.
            val saved = overview.profiles.firstOrNull { it.name == route }?.modelSelection
                ?.takeIf { selection -> overview.models.any { it.id == selection.modelId } }
            val selection = saved ?: overview.models.firstNotNullOfOrNull { model ->
                (model.choices.entries + model.operationChoices.orEmpty().entries)
                    .firstOrNull { it.value == route }?.let { ChatModelSelection(model.id, it.key) }
            }
            return LoopModelRouting(overview.models, selection)
        }
    }
}

/** What picking a row does. */
sealed interface ComposerCommandAction {
    data object Back : ComposerCommandAction
    data class Open(val id: ComposerCommandId) : ComposerCommandAction
    /** An engine's models: `/engine <engine>` ([run] false) or `/run-engine <engine>`. */
    data class OpenModels(val engine: String, val run: Boolean) : ComposerCommandAction
    /** … then a catalog model's thinking levels. */
    data class OpenThinking(val engine: String, val modelId: String) : ComposerCommandAction
    /** `/engine`: flip between "this device" and "default everywhere". */
    data object ToggleEngineScope : ComposerCommandAction
    /**
     * The per-device chat engine (and, [everywhere], `PUT /plane/chat-engine`).
     * [selection] is a Magician/Pi catalog model, sent as `model_selection`;
     * [model] is a harness engine's `harness_model`.
     */
    data class SetChatEngine(
        val engine: String,
        val model: String,
        val everywhere: Boolean,
        val selection: ChatModelSelection? = null,
    ) : ComposerCommandAction
    /**
     * `PUT /plane/engine`; [selection] is Pi's `model_selection` (null = the
     * coding agent's own default). `chat` is "Same as Chat".
     */
    data class SetRunEngine(val engine: String, val model: String, val selection: ChatModelSelection? = null) :
        ComposerCommandAction
    /** `PUT /llm/routing/agentic_decision`, pinning the run engine to Magician first when it is not. */
    data class SetLoopModel(val selection: ChatModelSelection) : ComposerCommandAction
    /** The composer's mic mode (`VoicePrefs.setComposerVoiceMode`). */
    data class SetVoiceMode(val mode: ComposerVoiceMode) : ComposerCommandAction
    data class PickBudgetTask(val taskId: String, val title: String) : ComposerCommandAction
    data class GrantBudget(val taskId: String, val title: String, val grant: BudgetGrant) : ComposerCommandAction
    /** `PUT /budgets/defaults`. */
    data class SetBudgetDefault(val kind: BudgetRunKind, val cap: BudgetCap) : ComposerCommandAction
    /** An informational row: nothing to pick. Always shown. */
    data object None : ComposerCommandAction
    /** A choice this device or server cannot use now: shown greyed, filtered like any option. */
    data object Unavailable : ComposerCommandAction

    /** Runs on the server (or the device setting) and closes the picker. */
    val terminal: Boolean
        get() = this is SetChatEngine || this is SetRunEngine || this is SetLoopModel || this is SetVoiceMode ||
            this is GrantBudget || this is SetBudgetDefault
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

    /** Nothing happens on a tap: a note, or an unavailable choice. */
    val inert: Boolean
        get() = action == ComposerCommandAction.None || action == ComposerCommandAction.Unavailable
}

/** A task the transcript shows, from its task cards (oldest first). */
data class ComposerCommandTask(val taskId: String, val title: String, val status: String)

/** [credentials] is the roster's `credentials` (`login` / `magician_key` / `none`), labelled after the name. */
data class ComposerCommandEngine(val name: String, val models: List<String>, val credentials: String? = null)

/** Everything the option rows read. */
data class ComposerCommandContext(
    val engines: List<ComposerCommandEngine> = emptyList(),
    val deviceEngine: String = "magician",
    /** A harness engine's model on this device (`harness_model`). */
    val deviceModel: String = "default",
    /** The composer's catalog (Magician/Pi) for [deviceEngine], and the model it sends. */
    val deviceModels: List<ChatModelChoice> = emptyList(),
    val deviceSelection: ChatModelSelection? = null,
    /** The server's chat engine (`chat_current`): what "everywhere" uses. */
    val serverChatEngine: String? = null,
    /** The run engine as the roster resolves it (`current`), and its model. */
    val runEngine: String? = null,
    val runModel: String? = null,
    /** Agentic flows follow the chat engine (`follows_chat`). */
    val runFollowsChat: Boolean = false,
    /** Pi's background-run model (`run_model_selection`). */
    val runSelection: ChatModelSelection? = null,
    /** `GET /chat/models?engine=` per catalog engine, read for the picker; absent = loading. */
    val modelCatalogs: Map<String, List<ChatModelChoice>> = emptyMap(),
    val modelCatalogErrors: Map<String, String> = emptyMap(),
    /** The `agentic_decision` operation's models and current selection; null models = loading. */
    val loopModels: List<ChatModelChoice>? = null,
    val loopSelection: ChatModelSelection? = null,
    val loopModelsError: String? = null,
    val voiceMode: ComposerVoiceMode = ComposerVoiceMode.Dictation,
    val liveVoiceAvailable: Boolean = false,
    val handsFreeAvailable: Boolean = false,
    val tasks: List<ComposerCommandTask> = emptyList(),
    val budgetDefaults: BudgetDefaults? = null,
    /** Why the defaults could not be read, when they could not. */
    val budgetDefaultsError: String? = null,
) {
    /** The catalog for [engine]: read for the picker, else the composer's own when it is that engine. */
    fun catalog(engine: String): List<ChatModelChoice>? =
        modelCatalogs[engine] ?: deviceModels.takeIf { engine == deviceEngine && it.isNotEmpty() }
}

/** The composer's mic mode and which modes this device can use (the screen knows the voice catalog). */
data class ComposerCommandVoice(
    val mode: ComposerVoiceMode = ComposerVoiceMode.Dictation,
    val liveVoiceAvailable: Boolean = false,
    val handsFreeAvailable: Boolean = false,
)

sealed interface ComposerCommandLevel {
    data class Command(val id: ComposerCommandId) : ComposerCommandLevel
    data class BudgetTask(val taskId: String, val title: String) : ComposerCommandLevel
    /** `/engine <engine>`: the engine's models for chat on this device. */
    data class EngineModels(val engine: String) : ComposerCommandLevel
    /** `/run-engine <engine>`: the engine's models for agentic flows. */
    data class RunModels(val engine: String) : ComposerCommandLevel
    /** `… <engine> <model>`: a catalog model's thinking levels ([run] under `/run-engine`). */
    data class Thinking(val engine: String, val modelId: String, val run: Boolean) : ComposerCommandLevel
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
        ComposerCommand(ComposerCommandId.VoiceMode, "What the voice button starts"),
    )

    /** Engines whose models come from `GET /chat/models?engine=` rather than the roster. */
    val catalogEngines = setOf("magician", "pi")

    /** `/run-engine`'s "Same as Chat" (`execution.harness_engine: chat`). */
    const val sameAsChat = "chat"

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

    /** Open [engine]'s models under `/engine` ([run] false) or `/run-engine`. */
    fun openModels(engine: String, run: Boolean, state: ComposerCommandState): ComposerCommandStep {
        val level = if (run) ComposerCommandLevel.RunModels(engine) else ComposerCommandLevel.EngineModels(engine)
        return push(state, level)
    }

    /** Open a catalog model's thinking levels (under the engine's models). */
    fun openThinking(engine: String, modelId: String, state: ComposerCommandState): ComposerCommandStep {
        val run = state.stack.lastOrNull() is ComposerCommandLevel.RunModels
        return push(state, ComposerCommandLevel.Thinking(engine, modelId, run))
    }

    private fun push(state: ComposerCommandState, level: ComposerCommandLevel): ComposerCommandStep {
        val stack = state.stack + level
        val prefix = prefixFor(stack)
        return ComposerCommandStep(state.copy(stack = stack, prefix = prefix), prefix)
    }

    /** The composer text a stack shows: `/engine pi <model> `. A chosen task adds no token. */
    internal fun prefixFor(stack: List<ComposerCommandLevel>): String = stack.fold("/") { text, level ->
        when (level) {
            is ComposerCommandLevel.Command -> "/${level.id.wire} "
            is ComposerCommandLevel.BudgetTask -> text
            is ComposerCommandLevel.EngineModels -> "$text${level.engine} "
            is ComposerCommandLevel.RunModels -> "$text${level.engine} "
            is ComposerCommandLevel.Thinking -> "$text${level.modelId} "
        }
    }

    fun back(state: ComposerCommandState): ComposerCommandStep {
        val stack = state.stack.dropLast(1)
        if (stack.isEmpty()) return ComposerCommandStep(ComposerCommandState(), "/")
        val prefix = prefixFor(stack)
        return ComposerCommandStep(state.copy(stack = stack, prefix = prefix), prefix)
    }

    /**
     * Send must not post this text: it is a command, or an open level's
     * prefix and filter (`/engine pi son`). Commands never reach the model.
     */
    fun holdsSend(text: String, state: ComposerCommandState): Boolean {
        if (exactCommand(text) != null) return true
        if (state.stack.isEmpty()) return false
        val prefix = state.prefix.trim().lowercase()
        return prefix.isNotEmpty() && text.trim().lowercase().startsWith(prefix)
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
            label = when (level) {
                is ComposerCommandLevel.BudgetTask -> "Back to tasks"
                is ComposerCommandLevel.EngineModels, is ComposerCommandLevel.RunModels -> "Back to engines"
                is ComposerCommandLevel.Thinking -> "Back to models"
                is ComposerCommandLevel.Command -> "Back to commands"
            },
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
        is ComposerCommandLevel.EngineModels -> modelRows(level.engine, run = false, context)
        is ComposerCommandLevel.RunModels -> modelRows(level.engine, run = true, context)
        is ComposerCommandLevel.Thinking -> thinkingRows(level, context)
        is ComposerCommandLevel.Command -> when (level.id) {
            ComposerCommandId.Engine -> engineRows(state, context)
            ComposerCommandId.RunEngine -> runEngineRows(context)
            ComposerCommandId.VoiceMode -> voiceRows(context)
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

    private fun noteRow(id: String, label: String, detail: String? = null) =
        ComposerCommandRow(id = id, badge = "Note", label = label, detail = detail, action = ComposerCommandAction.None)

    /** Whether picking [engine] needs a second level: a catalog engine, or a harness with more than `default`. */
    private fun hasModelChoice(engine: ComposerCommandEngine): Boolean =
        engine.name in catalogEngines || engine.models.any { it != "default" }

    /** `/engine` level one: what is set now, the scope, one row per installed engine. */
    private fun engineRows(state: ComposerCommandState, context: ComposerCommandContext): List<ComposerCommandRow> {
        val deviceModel = deviceModelLabel(context)
        val now = buildString {
            append("Now: ").append(engineLabel(context.deviceEngine, deviceModel)).append(" (this device)")
            context.serverChatEngine?.let { append(" · everywhere: ").append(engineDisplayName(it)) }
        }
        val note = noteRow(
            "engine:now", now,
            detail = if (state.engineEverywhere) "The model stays a choice of this device" else null,
        )
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
        return listOf(note, scope) + context.engines.map { engine ->
            val current = engine.name == context.deviceEngine
            ComposerCommandRow(
                id = "engine:${engine.name}", badge = "Engine",
                label = engineRowLabel(engine.name, "default", engine.credentials),
                detail = if (current && !state.engineEverywhere) deviceModel?.takeIf { it != "default" } else null,
                current = current,
                // The server stores no chat model selection: "everywhere" sets the engine only.
                action = if (!state.engineEverywhere && hasModelChoice(engine)) {
                    ComposerCommandAction.OpenModels(engine.name, run = false)
                } else {
                    ComposerCommandAction.SetChatEngine(engine.name, "default", state.engineEverywhere)
                },
            )
        }
    }

    /** `/run-engine` level one: what agentic flows use now, "Same as Chat", then the engines. */
    private fun runEngineRows(context: ComposerCommandContext): List<ComposerCommandRow> {
        val chatEngine = engineDisplayName(context.serverChatEngine ?: "magician")
        val now = if (context.runFollowsChat) {
            "Now: Same as Chat ($chatEngine)"
        } else {
            "Now: " + engineLabel(context.runEngine ?: "magician", runModelLabel(context))
        }
        val followChat = ComposerCommandRow(
            id = "run:$sameAsChat", badge = "Run", label = "Same as Chat", detail = chatEngine,
            current = context.runFollowsChat,
            action = ComposerCommandAction.SetRunEngine(sameAsChat, "default"),
        )
        return listOf(noteRow("run:now", now), followChat) + context.engines.map { engine ->
            ComposerCommandRow(
                id = "run:${engine.name}", badge = "Run",
                label = engineRowLabel(engine.name, "default", engine.credentials),
                current = !context.runFollowsChat && engine.name == context.runEngine,
                action = if (hasModelChoice(engine)) {
                    ComposerCommandAction.OpenModels(engine.name, run = true)
                } else {
                    ComposerCommandAction.SetRunEngine(engine.name, "default")
                },
            )
        }
    }

    /** Level two: a catalog (Magician/Pi, or the loop's models), else the roster's `models[]` (`default` first). */
    private fun modelRows(engine: String, run: Boolean, context: ComposerCommandContext): List<ComposerCommandRow> {
        val prefix = if (run) "run-model" else "model"
        val catalog = catalogLevel(engine, run, context)
        if (catalog == null) {
            val roster = context.engines.firstOrNull { it.name == engine }?.models.orEmpty()
            return (listOf("default") + roster.filter { it != "default" }).map { model ->
                ComposerCommandRow(
                    id = "$prefix:$engine:$model", badge = "Model", label = harnessModelLabel(model),
                    current = if (run) {
                        !context.runFollowsChat && engine == context.runEngine && model == (context.runModel ?: "default")
                    } else {
                        engine == context.deviceEngine && model == context.deviceModel
                    },
                    action = if (run) {
                        ComposerCommandAction.SetRunEngine(engine, model)
                    } else {
                        ComposerCommandAction.SetChatEngine(engine, model, everywhere = false)
                    },
                )
            }
        }
        val models = catalog.models
        if (models == null || catalog.error != null) {
            return listOf(
                if (catalog.error != null) noteRow("$prefix:error", "Models unavailable: ${catalog.error}")
                else noteRow("$prefix:loading", "Loading models…"),
            )
        }
        val rows = models.map { model ->
            val current = catalog.current?.modelId == model.id
            val inUseThinking = catalog.current?.thinking
                ?.takeIf { current && it.isNotBlank() && it != "auto" && model.thinking.mode != "none" }
            val provider = model.providerDisplayName.takeIf { it.isNotBlank() }
            ComposerCommandRow(
                id = "$prefix:$engine:${model.id}", badge = "Model", label = modelName(model),
                detail = if (!model.installed) {
                    "Not available here"
                } else {
                    listOfNotNull(provider, inUseThinking?.let { thinkingLevelLabel(it) }).joinToString(" · ").ifEmpty { null }
                },
                current = current,
                action = when {
                    !model.installed -> ComposerCommandAction.Unavailable
                    thinkingChoices(model, catalog.operation).size > 1 -> ComposerCommandAction.OpenThinking(engine, model.id)
                    else -> catalog.action(ChatModelSelection(model.id, keepThinking(model, catalog.keep, catalog.operation)))
                },
            )
        }
        if (!(run && engine == "pi")) return rows
        // Settings' "Use default": clear the run model (`model_selection: null`).
        val useDefault = ComposerCommandRow(
            id = "$prefix:pi:default", badge = "Model", label = "Default",
            detail = "The coding agent's own default model",
            current = !context.runFollowsChat && context.runEngine == "pi" && context.runSelection == null,
            action = ComposerCommandAction.SetRunEngine("pi", "default", null),
        )
        return listOf(useDefault) + rows
    }

    /**
     * A catalog model level's inputs: the list (null = loading), the pick in
     * force (only while that engine is the one in use), the pick whose
     * thinking level a new model keeps, and how a pick becomes an action.
     */
    private class CatalogLevel(
        val models: List<ChatModelChoice>?,
        val error: String?,
        val current: ChatModelSelection?,
        val keep: ChatModelSelection?,
        /** A routing operation's catalog (`operation_choices` count too). */
        val operation: Boolean,
        val action: (ChatModelSelection) -> ComposerCommandAction,
    )

    /** The catalog behind `/engine <engine>` or `/run-engine <engine>`; null for a roster engine. */
    private fun catalogLevel(engine: String, run: Boolean, context: ComposerCommandContext): CatalogLevel? {
        if (engine !in catalogEngines) return null
        if (!run) {
            return CatalogLevel(
                context.catalog(engine), context.modelCatalogErrors[engine],
                current = context.deviceSelection.takeIf { context.deviceEngine == engine },
                keep = context.deviceSelection, operation = false,
            ) { ComposerCommandAction.SetChatEngine(engine, "default", everywhere = false, selection = it) }
        }
        if (engine == "magician") {
            // Magician's loop decides with the `agentic_decision` model, in force only while runs think with Magician.
            return CatalogLevel(
                context.loopModels, context.loopModelsError,
                current = context.loopSelection.takeIf { runResolvesToMagician(context) },
                keep = context.loopSelection, operation = true,
            ) { ComposerCommandAction.SetLoopModel(it) }
        }
        return CatalogLevel(
            context.catalog(engine), context.modelCatalogErrors[engine],
            current = context.runSelection.takeIf { !context.runFollowsChat && context.runEngine == engine },
            keep = context.runSelection, operation = false,
        ) { ComposerCommandAction.SetRunEngine(engine, "default", it) }
    }

    /**
     * Level three: a model's thinking levels in `ModelThinkingControl`'s order
     * and words. The level in force is marked when this is the model in use,
     * else the model's default.
     */
    private fun thinkingRows(level: ComposerCommandLevel.Thinking, context: ComposerCommandContext): List<ComposerCommandRow> {
        val catalog = catalogLevel(level.engine, level.run, context) ?: return emptyList()
        val prefix = if (level.run) "run-thinking" else "thinking"
        val models = catalog.models
        if (models == null || catalog.error != null) {
            return listOf(
                if (catalog.error != null) noteRow("$prefix:error", "Models unavailable: ${catalog.error}")
                else noteRow("$prefix:loading", "Loading models…"),
            )
        }
        val model = models.firstOrNull { it.id == level.modelId } ?: return emptyList()
        val levels = thinkingChoices(model, catalog.operation)
        val inUse = catalog.current?.takeIf { it.modelId == model.id }?.thinking
        val fallback = model.thinking.defaultLevel
        val marked = when {
            inUse != null && inUse in levels -> inUse
            fallback != null && fallback in levels -> fallback
            else -> "auto"
        }
        return levels.map { thinking ->
            ComposerCommandRow(
                id = "$prefix:${level.engine}:${model.id}:$thinking", badge = "Thinking",
                label = thinkingLevelLabel(thinking),
                detail = fallback?.takeIf { thinking == "auto" && it != "auto" }?.let { "Default · ${thinkingLevelLabel(it)}" },
                current = thinking == marked,
                action = catalog.action(ChatModelSelection(model.id, thinking)),
            )
        }
    }

    /**
     * The thinking levels the picker offers, as `ModelThinkingControl` orders
     * them (Auto, then the control's levels), limited to those the model
     * routes. Empty without a thinking control.
     */
    fun thinkingChoices(model: ChatModelChoice, operation: Boolean = false): List<String> {
        if (model.thinking.mode == "none") return emptyList()
        val routed = if (operation) model.choices.keys + model.operationChoices.orEmpty().keys else model.choices.keys
        return (listOf("auto") + model.thinking.levels).distinct().filter { it in routed }
    }

    /** Agentic flows think with Magician: its engine, or "Same as Chat" while the chat default is Magician. */
    fun runResolvesToMagician(context: ComposerCommandContext): Boolean =
        if (context.runFollowsChat) context.serverChatEngine == "magician" else context.runEngine == "magician"

    /** `/voice-mode`: the composer mic's three modes, in the composer menu's words. */
    private fun voiceRows(context: ComposerCommandContext): List<ComposerCommandRow> =
        listOf(ComposerVoiceMode.Live, ComposerVoiceMode.Dictation, ComposerVoiceMode.HandsFree).map { mode ->
            val available = when (mode) {
                ComposerVoiceMode.Dictation -> true
                ComposerVoiceMode.Live -> context.liveVoiceAvailable
                ComposerVoiceMode.HandsFree -> context.handsFreeAvailable
            }
            ComposerCommandRow(
                id = "voice:${mode.wire}", badge = "Voice", label = voiceModeLabel(mode),
                detail = if (available) null else "Not available here",
                current = mode == context.voiceMode,
                action = if (available) ComposerCommandAction.SetVoiceMode(mode) else ComposerCommandAction.Unavailable,
            )
        }

    /** The composer's model now: a catalog label (Magician/Pi), else the harness model. */
    private fun deviceModelLabel(context: ComposerCommandContext): String? =
        if (context.deviceEngine in catalogEngines) {
            modelLabel(context.catalog(context.deviceEngine), context.deviceSelection)
        } else {
            context.deviceModel
        }

    /** The run engine's model now: Magician's loop model, Pi's selection, else `run_model`. */
    private fun runModelLabel(context: ComposerCommandContext): String? = when (context.runEngine) {
        "magician" -> modelLabel(context.loopModels, context.loopSelection)
        "pi" -> modelLabel(context.catalog("pi"), context.runSelection)
        else -> context.runModel
    }

    private fun modelName(model: ChatModelChoice): String =
        model.label.ifBlank { model.model.ifBlank { model.id } }

    /** A catalog model's label plus its thinking level when it has one (`GPT-6.1 Sol · High`). */
    fun modelLabel(models: List<ChatModelChoice>?, selection: ChatModelSelection?): String? {
        selection ?: return null
        val model = models?.firstOrNull { it.id == selection.modelId } ?: return selection.modelId
        val thinking = selection.thinking.takeIf { it.isNotBlank() && it != "auto" && model.thinking.mode != "none" }
        return modelName(model) + (thinking?.let { " · ${thinkingLevelLabel(it)}" } ?: "")
    }

    /**
     * The thinking level a newly picked model gets: the current one when the
     * model offers it, else the model's default (`auto` when it names none).
     * [operation] reads a routing operation's `operation_choices` too.
     */
    fun keepThinking(model: ChatModelChoice, current: ChatModelSelection?, operation: Boolean = false): String {
        val offered = if (operation) model.choices.keys + model.operationChoices.orEmpty().keys else model.choices.keys
        return current?.thinking?.takeIf { it in offered }
            ?: model.thinking.defaultLevel?.takeIf { it in offered }
            ?: "auto".takeIf { it in offered }
            ?: offered.firstOrNull()
            ?: "auto"
    }

    /** A harness `models[]` entry: `default` is the CLI's own pick. */
    fun harnessModelLabel(model: String): String = if (model == "default") "CLI default" else model

    fun voiceModeLabel(mode: ComposerVoiceMode): String = when (mode) {
        ComposerVoiceMode.Live -> "Live"
        ComposerVoiceMode.Dictation -> "Dictate"
        ComposerVoiceMode.HandsFree -> "Hands-free"
    }

    /** An engine's name in pickers (`claude_code` → `Claude Code`); unknown ids read with spaces. */
    fun engineDisplayName(id: String): String = when (id) {
        "magician" -> "Magician"; "pi" -> "Coding agent"; "claude_code", "claude" -> "Claude Code"
        "codex" -> "Codex"; "codex_app_server" -> "Codex App Server"
        "grok", "grok_acp" -> "Grok"; "agy", "agy_cli" -> "Antigravity"
        else -> id.replace('_', ' ')
    }

    /** `Coding agent · Claude Sonnet`; a missing or `default` model is the engine alone. */
    fun engineLabel(engine: String, model: String?): String =
        if (model.isNullOrBlank() || model == "default") engineDisplayName(engine) else "${engineDisplayName(engine)} · $model"

    /** A picker row: [engineLabel] plus whose credentials a harness engine runs on (`Codex · your login`). */
    fun engineRowLabel(engine: String, model: String, credentials: String?): String =
        HarnessCredentials.engineName(engine, engineLabel(engine, model), credentials)

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

    /** `PUT /plane/engine` / `PUT /plane/chat-engine`; [selection] adds Pi's `model_selection`. */
    fun engineBody(engine: String, model: String, selection: ChatModelSelection? = null): JsonObject = buildJsonObject {
        put("harness_engine", engine)
        put("harness_model", model)
        selection?.let { put("model_selection", selectionJson(it)) }
    }

    /**
     * `PUT /plane/engine` for agentic flows. Pi's run model is always explicit,
     * as Settings sends it: the selection, or JSON null for its own default.
     */
    fun runEngineBody(engine: String, model: String, selection: ChatModelSelection?): JsonObject =
        if (engine == "pi" && selection == null) {
            JsonObject(engineBody(engine, model) + ("model_selection" to JsonNull))
        } else {
            engineBody(engine, model, selection)
        }

    /** `PUT /llm/routing/agentic_decision`. */
    fun loopModelBody(selection: ChatModelSelection): JsonObject = buildJsonObject {
        put("selection", selectionJson(selection))
    }

    private fun selectionJson(selection: ChatModelSelection): JsonObject = buildJsonObject {
        put("model_id", selection.modelId)
        put("thinking", selection.thinking)
    }

    /** [model] is a display label (a catalog model's, or a harness model). */
    fun chatEngineConfirmation(engine: String, model: String?, everywhere: Boolean): String =
        if (everywhere) {
            "Chat engine everywhere: ${engineLabel(engine, model)}"
        } else {
            "Chat engine: ${engineLabel(engine, model)}"
        }

    fun runEngineConfirmation(engine: String, model: String?): String =
        "Agentic flows: " + if (engine == sameAsChat) "Same as Chat" else engineLabel(engine, model)

    fun voiceModeConfirmation(mode: ComposerVoiceMode): String = "Voice mode: ${voiceModeLabel(mode)}"

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
