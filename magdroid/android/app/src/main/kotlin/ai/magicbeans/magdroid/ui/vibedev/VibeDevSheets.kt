package ai.magicbeans.magdroid.ui.vibedev

import ai.magicbeans.magdroid.ui.LocalMagicanFontFamilies
import ai.magicbeans.magdroid.ui.MagicianTextField
import ai.magicbeans.magdroid.ui.activePalette
import ai.magicbeans.magdroid.ui.magicanSwitchColors
import ai.magicbeans.magdroid.ui.ModelThinkingControl
import ai.magicbeans.magdroid.ui.CatalogChoiceDropdown
import ai.magicbeans.magdroid.ui.CatalogChoiceOption
import ai.magicbeans.magdroid.ui.catalogEngineLabel
import ai.magicbeans.magdroid.ui.BasicComposerField
import ai.magicbeans.magdroid.ui.ComposerModeBand
import ai.magicbeans.magdroid.ui.ComposerModeSegment
import ai.magicbeans.magdroid.ui.ComposerCard
import ai.magicbeans.magdroid.ui.ComposerSelectorChip
import ai.magicbeans.magdroid.ui.ComposerKeyboardDismiss
import ai.magicbeans.magdroid.ui.ComposerModelPickerSheet
import ai.magicbeans.magdroid.ui.ComposerModelCatalogControls
import ai.magicbeans.magdroid.ui.VoiceHero
import ai.magicbeans.magdroid.ui.composerModeTrackColor
import ai.magicbeans.magdroid.ui.AccentFill
import ai.magicbeans.magdroid.ui.OnAccent
import ai.magicbeans.magdroid.ui.Lively
import ai.magicbeans.magdroid.vibedev.VibeDevComposed
import ai.magicbeans.magdroid.vibedev.VibeDevComposerViewModel
import ai.magicbeans.magdroid.vibedev.VibeDevNowViewModel
import ai.magicbeans.magdroid.vibedev.VibeDevProject
import ai.magicbeans.magdroid.voice.Dictation
import ai.magicbeans.magdroid.voice.DictationState
import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.FolderOpen
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.Storage
import androidx.compose.material.icons.filled.Mic
import androidx.compose.material.icons.outlined.Mic
import androidx.compose.material.icons.outlined.Stop
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Surface
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch

/** Hosts whichever VibeDev sheet is open. */
@Composable
internal fun VibeDevSheets(
    chrome: VibeDevChrome,
    composer: VibeDevComposerViewModel,
    now: VibeDevNowViewModel,
    snackbar: SnackbarHostState,
) {
    val scope = rememberCoroutineScope()
    val nowData by now.data.collectAsStateWithLifecycle()
    val state by composer.state.collectAsStateWithLifecycle()
    // A project screen can be the first read; make sure the picker has rows.
    LaunchedEffect(chrome.sheet) {
        if (chrome.sheet is VibeDevSheet.NewRun && nowData.projects.isEmpty()) now.refresh(quiet = true)
    }
    LaunchedEffect(state.result) {
        val result = state.result ?: return@LaunchedEffect
        composer.consumeResult()
        chrome.sheet = null
        when (result) {
            is VibeDevComposed.RunStarted -> {
                scope.launch { snackbar.showSnackbar(if (result.followUp) "Follow-up sent" else if (result.replayed) "Already started" else "Run started") }
                if (!result.followUp) chrome.push(VibeDevRoute.Run(result.taskId, result.title))
                chrome.refreshTick++
            }
            is VibeDevComposed.CrewProposed -> {
                scope.launch { snackbar.showSnackbar(if (result.crew.isRace) "Race set up. Start it from the project." else "Lanes planned. Start the crew from the project.") }
                chrome.reset()
                chrome.push(VibeDevRoute.Project(result.projectId, nowData.projects.firstOrNull { it.projectId == result.projectId }?.name.orEmpty()))
                chrome.refreshTick++
            }
            is VibeDevComposed.ProjectCreated -> {
                scope.launch { snackbar.showSnackbar("Project added") }
                chrome.push(VibeDevRoute.Project(result.project.projectId, result.project.name))
                now.refresh(quiet = true)
            }
        }
    }
    when (val sheet = chrome.sheet) {
        is VibeDevSheet.NewRun -> VibeDevComposer(sheet, nowData.openProjects, composer, onDismiss = { chrome.sheet = null })
        is VibeDevSheet.Steer -> SteerSheet(sheet, onDismiss = { chrome.sheet = null })
        VibeDevSheet.NewProject -> NewProjectSheet(composer, onDismiss = { chrome.sheet = null })
        null -> Unit
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun VibeSheet(onDismiss: () -> Unit, title: String, composerLayout: Boolean = false, welcome: (@Composable () -> Unit)? = null, content: @Composable ColumnScope.() -> Unit) {
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = if (composerLayout) activePalette.background else activePalette.elevated,
        contentColor = activePalette.text,
        scrimColor = activePalette.text.copy(alpha = 0.32f),
    ) {
        if (composerLayout) {
            Column(Modifier.fillMaxWidth().fillMaxHeight(0.9f).imePadding().navigationBarsPadding()) {
                Box(Modifier.weight(1f).fillMaxWidth().padding(horizontal = 24.dp), contentAlignment = Alignment.Center) {
                    if (welcome != null) Column(Modifier.verticalScroll(rememberScrollState())) { welcome() }
                }
                content()
            }
        } else {
            Column(
                Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).imePadding().navigationBarsPadding()
                    .padding(start = 20.dp, end = 20.dp, bottom = 20.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Text(title, modifier = Modifier.padding(horizontal = 0.dp), color = activePalette.text, fontSize = 18.sp, fontWeight = FontWeight.Bold,
                    fontFamily = LocalMagicanFontFamilies.current.display)
                content()
            }
        }
    }
}

@Composable
private fun FieldLabel(text: String) {
    Text(text, color = activePalette.secondaryText, fontSize = 12.sp, fontWeight = FontWeight.SemiBold,
        fontFamily = LocalMagicanFontFamilies.current.body)
}

@Composable
private fun SwitchRow(title: String, detail: String?, checked: Boolean, onChange: (Boolean) -> Unit) {
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Column(Modifier.weight(1f)) {
            Text(title, color = activePalette.text, fontSize = 14.sp, fontWeight = FontWeight.SemiBold,
                fontFamily = LocalMagicanFontFamilies.current.body)
            detail?.let { MetaText(it, maxLines = 3) }
        }
        Switch(checked = checked, onCheckedChange = onChange, colors = magicanSwitchColors())
    }
}

/**
 * New run, and Follow up when [VibeDevSheet.NewRun.parentTaskId] is set.
 * On a trusted project the VibeDev crew switch turns it into a crew: split
 * the goal into lanes, or race engines at it.
 */
@Composable
internal fun VibeDevComposer(
    sheet: VibeDevSheet.NewRun,
    projects: List<VibeDevProject>,
    composer: VibeDevComposerViewModel,
    onDismiss: () -> Unit,
    embedded: Boolean = false,
    workLocation: String? = null,
    onOpenProject: ((String) -> Unit)? = null,
    showPendingAsks: Boolean = true,
) {
    val state by composer.state.collectAsStateWithLifecycle()
    LaunchedEffect(sheet) { composer.open() }
    var prompt by remember { mutableStateOf("") }
    var composerFocusRequest by remember { mutableIntStateOf(0) }
    var projectId by remember { mutableStateOf(sheet.projectId ?: projects.firstOrNull()?.projectId) }
    LaunchedEffect(projects) {
        if (projectId == null) projectId = projects.firstOrNull()?.projectId
    }
    var mode by remember { mutableStateOf(sheet.mode) }
    var lastPanelMode by remember { mutableStateOf(sheet.mode.takeUnless { it == "build" } ?: "discuss") }
    val panelMode = if (mode == "build") lastPanelMode else mode
    androidx.compose.runtime.SideEffect { if (mode != "build") lastPanelMode = mode }
    var profileId by remember { mutableStateOf(sheet.codingRouteId) }
    val selectedProfile = if (profileId == null) state.profiles.firstOrNull { it.isDefault }
        else state.profiles.firstOrNull { it.id == profileId }
    val models = remember(state.profiles, selectedProfile?.engine) { state.profiles.filter { it.engine == selectedProfile?.engine }.mapNotNull { it.modelChoice } }
    val engines = remember(state.profiles) { state.profiles.filter { it.selectable }.distinctBy { it.engine ?: it.id } }
    val selection = selectedProfile?.modelSelection
    val selectedModel = models.firstOrNull { it.id == selection?.modelId }
    var budget by remember { mutableStateOf("") }
    var nightly by remember { mutableStateOf(false) }
    var attachments by remember(projectId) { mutableStateOf(emptyList<ai.magicbeans.magdroid.chat.StagedAttachment>()) }
    var crew by remember { mutableStateOf(sheet.crew) }
    var race by remember { mutableStateOf(false) }
    var attempts by remember { mutableIntStateOf(3) }
    var review by remember { mutableStateOf(true) }
    val followUp = sheet.parentTaskId != null
    val project = projects.firstOrNull { it.projectId == projectId }
    var location by remember(projectId) { mutableStateOf(project?.trust ?: "sandboxed") }
    val trusted = location == "trusted"
    val crewOn = crew && trusted && !followUp
    LaunchedEffect(state.profiles) {
        if (profileId == null) profileId = state.profiles.firstOrNull { it.engine == sheet.engine && it.modelSelection?.thinking == "auto" }?.id
            ?: state.profiles.firstOrNull { it.isDefault }?.id
    }

    var showModels by remember { mutableStateOf(false) }
    var showProjects by remember { mutableStateOf(false) }
    var showLocations by remember { mutableStateOf(false) }
    var wasSending by remember { mutableStateOf(false) }
    LaunchedEffect(state.submitting) {
        if (wasSending && !state.submitting && state.error == null) prompt = ""
        wasSending = state.submitting
    }
    val title = when {
        followUp -> "Follow up"
        crewOn -> "New VibeDev crew"
        else -> "New run"
    }
    val send = {
        projectId?.let { target ->
            if (crewOn) composer.proposeCrew(target, prompt, race, attempts, review, budget.toDoubleOrNull())
            else composer.startRun(prompt, target, mode, sheet.parentTaskId, profileId, budget.toDoubleOrNull(), if (followUp) null else location,
                attachments, project?.chatSessionId, nightly)
        }
        Unit
    }
    val form: @Composable ColumnScope.() -> Unit = {
        if (showPendingAsks) VibeDevAskDock(setOfNotNull(sheet.parentTaskId), project?.chatSessionId)
        sheet.parentTitle?.let { MetaText("Continues ${it.removePrefix("VibeDev · ")}", maxLines = 2) }
        PromptField(
            value = prompt,
            onValueChange = { prompt = it },
            placeholder = if (crewOn) "What should the crew build?" else if (followUp) "What next?" else "What should VibeDev do?",
            minLines = 4,
        )
        if (!followUp) {
            WorkLocationPicker(location) { location = it }
            FieldLabel("Project")
            if (projects.isEmpty()) MetaText("No projects yet. Add one from the + on VibeDev.")
            Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                projects.forEach { candidate ->
                    Pill(candidate.name, candidate.projectId == projectId, { projectId = candidate.projectId },
                        leading = if (candidate.health.needsAttention) {
                            { ProjectHealthGlyph(candidate.health, size = 14) }
                        } else null)
                }
            }
        }
        project?.let { ProjectHealthBanner(it.health) }
        if (!crewOn) {
            FieldLabel("Mode")
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                Pill("Build", mode == "build", { mode = "build" })
                Pill("Discuss", mode == "discuss", { mode = "discuss" })
                Pill("Autopilot", mode == "autopilot", { mode = "autopilot" })
            }
            FieldLabel("Engine")
            CatalogChoiceDropdown("Engine", if (profileId == null) "" else selectedProfile?.engine ?: profileId.orEmpty(),
                listOf(CatalogChoiceOption("", "Default")) + engines.map {
                    CatalogChoiceOption(it.engine ?: it.id, catalogEngineLabel(it.engine ?: it.id, it.credentials))
                }) { engine -> profileId = if (engine.isEmpty()) null else engines.firstOrNull { (it.engine ?: it.id) == engine }?.id }
            if (models.isNotEmpty()) {
                FieldLabel("Model")
                CatalogChoiceDropdown("Model", selectedModel?.id.orEmpty(),
                    models.map { CatalogChoiceOption(it.id, it.label, it.providerDisplayName) }) { id ->
                    models.firstOrNull { it.id == id }?.let { model ->
                        profileId = model.choices[selection?.thinking ?: "auto"] ?: model.choices["auto"]
                    }
                }
                selectedModel?.let { model ->
                    model.limits?.let { MetaText("${it.contextWindowTokens} token context") }
                    ModelThinkingControl(model.thinking, selection?.thinking ?: "auto") { level -> model.choices[level]?.let { profileId = it } }
                }
            }
        }
        MagicianTextField(
            value = budget,
            onValueChange = { text -> budget = text.filter { it.isDigit() || it == '.' }.take(8) },
            modifier = Modifier.fillMaxWidth(),
            singleLine = true,
            label = { Text("Budget, USD (optional)") },
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal),
        )
        if (trusted && !followUp) {
            SwitchRow("VibeDev crew", "Split the goal into lanes that run at once", crew, { crew = it })
        }
        if (crewOn) {
            FieldLabel("Crew")
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                Pill("Split the goal", !race, { race = false })
                Pill("Race engines", race, { race = true })
            }
            if (race) {
                FieldLabel("Attempts")
                Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    (2..8).forEach { count -> Pill("$count", attempts == count, { attempts = count }) }
                }
            }
            SwitchRow("Review the merged branch first", null, review, { review = it })
        }
        state.error?.let { Text(it, color = activePalette.danger, fontSize = 13.sp) }
        VibeButton(
            when {
                crewOn && race -> "Set up race"
                crewOn -> "Plan lanes"
                followUp -> "Send follow-up"
                else -> "Start run"
            },
            onClick = send,
            modifier = Modifier.fillMaxWidth(),
            kind = VibeButtonKind.Primary,
            enabled = prompt.isNotBlank() && projectId != null && (crewOn || selectedModel != null),
            busy = state.submitting,
        )
    }
    val compact: @Composable () -> Unit = {
        VibeDevChatComposer(
            askPanel = { if (showPendingAsks) VibeDevAskDock(setOfNotNull(sheet.parentTaskId), project?.chatSessionId) },
            prompt = prompt, onPrompt = { prompt = it }, project = project,
            attachments = attachments, onAttachments = { transform -> attachments = transform(attachments) },
            submitting = state.submitting, focusRequest = composerFocusRequest,
            canSend = projectId != null && (crewOn || selectedModel != null), onSend = send,
            placeholder = if (followUp) "What next?" else "What should VibeDev do?",
            modePanel = {
                androidx.compose.animation.AnimatedVisibility(visible = mode != "build",
                    enter = androidx.compose.animation.expandVertically() + androidx.compose.animation.fadeIn(),
                    exit = androidx.compose.animation.shrinkVertically() + androidx.compose.animation.fadeOut()) {
                    ComposerModeBand(if (panelMode == "discuss") "Discuss" else "Autopilot",
                        if (panelMode == "discuss") "Read and plan, without code edits."
                        else "Runs unattended; notifies you.", maxLines = 1) {
                        if (panelMode == "autopilot") Row(Modifier.fillMaxWidth().padding(bottom = 2.dp),
                            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                            val underline = activePalette.secondaryText.copy(alpha = 0.45f)
                            androidx.compose.foundation.text.BasicTextField(
                                value = budget, onValueChange = { budget = it }, singleLine = true,
                                textStyle = androidx.compose.ui.text.TextStyle(color = activePalette.text, fontSize = 12.sp,
                                    fontFamily = LocalMagicanFontFamilies.current.body),
                                cursorBrush = androidx.compose.ui.graphics.SolidColor(activePalette.accentFill),
                                modifier = Modifier.weight(1f).heightIn(min = 34.dp)
                                    .drawBehind { drawLine(underline, androidx.compose.ui.geometry.Offset(0f, size.height),
                                        androidx.compose.ui.geometry.Offset(size.width, size.height), strokeWidth = 1.dp.toPx()) }
                                    .padding(vertical = 8.dp)
                                    .semantics { contentDescription = "Budget USD (optional)" },
                                decorationBox = { inner -> Box(contentAlignment = Alignment.CenterStart) {
                                    if (budget.isEmpty()) Text("Budget USD", color = activePalette.secondaryText.copy(alpha = 0.6f), fontSize = 12.sp)
                                    inner()
                                } },
                                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal))
                            if (!followUp) Row(verticalAlignment = Alignment.CenterVertically,
                                horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                                Text("Run nightly", color = activePalette.text, fontSize = 12.sp)
                                Switch(checked = nightly, onCheckedChange = { nightly = it }, colors = magicanSwitchColors(),
                                    modifier = Modifier.semantics { contentDescription = "Run nightly at 02:00 UTC" })
                            }
                        }
                    }
                }
            },
            modeControl = { ComposerRunMode(mode) { mode = it } },
            modelControl = {
                val chipLabel = "${selectedProfile?.let { catalogEngineLabel(it.engine.orEmpty(), it.credentials) } ?: "Engine"} · ${selectedModel?.label ?: "Choose model"}"
                // Web `CodingModelPicker`: the selected thinking level rides the chip.
                val thinking = ai.magicbeans.magdroid.chat.modelChipThinkingLabel(selectedModel, selection)
                ComposerSelectorChip(chipLabel, onClick = { showModels = true }, thinking = thinking,
                    accessibilityLabel = ai.magicbeans.magdroid.chat.modelChipAccessibilityLabel("VibeDev coding engine: $chipLabel", thinking))
            },
            topPanel = {
                LazyRow(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    item { Box {
                        ComposerSelectorChip(project?.name ?: "Choose project", icon = Icons.Outlined.FolderOpen, maxWidth = 96.dp,
                            onClick = if (followUp) null else ({ showProjects = true }))
                        DropdownMenu(showProjects, { showProjects = false }, containerColor = activePalette.elevated) {
                            projects.forEach { candidate -> DropdownMenuItem(text = { Text(candidate.name) }, onClick = { projectId = candidate.projectId; showProjects = false }) }
                        }
                    } }
                    item { Box {
                        val frozenLocation = if (followUp) workLocation else location
                        val locations = listOf("original" to "Original folder", "sandboxed" to "Isolated workspace", "trusted" to "Isolated branch")
                        ComposerSelectorChip(locations.firstOrNull { it.first == frozenLocation }?.second ?: "Isolated workspace",
                            icon = if (followUp) Icons.Outlined.Lock else Icons.Outlined.Storage,
                            onClick = if (followUp) null else ({ showLocations = true }))
                        DropdownMenu(showLocations, { showLocations = false }, containerColor = activePalette.elevated) {
                            locations.forEach { (value, label) -> DropdownMenuItem(text = { Text(label) }, onClick = { location = value; showLocations = false }) }
                        }
                    } }
                }
                state.error?.let { Text(it, color = activePalette.danger, fontSize = 12.sp) }
            },
        )
    }
    if (embedded) compact()
    else if (sheet.crew) VibeSheet(onDismiss, title, content = form)
    else VibeSheet(onDismiss, title, composerLayout = true,
        welcome = { VibeDevNewRunWelcome(title) { starterMode, starter ->
            mode = starterMode; prompt = starter; composerFocusRequest++
        } }) { compact() }
    if (showModels) ComposerModelPickerSheet(
        selectedEngine = selectedProfile?.engine.orEmpty(),
        engines = engines.map { CatalogChoiceOption(it.engine ?: it.id, catalogEngineLabel(it.engine ?: it.id, it.credentials)) },
        onPickEngine = { engine -> profileId = engines.firstOrNull { (it.engine ?: it.id) == engine }?.id },
        onDismiss = { showModels = false },
    ) { ComposerModelCatalogControls(models, selection) { profileId = it } }
}

@Composable
private fun ComposerRunMode(mode: String, onChoose: (String) -> Unit) {
    Surface(color = composerModeTrackColor(), shape = Lively.chipShape) {
        Row(Modifier.padding(horizontal = 2.dp), horizontalArrangement = Arrangement.spacedBy(2.dp)) {
            listOf("build", "discuss", "autopilot").forEach { value ->
                ComposerModeSegment(value.uppercase(), mode == value) { onChoose(value) }
            }
        }
    }
}

/** One line to the running agent, with the mic. */
@Composable
private fun SteerSheet(sheet: VibeDevSheet.Steer, onDismiss: () -> Unit) {
    var text by remember { mutableStateOf("") }
    val send = {
        if (text.isNotBlank()) { sheet.onSend(text); onDismiss() }
    }
    VibeSheet(onDismiss, "Steer") {
        sheet.taskId?.let { VibeDevAskDock(setOf(it)) }
        MetaText(sheet.title.removePrefix("VibeDev · "), maxLines = 2)
        PromptField(
            value = text,
            onValueChange = { text = it },
            placeholder = "Tell it what to change",
            minLines = 1,
            singleLine = true,
            onDone = send,
        )
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            VibeButton("Cancel", onDismiss, Modifier.weight(1f))
            VibeButton("Send", send, Modifier.weight(1f), kind = VibeButtonKind.Primary, enabled = text.isNotBlank())
        }
    }
}

/** Name, a repository (suggested or typed), trust and base branch. */
@Composable
private fun NewProjectSheet(composer: VibeDevComposerViewModel, onDismiss: () -> Unit) {
    val state by composer.state.collectAsStateWithLifecycle()
    LaunchedEffect(Unit) { composer.openProject() }
    var name by remember { mutableStateOf("") }
    var path by remember { mutableStateOf("") }
    var base by remember { mutableStateOf("") }
    var location by remember { mutableStateOf("sandboxed") }
    VibeSheet(onDismiss, "New project") {
        MagicianTextField(value = name, onValueChange = { name = it }, modifier = Modifier.fillMaxWidth(),
            singleLine = true, label = { Text("Name") })
        val suggestions = state.repos.filter { it.project == null }
        if (suggestions.isNotEmpty()) {
            FieldLabel("Repository")
            Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                suggestions.forEach { repo ->
                    Pill(repo.name.ifBlank { repo.path.substringAfterLast('/') }, path == repo.path, {
                        path = repo.path
                        if (name.isBlank()) name = repo.name.replaceFirstChar { it.uppercase() }
                    })
                }
            }
        }
        MagicianTextField(value = path, onValueChange = { path = it }, modifier = Modifier.fillMaxWidth(),
            singleLine = true, label = { Text("Repository path") },
            textStyle = androidx.compose.ui.text.TextStyle(color = activePalette.text, fontSize = 13.sp,
                fontFamily = LocalMagicanFontFamilies.current.mono))
        WorkLocationPicker(location) { location = it }
        if (location == "trusted") {
            MagicianTextField(value = base, onValueChange = { base = it }, modifier = Modifier.fillMaxWidth(),
                singleLine = true, label = { Text("Base branch (optional)") })
        }
        state.error?.let { Text(it, color = activePalette.danger, fontSize = 13.sp) }
        VibeButton(
            "Add project",
            onClick = { composer.createProject(name, path, location == "trusted", base, location) },
            modifier = Modifier.fillMaxWidth(),
            kind = VibeButtonKind.Primary,
            enabled = name.isNotBlank() && path.isNotBlank(),
            busy = state.submitting,
        )
    }
}

/** A prompt field with the dictation mic; dictated words append. */
@Composable
private fun PromptField(
    value: String,
    onValueChange: (String) -> Unit,
    placeholder: String,
    minLines: Int,
    singleLine: Boolean = false,
    onDone: (() -> Unit)? = null,
    compact: Boolean = false,
    onSend: (() -> Unit)? = null,
    canSend: Boolean = true,
    sending: Boolean = false,
    voiceMode: Boolean = false,
    onVoiceModeChange: (Boolean) -> Unit = {},
    heroControls: (@Composable androidx.compose.foundation.layout.RowScope.() -> Unit)? = null,
) {
    val context = LocalContext.current
    val keyboard = LocalSoftwareKeyboardController.current
    val focusRequester = remember { FocusRequester() }
    LaunchedEffect(compact, voiceMode) {
        if (compact && !voiceMode) {
            androidx.compose.runtime.withFrameNanos { }
            focusRequester.requestFocus()
            keyboard?.show()
        }
    }
    val dictation = remember { Dictation(context.applicationContext) }
    DisposableEffect(dictation) { onDispose { dictation.close() } }
    val listening by dictation.state.collectAsStateWithLifecycle()
    val partial by dictation.partial.collectAsStateWithLifecycle()
    val error by dictation.error.collectAsStateWithLifecycle()
    val latest = rememberUpdatedState(value)
    val start = {
        dictation.start { heard ->
            val current = latest.value
            onValueChange(if (current.isBlank()) heard else current.trimEnd() + " " + heard)
        }
    }
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) start()
    }
    val dictate = {
        when {
            listening == DictationState.Recording -> dictation.stop()
            listening == DictationState.Transcribing -> Unit
            ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED -> start()
            else -> permission.launch(Manifest.permission.RECORD_AUDIO)
        }
    }
    if (compact) {
        if (voiceMode && value.isEmpty()) {
            VoiceHero(state = listening, partial = partial, onMic = dictate,
                onHoldStart = dictate, onHoldEnd = { dictation.stop() },
                onKeyboard = { onVoiceModeChange(false) }, holdToSend = false, sideControls = heroControls)
        } else {
        Row(verticalAlignment = Alignment.Bottom) {
            BasicComposerField(value, placeholder, onValueChange, Modifier.weight(1f).focusRequester(focusRequester))
            ComposerKeyboardDismiss()
            val sendReady = value.isNotBlank() && listening == DictationState.Idle
            Surface(color = if (listening == DictationState.Recording) activePalette.danger else AccentFill,
                shape = RoundedCornerShape(12.dp), modifier = Modifier.padding(start = 4.dp).size(36.dp)
                    .clickable(enabled = !sending && listening != DictationState.Transcribing && (!sendReady || canSend)) {
                        if (sendReady) onSend?.invoke()
                        else if (listening == DictationState.Idle) onVoiceModeChange(true)
                        else dictate()
                    }) {
                Box(contentAlignment = Alignment.Center) {
                    when {
                        sending || listening == DictationState.Transcribing -> CircularProgressIndicator(Modifier.size(18.dp), color = OnAccent, strokeWidth = 2.dp)
                        sendReady -> Text("↑", color = OnAccent, fontSize = 17.sp, fontWeight = FontWeight.SemiBold,
                            modifier = Modifier.semantics { contentDescription = "Send" })
                        else -> Icon(if (listening == DictationState.Recording) Icons.Outlined.Stop else Icons.Filled.Mic,
                            contentDescription = if (listening == DictationState.Recording) "Stop dictation" else "Dictate", tint = OnAccent, modifier = Modifier.size(20.dp))
                    }
                }
            }
        }
        }
    } else MagicianTextField(
        value = value,
        onValueChange = onValueChange,
        modifier = Modifier.fillMaxWidth(),
        placeholder = { Text(placeholder, color = activePalette.secondaryText, fontSize = 13.sp) },
        minLines = minLines,
        singleLine = singleLine,
        keyboardOptions = if (onDone != null) KeyboardOptions(imeAction = ImeAction.Send) else KeyboardOptions.Default,
        keyboardActions = KeyboardActions(onSend = { onDone?.invoke() }),
        trailingIcon = {
            VibeIconButton(onClick = dictate) {
                Icon(
                    if (listening == DictationState.Recording) Icons.Outlined.Stop else Icons.Outlined.Mic,
                    contentDescription = if (listening == DictationState.Recording) "Stop dictation" else "Dictate",
                    tint = if (listening == DictationState.Idle) activePalette.secondaryText else activePalette.accent,
                    modifier = Modifier.size(20.dp),
                )
            }
        },
    )
    when {
        listening == DictationState.Recording && partial.isNotBlank() -> MetaText(partial, maxLines = 3)
        listening == DictationState.Recording -> MetaText("Listening…")
        listening == DictationState.Transcribing -> MetaText("Transcribing…")
        error != null -> Text(error.orEmpty(), color = activePalette.danger, fontSize = 12.sp)
    }
}

@Composable
private fun WorkLocationPicker(location: String, onChoose: (String) -> Unit) {
    FieldLabel("Work location")
    Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        listOf("original" to "Original folder", "sandboxed" to "Isolated workspace", "trusted" to "Isolated branch").forEach { (value, label) ->
            Pill(label, location == value, { onChoose(value) })
        }
    }
    MetaText("Original folder keeps your branch and uncommitted files. Exact diffs are approved before edits. Independent of Super god mode.", maxLines = 4)
}
