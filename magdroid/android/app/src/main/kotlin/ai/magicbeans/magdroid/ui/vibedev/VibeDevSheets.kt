package ai.magicbeans.magdroid.ui.vibedev

import ai.magicbeans.magdroid.ui.LocalMagicanFontFamilies
import ai.magicbeans.magdroid.ui.MagicianTextField
import ai.magicbeans.magdroid.ui.activePalette
import ai.magicbeans.magdroid.ui.magicanSwitchColors
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
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Mic
import androidx.compose.material.icons.outlined.Stop
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
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
import androidx.compose.ui.platform.LocalContext
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
                scope.launch { snackbar.showSnackbar(if (result.crew.isRace) "Race set up. Start it from Now." else "Lanes planned. Start the crew from Now.") }
                chrome.reset()
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
        is VibeDevSheet.NewRun -> NewRunSheet(sheet, nowData.openProjects, composer, onDismiss = { chrome.sheet = null })
        is VibeDevSheet.Steer -> SteerSheet(sheet, onDismiss = { chrome.sheet = null })
        VibeDevSheet.NewProject -> NewProjectSheet(composer, onDismiss = { chrome.sheet = null })
        null -> Unit
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun VibeSheet(onDismiss: () -> Unit, title: String, content: @Composable ColumnScope.() -> Unit) {
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = activePalette.elevated,
        contentColor = activePalette.text,
        scrimColor = activePalette.text.copy(alpha = 0.32f),
    ) {
        Column(
            Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).imePadding().navigationBarsPadding()
                .padding(start = 20.dp, end = 20.dp, bottom = 20.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(title, color = activePalette.text, fontSize = 18.sp, fontWeight = FontWeight.Bold,
                fontFamily = LocalMagicanFontFamilies.current.display)
            content()
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
private fun NewRunSheet(
    sheet: VibeDevSheet.NewRun,
    projects: List<VibeDevProject>,
    composer: VibeDevComposerViewModel,
    onDismiss: () -> Unit,
) {
    val state by composer.state.collectAsStateWithLifecycle()
    LaunchedEffect(sheet) { composer.open() }
    var prompt by remember { mutableStateOf("") }
    var projectId by remember { mutableStateOf(sheet.projectId ?: projects.firstOrNull()?.projectId) }
    var mode by remember { mutableStateOf("build") }
    var profileId by remember { mutableStateOf<String?>(null) }
    var budget by remember { mutableStateOf("") }
    var crew by remember { mutableStateOf(sheet.crew) }
    var race by remember { mutableStateOf(false) }
    var attempts by remember { mutableIntStateOf(3) }
    var review by remember { mutableStateOf(true) }
    val followUp = sheet.parentTaskId != null
    val project = projects.firstOrNull { it.projectId == projectId }
    val trusted = project?.isTrusted == true
    val crewOn = crew && trusted && !followUp
    LaunchedEffect(state.profiles) {
        if (profileId == null) profileId = state.profiles.firstOrNull { it.isDefault }?.id
    }

    VibeSheet(onDismiss, when {
        followUp -> "Follow up"
        crewOn -> "New VibeDev crew"
        else -> "New run"
    }) {
        sheet.parentTitle?.let { MetaText("Continues ${it.removePrefix("VibeDev · ")}", maxLines = 2) }
        PromptField(
            value = prompt,
            onValueChange = { prompt = it },
            placeholder = if (crewOn) "What should the crew build?" else if (followUp) "What next?" else "What should VibeDev do?",
            minLines = 4,
        )
        if (!followUp) {
            FieldLabel("Project")
            if (projects.isEmpty()) MetaText("No projects yet. Add one from the + on VibeDev.")
            Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                projects.forEach { candidate ->
                    Pill(candidate.name, candidate.projectId == projectId, { projectId = candidate.projectId })
                }
            }
        }
        if (!crewOn) {
            FieldLabel("Mode")
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                Pill("Build", mode == "build", { mode = "build" })
                Pill("Discuss", mode == "discuss", { mode = "discuss" })
            }
            FieldLabel("Engine")
            Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                Pill("Default", profileId == null, { profileId = null })
                state.profiles.forEach { profile ->
                    Pill(profile.label.ifBlank { profile.id }, profileId == profile.id, { profileId = profile.id })
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
        val budgetUsd = budget.toDoubleOrNull()?.takeIf { it > 0 }
        VibeButton(
            when {
                crewOn && race -> "Set up race"
                crewOn -> "Plan lanes"
                followUp -> "Send follow-up"
                else -> "Start run"
            },
            onClick = {
                val target = projectId ?: return@VibeButton
                if (crewOn) composer.proposeCrew(target, prompt, race, attempts, review, budgetUsd)
                else composer.startRun(prompt, target, mode, sheet.parentTaskId, profileId, budgetUsd)
            },
            modifier = Modifier.fillMaxWidth(),
            kind = VibeButtonKind.Primary,
            enabled = prompt.isNotBlank() && projectId != null,
            busy = state.submitting,
        )
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
    var trusted by remember { mutableStateOf(false) }
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
        SwitchRow(
            "Trusted",
            "Runs get their own branch and can run at once. Needed for crews. The folder must be a git repository.",
            trusted, { trusted = it },
        )
        if (trusted) {
            MagicianTextField(value = base, onValueChange = { base = it }, modifier = Modifier.fillMaxWidth(),
                singleLine = true, label = { Text("Base branch (optional)") })
        }
        state.error?.let { Text(it, color = activePalette.danger, fontSize = 13.sp) }
        VibeButton(
            "Add project",
            onClick = { composer.createProject(name, path, trusted, base) },
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
) {
    val context = LocalContext.current
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
    MagicianTextField(
        value = value,
        onValueChange = onValueChange,
        modifier = Modifier.fillMaxWidth(),
        placeholder = { Text(placeholder, color = activePalette.secondaryText, fontSize = 13.sp) },
        minLines = minLines,
        singleLine = singleLine,
        keyboardOptions = if (onDone != null) KeyboardOptions(imeAction = ImeAction.Send) else KeyboardOptions.Default,
        keyboardActions = KeyboardActions(onSend = { onDone?.invoke() }),
        trailingIcon = {
            IconButton(onClick = {
                when {
                    listening == DictationState.Recording -> dictation.stop()
                    listening == DictationState.Transcribing -> Unit
                    ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED -> start()
                    else -> permission.launch(Manifest.permission.RECORD_AUDIO)
                }
            }) {
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
