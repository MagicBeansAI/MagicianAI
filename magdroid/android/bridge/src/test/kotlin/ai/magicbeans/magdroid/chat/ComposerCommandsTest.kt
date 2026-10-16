package ai.magicbeans.magdroid.chat

import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.double
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The `/` command picker's pure logic, against
 * `docs/plans/2026-10-03-slash-commands-and-run-budgets.md` and iOS's
 * `ComposerCommandsTests.swift` (same cases).
 */
class ComposerCommandsTest {
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
            listOf("/budget", "/budget default", "/engine", "/run-engine"),
            ComposerCommands.matches("").map { it.label },
        )
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
    fun `an exact command is intercepted on send`() {
        assertEquals(ComposerCommandId.Budget, ComposerCommands.exactCommand("/budget"))
        assertEquals(ComposerCommandId.BudgetDefault, ComposerCommands.exactCommand(" /budget  default "))
        assertEquals(ComposerCommandId.RunEngine, ComposerCommands.exactCommand("/RUN-ENGINE"))
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

    @Test
    fun `engine rows mark the device choice and carry the scope`() {
        var state = ComposerCommands.open(ComposerCommandId.Engine).state
        var rows = ComposerCommands.rows("/engine ", state, context)
        assertEquals(
            listOf("Back to commands", "This device", "magician", "claude_code", "claude_code · opus"),
            rows.map { it.label },
        )
        assertEquals(listOf("claude_code · opus"), rows.filter { it.current }.map { it.label })
        assertEquals(ComposerCommandAction.SetChatEngine("claude_code", "opus", false), rows.last().action)
        state = state.copy(engineEverywhere = true)
        rows = ComposerCommands.rows("/engine ", state, context)
        assertEquals("Default everywhere", rows[1].label)
        assertEquals(ComposerCommandAction.SetChatEngine("claude_code", "opus", true), rows.last().action)
    }

    @Test
    fun `run engine rows mark the run engine`() {
        val state = ComposerCommands.open(ComposerCommandId.RunEngine).state
        val rows = ComposerCommands.rows("/run-engine ", state, context)
        assertEquals(listOf("magician"), rows.filter { it.current }.map { it.label })
        assertEquals(ComposerCommandAction.SetRunEngine("claude_code", "opus"), rows.last().action)
        val body = ComposerCommands.engineBody("claude_code", "opus")
        assertEquals("claude_code", body["harness_engine"]!!.jsonPrimitive.content)
        assertEquals("opus", body["harness_model"]!!.jsonPrimitive.content)
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
    }
}
