package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tasks.PlanClarificationEntry
import ai.magicbeans.magdroid.tasks.PlanInspectorPane
import ai.magicbeans.magdroid.tasks.PlanJson
import ai.magicbeans.magdroid.tasks.PlanLoadable
import ai.magicbeans.magdroid.tasks.PlanManualResume
import ai.magicbeans.magdroid.tasks.PlanQuestion
import ai.magicbeans.magdroid.tasks.PlanSlotSnapshot
import ai.magicbeans.magdroid.tasks.PlanStrategyAttempt
import ai.magicbeans.magdroid.tasks.PlanVersionSummary
import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowLeft
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material.icons.outlined.KeyboardArrowDown
import androidx.compose.material.icons.outlined.KeyboardArrowUp
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale

/*
 * The Plan Inspector's History view and its More panes (Analysis, Slots,
 * Attempts, Clarifications) and the plan document. Port of iOS
 * `PlanInspectorMoreViews.swift`.
 */

private val planDateFormat: DateTimeFormatter =
    DateTimeFormatter.ofPattern("d MMM yyyy 'at' h:mm a", Locale.getDefault()).withZone(ZoneId.systemDefault())

internal fun planDate(epochMs: Long): String = planDateFormat.format(Instant.ofEpochMilli(epochMs))

@Composable
private fun Loading(minHeight: Int = 120) {
    Box(Modifier.fillMaxWidth().heightIn(min = minHeight.dp), contentAlignment = Alignment.Center) {
        CircularProgressIndicator(color = activePalette.accent)
    }
}

// History

/** The latest 20 saved versions; tap one to preview it read-only, then Restore (confirmed). */
@Composable
internal fun PlanHistoryView(controller: PlanInspectorController) {
    val state = controller.read()
    LaunchedEffect(Unit) {
        if (state.versions.isEmpty() && !state.versionsLoading) controller.run { loadVersions() }
    }
    val preview = state.preview
    if (preview != null) {
        HistoryPreview(controller, preview)
        return
    }
    val palette = activePalette
    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("Saved versions", style = planBodyStyle(15.sp, FontWeight.SemiBold), modifier = Modifier.weight(1f))
            if (state.versionsLoading) CircularProgressIndicator(Modifier.size(18.dp), color = palette.accent, strokeWidth = 2.dp)
            else if (state.hasMoreVersions) Text("Latest 20 shown", style = planBodyStyle(12.sp, color = palette.secondaryText))
        }
        val error = state.versionsError
        when {
            error != null -> PlanMessage(error, isError = true)
            state.versions.isEmpty() && !state.versionsLoading ->
                PlanMessage("No saved versions yet. Saving an edit keeps the previous plan here.")
        }
        for (version in state.versions) VersionCard(version) { controller.run { previewVersion(version) } }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun VersionCard(version: PlanVersionSummary, onClick: () -> Unit) {
    PlanCard(Modifier.clickable(role = Role.Button, onClickLabel = "Preview version", onClick = onClick)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(version.title, style = planBodyStyle(14.sp, FontWeight.SemiBold), modifier = Modifier.weight(1f))
            Icon(Icons.AutoMirrored.Outlined.KeyboardArrowRight, null, tint = activePalette.secondaryText)
        }
        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            PlanTag(planDate(version.epochMs))
            PlanTag(PlanStyle.plural(version.stepCount, "step"))
            version.confidence?.let { PlanTag(PlanStyle.percent(it), mono = true) }
        }
    }
}

@Composable
private fun HistoryPreview(controller: PlanInspectorController, version: PlanVersionSummary) {
    val state = controller.read()
    val palette = activePalette
    var confirmRestore by remember { mutableStateOf(false) }
    Column(Modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(end = 16.dp), verticalAlignment = Alignment.CenterVertically) {
            Row(
                Modifier
                    .heightIn(min = Lively.tapMin)
                    .clickable(role = Role.Button) { controller.mutate { clearPreview() } }
                    .padding(horizontal = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Icon(Icons.AutoMirrored.Outlined.KeyboardArrowLeft, null, tint = palette.accent)
                Text("Versions", style = planBodyStyle(15.sp, FontWeight.Medium, palette.accent))
            }
            Spacer(Modifier.weight(1f))
            Column(horizontalAlignment = Alignment.End) {
                Text(version.title, style = planBodyStyle(13.sp, FontWeight.SemiBold), maxLines = 1, overflow = TextOverflow.Ellipsis)
                Text(planDate(version.epochMs), style = planBodyStyle(11.sp, color = palette.secondaryText))
            }
        }
        when (val loaded = state.previewGraph) {
            PlanLoadable.Idle, PlanLoadable.Loading -> Box(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
                CircularProgressIndicator(color = palette.accent)
            }
            is PlanLoadable.Failed -> {
                PlanMessage(loaded.message, isError = true, modifier = Modifier.padding(16.dp))
                Spacer(Modifier.weight(1f))
            }
            is PlanLoadable.Loaded -> {
                PlanWaterfall(loaded.data, emptyMap(), onSelect = {}, modifier = Modifier.weight(1f))
                Box(Modifier.fillMaxWidth().background(palette.background.copy(alpha = 0.92f)).padding(horizontal = 16.dp, vertical = 8.dp)) {
                    DecisionButton(
                        if (state.isRestoring) "Restoring…" else "Restore this version",
                        palette.accent, filled = true, enabled = !state.isRestoring, modifier = Modifier.fillMaxWidth(),
                    ) { confirmRestore = true }
                }
            }
        }
    }
    if (confirmRestore) ConfirmDialog(
        title = "Restore historical plan version?",
        message = "This will replace the current task plan with this historical version.",
        confirm = "Restore",
        onConfirm = { confirmRestore = false; controller.run { restorePreview() } },
        onDismiss = { confirmRestore = false },
    )
}

// More panes

@Composable
internal fun PlanPaneScreen(controller: PlanInspectorController, pane: PlanInspectorPane, onBack: () -> Unit) {
    val state = controller.read()
    LaunchedEffect(pane) {
        if (state.paneState(pane) == PlanLoadable.Idle) controller.run { loadPane(pane) }
    }
    PlanSubPage(pane.title, onBack) {
        Column(
            Modifier.fillMaxSize().imePadding().verticalScroll(rememberScrollState()).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            when (pane) {
                PlanInspectorPane.Analysis -> Loadable(state.analysis) { value ->
                    if (value != null) PlanCard { PlanJsonDisclosure("Query analysis", value, expanded = true) }
                    else PlanMessage("No query analysis was persisted for this task plan.")
                }
                PlanInspectorPane.Slots -> Loadable(state.slots) { snapshot ->
                    if (snapshot != null) SlotsView(snapshot)
                    else PlanMessage("No slot graph snapshot was persisted for this task plan.")
                }
                PlanInspectorPane.Attempts -> Loadable(state.attempts) { attempts ->
                    if (attempts.isEmpty()) PlanMessage("No strategy attempts were persisted for this task plan.")
                    for (attempt in attempts) AttemptCard(attempt)
                }
                PlanInspectorPane.Clarifications -> ClarificationsView(controller)
            }
            LivelyChip(onClick = { controller.run { loadPane(pane) } }) { Text("Reload", style = livelyChipTextStyle()) }
        }
    }
}

@Composable
private fun <T> Loadable(state: PlanLoadable<T>, content: @Composable (T) -> Unit) {
    when (state) {
        PlanLoadable.Idle, PlanLoadable.Loading -> Loading()
        is PlanLoadable.Failed -> PlanMessage(state.message, isError = true)
        is PlanLoadable.Loaded -> content(state.data)
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun AttemptCard(attempt: PlanStrategyAttempt) {
    val palette = activePalette
    PlanCard {
        Text(attempt.title, style = planBodyStyle(14.sp, FontWeight.SemiBold))
        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            PlanTag("Confidence ${PlanStyle.percent(attempt.confidence)}", mono = true)
            val failed = attempt.failureReason != null
            PlanTag(if (failed) "Failed" else "Completed", if (failed) palette.danger else palette.success)
            attempt.attemptedAtMillis?.let { PlanTag(planDate(it)) }
        }
        attempt.failureReason?.let { PlanMessage(it, isError = true) }
        attempt.resourcesUsed?.let { PlanJsonDisclosure("Resources used", it) }
        attempt.explorationResult?.let { PlanJsonDisclosure("Exploration result", it, expanded = attempt.index == 0) }
    }
}

/** Web `getConfidenceClass`: high ≥ 0.8, medium ≥ 0.5. */
private fun slotColor(value: Double, palette: Palette = activePalette): Color = when {
    value >= 0.8 -> palette.success
    value >= 0.5 -> palette.warning
    else -> palette.danger
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun SlotsView(snapshot: PlanSlotSnapshot) {
    val palette = activePalette
    Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            PlanTag(PlanStyle.plural(snapshot.slots.size, "slot"))
            snapshot.overallConfidence?.let { PlanTag("Overall ${PlanStyle.percent(it)}", mono = true) }
            if (snapshot.unresolvedSlots.isNotEmpty()) PlanTag("${snapshot.unresolvedSlots.size} unresolved", palette.warning)
        }
        if (snapshot.slots.isEmpty()) PlanMessage("No slot records were captured for this task plan.")
        for (group in snapshot.groups) {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text(group.type.replaceFirstChar { it.uppercase() }, style = planBodyStyle(14.sp, FontWeight.SemiBold))
                    Text("${group.slots.size}", style = planMonoStyle(12.sp, color = palette.secondaryText))
                    Spacer(Modifier.weight(1f))
                    PlanTag("avg ${PlanStyle.percent(group.averageConfidence)}", slotColor(group.averageConfidence), mono = true)
                }
                for (slot in group.slots) {
                    PlanCard {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text(slot.shortId, style = planMonoStyle(13.sp, FontWeight.SemiBold), modifier = Modifier.weight(1f))
                            PlanTag(PlanStyle.percent(slot.confidence), slotColor(slot.confidence), mono = true)
                        }
                        val empty = slot.value is JsonNull
                        Text(
                            if (empty) "No value yet" else PlanJson.displayText(slot.value),
                            style = planBodyStyle(13.sp, color = if (empty) palette.secondaryText else palette.text),
                            maxLines = 6, overflow = TextOverflow.Ellipsis,
                        )
                        if (slot.sources.isNotEmpty()) {
                            Text(slot.sources.joinToString(" · "), style = planBodyStyle(11.sp, color = palette.secondaryText))
                        }
                    }
                }
            }
        }
        snapshot.clarifiedTask?.let { PlanCard { PlanJsonDisclosure("Clarified task", it) } }
        snapshot.triggerMappings?.let { PlanCard { PlanJsonDisclosure("Slot trigger mappings", it) } }
        snapshot.boostResults?.let { PlanCard { PlanJsonDisclosure("Confidence boost results", it) } }
        snapshot.enrichmentSummary?.let { PlanCard { PlanJsonDisclosure("Enrichment summary", it) } }
    }
}

// Clarifications

/** Pending questions (option chips or typed answer), the answered history, and Advanced → manual resume. */
@Composable
private fun ClarificationsView(controller: PlanInspectorController) {
    val state = controller.read()
    val palette = activePalette
    Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("Pending questions", style = planBodyStyle(15.sp, FontWeight.SemiBold), modifier = Modifier.weight(1f))
            PlanTag("${state.pendingQuestions.size} pending", if (state.pendingQuestions.isEmpty()) palette.success else palette.warning)
        }
        if (state.pendingQuestions.isEmpty()) PlanMessage("There are no pending planning questions right now.")
        for (question in state.pendingQuestions) QuestionCard(controller, question)

        Row(Modifier.padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            Text("Answered", style = planBodyStyle(15.sp, FontWeight.SemiBold), modifier = Modifier.weight(1f))
            state.clarifications.value?.let { PlanTag("${it.size} answered") }
        }
        when (val history = state.clarifications) {
            PlanLoadable.Idle, PlanLoadable.Loading -> Loading(60)
            is PlanLoadable.Failed -> PlanMessage(history.message, isError = true)
            is PlanLoadable.Loaded -> {
                if (history.data.isEmpty()) PlanMessage("No clarification history has been recorded yet.")
                for (entry in history.data) AnsweredCard(entry)
            }
        }
        if (state.canResume) ManualResume(controller)
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun QuestionCard(controller: PlanInspectorController, question: PlanQuestion) {
    val state = controller.read()
    val palette = activePalette
    val busy = question.id in state.answering
    val text = state.answers[question.id].orEmpty()
    PlanCard {
        Text(question.text, style = planBodyStyle(15.sp, FontWeight.SemiBold))
        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            question.sourceSlotId?.let { PlanTag(it, mono = true) }
            question.urgency?.let { PlanTag("Urgency ${PlanStyle.percent(it)}", palette.warning) }
        }
        if (question.options.isNotEmpty()) {
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                for (option in question.options) {
                    LivelyChip(onClick = { controller.run { answer(question, option.value) } }, enabled = !busy) {
                        Text(option.label, style = livelyChipTextStyle())
                    }
                }
            }
        }
        MagicianTextField(
            value = text,
            onValueChange = { value -> controller.mutate { answers[question.id] = value } },
            placeholder = { Text("Type your answer…") }, enabled = !busy, maxLines = 4,
            modifier = Modifier.fillMaxWidth(),
        )
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box(Modifier.weight(1f)) {
                state.answerErrors[question.id]?.let { Text(it, style = planBodyStyle(12.sp, color = palette.danger)) }
            }
            LivelyChip(
                onClick = { controller.run { answer(question) } },
                prominent = true,
                enabled = !busy && text.isNotBlank(),
            ) { Text(if (busy) "Sending…" else "Submit", style = livelyChipTextStyle(prominent = true)) }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun AnsweredCard(entry: PlanClarificationEntry) {
    PlanCard {
        Text(entry.questionText, style = planBodyStyle(14.sp, FontWeight.SemiBold))
        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            PlanTag(entry.stageLabel)
            entry.answeredAtMillis?.let { PlanTag(planDate(it)) }
            PlanTag(PlanStyle.plural(entry.slotUpdates.size, "slot update"))
        }
        entry.sourceSlotId?.let { Text("Source slot: $it", style = planBodyStyle(12.sp, color = activePalette.secondaryText)) }
        if (entry.slotUpdates.isNotEmpty()) PlanJsonDisclosure("Slot updates", JsonArray(entry.slotUpdates))
    }
}

@Composable
private fun ManualResume(controller: PlanInspectorController) {
    val state = controller.read()
    val palette = activePalette
    var open by remember { mutableStateOf(false) }
    val form = state.resumeForm
    fun edit(change: (PlanManualResume) -> PlanManualResume) = controller.mutate { resumeForm = change(resumeForm) }
    val motion = rememberAnimatorDurationScale() > 0f
    PlanCard(if (motion) Modifier.animateContentSize() else Modifier) {
        Row(
            Modifier.fillMaxWidth().heightIn(min = Lively.tapMin).clickable { open = !open },
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text("Advanced · manual resume", style = planBodyStyle(14.sp, FontWeight.SemiBold))
            state.status?.let { PlanTag(it.label, palette.warning) }
            Spacer(Modifier.weight(1f))
            Icon(if (open) Icons.Outlined.KeyboardArrowUp else Icons.Outlined.KeyboardArrowDown, null, tint = palette.accent)
        }
        if (open) {
            Text(
                "Resume planning when you already know the missing slot values, without answering through the question flow.",
                style = planBodyStyle(12.sp, color = palette.secondaryText),
            )
            MagicianTextField(
                value = form.handledQuestion, onValueChange = { v -> edit { it.copy(handledQuestion = v) } },
                label = { Text("Handled question id (optional)") }, singleLine = true,
                keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false),
                modifier = Modifier.fillMaxWidth(),
            )
            MagicianTextField(
                value = form.updatedConfidence, onValueChange = { v -> edit { it.copy(updatedConfidence = v) } },
                label = { Text("Updated confidence 0–1 (optional)") }, singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal),
                modifier = Modifier.fillMaxWidth(),
            )
            MagicianTextField(
                value = form.slotsJson, onValueChange = { v -> edit { it.copy(slotsJson = v) } },
                label = { Text("Slots JSON") },
                placeholder = { Text("""[{"slot_id":"contact","slot_type":"entity","value":"Jordan"}]""") },
                textStyle = planMonoStyle(12.sp), minLines = 3, maxLines = 8,
                keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false),
                modifier = Modifier.fillMaxWidth(),
            )
            state.resumeError?.let { Text(it, style = planBodyStyle(12.sp, color = palette.danger)) }
            LivelyChip(onClick = { controller.run { submitResume() } }, prominent = true, enabled = !state.isResuming) {
                Text(if (state.isResuming) "Resuming…" else "Resume planning", style = livelyChipTextStyle(prominent = true))
            }
        }
    }
}

// Plan document

@Composable
internal fun PlanDocumentScreen(markdown: String, onBack: () -> Unit) {
    PlanSubPage("Plan document", onBack) {
        Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp)) {
            MarkdownText(markdown, color = activePalette.text, fontSize = 15.sp, lineHeight = 22.sp)
        }
    }
}

