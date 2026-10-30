package ai.magicbeans.magdroid.ui

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The revamped Work views' counts, goal-tree folding and All work summary rules. */
class WorkLandingTest {
    private fun goal(name: String, parent: String? = null, extra: String = ""): WorkGoal =
        workParseGoalNote("---\nmagician_kind: goal\n${parent?.let { "parent: \"[[$it]]\"\n" } ?: ""}$extra---\n# $name", "G/$name.md")!!
    private fun space(name: String, goals: List<WorkGoal>, notShown: Int = 0, foldersNotRead: Int = 0) =
        WorkGoalWorkspace(name, "Workspaces/$name", goals, emptyList(), notShown, foldersNotRead)
    private fun shown(space: WorkGoalWorkspace) = workGoalShown(space)

    // Goals: the sub-goal count and the collapse state, as the web's GoalRow and GoalsWorkspace

    @Test fun countsDirectSubGoalsInTheWebsWords() {
        assertEquals("1 sub-goal", workGoalSubGoals(1))
        assertEquals("3 sub-goals", workGoalSubGoals(3))
        val tree = workBuildGoalTree(listOf(goal("Main"), goal("A", "Main"), goal("B", "Main"), goal("A1", "A")))
        // Direct children only: A1 is A's, not Main's.
        assertEquals(2, tree.roots.single().children.size)
    }

    @Test fun onlyAGoalWithSubGoalsCollapsesAndToggleFlipsIt() {
        val tree = workBuildGoalTree(listOf(goal("Main"), goal("A", "Main")))
        val main = tree.roots.single(); val leaf = main.children.single()
        val folded = workGoalToggle(emptySet(), main.goal.path)
        assertTrue(workGoalCollapsed(main, folded))
        assertFalse(workGoalCollapsed(main, emptySet()))
        // A leaf named in the set still shows: there is nothing to fold.
        assertFalse(workGoalCollapsed(leaf, setOf(leaf.goal.path)))
        assertEquals(emptySet<String>(), workGoalToggle(folded, main.goal.path))
    }

    @Test fun collapseAllFoldsEveryGoalWithSubGoalsIncludingNotLinkedTrees() {
        val acme = workBuildGoalTree(listOf(goal("Main"), goal("A", "Main"), goal("A1", "A"), goal("Leaf", "Main"),
            goal("Orphan", "Missing"), goal("O1", "Orphan")))
        val beta = workBuildGoalTree(listOf(goal("Solo")))
        assertEquals(setOf("G/Main.md", "G/A.md", "G/Orphan.md"), workGoalCollapsible(listOf(acme, beta)))
        assertEquals(emptySet<String>(), workGoalCollapsible(listOf(beta)))
    }

    @Test fun goalCountSumsWorkspacesAndMarksAPartialRead() {
        assertEquals("0 goals", workGoalCount(emptyList()))
        assertEquals("1 goal", workGoalCount(listOf(space("acme", listOf(goal("Main"))))))
        assertEquals("3 goals", workGoalCount(listOf(space("acme", listOf(goal("A"), goal("B"))), space("beta", listOf(goal("C"))))))
        assertEquals("2+ goals", workGoalCount(listOf(space("acme", listOf(goal("A"), goal("B")), notShown = 4))))
        assertEquals("1+ goals", workGoalCount(listOf(space("acme", listOf(goal("A")), foldersNotRead = 1))))
    }

    // The All work summary

    @Test fun landingShowsMainGoalsAcrossWorkspacesThenNotLinkedHeadsAtMostThree() {
        val acme = shown(space("acme", listOf(goal("Seed"), goal("Pitch", "Seed"), goal("Lost", "Missing"))))
        val beta = shown(space("beta", listOf(goal("Beta"), goal("Ship"))))
        assertEquals(listOf("Seed", "Beta", "Ship"), workGoalLandingNodes(listOf(acme, beta)).map { it.goal.name })
        assertEquals(listOf("Seed", "Beta", "Ship", "Lost"), workGoalLandingNodes(listOf(acme, beta), max = 10).map { it.goal.name })
        assertEquals(1, workGoalLandingNodes(listOf(acme, beta)).first().children.size)
    }

    @Test fun landingWithNoMainGoalListsTheGoalsAsReadWithoutSubGoals() {
        val loop = shown(space("acme", listOf(goal("A", "B"), goal("B", "A"), goal("C", "Missing"))))
        val nodes = workGoalLandingNodes(listOf(loop))
        assertEquals(listOf("A", "B", "C"), nodes.map { it.goal.name })
        assertTrue(nodes.all { it.children.isEmpty() })
        assertEquals(emptyList<WorkGoalNode>(), workGoalLandingNodes(emptyList()))
    }

    @Test fun landingShowsOnlyOpenGoalsAndCountsThem() {
        val acme = shown(space("acme", listOf(goal("Old", extra = "status: done\n"), goal("Freed", "Old"), goal("Gone", extra = "status: dropped\n"))))
        assertEquals(listOf("Freed"), workGoalLandingNodes(listOf(acme)).map { it.goal.name })
        assertEquals("Old", workGoalLandingNodes(listOf(acme)).single().closedParent)
        assertEquals("1 goal", workGoalCount(listOf(acme.space)))
        assertEquals(2, acme.closed.size)
        // Every goal read is closed: nothing to list, so the landing points at Closed.
        val closed = shown(space("beta", listOf(goal("Done", extra = "status: done\n"))))
        assertEquals(emptyList<WorkGoalNode>(), workGoalLandingNodes(listOf(closed)))
        assertEquals("0 goals", workGoalCount(listOf(closed.space)))
    }

    @Test fun landingReadsGoalsLightly() {
        val light = WorkGoalReadLimits.Landing
        assertEquals(WorkGoalReadLimits(folderDepth = 1, folders = 6, notes = 24), light)
        assertTrue(light.notes < WorkGoalReadLimits.Standard.notes && light.folders < WorkGoalReadLimits.Standard.folders)
        assertEquals(3, WorkLandingCopy.GOALS_SHOWN)
        assertEquals(4, WorkLandingCopy.PLAYBOOKS_SHOWN)
    }

    @Test fun playbooksKeepNamedDocumentsInOrderWithTheNameAsAMissingTitle() {
        val value = Json.parseToJsonElement("""{"programs":[{"name":"growth.md","title":"Growth"},{"title":"No name"},{"name":"ops.md"},{"name":"blank.md","title":""}]}""")
        assertEquals(listOf(WorkPlaybookSummary("growth.md", "Growth"), WorkPlaybookSummary("ops.md", "ops.md"), WorkPlaybookSummary("blank.md", "blank.md")),
            workPlaybookSummaries(value))
        assertEquals(emptyList<WorkPlaybookSummary>(), workPlaybookSummaries(null))
        assertEquals("1 playbook", workPlaybookCount(1))
        assertEquals("0 playbooks", workPlaybookCount(0))
        assertEquals("No playbook documents in this workspace.", WorkLandingCopy.NO_PLAYBOOKS)
    }

    // Quality checks

    @Test fun qualityNeedsReviewIsBriefReviewAndAttentionAndRunsCountsActive() {
        val counts = WorkQualityCounts.from(Json.parseToJsonElement("""{"items":[],"counts":{"brief":2,"review":1,"attention":3,"active":4,"complete":9,"stopped":1}}"""))
        assertEquals(WorkQualityCounts(needsReview = 6, active = 4), counts)
        assertEquals(listOf("inbox" to "Needs review · 6", "runs" to "Runs · 4 active"), workQualityLanes(counts))
        // A missing key counts as zero.
        assertEquals(WorkQualityCounts(0, 0), WorkQualityCounts.from(Json.parseToJsonElement("""{"counts":{}}""")))
    }

    @Test fun qualityLanesHaveNoNumberWithoutCounts() {
        assertNull(WorkQualityCounts.from(Json.parseToJsonElement("""{"items":[]}""")))
        assertEquals(listOf("inbox" to "Needs review", "runs" to "Runs"), workQualityLanes(null))
    }

    // Work bar

    @Test fun needsYouBadgeHidesAtZeroAndCapsAt99() {
        assertNull(workBadgeText(0))
        assertNull(workBadgeText(-1))
        assertEquals("7", workBadgeText(7))
        assertEquals("99+", workBadgeText(100))
    }

    @Test fun barMenuKeepsAnEqualListAndClearsOnNull() {
        val actions = TasksScreenActions()
        val expand = {}
        val first = listOf(WorkBarMenuItem("Expand all", enabled = false, onClick = expand))
        actions.bindMenu(first)
        assertTrue(actions.menu === first)
        actions.bindMenu(listOf(WorkBarMenuItem("Expand all", enabled = false, onClick = expand)))
        assertTrue("an equal menu is not replaced", actions.menu === first)
        actions.bindMenu(null)
        assertEquals(emptyList<WorkBarMenuItem>(), actions.menu)
    }
}
