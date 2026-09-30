package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.keyboard.KeyboardLane
import ai.magicbeans.magdroid.keyboard.KeyboardLanguage
import ai.magicbeans.magdroid.keyboard.KeyboardSkill
import ai.magicbeans.magdroid.keyboard.MagicanKeyboardService
import ai.magicbeans.magdroid.keyboard.MagicanKeyboardStore
import android.content.Context
import android.content.Intent
import android.provider.Settings
import android.view.inputmethod.InputMethodManager
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.AddCircleOutline
import androidx.compose.material.icons.outlined.Android
import androidx.compose.material.icons.outlined.ArrowDownward
import androidx.compose.material.icons.outlined.ArrowUpward
import androidx.compose.material.icons.outlined.CheckCircleOutline
import androidx.compose.material.icons.outlined.DeleteOutline
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.Info
import androidx.compose.material.icons.outlined.Keyboard
import androidx.compose.material.icons.automirrored.outlined.OpenInNew
import androidx.compose.material.icons.outlined.RadioButtonUnchecked
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.SmartToy
import androidx.compose.material.icons.outlined.TipsAndUpdates
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner

enum class SettingsPage(val title: String) {
    Root("Settings"),
    Connection("Connection"),
    HowToUse("How to Use"),
    Roadmap("Features & Roadmap"),
    Keyboard("Magican Keyboard"),
    ProtectedApps("Protected apps"),
}

data class AndroidUsageTip(val group: String, val title: String, val detail: String)

val androidUsageTips = listOf(
    AndroidUsageTip("Connect", "Connect to your Magician",
        "On the computer, open Magican Desktop Settings → Devices → Connect Android. On this phone, open Settings → Connection, choose the same route, scan the QR, and confirm the hostname. This connects chat, tasks, notes, and voice; the customer endpoint is not built into the app."),
    AndroidUsageTip("Talk anywhere", "Make Magician your digital assistant",
        "Open App Pilot → Default assistant. The system assistant gesture can then ask about the current screen, explain it with Tutor, or act through App Pilot."),
    AndroidUsageTip("Talk anywhere", "Listen for “magician”",
        "Turn on Talk to Magician in Settings. A permanent notification shows while the local wake-word listener is active and stops it in one tap."),
    AndroidUsageTip("Chat", "Send, plan, or mention",
        "Type and send normally, choose Plan when you want a plan first, and type @ to address an agent, tool, personality, or Tutor."),
    AndroidUsageTip("Chat", "Use images and files",
        "Attach images, documents, audio, or video from the composer. Share an image to Magican to open Tutor directly on that screen."),
    AndroidUsageTip("Tasks", "Work, monitors, and internal runs",
        "Tasks has the same three server-backed lanes as web and iOS. Filters, pagination, realtime refresh, control actions, task editing, and monitor history all remain authoritative on the backend."),
    AndroidUsageTip("Today", "See and act on what matters now",
        "Today combines Needs You, follow-ups, active work, deliveries, changes, resurfaced context, briefings, and activity. Swipe supported cards for their native actions."),
    AndroidUsageTip("Today", "Open native briefings",
        "Published GAUI/MUIJ dashboards render as native cards, metrics, tables, charts, Markdown, feeds, and trees instead of raw JSON."),
    AndroidUsageTip("At a glance", "Add the Magican Home Screen widget",
        "Long-press the Home Screen, choose Widgets, then add Magican at a glance. It opens Attention when something needs you, the exact task while work is active, and Talk when the day is clear. Remote updates use the same paired-device connection; without Firebase deployment setup the last truthful snapshot still refreshes periodically."),
    AndroidUsageTip("Observe", "Capture the room",
        "Start an observation to record and transcribe the room into a meeting thread. The foreground notification makes an active capture visible and stoppable."),
    AndroidUsageTip("Voice", "Dictate and hear the reply",
        "Tap or hold the microphone. Dictation fills the composer, voice-originated turns can speak their answer, and Settings controls the device-local voice behavior."),
    AndroidUsageTip("Magican Keyboard", "Write, Ask, and Act from any field",
        "Enable Magican Keyboard, switch with the globe key, then use a skill chip. Write and Ask preview their result before insertion; Act asks for confirmation before creating a task."),
    AndroidUsageTip("Magican Keyboard", "Secure fields stay private",
        "Agent skills are removed in password, OTP, and numeric fields before text is read. Ordinary key presses remain local."),
    AndroidUsageTip("App Pilot", "Let Magician operate Android apps",
        "In Magican Desktop Settings → Android App Observation, begin and review an attested enrollment, then scan that separate QR from App Pilot. Grant Accessibility there too; screenshots and taps require the hardware-backed automation identity rather than the ordinary mobile connection."),
    AndroidUsageTip("Attention", "Answer what needs you",
        "Attention carries approvals, questions, choices, and failures. Its badge and responses share the same scoped backend state as chat, Today, web, and iOS."),
)

data class AndroidRoadmapFeature(val name: String, val phase: String)

/** Pending-only. Shipped capabilities are documented in How to Use. */
val androidRoadmap = listOf(
    AndroidRoadmapFeature(
        "Screenshot + spoken request from Assistant / Quick Settings",
        "System entry points",
    ),
    AndroidRoadmapFeature("Inline visual confirmations in the Android assistant", "Assistant"),
    AndroidRoadmapFeature(
        "Agent-assisted negotiation from messaging apps",
        "Future experiences",
    ),
    AndroidRoadmapFeature("Camera or sketch handoff to VibeDev", "Future experiences"),
)

@Composable
fun HowToUseSettingsPage() {
    val groups = remember { androidUsageTips.groupBy(AndroidUsageTip::group).toList() }
    LazyColumn(
        Modifier.fillMaxSize().padding(horizontal = 16.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        item { Spacer(Modifier.height(2.dp)) }
        groups.forEach { (group, tips) ->
            item { Text(group, color = Secondary, fontSize = 12.sp, fontWeight = FontWeight.SemiBold) }
            item {
                Surface(color = Control, shape = RoundedCornerShape(8.dp), border = BorderStroke(1.dp, BorderSoft)) {
                    Column {
                        tips.forEachIndexed { index, tip ->
                            SettingsTipRow(tip)
                            if (index < tips.lastIndex) HorizontalDivider(color = BorderSoft, modifier = Modifier.padding(start = 48.dp))
                        }
                    }
                }
            }
        }
        item { Spacer(Modifier.height(18.dp)) }
    }
}

@Composable
private fun SettingsTipRow(tip: AndroidUsageTip) {
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 12.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.Top,
    ) {
        Icon(Icons.Outlined.TipsAndUpdates, null, tint = Coral, modifier = Modifier.size(20.dp))
        Column(verticalArrangement = Arrangement.spacedBy(3.dp)) {
            Text(tip.title, color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
            Text(tip.detail, color = Muted, fontSize = 12.sp, lineHeight = 16.sp)
        }
    }
}

@Composable
fun RoadmapSettingsPage() {
    LazyColumn(
        Modifier.fillMaxSize().padding(horizontal = 16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item {
            Column(
                Modifier.fillMaxWidth().padding(vertical = 16.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(7.dp),
            ) {
                Icon(Icons.Outlined.SmartToy, null, tint = Coral, modifier = Modifier.size(38.dp))
                Text("Coming to Magican on Android", color = Ink, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
                Text("Only features that have not shipped yet", color = Muted, fontSize = 12.sp)
            }
        }
        items(androidRoadmap) { feature ->
            Surface(color = Control, shape = RoundedCornerShape(8.dp), border = BorderStroke(1.dp, BorderSoft)) {
                Row(
                    Modifier.fillMaxWidth().padding(13.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(
                        Icons.Outlined.RadioButtonUnchecked,
                        null,
                        tint = Muted,
                    )
                    Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                        Text(feature.name, color = Ink, fontSize = 14.sp)
                        Text(feature.phase, color = Muted, fontSize = 11.sp)
                    }
                }
            }
        }
        item { Spacer(Modifier.height(18.dp)) }
    }
}

@Composable
fun MagicanKeyboardSettingsPage() {
    val context = LocalContext.current
    val status = rememberKeyboardStatus()
    var skills by remember { mutableStateOf(MagicanKeyboardStore.loadSkills(context)) }
    var editing by remember { mutableStateOf<KeyboardSkill?>(null) }
    var adding by remember { mutableStateOf(false) }
    var tutorialText by remember { mutableStateOf("") }
    var language by remember { mutableStateOf(MagicanKeyboardStore.language(context)) }
    var learnedCount by remember { mutableStateOf(MagicanKeyboardStore.learnedWordCount(context)) }

    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(18.dp),
    ) {
        KeyboardSection("Status") {
            KeyboardStatusRow(status.enabled, "Keyboard enabled", "Not enabled yet")
            HorizontalDivider(color = BorderSoft, modifier = Modifier.padding(start = 12.dp))
            KeyboardStatusRow(status.selected, "Magican is selected", "Another keyboard is selected")
            HorizontalDivider(color = BorderSoft, modifier = Modifier.padding(start = 12.dp))
            KeyboardActionRow(Icons.AutoMirrored.Outlined.OpenInNew, "Open keyboard settings") {
                context.startActivity(Intent(Settings.ACTION_INPUT_METHOD_SETTINGS).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
            }
            if (status.enabled) {
                HorizontalDivider(color = BorderSoft, modifier = Modifier.padding(start = 12.dp))
                KeyboardActionRow(Icons.Outlined.Keyboard, "Choose keyboard") {
                    (context.getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager)
                        .showInputMethodPicker()
                }
            }
        }

        Text(
            "Enable Magican in Android's keyboard list, then switch with the globe key. The keyboard provides ordinary typing plus explicit Write, Ask, and confirmed Act skills.",
            color = Muted, fontSize = 11.sp, lineHeight = 15.sp,
        )

        KeyboardSection("Try it") {
            Column(
                Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 12.dp),
                verticalArrangement = Arrangement.spacedBy(5.dp),
            ) {
                Text("Learn by doing", color = Ink, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
                Text(
                    "Tap the box, switch to Magican with the globe key, and follow these steps on the real keyboard.",
                    color = Muted,
                    fontSize = 11.sp,
                    lineHeight = 15.sp,
                )
            }
            HorizontalDivider(color = BorderSoft, modifier = Modifier.padding(start = 12.dp))
            MagicianTextField(
                value = tutorialText,
                onValueChange = { tutorialText = it },
                label = { Text("Type here to open the keyboard") },
                minLines = 3,
                modifier = Modifier.fillMaxWidth().padding(12.dp),
            )
            HorizontalDivider(color = BorderSoft, modifier = Modifier.padding(start = 12.dp))
            KeyboardTutorialStep(1, "Type a few words in the field above.")
            KeyboardTutorialStep(2, "Tap ✦ Magican in the action strip.")
            KeyboardTutorialStep(3, "Choose a Write or Ask skill and review its result.")
            KeyboardTutorialStep(4, "Try the Add task skill; Act always confirms before running.")
        }

        KeyboardSection("Language") {
            KeyboardLanguage.entries.forEachIndexed { index, option ->
                KeyboardLanguageRow(option, selected = language == option) {
                    language = option
                    MagicanKeyboardStore.setLanguage(context, option)
                }
                if (index < KeyboardLanguage.entries.lastIndex) {
                    HorizontalDivider(color = BorderSoft, modifier = Modifier.padding(start = 12.dp))
                }
            }
        }

        KeyboardSection("How to use Magican") {
            KeyboardUsageRow("Open Magican actions", "Tap ✦ Magican in the action strip above the keys.")
            KeyboardUsageRow("Rewrite", "Type or select text, choose a Write skill, then review before Replace.")
            KeyboardUsageRow("Ask", "Choose an Ask skill, wait for the answer, then tap Insert to add it.")
            KeyboardUsageRow("Run an action", "Choose an Act skill such as Add task and approve its confirmation card.")
            KeyboardUsageRow("Secure fields", "Password, OTP, and numeric fields expose no agent actions or field text.")
        }

        Text("Skills", color = Secondary, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
        Surface(color = Control, shape = RoundedCornerShape(8.dp), border = BorderStroke(1.dp, BorderSoft)) {
            Column {
                skills.forEachIndexed { index, skill ->
                    KeyboardSkillRow(
                        skill = skill,
                        canMoveUp = index > 0,
                        canMoveDown = index < skills.lastIndex,
                        canDelete = skills.size > 1,
                        onEdit = { editing = skill },
                        onMoveUp = {
                            skills = skills.toMutableList().also { list ->
                                val row = list.removeAt(index); list.add(index - 1, row)
                            }
                            MagicanKeyboardStore.saveSkills(context, skills)
                        },
                        onMoveDown = {
                            skills = skills.toMutableList().also { list ->
                                val row = list.removeAt(index); list.add(index + 1, row)
                            }
                            MagicanKeyboardStore.saveSkills(context, skills)
                        },
                        onDelete = {
                            skills = skills.filterNot { it.id == skill.id }
                            MagicanKeyboardStore.saveSkills(context, skills)
                        },
                    )
                    HorizontalDivider(color = BorderSoft, modifier = Modifier.padding(start = 12.dp))
                }
                KeyboardActionRow(Icons.Outlined.AddCircleOutline, "Add skill") { adding = true }
                HorizontalDivider(color = BorderSoft, modifier = Modifier.padding(start = 12.dp))
                KeyboardActionRow(Icons.Outlined.Refresh, "Reset default skills") {
                    MagicanKeyboardStore.resetSkills(context)
                    skills = MagicanKeyboardStore.defaultSkills
                }
                HorizontalDivider(color = BorderSoft, modifier = Modifier.padding(start = 12.dp))
                KeyboardActionRow(
                    Icons.Outlined.DeleteOutline,
                    if (learnedCount == 1) "Forget 1 learned word" else "Forget $learnedCount learned words",
                ) {
                    MagicanKeyboardStore.resetLearnedWords(context)
                    learnedCount = 0
                }
            }
        }
        Text(
            "Write and Ask never change a field until you approve the preview. Act always asks before creating a task. Password, OTP, and numeric fields expose no agent skills.",
            color = Muted, fontSize = 11.sp, lineHeight = 15.sp,
        )
        Spacer(Modifier.height(12.dp))
    }

    editing?.let { skill ->
        KeyboardSkillEditor(skill, onDismiss = { editing = null }) { updated ->
            skills = skills.map { if (it.id == updated.id) updated else it }
            MagicanKeyboardStore.saveSkills(context, skills)
            editing = null
        }
    }
    if (adding) {
        KeyboardSkillEditor(
            KeyboardSkill(label = "", lane = KeyboardLane.Write, template = ""),
            onDismiss = { adding = false },
        ) { created ->
            skills = skills + created
            MagicanKeyboardStore.saveSkills(context, skills)
            adding = false
        }
    }
}

@Composable
private fun KeyboardTutorialStep(number: Int, detail: String) {
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 9.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Surface(color = Coral, shape = CircleShape, modifier = Modifier.size(22.dp)) {
            Row(horizontalArrangement = Arrangement.Center, verticalAlignment = Alignment.CenterVertically) {
                Text(number.toString(), color = androidx.compose.ui.graphics.Color.White, fontSize = 11.sp, fontWeight = FontWeight.Bold)
            }
        }
        Text(detail, color = Secondary, fontSize = 12.sp, lineHeight = 16.sp, modifier = Modifier.weight(1f))
    }
}

@Composable
private fun KeyboardUsageRow(title: String, detail: String) {
    Column(
        Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 10.dp),
        verticalArrangement = Arrangement.spacedBy(3.dp),
    ) {
        Text(title, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
        Text(detail, color = Muted, fontSize = 11.sp, lineHeight = 15.sp)
    }
}

@Composable
private fun KeyboardLanguageRow(
    option: KeyboardLanguage,
    selected: Boolean,
    onSelect: () -> Unit,
) {
    Row(
        Modifier.fillMaxWidth().clickable { onSelect() }.padding(horizontal = 12.dp, vertical = 12.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(
            if (selected) Icons.Outlined.CheckCircleOutline else Icons.Outlined.RadioButtonUnchecked,
            null,
            tint = if (selected) Coral else Muted,
        )
        Text(option.label, color = Ink, fontSize = 14.sp, modifier = Modifier.weight(1f))
        Text(option.languageTag, color = Muted, fontSize = 11.sp)
    }
}

private data class KeyboardStatus(val enabled: Boolean, val selected: Boolean)

@Composable
private fun rememberKeyboardStatus(): KeyboardStatus {
    val context = LocalContext.current
    var status by remember { mutableStateOf(readKeyboardStatus(context)) }
    val owner = LocalLifecycleOwner.current
    DisposableEffect(owner) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_RESUME) status = readKeyboardStatus(context)
        }
        owner.lifecycle.addObserver(observer)
        onDispose { owner.lifecycle.removeObserver(observer) }
    }
    return status
}

private fun readKeyboardStatus(context: Context): KeyboardStatus {
    val manager = context.getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager
    val serviceName = MagicanKeyboardService::class.java.name
    val enabled = manager.enabledInputMethodList.any {
        it.packageName == context.packageName && it.serviceName == serviceName
    }
    val selected = Settings.Secure.getString(context.contentResolver, Settings.Secure.DEFAULT_INPUT_METHOD)
        .orEmpty().contains(serviceName.substringAfterLast('.')) && enabled
    return KeyboardStatus(enabled, selected)
}

@Composable
private fun KeyboardSection(title: String, content: @Composable () -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(7.dp)) {
        Text(title, color = Secondary, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
        Surface(color = Control, shape = RoundedCornerShape(8.dp), border = BorderStroke(1.dp, BorderSoft)) {
            Column { content() }
        }
    }
}

@Composable
private fun KeyboardStatusRow(ok: Boolean, on: String, off: String) {
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 12.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(
            if (ok) Icons.Outlined.CheckCircleOutline else Icons.Outlined.Info,
            null,
            tint = if (ok) Teal else Muted,
        )
        Text(if (ok) on else off, color = Ink, fontSize = 14.sp)
    }
}

@Composable
private fun KeyboardActionRow(icon: ImageVector, title: String, onClick: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().clickable { onClick() }.padding(horizontal = 12.dp, vertical = 12.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(icon, null, tint = Coral, modifier = Modifier.size(19.dp))
        Text(title, color = Ink, fontSize = 14.sp, modifier = Modifier.weight(1f))
        Text("›", color = Muted, fontSize = 16.sp)
    }
}

@Composable
private fun KeyboardSkillRow(
    skill: KeyboardSkill,
    canMoveUp: Boolean,
    canMoveDown: Boolean,
    canDelete: Boolean,
    onEdit: () -> Unit,
    onMoveUp: () -> Unit,
    onMoveDown: () -> Unit,
    onDelete: () -> Unit,
) {
    Row(
        Modifier.fillMaxWidth().padding(start = 12.dp, top = 8.dp, bottom = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(skill.label, color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
            Text(skill.lane.name, color = when (skill.lane) {
                KeyboardLane.Write -> Coral
                KeyboardLane.Ask -> Teal
                KeyboardLane.Act -> Danger
            }, fontSize = 10.sp, fontWeight = FontWeight.Bold)
            Text(skill.template, color = Muted, fontSize = 11.sp, maxLines = 1)
        }
        IconButton(onClick = onMoveUp, enabled = canMoveUp) { Icon(Icons.Outlined.ArrowUpward, "Move up", tint = if (canMoveUp) Secondary else BorderSoft) }
        IconButton(onClick = onMoveDown, enabled = canMoveDown) { Icon(Icons.Outlined.ArrowDownward, "Move down", tint = if (canMoveDown) Secondary else BorderSoft) }
        IconButton(onClick = onEdit) { Icon(Icons.Outlined.Edit, "Edit", tint = Coral) }
        IconButton(onClick = onDelete, enabled = canDelete) { Icon(Icons.Outlined.DeleteOutline, "Delete", tint = if (canDelete) Danger else BorderSoft) }
    }
}

@Composable
private fun KeyboardSkillEditor(
    initial: KeyboardSkill,
    onDismiss: () -> Unit,
    onSave: (KeyboardSkill) -> Unit,
) {
    var label by remember(initial.id) { mutableStateOf(initial.label) }
    var lane by remember(initial.id) { mutableStateOf(initial.lane) }
    var template by remember(initial.id) { mutableStateOf(initial.template) }
    val candidate = initial.copy(label = label.trim(), lane = lane, template = template.trim())
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(if (initial.label.isBlank()) "Add skill" else "Edit skill", color = Ink) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                MagicianTextField(label, { label = it }, label = { Text("Label") }, singleLine = true)
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    KeyboardLane.entries.forEach { option ->
                        Surface(
                            color = if (lane == option) Coral else Control,
                            shape = RoundedCornerShape(20.dp),
                            border = BorderStroke(1.dp, if (lane == option) Coral else BorderSoft),
                            modifier = Modifier.clickable { lane = option },
                        ) {
                            Text(
                                option.name,
                                color = if (lane == option) androidx.compose.ui.graphics.Color.White else Secondary,
                                fontSize = 11.sp,
                                fontWeight = FontWeight.SemiBold,
                                modifier = Modifier.padding(horizontal = 10.dp, vertical = 7.dp),
                            )
                        }
                    }
                }
                MagicianTextField(
                    template,
                    { template = it },
                    label = { Text("Template") },
                    supportingText = { Text(if (lane == KeyboardLane.Act) "Use {text}; confirmation is mandatory." else "Use {text} for the selected or field text.") },
                    minLines = 4,
                )
            }
        },
        confirmButton = {
            Button(shape = MagicanButtonShape,
                onClick = { onSave(candidate) },
                enabled = candidate.isValid(),
                colors = ButtonDefaults.buttonColors(containerColor = Coral),
            ) { Text("Save") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel", color = Secondary) } },
        containerColor = Panel,
    )
}
