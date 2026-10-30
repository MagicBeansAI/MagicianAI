package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.WorkRepository
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.add
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray
import kotlinx.serialization.json.putJsonObject
import java.util.Calendar
import java.util.Locale

// Goal notes: Markdown under `Workspaces/<name>/Goals/` with YAML frontmatter.
// A port of the web's `ui/unified-ui/src/lib/work/goals.ts` (and the iOS
// `WorkGoals.swift`): the same fields, defaults, tree rules, read limits and
// messages. Only the flat fields the goal template writes are read.

internal enum class WorkGoalKind(val key: String) { Main("main"), Sub("sub"), Peer("peer") }

internal enum class WorkGoalVisibility(val key: String, val meaning: String) {
    Private("private", "Shown only to you; never in an outward draft."),
    Internal("internal", "Agents may use it; never stated outward."),
    Public("public", "May be stated outward."),
}

internal data class WorkGoal(
    val name: String, val title: String, val path: String, val workspace: String?,
    val kind: WorkGoalKind,
    /** The parent's note name without link brackets; empty reads as no parent, as on the web. */
    val parent: String?,
    val peers: List<String>, val owner: String?, val status: String, val target: String?,
    val visibility: WorkGoalVisibility, val program: String?, val goalId: String?, val metrics: List<String>,
    /** `closed:`: the date (`YYYY-MM-DD`) the goal was marked done or dropped, when recorded. */
    val closed: String? = null,
    /** The note's content revision as read (`GET /notes/file`). An edit sends it back, so a change
     * made since the read (by an agent or another device) is refused, not overwritten. */
    val revision: String? = null,
)

/** Why a goal heads a "Not linked" tree instead of hanging under a main goal. */
internal enum class WorkGoalOrphanReason(val key: String) { MissingParent("missing-parent"), SelfParent("self-parent"), ParentLoop("parent-loop") }

/** [closedParent] names the closed goal an open goal was moved up from ([workOpenGoalTree]). */
internal data class WorkGoalNode(val goal: WorkGoal, val children: List<WorkGoalNode>, val closedParent: String? = null)
internal data class WorkGoalOrphan(val node: WorkGoalNode, val reason: WorkGoalOrphanReason) { val goal: WorkGoal get() = node.goal }

/** Every goal appears exactly once: under a main goal or under one orphan root.
 * [byKey] resolves links by [workGoalKey]; the first goal listed wins a shared key. */
internal data class WorkGoalTree(val roots: List<WorkGoalNode>, val orphans: List<WorkGoalOrphan>, val byKey: Map<String, WorkGoal>)

private val MD_SUFFIX = Regex("\\.md\\z", RegexOption.IGNORE_CASE)
private val LINE_BREAK = Regex("\\r?\\n")

private fun jsTrim(text: String): String = text.trim { it.isWhitespace() }

/** `[[Name|alias]]` → `Name`; anything else is returned trimmed. */
internal fun workGoalLinkTarget(value: String): String {
    val trimmed = jsTrim(value)
    val match = Regex("^\\[\\[([^\\]|]+)(?:\\|[^\\]]*)?\\]\\]\\z").find(trimmed)
    return jsTrim(match?.groupValues?.get(1) ?: trimmed)
}

/** The note a link names: `[[Goals/Name#part|alias]]`, `Name.md` and `name` all name the note `Name`. */
internal fun workGoalKey(link: String): String =
    jsTrim(workGoalLinkTarget(link).split('#')[0].split('/').last().replace(MD_SUFFIX, "")).lowercase()

private fun unquote(value: String): String {
    val v = jsTrim(value)
    return if (v.length > 1 && ((v.startsWith('"') && v.endsWith('"')) || (v.startsWith('\'') && v.endsWith('\'')))) v.substring(1, v.length - 1) else v
}

/** Splits at [separator] outside quotes and brackets and drops a ` #` comment.
 * A quote only opens at the start of a value, so `Founder's goal` stays plain. */
private fun scan(text: String, separator: Char? = null): List<String> {
    val parts = mutableListOf<String>()
    var quote: Char? = null
    var depth = 0; var start = 0; var fresh = true
    for (i in text.indices) {
        val c = text[i]
        if (quote != null) { if (c == quote) quote = null; continue }
        if ((c == '"' || c == '\'') && fresh) quote = c
        else if (c == '#' && (i == 0 || text[i - 1].isWhitespace())) return parts + text.substring(start, i)
        else if (c == '[') depth++
        else if (c == ']') depth = maxOf(0, depth - 1)
        else if (separator != null && c == separator && depth == 0) { parts += text.substring(start, i); start = i + 1; fresh = true; continue }
        if (!c.isWhitespace()) fresh = c == '[' || (c == ',' && depth > 0)
    }
    return parts + text.substring(start)
}

private sealed interface GoalField {
    data class Text(val value: String) : GoalField
    data class Items(val values: List<String>) : GoalField
}

private fun frontmatter(markdown: String): Pair<Map<String, GoalField>, String>? {
    val source = markdown.removePrefix("﻿")
    val match = Regex("^---[ \\t]*\\r?\\n([\\s\\S]*?)\\r?\\n---[ \\t]*(?:\\r?\\n|\\z)").find(source) ?: return null
    val fields = linkedMapOf<String, GoalField>()
    var listKey: String? = null
    for (line in match.groupValues[1].split(LINE_BREAK)) {
        val item = Regex("^\\s*-\\s+(.*)\\z").find(line)
        if (item != null) {
            val value = unquote(scan(item.groupValues[1])[0])
            val key = listKey
            val current = key?.let { fields[it] } as? GoalField.Items
            if (key != null && value.isNotEmpty() && current != null) fields[key] = GoalField.Items(current.values + value)
            continue
        }
        val pair = Regex("^([A-Za-z_][A-Za-z0-9_-]*):\\s*(.*)\\z").find(line) ?: continue
        val key = pair.groupValues[1]
        val value = jsTrim(scan(pair.groupValues[2])[0])
        listKey = null
        if (value.isEmpty()) { fields[key] = GoalField.Items(emptyList()); listKey = key }
        // A flow list, unless the whole value is one bare `[[link]]`.
        else if (value.startsWith('[') && value.endsWith(']') && !Regex("^\\[\\[[^\\[\\]]*\\]\\]\\z").containsMatchIn(value)) {
            fields[key] = GoalField.Items(scan(value.substring(1, value.length - 1), ',').map(::unquote).filter(String::isNotEmpty))
        } else fields[key] = GoalField.Text(unquote(value))
    }
    return fields to source.substring(match.value.length)
}

private fun text(field: GoalField?): String? = (field as? GoalField.Text)?.value?.takeIf { it.isNotEmpty() && it != "~" && it != "null" }
private fun list(field: GoalField?): List<String> = (field as? GoalField.Items)?.values ?: text(field)?.let { listOf(it) } ?: emptyList()

internal fun workParseGoalNote(markdown: String, path: String): WorkGoal? {
    val (fields, body) = frontmatter(markdown) ?: return null
    if (fields["magician_kind"] != GoalField.Text("goal")) return null
    val name = path.split('/').last().replace(MD_SUFFIX, "")
    // Only an H1 on the first non-blank line is the title; otherwise the file name is.
    val heading = body.split(LINE_BREAK).firstOrNull { jsTrim(it).isNotEmpty() }
        ?.let { Regex("^#[ \\t]+(.+)\\z").find(it)?.groupValues?.get(1) }?.let(::jsTrim)
    val parent = text(fields["parent"])
    val kind = WorkGoalKind.entries.firstOrNull { it.key == text(fields["kind"])?.lowercase() } ?: if (parent != null) WorkGoalKind.Sub else WorkGoalKind.Main
    val visibility = WorkGoalVisibility.entries.firstOrNull { it.key == text(fields["visibility"])?.lowercase() } ?: WorkGoalVisibility.Private
    return WorkGoal(
        name = name, title = heading?.takeIf(String::isNotEmpty) ?: name, path = path, workspace = text(fields["workspace"]), kind = kind,
        parent = parent?.let(::workGoalLinkTarget),
        peers = list(fields["peers"]).map(::workGoalLinkTarget), owner = text(fields["owner"]),
        status = text(fields["status"])?.lowercase() ?: "proposed", target = text(fields["target"]), visibility = visibility,
        program = text(fields["program"]), goalId = text(fields["goal_id"]), metrics = list(fields["metrics"]),
        closed = text(fields["closed"]),
    )
}

/** `done` and `dropped` goals are closed: the goal screens list them only under Closed. */
internal val WORK_CLOSED_GOAL_STATUSES = listOf("done", "dropped")

internal fun workIsClosedGoal(goal: WorkGoal): Boolean = goal.status in WORK_CLOSED_GOAL_STATUSES

/** The tree without closed goals. An open goal under a closed one moves up to its nearest open
 * ancestor, or heads a tree of its own, and names the closed goal it sat under. A closed head of a
 * Not linked tree passes its reason to the open goals lifted out of it. */
internal fun workOpenGoalTree(tree: WorkGoalTree): WorkGoalTree {
    // Children before parents, so deep chains need no recursion.
    val order = mutableListOf<WorkGoalNode>()
    val stack = ArrayDeque<WorkGoalNode>().apply { addAll(tree.roots); addAll(tree.orphans.map { it.node }) }
    while (stack.isNotEmpty()) { val node = stack.removeLast(); order += node; node.children.forEach(stack::addLast) }
    // What each node becomes under an open parent: itself, or (closed) the open goals lifted out of it.
    val pruned = java.util.IdentityHashMap<WorkGoalNode, List<WorkGoalNode>>()
    for (node in order.asReversed()) {
        pruned[node] = if (!workIsClosedGoal(node.goal)) listOf(WorkGoalNode(node.goal, node.children.flatMap { pruned.getValue(it) }))
        else node.children.flatMap { child ->
            if (workIsClosedGoal(child.goal)) pruned.getValue(child) else pruned.getValue(child).map { it.copy(closedParent = node.goal.title) }
        }
    }
    return WorkGoalTree(
        roots = tree.roots.flatMap { pruned.getValue(it) },
        orphans = tree.orphans.flatMap { orphan -> pruned.getValue(orphan.node).map { WorkGoalOrphan(it, orphan.reason) } },
        byKey = tree.byKey,
    )
}

/** Closed goals, most recently closed first; those without a `closed:` date last, by title. */
internal fun workClosedGoals(goals: List<WorkGoal>): List<WorkGoal> = goals.filter(::workIsClosedGoal).sortedWith(
    compareByDescending<WorkGoal> { it.closed.orEmpty() }.thenBy(String.CASE_INSENSITIVE_ORDER) { it.title }.thenBy { it.title },
)

/** One workspace as the goal screens show it: [tree] holds only open goals, [closed] lists the rest. */
internal data class WorkGoalShown(val space: WorkGoalWorkspace, val tree: WorkGoalTree, val closed: List<WorkGoal>)

internal fun workGoalShown(space: WorkGoalWorkspace): WorkGoalShown =
    WorkGoalShown(space, workOpenGoalTree(workBuildGoalTree(space.goals)), workClosedGoals(space.goals))

/** The title of the goal [goal] names as its parent, by note name; the link itself when it is not read. */
internal fun workGoalParentTitle(goal: WorkGoal, byKey: Map<String, WorkGoal>): String? =
    goal.parent?.takeIf(String::isNotEmpty)?.let { byKey[workGoalKey(it)]?.title ?: it }

private class TreeNode(val goal: WorkGoal, val index: Int) { val children = mutableListOf<TreeNode>() }

internal fun workBuildGoalTree(goals: List<WorkGoal>): WorkGoalTree {
    val nodes = goals.mapIndexed { index, goal -> TreeNode(goal, index) }
    val byKey = linkedMapOf<String, TreeNode>()
    for (node in nodes) byKey.putIfAbsent(workGoalKey(node.goal.name), node)
    val parentOf = HashMap<TreeNode, TreeNode>()
    val mains = mutableListOf<TreeNode>()
    val heads = mutableListOf<Pair<TreeNode, WorkGoalOrphanReason>>()
    for (node in nodes) {
        val link = node.goal.parent
        if (link.isNullOrEmpty()) { mains += node; continue }
        val parent = byKey[workGoalKey(link)]
        when {
            parent == null -> heads += node to WorkGoalOrphanReason.MissingParent
            parent === node -> heads += node to WorkGoalOrphanReason.SelfParent
            else -> { parent.children += node; parentOf[node] = parent }
        }
    }
    // Copies each subtree once; a link back into a placed goal (a closed loop) is dropped.
    val placed = HashSet<TreeNode>()
    fun place(node: TreeNode): WorkGoalNode {
        placed += node
        val copies = HashMap<TreeNode, MutableList<TreeNode>>()
        val order = mutableListOf<TreeNode>()
        val stack = ArrayDeque<TreeNode>().apply { addLast(node) }
        while (stack.isNotEmpty()) {
            val from = stack.removeLast()
            order += from
            val kept = mutableListOf<TreeNode>()
            for (child in from.children) {
                if (child in placed) continue
                placed += child; kept += child; stack.addLast(child)
            }
            copies[from] = kept
        }
        // Build the copies bottom-up so deep chains need no recursion.
        val built = HashMap<TreeNode, WorkGoalNode>()
        for (from in order.asReversed()) built[from] = WorkGoalNode(from.goal, copies[from].orEmpty().map { built.getValue(it) })
        return built.getValue(node)
    }
    val roots = mains.map(::place)
    val orphans = heads.map { (node, reason) -> node.index to WorkGoalOrphan(place(node), reason) }.toMutableList()
    // What is left hangs off a parent loop. Each loop is headed by its first member in goal-key order.
    for (node in nodes) {
        if (node in placed) continue
        val walked = HashSet<TreeNode>()
        var at = node
        while (walked.add(at)) at = parentOf.getValue(at)
        val loop = mutableListOf(at)
        var next = parentOf.getValue(at)
        while (next !== at) { loop += next; next = parentOf.getValue(next) }
        val head = loop.reduce { best, member ->
            val a = workGoalKey(member.goal.name); val b = workGoalKey(best.goal.name)
            if (a < b || (a == b && member.goal.path < best.goal.path)) member else best
        }
        orphans += head.index to WorkGoalOrphan(place(head), WorkGoalOrphanReason.ParentLoop)
    }
    orphans.sortBy { it.first }
    return WorkGoalTree(roots, orphans.map { it.second }, byKey.mapValues { it.value.goal })
}

/** Why [orphan] heads a Not linked tree; a goal lifted out of a closed goal says that instead. */
internal fun workGoalOrphanNote(orphan: WorkGoalOrphan): String = orphan.node.closedParent?.let(WorkGoalCopy::closedParent) ?: when (orphan.reason) {
    WorkGoalOrphanReason.MissingParent -> "Parent goal not found: ${orphan.goal.parent}"
    WorkGoalOrphanReason.SelfParent -> "Names itself as its parent."
    WorkGoalOrphanReason.ParentLoop -> "Part of a parent loop: its parent goals lead back to it."
}

/** Program documents are bare `*.md` names; a goal may name one without the extension. */
internal fun workGoalProgramName(program: String): String = if (MD_SUFFIX.containsMatchIn(program)) program else "$program.md"

/** How much of one workspace's `Goals/` the screen reads. Each main goal has its own folder
 * (`Goals/<Main goal>/`), so a folder is one listing request and a note one read request. */
internal data class WorkGoalReadLimits(val folderDepth: Int, val folders: Int, val notes: Int) {
    companion object {
        val Standard = WorkGoalReadLimits(folderDepth = 3, folders = 1000, notes = 2000)
        /** The All work summary: the main-goal folders directly in `Goals/` and the first notes in them,
         * enough for three open top-level goals and their sub-goal counts on a phone connection
         * (closed main goals still take a folder and their notes). */
        val Landing = WorkGoalReadLimits(folderDepth = 1, folders = 6, notes = 24)
    }
}

// The collapsible tree, as the web's GoalRow / GoalsWorkspace: a goal with sub-goals collapses by its
// note path, and a collapsed goal counts its direct sub-goals.

/** "1 sub-goal", "3 sub-goals": a goal's direct sub-goals, as the web counts them. */
internal fun workGoalSubGoals(count: Int): String = "$count sub-goal${if (count == 1) "" else "s"}"

/** Whether [node] shows collapsed: only a goal with sub-goals can be. */
internal fun workGoalCollapsed(node: WorkGoalNode, collapsed: Set<String>): Boolean = node.children.isNotEmpty() && node.goal.path in collapsed

internal fun workGoalToggle(collapsed: Set<String>, path: String): Set<String> = if (path in collapsed) collapsed - path else collapsed + path

/** Every goal with sub-goals, under main goals and Not linked heads alike: what Collapse all collapses. */
internal fun workGoalCollapsible(trees: List<WorkGoalTree>): Set<String> {
    val paths = linkedSetOf<String>()
    val stack = ArrayDeque<WorkGoalNode>()
    trees.forEach { tree -> stack.addAll(tree.roots); stack.addAll(tree.orphans.map { it.node }) }
    while (stack.isNotEmpty()) {
        val node = stack.removeFirst()
        if (node.children.isNotEmpty()) { paths += node.goal.path; stack.addAll(node.children) }
    }
    return paths
}

/** "1 goal", "12 goals": the open goals; a `+` when the read stopped at a limit, so the count is a floor. */
internal fun workGoalCount(workspaces: List<WorkGoalWorkspace>): String {
    val count = workspaces.sumOf { space -> space.goals.count { !workIsClosedGoal(it) } }
    val partial = workspaces.any { it.notShown > 0 || it.foldersNotRead > 0 }
    return "$count${if (partial) "+" else ""} goal${if (count == 1 && !partial) "" else "s"}"
}

/** The All work summary's goals, as the web's compact GoalsWorkspace: open main goals, then Not linked
 * heads, across workspaces; with no open main goal anywhere, the open goals as they were read, without sub-goals. */
internal fun workGoalLandingNodes(trees: List<WorkGoalShown>, max: Int = 3): List<WorkGoalNode> {
    val roots = trees.flatMap { it.tree.roots }
    val nodes = if (roots.isNotEmpty()) roots + trees.flatMap { shown -> shown.tree.orphans.map { it.node } }
        else trees.flatMap { shown -> shown.space.goals.filterNot(::workIsClosedGoal).map { WorkGoalNode(it, emptyList()) } }
    return nodes.take(max)
}

/** [problems] are notes or folders in this workspace that could not be read; the rest still show.
 * [notShown] counts notes past the note limit; [foldersNotRead] counts folders past the depth or folder limit. */
internal data class WorkGoalWorkspace(
    val name: String, val path: String, val goals: List<WorkGoal>, val problems: List<String>,
    val notShown: Int, val foldersNotRead: Int,
) {
    /** Read only in part, or with notes that could not be read: goals may be missing, so both
     * lists keep the workspace to show why. */
    private val readInPart get() = problems.isNotEmpty() || notShown > 0 || foldersNotRead > 0
    /** A workspace whose goals are all closed is listed only under Closed. One with no goals yet
     * stays in the open list, so it can be filled. */
    val showsInOpenList get() = readInPart || goals.isEmpty() || goals.any { !workIsClosedGoal(it) }
    val showsInClosedList get() = readInPart || goals.any(::workIsClosedGoal)
}

/** One GET's status and JSON body (null when the body is not JSON). */
internal typealias WorkGoalFetch = suspend (String) -> Pair<Int, JsonElement?>

internal class WorkGoalReadError(message: String) : Exception(message)

private data class NoteEntry(val name: String, val relativePath: String, val kind: String, val hasChildren: Boolean?)

private fun JsonElement?.field(key: String): String? = ((this as? JsonObject)?.get(key) as? JsonPrimitive)?.contentOrNull

internal object WorkGoalReader {
    private const val NOTES = "/api/magician/v2/notes"
    private const val WORKSPACES = "Workspaces"

    private fun failure(status: Int, value: JsonElement?, what: String) =
        WorkGoalReadError(value.field("message")?.takeIf(String::isNotEmpty) ?: "Could not $what ($status).")

    private fun message(error: Throwable) = error.message ?: error.toString()

    /** `null` when the folder does not exist. */
    private suspend fun noteTree(path: String, fetch: WorkGoalFetch): List<NoteEntry>? {
        val (status, value) = fetch("$NOTES/tree?path=${WorkRepository.segment(path)}")
        if (status == 404) return null
        if (status !in 200..299) throw failure(status, value, "list $path")
        return ((value as? JsonObject)?.get("entries") as? JsonArray).orEmpty().mapNotNull { row ->
            val name = row.field("name"); val relative = row.field("relative_path"); val kind = row.field("kind")
            if (name == null || relative == null || kind == null) null
            else NoteEntry(name, relative, kind, ((row as JsonObject)["has_children"] as? JsonPrimitive)?.booleanOrNull)
        }
    }

    private data class Listing(val entries: List<NoteEntry>, val problem: String?)
    private data class NoteRead(val goal: WorkGoal?, val problem: String?)

    /** A folder that is missing (removed since it was listed) is empty. */
    private suspend fun listFolder(path: String, fetch: WorkGoalFetch): Listing = try {
        Listing(noteTree(path, fetch).orEmpty(), null)
    } catch (cancelled: CancellationException) { throw cancelled } catch (error: Exception) { Listing(emptyList(), message(error)) }

    private suspend fun readGoalNote(path: String, fetch: WorkGoalFetch): NoteRead = try {
        val (status, value) = fetch("$NOTES/file?path=${WorkRepository.segment(path)}")
        when {
            // Removed between the listing and the read.
            status == 404 -> NoteRead(null, null)
            status !in 200..299 -> throw failure(status, value, "read $path")
            else -> {
                val markdown = ((value as? JsonObject)?.get("markdown") as? JsonPrimitive)?.takeIf { it.isString }?.content
                    ?: throw WorkGoalReadError("Could not read $path.")
                val revision = ((value as? JsonObject)?.get("revision") as? JsonPrimitive)?.takeIf { it.isString }?.content
                NoteRead(workParseGoalNote(markdown, path)?.copy(revision = revision), null)
            }
        }
    } catch (cancelled: CancellationException) { throw cancelled } catch (error: Exception) { NoteRead(null, message(error)) }

    /** At most four notes requests at once, results in input order. */
    private suspend fun <T, R> inPool(items: List<T>, run: suspend (T) -> R): List<R> = coroutineScope {
        val gate = Semaphore(4)
        items.map { item -> async { gate.withPermit { run(item) } } }.awaitAll()
    }

    private class Walk(val root: String) {
        val listed = LinkedHashMap<String, List<NoteEntry>>()
        val problems = mutableListOf<String>()
        var folders = 0
        var foldersNotRead = 0
    }

    private data class Folder(val space: Int, val path: String, val depth: Int)

    /** Lists every workspace's `Goals/` folder tree, one depth level at a time across all workspaces,
     * so a level's listings share one request pool. A folder the API reports as empty is not listed. */
    private suspend fun listGoalFolders(spaces: List<NoteEntry>, limits: WorkGoalReadLimits, fetch: WorkGoalFetch): List<Walk> {
        val walks = spaces.map { Walk("${it.relativePath}/Goals") }
        var level = walks.mapIndexed { space, walk -> Folder(space, walk.root, 0) }
        while (level.isNotEmpty()) {
            val results = inPool(level) { listFolder(it.path, fetch) }
            val next = mutableListOf<Folder>()
            level.forEachIndexed { i, folder ->
                val walk = walks[folder.space]
                val (entries, problem) = results[i]
                if (problem != null) walk.problems += problem
                walk.listed[folder.path] = entries
                for (entry in entries) {
                    if (entry.kind != "dir" || entry.hasChildren == false) continue
                    if (folder.depth + 1 > limits.folderDepth || walk.folders >= limits.folders) { walk.foldersNotRead++; continue }
                    walk.folders++
                    next += Folder(folder.space, entry.relativePath, folder.depth + 1)
                }
            }
            level = next
        }
        return walks
    }

    /** Markdown notes in reading order: the main-goal folders in `Goals/` first, then legacy notes
     * directly in `Goals/`. Inside a folder its notes come before its subfolders, and the note named
     * after the folder (the main goal) leads. */
    private fun goalNotePaths(walk: Walk): List<String> {
        val paths = mutableListOf<String>()
        fun visit(path: String) {
            val entries = walk.listed[path].orEmpty()
            val folder = path.split('/').last().lowercase()
            val own = { entry: NoteEntry -> entry.name.replace(MD_SUFFIX, "").lowercase() == folder }
            val notes = entries.filter { it.kind == "file" && MD_SUFFIX.containsMatchIn(it.name) }
            val subfolders = { for (entry in entries) if (entry.kind == "dir" && walk.listed.containsKey(entry.relativePath)) visit(entry.relativePath) }
            if (path == walk.root) subfolders()
            paths += (notes.filter(own) + notes.filterNot(own)).map { it.relativePath }
            if (path != walk.root) subfolders()
        }
        visit(walk.root)
        return paths
    }

    /** Every folder under `Workspaces/` with the goal notes under its `Goals/` folder: one folder per
     * main goal (`Goals/<Main goal>/`, its sub-goals flat beside it) and legacy notes directly in `Goals/`.
     * The `parent:` field, not the folder, places a goal in the tree. No `Workspaces/` folder is no workspaces. */
    suspend fun load(limits: WorkGoalReadLimits = WorkGoalReadLimits.Standard, fetch: WorkGoalFetch): List<WorkGoalWorkspace> {
        val spaces = noteTree(WORKSPACES, fetch).orEmpty().filter { it.kind == "dir" }
        val walks = listGoalFolders(spaces, limits, fetch)
        val listed = walks.map(::goalNotePaths)
        val files = listed.flatMapIndexed { space, paths -> paths.take(limits.notes).map { space to it } }
        val notes = inPool(files) { readGoalNote(it.second, fetch) }
        return spaces.mapIndexed { index, space ->
            val own = notes.filterIndexed { i, _ -> files[i].first == index }
            WorkGoalWorkspace(
                name = space.name, path = space.relativePath,
                goals = own.mapNotNull { it.goal },
                problems = walks[index].problems + own.mapNotNull { it.problem },
                notShown = maxOf(0, listed[index].size - limits.notes),
                foldersNotRead = walks[index].foldersNotRead,
            )
        }
    }
}

/** One PATCH's status and JSON body (null when the body is not JSON). */
internal typealias WorkGoalSend = suspend (String, JsonObject) -> Pair<Int, JsonElement?>

internal sealed interface WorkGoalSaveResult {
    data object Saved : WorkGoalSaveResult
    /** The note changed since it was read (409), or was read without a revision. */
    data object Conflict : WorkGoalSaveResult
    data class Failed(val message: String) : WorkGoalSaveResult
}

/** Today in the device's time zone, written `YYYY-MM-DD`. */
internal fun workGoalLocalDate(now: Calendar = Calendar.getInstance()): String =
    String.format(Locale.ROOT, "%04d-%02d-%02d", now.get(Calendar.YEAR), now.get(Calendar.MONTH) + 1, now.get(Calendar.DAY_OF_MONTH))

/** Mark done stamps `closed:` with [today] for the Closed list; Reopen sets `active` and removes `closed:`. */
internal fun workGoalStatusPatch(goal: WorkGoal, done: Boolean, today: String): JsonObject = buildJsonObject {
    put("path", goal.path)
    put("expected_revision", goal.revision)
    if (done) putJsonObject("set") { put("status", "done"); put("closed", today) }
    else { putJsonObject("set") { put("status", "active") }; putJsonArray("unset") { add("closed") } }
}

/** Changes only those front-matter keys through `PATCH /notes/front-matter`, as the web's `updateGoalStatus`.
 * The server keeps every other line and the body, and refuses with 409 when the note changed since
 * [WorkGoal.revision] was read. */
internal suspend fun workUpdateGoalStatus(goal: WorkGoal, done: Boolean, today: String = workGoalLocalDate(), send: WorkGoalSend): WorkGoalSaveResult {
    if (goal.revision == null) return WorkGoalSaveResult.Conflict
    return try {
        val (status, value) = send("/api/magician/v2/notes/front-matter", workGoalStatusPatch(goal, done, today))
        when {
            status == 409 -> WorkGoalSaveResult.Conflict
            status !in 200..299 -> WorkGoalSaveResult.Failed(value.field("message")?.takeIf(String::isNotEmpty)
                ?: "Could not ${if (done) "mark the goal done" else "reopen the goal"} ($status).")
            else -> { WorkGoalEdits.changed(); WorkGoalSaveResult.Saved }
        }
    } catch (cancelled: CancellationException) { throw cancelled } catch (error: Exception) { WorkGoalSaveResult.Failed(error.message ?: error.toString()) }
}

/** Counts saved goal edits, so the All work summary reads its goals again after one (the web forgets its cache). */
internal object WorkGoalEdits {
    private val count = MutableStateFlow(0)
    val saved: StateFlow<Int> = count.asStateFlow()
    fun changed() { count.value++ }
}

/** The Goals screen's words, the same as the web's. */
internal object WorkGoalCopy {
    const val INTRO = "Your open goals, the sub-goals under them and who owns each. Mark goals done here; done and dropped goals are listed under Closed. Other changes stay in chat or in their notes."
    const val EXPAND_ALL = "Expand all"
    const val COLLAPSE_ALL = "Collapse all"
    const val LOADING = "Loading goals…"
    const val EMPTY_LEAD = "No goal workspaces yet."
    const val EMPTY_LINK = "Ask Magican in chat"
    const val EMPTY_TAIL = "to set one up from a template."
    const val NO_GOALS = "No goals in this workspace yet."
    const val NO_OPEN_GOALS = "No open goals in this workspace."
    const val NO_CLOSED_GOALS = "No closed goals in this workspace."
    /** The Goals screen when every workspace's goals are closed, and its Closed list when none are. */
    const val NO_OPEN_GOALS_ANYWHERE = "No open goals: every goal is done or dropped."
    const val NO_CLOSED_GOALS_ANYWHERE = "No closed goals yet. A goal marked done or dropped is listed here."
    const val ALL_CLOSED = "No open goals: every goal read here is done or dropped."
    const val SEE_CLOSED = "See closed goals"
    const val MARK_DONE = "Mark done"
    const val REOPEN = "Reopen"
    const val SAVING = "Saving…"
    /** The Goals screen opened on its Closed list: a route of the Goals view itself, as Quality's own route. */
    val CLOSED_ROUTE = WorkRoute("Goals", "closed", "Goals")
    const val NOT_LINKED = "Not linked"
    const val NOT_LINKED_DETAIL = "These goals, and the sub-goals under them, do not lead up to a main goal."
    /** Very deep chains stop at 8 levels; the rest are counted. */
    const val MAX_DEPTH = 8

    private fun plural(count: Int, one: String, many: String) = "$count ${if (count == 1) one else many}"
    fun notShown(count: Int, limits: WorkGoalReadLimits) =
        "${plural(count, "more note", "more notes")} in Goals/ not shown: this screen reads the first ${limits.notes} notes of a workspace. Open the notes to see the rest."
    fun foldersNotRead(count: Int, limits: WorkGoalReadLimits) =
        "${plural(count, "folder", "folders")} in Goals/ not read: this screen reads folders up to ${plural(limits.folderDepth, "level", "levels")} deep, and at most ${limits.folders} of them."
    fun problems(count: Int) = if (count == 1) "One item could not be read" else "$count items could not be read"
    fun closedParent(title: String) = "Its parent goal “$title” is closed."
    fun open(count: Int) = "Open $count"
    fun closed(count: Int) = "Closed $count"
    fun under(title: String) = "Under $title"
    fun closedOn(date: String) = "Closed $date"
    /** What Mark done or Reopen of [goal] did, as the web words it. */
    fun statusNotice(goal: WorkGoal, done: Boolean, result: WorkGoalSaveResult): String = when (result) {
        WorkGoalSaveResult.Saved -> if (done) "“${goal.title}” is marked done. It is listed under Closed." else "“${goal.title}” is open again."
        WorkGoalSaveResult.Conflict -> "“${goal.title}” changed since this screen loaded, so it was not ${if (done) "marked done" else "reopened"}. The goals have been reloaded; try again if it is still needed."
        is WorkGoalSaveResult.Failed -> "Could not ${if (done) "mark" else "reopen"} “${goal.title}”${if (done) " done" else ""}: ${result.message}"
    }
    fun deeper(count: Int) = "$count deeper ${if (count == 1) "goal is" else "goals are"} not shown here. Open the note to follow them."
}
