package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.identity.ProductIdentity
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
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.ExpandLess
import androidx.compose.material.icons.outlined.TipsAndUpdates
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner

enum class SettingsPage(val title: String) {
    Root("Settings"),
    /** The shared web Settings, embedded ([EmbeddedWebSettingsPage]). */
    WebSettings("${ProductIdentity.productName} settings"),
    Connection("Connection"),
    HowToUse("How to Use"),
    Keyboard("Magican Keyboard"),
    ProtectedApps("Protected apps"),
}

data class AndroidUsageTip(val group: String, val title: String, val detail: String)

val androidUsageTips = listOf(
    AndroidUsageTip("Getting around", "Find your way around",
        "Use the bottom tabs for Chat, Work, Today, Attention and Observe. Open the menu at the top left for Thinking Map, Notes, VibeDev, Audio Notes, App Pilot, Settings and Themes. Back returns from a detail screen; close a sheet to return to the screen underneath."),
    AndroidUsageTip("Connect", "Connect to your Magician",
        "On the computer, open Settings → Connected devices → Connect Android and choose Same Wi-Fi or Remote. On this phone, open Settings → Connection, choose the same route, scan the QR and confirm the hostname. This connects chat, tasks, notes and voice; the destination is not built into the app."),
    AndroidUsageTip("Connect", "Switch machines and connection routes",
        "Open Settings → Connection to check the active address and choose a saved machine. Pair a new machine using its own QR. Use Same Wi-Fi only on a trusted shared network; Remote is for access from other networks. Forget removes that machine’s saved pairing from this phone."),
    AndroidUsageTip("Chat", "Send a message or make a plan",
        "Type your request and send it. Do asks the assistant to carry out the request; Plan asks it to prepare a plan first. The Ask/Accept control chooses whether edits need your review. Review the proposed plan and choose the action you want before starting it."),
    AndroidUsageTip("Chat", "Keep conversations organized",
        "Tap the composer’s conversation title or history control to open sessions and threads. Pick one to continue it, or use + for a new session. The three-dot menu contains actions for the current conversation. Changing sessions keeps each conversation’s messages together."),
    AndroidUsageTip("Chat", "Choose an engine, provider and model",
        "Open the model selector in the composer. Choose the engine that will handle the conversation, then a provider and model where those choices are offered. Engines such as Claude Code and Codex offer their own models. An unavailable model list may mean the connected computer needs attention."),
    AndroidUsageTip("Chat", "Set thinking effort",
        "Open the model selector and move the Thinking slider to a supported level. Auto uses that model or engine’s default; it does not start a separate adaptive escalation policy. Off is offered only when the model supports it. More thinking can improve difficult answers and take longer."),
    AndroidUsageTip("Chat", "Attach a file or use the camera",
        "Use the attachment icon to choose images, documents, audio or video. Use the camera control when it is offered to add a photo. If a model cannot accept images, choose a compatible model before sending an image request."),
    AndroidUsageTip("Chat", "Use mentions and slash commands",
        "Type @ to choose an agent, personality, skill or Tutor from the suggestions. Type / to see available composer commands, including engine and model choices. Select a suggestion rather than guessing a command name."),
    AndroidUsageTip("Chat", "Read, copy and check an answer",
        "Use the reply’s playback control to hear it and its message actions to copy or share it. An info icon means the answer has a verification note: tap once to read the saved reason. The main answer keeps its headings, paragraphs, links and figures."),
    AndroidUsageTip("Chat", "Inspect what happened in a run",
        "Tap Steps or a run’s activity control to inspect its execution. Run shows progress, steps, events, model usage and timing; Output or Result shows the delivered answer and files. Plan and History are available in the full task view. An info icon beside a summary opens its verification reason."),
    AndroidUsageTip("Chat", "Understand the assistant orb",
        "The animated orb indicates assistant activity. Tapping it changes God mode for that conversation when the control is available. God mode permits broader actions on the connected computer; review its explanation before enabling it. Your normal approval controls still matter."),
    AndroidUsageTip("Voice", "Switch between typing, dictation and conversation",
        "Use the composer’s microphone and voice menu to choose the available Dictation, Hands-free or Live mode. Dictation turns speech into a message; conversation modes let you speak and hear responses. Use Type instead to return to the keyboard, and stop the voice session when finished."),
    AndroidUsageTip("Voice", "Choose speech and speaking voices",
        "Open voice options in the composer to choose the available speech services and speaking voice. Shared speaking voices are in Settings → Magican settings; Settings → Voice & audio sets this phone’s own system voice. Live calls have their own engine and voice choices; changing a chat model or thinking slider does not replace the live call engine."),
    AndroidUsageTip("Voice", "Keep or discard dictation recordings",
        "Dictation normally uses the transcript without keeping a permanent recording. Enable Keep dictation recordings in the dictation settings if you want Audio Notes. Open Menu → Audio Notes to search recordings, play them, retry pending work or delete recordings you no longer need."),
    AndroidUsageTip("Voice", "Use the Android assistant and wake phrase",
        "Open App Pilot → Default assistant to select Magican as your system assistant, then use the system assistant gesture. In Settings → Voice & audio, check Wakes on and start the listener. Its permanent notification lets you stop it. The phrase follows your primary assistant’s name and aliases."),
    AndroidUsageTip("Voice", "Set availability and spoken replies",
        "Open Settings → Voice & audio to choose Stay available for, Conversation mode and Speak replies. The Also capture the screen switch adds screen observations to a meeting session only when enabled. Stop the active voice or observation session from its controls or notification when finished."),
    AndroidUsageTip("Work", "Choose the right Work view",
        "All work brings together Tasks, Monitors and System work. Needs you collects decisions blocking progress. Crew is your AI team, Playbooks hold reusable goals and instructions, Quality checks review outputs, and Team routines coordinates scheduled company work. Tap the info controls for a reminder of what a view does."),
    AndroidUsageTip("Work", "Create, find and manage tasks",
        "Open Work → All work → Tasks. Use search, filters and page controls to find a task, or New work to set an outcome, agent and schedule. Open a task to review its result, plan and run. Its actions menu offers the controls allowed by its current state, such as edit, run, stop, retry, reset or mark done."),
    AndroidUsageTip("Work", "Use monitors and inspect system work",
        "Choose Monitors for scheduled checks and their history. Choose System work for supporting background jobs. Open a row to inspect the exact run rather than starting another task. Search and filters help narrow long lists; show more or change the page when the current list is incomplete."),
    AndroidUsageTip("Work", "Read results and partial completion",
        "A task marked Partly done has finished with requirements still open. Read Open: and the result before deciding what to do next. Open delivered files from Output. Run contains the step checklist and activity; expand technical details when you need older runs, IDs, related tools or intermediate evidence."),
    AndroidUsageTip("Work", "Build your Crew",
        "Open Crew to create or edit a member’s role, instructions, tools and model choices. Choose a primary assistant when you want that member to answer ordinary conversations. A crew member’s detail shows its work and controls; pause or resume it there when those actions are available."),
    AndroidUsageTip("Work", "Set Autopilot, work cycles and focus areas",
        "Autopilot lets a crew member choose useful work from its playbook. Configure work cycles, task and step limits, and focus areas to bound that work. Give each focus area a priority, instructions and schedule, and choose its shared documents. Delegation controls which teammates the member can ask for help."),
    AndroidUsageTip("Work", "Edit Playbooks and shared documents",
        "Open Work → Playbooks to manage reusable goals, instructions, checks and documents. Edit a document and save it; if someone changed it while you were editing, reload the latest version before deciding how to reapply your draft. Assign relevant documents to the crew members or focus areas that need them."),
    AndroidUsageTip("Work", "Run Quality checks",
        "Open Quality checks, choose the output to improve, describe the criteria and select the participating agents. Set model and thinking choices where offered, then start the review. Inspect each round and the resulting output before accepting it; use the run controls to stop or continue a review."),
    AndroidUsageTip("Work", "Manage Team routines",
        "Open Team routines to inspect the shared company schedule, start a sequence manually or review a problem. Individual crew members also have their own work-cycle settings. Use the member’s controls when you want to change only that member’s schedule."),
    AndroidUsageTip("VibeDev", "Find recent runs",
        "Open VibeDev from the menu for all runs, newest activity first. Each row names the project, repository and work location; Needs you counts belong to that run. Pull down to refresh; load older runs from the server at the end of the list. The header folder opens projects; + adds one. Project details hold settings and crews. Use the header’s More menu to turn the Magician at work notification-shade summary on or off."),
    AndroidUsageTip("VibeDev", "Set up a project and start a run",
        "Tap New run for a guided welcome. Choose a starter to prefill a draft; Explore the code selects Discuss. Starters never send automatically. Choose the project and work location above the input. The model chip opens the same engine, model and thinking controls as Chat. Voice mode has the same reply mute, voice settings and Live controls. Use the paperclip or camera for files, photos or a new picture; wait for uploads to finish before sending. Build changes code; Discuss shows read-only guidance; Autopilot runs unattended with an optional budget and, for a new run, a nightly schedule at 02:00 UTC. Enter adds a line; use Hide keyboard to dismiss the keyboard without sending. Choose Original folder to work on the current branch and uncommitted files, Isolated workspace for a separate copy, or Isolated branch for branch review. Original-folder edits show their exact diff before writing: Allow once, for this session or always for the selected scope. Work location is independent of Super god mode."),
    AndroidUsageTip("VibeDev", "Review Turns and Changes",
        "Open a run and switch between Turns and Changes. Turns shows requests, progress and follow-ups; Changes shows the affected files and diffs. When the run needs you, read its proposed edits before approving or rejecting them. Partly done means requirements remain open even though the run ended."),
    AndroidUsageTip("VibeDev", "Steer, stop, follow up and finish",
        "While a run is working, use Steer to add direction or Stop to end it. After it ends, type in the bottom composer to continue that run. Follow-ups keep the original run’s work location. Where the run offers Open PR or Merge, review the changes and confirm the action. Discard branch is in the run’s menu when available and asks for confirmation."),
    AndroidUsageTip("VibeDev", "Coordinate a VibeDev crew",
        "Open a project’s VibeDev crews or choose New crew to coordinate coding work. Split the goal plans parallel lanes; Race engines tries the goal with multiple attempts. Set the optional crew budget and Review first when offered. Open a crew to inspect its runs, answer requests and follow progress. Work → Crew manages your general AI team."),
    AndroidUsageTip("Attention", "Answer questions and approvals",
        "Open Attention or tap a Needs you card. Read the request, then choose an option, type an answer or approve or reject the proposed action. Plan approvals include a review action. Password and verification-code requests have their own protected entry flow; respond in that request rather than posting a secret in chat."),
    AndroidUsageTip("Today", "See what matters now",
        "Today brings together Needs You, follow-ups, active work, deliveries, changes, resurfaced context and briefings. Tap a card to open its related work. Swipe or long-press cards that offer snooze or dismiss; use Hidden to restore a card you hid."),
    AndroidUsageTip("Today", "Read briefings and activity",
        "Open a briefing to view its cards, charts, tables and notes. Expand Activity to search and filter outcomes, learnings, failures and deliveries. A related task or conversation link takes you to the source rather than creating new work."),
    AndroidUsageTip("Observe", "Listen to a meeting or the room",
        "Open Observe and choose Listen here. Start capture, then open the transcript to follow the meeting thread, summaries and notes. The microphone hears nearby speech even if a meeting app is muted. Stop when finished; if another call or app interrupts the microphone, check the session and resume when offered."),
    AndroidUsageTip("Observe", "Join meetings and send a bot",
        "Upcoming lists available calendar meetings. Join or Join (Me) opens a meeting for you; Listen captures your observation; Send bot asks the connected service to send an agent attendee. For a meeting elsewhere, use Join as bot with its supported meeting link and inspect the resulting session."),
    AndroidUsageTip("Observe", "Browse Published Notes",
        "Find published task pages in Observe. Search, choose the page size and move through the pages, then open a result in Notes. Use the available publish action for a completed-task batch or send a selected page to memory review when you want it considered for later recall."),
    AndroidUsageTip("Observe", "Include the screen when you choose",
        "Enable Also capture the screen in Settings → Voice & audio if you want screen keyframes beside meeting audio. Android asks for screen-capture permission for the session. Watch the active-capture notification and stop capture when finished; it can include visible content from other apps."),
    AndroidUsageTip("Notes and files", "Read and edit Notes",
        "Open Menu → Notes. Use the folders panel to choose a note, or search to find it. Open the editor to make changes and save them; use the note and folder actions to create, rename or delete where offered. Notes are stored on the connected service, so check the connection if they do not load."),
    AndroidUsageTip("Notes and files", "Preview, share and publish results",
        "Tap a delivered file to open its preview: documents, web pages, images, audio and video use the available viewer. Use Share file to keep a copy. A completed task’s Publish to Notes action creates or updates its published page. External links can be opened in the browser."),
    AndroidUsageTip("Thinking Map", "Capture and branch an idea",
        "Open Thinking Map from the menu, choose Brainstorm an idea in Observe, or use @brainstorm in Chat. Speak or type a thought. Select an existing thought before adding another to branch from it, and use Break open to explore different directions."),
    AndroidUsageTip("Thinking Map", "Organize and harvest a map",
        "Use Ideas to resume saved maps. Canvas, Focus and Outline offer different views of the same ideas. Edit or connect thoughts, use Undo when needed, and Harvest to copy or share a concise result. When offered, turn a thought into a task or send it for memory review."),
    AndroidUsageTip("Tutor and sharing", "Ask Tutor for an explanation",
        "Mention @tutor with a concept to open a narrated blackboard lesson. Attach an image when you want an explanation of that image. In voice, begin with Tutor or Tutor Quick for a full or shorter lesson. Use Replay, Keep Showing or Ask Again on a completed guide."),
    AndroidUsageTip("Tutor and sharing", "Share content or ask about the current screen",
        "Use Android’s Share menu to send text, links, images or files to Magican. A shared screenshot can open Tutor. When Magican is the default assistant, use the assistant gesture for the available screen-aware Tutor or App Pilot actions; follow any capture or access prompt before continuing."),
    AndroidUsageTip("Magican Keyboard", "Enable and switch to the keyboard",
        "Open Settings → Magican Keyboard for setup and live status. Enable it in Android’s keyboard settings, then choose it from the keyboard switcher. The guide page includes a place to try it. Choose the language used for suggestions and autocorrect in the keyboard settings."),
    AndroidUsageTip("Magican Keyboard", "Write, Ask and Act without leaving a text field",
        "Open the keyboard’s Magican actions and choose a skill chip. Write previews a rewrite; Ask previews an answer for insertion; Act asks for confirmation before creating work. Secure password, OTP and numeric fields remove agent skills before text is read; ordinary typing remains available."),
    AndroidUsageTip("Magican Keyboard", "Customize skills and reset learned words",
        "Use Settings → Magican Keyboard to add, edit, reorder or remove skill chips and choose Write, Ask or Act behavior. In Settings, Reset learned words forgets the keyboard’s learned vocabulary after confirmation. Resetting skills restores their defaults instead of changing your conversations."),
    AndroidUsageTip("App Pilot", "Connect Android app control",
        "Open App Pilot from the menu. Its app-control enrollment is separate from the ordinary mobile connection: begin Android observation setup on the computer, review the device/build approval and scan that separate QR in App Pilot. Follow the displayed enrollment steps rather than reusing a chat connection code."),
    AndroidUsageTip("App Pilot", "Grant access and protect sensitive apps",
        "App Pilot shows the Android permissions it needs for the features you enable, including Accessibility and screen capture. Grant them through their system screens. Open Settings → Protected apps to exclude apps from observation and control. A connected chat does not itself grant app-control access."),
    AndroidUsageTip("App Pilot", "Check connection and activity",
        "Open App Pilot to check its connection and permission status. Activity shows connection events and tool actions, newest first. If an action fails, inspect that event and the relevant permission before retrying. Use the system assistant gesture for supported app guidance after selecting Magican as default assistant."),
    AndroidUsageTip("Widgets and notifications", "Add the at-a-glance Home Screen widget",
        "Long-press the Home Screen, choose Widgets and add Magican at a glance. It opens Attention for requests needing you, the task while work is active, and Talk when the day is clear. Settings → Notifications & permissions shows widget-update availability; updates depend on the selected computer’s connection."),
    AndroidUsageTip("Widgets and notifications", "Allow alerts and answer from a notification",
        "Open Settings → Notifications & permissions to allow notifications or open Android’s notification settings. Show questions on lock screen controls question previews; answering requires unlocking. Simple questions may offer answer buttons or a reply field; larger or protected requests open in the app. Refresh registration updates remote delivery where supported."),
    AndroidUsageTip("Settings", "Choose themes and appearance",
        "Open Themes from the menu to preview and choose a theme. Use the appearance controls for System, Day or Night where offered. The chosen theme supplies the colours and type used throughout the app; the device’s text-size settings can also affect how much fits on screen."),
    AndroidUsageTip("Settings", "Find a setting",
        "Type in the search field at the top of Settings. It filters this phone’s settings by name and by what they control. Settings shared across your devices, such as budgets, harness credentials, preferences and speaking voices, are in Magican settings; a search for one of them shows that row."),
    AndroidUsageTip("Settings", "Open the shared Magican settings",
        "Open Settings → Magican settings for the same Settings page as the web app, on the paired machine. Sign in there once; the phone keeps that sign-in. It holds your preferences, budgets, harness credentials, storage maintenance, speaking voices and other shared settings. Back steps back through its pages, then returns to Settings."),
    AndroidUsageTip("Settings", "Check services and versions",
        "Settings → Connection → Services shows connection health and offers a refresh. Help & about → About shows app and service versions. If a page fails to load, check the selected machine and route first, then refresh service status."),
)

@Composable
fun HowToUseSettingsPage() {
    var query by rememberSaveable { mutableStateOf("") }
    var section by rememberSaveable { mutableStateOf("") }
    val sections = remember { androidUsageTips.map { it.group }.distinct() }
    val groups = remember(query, section) {
        val terms = query.trim().split(Regex("\\s+")).filter(String::isNotEmpty)
        androidUsageTips.filter { tip ->
            (section.isEmpty() || tip.group == section) &&
                terms.all { (tip.group + " " + tip.title + " " + tip.detail).contains(it, ignoreCase = true) }
        }.groupBy(AndroidUsageTip::group)
    }
    Column(Modifier.fillMaxSize().padding(horizontal = 16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("Find a screen or control, then tap a topic for its steps.", color = Muted, fontSize = 12.sp)
        OutlinedTextField(
            value = query, onValueChange = { query = it }, singleLine = true,
            placeholder = { Text("Search screens and controls", fontSize = 13.sp) },
            leadingIcon = { Icon(Icons.Outlined.Search, null, tint = Muted) },
            trailingIcon = if (query.isEmpty()) null else { { IconButton(onClick = { query = "" }) {
                Icon(Icons.Outlined.Close, "Clear search", tint = Muted)
            } } },
            modifier = Modifier.fillMaxWidth(), shape = RoundedCornerShape(8.dp),
            textStyle = TextStyle(fontSize = 14.sp),
            colors = OutlinedTextFieldDefaults.colors(
                focusedTextColor = Ink, unfocusedTextColor = Ink, cursorColor = Coral,
                focusedContainerColor = Control, unfocusedContainerColor = Control,
                focusedBorderColor = Coral, unfocusedBorderColor = BorderSoft,
            ),
        )
        CatalogChoiceDropdown("Section", section,
            listOf(CatalogChoiceOption("", "All sections")) + sections.map { CatalogChoiceOption(it, it) },
            onSelect = { section = it })
        LazyColumn(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            groups.forEach { (group, tips) ->
                item(key = "section:$group") {
                    Text(group, color = Secondary, fontSize = 12.sp, fontWeight = FontWeight.SemiBold,
                        modifier = Modifier.padding(top = 10.dp, bottom = 3.dp))
                }
                items(tips, key = { it.group + ":" + it.title }) { tip -> SettingsTipRow(tip, query.isNotBlank()) }
            }
            if (groups.isEmpty()) item {
                Text("No matching topics. Try another word or choose All sections.", color = Muted, fontSize = 13.sp)
                TextButton(onClick = { query = ""; section = "" }) { Text("Show all topics", color = Coral) }
            }
            item { Spacer(Modifier.height(18.dp)) }
        }
    }
}

@Composable
private fun SettingsTipRow(tip: AndroidUsageTip, searching: Boolean) {
    var open by rememberSaveable(tip.group, tip.title) { mutableStateOf(false) }
    Surface(color = Control, shape = RoundedCornerShape(8.dp), border = BorderStroke(1.dp, BorderSoft)) {
        Column {
            Row(
                Modifier.fillMaxWidth().clickable(enabled = !searching, role = Role.Button) { open = !open }
                    .padding(horizontal = 12.dp, vertical = 12.dp),
                horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.CenterVertically,
            ) {
                Icon(Icons.Outlined.TipsAndUpdates, null, tint = Coral, modifier = Modifier.size(18.dp))
                Text(tip.title, color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
                if (!searching) Icon(if (open) Icons.Outlined.ExpandLess else Icons.Outlined.ExpandMore, if (open) "Collapse topic" else "Expand topic", tint = Muted, modifier = Modifier.size(18.dp))
            }
            if (open || searching) Text(tip.detail, color = Muted, fontSize = 13.sp, lineHeight = 19.sp,
                modifier = Modifier.padding(start = 12.dp, end = 12.dp, bottom = 12.dp))
        }
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
        Surface(color = AccentFill, shape = CircleShape, modifier = Modifier.size(22.dp)) {
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
                            color = if (lane == option) AccentFill else Control,
                            shape = Lively.chipShape,
                            border = BorderStroke(1.dp, if (lane == option) AccentFill else BorderSoft),
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
                colors = magicanButtonColors(),
            ) { Text("Save") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel", color = Secondary) } },
        containerColor = Panel,
    )
}
