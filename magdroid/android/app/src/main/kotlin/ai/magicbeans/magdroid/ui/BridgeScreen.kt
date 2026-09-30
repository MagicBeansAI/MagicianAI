package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.access.DeviceEnrollmentClient
import ai.magicbeans.magdroid.access.DeviceEnrollmentLink
import ai.magicbeans.magdroid.access.DeviceEnrollmentLinks
import ai.magicbeans.magdroid.access.DeviceConnectionMode
import ai.magicbeans.magdroid.bridge.BridgeLog
import ai.magicbeans.magdroid.log.CommandLog
import ai.magicbeans.magdroid.service.MagdroidAccessibilityService
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
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
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.DeleteOutline
import androidx.compose.material.icons.outlined.ExpandLess
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.Pause
import androidx.compose.material.icons.outlined.PlayArrow
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.ContextCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import androidx.compose.runtime.rememberCoroutineScope

/** App Pilot has one operational surface and one diagnostic timeline. */
internal enum class AppPilotTab(val label: String) {
    Status("Status"),
    Activity("Activity"),
}

/** The app has two deliberately separate device credentials. */
internal enum class PairingPurpose {
    MobileAccess,
    AppAutomation,
}

internal fun pairingPurposeMismatch(
    purpose: PairingPurpose,
    appsAutomation: Boolean,
): String? = when {
    purpose == PairingPurpose.MobileAccess && appsAutomation ->
        "This is an App Pilot enrollment code. Scan it from App Pilot."
    purpose == PairingPurpose.AppAutomation && !appsAutomation ->
        "This is a mobile connection code. Scan it from Settings → Connection."
    else -> null
}

/** Stable capability inventory used by both the UI and its parity tests. */
internal enum class AppPilotGrantKind {
    Accessibility,
    NotificationAccess,
    Notifications,
    Battery,
    ScreenCapture,
    Microphone,
    Assistant,
    Overlay,
}

internal val APP_PILOT_GRANT_KINDS = AppPilotGrantKind.entries.toList()

/** Stable partition: unresolved grants first, original order within each group. */
internal fun <T> prioritizeMissing(items: List<T>, granted: (T) -> Boolean): List<T> =
    items.sortedBy(granted)

private const val PILOT_PREFS = "magdroid_prefs"
private const val PILOT_ENABLED = "nb_enabled"

/**
 * App Pilot's canonical, themed control surface.
 *
 * This is the legacy Status / Setup / Logs feature set rebuilt in Compose. The
 * old Setup destination is folded into Status as state-aware accordions, while
 * Activity remains a distinct diagnostic timeline. App Pilot owns automation
 * enrollment and bridge settings; general Settings owns the separate mobile
 * connection used by chat, tasks, notes, and voice.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun AppPilotScreen(
    onBack: () -> Unit,
    pendingEnrollmentUri: String? = null,
    onEnrollmentConsumed: () -> Unit = {},
    pairingScanIssue: PairingScanIssue? = null,
    onPairingScanIssueConsumed: () -> Unit = {},
    onScanPairing: () -> Unit = {},
) {
    val context = LocalContext.current
    var tab by remember { mutableStateOf(AppPilotTab.Status) }
    var revision by remember { mutableStateOf(0) }
    val owner = LocalLifecycleOwner.current

    DisposableEffect(owner) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_RESUME) {
                MagdroidAccessibilityService.instance?.tryConsumeMediaProjectionConsent()
                revision += 1
            }
        }
        owner.lifecycle.addObserver(observer)
        onDispose { owner.lifecycle.removeObserver(observer) }
    }
    LaunchedEffect(Unit) {
        while (isActive) {
            delay(1_000)
            revision += 1
        }
    }

    Scaffold(
        containerColor = Ground,
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text("App Pilot", color = Ink, fontSize = 18.sp, fontWeight = FontWeight.SemiBold)
                        Text("Device automation", color = Muted, fontSize = 11.sp)
                    }
                },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Outlined.ArrowBack, "Back", tint = Ink)
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = Ground),
            )
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            PilotTabBar(tab, onSelect = { tab = it })
            when (tab) {
                AppPilotTab.Status -> StatusTab(
                    context = context,
                    revision = revision,
                    onRefresh = { revision += 1 },
                    pendingEnrollmentUri = pendingEnrollmentUri,
                    onEnrollmentConsumed = onEnrollmentConsumed,
                    pairingScanIssue = pairingScanIssue,
                    onPairingScanIssueConsumed = onPairingScanIssueConsumed,
                    onScanPairing = onScanPairing,
                )
                AppPilotTab.Activity -> ActivityTab()
            }
        }
    }
}

@Composable
private fun PilotTabBar(selected: AppPilotTab, onSelect: (AppPilotTab) -> Unit) {
    Surface(color = Panel, border = BorderStroke(0.dp, BorderSoft)) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            AppPilotTab.entries.forEach { tab ->
                val active = selected == tab
                Surface(
                    modifier = Modifier.weight(1f).clickable { onSelect(tab) },
                    color = if (active) Coral.copy(alpha = 0.14f) else activePalette.control,
                    shape = RoundedCornerShape(10.dp),
                    border = BorderStroke(1.dp, if (active) Coral.copy(alpha = 0.45f) else BorderSoft),
                ) {
                    Text(
                        tab.label,
                        color = if (active) Coral else Secondary,
                        fontSize = 13.sp,
                        fontWeight = if (active) FontWeight.SemiBold else FontWeight.Normal,
                        modifier = Modifier.padding(vertical = 9.dp),
                        textAlign = androidx.compose.ui.text.style.TextAlign.Center,
                    )
                }
            }
        }
    }
}

private data class PilotStatus(
    val enabled: Boolean,
    val accessibility: Boolean,
    val mobilePaired: Boolean,
    val automationEnrolled: Boolean,
    val bridgeConnected: Boolean,
    val bridgeIssue: String?,
    val screenshotFast: Boolean,
    val endpoint: String,
    val stats: CommandLog.Stats,
)

internal fun magicianMcpStatus(
    enabled: Boolean,
    mobilePaired: Boolean,
    automationEnrolled: Boolean,
    bridgeConnected: Boolean,
    bridgeIssue: String? = null,
): String = when {
    bridgeConnected -> "Running"
    !enabled -> "Disabled"
    !mobilePaired -> "Not paired"
    !automationEnrolled -> "Needs setup"
    bridgeIssue?.contains("APK no longer matches its Apps enrollment") == true -> "Re-enroll"
    bridgeIssue != null -> "Retrying"
    else -> "Connecting"
}

private fun readStatus(context: Context): PilotStatus {
    val service = MagdroidAccessibilityService.instance
    return PilotStatus(
        enabled = context.getSharedPreferences(PILOT_PREFS, Context.MODE_PRIVATE)
            .getBoolean(PILOT_ENABLED, false),
        accessibility = accessibilityEnabled(context),
        mobilePaired = MagicianAccess.isConfigured(context),
        automationEnrolled = MagicianAccess.hasAutomationEnrollment(context),
        bridgeConnected = service?.isBridgeConnected() == true,
        bridgeIssue = BridgeLog.lines.value.lastOrNull {
            it.tag == "MagdroidBridge" && it.level != BridgeLog.Level.Info
        }?.message,
        screenshotFast = service?.hasMediaProjectionPermission() == true,
        endpoint = MagicianAccess.baseUrlLabel(context),
        stats = CommandLog.getPerformanceStats(),
    )
}

@Composable
private fun StatusTab(
    context: Context,
    revision: Int,
    onRefresh: () -> Unit,
    pendingEnrollmentUri: String?,
    onEnrollmentConsumed: () -> Unit,
    pairingScanIssue: PairingScanIssue?,
    onPairingScanIssueConsumed: () -> Unit,
    onScanPairing: () -> Unit,
) {
    val status = remember(revision) { readStatus(context) }
    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Surface(
            color = activePalette.soft,
            shape = RoundedCornerShape(16.dp),
            border = BorderStroke(1.dp, if (status.bridgeConnected) Teal.copy(alpha = 0.45f) else BorderSoft),
        ) {
            Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Box(
                        Modifier.size(10.dp).background(
                            when {
                                !status.enabled -> Muted
                                status.bridgeConnected -> Teal
                                else -> activePalette.warning
                            },
                            CircleShape,
                        ),
                    )
                    Spacer(Modifier.size(10.dp))
                    Column(Modifier.weight(1f)) {
                        Text(
                            when {
                                !status.enabled -> "App Pilot is off"
                                status.bridgeConnected -> "Connected to Magician"
                                !status.accessibility -> "Setup incomplete"
                                !status.mobilePaired -> "Connect to Magician in Settings"
                                !status.automationEnrolled -> "App Pilot enrollment needed"
                                status.bridgeIssue?.contains("APK no longer matches its Apps enrollment") == true ->
                                    "Replace App Pilot enrollment"
                                else -> "Ready — waiting for Magician"
                            },
                            color = Ink,
                            fontSize = 17.sp,
                            fontWeight = FontWeight.SemiBold,
                        )
                        Text(
                            when {
                                status.bridgeConnected -> status.endpoint
                                status.bridgeIssue != null -> status.bridgeIssue
                                else -> "The phone dials out; no public port is opened."
                            },
                            color = Muted,
                            fontSize = 11.sp,
                            lineHeight = 15.sp,
                        )
                    }
                    Switch(
                        checked = status.enabled,
                        onCheckedChange = { enabled ->
                            context.getSharedPreferences(PILOT_PREFS, Context.MODE_PRIVATE)
                                .edit().putBoolean(PILOT_ENABLED, enabled).apply()
                            if (enabled) MagdroidAccessibilityService.instance?.enable()
                            else MagdroidAccessibilityService.instance?.disable()
                        },
                        colors = pilotSwitchColors(),
                    )
                }
            }
        }

        DevicePairingSetup(
            context = context,
            purpose = PairingPurpose.AppAutomation,
            pendingEnrollmentUri = pendingEnrollmentUri,
            onEnrollmentConsumed = onEnrollmentConsumed,
            pairingScanIssue = pairingScanIssue,
            onPairingScanIssueConsumed = onPairingScanIssueConsumed,
            onScanPairing = onScanPairing,
            onPaired = onRefresh,
        )

        SetupAccordions(
            context = context,
            status = status,
            revision = revision,
            onRefresh = onRefresh,
            permissionsOnly = true,
        )

        Section("System") {
            Row(
                Modifier.fillMaxWidth().padding(10.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                StatusMetric("Accessibility", if (status.accessibility) "Active" else "Off", status.accessibility, Modifier.weight(1f))
                StatusMetric(
                    "Magician MCP",
                    magicianMcpStatus(
                        status.enabled,
                        status.mobilePaired,
                        status.automationEnrolled,
                        status.bridgeConnected,
                        status.bridgeIssue,
                    ),
                    status.bridgeConnected,
                    Modifier.weight(1f),
                )
                StatusMetric("Screenshots", if (status.screenshotFast) "Fast" else "Slow", status.screenshotFast, Modifier.weight(1f))
            }
        }

        Section("Performance") {
            Row(
                Modifier.fillMaxWidth().padding(10.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                PerfMetric("p50", status.stats.p50, Modifier.weight(1f))
                PerfMetric("p95", status.stats.p95, Modifier.weight(1f))
                PerfMetric("p99", status.stats.p99, Modifier.weight(1f))
                Column(Modifier.weight(1f), horizontalAlignment = Alignment.CenterHorizontally) {
                    Text(status.stats.count.toString(), color = Ink, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
                    Text("Measured", color = Muted, fontSize = 9.sp)
                }
            }
            Text(
                "Measures command execution only; UI stabilization waits remain in Activity but do not inflate latency.",
                color = Muted,
                fontSize = 10.sp,
                lineHeight = 14.sp,
                modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
            )
        }

        SetupAccordions(
            context = context,
            status = status,
            revision = revision,
            onRefresh = onRefresh,
            permissionsOnly = false,
        )
        Spacer(Modifier.height(8.dp))
    }
}

@Composable
internal fun DevicePairingSetup(
    context: Context,
    purpose: PairingPurpose,
    pendingEnrollmentUri: String?,
    onEnrollmentConsumed: () -> Unit,
    pairingScanIssue: PairingScanIssue?,
    onPairingScanIssueConsumed: () -> Unit,
    onScanPairing: () -> Unit,
    onPaired: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    var candidate by remember { mutableStateOf<DeviceEnrollmentLink?>(null) }
    var message by rememberSaveable { mutableStateOf("") }
    var messageKind by rememberSaveable { mutableStateOf(PairingMessageKind.Info) }
    var pairing by remember { mutableStateOf(false) }
    var replacingPairing by rememberSaveable { mutableStateOf(false) }
    var cameraPermissionBlocked by rememberSaveable { mutableStateOf(false) }
    var selectedConnectionMode by rememberSaveable {
        mutableStateOf(
            if (DeviceEnrollmentLinks.isSameWifiOrigin(MagicianAccess.baseUrl(context))) {
                DeviceConnectionMode.SameWifi
            } else {
                DeviceConnectionMode.Remote
            },
        )
    }
    val paired = when (purpose) {
        PairingPurpose.MobileAccess -> MagicianAccess.isConfigured(context)
        PairingPurpose.AppAutomation -> MagicianAccess.hasAutomationEnrollment(context)
    }
    LaunchedEffect(purpose) {
        if (purpose != PairingPurpose.AppAutomation) return@LaunchedEffect
        if (pendingEnrollmentUri == null && candidate == null) {
            val retained = runCatching {
                DeviceEnrollmentClient(context).pendingAppsEnrollmentLink()
            }.getOrElse { error ->
                message = error.message ?: "The retained Apps enrollment could not be opened."
                messageKind = PairingMessageKind.Error
                null
            }
            if (retained != null) {
                candidate = retained
                message = "Resuming the exact Apps enrollment awaiting desktop owner approval."
                messageKind = PairingMessageKind.Info
                replacingPairing = true
            }
        }
    }

    LaunchedEffect(pendingEnrollmentUri) {
        val raw = pendingEnrollmentUri ?: return@LaunchedEffect
        val parsed = DeviceEnrollmentLinks.parse(raw)
        if (parsed == null) {
            candidate = null
            message = "That QR code is not a valid Magician pairing code."
            messageKind = PairingMessageKind.Error
            onEnrollmentConsumed()
        } else if (pairingPurposeMismatch(purpose, parsed.appsAutomation) != null) {
            candidate = null
            message = requireNotNull(pairingPurposeMismatch(purpose, parsed.appsAutomation))
            messageKind = PairingMessageKind.Error
            onEnrollmentConsumed()
        } else if (
            purpose == PairingPurpose.MobileAccess &&
            parsed.connectionMode != selectedConnectionMode
        ) {
            candidate = null
            message = "That is a ${parsed.connectionMode.label} code. Choose ${parsed.connectionMode.label} on this phone, or create a ${selectedConnectionMode.label} code on the computer, then scan again."
            messageKind = PairingMessageKind.Error
            onEnrollmentConsumed()
        } else {
            val retained = if (purpose == PairingPurpose.AppAutomation) {
                runCatching { DeviceEnrollmentClient(context).pendingAppsEnrollmentLink() }
                    .getOrElse { error ->
                        candidate = null
                        message = error.message ?: "The retained Apps enrollment could not be opened."
                        messageKind = PairingMessageKind.Error
                        onEnrollmentConsumed()
                        return@LaunchedEffect
                    }
            } else {
                null
            }
            if (shouldResumeRetainedAppsEnrollment(parsed, retained)) {
                candidate = retained
                message = "Finish the previous App Pilot enrollment first. Confirm and pair once to reconcile it, then create and scan a fresh QR."
                messageKind = PairingMessageKind.Info
                replacingPairing = true
                onEnrollmentConsumed()
            } else {
                candidate = parsed
                message = ""
                cameraPermissionBlocked = false
            }
        }
    }

    LaunchedEffect(pairingScanIssue) {
        when (pairingScanIssue ?: return@LaunchedEffect) {
            PairingScanIssue.CameraPermissionDenied -> {
                replacingPairing = true
                cameraPermissionBlocked = true
                message = "Camera access is needed only while scanning a pairing code. Allow it in Android settings, then scan again."
                messageKind = PairingMessageKind.Error
            }
            PairingScanIssue.Cancelled -> {
                message = "Scan cancelled. You can try again when you are ready."
                messageKind = PairingMessageKind.Info
            }
        }
        onPairingScanIssueConsumed()
    }

    LaunchedEffect(cameraPermissionBlocked, permitted(context, android.Manifest.permission.CAMERA)) {
        if (cameraPermissionBlocked && permitted(context, android.Manifest.permission.CAMERA)) {
            cameraPermissionBlocked = false
            message = "Camera access is ready. Scan the pairing code again."
            messageKind = PairingMessageKind.Info
        }
    }

    Surface(
        color = activePalette.soft,
        shape = RoundedCornerShape(16.dp),
        border = BorderStroke(1.dp, if (candidate != null) Coral.copy(alpha = 0.5f) else BorderSoft),
    ) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Text(
                if (paired && candidate == null) {
                    when (purpose) {
                        PairingPurpose.MobileAccess -> "Connected to Magician"
                        PairingPurpose.AppAutomation -> "App Pilot enrolled"
                    }
                } else {
                    when (purpose) {
                        PairingPurpose.MobileAccess -> "Connect to Magician"
                        PairingPurpose.AppAutomation -> "Enroll App Pilot"
                    }
                },
                color = Ink,
                fontSize = 15.sp,
                fontWeight = FontWeight.SemiBold,
            )
            if (candidate == null && paired && !replacingPairing) {
                Text(
                    MagicianAccess.baseUrlLabel(context),
                    color = Ink,
                    fontSize = 13.sp,
                    fontWeight = FontWeight.Medium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    when (purpose) {
                        PairingPurpose.MobileAccess ->
                            "Chat, tasks, notes, and voice use this encrypted device credential and reconnect automatically."
                        PairingPurpose.AppAutomation ->
                            "The hardware-backed App Pilot identity is enrolled and reconnects Magician MCP automatically."
                    },
                    color = Muted,
                    fontSize = 11.sp,
                    lineHeight = 15.sp,
                )
                TextButton(onClick = {
                    replacingPairing = true
                    message = ""
                }) {
                    Text(
                        if (purpose == PairingPurpose.MobileAccess) "Replace connection" else "Replace App Pilot enrollment",
                        color = Coral,
                    )
                }
            } else if (candidate == null) {
                Text(
                    when {
                        purpose == PairingPurpose.AppAutomation && paired ->
                            "Create a replacement enrollment in Magican Desktop Settings → Android App Observation, review it there, then scan its one-time QR."
                        purpose == PairingPurpose.AppAutomation ->
                            "Open Magican Desktop Settings → Android App Observation, choose Begin Attested Enrollment, review the device identity, then scan its one-time QR here."
                        paired ->
                            "Scan a new one-time code to move this phone to another Magician or replace its mobile credential."
                        else ->
                            "On the computer, open Magican Desktop Settings → Devices → Connect Android. Choose Same Wi-Fi or Remote, then scan the matching one-time code."
                    },
                    color = Muted,
                    fontSize = 11.sp,
                    lineHeight = 15.sp,
                )
                if (purpose == PairingPurpose.MobileAccess) {
                    Text("Connection route", color = Ink, fontSize = 12.sp, fontWeight = FontWeight.Medium)
                    Row(
                        Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        DeviceConnectionMode.entries.forEach { mode ->
                            val selected = selectedConnectionMode == mode
                            Surface(
                                modifier = Modifier.weight(1f).clickable(enabled = !pairing) {
                                    selectedConnectionMode = mode
                                    message = ""
                                },
                                color = if (selected) Coral.copy(alpha = 0.14f) else activePalette.control,
                                shape = RoundedCornerShape(10.dp),
                                border = BorderStroke(1.dp, if (selected) Coral.copy(alpha = 0.5f) else BorderSoft),
                            ) {
                                Text(
                                    mode.label,
                                    color = if (selected) Coral else Secondary,
                                    fontSize = 12.sp,
                                    fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
                                    modifier = Modifier.padding(vertical = 10.dp),
                                    textAlign = androidx.compose.ui.text.style.TextAlign.Center,
                                )
                            }
                        }
                    }
                    Text(
                        if (selectedConnectionMode == DeviceConnectionMode.SameWifi) {
                            "Use when this phone and the Magician computer share the same trusted Wi-Fi. Choose Same Wi-Fi on the computer too."
                        } else {
                            "Use from another network or when Magician runs remotely. Choose Remote on the computer too."
                        },
                        color = Muted,
                        fontSize = 10.sp,
                        lineHeight = 14.sp,
                    )
                }
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                    Button(shape = MagicanButtonShape,
                        onClick = {
                            message = ""
                            cameraPermissionBlocked = false
                            onScanPairing()
                        },
                        colors = ButtonDefaults.buttonColors(containerColor = Coral, contentColor = activePalette.onAccent),
                    ) {
                        Text(
                            if (purpose == PairingPurpose.MobileAccess) {
                                "Scan ${selectedConnectionMode.label} QR"
                            } else {
                                "Scan App Pilot QR"
                            },
                        )
                    }
                    if (paired) {
                        TextButton(onClick = {
                            replacingPairing = false
                            message = ""
                        }) { Text("Cancel", color = Secondary) }
                    }
                }
            } else {
                val sameWifi = candidate!!.connectionMode == DeviceConnectionMode.SameWifi
                Text(
                    if (purpose == PairingPurpose.MobileAccess) "Connect this phone to" else "Enroll App Pilot with",
                    color = Muted,
                    fontSize = 11.sp,
                )
                Text(
                    if (sameWifi) "Same Wi-Fi · this computer" else "Remote · works anywhere",
                    color = Coral,
                    fontSize = 11.sp,
                    fontWeight = FontWeight.SemiBold,
                )
                Text(
                    candidate!!.baseUrl,
                    color = Ink,
                    fontSize = 13.sp,
                    fontWeight = FontWeight.Medium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    if (sameWifi) {
                        "Choose this when the phone and computer are on the same trusted Wi-Fi. The private address is the computer; localhost would mean this phone."
                    } else {
                        "Choose this when the phone is away from the computer or on another network. It connects through connect.magican.ai."
                    },
                    color = Muted,
                    fontSize = 10.sp,
                    lineHeight = 14.sp,
                )
                Text(
                    "The code is single-use and expires after five minutes. Confirm only if this is the Magician you opened.",
                    color = Muted,
                    fontSize = 10.sp,
                    lineHeight = 14.sp,
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(shape = MagicanButtonShape,
                        enabled = !pairing,
                        onClick = {
                            val link = candidate ?: return@Button
                            pairing = true
                            message = "Pairing…"
                            messageKind = PairingMessageKind.Info
                            scope.launch {
                                try {
                                    val result = DeviceEnrollmentClient(context).exchange(link) { progress ->
                                        message = progress
                                    }
                                    MagicianAccess.saveEnrollment(
                                        context,
                                        result.baseUrl,
                                        result.principal,
                                        result.workspace,
                                        result.token,
                                        result.cloudflareClientId,
                                        result.cloudflareClientSecret,
                                        result.automationKeyAlias,
                                        result.automationKeyId,
                                        result.automationApkSha256,
                                        result.automationAttestationPolicyDigest,
                                        result.automationPlayIntegrityCloudProjectNumber,
                                        result.automationTrustMode,
                                        result.pendingAppsEnrollmentId,
                                        result.pendingAppsRequestSha256,
                                    )
                                    candidate = null
                                    onEnrollmentConsumed()
                                    message = if (link.appsAutomation) {
                                        "App Pilot enrolled. Connection status appears above."
                                    } else {
                                        "Mobile connection ready for chat, tasks, notes, and voice."
                                    }
                                    messageKind = PairingMessageKind.Success
                                    replacingPairing = false
                                    if (link.appsAutomation) BridgeLog.clear()
                                    MagdroidAccessibilityService.instance?.restartMagicianBridge()
                                    ai.magicbeans.magdroid.push.AndroidMobilePushRegistration.ensure(context)
                                    onPaired()
                                } catch (cancelled: CancellationException) {
                                    throw cancelled
                                } catch (error: Throwable) {
                                    message = error.message ?: "Pairing failed."
                                    messageKind = PairingMessageKind.Error
                                } finally {
                                    pairing = false
                                }
                            }
                        },
                        colors = ButtonDefaults.buttonColors(containerColor = Coral, contentColor = activePalette.onAccent),
                    ) { Text(if (pairing) "Pairing…" else "Confirm and pair") }
                    TextButton(
                        enabled = !pairing,
                        onClick = {
                            candidate = null
                            message = ""
                            onEnrollmentConsumed()
                        },
                    ) { Text("Cancel", color = Secondary) }
                }
            }
            if (message.isNotBlank()) {
                PairingMessage(message, messageKind)
            }
            if (cameraPermissionBlocked) {
                TextButton(onClick = { openAppSettings(context) }) {
                    Text("Open app settings", color = Coral)
                }
            }
        }
    }
}

private enum class PairingMessageKind { Info, Success, Error }

@Composable
private fun PairingMessage(message: String, kind: PairingMessageKind) {
    val color = when (kind) {
        PairingMessageKind.Info -> Secondary
        PairingMessageKind.Success -> Teal
        PairingMessageKind.Error -> Danger
    }
    Surface(
        modifier = Modifier.fillMaxWidth().semantics {
            liveRegion = if (kind == PairingMessageKind.Error) LiveRegionMode.Assertive else LiveRegionMode.Polite
        },
        color = color.copy(alpha = 0.08f),
        border = BorderStroke(1.dp, color.copy(alpha = 0.3f)),
        shape = RoundedCornerShape(10.dp),
    ) {
        Text(
            message,
            color = color,
            fontSize = 11.sp,
            lineHeight = 15.sp,
            modifier = Modifier.padding(horizontal = 12.dp, vertical = 10.dp),
        )
    }
}

@Composable
private fun StatusMetric(label: String, value: String, healthy: Boolean, modifier: Modifier) {
    Surface(modifier, color = activePalette.control, shape = RoundedCornerShape(10.dp), border = BorderStroke(1.dp, BorderSoft)) {
        Column(Modifier.padding(horizontal = 8.dp, vertical = 10.dp), horizontalAlignment = Alignment.CenterHorizontally) {
            Box(Modifier.size(7.dp).background(if (healthy) Teal else activePalette.warning, CircleShape))
            Spacer(Modifier.height(6.dp))
            Text(value, color = Ink, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
            Text(label, color = Muted, fontSize = 9.sp, maxLines = 1)
        }
    }
}

@Composable
private fun PerfMetric(label: String, value: Int, modifier: Modifier) {
    Column(modifier, horizontalAlignment = Alignment.CenterHorizontally) {
        Text(if (value == 0) "—" else "${value}ms", color = Ink, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
        Text(label, color = Muted, fontSize = 9.sp)
    }
}

private data class Grant(
    val kind: AppPilotGrantKind,
    val title: String,
    val detail: String,
    val granted: Boolean,
    val open: (Context) -> Unit,
)

private fun readGrants(context: Context, requestPermission: (String) -> Unit): List<Grant> = listOf(
    Grant(
        AppPilotGrantKind.Accessibility,
        "Accessibility service",
        "Reads the visible UI and performs taps, swipes, typing, and navigation.",
        accessibilityEnabled(context),
    ) { it.startActivity(Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS)) },
    Grant(
        AppPilotGrantKind.NotificationAccess,
        "Notification access",
        "Reads notification content, including one-time codes requested by an active task.",
        notificationListenerEnabled(context),
    ) { it.startActivity(Intent(Settings.ACTION_NOTIFICATION_LISTENER_SETTINGS)) },
    Grant(
        AppPilotGrantKind.Notifications,
        "Notifications",
        "Shows when App Pilot, wake word, or recording is active.",
        permitted(context, android.Manifest.permission.POST_NOTIFICATIONS),
    ) { requestPermission(android.Manifest.permission.POST_NOTIFICATIONS) },
    Grant(
        AppPilotGrantKind.Battery,
        "Unrestricted battery",
        "Keeps the bridge connected while the screen is off.",
        batteryUnrestricted(context),
        ::openBatterySettings,
    ),
    Grant(
        AppPilotGrantKind.ScreenCapture,
        "Screen capture",
        "Enables fast screenshots. Without it App Pilot uses the slower accessibility fallback.",
        MagdroidAccessibilityService.instance?.hasMediaProjectionPermission() == true,
    ) {
        it.startActivity(Intent(it, ai.magicbeans.magdroid.screenshot.ScreenshotConsentActivity::class.java))
    },
    Grant(
        AppPilotGrantKind.Microphone,
        "Microphone",
        "Used for dictation, wake word, and room capture.",
        permitted(context, android.Manifest.permission.RECORD_AUDIO),
        ::openAppSettings,
    ),
    Grant(
        AppPilotGrantKind.Assistant,
        "Default assistant",
        "Lets long-press power ask Magician about the current screen.",
        isDefaultAssistant(context),
    ) { it.startActivity(Intent(Settings.ACTION_VOICE_INPUT_SETTINGS)) },
    Grant(
        AppPilotGrantKind.Overlay,
        "Draw over other apps",
        "Lets Tutor explain directly on top of the app being used.",
        TutorOverlayService.canDraw(context),
    ) { TutorOverlayService.requestPermission(it) },
)

@Composable
private fun SetupAccordions(
    context: Context,
    status: PilotStatus,
    revision: Int,
    onRefresh: () -> Unit,
    permissionsOnly: Boolean,
) {
    if (permissionsOnly) {
        val permissionLauncher = rememberLauncherForActivityResult(
            ActivityResultContracts.RequestPermission(),
        ) { onRefresh() }
        val grants = remember(revision) {
            readGrants(context) { permission ->
                if (
                    permission != android.Manifest.permission.POST_NOTIFICATIONS ||
                    Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU
                ) {
                    permissionLauncher.launch(permission)
                } else {
                    openAppSettings(context)
                }
            }.let { prioritizeMissing(it, Grant::granted) }
        }
        val granted = grants.count(Grant::granted)
        AccordionSection(
            title = "Permissions",
            summary = if (granted == grants.size) "All ${grants.size} granted" else "${grants.size - granted} need attention · $granted/${grants.size}",
            initiallyExpanded = granted < grants.size,
            forceExpanded = granted < grants.size,
            attention = granted < grants.size,
        ) {
            LinearProgressIndicator(
                progress = { if (grants.isEmpty()) 0f else granted.toFloat() / grants.size },
                modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 10.dp),
                color = Teal,
                trackColor = activePalette.control,
            )
            if (grants.isNotEmpty()) RowDivider()
            grants.forEachIndexed { index, grant ->
                if (index > 0) RowDivider()
                GrantRow(grant) { grant.open(context) }
            }
        }
        return
    }

    val host = MagicianAccess.baseUrl(context)
    var clientId by rememberSaveable { mutableStateOf(MagicianAccess.clientId(context)) }
    var clientSecret by rememberSaveable { mutableStateOf("") }
    var pairingToken by rememberSaveable { mutableStateOf("") }
    var saveMessage by rememberSaveable { mutableStateOf("") }
    val connectionStatus = when {
        status.bridgeConnected -> "Connected"
        MagicianAccess.isConfigured(context) -> "Paired"
        else -> "Not configured"
    }

    AccordionSection(
        title = "Manual connection",
        summary = "$connectionStatus · recovery only",
        initiallyExpanded = false,
        forceExpanded = saveMessage.startsWith("Enter"),
        attention = false,
    ) {
        ValueLine("Status", connectionStatus)
        RowDivider()
        ValueLine("Magician", status.endpoint)
        RowDivider()
        LabelLine("Device ID")
        RowDivider()
        CopyableValueLine(MagicianAccess.deviceId(context), "Copy device ID")
        RowDivider()
        ValueLine("Scope", "${MagicianAccess.principal(context)} / ${MagicianAccess.workspace(context)}")
        RowDivider()
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            PilotField(clientId, { clientId = it }, "Cloudflare Access client ID")
            PilotField(
                clientSecret,
                { clientSecret = it },
                if (MagicianAccess.hasClientSecret(context)) "Client secret · stored (blank keeps it)" else "Cloudflare Access client secret",
                password = true,
            )
            PilotField(pairingToken, { pairingToken = it }, "Pairing token", password = true)
            Text(
                "Recovery for the paired server only. To change servers, scan a new owner-issued connection QR above. Secrets are never read back; leaving either secret blank preserves its stored value.",
                color = Muted,
                fontSize = 10.sp,
                lineHeight = 14.sp,
            )
            Button(shape = MagicanButtonShape,
                onClick = {
                    try {
                        MagicianAccess.save(context, host.trim(), clientId.trim(), clientSecret)
                        if (pairingToken.isNotBlank()) MagicianAccess.saveBridgeToken(context, pairingToken.trim())
                        clientSecret = ""
                        pairingToken = ""
                        saveMessage = "Connection saved."
                        if (context.getSharedPreferences(PILOT_PREFS, Context.MODE_PRIVATE)
                                .getBoolean(PILOT_ENABLED, false)
                        ) {
                            MagdroidAccessibilityService.instance?.restartMagicianBridge()
                        }
                        onRefresh()
                    } catch (_: IllegalArgumentException) {
                        saveMessage = "Scan an owner-issued connection QR before saving recovery credentials."
                    }
                },
                colors = ButtonDefaults.buttonColors(containerColor = Coral, contentColor = activePalette.onAccent),
            ) { Text("Save connection") }
            if (saveMessage.isNotEmpty()) Text(saveMessage, color = Secondary, fontSize = 11.sp)
        }
    }

    Spacer(Modifier.height(16.dp))
    AccordionSection(title = "Device details", summary = "${Build.MANUFACTURER} ${Build.MODEL}") {
        val dm = context.resources.displayMetrics
        ValueLine("Model", "${Build.MANUFACTURER} ${Build.MODEL}")
        RowDivider()
        ValueLine("Android", "${Build.VERSION.RELEASE} · API ${Build.VERSION.SDK_INT}")
        RowDivider()
        ValueLine("Display", "${dm.widthPixels} × ${dm.heightPixels} · ${dm.densityDpi} dpi")
        RowDivider()
        ValueLine("Density", "${"%.2f".format(dm.density)}×")
    }

}

@Composable
private fun PilotField(
    value: String,
    onValueChange: (String) -> Unit,
    label: String,
    placeholder: String = "",
    password: Boolean = false,
) {
    MagicianTextField(
        value = value,
        onValueChange = onValueChange,
        label = { Text(label) },
        placeholder = if (placeholder.isEmpty()) null else ({ Text(placeholder) }),
        modifier = Modifier.fillMaxWidth(),
        singleLine = true,
        visualTransformation = if (password) PasswordVisualTransformation() else androidx.compose.ui.text.input.VisualTransformation.None,
    )
}

@Composable
private fun GrantRow(grant: Grant, onOpen: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().clickable(onClick = onOpen).padding(horizontal = 12.dp, vertical = 12.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(
            Icons.Outlined.CheckCircle,
            contentDescription = null,
            tint = if (grant.granted) Teal else Muted,
            modifier = Modifier.size(18.dp),
        )
        Column(Modifier.weight(1f)) {
            Text(grant.title, color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
            Text(grant.detail, color = Muted, fontSize = 11.sp, lineHeight = 15.sp)
        }
        Text(
            if (grant.granted) "On" else "Grant",
            color = if (grant.granted) Secondary else Coral,
            fontSize = 12.sp,
            fontWeight = if (grant.granted) FontWeight.Normal else FontWeight.SemiBold,
        )
    }
}

@Composable
private fun SwitchLine(
    title: String,
    detail: String,
    checked: Boolean,
    enabled: Boolean = true,
    onChange: (Boolean) -> Unit,
) {
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Column(Modifier.weight(1f)) {
            Text(title, color = if (enabled) Ink else Muted, fontSize = 14.sp)
            Text(detail, color = Muted, fontSize = 10.sp, lineHeight = 14.sp)
        }
        Switch(checked = checked, onCheckedChange = onChange, enabled = enabled, colors = pilotSwitchColors())
    }
}

@Composable
private fun ActivityTab() {
    val bridgeLog by BridgeLog.lines.collectAsStateWithLifecycle()
    val commandLog by CommandLog.entries.collectAsStateWithLifecycle()
    val latest = remember(bridgeLog, commandLog) { mergeActivity(bridgeLog, commandLog) }
    var paused by remember { mutableStateOf(false) }
    var snapshot by remember { mutableStateOf(latest) }
    var filter by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(latest, paused) {
        if (!paused) snapshot = latest
    }
    val visible = remember(snapshot, filter) {
        if (filter == null) snapshot else snapshot.filter { it.category?.name == filter }
    }

    Column(Modifier.fillMaxSize()) {
        Row(
            Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 16.dp, vertical = 10.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            FilterChip("All", filter == null) { filter = null }
            CommandLog.Category.entries.forEach { category ->
                FilterChip(category.name.lowercase().replaceFirstChar(Char::uppercase), filter == category.name) {
                    filter = category.name
                }
            }
        }
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                if (paused) "Paused at ${snapshot.size} events" else "${visible.size} recent events",
                color = Muted,
                fontSize = 11.sp,
            )
            Row {
                TextButton(onClick = { paused = !paused }) {
                    Icon(if (paused) Icons.Outlined.PlayArrow else Icons.Outlined.Pause, null, tint = Coral, modifier = Modifier.size(16.dp))
                    Spacer(Modifier.size(5.dp))
                    Text(if (paused) "Resume" else "Pause", color = Coral)
                }
                TextButton(onClick = {
                    BridgeLog.clear()
                    CommandLog.clear()
                    snapshot = emptyList()
                }) {
                    Icon(Icons.Outlined.DeleteOutline, null, tint = Danger, modifier = Modifier.size(16.dp))
                    Spacer(Modifier.size(5.dp))
                    Text("Clear", color = Danger)
                }
            }
        }
        if (visible.isEmpty()) {
            Box(Modifier.fillMaxSize().padding(32.dp), contentAlignment = Alignment.Center) {
                Text(
                    if (filter == null) "No activity yet. Connections, captures, and tool calls appear here."
                    else "No ${filter!!.lowercase()} activity in the current history.",
                    color = Muted,
                    fontSize = 12.sp,
                    lineHeight = 17.sp,
                    textAlign = androidx.compose.ui.text.style.TextAlign.Center,
                )
            }
        } else {
            Column(
                Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp, vertical = 6.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                visible.take(100).forEach { line -> LogLine(line) }
                Spacer(Modifier.height(8.dp))
            }
        }
    }
}

@Composable
private fun FilterChip(label: String, selected: Boolean, onClick: () -> Unit) {
    Surface(
        color = if (selected) Coral.copy(alpha = 0.14f) else Panel,
        shape = RoundedCornerShape(100.dp),
        border = BorderStroke(1.dp, if (selected) Coral.copy(alpha = 0.55f) else BorderSoft),
        modifier = Modifier.clickable(onClick = onClick),
    ) {
        Text(
            label,
            color = if (selected) Coral else Secondary,
            fontSize = 11.sp,
            fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
            modifier = Modifier.padding(horizontal = 12.dp, vertical = 7.dp),
        )
    }
}

internal enum class ActivityTone { Info, Warn, Error }

internal data class ActivityLine(
    val message: String,
    val source: String,
    val atMs: Long,
    val tone: ActivityTone,
    val category: CommandLog.Category? = null,
)

internal fun mergeActivity(
    bridge: List<BridgeLog.Line>,
    commands: List<CommandLog.Entry>,
): List<ActivityLine> = (
    bridge.map { line ->
        ActivityLine(
            message = line.message,
            source = line.tag,
            atMs = line.atMs,
            tone = when (line.level) {
                BridgeLog.Level.Info -> ActivityTone.Info
                BridgeLog.Level.Warn -> ActivityTone.Warn
                BridgeLog.Level.Error -> ActivityTone.Error
            },
        )
    } + commands.map { entry ->
        ActivityLine(
            message = "${entry.command} · ${entry.latencyMs}ms",
            source = entry.category.name.lowercase(),
            atMs = entry.timestamp,
            tone = if (entry.success) ActivityTone.Info else ActivityTone.Error,
            category = entry.category,
        )
    }
).sortedByDescending(ActivityLine::atMs)

@Composable
private fun LogLine(line: ActivityLine) {
    Surface(color = Panel, shape = RoundedCornerShape(10.dp), border = BorderStroke(1.dp, BorderSoft)) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 10.dp),
            horizontalArrangement = Arrangement.spacedBy(9.dp),
        ) {
            Box(
                Modifier.padding(top = 5.dp).size(7.dp).background(
                    when (line.tone) {
                        ActivityTone.Error -> Danger
                        ActivityTone.Warn -> activePalette.warning
                        ActivityTone.Info -> Teal
                    },
                    CircleShape,
                ),
            )
            Column(Modifier.weight(1f)) {
                Text(line.message, color = Ink, fontSize = 12.sp, lineHeight = 16.sp)
                Text(
                    "${line.source} · ${ago(line.atMs)}",
                    color = Muted,
                    fontSize = 10.sp,
                    fontFamily = LocalMagicanFontFamilies.current.mono,
                )
            }
        }
    }
}

/**
 * A compact operational section whose summary remains useful while collapsed.
 * Missing or invalid prerequisites force the relevant section open; ordinary
 * recomposition never closes a section the owner opened manually.
 */
@Composable
private fun AccordionSection(
    title: String,
    summary: String,
    initiallyExpanded: Boolean = false,
    forceExpanded: Boolean = false,
    attention: Boolean = false,
    content: @Composable () -> Unit,
) {
    var expanded by rememberSaveable(title) { mutableStateOf(initiallyExpanded) }
    LaunchedEffect(forceExpanded) {
        if (forceExpanded) expanded = true
    }
    Surface(
        color = Panel,
        shape = RoundedCornerShape(14.dp),
        border = BorderStroke(1.dp, if (attention) activePalette.warning.copy(alpha = 0.55f) else BorderSoft),
    ) {
        Column {
            Row(
                Modifier.fillMaxWidth().clickable { expanded = !expanded }
                    .padding(horizontal = 12.dp, vertical = 12.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                if (attention) {
                    Box(Modifier.size(8.dp).background(activePalette.warning, CircleShape))
                }
                Column(Modifier.weight(1f)) {
                    Text(title, color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                    Text(
                        summary,
                        color = if (attention) activePalette.warning else Muted,
                        fontSize = 10.sp,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                Icon(
                    if (expanded) Icons.Outlined.ExpandLess else Icons.Outlined.ExpandMore,
                    contentDescription = if (expanded) "Collapse $title" else "Expand $title",
                    tint = Secondary,
                )
            }
            if (expanded) {
                HorizontalDivider(color = BorderSoft)
                content()
            }
        }
    }
}

@Composable
private fun Section(title: String, content: @Composable () -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(7.dp)) {
        Text(
            title.uppercase(),
            color = Secondary,
            fontSize = 10.sp,
            fontWeight = FontWeight.Bold,
            letterSpacing = 0.8.sp,
            modifier = Modifier.padding(start = 3.dp),
        )
        Surface(color = Panel, shape = RoundedCornerShape(14.dp), border = BorderStroke(1.dp, BorderSoft)) {
            Column { content() }
        }
    }
}

@Composable
private fun ValueLine(label: String, value: String, monospace: Boolean = false) {
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(label, color = Ink, fontSize = 13.sp, modifier = Modifier.weight(1f))
        Text(
            value,
            color = Muted,
            fontSize = 11.sp,
            fontFamily = if (monospace) LocalMagicanFontFamilies.current.mono else LocalMagicanFontFamilies.current.body,
            maxLines = 2,
        )
    }
}

@Composable
private fun LabelLine(label: String) {
    Text(
        label,
        color = Ink,
        fontSize = 13.sp,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 10.dp),
    )
}

@Composable
private fun CopyableValueLine(value: String, copyDescription: String) {
    val clipboard = LocalClipboardManager.current
    Row(
        Modifier.fillMaxWidth().padding(start = 12.dp, end = 4.dp, top = 4.dp, bottom = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            value,
            color = Secondary,
            fontSize = 11.sp,
            fontFamily = LocalMagicanFontFamilies.current.mono,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        IconButton(onClick = { clipboard.setText(AnnotatedString(value)) }) {
            Icon(
                Icons.Outlined.ContentCopy,
                contentDescription = copyDescription,
                tint = Coral,
                modifier = Modifier.size(17.dp),
            )
        }
    }
}

@Composable
private fun WrappedValueLine(value: String) {
    Text(
        value,
        color = Secondary,
        fontSize = 11.sp,
        fontFamily = LocalMagicanFontFamilies.current.mono,
        lineHeight = 16.sp,
        softWrap = true,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 10.dp),
    )
}

@Composable
private fun RowDivider() = HorizontalDivider(color = BorderSoft, modifier = Modifier.padding(start = 12.dp))

@Composable
private fun pilotSwitchColors() = SwitchDefaults.colors(
    checkedThumbColor = activePalette.onAccent,
    checkedTrackColor = Coral,
    uncheckedThumbColor = Muted,
    uncheckedTrackColor = activePalette.control,
    uncheckedBorderColor = BorderSoft,
)

private fun ago(atMs: Long): String {
    val seconds = ((System.currentTimeMillis() - atMs) / 1000).coerceAtLeast(0)
    return when {
        seconds < 60 -> "${seconds}s ago"
        seconds < 3600 -> "${seconds / 60}m ago"
        else -> "${seconds / 3600}h ago"
    }
}

private fun isDefaultAssistant(context: Context): Boolean =
    Settings.Secure.getString(context.contentResolver, "assistant")
        .orEmpty().contains(context.packageName, ignoreCase = true)

private fun accessibilityEnabled(context: Context): Boolean =
    Settings.Secure.getString(context.contentResolver, Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES)
        .orEmpty().contains(context.packageName, ignoreCase = true)

private fun notificationListenerEnabled(context: Context): Boolean =
    Settings.Secure.getString(context.contentResolver, "enabled_notification_listeners")
        .orEmpty().contains(context.packageName, ignoreCase = true)

private fun batteryUnrestricted(context: Context): Boolean =
    context.getSystemService(PowerManager::class.java)
        ?.isIgnoringBatteryOptimizations(context.packageName) ?: false

private fun permitted(context: Context, permission: String): Boolean =
    (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU &&
        permission == android.Manifest.permission.POST_NOTIFICATIONS) ||
        ContextCompat.checkSelfPermission(context, permission) == android.content.pm.PackageManager.PERMISSION_GRANTED

private fun openBatterySettings(context: Context) {
    val direct = Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS)
        .setData(Uri.parse("package:${context.packageName}"))
    runCatching { context.startActivity(direct) }.onFailure { openAppSettings(context) }
}

private fun openAppSettings(context: Context) {
    context.startActivity(
        Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS)
            .setData(Uri.parse("package:${context.packageName}")),
    )
}
