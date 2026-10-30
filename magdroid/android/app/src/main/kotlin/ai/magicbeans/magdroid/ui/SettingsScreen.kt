package ai.magicbeans.magdroid.ui

import android.Manifest
import android.os.Build
import android.content.pm.PackageManager
import ai.magicbeans.magdroid.BuildConfig
import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.chat.ChatRepository
import ai.magicbeans.magdroid.chat.ServiceHealth
import ai.magicbeans.magdroid.identity.ProductIdentity
import ai.magicbeans.magdroid.push.AndroidMobilePushRegistration
import ai.magicbeans.magdroid.push.AttentionLockScreenPrivacy
import ai.magicbeans.magdroid.push.NotificationPermissionMemory
import ai.magicbeans.magdroid.push.NotificationPrompt
import ai.magicbeans.magdroid.push.notificationPromptFor
import ai.magicbeans.magdroid.push.notificationsAllowed
import ai.magicbeans.magdroid.push.openNotificationSettings
import ai.magicbeans.magdroid.voice.AmbientLeash
import ai.magicbeans.magdroid.voice.AmbientVoiceMode
import androidx.compose.material.icons.automirrored.outlined.ScreenShare
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.NotificationsNone
import androidx.compose.material.icons.outlined.CloudQueue
import androidx.compose.material.icons.outlined.Widgets
import androidx.compose.material.icons.outlined.StopCircle
import androidx.compose.ui.draw.alpha
import ai.magicbeans.magdroid.voice.VoicePrefs
import ai.magicbeans.magdroid.voice.WakeService
import ai.magicbeans.magdroid.voice.PrimaryAgentWakeIdentityStore
import ai.magicbeans.magdroid.keyboard.MagicanKeyboardStore
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.interaction.DragInteraction
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.selection.selectable
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.layout.positionInParent
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.VolumeUp
import androidx.compose.material.icons.outlined.Autorenew
import androidx.compose.material.icons.automirrored.outlined.HelpOutline
import androidx.compose.material.icons.outlined.Keyboard
import androidx.compose.material.icons.outlined.DeleteOutline
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.Sensors
import androidx.compose.material.icons.outlined.Tune
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.ContextCompat
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import kotlinx.coroutines.launch
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import com.google.firebase.FirebaseApp

/**
 * Settings: only what belongs to this phone, as one headed list with a
 * search field, under a top row that opens the shared web Settings
 * ([SettingsPage.WebSettings]).
 *
 * Budgets, harness credentials, preferences, storage maintenance, speaking
 * voices and every other setting shared across devices live in that web
 * page; they have no native copy here. The rows, their sections and their
 * search keywords are indexed in [DeviceSettingsRow], so a search for a
 * shared setting still finds the row that leads to it.
 *
 * Every visible row has a real destination or mutation. Platform-only iOS
 * affordances are represented by their Android equivalent, never by a
 * "not built" dialog that looks actionable and then goes nowhere.
 */
@Composable
internal fun SettingsScreen(
    page: SettingsPage,
    onOpenPage: (SettingsPage) -> Unit,
    mobilePairing: PairingUiBridge = PairingUiBridge(),
) {
    when (page) {
        SettingsPage.WebSettings -> return EmbeddedWebSettingsPage(
            onOpenConnection = { onOpenPage(SettingsPage.Connection) },
        )
        SettingsPage.Connection -> return MobileConnectionSettingsPage(mobilePairing)
        SettingsPage.HowToUse -> return HowToUseSettingsPage()
        SettingsPage.Keyboard -> return MagicanKeyboardSettingsPage()
        SettingsPage.ProtectedApps -> return ProtectedAppsSettingsPage()
        SettingsPage.Root -> Unit
    }
    val context = LocalContext.current
    val remoteDeliveryPackaged = remember(context) { FirebaseApp.getApps(context).isNotEmpty() }
    var query by rememberSaveable { mutableStateOf("") }
    var showResetLearned by remember { mutableStateOf(false) }
    var learnedCleared by remember { mutableStateOf(false) }
    val prefs = remember(context) { VoicePrefs.get(context) }
    val speakReplies by prefs.speakReplies.collectAsStateWithLifecycle()
    val listening by WakeService.listening.collectAsStateWithLifecycle()
    val wakeProblem by WakeService.problem.collectAsStateWithLifecycle()
    remember(context) { PrimaryAgentWakeIdentityStore.initialize(context) }
    val wakeIdentity by PrimaryAgentWakeIdentityStore.identity.collectAsStateWithLifecycle()
    val wakePhrases by PrimaryAgentWakeIdentityStore.phrases.collectAsStateWithLifecycle()
    val wakeIdentityProblem by PrimaryAgentWakeIdentityStore.problem.collectAsStateWithLifecycle()
    val leash by prefs.leash.collectAsStateWithLifecycle()
    val observeScreen by prefs.observeScreen.collectAsStateWithLifecycle()
    val ambientMode by prefs.ambientMode.collectAsStateWithLifecycle()
    var micGranted by remember {
        mutableStateOf(
            ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) ==
                PackageManager.PERMISSION_GRANTED,
        )
    }
    var permissionProblem by remember { mutableStateOf<String?>(null) }
    var notificationsGranted by remember {
        mutableStateOf(notificationsAllowed(context))
    }
    var notificationDecision by remember {
        mutableStateOf(notificationPromptFor(context))
    }
    var showQuestionsOnLockScreen by remember {
        mutableStateOf(AttentionLockScreenPrivacy.showQuestions(context))
    }
    val requestMicrophone = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        micGranted = granted
        if (granted) {
            permissionProblem = null
            WakeService.start(context)
        } else {
            permissionProblem = "Microphone permission is off. Allow it in App Pilot → Permissions before starting wake-word listening."
        }
    }
    val requestNotifications = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        NotificationPermissionMemory.markAsked(context)
        notificationsGranted = notificationsAllowed(context) || granted
        notificationDecision = notificationPromptFor(context)
        if (granted) AndroidMobilePushRegistration.ensure(context, force = true)
    }

    val repository = remember(context) { ChatRepository(context) }
    DisposableEffect(repository) { onDispose(repository::close) }
    // Three services, as iOS lists them. Magicutor and DesktopProxy answer on
    // their own paths behind the same host.
    var health by remember { mutableStateOf<ServiceHealth?>(null) }
    var magicutor by remember { mutableStateOf<ServiceHealth?>(null) }
    var desktop by remember { mutableStateOf<ServiceHealth?>(null) }
    var supervisorVersion by remember { mutableStateOf<String?>(null) }
    var apiVersion by remember { mutableStateOf<String?>(null) }
    var checking by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val lifecycleOwner = LocalLifecycleOwner.current
    // Match iOS's foreground refresh, not merely first composition. Returning
    // from Crew or App Pilot can change the endpoint or primary definition
    // while this Settings composition remains alive.
    DisposableEffect(lifecycleOwner, context) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_RESUME) {
                scope.launch { PrimaryAgentWakeIdentityStore.refresh(context) }
                notificationsGranted = notificationsAllowed(context)
                notificationDecision = notificationPromptFor(context)
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }
    val refresh: () -> Unit = {
        if (!checking) {
            checking = true
            scope.launch {
                val stack = repository.healthStack()
                health = stack.magician
                magicutor = stack.magicutor
                desktop = stack.desktop
                supervisorVersion = stack.supervisorVersion
                apiVersion = stack.apiVersion
                checking = false
            }
        }
    }
    // Same live cadence as iOS while Settings is visible. Leaving the surface
    // cancels this effect and its in-flight probe; the manual row uses the same
    // single-flight action rather than starting a second polling system.
    LaunchedEffect(Unit) {
        while (isActive) {
            refresh()
            delay(10_000)
        }
    }

    val searching = query.isNotBlank()
    val sections = visibleDeviceSettings(query) { row ->
        row != DeviceSettingsRow.RefreshRegistration || notificationsGranted
    }
    val showWebSettings = DeviceSettingsRow.WebSettings.matches(query)
    val assistantLabel = wakeIdentity?.name?.takeIf(String::isNotBlank) ?: "your assistant"

    // The section chip bar: one chip per drawn section, pinned above the
    // scrolling list. Tapping a chip scrolls to that section's heading; while
    // the list scrolls the chip of the section in view lights up.
    val contentScroll = rememberScrollState()
    val sectionOrder = sections.map { it.first }
    // Each section's top, in the scrolled content's coordinates.
    val headerOffsets = remember { mutableStateMapOf<DeviceSettingsSection, Int>() }
    val density = LocalDensity.current
    val contentPaddingPx = with(density) { SETTINGS_CONTENT_PADDING_DP.dp.roundToPx() }
    val spyLinePx = with(density) { SETTINGS_SPY_LINE_DP.dp.roundToPx() }
    val spiedSection by remember(sectionOrder, spyLinePx) {
        derivedStateOf {
            activeSettingsSection(
                sectionOrder.mapNotNull { section -> headerOffsets[section]?.let { section to it } },
                scrollY = contentScroll.value,
                maxScroll = contentScroll.maxValue,
                linePx = spyLinePx,
            )
        }
    }
    // A tapped chip stays active through its scroll animation (and after it,
    // when the list cannot scroll that far) until the user drags the list.
    var tappedSection by remember(query) { mutableStateOf<DeviceSettingsSection?>(null) }
    LaunchedEffect(contentScroll) {
        contentScroll.interactionSource.interactions.collect { interaction ->
            if (interaction is DragInteraction.Start) tappedSection = null
        }
    }
    val activeSection = tappedSection?.takeIf { it in sectionOrder }
        ?: spiedSection
        ?: sectionOrder.firstOrNull()
    val jumpToSection: (DeviceSettingsSection) -> Unit = { section ->
        tappedSection = section
        headerOffsets[section]?.let { top ->
            scope.launch {
                contentScroll.animateScrollTo((top - contentPaddingPx).coerceIn(0, contentScroll.maxValue))
            }
        }
    }

    Column(Modifier.fillMaxSize()) {
        WorkSearchField("Search settings", query, { query = it }, Modifier.padding(start = 16.dp, end = 16.dp, top = 16.dp))
        if (sectionOrder.isNotEmpty()) {
            SettingsSectionBar(sectionOrder, activeSection, jumpToSection)
        }

        Column(
            Modifier
                .weight(1f)
                .fillMaxWidth()
                .verticalScroll(contentScroll)
                .padding(SETTINGS_CONTENT_PADDING_DP.dp),
            verticalArrangement = Arrangement.spacedBy(20.dp),
        ) {
            if (showWebSettings) {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    SettingsCard {
                        NavDetailRow(
                            Icons.Outlined.Tune,
                            DeviceSettingsRow.WebSettings.title,
                            "Budgets, harness credentials, preferences, voices and everything shared across your devices",
                        ) { onOpenPage(SettingsPage.WebSettings) }
                    }
                    if (!searching) {
                        Footnote("The same Settings as the web app, on the paired machine. Below are the settings that belong to this phone.")
                    }
                }
            }

            sections.forEach { (section, rows) ->
                SettingsSection(
                    section.title,
                    Modifier.onGloballyPositioned { coordinates ->
                        // Relative to the scrolling column's padded content, so
                        // add the padding back to get the scroll-content offset.
                        val top = coordinates.positionInParent().y.toInt() + contentPaddingPx
                        if (headerOffsets[section] != top) headerOffsets[section] = top
                    },
                ) {
                    rows.forEachIndexed { index, row ->
                        if (index > 0) RowDivider()
                        when (row) {
                            DeviceSettingsRow.WebSettings -> Unit
                            // iOS's arm control: one button whose label is the state, not a
                            // switch. Arming is an action with a consequence — the
                            // microphone opens — and a switch invites the flick that a
                            // button does not.
                            DeviceSettingsRow.TalkToAssistant -> NavRow(
                                if (listening) Icons.Outlined.StopCircle else Icons.Outlined.Sensors,
                                if (listening) "Stop listening" else "Talk to $assistantLabel",
                            ) {
                                if (listening) {
                                    WakeService.stop(context)
                                } else if (wakePhrases.isEmpty()) {
                                    permissionProblem = wakeIdentityProblem
                                        ?: "${ProductIdentity.productName} hasn't loaded the primary assistant name yet. Check the self-hosted connection and try again."
                                } else if (micGranted) {
                                    WakeService.start(context)
                                } else {
                                    requestMicrophone.launch(Manifest.permission.RECORD_AUDIO)
                                }
                            }
                            // The phrase readout. Naming what it wakes on is the difference
                            // between a microphone you armed and one you are guessing about.
                            DeviceSettingsRow.WakesOn -> ValueRow(
                                "Wakes on",
                                wakePhrases.takeIf { it.isNotEmpty() }
                                    ?.joinToString(" or ") { "“$it”" }
                                    ?: "Waiting for primary agent sync",
                            )
                            DeviceSettingsRow.PhoneVoice -> Box(Modifier.padding(horizontal = 12.dp, vertical = 8.dp)) {
                                DeviceSpeechVoicePicker()
                            }
                            DeviceSettingsRow.StayAvailable -> {
                                Label("Stay available for")
                                AmbientLeash.entries.forEach { option ->
                                    RowDivider()
                                    SelectRow(option.label, selected = leash == option) { prefs.setLeash(option) }
                                }
                            }
                            DeviceSettingsRow.CaptureScreen -> SwitchRow(
                                icon = Icons.AutoMirrored.Outlined.ScreenShare,
                                title = "Also capture the screen",
                                subtitle = "Sends screen keyframes into the meeting thread beside the audio. " +
                                    "Off unless you turn it on.",
                                checked = observeScreen,
                            ) {
                                prefs.setObserveScreen(!observeScreen)
                            }
                            DeviceSettingsRow.ConversationMode -> {
                                Label("Conversation mode")
                                AmbientVoiceMode.entries.filter(AmbientVoiceMode::built).forEach { option ->
                                    RowDivider()
                                    SelectRow(
                                        option.label,
                                        selected = ambientMode == option,
                                        enabled = true,
                                    ) { prefs.setAmbientMode(option) }
                                }
                            }
                            DeviceSettingsRow.SpeakReplies -> SwitchRow(
                                icon = Icons.AutoMirrored.Outlined.VolumeUp,
                                title = "Speak replies",
                                subtitle = "Read answers aloud when you asked by voice.",
                                checked = speakReplies,
                            ) {
                                prefs.setSpeakReplies(!speakReplies)
                            }
                            DeviceSettingsRow.Notifications -> {
                                InfoRow(
                                    Icons.Outlined.NotificationsNone,
                                    "Notifications",
                                    if (notificationsGranted) "Allowed" else "Not allowed",
                                )
                                RowDivider()
                                NavRow(
                                    Icons.Outlined.NotificationsNone,
                                    when (notificationDecision) {
                                        NotificationPrompt.Request -> "Allow notifications"
                                        NotificationPrompt.Granted -> "Open notification settings"
                                        NotificationPrompt.OpenSettings -> "Open notification settings"
                                    },
                                ) {
                                    when (notificationDecision) {
                                        NotificationPrompt.Request -> {
                                            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                                                requestNotifications.launch(Manifest.permission.POST_NOTIFICATIONS)
                                            } else {
                                                openNotificationSettings(context)
                                            }
                                        }
                                        NotificationPrompt.Granted, NotificationPrompt.OpenSettings ->
                                            openNotificationSettings(context)
                                    }
                                }
                            }
                            DeviceSettingsRow.RefreshRegistration -> NavRow(Icons.Outlined.Autorenew, "Refresh registration") {
                                AndroidMobilePushRegistration.ensure(context, force = true)
                            }
                            DeviceSettingsRow.LockScreenQuestions -> SwitchRow(
                                Icons.Outlined.Lock,
                                "Show questions on lock screen",
                                if (showQuestionsOnLockScreen) {
                                    "Questions and their answer buttons show while the phone is locked. Answering still asks you to unlock."
                                } else {
                                    "Questions stay hidden until you unlock. The lock screen only shows the first alert."
                                },
                                showQuestionsOnLockScreen,
                            ) {
                                showQuestionsOnLockScreen = !showQuestionsOnLockScreen
                                AttentionLockScreenPrivacy.setShowQuestions(context, showQuestionsOnLockScreen)
                            }
                            DeviceSettingsRow.HomeScreenWidget ->
                                InfoRow(Icons.Outlined.Widgets, "Home Screen widget", "Add from your launcher’s Widgets menu")
                            DeviceSettingsRow.RemoteDelivery -> InfoRow(
                                Icons.Outlined.CloudQueue,
                                "Remote delivery",
                                when {
                                    !MagicianAccess.isConfigured(context) -> "Connect to Magician first"
                                    !remoteDeliveryPackaged -> "Polling only"
                                    else -> "Packaged · host-dependent"
                                },
                            )
                            DeviceSettingsRow.Keyboard ->
                                NavRow(Icons.Outlined.Keyboard, "${ProductIdentity.productName} Keyboard") { onOpenPage(SettingsPage.Keyboard) }
                            DeviceSettingsRow.ResetLearnedWords -> {
                                NavRow(Icons.Outlined.DeleteOutline, "Reset learned words") {
                                    showResetLearned = true
                                }
                                if (learnedCleared) {
                                    RowDivider()
                                    ValueRow("Learned vocabulary", "cleared")
                                }
                            }
                            DeviceSettingsRow.ProtectedApps -> NavRow(
                                Icons.Outlined.Lock,
                                "Protected apps",
                                onClick = { onOpenPage(SettingsPage.ProtectedApps) },
                            )
                            DeviceSettingsRow.MagicianConnection -> {
                                NavRow(
                                    Icons.Outlined.CloudQueue,
                                    if (MagicianAccess.isConfigured(context)) "Connected to Magician" else "Connect to Magician",
                                ) { onOpenPage(SettingsPage.Connection) }
                                if (MagicianAccess.isConfigured(context)) {
                                    RowDivider()
                                    ValueRow("Magician", MagicianAccess.baseUrlLabel(context))
                                }
                            }
                            DeviceSettingsRow.Services -> {
                                ServiceAccountsButton()
                                RowDivider()
                                ServiceRow("Magician backend", health, checking)
                                RowDivider()
                                ServiceRow("Magicutor", magicutor, checking)
                                RowDivider()
                                ServiceRow("DesktopProxy", desktop, checking)
                                RowDivider()
                                NavRow(
                                    Icons.Outlined.Autorenew,
                                    if (checking) "Checking…" else "Refresh service status",
                                    onClick = refresh,
                                )
                            }
                            DeviceSettingsRow.HowToUse ->
                                NavRow(Icons.AutoMirrored.Outlined.HelpOutline, "How to Use") { onOpenPage(SettingsPage.HowToUse) }
                            DeviceSettingsRow.About -> {
                                ValueRow(
                                    "${ProductIdentity.productName} Android",
                                    "${BuildConfig.VERSION_NAME} (${BuildConfig.VERSION_CODE})",
                                )
                                RowDivider()
                                // The Magician core version (`versions.magician`, else `version`).
                                ValueRow("Magician backend", health?.version ?: "—")
                                // Shown only when the endpoint reported one. A row reading "—" for a
                                // service that is simply not deployed says something is wrong with
                                // it, which is a different claim from saying nothing.
                                apiVersion?.takeIf { it.isNotBlank() }?.let {
                                    RowDivider(); ValueRow("Magician API", it)
                                }
                                magicutor?.version?.takeIf { it.isNotBlank() }?.let {
                                    RowDivider(); ValueRow("Magicutor", it)
                                }
                                supervisorVersion?.takeIf { it.isNotBlank() }?.let {
                                    RowDivider(); ValueRow("Supervisor", it)
                                }
                                desktop?.version?.takeIf { it.isNotBlank() }?.let {
                                    RowDivider(); ValueRow("Desktop", it)
                                }
                                RowDivider()
                                ValueRow("Scope", "${MagicianAccess.principal(context)} / ${MagicianAccess.workspace(context)}")
                            }
                        }
                    }
                }
                when (section) {
                    DeviceSettingsSection.VoiceAndAudio -> {
                        (permissionProblem ?: wakeProblem ?: wakeIdentityProblem)?.let { problem ->
                            Text(problem, color = Danger, fontSize = 11.sp, lineHeight = 15.sp)
                        }
                        if (!searching) Footnote("${leash.detail} ${ambientMode.detail}")
                    }
                    DeviceSettingsSection.NotificationsAndPermissions -> if (!searching) {
                        val notificationNote = if (notificationsGranted) {
                            "Notifications are allowed. Open notification settings to review or turn them off. Refresh registration sends this phone’s alert route again."
                        } else {
                            "${ProductIdentity.productName} asks for notifications when it opens, while Android will still show the dialog. Allow notifications asks again. After a denial, that row opens Android’s notification settings."
                        }
                        Footnote(
                            "$notificationNote The widget keeps its last truthful Today snapshot. " +
                                "Firebase adds background Attention and followed-task updates when this deployment includes it.",
                        )
                    }
                    DeviceSettingsSection.KeyboardAndPrivacy -> if (!searching) {
                        Footnote("Reset learned words forgets words learned by ${ProductIdentity.productName} Keyboard. Custom Write, Ask, and Act skills are kept.")
                    }
                    DeviceSettingsSection.Connection, DeviceSettingsSection.HelpAndAbout -> Unit
                }
            }

            if (searching && sections.isEmpty() && !showWebSettings) {
                SettingsCard {
                    InfoRow(
                        Icons.Outlined.Search,
                        "No settings match",
                        "Nothing on this phone matches “${query.trim()}”. Shared settings are in ${DeviceSettingsRow.WebSettings.title}.",
                    )
                    RowDivider()
                    NavRow(Icons.Outlined.Tune, "Open ${DeviceSettingsRow.WebSettings.title}") {
                        onOpenPage(SettingsPage.WebSettings)
                    }
                }
            }

            Spacer(Modifier.height(8.dp))
        }
    }

    if (showResetLearned) {
        AlertDialog(
            onDismissRequest = { showResetLearned = false },
            title = { Text("Reset learned words?", color = Ink) },
            text = { Text("This forgets every word learned from typing. It cannot be undone.", color = Secondary) },
            confirmButton = {
                TextButton(onClick = {
                    MagicanKeyboardStore.resetLearnedWords(context)
                    learnedCleared = true
                    showResetLearned = false
                }) { Text("Reset", color = Danger) }
            },
            dismissButton = {
                TextButton(onClick = { showResetLearned = false }) { Text("Cancel", color = Secondary) }
            },
            containerColor = Panel,
        )
    }

}

@Composable
private fun MobileConnectionSettingsPage(pairing: PairingUiBridge) {
    val context = LocalContext.current
    Column(
        Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(
            "This connection is used by chat, tasks, notes, voice, widgets, and notifications. App Pilot automation is enrolled separately inside App Pilot.",
            color = Muted,
            fontSize = 11.sp,
            lineHeight = 15.sp,
        )
        MachinesSection(onActiveChanged = pairing.onPaired)
        DevicePairingSetup(
            context = context,
            purpose = PairingPurpose.MobileAccess,
            pendingEnrollmentUri = pairing.pendingEnrollmentUri,
            onEnrollmentConsumed = pairing.onEnrollmentConsumed,
            pairingScanIssue = pairing.scanIssue,
            onPairingScanIssueConsumed = pairing.onScanIssueConsumed,
            onScanPairing = pairing.onScan,
            onPaired = pairing.onPaired,
        )
    }
}

/** A one-of-many choice, checked rather than switched. */
@Composable
private fun SelectRow(
    title: String,
    selected: Boolean,
    enabled: Boolean = true,
    onSelect: () -> Unit,
) {
    Row(
        Modifier
            .fillMaxWidth()
            .background(if (selected) Coral.copy(alpha = 0.08f) else Color.Transparent)
            .alpha(if (enabled) 1f else 0.45f)
            .clickable(enabled = enabled) { onSelect() }
            .padding(horizontal = 12.dp, vertical = 11.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            title,
            color = Ink, fontSize = 14.sp,
            fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
            modifier = Modifier.weight(1f),
        )
        if (selected) {
            Icon(
                Icons.Filled.CheckCircle, contentDescription = "Selected",
                tint = Coral, modifier = Modifier.size(17.dp),
            )
        }
    }
}

/** A heading inside a card, for a group of choices. */
@Composable
private fun Label(text: String) {
    Text(
        text,
        color = Secondary, fontSize = 12.sp, fontWeight = FontWeight.SemiBold,
        modifier = Modifier.padding(start = 12.dp, top = 12.dp, bottom = 2.dp),
    )
}

/** Padding around the scrolling Settings list; a jumped-to heading lands this far below the chip bar. */
private const val SETTINGS_CONTENT_PADDING_DP = 16

/** The scroll-spy line, below the top of the list: the last heading above it is the active section. */
private const val SETTINGS_SPY_LINE_DP = 56

/** A headed group of rows under a [SettingsSectionHeading]. */
@Composable
private fun SettingsSection(title: String, modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    Column(modifier, verticalArrangement = Arrangement.spacedBy(10.dp)) {
        SettingsSectionHeading(title)
        SettingsCard(content)
    }
}

/**
 * A Settings section heading: bold, larger than the rows, sentence case, in
 * the primary text colour, start-aligned with the rows' content inside the
 * cards. Only this screen uses it; other lists keep [WorkGroupTitle].
 */
@Composable
private fun SettingsSectionHeading(title: String) {
    Text(
        title,
        color = Ink,
        style = MaterialTheme.typography.titleMedium,
        fontSize = 18.sp,
        lineHeight = 24.sp,
        fontWeight = FontWeight.Bold,
        modifier = Modifier
            .fillMaxWidth()
            .padding(start = 12.dp, end = 12.dp, top = 8.dp)
            .semantics { heading() },
    )
}

/**
 * The pinned, horizontally scrolling bar of section chips under the search
 * field. The active chip uses the accent tint the Work view switcher uses for
 * its selected segment; the rest sit on the control fill. The row scrolls
 * itself to keep the active chip in sight.
 */
@Composable
private fun SettingsSectionBar(
    sections: List<DeviceSettingsSection>,
    active: DeviceSettingsSection?,
    onSelect: (DeviceSettingsSection) -> Unit,
) {
    val rowState = rememberLazyListState()
    LaunchedEffect(active, sections) {
        val index = sections.indexOf(active)
        if (index < 0) return@LaunchedEffect
        val layout = rowState.layoutInfo
        val shown = layout.visibleItemsInfo.firstOrNull { it.index == index }
        val fullyShown = shown != null &&
            shown.offset >= layout.viewportStartOffset &&
            shown.offset + shown.size <= layout.viewportEndOffset
        if (!fullyShown) rowState.animateScrollToItem(index)
    }
    Column(Modifier.fillMaxWidth().testTag("settings-section-bar")) {
        LazyRow(
            state = rowState,
            contentPadding = PaddingValues(horizontal = 16.dp, vertical = 8.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier.fillMaxWidth(),
        ) {
            items(sections, key = { it.name }) { section ->
                SettingsSectionChip(section, active = section == active) { onSelect(section) }
            }
        }
        HorizontalDivider(color = BorderSoft)
    }
}

@Composable
private fun SettingsSectionChip(section: DeviceSettingsSection, active: Boolean, onClick: () -> Unit) {
    val shape = RoundedCornerShape(50)
    Box(
        Modifier
            .heightIn(min = 36.dp)
            .clip(shape)
            .background(if (active) Coral.copy(alpha = 0.14f) else Control)
            .border(1.dp, if (active) Coral.copy(alpha = 0.45f) else ControlBorder, shape)
            .selectable(selected = active, role = Role.Tab, onClick = onClick)
            .semantics { selected = active }
            .testTag("settings-section-chip-${section.name}")
            .padding(horizontal = 14.dp, vertical = 8.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            section.title,
            color = if (active) Coral else Ink.copy(alpha = 0.78f),
            fontSize = 14.sp,
            fontWeight = if (active) FontWeight.SemiBold else FontWeight.Medium,
            maxLines = 1,
            softWrap = false,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun SettingsCard(content: @Composable () -> Unit) {
    Surface(color = Control, shape = RoundedCornerShape(8.dp), border = BorderStroke(1.dp, BorderSoft)) {
        Column { content() }
    }
}

/** Explanatory text under a section. */
@Composable
private fun Footnote(text: String) {
    Text(text, color = Muted, fontSize = 11.sp, lineHeight = 15.sp)
}

@Composable
private fun RowDivider() = HorizontalDivider(color = BorderSoft, modifier = Modifier.padding(start = 12.dp))

@Composable
private fun NavRow(icon: ImageVector, title: String, onClick: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().clickable { onClick() }.padding(horizontal = 12.dp, vertical = 13.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(icon, contentDescription = null, tint = Coral, modifier = Modifier.size(17.dp))
        Text(title, color = Ink, fontSize = 14.sp, modifier = Modifier.weight(1f))
        Text("›", color = Muted, fontSize = 16.sp)
    }
}

/** A navigation row whose detail sits below its title. */
@Composable
private fun NavDetailRow(icon: ImageVector, title: String, detail: String, onClick: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().clickable { onClick() }.padding(horizontal = 12.dp, vertical = 13.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(icon, contentDescription = null, tint = Coral, modifier = Modifier.size(17.dp))
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(title, color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
            Text(detail, color = Muted, fontSize = 12.sp, lineHeight = 16.sp)
        }
        Text("›", color = Muted, fontSize = 16.sp)
    }
}

/** Instructions sit below their title so long values cannot squeeze the label. */
@Composable
private fun InfoRow(icon: ImageVector, title: String, detail: String) {
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 13.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(icon, contentDescription = null, tint = Coral, modifier = Modifier.size(17.dp))
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(title, color = Ink, fontSize = 14.sp)
            Text(detail, color = Muted, fontSize = 12.sp, lineHeight = 16.sp)
        }
    }
}

@Composable
private fun SwitchRow(
    icon: ImageVector,
    title: String,
    subtitle: String,
    checked: Boolean,
    onToggle: () -> Unit,
) {
    Row(
        Modifier.fillMaxWidth().clickable { onToggle() }.padding(horizontal = 12.dp, vertical = 11.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.size(22.dp), contentAlignment = Alignment.Center) {
            Icon(icon, contentDescription = null, tint = Coral, modifier = Modifier.size(15.dp))
        }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(title, color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
            Text(subtitle, color = Muted, fontSize = 11.sp, lineHeight = 15.sp)
        }
        Switch(
            checked = checked,
            onCheckedChange = { onToggle() },
            colors = magicanSwitchColors(),
        )
    }
}

/**
 * A service, its state, and its version.
 *
 * The dot carries the state rather than a word alone, because it is the part
 * anyone scans for. Unknown is its own colour: "not checked yet" and "down" are
 * different things and showing both as red teaches people to ignore the dot.
 */
@Composable
private fun ServiceRow(label: String, health: ServiceHealth?, checking: Boolean) {
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 13.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(
            Modifier.size(9.dp).background(
                when {
                    health == null -> Muted
                    health.reachable -> Teal
                    else -> Danger
                },
                CircleShape,
            ),
        )
        Text(label, color = Ink, fontSize = 14.sp)
        health?.version?.takeIf { it.isNotBlank() }?.let {
            Text(it, color = Muted, fontSize = 11.sp, fontFamily = LocalMagicanFontFamilies.current.mono)
        }
        Spacer(Modifier.weight(1f))
        Text(
            when {
                checking -> "Checking…"
                health == null -> "Unknown"
                else -> health.label()
            },
            color = Secondary, fontSize = 12.sp,
        )
    }
}

@Composable
private fun ValueRow(label: String, value: String) {
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 13.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(label, color = Ink, fontSize = 14.sp, modifier = Modifier.weight(1f))
        Text(value, color = Muted, fontSize = 12.sp, fontFamily = LocalMagicanFontFamilies.current.mono)
    }
}
