package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.apps.AppSurfacingViewModel
import ai.magicbeans.magdroid.observe.ObserveService
import ai.magicbeans.magdroid.observe.ObserveState
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/**
 * A live observation, shown on every tab.
 *
 * iOS docks the same bar above its tab bar for the same reason: a recording of
 * somebody's room that you can forget is running is the failure worth designing
 * against. The notification exists too, but the notification is in the shade
 * and the shade is closed.
 */
@Composable
fun ObservationMiniBar(onOpen: () -> Unit) {
    val state by ObserveService.state.collectAsStateWithLifecycle()
    val chunks by ObserveService.chunksSent.collectAsStateWithLifecycle()
    val sharing by ObserveService.screenSharing.collectAsStateWithLifecycle()
    val context = androidx.compose.ui.platform.LocalContext.current

    AnimatedVisibility(visible = state == ObserveState.Listening || state == ObserveState.Starting) {
        Surface(color = Danger.copy(alpha = 0.12f), modifier = Modifier.fillMaxWidth()) {
            Row(
                Modifier.padding(horizontal = 16.dp, vertical = 10.dp).clickable { onOpen() },
                horizontalArrangement = Arrangement.spacedBy(10.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Box(Modifier.size(8.dp).background(Danger, CircleShape))
                Text(
                    ai.magicbeans.magdroid.observe.observeLiveLabel(state, sharing),
                    color = Ink, fontSize = 13.sp, fontWeight = FontWeight.Medium,
                )
                Text(observeElapsed(chunks), color = Secondary, fontSize = 12.sp)
                Spacer(Modifier.weight(1f))
                // Stop is on the bar itself. Making someone navigate to a
                // screen to end a recording is the wrong number of taps for
                // this particular action.
                Text(
                    "Stop",
                    color = Danger, fontSize = 13.sp, fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.clickable { ObserveService.stop(context) },
                )
            }
        }
    }
}

/** `m:ss` from the chunks that actually landed. */
internal fun observeElapsed(chunks: Long): String {
    val seconds = ObserveService.elapsedSeconds(chunks)
    return "%d:%02d".format(seconds / 60, seconds % 60)
}

internal fun otherActiveMeetings(
    active: List<ai.magicbeans.magdroid.meetings.ActiveMeeting>,
    localSessionId: String?,
): List<ai.magicbeans.magdroid.meetings.ActiveMeeting> =
    active.filterNot { localSessionId != null && it.sessionId == localSessionId }

/** A live block exists only for a real live capture; a failed probe is not one. */
internal fun observeHasNow(localListening: Boolean, activeMeetingCount: Int): Boolean =
    localListening || activeMeetingCount > 0

/**
 * Observe, as the web "Command Deck": a command header, four KPI cards that
 * are the only view switchers, the LIVE block above every view, and the Now /
 * Sources / Audio / Notes views.
 */
@OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)
@Composable
fun ObserveScreen(
    appSurfacing: AppSurfacingViewModel = androidx.lifecycle.viewmodel.compose.viewModel(),
    onOpenThread: (String) -> Unit = {},
    autoStart: Boolean = false,
    onOpenAudioNotes: () -> Unit = {},
) {
    var showMaps by remember { mutableStateOf(false) }
    if (showMaps) {
        ThinkingMapScreen(onClose = { showMaps = false })
        return
    }

    val context = androidx.compose.ui.platform.LocalContext.current
    val state by ObserveService.state.collectAsStateWithLifecycle()
    val problem by ObserveService.problem.collectAsStateWithLifecycle()
    val chunks by ObserveService.chunksSent.collectAsStateWithLifecycle()
    val sharing by ObserveService.screenSharing.collectAsStateWithLifecycle()
    val powerWarning by ObserveService.powerWarning.collectAsStateWithLifecycle()
    val localSessionId by ObserveService.activeSessionId.collectAsStateWithLifecycle()
    val localThreadId by ObserveService.activeThreadId.collectAsStateWithLifecycle()
    val listening = state == ObserveState.Listening || state == ObserveState.Starting
    val prefs = remember(context) { ai.magicbeans.magdroid.voice.VoicePrefs.get(context) }
    val observeScreenPref by prefs.observeScreen.collectAsStateWithLifecycle()
    var meetingTitle by remember { mutableStateOf("") }
    var pendingListenTitle by remember { mutableStateOf<String?>(null) }
    var pendingListenUrl by remember { mutableStateOf<String?>(null) }
    var permissionProblem by remember { mutableStateOf<String?>(null) }
    var notificationNotice by remember { mutableStateOf<String?>(null) }
    // Re-read permissions and battery whenever Observe comes back to the
    // foreground: the answer usually changed in system settings.
    var deviceTick by remember { mutableStateOf(0) }
    androidx.lifecycle.compose.LifecycleResumeEffect(Unit) {
        deviceTick++
        onPauseOrDispose { }
    }

    val startNow: (String?, String?) -> Unit = { title, url ->
        permissionProblem = null
        notificationNotice = if (notificationsEnabled(context)) null else NOTIFICATIONS_OFF_NOTICE
        ObserveService.start(context, title, url)
    }

    val requestStart = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions(),
    ) { results ->
        ObservePermissionMemory.markAsked(context, results.keys)
        deviceTick++
        if (micGranted(context)) {
            startNow(pendingListenTitle, pendingListenUrl)
        } else {
            permissionProblem = "Microphone access is needed to listen to this room."
        }
        pendingListenTitle = null
        pendingListenUrl = null
    }

    val startListen: (String?, String?) -> Unit = { requestedTitle, requestedUrl ->
        val title = requestedTitle?.trim()?.takeIf(String::isNotEmpty)
        val url = requestedUrl?.trim()?.takeIf(String::isNotEmpty)
        // Ask for notifications alongside the microphone, once: Observe runs
        // as a foreground service whose notification carries Stop. A refusal
        // never blocks listening — it is explained instead.
        val missing = buildList {
            if (!micGranted(context)) add(android.Manifest.permission.RECORD_AUDIO)
            if (android.os.Build.VERSION.SDK_INT >= 33 && !notificationsEnabled(context) &&
                !ObservePermissionMemory.asked(context, NOTIFICATIONS_PERMISSION)
            ) {
                add(NOTIFICATIONS_PERMISSION)
            }
        }
        if (missing.isEmpty()) {
            startNow(title, url)
        } else {
            pendingListenTitle = title
            pendingListenUrl = url
            requestStart.launch(missing.toTypedArray())
        }
    }

    val requestSingle = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions(),
    ) { results ->
        ObservePermissionMemory.markAsked(context, results.keys)
        deviceTick++
        if (micGranted(context)) permissionProblem = null
    }

    val shareConsent = rememberLauncherForActivityResult(
        ActivityResultContracts.StartActivityForResult(),
    ) { result ->
        val data = result.data
        if (result.resultCode == android.app.Activity.RESULT_OK && data != null) {
            ObserveService.shareScreen(context, result.resultCode, data)
        }
    }

    val toggleScreenShare = {
        if (sharing) {
            ObserveService.stopSharingScreen(context)
        } else {
            val manager = context.getSystemService(
                android.media.projection.MediaProjectionManager::class.java,
            )
            shareConsent.launch(manager.createScreenCaptureIntent())
        }
    }

    // "Start listening" from the launcher means start, not "open the screen
    // where you could". Routed through the same permission path the button
    // uses rather than a second copy of it, and guarded on `listening` so
    // returning to Observe later does not restart a session that is running.
    LaunchedEffect(autoStart) {
        if (!autoStart || listening) return@LaunchedEffect
        startListen(null, null)
    }

    val meetings: ai.magicbeans.magdroid.meetings.MeetingsViewModel =
        androidx.lifecycle.viewmodel.compose.viewModel()
    val meetingState by meetings.state.collectAsStateWithLifecycle()
    DisposableEffect(meetings) {
        meetings.start()
        onDispose {
            meetings.stop()
            meetings.stopFollowingTranscript()
        }
    }
    val deck: ai.magicbeans.magdroid.observe.ObserveDeckViewModel =
        androidx.lifecycle.viewmodel.compose.viewModel()
    val deckState by deck.state.collectAsStateWithLifecycle()
    val notes: ai.magicbeans.magdroid.notes.PublishedNotesViewModel =
        androidx.lifecycle.viewmodel.compose.viewModel()
    val notesState by notes.state.collectAsStateWithLifecycle()
    LaunchedEffect(deck) { deck.refreshAll() }

    val followedMeeting = meetingState.active.firstOrNull { it.sessionId == localSessionId }
        ?: meetingState.visibleActive.firstOrNull()
    LaunchedEffect(followedMeeting?.sessionId) {
        if (followedMeeting != null) meetings.followTranscript(followedMeeting)
        else meetings.stopFollowingTranscript()
    }
    val otherActive = otherActiveMeetings(meetingState.visibleActive, localSessionId)
    val hasLive = observeHasNow(listening, otherActive.size)
    val activeCaptures = observeActiveCaptureCount(meetingState.visibleActive, listening, localSessionId)
    val liveMeetings = liveCalendarMeetings(meetingState.upcoming)

    // The selected view, remembered per device; a deep link overrides it.
    var pane by remember { mutableStateOf(ObservePaneMemory.load(context)) }
    val requestedPane by ObservePaneRequests.pane.collectAsStateWithLifecycle()
    LaunchedEffect(requestedPane) {
        requestedPane?.let {
            pane = it
            ObservePaneMemory.save(context, it)
            ObservePaneRequests.consume(it)
        }
    }
    LaunchedEffect(pane) {
        if (pane == ObservePane.Notes) notes.loadIfNeeded()
    }
    val scroll = rememberScrollState()
    val selectPane: (ObservePane) -> Unit = { next ->
        pane = next
        ObservePaneMemory.save(context, next)
    }

    val refreshing = meetingState.loadingUpcoming || deckState.recent.loading ||
        deckState.channels.loading || deckState.audioSelection.loading
    val refreshAll: () -> Unit = {
        meetings.refresh(refreshCalendar = true)
        deck.refreshAll()
        if (notesState.loaded) notes.reload()
        deviceTick++
    }

    var openNotePath by remember { mutableStateOf<String?>(null) }

    Column(
        Modifier
            .fillMaxSize()
            .verticalScroll(scroll)
            .padding(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 20.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp),
        horizontalAlignment = Alignment.Start,
    ) {
        observeDeckSections(pane, hasLive).forEach { section ->
            when (section) {
                ObserveDeckSection.CommandHeader -> ObserveCommandHeader(
                    statusLine = observeCaptureStatusLine(activeCaptures, liveMeetings),
                    live = activeCaptures > 0,
                    refreshing = refreshing,
                    onRefresh = refreshAll,
                )
                ObserveDeckSection.Kpis -> Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                    ObserveKpiGrid(
                        kpis = observeKpis(
                            activeCaptures = activeCaptures,
                            liveMeetings = liveMeetings,
                            sourcesOn = if (deckState.channels.loaded || deckState.calendar.loaded ||
                                deckState.ambient.loaded || deckState.subscriptions.loaded
                            ) deckState.sourcesOn.toString() else "—",
                            audioValue = audioKpiValue(deckState.audioSelection.loaded),
                            notesValue = notesKpiValue(
                                deckState.recent.value?.size,
                                notesState.total.takeIf { notesState.loaded },
                            ),
                        ),
                        selected = pane,
                        onSelect = selectPane,
                    )
                    (permissionProblem ?: problem)?.let { ObserveBanner(it) }
                    notificationNotice?.takeIf { listening }?.let { ObserveBanner(it, tone = MWarn) }
                }
                ObserveDeckSection.Live -> ObserveLiveBlock(
                    localState = state,
                    chunks = chunks,
                    sharing = sharing,
                    powerWarning = powerWarning,
                    localThreadId = localThreadId,
                    localMeeting = followedMeeting?.takeIf { it.sessionId == localSessionId },
                    localTranscript = meetingState.transcript.takeIf {
                        meetingState.transcriptSessionId == localSessionId
                    }.orEmpty(),
                    otherActive = otherActive,
                    transcriptState = meetingState,
                    onOpenThread = onOpenThread,
                    onStopLocal = { ObserveService.stop(context) },
                    onToggleScreenShare = toggleScreenShare,
                    onStopRemote = meetings::stop,
                    onRetryActive = meetings::refreshActive,
                )
                ObserveDeckSection.Launchpad -> CaptureLaunchpad(
                    localLive = listening,
                    listening = state == ObserveState.Listening,
                    sharing = sharing,
                    meetingState = meetingState,
                    listenTitle = meetingTitle,
                    onListenTitleChange = { meetingTitle = it },
                    onListen = { startListen(meetingTitle, null) },
                    onOpenUrl = { openMeeting(context, it) },
                    onSendBot = { meetings.join(it, null) },
                    onToggleScreenShare = toggleScreenShare,
                    onBrainstorm = { showMaps = true },
                )
                ObserveDeckSection.Upcoming -> UpcomingSection(
                    state = meetingState,
                    localListening = listening,
                    onOpenUrl = { openMeeting(context, it) },
                    onListen = startListen,
                    onSendBot = meetings::join,
                    onOpenThread = onOpenThread,
                    onRefresh = { meetings.refresh(refreshCalendar = true) },
                )
                ObserveDeckSection.Widgets -> AppWidgetSlotRegion(
                    page = "/observe",
                    region = "reviews",
                    viewModel = appSurfacing,
                )
                ObserveDeckSection.Recent -> RecentSection(
                    lane = deckState.recent,
                    onOpenThread = onOpenThread,
                    onRetry = deck::loadRecent,
                )
                ObserveDeckSection.ThisPhone -> {
                    val phone = remember(deviceTick, observeScreenPref, powerWarning) {
                        thisPhoneState(context, observeScreenPref, powerWarning)
                    }
                    ThisPhoneSources(
                        state = phone,
                        onRequestMic = { requestSingle.launch(arrayOf(android.Manifest.permission.RECORD_AUDIO)) },
                        onRequestNotifications = { requestSingle.launch(arrayOf(NOTIFICATIONS_PERMISSION)) },
                        onOpenAppSettings = { openAppSettings(context) },
                        onOpenNotificationSettings = { openNotificationSettings(context) },
                        onObserveScreenChange = prefs::setObserveScreen,
                    )
                }
                ObserveDeckSection.WebAccounts -> WebAccountSources(deckState) { block ->
                    when (block) {
                        SourceBlock.Channels -> deck.loadChannels()
                        SourceBlock.Calendar -> deck.loadCalendar()
                        SourceBlock.Subscriptions -> deck.loadSubscriptions()
                        SourceBlock.BrowserTabs -> deck.loadAmbient()
                        SourceBlock.CatchUp -> deck.loadCatchUp()
                    }
                }
                ObserveDeckSection.AudioProfiles -> ObserveAudioView(
                    state = deckState,
                    onSelect = deck::selectAudioProfile,
                    onRetry = deck::loadAudio,
                )
                ObserveDeckSection.PublishedNotes -> PublishedNotesPanel(
                    state = notesState,
                    onSearch = notes::submitSearch,
                    onClearSearch = notes::clearSearch,
                    onPageSize = notes::setPageSize,
                    onPrevious = notes::previousPage,
                    onNext = notes::nextPage,
                    onReload = notes::reload,
                    onBackfill = notes::backfillNextBatch,
                    onPromote = notes::promote,
                    onOpen = { note -> if (note.isOpenable) openNotePath = note.notePath },
                )
                ObserveDeckSection.AudioNotes -> AudioNotesLink(onOpen = onOpenAudioNotes)
            }
        }
    }
    openNotePath?.let { path ->
        androidx.compose.material3.ModalBottomSheet(onDismissRequest = { openNotePath = null }) {
            NotesScreen(initialPath = path)
        }
    }
}

private const val NOTIFICATIONS_PERMISSION = "android.permission.POST_NOTIFICATIONS"
private const val NOTIFICATIONS_OFF_NOTICE =
    "Notifications are off, so the recording notification (with its Stop button) won't show. " +
        "Stop from the live bar or here; turn notifications on in Sources."

private fun notificationsEnabled(context: android.content.Context): Boolean =
    androidx.core.app.NotificationManagerCompat.from(context).areNotificationsEnabled()

/** Which runtime permissions Observe has asked for, so "blocked" can be told from "never asked". */
private object ObservePermissionMemory {
    private const val STORE = "magdroid.observe"

    fun asked(context: android.content.Context, permission: String): Boolean = runCatching {
        context.getSharedPreferences(STORE, android.content.Context.MODE_PRIVATE)
            .getBoolean("asked:$permission", false)
    }.getOrDefault(false)

    fun markAsked(context: android.content.Context, permissions: Collection<String>) {
        runCatching {
            val edit = context.getSharedPreferences(STORE, android.content.Context.MODE_PRIVATE).edit()
            permissions.forEach { edit.putBoolean("asked:$it", true) }
            edit.apply()
        }
    }
}

private fun android.content.Context.findActivity(): android.app.Activity? {
    var current: android.content.Context? = this
    while (current is android.content.ContextWrapper) {
        if (current is android.app.Activity) return current
        current = current.baseContext
    }
    return null
}

private fun thisPhoneState(
    context: android.content.Context,
    observeScreen: Boolean,
    liveWarning: ai.magicbeans.magdroid.voice.AmbientPowerBlock?,
): ThisPhoneState {
    val activity = context.findActivity()
    fun rationale(permission: String) = activity?.let {
        androidx.core.app.ActivityCompat.shouldShowRequestPermissionRationale(it, permission)
    } ?: false
    val notificationsApplicable = android.os.Build.VERSION.SDK_INT >= 33
    return ThisPhoneState(
        mic = permissionStatus(
            granted = micGranted(context),
            askedBefore = ObservePermissionMemory.asked(context, android.Manifest.permission.RECORD_AUDIO),
            showRationale = rationale(android.Manifest.permission.RECORD_AUDIO),
        ),
        notifications = if (notificationsApplicable) {
            permissionStatus(
                granted = notificationsEnabled(context),
                askedBefore = ObservePermissionMemory.asked(context, NOTIFICATIONS_PERMISSION),
                showRationale = rationale(NOTIFICATIONS_PERMISSION),
            )
        } else if (notificationsEnabled(context)) {
            PermissionStatus.Granted
        } else {
            // Below Android 13 there is no runtime prompt; only settings can change it.
            PermissionStatus.Blocked
        },
        notificationsApplicable = notificationsApplicable,
        observeScreen = observeScreen,
        battery = ai.magicbeans.magdroid.voice.AmbientPowerMonitor.read(context),
        liveWarning = liveWarning,
    )
}

private fun openAppSettings(context: android.content.Context) {
    runCatching {
        context.startActivity(
            android.content.Intent(
                android.provider.Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
                android.net.Uri.fromParts("package", context.packageName, null),
            ).addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK),
        )
    }
}

private fun openNotificationSettings(context: android.content.Context) {
    runCatching {
        context.startActivity(
            android.content.Intent(android.provider.Settings.ACTION_APP_NOTIFICATION_SETTINGS)
                .putExtra(android.provider.Settings.EXTRA_APP_PACKAGE, context.packageName)
                .addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK),
        )
    }.onFailure { openAppSettings(context) }
}

internal fun meetingWindowText(start: String?, end: String?): String {
    val startInstant = start?.let { runCatching { java.time.Instant.parse(it) }.getOrNull() }
        ?: return ""
    val zone = java.time.ZoneId.systemDefault()
    val startLocal = startInstant.atZone(zone)
    val now = java.time.LocalDate.now(zone)
    val prefix = when (startLocal.toLocalDate()) {
        now -> ""
        now.plusDays(1) -> "Tomorrow "
        else -> startLocal.format(java.time.format.DateTimeFormatter.ofPattern("EEE d MMM "))
    }
    val time = java.time.format.DateTimeFormatter.ofPattern("h:mm a")
    val endLocal = end?.let { runCatching { java.time.Instant.parse(it) }.getOrNull() }?.atZone(zone)
    return if (endLocal != null) {
        "$prefix${startLocal.format(time)}–${endLocal.format(time)}"
    } else {
        "$prefix${startLocal.format(time)}"
    }
}

private fun openMeeting(context: android.content.Context, rawUrl: String) {
    val trimmed = rawUrl.trim()
    if (trimmed.isEmpty()) return
    val normalized = if (trimmed.contains("://")) trimmed else "https://$trimmed"
    runCatching {
        context.startActivity(
            android.content.Intent(android.content.Intent.ACTION_VIEW, android.net.Uri.parse(normalized))
                .addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK),
        )
    }
}

private fun micGranted(context: android.content.Context): Boolean =
    androidx.core.content.ContextCompat.checkSelfPermission(
        context, android.Manifest.permission.RECORD_AUDIO,
    ) == android.content.pm.PackageManager.PERMISSION_GRANTED

