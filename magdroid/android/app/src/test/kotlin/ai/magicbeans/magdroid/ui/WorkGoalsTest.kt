package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.WorkHelpTopic
import ai.magicbeans.magdroid.work.WorkSection
import ai.magicbeans.magdroid.work.WorkWebContext
import ai.magicbeans.magdroid.work.help
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import java.net.URLDecoder

/** The web's `goals.test.ts` and the Goals screen copy, against the Android port. */
class WorkGoalsTest {
    private val main = """
        ---
        magician_kind: goal
        workspace: acme
        kind: main
        owner: user
        status: active
        target: 2026-12-01
        visibility: private
        ---
        # Raise a seed round
        Why it matters.
    """.trimIndent()

    private val sub = """
        ---
        magician_kind: goal
        kind: sub
        parent: "[[Raise a seed round]]"
        peers: ["[[Find design partners]]"]
        owner: cro
        status: blocked
        visibility: internal
        program: fundraising.md
        goal_id: harness:cro:fundraising
        metrics:
          - investor_meetings
        ---
        # Apply to accelerators
    """.trimIndent()

    private fun goalNamed(name: String, parent: String?): WorkGoal =
        workParseGoalNote("---\nmagician_kind: goal\n${parent?.let { "parent: \"[[$it]]\"\n" } ?: ""}---\n# $name", "G/$name.md")!!
    private fun names(nodes: List<WorkGoalNode>): List<String> = nodes.flatMap { listOf(it.goal.name) + names(it.children) }
    private fun reasons(tree: WorkGoalTree) = tree.orphans.map { it.goal.name to it.reason.key }
    private fun goalWith(name: String, parent: String?, status: String, closed: String? = null): WorkGoal =
        workParseGoalNote("---\nmagician_kind: goal\n${parent?.let { "parent: \"[[$it]]\"\n" } ?: ""}status: $status\n${closed?.let { "closed: $it\n" } ?: ""}---\n# $name", "G/$name.md")!!

    // Goal notes

    @Test fun readsFrontmatterFieldsAndTheTitle() {
        val goal = workParseGoalNote(main, "Workspaces/acme/Goals/Raise a seed round.md")!!
        assertEquals("Raise a seed round", goal.name); assertEquals("Raise a seed round", goal.title)
        assertEquals(WorkGoalKind.Main, goal.kind); assertEquals("user", goal.owner); assertEquals("active", goal.status)
        assertEquals("2026-12-01", goal.target); assertEquals(WorkGoalVisibility.Private, goal.visibility)
        assertNull(goal.parent); assertEquals(emptyList<String>(), goal.peers); assertEquals("acme", goal.workspace)
    }

    @Test fun readsLinksInlineListsAndBlockLists() {
        val goal = workParseGoalNote(sub, "Workspaces/acme/Goals/Apply to accelerators.md")!!
        assertEquals("Raise a seed round", goal.parent); assertEquals(listOf("Find design partners"), goal.peers)
        assertEquals("fundraising.md", goal.program); assertEquals("harness:cro:fundraising", goal.goalId)
        assertEquals(listOf("investor_meetings"), goal.metrics); assertEquals(WorkGoalKind.Sub, goal.kind)
        assertEquals(WorkGoalVisibility.Internal, goal.visibility)
    }

    @Test fun ignoresNotesThatAreNotGoals() {
        assertNull(workParseGoalNote("---\nmagician_kind: note\n---\n# x", "a.md"))
        assertNull(workParseGoalNote("# no frontmatter", "b.md"))
    }

    @Test fun stripsLinkBracketsAndAliases() {
        assertEquals("Raise a seed round", workGoalLinkTarget("[[Raise a seed round|seed]]"))
        assertEquals("plain", workGoalLinkTarget("plain"))
    }

    @Test fun dropsCommentsKeepsQuotedHashesAndReadsBareLinksAndUnindentedLists() {
        val goal = workParseGoalNote("""
            ---
            # written by the goal template
            magician_kind: goal # a goal
            parent: [[Raise a seed round]]
            peers: [[[Find design partners]], "[[Hire a designer|design]]"]
            status: "Blocked #2"
            owner: Founder's assistant
            metrics:
            - investor_meetings # weekly
            - "pipeline, qualified"
            ---
            Body without a heading.
        """.trimIndent(), "Workspaces/acme/Goals/Close the round.md")!!
        assertEquals("Close the round", goal.title); assertEquals(WorkGoalKind.Sub, goal.kind); assertEquals("Raise a seed round", goal.parent)
        assertEquals(listOf("Find design partners", "Hire a designer"), goal.peers); assertEquals("blocked #2", goal.status)
        assertEquals("Founder's assistant", goal.owner); assertEquals(listOf("investor_meetings", "pipeline, qualified"), goal.metrics)
        assertEquals(WorkGoalVisibility.Private, goal.visibility)
    }

    @Test fun keepsAHashInsideALaterQuotedFlowListItem() {
        assertEquals(listOf("a", "b #2"), workParseGoalNote("---\nmagician_kind: goal\npeers: [\"a\", \"b #2\"] # peers\n---\n# x", "x.md")?.peers)
    }

    @Test fun titleOnlyFromAnH1OnTheFirstNonBlankLineAndAFenceWithTrailingSpaces() {
        assertEquals("Ship the beta", workParseGoalNote("---  \nmagician_kind: goal\n---\n\n# Ship the beta\n", "G/beta.md")?.title)
        assertEquals("Ship the beta", workParseGoalNote("---\nmagician_kind: goal\n---\nIntro first.\n# Later heading", "G/Ship the beta.md")?.title)
    }

    @Test fun readsCrlfNotesAndAByteOrderMark() {
        val goal = workParseGoalNote("﻿---\r\nmagician_kind: goal\r\nparent: \"[[Plan]]\"\r\n---\r\n# Windows goal\r\n", "G/w.md")
        assertEquals("Windows goal", goal?.title); assertEquals("Plan", goal?.parent); assertEquals(WorkGoalKind.Sub, goal?.kind)
    }

    // Tree

    @Test fun resolvesASharedLinkKeyToTheFirstGoalListed() {
        val first = workParseGoalNote("---\nmagician_kind: goal\n---\n# First", "G/Plan.md")!!
        val second = workParseGoalNote("---\nmagician_kind: goal\n---\n# Second", "H/plan.md")!!
        assertEquals(first, workBuildGoalTree(listOf(first, second)).byKey["plan"])
    }

    @Test fun nestsSubGoalsUnderTheirParentAndKeepsOrphansVisible() {
        val lost = sub.replace("Raise a seed round", "Missing parent").replace("Apply to accelerators", "Lost")
        val tree = workBuildGoalTree(listOf(workParseGoalNote(main, "G/Raise a seed round.md")!!, workParseGoalNote(sub, "G/Apply to accelerators.md")!!, workParseGoalNote(lost, "G/Lost.md")!!))
        assertEquals(listOf("Raise a seed round"), tree.roots.map { it.goal.name })
        assertEquals(listOf("Apply to accelerators"), tree.roots[0].children.map { it.goal.name })
        assertEquals(listOf("Lost" to "missing-parent"), reasons(tree))
    }

    @Test fun headsANotLinkedTreeAtAMissingParentAndKeepsItsSubGoalsNested() {
        val tree = workBuildGoalTree(listOf(goalNamed("C", "B"), goalNamed("B", "Missing"), goalNamed("D", "C")))
        assertEquals(emptyList<WorkGoalNode>(), tree.roots)
        assertEquals(listOf("B" to "missing-parent"), reasons(tree))
        assertEquals(listOf("C"), tree.orphans[0].node.children.map { it.goal.name })
        assertEquals(listOf("D"), tree.orphans[0].node.children[0].children.map { it.goal.name })
        assertEquals("Parent goal not found: Missing", workGoalOrphanNote(tree.orphans[0]))
    }

    @Test fun headsASelfParentedGoalAndOneMemberOfEachParentLoopPlacingEveryGoalOnce() {
        val tree = workBuildGoalTree(listOf(goalNamed("Loop", "Loop"), goalNamed("Pong", "Ping"), goalNamed("Ping", "Pong"), goalNamed("Tail", "Pong"), goalNamed("Root", null), goalNamed("Under loop", "Loop")))
        assertEquals(listOf("Root"), tree.roots.map { it.goal.name })
        assertEquals(listOf("Loop" to "self-parent", "Ping" to "parent-loop"), reasons(tree))
        assertEquals(listOf("Under loop"), tree.orphans[0].node.children.map { it.goal.name })
        val ping = tree.orphans[1].node
        assertEquals(listOf("Pong"), ping.children.map { it.goal.name })
        assertEquals(listOf("Tail"), ping.children[0].children.map { it.goal.name })
        assertEquals(listOf("Loop", "Ping", "Pong", "Root", "Tail", "Under loop"), names(tree.roots + tree.orphans.map { it.node }).sorted())
        assertEquals(listOf("Names itself as its parent.", "Part of a parent loop: its parent goals lead back to it."), tree.orphans.map(::workGoalOrphanNote))
    }

    @Test fun matchesAParentLinkByNoteNameIgnoringFoldersHeadingsExtensionAndCase() {
        val tree = workBuildGoalTree(listOf(goalNamed("Raise a seed round", null), goalNamed("Pitch deck", "Workspaces/acme/Goals/raise a seed round.md#Why")))
        assertEquals(listOf("Pitch deck"), tree.roots[0].children.map { it.goal.name })
        assertEquals(emptyList<WorkGoalOrphan>(), tree.orphans)
    }

    @Test fun linksAProgramByItsDocumentName() {
        assertEquals("fundraising.md", workGoalProgramName("fundraising.md"))
        assertEquals("hiring plan.md", workGoalProgramName("hiring plan"))
    }

    // Closed goals

    @Test fun readsTheClosedDateAndTreatsDoneAndDroppedAsClosed() {
        val done = workParseGoalNote("---\nmagician_kind: goal\nstatus: Done\nclosed: \"2026-10-20\"\n---\n# Shipped", "G/Shipped.md")!!
        assertEquals("done", done.status); assertEquals("2026-10-20", done.closed); assertTrue(workIsClosedGoal(done))
        assertTrue(workIsClosedGoal(goalWith("Gone", null, "dropped")))
        assertNull(goalWith("Gone", null, "dropped").closed)
        assertNull(workParseGoalNote("---\nmagician_kind: goal\nclosed: ~\n---\n# x", "x.md")!!.closed)
        listOf("active", "proposed", "blocked", "paused").forEach { assertFalse(it, workIsClosedGoal(goalWith("Open", null, it))) }
        assertEquals(listOf("done", "dropped"), WORK_CLOSED_GOAL_STATUSES)
    }

    @Test fun openTreeLiftsOpenGoalsToTheirNearestOpenAncestorAndNamesTheClosedGoal() {
        val tree = workOpenGoalTree(workBuildGoalTree(listOf(
            goalWith("Main", null, "active"),
            goalWith("Closed sub", "Main", "done"), goalWith("Under closed", "Closed sub", "active"), goalWith("Grandchild", "Under closed", "active"),
            goalWith("Dropped", "Main", "dropped"), goalWith("Done below dropped", "Dropped", "done"), goalWith("Deep", "Done below dropped", "proposed"),
            goalWith("Old main", null, "done"), goalWith("Freed", "Old main", "active"),
            goalWith("Open sub", "Main", "active"),
        )))
        assertEquals(listOf("Main", "Freed"), tree.roots.map { it.goal.name })
        val main = tree.roots[0]
        assertNull(main.closedParent)
        assertEquals(listOf("Under closed" to "Closed sub", "Deep" to "Done below dropped", "Open sub" to null), main.children.map { it.goal.name to it.closedParent })
        // An open goal under an open one keeps no closed parent of its own.
        assertEquals(listOf("Grandchild" to null), main.children[0].children.map { it.goal.name to it.closedParent })
        assertEquals("Old main", tree.roots[1].closedParent)
        assertEquals(listOf("Deep", "Freed", "Grandchild", "Main", "Open sub", "Under closed"), names(tree.roots).sorted())
        // byKey still resolves closed goals, for the Closed list's "Under …".
        assertEquals("Closed sub", tree.byKey["closed sub"]?.title)
    }

    @Test fun openTreeKeepsANotLinkedReasonAndSaysTheParentIsClosedInstead() {
        val tree = workOpenGoalTree(workBuildGoalTree(listOf(
            goalWith("Stray", "Missing", "active"),
            goalWith("Closed stray", "Missing", "done"), goalWith("Left behind", "Closed stray", "blocked"),
            goalWith("Closed alone", "Missing", "dropped"),
        )))
        assertEquals(emptyList<WorkGoalNode>(), tree.roots)
        assertEquals(listOf("Stray" to "missing-parent", "Left behind" to "missing-parent"), reasons(tree))
        assertEquals(listOf("Parent goal not found: Missing", "Its parent goal “Closed stray” is closed."), tree.orphans.map(::workGoalOrphanNote))
        assertEquals("Its parent goal “Closed stray” is closed.", WorkGoalCopy.closedParent("Closed stray"))
    }

    @Test fun closedGoalsAreMostRecentlyClosedFirstThenUndatedByTitle() {
        val goals = listOf(
            goalWith("Open", null, "active"),
            goalWith("beta", null, "done"), goalWith("Older", null, "done", "2026-09-01"),
            goalWith("Alpha", null, "dropped"), goalWith("Newest", null, "dropped", "2026-10-27"), goalWith("Mid", null, "done", "2026-10-02"),
        )
        assertEquals(listOf("Newest", "Mid", "Older", "Alpha", "beta"), workClosedGoals(goals).map { it.title })
        val shown = workGoalShown(WorkGoalWorkspace("acme", "Workspaces/acme", goals, emptyList(), 0, 0))
        assertEquals(listOf("Open"), shown.tree.roots.map { it.goal.name })
        assertEquals(5, shown.closed.size)
    }

    @Test fun aWorkspaceWhoseGoalsAreAllClosedIsListedOnlyUnderClosed() {
        fun space(goals: List<WorkGoal>, problems: List<String> = emptyList(), notShown: Int = 0, foldersNotRead: Int = 0) =
            WorkGoalWorkspace("w", "Workspaces/w", goals, problems, notShown, foldersNotRead)
        val allClosed = space(listOf(goalWith("Main", null, "done"), goalWith("Step", "Main", "dropped")))
        val mixed = space(listOf(goalWith("Main", null, "done"), goalWith("Step", "Main", "active")))
        val open = space(listOf(goalWith("Main", null, "active")))
        val empty = space(emptyList())
        assertEquals(listOf(false, true, true, true), listOf(allClosed, mixed, open, empty).map { it.showsInOpenList })
        assertEquals(listOf(true, true, false, false), listOf(allClosed, mixed, open, empty).map { it.showsInClosedList })
        // A partial read may have missed goals, so both lists keep the workspace and its notes.
        for (read in listOf(space(emptyList(), problems = listOf("G/x.md could not be read")), space(emptyList(), notShown = 2), space(emptyList(), foldersNotRead = 1))) {
            assertTrue(allClosed.copy(problems = read.problems, notShown = read.notShown, foldersNotRead = read.foldersNotRead).showsInOpenList)
            assertTrue(open.copy(problems = read.problems, notShown = read.notShown, foldersNotRead = read.foldersNotRead).showsInClosedList)
        }
    }

    @Test fun closedGoalParentTitleResolvesByNoteNameOrShowsTheLink() {
        val goals = listOf(goalWith("Raise a seed round", null, "active"), goalWith("Pitch", "raise a seed round", "done"), goalWith("Lost", "Missing", "done"))
        val tree = workBuildGoalTree(goals)
        assertEquals("Raise a seed round", workGoalParentTitle(goals[1], tree.byKey))
        assertEquals("Missing", workGoalParentTitle(goals[2], tree.byKey))
        assertNull(workGoalParentTitle(goals[0], tree.byKey))
    }

    @Test fun markDoneAndReopenPatchOnlyStatusAndClosed() {
        val goal = goalWith("Ship", null, "active").copy(revision = "rev-1")
        assertEquals(Json.parseToJsonElement("""{"path":"G/Ship.md","expected_revision":"rev-1","set":{"status":"done","closed":"2026-10-28"}}"""),
            workGoalStatusPatch(goal, done = true, today = "2026-10-28"))
        assertEquals(Json.parseToJsonElement("""{"path":"G/Ship.md","expected_revision":"rev-1","set":{"status":"active"},"unset":["closed"]}"""),
            workGoalStatusPatch(goal, done = false, today = "2026-10-28"))
        val day = java.util.Calendar.getInstance().apply { clear(); set(2026, java.util.Calendar.MARCH, 7) }
        assertEquals("2026-03-07", workGoalLocalDate(day))
    }

    @Test fun markDoneSendsThePatchAndWordsEachOutcome() = runBlocking {
        val goal = goalWith("Ship", null, "active").copy(revision = "rev-1")
        val sent = mutableListOf<Pair<String, JsonObject>>()
        fun send(status: Int, body: String? = null): WorkGoalSend = { path, patch -> sent += path to patch; status to body?.let { Json.parseToJsonElement(it) } }
        assertEquals(WorkGoalSaveResult.Saved, workUpdateGoalStatus(goal, true, "2026-10-28", send(200, """{"revision":"rev-2"}""")))
        assertEquals("/api/magician/v2/notes/front-matter", sent.single().first)
        assertEquals(WorkGoalSaveResult.Conflict, workUpdateGoalStatus(goal, false, "2026-10-28", send(409, """{"error":"revision_mismatch"}""")))
        assertEquals(WorkGoalSaveResult.Failed("Notes are read-only."), workUpdateGoalStatus(goal, true, "2026-10-28", send(503, """{"message":"Notes are read-only."}""")))
        assertEquals(WorkGoalSaveResult.Failed("Could not reopen the goal (500)."), workUpdateGoalStatus(goal, false, "2026-10-28", send(500)))
        // Without a revision nothing is sent: the screen reloads to get one.
        sent.clear()
        assertEquals(WorkGoalSaveResult.Conflict, workUpdateGoalStatus(goal.copy(revision = null), true, "2026-10-28", send(200)))
        assertEquals(emptyList<Pair<String, JsonObject>>(), sent)

        assertEquals("“Ship” is marked done. It is listed under Closed.", WorkGoalCopy.statusNotice(goal, true, WorkGoalSaveResult.Saved))
        assertEquals("“Ship” is open again.", WorkGoalCopy.statusNotice(goal, false, WorkGoalSaveResult.Saved))
        assertEquals("“Ship” changed since this screen loaded, so it was not marked done. The goals have been reloaded; try again if it is still needed.",
            WorkGoalCopy.statusNotice(goal, true, WorkGoalSaveResult.Conflict))
        assertEquals("“Ship” changed since this screen loaded, so it was not reopened. The goals have been reloaded; try again if it is still needed.",
            WorkGoalCopy.statusNotice(goal, false, WorkGoalSaveResult.Conflict))
        assertEquals("Could not mark “Ship” done: Offline", WorkGoalCopy.statusNotice(goal, true, WorkGoalSaveResult.Failed("Offline")))
        assertEquals("Could not reopen “Ship”: Offline", WorkGoalCopy.statusNotice(goal, false, WorkGoalSaveResult.Failed("Offline")))
    }

    @Test fun closedSwitchCopyAndDeepLink() {
        assertEquals("Open 3", WorkGoalCopy.open(3)); assertEquals("Closed 0", WorkGoalCopy.closed(0))
        assertEquals("Under Raise a seed round", WorkGoalCopy.under("Raise a seed round"))
        assertEquals("Closed 2026-10-28", WorkGoalCopy.closedOn("2026-10-28"))
        assertEquals("No open goals: every goal read here is done or dropped.", WorkGoalCopy.ALL_CLOSED)
        assertEquals(WorkLinkTarget.Library(WorkSection.Goals, WorkRoute("Goals", "closed", "Goals")), WorkDeepLinks.parse("/work?view=goals&goals=closed"))
        assertEquals(WorkLinkTarget.Library(WorkSection.Goals), WorkDeepLinks.parse("/work?view=goals&goals=open"))
    }

    // Workspaces

    private class FakeNotes(private val handle: (kind: String, path: String) -> Pair<Int, String>) {
        val calls = mutableListOf<Pair<String, String>>()
        val raw = mutableListOf<String>()
        val fetch: WorkGoalFetch = { url ->
            val kind = if (url.substringBefore('?').endsWith("/notes/tree")) "tree" else "file"
            val path = URLDecoder.decode(url.substringAfter("path="), "UTF-8")
            synchronized(this) { calls += kind to path; raw += url }
            val (status, body) = handle(kind, path)
            status to runCatching { Json.parseToJsonElement(body) }.getOrNull()
        }
        fun listed() = synchronized(this) { calls.filter { it.first == "tree" }.map { it.second } }
        fun read() = synchronized(this) { calls.filter { it.first == "file" }.map { it.second } }
    }

    private fun entries(vararg rows: JsonElement) = buildJsonObject { put("entries", JsonArray(rows.toList())) }.toString()
    private fun file(name: String, path: String) = buildJsonObject { put("name", name); put("relative_path", path); put("kind", "file") }
    private fun dir(name: String, path: String, children: Boolean? = true) = buildJsonObject {
        put("name", name); put("relative_path", path); put("kind", "dir"); children?.let { put("has_children", it) }
    }
    private fun markdown(text: String) = buildJsonObject { put("markdown", text) }.toString()
    private val notFound = 404 to """{"error":"not_found"}"""

    /** A notes space as the tree API lists it: one level per request, folders first. */
    private fun notesSpace(paths: List<String>, broken: List<String> = emptyList()): FakeNotes {
        val folders = linkedMapOf<String, MutableList<JsonObject>>()
        for (path in paths) {
            val parts = path.split('/')
            for (i in 1 until parts.size) {
                val folder = parts.subList(0, i).joinToString("/"); val relative = parts.subList(0, i + 1).joinToString("/")
                val isFile = i == parts.size - 1 && path.endsWith(".md")
                val rows = folders.getOrPut(folder) { mutableListOf() }
                if (rows.none { (it["relative_path"] as JsonPrimitive).content == relative }) rows += if (isFile) file(parts[i], relative) else dir(parts[i], relative, i != parts.size - 1)
            }
        }
        folders.values.forEach { rows -> rows.sortWith(compareBy<JsonObject>({ (it["kind"] as JsonPrimitive).content }, { (it["name"] as JsonPrimitive).content.lowercase() })) }
        return FakeNotes { kind, path ->
            if (kind == "tree") {
                if (path in broken) 500 to buildJsonObject { put("error", "io_error"); put("message", "Could not list $path.") }.toString()
                else folders[path]?.let { 200 to buildJsonObject { put("path", path); put("entries", JsonArray(it)) }.toString() } ?: notFound
            } else 200 to markdown("---\nmagician_kind: goal\n---\n# ${path.split('/').last().removeSuffix(".md")}\n")
        }
    }

    @Test fun readsEveryGoalNoteInEachWorkspaceGoalsFolder() = runBlocking {
        val notes = FakeNotes { kind, path ->
            if (kind == "tree") when (path) {
                "Workspaces" -> 200 to entries(dir("acme", "Workspaces/acme"), file("README.md", "Workspaces/README.md"))
                "Workspaces/acme/Goals" -> 200 to entries(
                    file("Raise a seed round.md", "Workspaces/acme/Goals/Raise a seed round.md"),
                    file("Apply to accelerators.md", "Workspaces/acme/Goals/Apply to accelerators.md"),
                    dir("Archive", "Workspaces/acme/Goals/Archive"),
                    file("pitch.pdf", "Workspaces/acme/Goals/pitch.pdf"),
                )
                else -> notFound
            } else 200 to markdown(if (path.endsWith("Raise a seed round.md")) main else sub)
        }
        val workspaces = WorkGoalReader.load(fetch = notes.fetch)
        assertEquals(1, workspaces.size)
        assertEquals(WorkGoalWorkspace("acme", "Workspaces/acme", workspaces[0].goals, emptyList(), 0, 0), workspaces[0])
        assertEquals(listOf("Raise a seed round", "Apply to accelerators"), workspaces[0].goals.map { it.name })
        assertEquals("Workspaces/acme/Goals/Apply to accelerators.md", workspaces[0].goals[1].path)
        assertEquals(listOf("Workspaces/acme/Goals/Raise a seed round.md", "Workspaces/acme/Goals/Apply to accelerators.md"), notes.read())
        assertEquals("/api/magician/v2/notes/tree?path=Workspaces", notes.raw.first())
    }

    @Test fun keepsTheRevisionEachNoteWasReadAt() = runBlocking {
        val notes = FakeNotes { kind, path ->
            if (kind == "tree") if (path == "Workspaces") 200 to entries(dir("acme", "Workspaces/acme"))
                else 200 to entries(file("A.md", "Workspaces/acme/Goals/A.md"), file("B.md", "Workspaces/acme/Goals/B.md"))
            else if (path.endsWith("A.md")) 200 to buildJsonObject { put("markdown", "---\nmagician_kind: goal\n---\n# A"); put("revision", "sha-a") }.toString()
            else 200 to markdown("---\nmagician_kind: goal\n---\n# B")
        }
        assertEquals(listOf("sha-a", null), WorkGoalReader.load(fetch = notes.fetch)[0].goals.map { it.revision })
    }

    @Test fun encodesNotePathsForTheTreeAndFileApis() = runBlocking {
        val notes = FakeNotes { kind, path ->
            if (kind == "tree") if (path == "Workspaces") 200 to entries(dir("acme co", "Workspaces/acme co")) else 200 to entries(file("A b.md", "Workspaces/acme co/Goals/A b.md"))
            else 200 to markdown("---\nmagician_kind: goal\n---\n# A")
        }
        WorkGoalReader.load(fetch = notes.fetch)
        assertEquals(listOf(
            "/api/magician/v2/notes/tree?path=Workspaces",
            "/api/magician/v2/notes/tree?path=Workspaces%2Facme%20co%2FGoals",
            "/api/magician/v2/notes/file?path=Workspaces%2Facme%20co%2FGoals%2FA%20b.md",
        ), notes.raw)
    }

    @Test fun treatsAMissingWorkspacesFolderAsNoWorkspaces() = runBlocking {
        val notes = FakeNotes { _, _ -> notFound }
        assertEquals(emptyList<WorkGoalWorkspace>(), WorkGoalReader.load(fetch = notes.fetch))
        assertEquals(1, notes.calls.size)
    }

    @Test fun failsWhenTheWorkspacesFolderCannotBeListed() = runBlocking {
        try { WorkGoalReader.load(fetch = FakeNotes { _, _ -> 503 to """{"error":"notes_unavailable","message":"Notes are unavailable."}""" }.fetch); fail("expected a failure") }
        catch (error: WorkGoalReadError) { assertEquals("Notes are unavailable.", error.message) }
        try { WorkGoalReader.load(fetch = FakeNotes { _, _ -> 500 to """{"error":"io_error"}""" }.fetch); fail("expected a failure") }
        catch (error: WorkGoalReadError) { assertEquals("Could not list Workspaces (500).", error.message) }
    }

    @Test fun keepsTheReadableGoalsAndReportsANoteThatCouldNotBeRead() = runBlocking {
        val notes = FakeNotes { kind, path ->
            if (kind == "tree") when (path) {
                "Workspaces" -> 200 to entries(dir("acme", "Workspaces/acme", null), dir("empty", "Workspaces/empty", null))
                "Workspaces/acme/Goals" -> 200 to entries(file("Raise a seed round.md", "Workspaces/acme/Goals/Raise a seed round.md"), file("Broken.md", "Workspaces/acme/Goals/Broken.md"))
                else -> notFound
            } else if (path.endsWith("Broken.md")) 500 to """{"error":"io_error"}""" else 200 to markdown(main)
        }
        val workspaces = WorkGoalReader.load(fetch = notes.fetch)
        assertEquals(listOf("acme", "empty"), workspaces.map { it.name })
        assertEquals(listOf("Raise a seed round"), workspaces[0].goals.map { it.name })
        assertEquals(listOf("Could not read Workspaces/acme/Goals/Broken.md (500)."), workspaces[0].problems)
        assertEquals(emptyList<WorkGoal>(), workspaces[1].goals); assertEquals(emptyList<String>(), workspaces[1].problems)
    }

    @Test fun readsOneFolderPerMainGoalItsSubGoalsBesideItAndLegacyNotesDirectlyInGoals() = runBlocking {
        val g = "Workspaces/acme/Goals"
        val space = notesSpace(listOf(
            "$g/Ship Picker v1/Pilot with three warehouses.md",
            "$g/Ship Picker v1/Ship Picker v1.md",
            "$g/Ship Picker v1/Drafts/Draft goal.md",
            "$g/Hire two field engineers/Hire two field engineers.md",
            "$g/Empty folder",
            "$g/Old flat goal.md",
        ))
        val acme = WorkGoalReader.load(fetch = space.fetch)[0]
        // The main goal's own note leads its folder; a folder's notes come before its subfolders.
        assertEquals(listOf(
            "$g/Hire two field engineers/Hire two field engineers.md",
            "$g/Ship Picker v1/Ship Picker v1.md",
            "$g/Ship Picker v1/Pilot with three warehouses.md",
            "$g/Ship Picker v1/Drafts/Draft goal.md",
            "$g/Old flat goal.md",
        ), acme.goals.map { it.path })
        assertEquals(listOf(emptyList<String>(), 0, 0), listOf(acme.problems, acme.notShown, acme.foldersNotRead))
        // A folder the tree API reports as empty is never listed.
        assertFalse("$g/Empty folder" in space.listed())
        assertTrue("$g/Ship Picker v1/Drafts" in space.listed())
    }

    @Test fun placesASubGoalByItsParentFieldNotByTheFolderItSitsIn() = runBlocking {
        val g = "Workspaces/acme/Goals"
        val notes = FakeNotes { kind, path ->
            if (kind == "tree") when (path) {
                "Workspaces" -> 200 to entries(dir("acme", "Workspaces/acme"))
                g -> 200 to entries(dir("Raise a seed round", "$g/Raise a seed round"), dir("Ship the beta", "$g/Ship the beta"))
                "$g/Raise a seed round" -> 200 to entries(file("Raise a seed round.md", "$g/Raise a seed round/Raise a seed round.md"))
                "$g/Ship the beta" -> 200 to entries(file("Apply to accelerators.md", "$g/Ship the beta/Apply to accelerators.md"), file("Ship the beta.md", "$g/Ship the beta/Ship the beta.md"))
                else -> notFound
            } else when {
                path.endsWith("Raise a seed round.md") -> 200 to markdown(main)
                path.endsWith("Apply to accelerators.md") -> 200 to markdown(sub)
                else -> 200 to markdown("---\nmagician_kind: goal\nkind: main\n---\n# Ship the beta")
            }
        }
        val tree = workBuildGoalTree(WorkGoalReader.load(fetch = notes.fetch)[0].goals)
        assertEquals(listOf("Raise a seed round" to listOf("Apply to accelerators"), "Ship the beta" to emptyList()),
            tree.roots.map { node -> node.goal.name to node.children.map { it.goal.name } })
    }

    @Test fun stopsAtTheFolderDepthLimitAndCountsTheFoldersItDidNotRead() = runBlocking {
        val g = "Workspaces/acme/Goals"
        val space = notesSpace(listOf("$g/Main/Main.md", "$g/Main/Two/Three/Deep.md", "$g/Main/Two/Three/Four/Too deep.md"))
        val acme = WorkGoalReader.load(WorkGoalReadLimits(folderDepth = 3, folders = 1000, notes = 2000), space.fetch)[0]
        assertEquals(listOf("Main", "Deep"), acme.goals.map { it.name })
        assertEquals(1, acme.foldersNotRead); assertEquals(0, acme.notShown)
        assertFalse("$g/Main/Two/Three/Four" in space.listed())
    }

    @Test fun readsAtMostTheNoteLimitAndTheFolderLimitPerWorkspaceAndCountsTheRest() = runBlocking {
        val g = "Workspaces/acme/Goals"
        val space = notesSpace(listOf("$g/A/A.md", "$g/A/A1.md", "$g/A/A2.md", "$g/B/B.md", "$g/C/C.md", "$g/Flat.md"))
        val acme = WorkGoalReader.load(WorkGoalReadLimits(folderDepth = 3, folders = 2, notes = 3), space.fetch)[0]
        assertEquals(listOf("A", "A1", "A2"), acme.goals.map { it.name })
        // B.md and Flat.md are listed but past the note limit; folder C is past the folder limit.
        assertEquals(2, acme.notShown); assertEquals(1, acme.foldersNotRead); assertEquals(emptyList<String>(), acme.problems)
        assertEquals(3, space.read().size)
        assertFalse("$g/C" in space.listed())
    }

    @Test fun reportsAMainGoalFolderThatCouldNotBeListedAndStillShowsTheOthers() = runBlocking {
        val g = "Workspaces/acme/Goals"
        val space = notesSpace(listOf("$g/Good/Good.md", "$g/Bad/Bad.md", "$g/Flat.md"), broken = listOf("$g/Bad"))
        val acme = WorkGoalReader.load(fetch = space.fetch)[0]
        assertEquals(listOf("Good", "Flat"), acme.goals.map { it.name })
        assertEquals(listOf("Could not list $g/Bad."), acme.problems)
    }

    @Test fun defaultLimitsMatchTheWeb() {
        assertEquals(WorkGoalReadLimits(folderDepth = 3, folders = 1000, notes = 2000), WorkGoalReadLimits.Standard)
    }

    // Screen copy and navigation

    @Test fun screenCopyMatchesTheWeb() {
        val limits = WorkGoalReadLimits(folderDepth = 1, folders = 10, notes = 3)
        assertEquals("1 more note in Goals/ not shown: this screen reads the first 3 notes of a workspace. Open the notes to see the rest.", WorkGoalCopy.notShown(1, limits))
        assertEquals("1 folder in Goals/ not read: this screen reads folders up to 1 level deep, and at most 10 of them.", WorkGoalCopy.foldersNotRead(1, limits))
        assertEquals("2 folders in Goals/ not read: this screen reads folders up to 3 levels deep, and at most 1000 of them.", WorkGoalCopy.foldersNotRead(2, WorkGoalReadLimits.Standard))
        assertEquals("One item could not be read", WorkGoalCopy.problems(1))
        assertEquals("3 items could not be read", WorkGoalCopy.problems(3))
        assertEquals("1 deeper goal is not shown here. Open the note to follow them.", WorkGoalCopy.deeper(1))
        assertEquals("2 deeper goals are not shown here. Open the note to follow them.", WorkGoalCopy.deeper(2))
        assertEquals("No goal workspaces yet. Ask Magican in chat to set one up from a template.", "${WorkGoalCopy.EMPTY_LEAD} ${WorkGoalCopy.EMPTY_LINK} ${WorkGoalCopy.EMPTY_TAIL}")
        assertEquals("Agents may use it; never stated outward.", WorkGoalVisibility.Internal.meaning)
    }

    @Test fun goalsIsAWorkViewWithHelpAndARoute() {
        assertEquals("Goals", WorkSection.Goals.title)
        assertEquals(WorkHelpTopic.Goals, WorkSection.Goals.help)
        assertEquals("Your goal workspaces: goals, the sub-goals under them and who owns each. Set one up from a template in chat.", WorkHelpTopic.Goals.explanation)
        assertEquals(WorkHelpTopic.Goals, WorkHelpTopic.forLabel("Goals"))
        assertEquals(WorkLinkTarget.Library(WorkSection.Goals), WorkDeepLinks.parse("/work?view=goals"))
        assertEquals("/work?view=goals", WorkWebContext(section = WorkSection.Goals).returnPath())
    }
}
