package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.net.Failure
import ai.magicbeans.magdroid.net.FailureKind
import ai.magicbeans.magdroid.net.Failures
import ai.magicbeans.magdroid.preferences.OwnerPreference
import ai.magicbeans.magdroid.preferences.PreferenceEditOutcome
import ai.magicbeans.magdroid.preferences.PreferenceHistoryEntry
import ai.magicbeans.magdroid.preferences.PreferenceKind
import ai.magicbeans.magdroid.preferences.PreferenceView
import ai.magicbeans.magdroid.preferences.PreferencesError
import ai.magicbeans.magdroid.preferences.PreferencesRepository
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.FilterChip
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.launch
import java.text.DateFormat
import java.util.Date

/**
 * Settings → Your preferences: the owner's preferences exactly as preference
 * checks see them — the Android port of the web's Memory → Preferences tab and
 * iOS's `OwnerPreferencesView`.
 *
 * Add, reword, confirm an inferred one as the owner's own, retire, or forget
 * (wording erased from the history, a dated entry kept). Every change but add
 * names the revision this screen showed: if the preference changed elsewhere
 * since, nothing is saved and the screen reloads with a note.
 */
@Composable
fun PreferencesSettingsPage() {
    val context = LocalContext.current
    val repository = remember(context) { PreferencesRepository(context) }
    DisposableEffect(repository) { onDispose { repository.close() } }
    val scope = rememberCoroutineScope()

    var view by remember { mutableStateOf<PreferenceView?>(null) }
    var failure by remember { mutableStateOf<Failure?>(null) }
    var notice by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }
    var reloadKey by remember { mutableStateOf(0) }
    var adding by remember { mutableStateOf(false) }
    var editing by remember { mutableStateOf<OwnerPreference?>(null) }
    var forgetting by remember { mutableStateOf<OwnerPreference?>(null) }
    var showRetired by remember { mutableStateOf(false) }
    val histories = remember { mutableStateMapOf<String, List<PreferenceHistoryEntry>?>() }

    LaunchedEffect(reloadKey) {
        failure = null
        runCatching { repository.view() }
            .onSuccess { view = it }
            .onFailure { cause ->
                failure = if (cause is PreferencesError) {
                    // A refusal the server worded itself: the sentence is all of it.
                    Failure(FailureKind.Unknown, cause.message ?: "Your preferences are unavailable.", "", retryable = true)
                } else {
                    Failures.of(cause, "your preferences")
                }
            }
    }

    val loadFailure = failure
    if (view == null && loadFailure != null) {
        FailurePane(loadFailure, onRetry = { reloadKey++ })
        return
    }

    fun change(action: suspend () -> PreferenceEditOutcome, done: String) {
        busy = true
        scope.launch {
            runCatching { action() }
                .onSuccess { outcome ->
                    notice = when (outcome) {
                        is PreferenceEditOutcome.Applied -> done
                        is PreferenceEditOutcome.Stale -> outcome.message
                    }
                    histories.clear()
                    reloadKey++
                }
                .onFailure { notice = it.message ?: "The change was not saved." }
            busy = false
        }
    }

    LazyColumn(
        Modifier.fillMaxSize().padding(horizontal = 16.dp).testTag("preferences-page"),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        item { Spacer(Modifier.height(2.dp)) }
        item {
            Text(
                "What Magician checks your actions against before it sends, books or buys. Change them here, in chat, or on your other devices.",
                color = Muted, fontSize = 12.sp, lineHeight = 16.sp,
            )
        }
        item {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = { adding = true },
                    enabled = !busy && view != null,
                    shape = MagicanButtonShape,
                    colors = ButtonDefaults.buttonColors(containerColor = AccentFill),
                ) { Text("Add a preference") }
                OutlinedButton(
                    onClick = { histories.clear(); reloadKey++ },
                    enabled = !busy,
                    shape = MagicanButtonShape,
                    border = BorderStroke(1.dp, ControlBorder),
                ) { Text("Refresh", color = Ink) }
            }
        }
        notice?.let { item { Text(it, color = Ink, fontSize = 13.sp, modifier = Modifier.testTag("preferences-notice")) } }
        // A refresh that fails keeps the list on screen and says so above it.
        loadFailure?.let { item { InlineFailure(it, onRetry = { reloadKey++ }) } }
        val current = view
        if (current == null) item { Text("Loading your preferences…", color = Muted, fontSize = 13.sp) }
        if (current != null) {
            if (current.preferences.isEmpty()) {
                item { Text("No preferences yet. Tell Magician in chat, or add one here.", color = Muted, fontSize = 13.sp) }
            }
            if (current.coverage != "complete" || current.omitted > 0) {
                item {
                    Text(
                        "Some memory could not be read just now, so this list may be incomplete.",
                        color = Muted, fontSize = 12.sp, fontStyle = FontStyle.Italic,
                    )
                }
            }
            // Grouped as web and iOS group them: rules, goals, tastes.
            PREFERENCE_GROUPS.forEach { (kind, title) ->
                val group = current.preferences.filter { it.kind == kind }
                if (group.isEmpty()) return@forEach
                item(key = "group-$title") {
                    Text(
                        title,
                        color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold,
                        modifier = Modifier.padding(top = 6.dp),
                    )
                }
                items(group, key = { it.id }) { preference ->
                PreferenceCard(
                    preference = preference,
                    busy = busy,
                    history = histories[preference.id],
                    historyOpen = histories.containsKey(preference.id),
                    onEdit = { editing = preference },
                    onConfirm = { change({ repository.confirm(preference.id, preference.revision) }, "Confirmed as your own.") },
                    onRetire = { change({ repository.retire(preference.id, preference.revision) }, "Retired. It no longer governs checks.") },
                    onForget = { forgetting = preference },
                    onHistory = {
                        if (histories.containsKey(preference.id)) {
                            histories.remove(preference.id)
                        } else {
                            histories[preference.id] = null
                            scope.launch {
                                histories[preference.id] = runCatching { repository.history(preference.id) }.getOrDefault(emptyList())
                            }
                        }
                    },
                )
                }
            }
            if (current.retired.isNotEmpty()) {
                item {
                    TextButton(onClick = { showRetired = !showRetired }) {
                        Text(
                            if (showRetired) "Hide retired (${current.retired.size})" else "Show retired (${current.retired.size})",
                            color = Muted, fontSize = 13.sp,
                        )
                    }
                }
                if (showRetired) {
                    items(current.retired, key = { "retired-${it.id}" }) { retired ->
                        Text(
                            "${retired.statement}  ·  ${if (retired.supersededBy != null) "replaced" else retired.lifecycle}",
                            color = Muted, fontSize = 13.sp, modifier = Modifier.padding(start = 4.dp),
                        )
                    }
                }
            }
        }
        item { Spacer(Modifier.height(24.dp)) }
    }

    if (adding) {
        AddPreferenceDialog(
            onDismiss = { adding = false },
            onAdd = { kind, wording, conditions ->
                adding = false
                change({ repository.add(kind, wording, conditions) }, "Added.")
            },
        )
    }
    editing?.let { preference ->
        RewordDialog(
            preference = preference,
            onDismiss = { editing = null },
            onSave = { wording ->
                editing = null
                change({ repository.edit(preference.id, preference.revision, wording) }, "Reworded.")
            },
        )
    }
    forgetting?.let { preference ->
        AlertDialog(
            onDismissRequest = { forgetting = null },
            title = { Text("Forget this preference?") },
            text = {
                Text(
                    "“${preference.statement}” is deleted, and its wording is erased from the history. A dated entry stays so the history has no gaps.",
                    fontSize = 14.sp,
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    forgetting = null
                    change({ repository.forget(preference.id, preference.revision) }, "Forgotten.")
                }) { Text("Forget", color = Danger) }
            },
            dismissButton = { TextButton(onClick = { forgetting = null }) { Text("Keep it") } },
        )
    }
}

private val PREFERENCE_GROUPS = listOf(
    PreferenceKind.StandingInstruction to "Rules you set",
    PreferenceKind.Goal to "Goals",
    PreferenceKind.Taste to "Tastes",
)

/** A compact action, so a card's actions fit on one or two even rows. */
@Composable
private fun CardAction(label: String, enabled: Boolean, color: androidx.compose.ui.graphics.Color = Coral, onClick: () -> Unit) {
    TextButton(
        onClick = onClick,
        enabled = enabled,
        contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 8.dp, vertical = 4.dp),
        modifier = Modifier.height(36.dp),
    ) { Text(label, color = color, fontSize = 13.sp) }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun PreferenceCard(
    preference: OwnerPreference,
    busy: Boolean,
    history: List<PreferenceHistoryEntry>?,
    historyOpen: Boolean,
    onEdit: () -> Unit,
    onConfirm: () -> Unit,
    onRetire: () -> Unit,
    onForget: () -> Unit,
    onHistory: () -> Unit,
) {
    Surface(
        color = Control,
        shape = RoundedCornerShape(8.dp),
        border = BorderStroke(1.dp, BorderSoft),
        modifier = Modifier.fillMaxWidth().testTag("preference-row"),
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            val presented = preference.presented
            if (presented != null) {
                Text(presented.title, color = Ink, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
                presented.fields.forEach { (label, value) ->
                    // A fixed label column, so values line up as on the web.
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text(label, color = Muted, fontSize = 12.sp, modifier = Modifier.width(104.dp))
                        Text(value, color = Ink, fontSize = 12.sp, modifier = Modifier.weight(1f))
                    }
                }
            } else {
                Text(preference.statement, color = Ink, fontSize = 15.sp)
            }
            preference.conditions?.let { Text("When: $it", color = Muted, fontSize = 12.sp) }
            Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                Text(
                    listOf(preference.kind.label, preference.statusLabel, preference.source).joinToString("  ·  "),
                    color = Secondary, fontSize = 11.sp, fontWeight = FontWeight.Medium,
                    modifier = Modifier.weight(1f),
                )
                // History sits with the facts about the preference, so the
                // edit actions below fit one row on a phone.
                CardAction(if (historyOpen) "Hide history" else "History", true, color = Muted, onClick = onHistory)
            }
            if (preference.unresolved) {
                Text(
                    "Two of your statements disagree. Magician will ask which one holds before acting on it.",
                    color = Coral, fontSize = 12.sp,
                )
            }
            preference.lastCheckedAtMs?.let {
                Text("Last used in a check ${DateFormat.getDateInstance(DateFormat.MEDIUM).format(Date(it))}", color = Muted, fontSize = 11.sp)
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(2.dp), modifier = Modifier.padding(top = 2.dp)) {
                if (preference.editable) {
                    CardAction("Reword", !busy, onClick = onEdit)
                    if (preference.confirmable) CardAction("It's mine", !busy, onClick = onConfirm)
                    CardAction("Retire", !busy, onClick = onRetire)
                    CardAction("Forget", !busy, color = Danger, onClick = onForget)
                }
            }
            if (historyOpen) {
                when {
                    history == null -> Text("Loading history…", color = Muted, fontSize = 12.sp)
                    history.isEmpty() -> Text("No recorded changes yet.", color = Muted, fontSize = 12.sp)
                    else -> Column(verticalArrangement = Arrangement.spacedBy(4.dp), modifier = Modifier.testTag("preference-history")) {
                        history.forEach { entry -> HistoryLine(entry) }
                    }
                }
            }
        }
    }
}

@Composable
private fun HistoryLine(entry: PreferenceHistoryEntry) {
    val whenText = DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT).format(Date(entry.atMs))
    Column {
        Text("$whenText  ·  ${entry.opLabel} by ${entry.whoLabel}", color = Muted, fontSize = 11.sp)
        when {
            entry.wordingErased -> Text("wording erased", color = Muted, fontSize = 12.sp, fontStyle = FontStyle.Italic)
            entry.wordingBefore != null && entry.wordingAfter != null && entry.wordingBefore != entry.wordingAfter ->
                Text("${entry.wordingBefore} → ${entry.wordingAfter}", color = Ink, fontSize = 12.sp)
            entry.wordingAfter != null -> Text(entry.wordingAfter!!, color = Ink, fontSize = 12.sp)
        }
    }
}

@Composable
private fun AddPreferenceDialog(onDismiss: () -> Unit, onAdd: (PreferenceKind, String, String?) -> Unit) {
    var kind by remember { mutableStateOf(PreferenceKind.StandingInstruction) }
    var wording by remember { mutableStateOf("") }
    var conditions by remember { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Add a preference") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    PreferenceKind.entries.forEach { option ->
                        FilterChip(selected = kind == option, onClick = { kind = option }, label = { Text(option.label) })
                    }
                }
                Text(
                    when (kind) {
                        PreferenceKind.StandingInstruction -> "A rule Magician checks with you about before acting: “Check with me before …”."
                        PreferenceKind.Goal -> "Something you're working toward. Magician mentions it, it doesn't hold you to it."
                        PreferenceKind.Taste -> "Something you like or dislike. Magician offers a choice when an act goes against it."
                    },
                    color = Muted, fontSize = 12.sp,
                )
                OutlinedTextField(value = wording, onValueChange = { wording = it }, label = { Text("In your words") }, modifier = Modifier.fillMaxWidth())
                OutlinedTextField(value = conditions, onValueChange = { conditions = it }, label = { Text("Only when (optional)") }, modifier = Modifier.fillMaxWidth())
            }
        },
        confirmButton = {
            TextButton(onClick = { onAdd(kind, wording, conditions.ifBlank { null }) }, enabled = wording.isNotBlank()) {
                Text("Add", color = Coral)
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

@Composable
private fun RewordDialog(preference: OwnerPreference, onDismiss: () -> Unit, onSave: (String) -> Unit) {
    var wording by remember(preference.id) { mutableStateOf(preference.statement) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Reword") },
        text = { OutlinedTextField(value = wording, onValueChange = { wording = it }, modifier = Modifier.fillMaxWidth()) },
        confirmButton = {
            TextButton(onClick = { onSave(wording) }, enabled = wording.isNotBlank() && wording.trim() != preference.statement) {
                Text("Save", color = Coral)
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}
