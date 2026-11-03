package ai.magicbeans.magdroid.chat

import ai.magicbeans.magdroid.voice.ComposerVoiceMode
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.double
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The `/` command picker's pure logic, against
 * `docs/archive/plans/2026-10-03-slash-commands-and-run-budgets.md` and iOS's
 * `ComposerCommandsTests.swift` (same cases).
 */
class ComposerCommandsTest {
    private val sol = ChatModelChoice(
        id = "openai/gpt-6.1-sol", provider = "openai", label = "GPT-6.1 Sol",
        thinking = ChatThinkingControl("levels", listOf("low", "high"), "low"),
        choices = mapOf("auto" to "sol-auto", "low" to "sol-low", "high" to "sol-high"),
    )
    private val sonnet = ChatModelChoice(
        id = "anthropic/claude-sonnet", provider = "anthropic", label = "Claude Sonnet",
        choices = mapOf("auto" to "sonnet"),
    )
    private val offline = ChatModelChoice(
        id = "ollama/tiny", provider = "ollama", label = "Tiny", installed = false,
        choices = mapOf("auto" to "tiny"),
    )

    private val context = ComposerCommandContext(
        engines = listOf(
            ComposerCommandEngine("magician", listOf("default")),
            ComposerCommandEngine("claude_code", listOf("default", "opus")),
        ),
        deviceEngine = "claude_code", deviceModel = "opus",
        runEngine = "magician", runModel = "default",
        tasks = listOf(
            ComposerCommandTask("t-old", "Old research", "running"),
            ComposerCommandTask("t-done", "Done", "completed"),
            ComposerCommandTask("t-plan", "Planned", "planning"),
            ComposerCommandTask("t-new", "New research", "paused"),
            ComposerCommandTask("t-done", "Done", "completed"),
        ),
    )

    @Test
    fun `trigger needs a leading slash on one line`() {
        assertEquals("", ComposerCommands.trigger("/"))
        assertEquals("bud", ComposerCommands.trigger("/bud"))
        assertNull(ComposerCommands.trigger(" /budget"))
        assertNull(ComposerCommands.trigger("hello /budget"))
        assertNull(ComposerCommands.trigger("/budget\nmore"))
        assertNull(ComposerCommands.trigger(""))
    }

    @Test
    fun `command list filters as you type`() {
        assertEquals(
            listOf("/budget", "/budget default", "/engine", "/run-engine", "/voice-mode"),
            ComposerCommands.matches("").map { it.label },
        )
        assertEquals(listOf("/voice-mode"), ComposerCommands.matches("voice").map { it.label })
        assertEquals(listOf("/budget", "/budget default"), ComposerCommands.matches("bud").map { it.label })
        assertEquals(listOf("/engine", "/run-engine"), ComposerCommands.matches("engine").map { it.label })
        assertEquals(listOf("/run-engine"), ComposerCommands.matches("run").map { it.label })
        assertTrue(ComposerCommands.matches("zzz").isEmpty())
    }

    @Test
    fun `rows without a level are the command list`() {
        val state = ComposerCommandState()
        assertEquals(
            listOf(ComposerCommandAction.Open(ComposerCommandId.BudgetDefault)),
            ComposerCommands.rows("/def", state, context).map { it.action },
        )
        assertTrue(ComposerCommands.rows("plain text", state, context).isEmpty())
    }

    @Test
    fun `a partial command opens instead of reaching the model`() {
        val empty = ComposerCommandState()
        assertEquals(ComposerCommandId.Engine, ComposerCommands.partialCommand("/eng", empty))
        assertEquals(ComposerCommandId.BudgetDefault, ComposerCommands.partialCommand("/budget defa", empty))
        assertNull("a message, not a command", ComposerCommands.partialCommand("/budget please help me", empty))
        assertNull(ComposerCommands.partialCommand("/voice-mode is broken", empty))
        assertNull(ComposerCommands.partialCommand("/usr/bin is missing", empty))
        assertNull(ComposerCommands.partialCommand("engine", empty))
        assertTrue(ComposerCommands.holdsSend("/eng", empty))
        assertFalse(ComposerCommands.holdsSend("/usr/bin is missing", empty))
    }

    @Test
    fun `an exact command is intercepted on send`() {
        assertEquals(ComposerCommandId.Budget, ComposerCommands.exactCommand("/budget"))
        assertEquals(ComposerCommandId.BudgetDefault, ComposerCommands.exactCommand(" /budget  default "))
        assertEquals(ComposerCommandId.RunEngine, ComposerCommands.exactCommand("/RUN-ENGINE"))
        assertEquals(ComposerCommandId.VoiceMode, ComposerCommands.exactCommand("/voice-mode"))
        assertNull(ComposerCommands.exactCommand("/budget please"))
        assertNull(ComposerCommands.exactCommand("budget"))
    }

    @Test
    fun `open back and sync walk the tree`() {
        var step = ComposerCommands.open(ComposerCommandId.Budget)
        assertEquals("/budget ", step.text)
        step = ComposerCommands.pickTask("t-new", "New research", step.state)
        assertEquals("/budget ", step.text)
        assertEquals(2, step.state.stack.size)
        val grants = ComposerCommands.rows(step.text, step.state, context)
        assertEquals("Back to tasks", grants.first().label)
        assertEquals(
            listOf("+$25", "+$100", "+1M tokens", "+50 iterations", "Unlimited"),
            grants.drop(1).map { it.label },
        )
        step = ComposerCommands.back(step.state)
        assertEquals("/budget ", step.text)
        assertEquals(listOf<ComposerCommandLevel>(ComposerCommandLevel.Command(ComposerCommandId.Budget)), step.state.stack)
        step = ComposerCommands.back(step.state)
        assertEquals("/", step.text)
        assertTrue(step.state.stack.isEmpty())

        val engine = ComposerCommands.open(ComposerCommandId.Engine).state
        assertEquals(engine, ComposerCommands.sync("/engine op", engine))
        assertTrue("deleting into the prefix closes the level", ComposerCommands.sync("/engin", engine).stack.isEmpty())
    }

    @Test
    fun `budget lists running and paused tasks newest first`() {
        val state = ComposerCommands.open(ComposerCommandId.Budget).state
        val rows = ComposerCommands.rows("/budget ", state, context)
        assertEquals(ComposerCommandAction.Back, rows.first().action)
        assertEquals(
            listOf(
                ComposerCommandAction.PickBudgetTask("t-new", "New research"),
                ComposerCommandAction.PickBudgetTask("t-old", "Old research"),
            ),
            rows.drop(1).map { it.action },
        )
        assertEquals(listOf("back", "task:t-old"), ComposerCommands.rows("/budget old", state, context).map { it.id })
    }

    @Test
    fun `budget with no tasks says so`() {
        val state = ComposerCommands.open(ComposerCommandId.Budget).state
        assertEquals(
            listOf(ComposerCommandAction.Back, ComposerCommandAction.None),
            ComposerCommands.rows("/budget ", state, ComposerCommandContext()).map { it.action },
        )
    }

    @Test
    fun `grant bodies match the contract`() {
        assertEquals(25, BudgetGrant.Usd25.body()["add_cost_usd"]!!.jsonPrimitive.int)
        assertEquals(100, BudgetGrant.Usd100.body()["add_cost_usd"]!!.jsonPrimitive.int)
        assertEquals(1_000_000, BudgetGrant.Tokens1M.body()["add_tokens"]!!.jsonPrimitive.int)
        assertEquals(50, BudgetGrant.Iterations50.body()["add_iterations"]!!.jsonPrimitive.int)
        assertEquals(true, BudgetGrant.Unlimited.body()["unlimited"]!!.jsonPrimitive.boolean)
        assertEquals("Added $25 to Research — resumed", ComposerCommands.grantConfirmation(BudgetGrant.Usd25, "Research", true))
        assertEquals(
            "Removed the budget cap on Research",
            ComposerCommands.grantConfirmation(BudgetGrant.Unlimited, "Research", false),
        )
        assertTrue(ComposerCommands.resumed("""{"execution_id":"e","resumed":true}"""))
        assertTrue(!ComposerCommands.resumed("not json"))
    }

    /** Magician on this device with GPT-6.1 Sol at high thinking; the coding agent is the server's chat engine. */
    private val engineContext = ComposerCommandContext(
        engines = listOf(
            ComposerCommandEngine("magician", listOf("default")),
            ComposerCommandEngine("pi", listOf("default")),
            ComposerCommandEngine("claude_code", listOf("default", "opus")),
            ComposerCommandEngine("codex", listOf("default"), credentials = "login"),
        ),
        deviceEngine = "magician", deviceModel = "default",
        deviceModels = listOf(sol, sonnet, offline),
        deviceSelection = ChatModelSelection(sol.id, "high"),
        serverChatEngine = "pi",
        runEngine = "magician", runModel = "default",
    )

    @Test
    fun `engine level one shows the current choice and one row per engine`() {
        val state = ComposerCommands.open(ComposerCommandId.Engine).state
        val rows = ComposerCommands.rows("/engine ", state, engineContext)
        assertEquals(
            listOf(
                "Back to commands",
                "Now: Magician · GPT-6.1 Sol · High (this device) · everywhere: Coding agent",
                "This device", "Magician", "Coding agent", "Claude Code", "Codex · your login",
            ),
            rows.map { it.label },
        )
        assertEquals(ComposerCommandAction.None, rows[1].action)
        assertEquals(listOf("Magician"), rows.filter { it.current }.map { it.label })
        assertEquals("GPT-6.1 Sol · High", rows.first { it.label == "Magician" }.detail)
        assertEquals(
            listOf(
                ComposerCommandAction.OpenModels("magician", run = false),
                ComposerCommandAction.OpenModels("pi", run = false),
                ComposerCommandAction.OpenModels("claude_code", run = false),
                // A default-only engine is set at level one.
                ComposerCommandAction.SetChatEngine("codex", "default", everywhere = false),
            ),
            rows.drop(3).map { it.action },
        )
        // Typing filters the engines; the note and the scope stay.
        assertEquals(
            listOf("back", "engine:now", "engine:scope", "engine:codex"),
            ComposerCommands.rows("/engine codex", state, engineContext).map { it.id },
        )
    }

    @Test
    fun `engine everywhere picks engines only and says the model stays per device`() {
        val state = ComposerCommands.open(ComposerCommandId.Engine).state.copy(engineEverywhere = true)
        val rows = ComposerCommands.rows("/engine ", state, engineContext)
        assertEquals("Default everywhere", rows[2].label)
        assertEquals("The model stays a choice of this device", rows[1].detail)
        assertEquals(
            listOf(
                ComposerCommandAction.SetChatEngine("magician", "default", everywhere = true),
                ComposerCommandAction.SetChatEngine("pi", "default", everywhere = true),
                ComposerCommandAction.SetChatEngine("claude_code", "default", everywhere = true),
                ComposerCommandAction.SetChatEngine("codex", "default", everywhere = true),
            ),
            rows.drop(3).map { it.action },
        )
    }

    @Test
    fun `engine level two lists the catalog and opens a model's thinking levels`() {
        val open = ComposerCommands.open(ComposerCommandId.Engine)
        val step = ComposerCommands.openModels("magician", run = false, state = open.state)
        assertEquals("/engine magician ", step.text)
        assertEquals(ComposerCommandLevel.EngineModels("magician"), step.state.stack.last())
        val rows = ComposerCommands.rows(step.text, step.state, engineContext)
        assertEquals(listOf("Back to engines", "GPT-6.1 Sol", "Claude Sonnet", "Tiny"), rows.map { it.label })
        assertEquals(listOf("GPT-6.1 Sol"), rows.filter { it.current }.map { it.label })
        // Sol has a thinking choice: it opens its levels.
        assertEquals(ComposerCommandAction.OpenThinking("magician", sol.id), rows[1].action)
        // Sonnet has no thinking control: picked at once.
        assertEquals(
            ComposerCommandAction.SetChatEngine("magician", "default", false, ChatModelSelection(sonnet.id, "auto")),
            rows[2].action,
        )
        assertEquals(ComposerCommandAction.Unavailable, rows[3].action)
        assertEquals("Not available here", rows[3].detail)
        assertTrue(rows[3].inert)
        // Back returns to the engines, not the commands.
        val back = ComposerCommands.back(step.state)
        assertEquals(listOf<ComposerCommandLevel>(ComposerCommandLevel.Command(ComposerCommandId.Engine)), back.state.stack)
        assertEquals("/engine ", back.text)
    }

    @Test
    fun `engine level three lists the thinking levels and picks one`() {
        val models = ComposerCommands.openModels("magician", false, ComposerCommands.open(ComposerCommandId.Engine).state)
        val step = ComposerCommands.openThinking("magician", sol.id, models.state)
        assertEquals("/engine magician ${sol.id} ", step.text)
        assertEquals(ComposerCommandLevel.Thinking("magician", sol.id, run = false), step.state.stack.last())
        val rows = ComposerCommands.rows(step.text, step.state, engineContext)
        assertEquals(listOf("Back to models", "Auto", "Low", "High"), rows.map { it.label })
        assertEquals("Default · Low", rows[1].detail)
        // Sol is the model in use: its level is marked.
        assertEquals(listOf("High"), rows.filter { it.current }.map { it.label })
        assertEquals(
            ComposerCommandAction.SetChatEngine("magician", "default", false, ChatModelSelection(sol.id, "low")),
            rows[2].action,
        )
        assertTrue(ComposerCommands.holdsSend("/engine magician ${sol.id} hi", step.state))
        val back = ComposerCommands.back(step.state)
        assertEquals("/engine magician ", back.text)
        assertEquals(ComposerCommandLevel.EngineModels("magician"), back.state.stack.last())
        // Not the model in use: its default is marked.
        val other = engineContext.copy(deviceSelection = ChatModelSelection(sonnet.id, "auto"))
        assertEquals(listOf("Low"), ComposerCommands.rows(step.text, step.state, other).filter { it.current }.map { it.label })
    }

    @Test
    fun `an open level holds the send`() {
        assertTrue(ComposerCommands.holdsSend("/engine", ComposerCommandState()))
        assertTrue(!ComposerCommands.holdsSend("hello", ComposerCommandState()))
        val state = ComposerCommands.openModels("pi", false, ComposerCommands.open(ComposerCommandId.Engine).state).state
        assertTrue(ComposerCommands.holdsSend("/engine pi ", state))
        assertTrue(ComposerCommands.holdsSend("/engine pi son", state))
    }

    @Test
    fun `engine level two for another catalog engine loads and fails visibly`() {
        val state = ComposerCommands.openModels("pi", false, ComposerCommands.open(ComposerCommandId.Engine).state).state
        var rows = ComposerCommands.rows("/engine ", state, engineContext)
        assertEquals(listOf("Back to engines", "Loading models…"), rows.map { it.label })
        assertEquals(ComposerCommandAction.None, rows[1].action)

        rows = ComposerCommands.rows("/engine ", state, engineContext.copy(modelCatalogErrors = mapOf("pi" to "boom")))
        assertEquals("Models unavailable: boom", rows[1].label)

        rows = ComposerCommands.rows("/engine ", state, engineContext.copy(modelCatalogs = mapOf("pi" to listOf(sonnet))))
        assertEquals(listOf("Claude Sonnet"), rows.drop(1).map { it.label })
        assertTrue("the coding agent is not this device's engine", rows.none { it.current })
        assertEquals(
            ComposerCommandAction.SetChatEngine("pi", "default", false, ChatModelSelection(sonnet.id, "auto")),
            rows[1].action,
        )
    }

    @Test
    fun `engine level two for a harness lists the roster models`() {
        val state = ComposerCommands.openModels("claude_code", false, ComposerCommands.open(ComposerCommandId.Engine).state).state
        val rows = ComposerCommands.rows("/engine ", state, engineContext)
        assertEquals(listOf("Back to engines", "CLI default", "opus"), rows.map { it.label })
        assertEquals(ComposerCommandAction.SetChatEngine("claude_code", "opus", false), rows.last().action)
    }

    @Test
    fun `run engine level one shows the run engine and offers same as chat`() {
        val state = ComposerCommands.open(ComposerCommandId.RunEngine).state
        val loop = engineContext.copy(loopModels = listOf(sol, sonnet), loopSelection = ChatModelSelection(sol.id, "low"))
        var rows = ComposerCommands.rows("/run-engine ", state, loop)
        assertEquals(
            listOf(
                "Back to commands", "Now: Magician · GPT-6.1 Sol · Low",
                "Same as Chat", "Magician", "Coding agent", "Claude Code", "Codex · your login",
            ),
            rows.map { it.label },
        )
        assertEquals("Coding agent", rows[2].detail)
        assertEquals(listOf("Magician"), rows.filter { it.current }.map { it.label })
        assertEquals(
            listOf(
                ComposerCommandAction.SetRunEngine("chat", "default"),
                ComposerCommandAction.OpenModels("magician", run = true),
                ComposerCommandAction.OpenModels("pi", run = true),
                ComposerCommandAction.OpenModels("claude_code", run = true),
                ComposerCommandAction.SetRunEngine("codex", "default"),
            ),
            rows.drop(2).map { it.action },
        )

        rows = ComposerCommands.rows("/run-engine ", state, loop.copy(runFollowsChat = true))
        assertEquals("Now: Same as Chat (Coding agent)", rows[1].label)
        assertEquals(listOf("Same as Chat"), rows.filter { it.current }.map { it.label })

        rows = ComposerCommands.rows("/run-engine ", state, loop.copy(runEngine = "claude_code", runModel = "opus"))
        assertEquals("Now: Claude Code · opus", rows[1].label)
    }

    @Test
    fun `run engine magician picks the agentic decision model`() {
        val state = ComposerCommands.openModels("magician", true, ComposerCommands.open(ComposerCommandId.RunEngine).state).state
        assertEquals(ComposerCommandLevel.RunModels("magician"), state.stack.last())
        assertEquals(
            listOf("Back to engines", "Loading models…"),
            ComposerCommands.rows("/run-engine ", state, engineContext).map { it.label },
        )
        assertEquals(
            "Models unavailable: routing down",
            ComposerCommands.rows("/run-engine ", state, engineContext.copy(loopModelsError = "routing down"))[1].label,
        )
        val loop = engineContext.copy(loopModels = listOf(sol, sonnet), loopSelection = ChatModelSelection(sol.id, "low"))
        val rows = ComposerCommands.rows("/run-engine magician ", state, loop)
        assertEquals(listOf("GPT-6.1 Sol"), rows.filter { it.current }.map { it.label })
        assertEquals(
            listOf(
                ComposerCommandAction.OpenThinking("magician", sol.id),
                ComposerCommandAction.SetLoopModel(ChatModelSelection(sonnet.id, "auto")),
            ),
            rows.drop(1).map { it.action },
        )
        assertTrue(ComposerCommandAction.SetLoopModel(ChatModelSelection(sol.id, "low")).terminal)
        // The loop model's thinking level.
        val levels = ComposerCommands.openThinking("magician", sol.id, state)
        assertEquals(ComposerCommandLevel.Thinking("magician", sol.id, run = true), levels.state.stack.last())
        val levelRows = ComposerCommands.rows(levels.text, levels.state, loop)
        assertEquals(listOf("Low"), levelRows.filter { it.current }.map { it.label })
        assertEquals(ComposerCommandAction.SetLoopModel(ChatModelSelection(sol.id, "high")), levelRows[3].action)
        // In force only while runs resolve to Magician: its engine, or Same as Chat with a Magician chat default.
        assertTrue(ComposerCommands.runResolvesToMagician(loop))
        val followsPi = loop.copy(runFollowsChat = true)
        assertTrue(!ComposerCommands.runResolvesToMagician(followsPi))
        assertTrue(ComposerCommands.rows("/run-engine magician ", state, followsPi).none { it.current })
        assertTrue(ComposerCommands.runResolvesToMagician(followsPi.copy(serverChatEngine = "magician")))
    }

    @Test
    fun `run engine pi carries a model selection`() {
        val state = ComposerCommands.openModels("pi", true, ComposerCommands.open(ComposerCommandId.RunEngine).state).state
        val rows = ComposerCommands.rows(
            "/run-engine pi ", state, engineContext.copy(modelCatalogs = mapOf("pi" to listOf(sol, sonnet))),
        )
        assertEquals(listOf("Back to engines", "Default", "GPT-6.1 Sol", "Claude Sonnet"), rows.map { it.label })
        // "Default" clears the run model (Settings' "Use default").
        assertEquals("The coding agent's own default model", rows[1].detail)
        assertEquals(ComposerCommandAction.SetRunEngine("pi", "default", null), rows[1].action)
        assertEquals(ComposerCommandAction.OpenThinking("pi", sol.id), rows[2].action)
        assertEquals(ComposerCommandAction.SetRunEngine("pi", "default", ChatModelSelection(sonnet.id, "auto")), rows[3].action)
        // No run selection yet: the model's own default level is marked.
        val levels = ComposerCommands.openThinking("pi", sol.id, state)
        val levelRows = ComposerCommands.rows(levels.text, levels.state, engineContext.copy(modelCatalogs = mapOf("pi" to listOf(sol))))
        assertEquals(listOf("Low"), levelRows.filter { it.current }.map { it.label })
        assertEquals(ComposerCommandAction.SetRunEngine("pi", "default", ChatModelSelection(sol.id, "low")), levelRows[2].action)
        val cleared = ComposerCommands.runEngineBody("pi", "default", null)
        assertEquals(kotlinx.serialization.json.JsonNull, cleared["model_selection"])
        assertNull(ComposerCommands.runEngineBody("codex", "default", null)["model_selection"])
        val onDefault = engineContext.copy(modelCatalogs = mapOf("pi" to listOf(sol)), runEngine = "pi", runSelection = null)
        assertEquals(listOf("Default"), ComposerCommands.rows("/run-engine pi ", state, onDefault).filter { it.current }.map { it.label })
        val body = ComposerCommands.engineBody("pi", "default", ChatModelSelection(sol.id, "low"))
        assertEquals("pi", body["harness_engine"]!!.jsonPrimitive.content)
        assertEquals("default", body["harness_model"]!!.jsonPrimitive.content)
        val selection = body["model_selection"]!!.jsonObject
        assertEquals(sol.id, selection["model_id"]!!.jsonPrimitive.content)
        assertEquals("low", selection["thinking"]!!.jsonPrimitive.content)
        assertNull(ComposerCommands.engineBody("codex", "default")["model_selection"])

        val marked = ComposerCommands.rows(
            "/run-engine ", state,
            engineContext.copy(
                modelCatalogs = mapOf("pi" to listOf(sol)), runEngine = "pi",
                runSelection = ChatModelSelection(sol.id, "high"),
            ),
        )
        assertEquals(listOf("GPT-6.1 Sol"), marked.filter { it.current }.map { it.label })
        assertEquals(ComposerCommandAction.OpenThinking("pi", sol.id), marked[2].action)
    }

    @Test
    fun `run engine for a harness lists the roster models`() {
        val state = ComposerCommands.openModels("claude_code", true, ComposerCommands.open(ComposerCommandId.RunEngine).state).state
        val rows = ComposerCommands.rows("/run-engine ", state, engineContext.copy(runEngine = "claude_code", runModel = "opus"))
        assertEquals(listOf("Back to engines", "CLI default", "opus"), rows.map { it.label })
        assertEquals(listOf("opus"), rows.filter { it.current }.map { it.label })
        assertEquals(ComposerCommandAction.SetRunEngine("claude_code", "opus"), rows.last().action)
        val body = ComposerCommands.engineBody("claude_code", "opus")
        assertEquals("claude_code", body["harness_engine"]!!.jsonPrimitive.content)
        assertEquals("opus", body["harness_model"]!!.jsonPrimitive.content)
    }

    @Test
    fun `voice mode marks the current mode and disables unavailable ones`() {
        val state = ComposerCommands.open(ComposerCommandId.VoiceMode).state
        val rows = ComposerCommands.rows(
            "/voice-mode ", state,
            engineContext.copy(
                voiceMode = ComposerVoiceMode.Dictation, liveVoiceAvailable = false, handsFreeAvailable = true,
            ),
        )
        assertEquals(listOf("Back to commands", "Live", "Dictate", "Hands-free"), rows.map { it.label })
        assertEquals(listOf("Dictate"), rows.filter { it.current }.map { it.label })
        assertEquals("Not available here", rows[1].detail)
        assertEquals(
            listOf(
                ComposerCommandAction.Unavailable,
                ComposerCommandAction.SetVoiceMode(ComposerVoiceMode.Dictation),
                ComposerCommandAction.SetVoiceMode(ComposerVoiceMode.HandsFree),
            ),
            rows.drop(1).map { it.action },
        )
        assertTrue(ComposerCommandAction.SetVoiceMode(ComposerVoiceMode.HandsFree).terminal)
    }

    @Test
    fun `keep thinking keeps an offered level, else the model's default`() {
        assertEquals("high", ComposerCommands.keepThinking(sol, ChatModelSelection(sonnet.id, "high")))
        assertEquals("low", ComposerCommands.keepThinking(sol, ChatModelSelection(sonnet.id, "xhigh")))
        assertEquals("low", ComposerCommands.keepThinking(sol, null))
        assertEquals("auto", ComposerCommands.keepThinking(sonnet, ChatModelSelection(sol.id, "high")))
    }

    @Test
    fun `confirmations name the engine and model`() {
        assertEquals("Chat engine: Coding agent · Claude Sonnet", ComposerCommands.chatEngineConfirmation("pi", "Claude Sonnet", false))
        assertEquals(
            "Chat engine: Coding agent · GPT-6.1 Sol · High",
            ComposerCommands.chatEngineConfirmation("pi", ComposerCommands.modelLabel(listOf(sol), ChatModelSelection(sol.id, "high")), false),
        )
        assertEquals("Chat engine: Codex", ComposerCommands.chatEngineConfirmation("codex", "default", false))
        assertEquals("Chat engine everywhere: Coding agent", ComposerCommands.chatEngineConfirmation("pi", null, true))
        assertEquals("Agentic flows: Coding agent · Default", ComposerCommands.runEngineConfirmation("pi", "Default"))
        assertEquals(
            "Agentic flows: Magician · GPT-6.1 Sol",
            ComposerCommands.runEngineConfirmation("magician", "GPT-6.1 Sol"),
        )
        assertEquals("Agentic flows: Same as Chat", ComposerCommands.runEngineConfirmation("chat", "default"))
        assertEquals("Voice mode: Hands-free", ComposerCommands.voiceModeConfirmation(ComposerVoiceMode.HandsFree))
        assertEquals("GPT-6.1 Sol · High", ComposerCommands.modelLabel(listOf(sol), ChatModelSelection(sol.id, "high")))
        assertEquals("GPT-6.1 Sol", ComposerCommands.modelLabel(listOf(sol), ChatModelSelection(sol.id, "auto")))
        assertEquals("x/unknown", ComposerCommands.modelLabel(listOf(sol), ChatModelSelection("x/unknown", "high")))
    }

    @Test
    fun `loop routing reads the agentic decision model`() {
        val body = """{"models":[{"id":"openai/gpt-6.1-sol","label":"GPT-6.1 Sol","choices":{"auto":"sol-auto"},
            "operation_choices":{"low":"sol-op-low"}}],"profiles":[{"name":"sol-auto"}],
            "operations":[{"operation":"query_analysis","effective_profile":"sol-auto"},
            {"operation":"agentic_decision","effective_profile":"sol-op-low"}]}"""
        val routing = LoopModelRouting.decode(body)
        assertEquals(listOf("openai/gpt-6.1-sol"), routing.models.map { it.id })
        assertEquals(ChatModelSelection("openai/gpt-6.1-sol", "low"), routing.selection)
        assertNull(routing.error)

        val saved = LoopModelRouting.decode(
            """{"models":[{"id":"m","choices":{"auto":"r"}}],"profiles":[{"name":"p","model_selection":{"model_id":"m","thinking":"auto"}}],
            "operations":[{"operation":"agentic_decision","effective_profile":"p"}]}""",
        )
        assertEquals(ChatModelSelection("m", "auto"), saved.selection)

        val missing = LoopModelRouting.decode("""{"models":[],"operations":[]}""")
        assertEquals("The agentic decision mapping is not in the model catalog.", missing.error)
        assertEquals("Model routing could not be read.", LoopModelRouting.decode("nope").error)
        val selectionBody = ComposerCommands.loopModelBody(ChatModelSelection("m", "auto"))["selection"]!!.jsonObject
        assertEquals("m", selectionBody["model_id"]!!.jsonPrimitive.content)
    }

    @Test
    fun `the roster decodes the run selection and the chat default`() {
        val roster = chatJson.decodeFromString(
            ChatHarnessRoster.serializer(),
            """{"engines":[],"current":"pi","follows_chat":true,"chat_current":"pi","chat_model":"default",
            "run_model":"default","run_model_selection":{"model_id":"m","thinking":"high"}}""",
        )
        assertEquals(true, roster.followsChat)
        assertEquals("pi", roster.chatCurrent)
        assertEquals(ChatModelSelection("m", "high"), roster.runModelSelection)
        val cleared = chatJson.decodeFromString(ChatHarnessRoster.serializer(), """{"run_model_selection":null}""")
        assertNull(cleared.runModelSelection)
    }

    @Test
    fun `budget defaults decode, mark and encode`() {
        val defaults = BudgetDefaults.decode("""{"native_max_cost_usd":15,"harness_max_cost_usd":"unlimited"}""")
        assertEquals(BudgetDefaults(native = BudgetCap.Usd(15.0), harness = BudgetCap.Unlimited), defaults)

        val state = ComposerCommands.open(ComposerCommandId.BudgetDefault).state
        val rows = ComposerCommands.rows("/budget default ", state, context.copy(budgetDefaults = defaults))
        assertEquals(
            listOf("$50", "$100", "$250", "Unlimited", "$5", "$15", "$50", "Unlimited"),
            rows.drop(1).map { it.label },
        )
        assertEquals(listOf("default:harness:Unlimited", "default:native:$15"), rows.filter { it.current }.map { it.id })

        val body = ComposerCommands.defaultsBody(BudgetRunKind.Harness, BudgetCap.Usd(250.0), defaults)
        assertEquals(250.0, body["harness_max_cost_usd"]!!.jsonPrimitive.double, 0.0)
        assertEquals(15.0, body["native_max_cost_usd"]!!.jsonPrimitive.double, 0.0)
        val unlimited = ComposerCommands.defaultsBody(BudgetRunKind.Native, BudgetCap.Unlimited, null)
        assertEquals("unlimited", unlimited["native_max_cost_usd"]!!.jsonPrimitive.content)
        assertNull(unlimited["harness_max_cost_usd"])
    }

    @Test
    fun `a defaults error shows as a note row`() {
        val state = ComposerCommands.open(ComposerCommandId.BudgetDefault).state
        val rows = ComposerCommands.rows("/budget default ", state, context.copy(budgetDefaultsError = "route not found"))
        assertEquals("route not found", rows[1].detail)
        assertEquals(ComposerCommandAction.None, rows[1].action)
    }

    @Test
    fun `server error message prefers the server's words`() {
        assertEquals(
            "`grok` is not installed",
            ComposerCommands.serverErrorMessage(400, """{"error":"harness_engine_not_installed","message":"`grok` is not installed"}"""),
        )
        assertEquals("task_not_found", ComposerCommands.serverErrorMessage(404, """{"error":"task_not_found"}"""))
        assertEquals("bad gateway", ComposerCommands.serverErrorMessage(502, "bad gateway"))
        assertEquals("Not available on this Magician (HTTP 404)", ComposerCommands.serverErrorMessage(404, null))
    }

    @Test
    fun `the chat state builds its context from task cards and the roster`() {
        val state = ChatUiState(
            chatHarnesses = listOf(ChatHarnessOption("magician", true), ChatHarnessOption("grok", false)),
            messages = listOf(
                ChatMessage(
                    id = "m1", fromUser = false, text = "", kind = MessageKind.TaskStatus,
                    task = TaskStatusCard(taskId = "t1", title = "Research", status = "running"),
                ),
            ),
        )
        val ctx = state.commandContext()
        assertEquals(listOf("magician"), ctx.engines.map { it.name })
        assertEquals(listOf(ComposerCommandTask("t1", "Research", "running")), ctx.tasks)
        assertEquals(ComposerVoiceMode.Dictation, ctx.voiceMode)

        val voiced = state.copy(
            profiles = ChatModelCatalog(listOf(sol)).rows(), selectedProfile = "sol-high",
        ).commandContext(ComposerCommandVoice(ComposerVoiceMode.Live, liveVoiceAvailable = true, handsFreeAvailable = false))
        assertEquals(ChatModelSelection(sol.id, "high"), voiced.deviceSelection)
        assertEquals(listOf(sol.id), voiced.deviceModels.map { it.id })
        assertEquals(ComposerVoiceMode.Live, voiced.voiceMode)
        assertTrue(!voiced.handsFreeAvailable)
    }
}
