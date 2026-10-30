package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.*
import androidx.activity.compose.BackHandler
import androidx.activity.compose.LocalOnBackPressedDispatcherOwner
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.CallMade
import androidx.compose.material.icons.automirrored.outlined.CallReceived
import androidx.compose.material.icons.automirrored.outlined.Notes
import androidx.compose.material.icons.filled.Star
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material.icons.outlined.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

internal data class WorkRoute(val kind: String, val id: String = "", val title: String = "", val artifactHash: String = "", val taskId: String = "", val agentId: String = "", val goalId: String = "", val tier: String = "", val previousHash: String = "")

/** The scrolling page every Work screen sits in: 16 gutters, 20 between groups. */
@Composable
internal fun WorkPage(modifier: Modifier = Modifier, spacing: Int = WORK_GROUP_SPACING_DP, content: LazyListScope.() -> Unit) {
    LazyColumn(
        modifier.fillMaxSize().background(Ground),
        contentPadding = PaddingValues(start = WORK_SCREEN_PADDING_DP.dp, end = WORK_SCREEN_PADDING_DP.dp, top = 8.dp, bottom = 24.dp),
        verticalArrangement = Arrangement.spacedBy(spacing.dp),
        content = content,
    )
}

@Composable
internal fun WorkLibraryScreen(section: WorkSection, actions: TasksScreenActions, onOpenTask: (String) -> Unit, initialRoute: WorkRoute? = null, onOpenChat: () -> Unit = {}) {
    val stack = remember { mutableStateListOf<WorkRoute>().apply { initialRoute?.let(::add) } }
    val root = WorkRoute(section.name)
    val route = stack.lastOrNull() ?: root
    val push: (WorkRoute) -> Unit = { stack.add(it) }
    BackHandler(stack.isNotEmpty()) { if (stack.isNotEmpty()) stack.removeAt(stack.lastIndex) }
    val backDispatcher = LocalOnBackPressedDispatcherOwner.current?.onBackPressedDispatcher
    DisposableEffect(stack.isNotEmpty()) {
        actions.showDetail(stack.isNotEmpty())
        onDispose { actions.showDetail(false) }
    }
    // The bar's Refresh reaches every resource of the page on screen.
    val refreshes = remember { MutableSharedFlow<Unit>(extraBufferCapacity = 1) }
    DisposableEffect(actions, refreshes) {
        actions.bindRefresh { refreshes.tryEmit(Unit) }
        onDispose { actions.bindRefresh(null) }
    }
    Column(Modifier.fillMaxSize().background(Ground)) {
        if (stack.isNotEmpty()) WorkDetailBar(
            // A sealed-evidence route carries its expected hash as its title.
            if (route.kind == "sealed-evidence") "Sealed evidence" else route.title.ifEmpty { workLabel(route.kind) },
            // Through the dispatcher, so a screen guarding unsaved work (a program draft) sees it as system back does.
            onBack = { if (stack.isNotEmpty()) backDispatcher?.onBackPressed() ?: stack.removeAt(stack.lastIndex) },
            modifier = Modifier.statusBarsPadding(),
        ) {
            WorkBarIcon(Icons.Outlined.Refresh, "Refresh") { refreshes.tryEmit(Unit) }
            WorkBarIcon(Icons.Outlined.Add, "New work", enabled = actions.canCreate, onClick = actions::showCreate)
        }
        CompositionLocalProvider(LocalWorkRefreshRequests provides refreshes) {
            key(route) {
                when (route.kind) {
                    "Crew" -> WorkCrewList(push)
                    "member" -> WorkCrewDetail(route.id, push)
                    "member-edit" -> WorkCrewDetail(route.id, push, openEditor = true)
                    "member-rules" -> WorkCrewRules(route.id)
                    "member-bootstrap" -> WorkCrewBootstrap(route.id)
                    "member-memory" -> WorkCrewMemory(route.id, push)
                    "user-memory" -> WorkCrewUserMemory(route.id, push)
                    "memory-tier" -> WorkCrewTier(route.id, route.tier, route.goalId)
                    "member-create" -> WorkCrewCreate { if (stack.isNotEmpty()) stack.removeAt(stack.lastIndex) }
                    "operator" -> WorkOperatorScreen(route.id)
                    "Goals" -> WorkGoalsScreen(push, onOpenChat, actions = actions, closed = route.id == "closed")
                    "Programs" -> WorkProgramsList(push)
                    "program" -> WorkProgramDetail(route.id, push)
                    "Quality" -> WorkQualityScreen(push, onOpenTask, agentId = route.agentId)
                    "quality-tasks" -> WorkQualityTasks(push, onOpenTask)
                    "guidance-proposals" -> WorkGuidanceProposals(push)
                    "quality-context" -> WorkQualityScreen(push, onOpenTask, taskId = route.id)
                    "quality-run" -> WorkQualityRun(route.id, push, onOpenTask)
                    "quality-proposal" -> WorkQualityProposal(route.id, push)
                    "quality-campaign" -> WorkPolicies(WorkPolicyKind.Capture, push, onOpenTask, initialPolicyId = route.id)
                    "quality-compose" -> WorkQualityCompose(route.id, onOpenRun = { push(WorkRoute("quality-run", it, "Quality run")) }, agentId = route.agentId)
                    "artifact" -> WorkQualityArtifact(route.id, push)
                    "artifact-compare" -> WorkQualityComparison(route.id, route.artifactHash, route.previousHash)
                    "Operations" -> WorkOperations(push)
                    "capture-policies" -> WorkPolicies(WorkPolicyKind.Capture, push, onOpenTask)
                    "learning-policies" -> WorkPolicies(WorkPolicyKind.Learning, push, onOpenTask)
                    "capture-bind" -> WorkCaptureBinder(route.id, route.artifactHash, route.taskId, push)
                    "captures" -> WorkCaptures(route.id, push)
                    "sealed-evidence" -> WorkSealedEvidence(route.id, route.title)
                    "endpoint" -> WorkEndpoint(route.id)
                }
            }
        }
    }
}

private val CREW_VIEWS = listOf("Members", "Leaderboard", "Hierarchy", "Pipeline roles").map { it to it }
private val CREW_STATUSES = listOf("All", "Active", "Attention", "Paused", "Disabled").map { it to it }
private val CREW_KINDS = listOf("All", "Personal", "Worker").map { it to it }

/** A crew member's chips: kind, status (or Disabled) and Primary. */
private fun crewChips(member: WorkCrewMember): List<WorkChipSpec> = buildList {
    add(WorkChipSpec(if (member.kind.isBlank()) "Member" else workLabel(member.kind.lowercase()), Muted))
    add(WorkChipSpec(if (member.disabled) "Disabled" else member.status, if (member.disabled) Muted else workStatusTone(if (member.status == "idle") "active" else member.status)))
    if (member.isPrimary) add(WorkChipSpec("Primary", Coral, Icons.Filled.Star))
}

@Composable
private fun WorkCrewList(push: (WorkRoute) -> Unit) {
    val resource = rememberWorkResource("/api/magician/v2/agents")
    val state by resource.state.collectAsStateWithLifecycle()
    val health = rememberWorkResource("/api/magician/v2/agents/health")
    val healthState by health.state.collectAsStateWithLifecycle()
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    LaunchedEffect(resource, health, lifecycle) { lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) { while (true) { delay(30_000); resource.refresh(); health.refresh() } } }
    val scope = rememberCoroutineScope()
    var search by remember { mutableStateOf("") }
    var tab by remember { mutableStateOf("Members") }
    var status by remember { mutableStateOf("All") }
    var kind by remember { mutableStateOf("All") }
    var reloadDefinitions by remember { mutableStateOf(false) }
    LaunchedEffect(state.identity) { reloadDefinitions = false }
    val raw = state.value.workRows("agents").ifEmpty { (state.value as? JsonArray)?.toList().orEmpty() }
    val allMembers = raw.mapNotNull { runCatching { WorkCrewMember.decode(it as JsonObject) }.getOrNull() }
    fun memberHealth(id: String) = healthState.value.workRows("agents").firstOrNull { it.workText("agent_id") == id }
    var members = allMembers.filter { member ->
        val match = search.isBlank() || "${member.name} ${member.id} ${member.description}".contains(search, ignoreCase = true)
        val statusMatch = when (status) {
            "Active" -> member.status in listOf("running", "triggered")
            "Attention" -> member.status in listOf("error", "partial") || memberHealth(member.id).workField("overall").workField("inputs").workText("state") == "needs_attention"
            "Paused" -> member.status == "paused"
            "Disabled" -> member.disabled || member.status == "disabled"
            else -> true
        }
        match && statusMatch && (kind == "All" || member.kind.equals(kind, ignoreCase = true))
    }
    members = if (tab == "Leaderboard") members.sortedWith(compareByDescending<WorkCrewMember> { memberHealth(it.id).workField("rolling_7d").workText("calls")?.toLongOrNull() ?: -1L }
        .thenByDescending { memberHealth(it.id).workField("rolling_7d").workText("spend_usd")?.toDoubleOrNull() ?: -1.0 }.thenBy { it.name.lowercase() }) else members.sortedBy { it.name.lowercase() }
    WorkPage {
        item { WorkSearchField("Find crew", search, { search = it }) }
        item { WorkGroup("View and filters") {
            WorkChoiceRow("Crew view", CREW_VIEWS, tab, { tab = it })
            if (tab != "Pipeline roles") {
                WorkRowDivider(); WorkChoiceRow("Status", CREW_STATUSES, status, { status = it })
                WorkRowDivider(); WorkChoiceRow("Kind", CREW_KINDS, kind, { kind = it })
            }
        } }
        workStatusItem(state, resource)
        if (tab == "Leaderboard" || status == "Attention") workStatusItem(healthState, health, key = "health-status")
        if (tab == "Pipeline roles") {
            if (state.value != null) item {
                val pipeline = state.value.workRows("system_agents").filter { search.isBlank() || it.toString().contains(search, ignoreCase = true) }
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    WorkGroup("Pipeline roles") {
                        if (pipeline.isEmpty()) WorkRow("No matching pipeline descriptors were reported.", labelColor = Muted, accessory = WorkRowAccessory.None)
                        pipeline.forEachIndexed { index, role ->
                            if (index > 0) WorkRowDivider()
                            WorkItemRow(role.workText("name") ?: role.workText("agent_id").orEmpty(), subtitle = role.workText("agent_id"), detail = role.workText("description"),
                                icon = Icons.Outlined.SettingsSuggest, accessory = WorkRowAccessory.None)
                        }
                    }
                    WorkFootnote("Read-only runtime pipeline roles.")
                }
            }
        } else if (state.value != null) {
            // One item: a crew is a short list, and its cards sit closer together than the page's groups.
            item { Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                WorkGroupTitle(if (tab == "Members") "Members" else tab)
                WorkFootnote("${allMembers.size} members · ${allMembers.count { it.status in listOf("running", "triggered") }} active · ${allMembers.count { it.disabled }} disabled", color = Muted)
                if (members.isEmpty()) WorkGroup { WorkEmptyState(if (search.isBlank()) "No crew members match these filters." else "No matching crew members in this workspace.", icon = Icons.Outlined.GroupOff) }
                members.forEach { member -> key(member.id) { Box(Modifier.padding(top = 4.dp)) { WorkGroup {
                WorkItemRow(
                    (if (tab == "Leaderboard") "${members.indexOf(member) + 1}. " else "") + member.name,
                    detail = if (tab == "Members") member.description else null,
                    icon = if (member.isPrimary) Icons.Filled.Star else Icons.Outlined.AccountCircle,
                    chips = crewChips(member),
                ) { push(WorkRoute("member", member.id, member.name)) }
                when (tab) {
                    "Leaderboard" -> { WorkRowDivider(); CrewHealthSummary(memberHealth(member.id)) }
                    "Hierarchy" -> { WorkRowDivider(); CrewDelegation(member, allMembers, push) }
                }
            } } } }
            } }
        }
        item { WorkGroup {
            WorkActionRow("Add member", Icons.Outlined.PersonAddAlt) { push(WorkRoute("member-create", title = "Add member")) }
            WorkRowDivider()
            WorkActionRow("Reload definitions from disk", Icons.Outlined.SettingsBackupRestore, enabled = state.value != null && !state.busy) { reloadDefinitions = true }
        } }
    }
    if (reloadDefinitions) AlertDialog(onDismissRequest = { reloadDefinitions = false }, title = { Text("Reload Crew definitions from disk?", color = Ink) }, text = { Text("Reloads the runtime definition cache after edits made outside the app.", color = Muted) }, confirmButton = {
        TextButton(onClick = { val identity = state.identity; reloadDefinitions = false; scope.launch { if (resource.perform("/api/magician/v2/agents/refresh-definitions", identity) != null) { resource.refresh(); health.refresh() } } }) { Text("Reload definitions", color = Coral) }
    }, dismissButton = { TextButton(onClick = { reloadDefinitions = false }) { Text("Cancel", color = Muted) } }, containerColor = Ground)
}

@Composable
private fun CrewHealthSummary(metrics: JsonElement?) {
    if (metrics == null) { WorkNote("Health and activity have not been reported."); return }
    val overall = metrics.workField("overall"); val rolling = metrics.workField("rolling_7d")
    WorkRow("Health", icon = Icons.Outlined.MonitorHeart, value = "${overall.workField("score").workDisplay()} · ${overall.workField("inputs").workText("state").orEmpty().replace('_', ' ')}", accessory = WorkRowAccessory.None)
    WorkRowDivider()
    WorkRow("Last 7 days", icon = Icons.Outlined.BarChart, value = "${rolling.workField("calls").workDisplay()} calls · $${rolling.workField("spend_usd").workDisplay()}",
        detail = "Success ${rolling.workField("success_rate").workDisplay()}", accessory = WorkRowAccessory.None)
    WorkRowDivider()
    WorkRecordRow("Health, coverage and history", metrics, icon = Icons.AutoMirrored.Outlined.Notes)
}

@Composable
private fun CrewDelegation(member: WorkCrewMember, allMembers: List<WorkCrewMember>, push: (WorkRoute) -> Unit) {
    fun targets(record: WorkCrewMember) = record.definition.workRows("delegation_targets").mapNotNull { (it as? JsonPrimitive)?.contentOrNull }
    val targets = targets(member)
    val parents = allMembers.filter { member.id in targets(it) || "*" in targets(it) }
    WorkRow("Delegation depth", icon = Icons.Outlined.AccountTree, value = member.definition.workField("constraints").workField("coordination").workField("max_delegation_depth").workDisplay(), accessory = WorkRowAccessory.None)
    WorkRowDivider()
    if (parents.isEmpty()) WorkRow("No configured parent", icon = Icons.AutoMirrored.Outlined.CallReceived, labelColor = Muted, accessory = WorkRowAccessory.None)
    parents.forEachIndexed { index, parent ->
        if (index > 0) WorkRowDivider()
        WorkRow("From: ${parent.name}", icon = Icons.AutoMirrored.Outlined.CallReceived) { push(WorkRoute("member", parent.id, parent.name)) }
    }
    WorkRowDivider()
    if (targets.isEmpty()) WorkRow("No delegation targets", icon = Icons.AutoMirrored.Outlined.CallMade, labelColor = Muted, accessory = WorkRowAccessory.None)
    targets.distinct().forEachIndexed { index, id ->
        if (index > 0) WorkRowDivider()
        val target = allMembers.firstOrNull { it.id == id }
        when {
            id == "*" -> WorkRow("Can delegate to any member", icon = Icons.AutoMirrored.Outlined.CallMade, accessory = WorkRowAccessory.None)
            target == null -> WorkNote("Missing target: $id", color = activePalette.warning)
            else -> {
                WorkRow("To: ${target.name}", icon = Icons.AutoMirrored.Outlined.CallMade) { push(WorkRoute("member", id, target.name)) }
                crewDelegationCycle(member.id, id, allMembers)?.let { cycle -> WorkNote("Delegation cycle: ${cycle.joinToString(" → ")}", color = activePalette.warning) }
            }
        }
    }
}

@Composable
private fun WorkProgramsList(push: (WorkRoute) -> Unit) {
    val resource = rememberWorkResource("/api/magician/v2/programs")
    val state by resource.state.collectAsStateWithLifecycle()
    WorkPage {
        workStatusItem(state, resource)
        state.value?.let { value -> item { Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            val rows = workPlaybookSummaries(value)
            WorkGroupTitle("Playbook documents")
            WorkFootnote(workPlaybookCount(rows.size), color = Muted, modifier = Modifier.testTag("work-programs-count"))
            WorkGroup {
                if (rows.isEmpty()) WorkEmptyState(WorkLandingCopy.NO_PLAYBOOKS, icon = Icons.Outlined.Description)
                rows.forEachIndexed { index, row ->
                    if (index > 0) WorkRowDivider()
                    WorkItemRow(row.title, subtitle = row.name, icon = Icons.Outlined.Description, modifier = Modifier.testTag("work-program-${row.name}")) {
                        push(WorkRoute("program", row.name, row.title))
                    }
                }
            }
        } } }
        item { WorkProgramMembers("", push) }
    }
}

/** What an unsaved program draft is being discarded for. */
private enum class ProgramDiscard { Cancel, Reload, Leave }

@Composable
private fun WorkProgramDetail(name: String, push: (WorkRoute) -> Unit) {
    val path = "/api/magician/v2/programs/${WorkRepository.segment(name)}"
    val resource = rememberWorkResource(path)
    val state by resource.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    var restore by remember { mutableStateOf<String?>(null) }
    // Owner edit: the draft is bound to the version (content + hash) it was opened from.
    var editing by remember { mutableStateOf(false) }
    var draft by remember { mutableStateOf("") }
    var editOriginal by remember { mutableStateOf("") }
    var editBase by remember { mutableStateOf("") }
    var saveError by remember { mutableStateOf<String?>(null) }
    var conflict by remember { mutableStateOf(false) }
    var discard by remember { mutableStateOf<ProgramDiscard?>(null) }
    fun stopEditing() { editing = false; draft = ""; editOriginal = ""; editBase = ""; saveError = null; conflict = false; discard = null }
    LaunchedEffect(state.identity) { restore = null; stopEditing() }
    val dirty = editing && draft != editOriginal
    val back = LocalOnBackPressedDispatcherOwner.current?.onBackPressedDispatcher
    // Leaving waits a frame: the guard below must stand aside before back is replayed.
    var leaving by remember { mutableStateOf(false) }
    LaunchedEffect(leaving) { if (leaving) { leaving = false; back?.onBackPressed() } }
    fun afterDiscard(reason: ProgramDiscard) {
        stopEditing()
        when (reason) {
            ProgramDiscard.Cancel -> Unit
            ProgramDiscard.Reload -> resource.refresh()
            ProgramDiscard.Leave -> leaving = true
        }
    }
    fun requestDiscard(reason: ProgramDiscard) { if (state.busy) return; if (dirty) discard = reason else afterDiscard(reason) }
    BackHandler(enabled = dirty) { requestDiscard(ProgramDiscard.Leave) }
    fun save() {
        val content = draft; val base = editBase; val identity = state.identity
        if (!editing || state.busy || conflict || content == editOriginal || content.isBlank()) return
        saveError = null
        scope.launch {
            val outcome = resource.write(identity, successNotice = "Playbook saved") { it.saveProgram(name, content, base) } ?: return@launch
            outcome.onSuccess { stopEditing(); resource.refresh() }.onFailure { error ->
                if (error is WorkProgramChangedError) {
                    conflict = true
                    saveError = "${error.message} Your edits are still in the editor; copy anything you need, then reload the latest version."
                } else saveError = error.message ?: "The playbook could not be saved."
            }
        }
    }
    WorkPage(Modifier.imePadding()) {
        workStatusItem(state, resource)
        state.value?.let { document ->
            val sha256 = document.workText("sha256")?.trim().orEmpty()
            item { WorkHeaderCard(document.workText("title") ?: name, subtitle = document.workText("name") ?: name, trailing = {
                // A server that predates owner editing sends no hash to save against.
                if (!editing && sha256.isNotEmpty()) OutlinedButton(
                    onClick = {
                        draft = document.workText("content").orEmpty(); editOriginal = draft; editBase = sha256
                        saveError = null; conflict = false; editing = true
                    },
                    enabled = !state.busy, shape = MagicanButtonShape, border = BorderStroke(1.dp, Coral.copy(alpha = 0.5f)),
                    colors = ButtonDefaults.outlinedButtonColors(contentColor = Coral),
                    contentPadding = PaddingValues(horizontal = 14.dp),
                    modifier = Modifier.heightIn(min = 48.dp).testTag("work-program-edit"),
                ) { Icon(Icons.Outlined.Edit, null, modifier = Modifier.size(16.dp)); Spacer(Modifier.width(6.dp)); Text("Edit") }
            }) }
            if (editing) item { Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                WorkGroupTitle("Markdown source")
                WorkCard {
                    WorkLabeledField("Markdown for ${document.workText("name") ?: name}", draft, { draft = it }, enabled = !state.busy,
                        fieldModifier = Modifier.heightIn(max = 560.dp).testTag("work-program-editor"), minLines = 12, maxLines = Int.MAX_VALUE, mono = true)
                    saveError?.let { message ->
                        val tone = if (conflict) MWarn else Danger
                        Surface(color = tone.copy(alpha = 0.10f), shape = RoundedCornerShape(10.dp), border = BorderStroke(1.dp, tone.copy(alpha = 0.45f)), modifier = Modifier.fillMaxWidth()) {
                            Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                                Row(verticalAlignment = Alignment.Top) {
                                    Icon(Icons.Filled.Warning, null, tint = tone, modifier = Modifier.padding(top = 2.dp).size(16.dp))
                                    Spacer(Modifier.width(8.dp))
                                    Text(message, color = Ink, fontSize = 14.sp, modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite }.testTag("work-program-save-error"))
                                }
                                if (conflict) TextButton(onClick = { requestDiscard(ProgramDiscard.Reload) }, enabled = !state.busy, modifier = Modifier.heightIn(min = 48.dp).testTag("work-program-reload")) { Text("Reload latest version", color = Coral) }
                            }
                        }
                    }
                    Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                        OutlinedButton(onClick = { requestDiscard(ProgramDiscard.Cancel) }, enabled = !state.busy, shape = MagicanButtonShape, border = BorderStroke(1.dp, Coral.copy(alpha = 0.5f)),
                            colors = ButtonDefaults.outlinedButtonColors(contentColor = Coral), modifier = Modifier.weight(1f).heightIn(min = 48.dp).testTag("work-program-cancel")) { Text("Cancel") }
                        Button(onClick = { save() }, enabled = !state.busy && !conflict && draft != editOriginal && draft.isNotBlank(),
                            colors = magicanButtonColors(), shape = MagicanButtonShape, modifier = Modifier.weight(1f).heightIn(min = 48.dp).testTag("work-program-save")) {
                            Text(if (state.busy) "Saving…" else "Save")
                        }
                    }
                }
                WorkFootnote("Saving keeps the current version in Document history, so you can restore it.")
            } } else item { Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                WorkGroupTitle("Document")
                WorkCard {
                    SelectionContainer { MarkdownText(document.workText("content").orEmpty(), color = Ink, fontSize = 15.sp, lineHeight = 21.sp) }
                    if (sha256.isEmpty()) Text("Editing needs a newer Magician runtime.", color = Muted, fontSize = 12.sp)
                }
            } }
            item { WorkProgramMembers(name, push) }
            item { Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                WorkGroup("Document history") {
                    val history = document.workRows("history").filter { it.workText("snapshot") != null }
                    if (history.isEmpty()) WorkRow("No saved versions.", icon = Icons.Outlined.History, labelColor = Muted, accessory = WorkRowAccessory.None)
                    history.forEachIndexed { index, version ->
                        val snapshot = version.workText("snapshot").orEmpty()
                        if (index > 0) WorkRowDivider()
                        WorkRow(version.workText("modified") ?: snapshot, icon = Icons.Outlined.History, detail = snapshot, accessory = WorkRowAccessory.None, trailing = {
                            TextButton(onClick = { restore = snapshot }, enabled = !state.busy && !editing, modifier = Modifier.heightIn(min = 48.dp)) {
                                Text("Restore version", color = if (editing) Muted else Coral, fontWeight = FontWeight.SemiBold)
                            }
                        })
                    }
                }
                if (editing) WorkFootnote("Finish editing to restore a saved version.")
            } }
            item { WorkGroup("Web console") { WorkRelatedTools(WorkWebContext(section = WorkSection.Programs, program = name), listOf(WorkSpecialist.History, WorkSpecialist.Memory, WorkSpecialist.Evals, WorkSpecialist.Events)) } }
        }
    }
    restore?.let { snapshot -> AlertDialog(
        onDismissRequest = { restore = null }, title = { Text("Restore playbook version?", color = Ink) },
        text = { Text("Restore $snapshot through the existing playbook provider.", color = Muted) },
        confirmButton = { TextButton(onClick = {
            val identity = state.identity; restore = null
            scope.launch { if (resource.perform("$path/revert", identity, body = buildJsonObject { put("snapshot", snapshot) }) != null) resource.refresh() }
        }) { Text("Restore", color = Coral) } },
        dismissButton = { TextButton(onClick = { restore = null }) { Text("Keep current", color = Muted) } }, containerColor = Ground,
    ) }
    discard?.let { reason -> AlertDialog(
        onDismissRequest = { discard = null }, title = { Text("Discard changes", color = Ink) },
        text = { Text("Discard your unsaved changes to ${state.value.workText("title") ?: name}?", color = Muted) },
        confirmButton = { TextButton(onClick = { afterDiscard(reason) }, modifier = Modifier.testTag("work-program-discard")) { Text("Discard changes", color = Danger) } },
        dismissButton = { TextButton(onClick = { discard = null }) { Text("Keep editing", color = Coral) } }, containerColor = Ground,
    ) }
}

@Composable
internal fun WorkEndpoint(path: String) {
    val resource = rememberWorkResource(path)
    val state by resource.state.collectAsStateWithLifecycle()
    WorkPage {
        workStatusItem(state, resource)
        state.value?.let { value -> item { WorkGroup { WorkRecord(value) } } }
    }
}

@Composable
internal fun WorkProgramMembers(program: String, push: (WorkRoute) -> Unit) {
    val crew = rememberWorkResource("/api/magician/v2/agents")
    val state by crew.state.collectAsStateWithLifecycle()
    val members = state.value.workRows("agents").filter { it.workField("definition").workText("kind").equals("personal", ignoreCase = true) && it.workField("definition").workField("harness") is JsonObject }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        WorkGroupTitle(if (program.isBlank()) "Runtime direction and progress" else "Responsible crew and progress")
        Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
            WorkResourceStatus(state, crew)
            if (state.value != null && members.isEmpty()) WorkGroup { WorkRow("No playbook-enabled crew in this workspace.", icon = Icons.Outlined.GroupOff, labelColor = Muted, accessory = WorkRowAccessory.None) }
            members.forEach { member -> key(member.workField("definition").workText("agent_id")) { WorkProgramProgress(member, program, push) } }
        }
    }
}

/** "Current phase: Verify migration" as one text, so screen readers and tests read it whole. */
@Composable
private fun ProgramFact(title: String, value: String) {
    Text(
        buildAnnotatedString {
            withStyle(SpanStyle(color = Muted, fontWeight = FontWeight.SemiBold, fontSize = 14.sp)) { append("$title: ") }
            withStyle(SpanStyle(color = Ink, fontSize = 15.sp)) { append(value) }
        },
        lineHeight = 20.sp,
        modifier = Modifier.fillMaxWidth().heightIn(min = WORK_ROW_MIN_HEIGHT_DP.dp).padding(vertical = 12.dp),
    )
}

@Composable
private fun WorkProgramProgress(record: JsonElement, program: String, push: (WorkRoute) -> Unit) {
    val definition = record.workField("definition")
    val agentId = definition.workText("agent_id").orEmpty()
    val overview = rememberWorkResource("/api/magician/v2/agents/${WorkRepository.segment(agentId)}/harness-overview")
    val state by overview.state.collectAsStateWithLifecycle()
    val openTask = LocalOpenTaskDetail.current
    val references = listOf(buildJsonObject { put("path", "program.md"); put("section", definition.workField("harness").workField("program_section") ?: JsonNull) }) +
        definition.workField("autonomous_config").workRows("focus_areas").map { area -> buildJsonObject {
            val path = area.workText("program").orEmpty().trim()
            put("path", path.ifBlank { "program.md" }); put("focus", area.workField("name") ?: JsonNull)
            put("section", if (path.isBlank()) definition.workField("harness").workField("program_section") ?: JsonNull else JsonNull)
        } }
    val active = state.value.workField("evaluation").workField("active_program_state")
    val associated = program.isBlank() || references.any { it.workText("path") == program } || state.value.workField("context").workField("program").workText("relative_path") == program
    val matching = program.isBlank() || active.workField("program").workText("relative_path") == program
    if (associated || state.loading || state.error != null) WorkGroup {
        WorkItemRow(definition.workText("name") ?: agentId, subtitle = "Definition version: ${record.workField("version").workDisplay()}",
            icon = if (definition.workBool("is_primary")) Icons.Filled.Star else Icons.Outlined.AccountCircle) { push(WorkRoute("member", agentId, "Crew member")) }
        references.filter { program.isBlank() || it.workText("path") == program }.forEach { reference ->
            WorkRowDivider()
            WorkRow(reference.workText("path").orEmpty(), icon = Icons.Outlined.Description,
                value = listOfNotNull(reference.workText("section"), reference.workText("focus")?.let { "focus: $it" }).joinToString(" · ").ifBlank { null }, accessory = WorkRowAccessory.None)
        }
        if (state.hasStatus) Box(Modifier.padding(vertical = 8.dp)) { WorkResourceStatus(state, overview) }
        if (state.value != null) {
            WorkRowDivider()
            if (matching && active is JsonObject && active.isNotEmpty()) {
                ProgramFact("Current source", active.workField("program").workField("relative_path").workDisplay())
                listOf("current_phase", "current_step", "updated_at", "update_reason").forEach { key ->
                    active.workField(key)?.takeIf { it != JsonNull }?.let { WorkRowDivider(); ProgramFact(workLabel(key), it.workDisplay()) }
                }
                if (active.workRows("open_loops").isNotEmpty()) { WorkRowDivider(); WorkRecordRow("Open loops", active.workField("open_loops"), icon = Icons.Outlined.Pending) }
                active.workField("last_run_summary")?.takeIf { it != JsonNull }?.let { WorkRowDivider(); WorkRecordRow("Latest cycle summary", it, icon = Icons.AutoMirrored.Outlined.Notes) }
                active.workRows("update_history").takeLast(5).reversed().forEach { update ->
                    WorkRowDivider()
                    WorkNote(update.workField("reason").workDisplay(), color = Ink)
                    update.workText("task_id")?.let { id -> WorkRow("Task and execution evidence", icon = Icons.Outlined.FindInPage, enabled = openTask != null) { openTask?.open(id, update.workText("execution_id"), false) } }
                }
            } else WorkRow("No current runtime progress for this document.", icon = Icons.Outlined.HourglassEmpty, labelColor = Muted, accessory = WorkRowAccessory.None)
            val metrics = state.value.workField("evaluation").workField("program_metrics").workRows("documents").filter { program.isBlank() || it.workText("relative_path") == program }
            if (metrics.isNotEmpty()) { WorkRowDivider(); WorkRecordRow("Evaluation metrics", JsonArray(metrics), icon = Icons.Outlined.BarChart) }
        }
        WorkRowDivider()
        WorkRow("Focus health and operating controls", icon = Icons.Outlined.Speed) { push(WorkRoute("operator", agentId, "Operator view")) }
    }
}
