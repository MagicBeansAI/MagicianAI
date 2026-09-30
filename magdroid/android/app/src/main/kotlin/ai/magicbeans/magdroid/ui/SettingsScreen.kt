package ai.magicbeans.magdroid.ui

import android.Manifest
import android.os.Build
import android.content.pm.PackageManager
import ai.magicbeans.magdroid.BuildConfig
import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.chat.ChatRepository
import ai.magicbeans.magdroid.chat.ServiceHealth
import ai.magicbeans.magdroid.identity.ProductIdentity
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
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
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
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.automirrored.outlined.HelpOutline
import androidx.compose.material.icons.outlined.Keyboard
import androidx.compose.material.icons.outlined.DeleteOutline
import androidx.compose.material.icons.outlined.Sensors
import androidx.compose.material.icons.outlined.StarOutline
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
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
import androidx.compose.runtime.setValue
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.ContextCompat
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.repeatOnLifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import kotlinx.coroutines.launch
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import com.google.firebase.FirebaseApp

/**
 * Settings, in the sections iOS uses and the order it uses them.
 *
 * The one addition is the device bridge: an Android companion can drive the
 * handset itself, which has no iOS counterpart, so it gets the slot iOS spends
 * on its desktop gateway pairing.
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
        SettingsPage.Connection -> return MobileConnectionSettingsPage(mobilePairing)
        SettingsPage.HowToUse -> return HowToUseSettingsPage()
        SettingsPage.Roadmap -> return RoadmapSettingsPage()
        SettingsPage.Keyboard -> return MagicanKeyboardSettingsPage()
        SettingsPage.ProtectedApps -> return ProtectedAppsSettingsPage()
        SettingsPage.Root -> Unit
    }
    val context = LocalContext.current
    val remoteDeliveryPackaged = remember(context) { FirebaseApp.getApps(context).isNotEmpty() }
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
        mutableStateOf(
            Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
                ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) ==
                PackageManager.PERMISSION_GRANTED,
        )
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
    ) { granted -> notificationsGranted = granted }

    val repository = remember(context) { ChatRepository(context) }
    DisposableEffect(repository) { onDispose(repository::close) }
    // Three services, as iOS lists them. Magicutor and DesktopProxy answer on
    // their own paths behind the same host.
    var health by remember { mutableStateOf<ServiceHealth?>(null) }
    var magicutor by remember { mutableStateOf<ServiceHealth?>(null) }
    var desktop by remember { mutableStateOf<ServiceHealth?>(null) }
    var supervisorVersion by remember { mutableStateOf<String?>(null) }
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
                notificationsGranted = Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
                    ContextCompat.checkSelfPermission(
                        context,
                        Manifest.permission.POST_NOTIFICATIONS,
                    ) == PackageManager.PERMISSION_GRANTED
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

    val store = remember(context) { ThemeStore.get(context) }
    val systemIsDark = isSystemInDarkTheme()
    // Read from the store, never from a local copy. Writing is what changes the
    // theme; the activity is watching the same values and applies them.
    val familyId by store.familyId.collectAsStateWithLifecycle()
    val mode by store.mode.collectAsStateWithLifecycle()
    var showThemeSheet by remember { mutableStateOf(false) }

    if (showThemeSheet) {
        ThemePickerSheet(
            selected = familyId,
            mode = mode,
            systemIsDark = systemIsDark,
            onDismiss = { showThemeSheet = false },
            onPick = { family ->
                store.setFamily(family.id)
                applyTheme(family.id, mode, systemIsDark)
                // Left open on purpose: the whole screen repaints underneath,
                // so the sheet is where you see what you just chose. Closing on
                // tap would show the result only after the thing that caused it
                // had gone.
            },
        )
    }

    Column(
        Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(20.dp),
    ) {
        SettingsSection("Connection") {
            NavRow(
                Icons.Outlined.CloudQueue,
                if (MagicianAccess.isConfigured(context)) "Connected to Magician" else "Connect to Magician",
            ) { onOpenPage(SettingsPage.Connection) }
            if (MagicianAccess.isConfigured(context)) {
                RowDivider()
                ValueRow("Magician", MagicianAccess.baseUrlLabel(context))
            }
        }

        SettingsSection("Guide") {
            NavRow(Icons.AutoMirrored.Outlined.HelpOutline, "How to Use") { onOpenPage(SettingsPage.HowToUse) }
            RowDivider()
            NavRow(Icons.Outlined.StarOutline, "Features & Roadmap") { onOpenPage(SettingsPage.Roadmap) }
            RowDivider()
            NavRow(Icons.Outlined.Keyboard, "${ProductIdentity.productName} Keyboard") { onOpenPage(SettingsPage.Keyboard) }
        }

        // iOS's arm control: one button whose label is the state, not a switch.
        // Arming is an action with a consequence — the microphone opens — and a
        // switch invites the flick that a button does not.
        val assistantLabel = wakeIdentity?.name?.takeIf(String::isNotBlank) ?: "your assistant"
        SettingsSection("Talk to $assistantLabel") {
            NavRow(
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
            RowDivider()
            // The phrase readout. Naming what it wakes on is the difference
            // between a microphone you armed and one you are guessing about.
            ValueRow(
                "Wakes on",
                wakePhrases.takeIf { it.isNotEmpty() }
                    ?.joinToString(" or ") { "“$it”" }
                    ?: "Waiting for primary agent sync",
            )
        }
        (permissionProblem ?: wakeProblem ?: wakeIdentityProblem)?.let { problem ->
            Text(problem, color = Danger, fontSize = 11.sp, lineHeight = 15.sp)
        }

        SettingsSection("Talk behavior") {
            Label("Stay available for")
            AmbientLeash.entries.forEach { option ->
                RowDivider()
                SelectRow(option.label, selected = leash == option) { prefs.setLeash(option) }
            }
            RowDivider()
            SwitchRow(
                icon = Icons.AutoMirrored.Outlined.ScreenShare,
                title = "Also capture the screen",
                subtitle = "Sends screen keyframes into the meeting thread beside the audio. " +
                    "Off unless you turn it on.",
                checked = observeScreen,
            ) {
                prefs.setObserveScreen(!observeScreen)
            }
            RowDivider()
            Label("Conversation mode")
            AmbientVoiceMode.entries.filter(AmbientVoiceMode::built).forEach { option ->
                RowDivider()
                SelectRow(
                    option.label,
                    selected = ambientMode == option,
                    enabled = true,
                ) { prefs.setAmbientMode(option) }
            }
            RowDivider()
            SwitchRow(
                icon = Icons.AutoMirrored.Outlined.VolumeUp,
                title = "Speak replies",
                subtitle = "Read answers aloud when you asked by voice.",
                checked = speakReplies,
            ) {
                prefs.setSpeakReplies(!speakReplies)
            }
        }
        Text(
            "${leash.detail} ${ambientMode.detail}",
            color = Muted, fontSize = 11.sp, lineHeight = 15.sp,
        )

        SettingsSection("At a glance") {
            InfoRow(Icons.Outlined.Widgets, "Home Screen widget", "Add from your launcher’s Widgets menu")
            RowDivider()
            InfoRow(
                Icons.Outlined.CloudQueue,
                "Remote delivery",
                when {
                    !MagicianAccess.isConfigured(context) -> "Connect to Magician first"
                    !remoteDeliveryPackaged -> "Polling only"
                    else -> "Packaged · host-dependent"
                },
            )
            RowDivider()
            if (notificationsGranted) {
                InfoRow(Icons.Outlined.NotificationsNone, "Alerts and task cards", "Allowed")
            } else {
                NavRow(Icons.Outlined.NotificationsNone, "Allow alerts and task cards") {
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                        requestNotifications.launch(Manifest.permission.POST_NOTIFICATIONS)
                    } else {
                        notificationsGranted = true
                    }
                }
            }
        }
        Text(
            "The widget keeps its last truthful Today snapshot. Firebase adds background Attention and followed-task updates when this deployment includes it.",
            color = Muted, fontSize = 11.sp, lineHeight = 15.sp,
        )

        SettingsSection("Services") {
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

        StorageMaintenanceSection(repository)

        SettingsSection("Privacy") {
            NavRow(
                Icons.Outlined.Lock,
                "Protected apps",
                onClick = { onOpenPage(SettingsPage.ProtectedApps) },
            )
        }

        // Keep this structurally identical to iOS: one theme-family row, one
        // compact appearance control, then the explanatory copy. The palette
        // preview is an intentional parity improvement shared by both clients.
        SettingsSection("Appearance") {
            Column {
                // One row instead of eleven. A stacked list of names asked the
                // owner to know what "Risograph" looks like before choosing it,
                // and the answer was only available by picking it and looking.
                ThemeRow(
                    current = Themes.families.firstOrNull { it.id == familyId },
                    mode = mode,
                    systemIsDark = systemIsDark,
                    onClick = { showThemeSheet = true },
                )
                RowDivider()
                AppearanceModePicker(
                    current = mode,
                    onSelect = { entry ->
                        store.setMode(entry)
                        applyTheme(familyId, entry, systemIsDark)
                    },
                )
                RowDivider()
                Text(
                    "System follows your device's Day/Night. The ${ProductIdentity.productName} keyboard always follows the device appearance, regardless of this.",
                    color = Muted,
                    fontSize = 11.sp,
                    lineHeight = 15.sp,
                    modifier = Modifier.padding(horizontal = 12.dp, vertical = 9.dp),
                )
            }
        }

        SettingsSection("Keyboard") {
            NavRow(Icons.Outlined.DeleteOutline, "Reset learned words") {
                showResetLearned = true
            }
            if (learnedCleared) {
                RowDivider()
                ValueRow("Learned vocabulary", "cleared")
            }
        }
        Text(
            "Forget words learned by ${ProductIdentity.productName} Keyboard. Custom Write, Ask, and Act skills are kept.",
            color = Muted, fontSize = 11.sp, lineHeight = 15.sp,
        )

        SettingsSection("About") {
            ValueRow(ProductIdentity.productName, "v${BuildConfig.VERSION_NAME}")
            RowDivider()
            ValueRow("Magician backend", health?.version ?: "—")
            // Shown only when the endpoint reported one. A row reading "—" for a
            // service that is simply not deployed says something is wrong with
            // it, which is a different claim from saying nothing.
            magicutor?.version?.takeIf { it.isNotBlank() }?.let {
                RowDivider(); ValueRow("Magicutor", it)
            }
            supervisorVersion?.takeIf { it.isNotBlank() }?.let {
                RowDivider(); ValueRow("Supervisor", it)
            }
            desktop?.version?.takeIf { it.isNotBlank() }?.let {
                RowDivider(); ValueRow("DesktopProxy", it)
            }
            RowDivider()
            ValueRow("Scope", "${MagicianAccess.principal(context)} / ${MagicianAccess.workspace(context)}")
        }

        Spacer(Modifier.height(8.dp))
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
/**
 * What a theme looks like, in one chip.
 *
 * Drawn from the real [Palette] rather than a hand-listed preview triple. The
 * web keeps a separate `preview: {bg, accent, text}` per theme, which is a
 * second copy of the same fact and drifts the first time a palette is tuned
 * and the preview is not. Here the swatch cannot disagree with the theme,
 * because it *is* the theme.
 *
 * Background, accent and text: the three that decide whether something reads
 * as warm paper or a terminal.
 */
@Composable
private fun ThemeSwatch(palette: Palette, modifier: Modifier = Modifier) {
    Row(
        modifier
            .size(width = 46.dp, height = 28.dp)
            .background(palette.background, RoundedCornerShape(7.dp))
            .border(1.dp, palette.accent.copy(alpha = 0.55f), RoundedCornerShape(7.dp))
            .padding(horizontal = 6.dp),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(
            Modifier
                .size(width = 12.dp, height = 4.dp)
                .background(palette.accent, RoundedCornerShape(2.dp)),
        )
        Box(
            Modifier
                .size(width = 18.dp, height = 4.dp)
                .background(palette.text.copy(alpha = 0.7f), RoundedCornerShape(2.dp)),
        )
    }
}

/**
 * The variant that would actually be applied right now.
 *
 * A family holds a day and a night palette, and previewing the wrong one is
 * worse than not previewing: it promises paper and delivers a dark terminal.
 */
private fun previewPalette(
    family: ThemeFamily,
    mode: AppearanceMode,
    systemIsDark: Boolean,
): Palette = Themes.palette(Themes.resolve(family.id, mode, systemIsDark))

/** The collapsed row: what is chosen, and what it looks like. */
@Composable
private fun ThemeRow(
    current: ThemeFamily?,
    mode: AppearanceMode,
    systemIsDark: Boolean,
    onClick: () -> Unit,
) {
    Row(
        Modifier
            .fillMaxWidth()
            .clickable { onClick() }
            .padding(horizontal = 12.dp, vertical = 11.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text("Dashboard Theme", color = Ink, fontSize = 14.sp, modifier = Modifier.weight(1f))
        Text(
            current?.name ?: "Theme",
            color = Secondary,
            fontSize = 13.sp,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.widthIn(max = 110.dp).padding(end = 10.dp),
        )
        current?.let { ThemeSwatch(previewPalette(it, mode, systemIsDark)) }
        Icon(
            Icons.Outlined.ChevronRight,
            contentDescription = null,
            tint = Muted,
            modifier = Modifier.padding(start = 6.dp).size(16.dp),
        )
    }
}

/**
 * Every theme, with a look at each.
 *
 * A sheet rather than the web's dropdown: eleven rows carrying a swatch need
 * room, and a sheet is the surface Android already uses for a list of choices.
 * The idea borrowed from the web is the preview, not the chrome.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ThemePickerSheet(
    selected: String,
    mode: AppearanceMode,
    systemIsDark: Boolean,
    onDismiss: () -> Unit,
    onPick: (ThemeFamily) -> Unit,
) {
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = Ground,
    ) {
        Column(
            Modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 18.dp)
                .padding(bottom = 30.dp),
        ) {
            Text("Theme", color = Ink, fontSize = 19.sp, fontWeight = FontWeight.Bold)
            Text(
                if (mode == AppearanceMode.System) {
                    "Showing the variant your device is asking for."
                } else {
                    "Showing the ${mode.label.lowercase()} variant."
                },
                color = Muted,
                fontSize = 12.sp,
                modifier = Modifier.padding(top = 3.dp, bottom = 12.dp),
            )
            Themes.families.forEach { family ->
                val isSelected = family.id == selected
                Row(
                    Modifier
                        .fillMaxWidth()
                        .background(
                            if (isSelected) Coral.copy(alpha = 0.10f) else Color.Transparent,
                            RoundedCornerShape(10.dp),
                        )
                        .clickable { onPick(family) }
                        .padding(horizontal = 10.dp, vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    ThemeSwatch(previewPalette(family, mode, systemIsDark))
                    Text(
                        family.name,
                        color = Ink,
                        fontSize = 15.sp,
                        fontWeight = if (isSelected) FontWeight.SemiBold else FontWeight.Normal,
                        modifier = Modifier.weight(1f).padding(start = 12.dp),
                    )
                    if (isSelected) {
                        Icon(
                            Icons.Filled.CheckCircle,
                            contentDescription = "Selected",
                            tint = Coral,
                            modifier = Modifier.size(18.dp),
                        )
                    }
                }
            }
        }
    }
}

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

/**
 * The app-wide appearance choice is one compact control, matching iOS.
 *
 * These are three variants of one setting, not three independent Settings
 * rows. Keeping them on one line makes that relationship visible and also
 * prevents appearance from consuming most of the section on a phone.
 */
@Composable
private fun AppearanceModePicker(
    current: AppearanceMode,
    onSelect: (AppearanceMode) -> Unit,
) {
    val shape = RoundedCornerShape(8.dp)
    Row(
        Modifier
            .fillMaxWidth()
            .padding(horizontal = 12.dp, vertical = 7.dp)
            .clip(shape)
            .border(1.dp, BorderSoft, shape)
            .selectableGroup(),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        AppearanceMode.entries.forEachIndexed { index, entry ->
            Box(
                Modifier
                    .weight(1f)
                    .background(if (current == entry) Coral.copy(alpha = 0.14f) else Color.Transparent)
                    .selectable(
                        selected = current == entry,
                        role = Role.RadioButton,
                        onClick = { onSelect(entry) },
                    )
                    .padding(vertical = 6.dp),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    entry.label,
                    color = if (current == entry) Coral else Secondary,
                    fontSize = 12.sp,
                    fontWeight = if (current == entry) FontWeight.SemiBold else FontWeight.Normal,
                )
            }
            if (index != AppearanceMode.entries.lastIndex) {
                Box(Modifier.width(1.dp).height(28.dp).background(BorderSoft))
            }
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

@Composable
private fun SettingsSection(title: String, content: @Composable () -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(title, color = Secondary, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
        Surface(color = Control, shape = RoundedCornerShape(8.dp), border = BorderStroke(1.dp, BorderSoft)) {
            Column { content() }
        }
    }
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
            colors = SwitchDefaults.colors(checkedThumbColor = Color.White, checkedTrackColor = Coral),
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


@Composable
private fun StorageMaintenanceSection(repository: ChatRepository) {
    var rows by remember { mutableStateOf(emptyList<ai.magicbeans.magdroid.chat.StorageMaintenanceStatus>()) }
    var unavailable by remember { mutableStateOf(false) }
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    LaunchedEffect(repository, lifecycle) {
        lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) {
            while (isActive) {
                try {
                    rows = repository.storageMaintenance()
                    unavailable = false
                } catch (error: kotlinx.coroutines.CancellationException) {
                    throw error
                } catch (_: Exception) {
                    rows = emptyList(); unavailable = true
                }
                delay(10_000)
            }
        }
    }
    SettingsSection("Automatic maintenance") {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            if (unavailable) Text("Maintenance status is unavailable. Retrying automatically.", color = Muted, fontSize = 12.sp)
            rows.forEach { row ->
                Text("${if (row.database == "channel_assist") "Comms intelligence" else "Feed"} · ${row.state}", color = Ink, fontSize = 13.sp)
                Text(row.message, color = Muted, fontSize = 12.sp)
                row.last_success_at_ms?.let { completed ->
                    Text("Last completed ${java.text.DateFormat.getDateTimeInstance().format(java.util.Date(completed))}", color = Muted, fontSize = 11.sp)
                }
            }
            Text("Storage optimization runs while the service stays online. Related requests may briefly wait.", color = Muted, fontSize = 11.sp)
        }
    }
}
