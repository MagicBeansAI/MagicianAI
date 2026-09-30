package ai.magicbeans.magdroid.ui

import android.text.format.DateFormat
import java.util.Date
import ai.magicbeans.magdroid.chat.ShellViewModel
import androidx.lifecycle.viewmodel.compose.viewModel
import ai.magicbeans.magdroid.chat.ChatUiState
import ai.magicbeans.magdroid.chat.ChatHistoryLane
import ai.magicbeans.magdroid.chat.ChatHistoryState
import ai.magicbeans.magdroid.chat.ChatHistoryTab
import ai.magicbeans.magdroid.chat.ChatHistoryViewModel
import ai.magicbeans.magdroid.chat.ChatViewModel
import ai.magicbeans.magdroid.chat.HistoryMutation
import ai.magicbeans.magdroid.chat.HistorySearchItem
import ai.magicbeans.magdroid.chat.SessionSummary
import ai.magicbeans.magdroid.chat.UiThreadRecord
import ai.magicbeans.magdroid.apps.AppSurfacingViewModel
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.outlined.Forum
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.Archive
import androidx.compose.material.icons.outlined.ChevronLeft
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.DeleteOutline
import androidx.compose.material.icons.outlined.DeleteSweep
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.Restore
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.filled.Lock
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.material.icons.outlined.Checklist
import androidx.compose.material.icons.outlined.Hearing
import androidx.compose.material.icons.outlined.NotificationsNone
import androidx.compose.material.icons.outlined.WbSunny
import androidx.compose.material.icons.filled.ChatBubble
import androidx.compose.material.icons.filled.Checklist as FilledChecklist
import androidx.compose.material.icons.filled.Hearing as FilledHearing
import androidx.compose.material.icons.filled.Notifications as FilledNotifications
import androidx.compose.material.icons.filled.WbSunny as FilledWbSunny
import androidx.compose.material3.Badge
import androidx.compose.material3.BadgedBox
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.activity.compose.BackHandler
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.unit.LayoutDirection
import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.BuildConfig
import androidx.compose.material.icons.outlined.AutoAwesome
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.GraphicEq
import androidx.compose.material.icons.outlined.Hub
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ChatBubbleOutline
import androidx.compose.material.icons.outlined.Menu
import androidx.compose.material.icons.outlined.PhoneAndroid
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import kotlinx.coroutines.launch

/** The app's top-level places. */
/**
 * The five surfaces, in the order iOS puts them.
 *
 * Today sits in the middle and is where the app opens, because the first
 * question on picking up the phone is "what is happening", not "what shall I
 * ask". Chat is one tab of five rather than the whole app.
 *
 * Settings is deliberately absent: it lives in the drawer, which is also where
 * the device companion belongs — both are things you configure once, not places
 * you move between.
 */
enum class Destination(val label: String) {
    Chat("Chat"),
    Tasks("Tasks"),
    Today("Today"),
    Attention("Attention"),
    Observe("Observe"),
}

internal data class NativeAppPageBinding(val page: String, val regions: List<String>)

internal fun Destination.nativeAppPageBinding(): NativeAppPageBinding? = when (this) {
    Destination.Today -> NativeAppPageBinding("/", listOf("primary", "secondary"))
    Destination.Observe -> NativeAppPageBinding("/observe", listOf("reviews"))
    else -> null
}

/**
 * Pull the five destinations into a slightly tighter centre group. The inset
 * creates deliberate breathing room at both screen edges while leaving a
 * 57.6dp-wide slot even on a 320dp phone, above Android's 48dp touch minimum.
 */
internal const val BOTTOM_NAV_SIDE_INSET_DP = 16
internal const val HISTORY_PANEL_WIDTH_DP = 300
internal const val HISTORY_ROW_CORNER_DP = 12
internal const val HISTORY_ROW_PADDING_DP = 10
internal const val HISTORY_ROW_TITLE_SP = 14
internal const val HISTORY_ROW_META_SP = 10
internal const val HISTORY_ROW_SUMMARY_SP = 10
internal const val HISTORY_RESULT_BADGE_SP = 9

internal fun bottomNavigationSlotWidthDp(containerWidthDp: Int): Float =
    (containerWidthDp - (BOTTOM_NAV_SIDE_INSET_DP * 2)).toFloat() / Destination.entries.size

/** Material icons, because a bottom bar drawn with text glyphs is not one. */
internal fun Destination.icon(selected: Boolean): androidx.compose.ui.graphics.vector.ImageVector =
    if (selected) when (this) {
        Destination.Chat -> Icons.Filled.ChatBubble
        Destination.Tasks -> Icons.Filled.FilledChecklist
        Destination.Today -> Icons.Filled.FilledWbSunny
        Destination.Attention -> Icons.Filled.FilledNotifications
        Destination.Observe -> Icons.Filled.FilledHearing
    } else when (this) {
        Destination.Chat -> Icons.Outlined.ChatBubbleOutline
        Destination.Tasks -> Icons.Outlined.Checklist
        Destination.Today -> Icons.Outlined.WbSunny
        Destination.Attention -> Icons.Outlined.NotificationsNone
        // Observe is room capture — listening, not speaking, which a plain mic
        // would imply.
        Destination.Observe -> Icons.Outlined.Hearing
    }

/**
 * The shell: a drawer of past conversations on the left, a bottom bar between
 * places, and the current place filling the rest.
 *
 * Session history is a drawer rather than a tab because it is a way back into
 * chat, not a place of its own — putting it in the bottom bar would say
 * otherwise.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun AppShell(
    viewModel: ChatViewModel,
    onOpenAppPilot: () -> Unit,
    mobilePairing: PairingUiBridge = PairingUiBridge(),
) {
    // `@brainstorm` is a client lane: chat hands over a seed and this shows the
    // map. Kept here rather than in ChatScreen so the chat view model never
    // needs to know which surface draws a map.
    val brainstormSeed by viewModel.brainstormSeed.collectAsStateWithLifecycle()
    val state by viewModel.state.collectAsStateWithLifecycle()
    val drawer = rememberDrawerState(DrawerValue.Closed)
    val history = rememberDrawerState(DrawerValue.Closed)
    val historyViewModel: ChatHistoryViewModel = viewModel()
    val historyState by historyViewModel.state.collectAsStateWithLifecycle()
    var unbuilt by remember { mutableStateOf<String?>(null) }
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    val focusManager = LocalFocusManager.current
    val keyboard = LocalSoftwareKeyboardController.current
    var showNotes by remember { mutableStateOf(false) }
    val notesChrome = remember { NotesChrome() }
    val shell: ShellViewModel = viewModel()
    val appSurfacing: AppSurfacingViewModel = viewModel()
    val appSurfacingState by appSurfacing.state.collectAsStateWithLifecycle()
    val lifecycleOwner = LocalLifecycleOwner.current
    DisposableEffect(lifecycleOwner, appSurfacing) {
        val observer = LifecycleEventObserver { _, event ->
            when (event) {
                Lifecycle.Event.ON_START -> appSurfacing.onForeground()
                Lifecycle.Event.ON_STOP -> appSurfacing.onBackground()
                else -> Unit
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        if (lifecycleOwner.lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)) {
            appSurfacing.onForeground()
        }
        onDispose {
            lifecycleOwner.lifecycle.removeObserver(observer)
            appSurfacing.onBackground()
        }
    }
    val tasksActions = rememberTasksScreenActions()
    // Activity-scoped, so TodayScreen's `viewModel()` resolves to this same
    // instance; the top bar hosts Today's refresh and its greeting title.
    val todayViewModel: ai.magicbeans.magdroid.today.TodayViewModel = viewModel()
    var showSettings by remember { mutableStateOf(false) }
    var showAudioNotes by remember { mutableStateOf(false) }
    var showThinkingMap by remember { mutableStateOf(false) }
    var settingsPage by remember { mutableStateOf(SettingsPage.Root) }
    LaunchedEffect(mobilePairing.pendingEnrollmentUri) {
        if (mobilePairing.pendingEnrollmentUri != null) {
            showSettings = true
            settingsPage = SettingsPage.Connection
        }
    }
    // System back unwinds the shell before it leaves the app: the drawer
    // closes, then the device surface, and only then does back mean "exit".
    // Without this an open drawer swallowed nothing and back dropped the owner
    // straight out of the app.
    BackHandler(enabled = history.isOpen) { scope.launch { history.close() } }
    BackHandler(enabled = drawer.isOpen && !history.isOpen) { scope.launch { drawer.close() } }
    BackHandler(enabled = showAudioNotes && !drawer.isOpen && !history.isOpen) { showAudioNotes = false }
    BackHandler(enabled = showNotes && !drawer.isOpen && !history.isOpen) { showNotes = false }
    BackHandler(enabled = showThinkingMap && !drawer.isOpen && !history.isOpen) { showThinkingMap = false }
    BackHandler(enabled = showSettings && !drawer.isOpen && !history.isOpen) {
        if (settingsPage != SettingsPage.Root) settingsPage = SettingsPage.Root
        else showSettings = false
    }
    val attentionBadge by shell.attentionBadge.collectAsStateWithLifecycle()
    val magicianReachable by shell.magicianReachable.collectAsStateWithLifecycle()
    // The chosen tab survives a relaunch, as iOS's `@AppStorage` does. Coming
    // back to where you were is the whole reason a bottom bar is worth having.
    var destination by remember { mutableStateOf(TabMemory.load(context)) }
    val taskDeepLink by TaskDeepLinks.target.collectAsStateWithLifecycle()
    LaunchedEffect(taskDeepLink) {
        if (taskDeepLink != null) destination = Destination.Tasks
    }

    // A launcher shortcut lands somewhere specific. Consumed on arrival: a
    // target left set would re-fire on every recomposition, and for "new chat"
    // that means silently discarding the session just started in.
    val shortcut by AppShortcutLinks.target.collectAsStateWithLifecycle()
    var startListeningOnArrival by remember { mutableStateOf(false) }
    var startTalkingOnArrival by remember { mutableStateOf(false) }
    LaunchedEffect(shortcut) {
        val requested = shortcut
        when (requested) {
            AppShortcutTarget.NewChat -> {
                viewModel.newSession()
                destination = Destination.Chat
            }

            AppShortcutTarget.AskTutor -> {
                viewModel.startBlackboard()
                destination = Destination.Chat
            }

            AppShortcutTarget.StartListening -> {
                startListeningOnArrival = true
                destination = Destination.Observe
            }

            AppShortcutTarget.StartTalking -> {
                startTalkingOnArrival = true
                destination = Destination.Chat
            }

            AppShortcutTarget.OpenAttention -> destination = Destination.Attention

            AppShortcutTarget.OpenToday -> destination = Destination.Today

            is AppShortcutTarget.OpenObserve -> {
                ObservePaneRequests.request(requested.pane)
                destination = Destination.Observe
            }

            null -> Unit
        }
        requested?.let { AppShortcutLinks.consume(it) }
    }
    LaunchedEffect(destination) {
        // The arrival is one-shot. Left armed, leaving Observe and coming back
        // would restart a recording the owner had deliberately stopped.
        if (destination != Destination.Observe) startListeningOnArrival = false
        if (destination != Destination.Chat) startTalkingOnArrival = false
        TabMemory.save(context, destination)
        // Re-read on every tab change, as iOS does, so the count is right at
        // the moment it might be acted on.
        shell.refreshBadge()
        destination.nativeAppPageBinding()?.let { binding ->
            appSurfacing.selectPage(binding.page, binding.regions)
        } ?: appSurfacing.selectPage(null)
    }
    LaunchedEffect(state.activeSessionId, state.sessions) {
        val threadId = state.sessions
            .firstOrNull { it.identifier() == state.activeSessionId }
            ?.uiThreadId
        historyViewModel.syncActive(state.activeSessionId, threadId)
    }
    LaunchedEffect(history.targetValue) {
        // Begin the read when the drawer starts opening, not after its slide
        // animation finishes; the latter makes a warm local fetch look slow.
        if (history.targetValue == DrawerValue.Open) historyViewModel.refresh()
    }

    val taskScreenVisible = destination == Destination.Tasks && !showSettings &&
        !showAudioNotes && !showNotes && !showThinkingMap && brainstormSeed == null
    val taskDetailVisible = taskScreenVisible && tasksActions.isDetailVisible

    // Two panels, on the sides their contents belong to. History is opened by
    // the control in the top-right and slides from the right, so the gesture
    // and the button agree about where the thing lives; Compose only offers a
    // leading drawer, so this one is mirrored and its content flipped back.
    CompositionLocalProvider(LocalLayoutDirection provides LayoutDirection.Rtl) {
    ModalNavigationDrawer(
        drawerState = history,
        gesturesEnabled = !taskDetailVisible,
        drawerContent = {
            CompositionLocalProvider(LocalLayoutDirection provides LayoutDirection.Ltr) {
                HistoryPanel(
                    state = historyState,
                    onClose = { scope.launch { history.close() } },
                    onSelectTab = historyViewModel::selectTab,
                    onSelectLane = historyViewModel::selectLane,
                    onSearchChange = historyViewModel::updateSearchText,
                    onSearchSubmit = historyViewModel::submitSearch,
                    onClearSearch = historyViewModel::clearSearch,
                    onRetry = historyViewModel::refresh,
                    onDismissError = historyViewModel::dismissError,
                    onPrevious = historyViewModel::loadPreviousPage,
                    onNext = historyViewModel::loadNextPage,
                    onOpenSession = { session ->
                        historyViewModel.openSession(session) { id ->
                            viewModel.openSession(id)
                            scope.launch { history.close() }
                        }
                    },
                    onOpenThread = { thread ->
                        historyViewModel.openThread(thread) { id ->
                            viewModel.openSession(id)
                            scope.launch { history.close() }
                        }
                    },
                    onNewSession = {
                        historyViewModel.createSession { id ->
                            viewModel.openSession(id)
                            scope.launch { history.close() }
                        }
                    },
                    onNewThread = { name ->
                        historyViewModel.createThread(name) {
                            scope.launch { history.close() }
                        }
                    },
                    onSessionMutation = { session, mutation ->
                        historyViewModel.mutateSession(session, mutation) {
                            viewModel.newSession()
                        }
                    },
                    onThreadMutation = { thread, mutation ->
                        historyViewModel.mutateThread(thread, mutation) {
                            viewModel.newSession()
                        }
                    },
                )
            }
        },
    ) {
    CompositionLocalProvider(LocalLayoutDirection provides LayoutDirection.Ltr) {
    ModalNavigationDrawer(
        drawerState = drawer,
        gesturesEnabled = !taskDetailVisible,
        drawerContent = {
            SideMenu(
                onSettings = {
                    scope.launch { drawer.close() }
                    settingsPage = SettingsPage.Root
                    showSettings = true
                },
                onDevice = {
                    scope.launch { drawer.close() }
                    onOpenAppPilot()
                },
                onAudioNotes = {
                    scope.launch { drawer.close() }
                    showSettings = false
                    showAudioNotes = true
                },
                onNotes = {
                    scope.launch { drawer.close() }
                    notesChrome.explorerOpen = true
                    showNotes = true
                },
                onThinkingMap = {
                    scope.launch { drawer.close() }
                    showSettings = false
                    showAudioNotes = false
                    showThinkingMap = true
                },
                onUnbuilt = { name ->
                    scope.launch { drawer.close() }
                    unbuilt = name
                },
            )
        },
    ) {
        Scaffold(
            containerColor = Ground,
            topBar = {
                if (taskScreenVisible) {
                    if (!taskDetailVisible) TasksTopBar(
                        onMenu = { scope.launch { drawer.open() } },
                        onCreate = tasksActions::showCreate,
                        onRefresh = tasksActions::refresh,
                    )
                } else {
                TopAppBar(
                    title = {
                        if (showSettings) {
                            Text(
                                settingsPage.title,
                                color = Ink, fontSize = 17.sp, fontWeight = FontWeight.SemiBold,
                            )
                        } else if (showAudioNotes) {
                            Text(
                                "Audio Notes",
                                color = Ink, fontSize = 17.sp, fontWeight = FontWeight.SemiBold,
                            )
                        } else if (showNotes) {
                            NotesField(
                                value = notesChrome.query,
                                onValueChange = { notesChrome.query = it },
                                placeholder = "Search Notes",
                                clearable = true,
                            )
                        } else if (showThinkingMap) {
                            Text(
                                "Thinking Map",
                                color = Ink, fontSize = 17.sp, fontWeight = FontWeight.SemiBold,
                            )
                        } else if (destination == Destination.Chat) {
                            ChatTitle(state, magicianReachable) { scope.launch { history.open() } }
                        } else if (destination == Destination.Today) {
                            // The masthead already says "Today's"; the bar greets
                            // instead, re-read each minute so it turns over live.
                            Text(
                                ai.magicbeans.magdroid.today.todayGreeting(rememberMastheadClock().hour),
                                color = Ink, fontSize = 17.sp, fontWeight = FontWeight.SemiBold,
                            )
                        } else {
                            Text(
                                destination.label,
                                color = Ink,
                                fontSize = 17.sp,
                                fontWeight = FontWeight.SemiBold,
                            )
                        }
                    },
                    navigationIcon = {
                        // A surface opened from the drawer needs a way back
                        // out. Leaving the hamburger here strands whoever
                        // opened it, which is what the bridge screen did.
                        androidx.compose.material3.IconButton(
                            onClick = {
                                when {
                                    showNotes -> {
                                        val opening = !notesChrome.explorerOpen
                                        notesChrome.explorerOpen = !notesChrome.explorerOpen
                                        if (opening) {
                                            focusManager.clearFocus(true)
                                            keyboard?.hide()
                                        }
                                    }
                                    showSettings && settingsPage != SettingsPage.Root -> settingsPage = SettingsPage.Root
                                    showSettings -> showSettings = false
                                    showAudioNotes -> showAudioNotes = false
                                    showThinkingMap -> showThinkingMap = false
                                    else -> scope.launch { drawer.open() }
                                }
                            },
                        ) {
                            androidx.compose.material3.Icon(
                                if (showNotes || !(showSettings || showAudioNotes || showThinkingMap)) Icons.Outlined.Menu
                                else Icons.AutoMirrored.Outlined.ArrowBack,
                                contentDescription = if (showNotes) "Folders" else if (showSettings || showAudioNotes || showThinkingMap) "Back" else "Chats",
                                tint = Ink,
                            )
                        }
                    },
                    actions = {
                        if (showNotes) {
                            androidx.compose.material3.IconButton(onClick = {
                                focusManager.clearFocus(true)
                                keyboard?.hide()
                                notesChrome.query = ""
                                showNotes = false
                            }) {
                                androidx.compose.material3.Icon(
                                    Icons.Outlined.Close,
                                    contentDescription = "Close",
                                    tint = Ink,
                                )
                            }
                        }
                        // History, not "New": starting a conversation is one row
                        // inside the drawer, while getting *back* to one is the
                        // thing reached from every turn. iOS puts the same
                        // control here for that reason.
                        if (destination == Destination.Chat && !showSettings && !showAudioNotes && !showNotes && !showThinkingMap) {
                            androidx.compose.material3.IconButton(
                                onClick = { scope.launch { history.open() } },
                            ) {
                                androidx.compose.material3.Icon(
                                    Icons.Outlined.Forum,
                                    contentDescription = "Chat history",
                                    tint = Coral,
                                )
                            }
                            state.activeSessionId?.let {
                                ChatSessionActionsMenu(
                                    state = state,
                                    onClear = viewModel::clearSession,
                                    onArchive = viewModel::archiveSession,
                                    onDelete = viewModel::deleteSession,
                                )
                            }
                        } else if (destination == Destination.Today && !showSettings && !showAudioNotes && !showNotes && !showThinkingMap) {
                            val refreshing = todayViewModel.state.collectAsStateWithLifecycle().value.refreshing
                            androidx.compose.material3.IconButton(onClick = todayViewModel::refresh, enabled = !refreshing) {
                                if (refreshing) {
                                    androidx.compose.material3.CircularProgressIndicator(
                                        Modifier.size(18.dp), strokeWidth = 2.dp, color = Coral,
                                    )
                                } else {
                                    androidx.compose.material3.Icon(
                                        Icons.Outlined.Refresh,
                                        contentDescription = "Refresh Today",
                                        tint = Coral,
                                    )
                                }
                            }
                        }
                    },
                    colors = TopAppBarDefaults.topAppBarColors(
                        containerColor = Ground,
                        scrolledContainerColor = Ground,
                        navigationIconContentColor = Ink,
                        titleContentColor = Ink,
                        actionIconContentColor = Ink,
                    ),
                )
                }
            },
            bottomBar = {
                if (!taskDetailVisible && !showSettings && !showAudioNotes && !showNotes && !showThinkingMap) Column {
                    // A running observation is visible from every tab, as iOS
                    // does it. A recording you can forget about is the failure
                    // mode this exists to prevent — the notification is in the
                    // shade, and the shade is closed.
                    ObservationMiniBar(onOpen = { destination = Destination.Observe })
                    NavigationBar(
                        modifier = Modifier.padding(horizontal = BOTTOM_NAV_SIDE_INSET_DP.dp),
                        containerColor = Ground,
                        tonalElevation = 0.dp,
                    ) {
                    Destination.entries.forEach { entry ->
                        val selected = destination == entry
                        NavigationBarItem(
                            selected = selected,
                            onClick = { destination = entry },
                            icon = {
                                // Attention carries a count. The badge is the
                                // only part of that surface visible from
                                // elsewhere, so it is live even while the tab
                                // is not.
                                if (entry == Destination.Attention && attentionBadge > 0) {
                                    BadgedBox(
                                        badge = {
                                            Badge(containerColor = Coral, contentColor = Color.White) {
                                                Text(if (attentionBadge > 99) "99+" else "$attentionBadge")
                                            }
                                        },
                                    ) {
                                        androidx.compose.material3.Icon(
                                            entry.icon(selected),
                                            contentDescription = entry.label,
                                            modifier = Modifier.size(22.dp),
                                        )
                                    }
                                } else {
                                    androidx.compose.material3.Icon(
                                        entry.icon(selected),
                                        contentDescription = entry.label,
                                        modifier = Modifier.size(22.dp),
                                    )
                                }
                            },
                            label = { Text(entry.label, fontSize = 11.sp) },
                            colors = NavigationBarItemDefaults.colors(
                                selectedIconColor = Coral,
                                selectedTextColor = Coral,
                                unselectedIconColor = Muted,
                                unselectedTextColor = Muted,
                                indicatorColor = Color.Transparent,
                            ),
                        )
                    }
                    }
                }
            },
        ) { padding ->
            Box(Modifier.padding(padding)) {
                if (showSettings) {
                    SettingsScreen(
                        page = settingsPage,
                        onOpenPage = { settingsPage = it },
                        mobilePairing = mobilePairing,
                    )
                }
                else if (brainstormSeed != null) {
                    ThinkingMapScreen(
                        onClose = viewModel::consumeBrainstormSeed,
                        seed = brainstormSeed,
                        onSeedTaken = viewModel::consumeBrainstormSeed,
                    )
                }
                else if (showAudioNotes) AudioNotesScreen()
                else if (showNotes) NotesScreen(chrome = notesChrome)
                else if (showThinkingMap) ThinkingMapScreen(
                    onClose = { showThinkingMap = false },
                    // The app bar's back arrow is the way out here.
                    ownsDismiss = false,
                )
                else when (destination) {
                    Destination.Chat -> ChatScreen(
                        viewModel,
                        onOpenSettings = {
                            settingsPage = SettingsPage.Root
                            showSettings = true
                        },
                        autoStartVoice = startTalkingOnArrival,
                        onAutoStartVoiceConsumed = { startTalkingOnArrival = false },
                        onOpenAttention = { itemId ->
                            AttentionDeepLinks.request(itemId)
                            destination = Destination.Attention
                        },
                        onOpenTask = { taskId ->
                            TaskDeepLinks.request(TaskDeepLinkTarget.Task(taskId))
                            destination = Destination.Tasks
                        },
                    )
                    Destination.Tasks -> TasksScreen(actions = tasksActions)
                    Destination.Today -> TodayScreen(
                        viewModel = todayViewModel,
                        appSurfacing = appSurfacing,
                        onOpenTask = { taskId ->
                            TaskDeepLinks.request(TaskDeepLinkTarget.Task(taskId))
                            destination = Destination.Tasks
                        },
                        onOpenMonitor = { taskId, updateId ->
                            TaskDeepLinks.request(TaskDeepLinkTarget.Monitor(taskId, updateId))
                            destination = Destination.Tasks
                        },
                        onOpenAttention = { itemId ->
                            // Attention owns its item resolution; Today only
                            // transfers the reader to the authoritative lane.
                            AttentionDeepLinks.request(itemId)
                            destination = Destination.Attention
                        },
                        onOpenThread = { threadId ->
                            viewModel.openSession(threadId)
                            destination = Destination.Chat
                        },
                        onOpenTasks = { destination = Destination.Tasks },
                    )
                    Destination.Attention -> AttentionScreen(
                        onOpenTask = { taskId ->
                            TaskDeepLinks.request(TaskDeepLinkTarget.Task(taskId))
                            destination = Destination.Tasks
                        },
                        onOpenThread = { threadId ->
                            viewModel.openSession(threadId)
                            destination = Destination.Chat
                        },
                        // A rejected credential is fixed in Settings, so the
                        // failure state offers the way there rather than
                        // naming a screen and leaving it to be found.
                        onOpenSettings = {
                            settingsPage = SettingsPage.Root
                            showSettings = true
                        },
                    )
                    Destination.Observe -> ObserveScreen(
                        appSurfacing = appSurfacing,
                        onOpenThread = { threadId ->
                            viewModel.openSession(threadId)
                            destination = Destination.Chat
                        },
                        autoStart = startListeningOnArrival,
                        onOpenAudioNotes = {
                            showSettings = false
                            showAudioNotes = true
                        },
                    )
                    else -> SurfacePlaceholder(destination)
                }
                unbuilt?.let { name ->
                    UnbuiltNotice(name) { unbuilt = null }
                }
                // Hosted here, not inside a widget card: opening an app's
                // entity route replaces the active slot page, which retires
                // the very region the card was drawn in.
                appSurfacingState.entitySurface?.let { surface ->
                    AppEntitySurfaceSheet(
                        surface = surface,
                        viewModel = appSurfacing,
                        onDismiss = appSurfacing::closeEntityPage,
                    )
                }
            }
        }
    }
    }
    }
}
}

/**
 * A menu entry that leads somewhere not built yet.
 *
 * Shown rather than silently doing nothing: a row that swallows a press reads
 * as a broken app, and one quietly missing from the menu teaches the wrong
 * shape of what this is.
 */
@Composable
private fun UnbuiltNotice(name: String, onDismiss: () -> Unit) {
    androidx.compose.material3.AlertDialog(
        onDismissRequest = onDismiss,
        confirmButton = {
            androidx.compose.material3.TextButton(onClick = onDismiss) {
                Text("Close", color = Coral)
            }
        },
        title = { Text(name, color = Ink) },
        text = { Text("Not built on Android yet.", color = Muted) },
        containerColor = Panel,
    )
}

/**
 * The left menu: everything that is not a place you navigate between.
 *
 * iOS keeps the same list here — identity at the top, the things you configure
 * or open occasionally, and the build at the foot. Session history is
 * deliberately absent: it belongs on the right, beside the conversation it
 * changes.
 */
@Composable
private fun SideMenu(
    onSettings: () -> Unit,
    onDevice: () -> Unit,
    onAudioNotes: () -> Unit,
    onNotes: () -> Unit,
    onThinkingMap: () -> Unit,
    onUnbuilt: (String) -> Unit,
) {
    val context = androidx.compose.ui.platform.LocalContext.current
    ModalDrawerSheet(drawerContainerColor = Ground, modifier = Modifier.width(300.dp)) {
        Column(Modifier.fillMaxSize()) {
            Column(
                Modifier.padding(start = 20.dp, end = 20.dp, top = 24.dp, bottom = 20.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Row(
                    horizontalArrangement = Arrangement.spacedBy(10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    androidx.compose.material3.Icon(
                        Icons.Outlined.AutoAwesome,
                        contentDescription = null,
                        tint = Coral,
                        modifier = Modifier.size(22.dp),
                    )
                    Text(
                        "Magican",
                        color = Ink,
                        fontSize = 22.sp,
                        fontWeight = FontWeight.Bold,
                        fontFamily = LocalMagicanFontFamilies.current.brand,
                    )
                }
                // Which account and workspace this app is acting as. Without it
                // there is no way to tell one install from another.
                Text(
                    "${MagicianAccess.principal(context)} / ${MagicianAccess.workspace(context)}",
                    color = Muted, fontSize = 13.sp,
                )
            }
            androidx.compose.material3.HorizontalDivider(
                color = BorderSoft,
                modifier = Modifier.padding(horizontal = 16.dp),
            )
            DrawerAction(Icons.Outlined.Settings, "Settings", onSettings)
            DrawerAction(Icons.Outlined.PhoneAndroid, "App Pilot", onDevice)
            DrawerAction(Icons.Outlined.GraphicEq, "Audio Notes", onAudioNotes)
            DrawerAction(Icons.Outlined.Description, "Notes", onNotes)
            // Thinking Map was built and this row went on apologising for it.
            // It had two ways in — a brainstorm deep link and a card on Observe
            // — and the one place someone would look for it said it did not
            // exist. An "unbuilt" notice outlives the building unless opening
            // the screen is what removes it.
            DrawerAction(Icons.Outlined.Hub, "Thinking Map", onThinkingMap)
            Spacer(Modifier.weight(1f))
            Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text("About", color = Ink, fontSize = 13.sp, fontWeight = FontWeight.Medium)
                Text("v${BuildConfig.VERSION_NAME}", color = Muted, fontSize = 12.sp)
            }
        }
    }
}

/** The right-side conversation navigator, kept in lockstep with iOS. */
@Composable
private fun HistoryPanel(
    state: ChatHistoryState,
    onClose: () -> Unit,
    onSelectTab: (ChatHistoryTab) -> Unit,
    onSelectLane: (ChatHistoryLane) -> Unit,
    onSearchChange: (String) -> Unit,
    onSearchSubmit: () -> Unit,
    onClearSearch: () -> Unit,
    onRetry: () -> Unit,
    onDismissError: () -> Unit,
    onPrevious: () -> Unit,
    onNext: () -> Unit,
    onOpenSession: (SessionSummary) -> Unit,
    onOpenThread: (UiThreadRecord) -> Unit,
    onNewSession: () -> Unit,
    onNewThread: (String) -> Unit,
    onSessionMutation: (SessionSummary, HistoryMutation) -> Unit,
    onThreadMutation: (UiThreadRecord, HistoryMutation) -> Unit,
) {
    var showThreadPrompt by remember { mutableStateOf(false) }
    var threadName by remember { mutableStateOf("") }
    ModalDrawerSheet(drawerContainerColor = Panel, modifier = Modifier.width(HISTORY_PANEL_WIDTH_DP.dp)) {
        Column(Modifier.fillMaxSize().background(Ground)) {
            Row(
                Modifier.fillMaxWidth().background(Panel).padding(horizontal = 14.dp, vertical = 12.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f)) {
                    Text("History", color = Ink, fontSize = 22.sp, fontWeight = FontWeight.Bold)
                    Text(
                        "${state.total} ${if (state.searchActive) "results" else state.activeTab.wire}",
                        color = Muted,
                        fontSize = 11.sp,
                    )
                }
                IconButton(onClick = onClose, modifier = Modifier.size(36.dp)) {
                    Icon(Icons.Outlined.Close, contentDescription = "Close history", tint = Muted)
                }
            }

            if (state.searchActive) {
                Row(
                    Modifier.padding(horizontal = 14.dp, vertical = 7.dp)
                        .fillMaxWidth()
                        .background(Control, RoundedCornerShape(8.dp))
                        .padding(horizontal = 10.dp, vertical = 9.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(Icons.Outlined.Forum, null, tint = Muted, modifier = Modifier.size(16.dp))
                    Spacer(Modifier.width(7.dp))
                    Text("All sessions and threads", color = Ink, fontSize = 11.sp, fontWeight = FontWeight.SemiBold)
                    Spacer(Modifier.weight(1f))
                    Text("Personal + Automated", color = Muted, fontSize = 9.sp)
                }
            } else {
                Row(
                    Modifier.fillMaxWidth().background(Panel).padding(horizontal = 14.dp, vertical = 7.dp),
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    ChatHistoryTab.entries.forEach { tab ->
                        HistorySegment(
                            text = tab.title,
                            selected = state.activeTab == tab,
                            primary = true,
                            modifier = Modifier.weight(1f),
                        ) { onSelectTab(tab) }
                    }
                }
                Row(
                    Modifier.padding(horizontal = 14.dp).fillMaxWidth()
                        .background(Control, RoundedCornerShape(8.dp)).padding(3.dp),
                    horizontalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    ChatHistoryLane.entries.forEach { lane ->
                        HistorySegment(
                            text = lane.title,
                            selected = state.historyLane == lane,
                            modifier = Modifier.weight(1f),
                        ) { onSelectLane(lane) }
                    }
                }
                Spacer(Modifier.height(7.dp))
            }

            MagicianTextField(
                value = state.searchText,
                onValueChange = onSearchChange,
                modifier = Modifier.padding(horizontal = 14.dp).fillMaxWidth(),
                placeholder = { Text("Search all history", color = Muted, fontSize = 12.sp) },
                leadingIcon = { Icon(Icons.Outlined.Search, null, tint = Muted, modifier = Modifier.size(17.dp)) },
                trailingIcon = if (state.searchText.isNotEmpty()) {{
                    IconButton(onClick = onClearSearch, modifier = Modifier.size(30.dp)) {
                        Icon(Icons.Outlined.Close, "Clear history search", tint = Muted, modifier = Modifier.size(16.dp))
                    }
                }} else null,
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                keyboardActions = KeyboardActions(onSearch = { onSearchSubmit() }),
                singleLine = true,
            )

            if (state.canCreate) {
                Spacer(Modifier.height(7.dp))
                Surface(
                    color = Coral.copy(alpha = 0.12f),
                    shape = RoundedCornerShape(8.dp),
                    modifier = Modifier.padding(horizontal = 14.dp).fillMaxWidth().clickable {
                        if (state.activeTab == ChatHistoryTab.Sessions) onNewSession()
                        else {
                            threadName = ""
                            showThreadPrompt = true
                        }
                    },
                ) {
                    Row(
                        Modifier.padding(vertical = 9.dp),
                        horizontalArrangement = Arrangement.Center,
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Icon(Icons.Outlined.Add, null, tint = Coral, modifier = Modifier.size(17.dp))
                        Spacer(Modifier.width(5.dp))
                        Text(
                            if (state.activeTab == ChatHistoryTab.Sessions) "New Session" else "New Thread",
                            color = Coral,
                            fontSize = 13.sp,
                            fontWeight = FontWeight.SemiBold,
                        )
                    }
                }
            }
            Spacer(Modifier.height(8.dp))
            HorizontalDivider(color = BorderSoft)
            if (state.loading && !state.currentEmpty) {
                LinearProgressIndicator(
                    modifier = Modifier.fillMaxWidth().height(2.dp),
                    color = Coral,
                    trackColor = Color.Transparent,
                )
            }
            state.error?.takeIf { !state.currentEmpty }?.let { message ->
                Row(
                    Modifier.fillMaxWidth().background(MWarn.copy(alpha = 0.12f))
                        .padding(horizontal = 12.dp, vertical = 7.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(message, color = Ink, fontSize = 11.sp, maxLines = 2, modifier = Modifier.weight(1f))
                    IconButton(onClick = onDismissError, modifier = Modifier.size(28.dp)) {
                        Icon(Icons.Outlined.Close, "Dismiss history error", tint = Muted, modifier = Modifier.size(15.dp))
                    }
                }
            }

            val fullPageError = state.error
            Box(Modifier.weight(1f).fillMaxWidth()) {
                when {
                    fullPageError != null && state.currentEmpty -> HistoryFailure(fullPageError, onRetry)
                    state.loading && state.currentEmpty -> CircularProgressIndicator(
                        color = Coral,
                        strokeWidth = 2.dp,
                        modifier = Modifier.size(24.dp).align(Alignment.Center),
                    )
                    state.currentEmpty -> Text(
                        when {
                            state.searchActive -> "No matching history."
                            state.activeTab == ChatHistoryTab.Sessions -> "No sessions yet."
                            else -> "No threads yet."
                        },
                        color = Muted,
                        fontSize = 12.sp,
                        modifier = Modifier.align(Alignment.TopCenter).padding(22.dp),
                    )
                    else -> LazyColumn(
                        Modifier.fillMaxSize().padding(16.dp),
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        if (state.searchActive) {
                            items(state.searchResults, key = { it.identifier }) { result ->
                                HistorySearchRow(
                                    result = result,
                                    state = state,
                                    onOpenSession = onOpenSession,
                                    onOpenThread = onOpenThread,
                                    onSessionMutation = onSessionMutation,
                                    onThreadMutation = onThreadMutation,
                                )
                            }
                        } else if (state.activeTab == ChatHistoryTab.Sessions) {
                            items(state.sessions, key = { it.identifier() }) { session ->
                                HistorySessionRow(
                                    session = session,
                                    active = session.identifier() == state.activeSessionId,
                                    lane = null,
                                    busy = state.mutationInFlight != null,
                                    onOpen = { onOpenSession(session) },
                                    onMutation = { onSessionMutation(session, it) },
                                )
                            }
                        } else {
                            items(state.threads, key = { it.id }) { thread ->
                                HistoryThreadRow(
                                    thread = thread,
                                    active = thread.id == state.activeThreadId,
                                    lane = null,
                                    busy = state.mutationInFlight != null || state.openingThreadId != null,
                                    opening = thread.id == state.openingThreadId,
                                    onOpen = { onOpenThread(thread) },
                                    onMutation = { onThreadMutation(thread, it) },
                                )
                            }
                        }
                    }
                }
            }

            HorizontalDivider(color = BorderSoft)
            Row(
                Modifier.fillMaxWidth().background(Panel).height(44.dp).padding(horizontal = 12.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                IconButton(onClick = onPrevious, enabled = state.canLoadPrevious, modifier = Modifier.size(34.dp)) {
                    Icon(Icons.Outlined.ChevronLeft, "Previous history page", tint = if (state.canLoadPrevious) Coral else Muted)
                }
                Spacer(Modifier.weight(1f))
                Text("${state.pageStart}-${state.pageEnd} of ${state.total}", color = Muted, fontSize = 10.sp)
                Spacer(Modifier.weight(1f))
                IconButton(onClick = onNext, enabled = state.canLoadNext, modifier = Modifier.size(34.dp)) {
                    Icon(Icons.Outlined.ChevronRight, "Next history page", tint = if (state.canLoadNext) Coral else Muted)
                }
            }
        }
    }

    if (showThreadPrompt) {
        AlertDialog(
            onDismissRequest = { showThreadPrompt = false },
            title = { Text("New Thread", color = Ink) },
            text = {
                MagicianTextField(
                    value = threadName,
                    onValueChange = { threadName = it },
                    modifier = Modifier.fillMaxWidth(),
                    placeholder = { Text("Thread name", color = Muted) },
                    singleLine = true,
                )
            },
            confirmButton = {
                TextButton(
                    enabled = threadName.isNotBlank() && state.mutationInFlight == null,
                    onClick = {
                        onNewThread(threadName)
                        showThreadPrompt = false
                    },
                ) { Text("Create", color = Coral) }
            },
            dismissButton = { TextButton(onClick = { showThreadPrompt = false }) { Text("Cancel", color = Muted) } },
            containerColor = Panel,
        )
    }
}

@Composable
private fun HistorySegment(
    text: String,
    selected: Boolean,
    primary: Boolean = false,
    modifier: Modifier = Modifier,
    onClick: () -> Unit,
) {
    Surface(
        color = when {
            !selected -> Color.Transparent
            primary -> Coral.copy(alpha = 0.10f)
            else -> Panel
        },
        shape = RoundedCornerShape(if (primary) 8.dp else 6.dp),
        modifier = modifier.height(if (primary) 38.dp else 30.dp).clickable(onClick = onClick),
    ) {
        Box(contentAlignment = Alignment.Center) {
            Text(
                text,
                color = when {
                    !selected -> Muted
                    primary -> Coral
                    else -> Ink
                },
                fontSize = if (primary) 15.sp else 12.sp,
                fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
            )
        }
    }
}

@Composable
private fun HistoryFailure(message: String, onRetry: () -> Unit) {
    Column(
        Modifier.fillMaxSize().padding(20.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text(message, color = Muted, fontSize = 12.sp, textAlign = androidx.compose.ui.text.style.TextAlign.Center)
        Spacer(Modifier.height(8.dp))
        TextButton(onClick = onRetry) { Text("Retry", color = Coral, fontSize = 12.sp) }
    }
}

@Composable
private fun HistorySearchRow(
    result: HistorySearchItem,
    state: ChatHistoryState,
    onOpenSession: (SessionSummary) -> Unit,
    onOpenThread: (UiThreadRecord) -> Unit,
    onSessionMutation: (SessionSummary, HistoryMutation) -> Unit,
    onThreadMutation: (UiThreadRecord, HistoryMutation) -> Unit,
) {
    result.session?.let { session ->
        HistorySessionRow(
            session = session,
            active = session.identifier() == state.activeSessionId,
            lane = result.historyLane,
            busy = state.mutationInFlight != null,
            onOpen = { onOpenSession(session) },
            onMutation = { onSessionMutation(session, it) },
        )
    } ?: result.thread?.let { thread ->
        HistoryThreadRow(
            thread = thread,
            active = thread.id == state.activeThreadId,
            lane = result.historyLane,
            busy = state.mutationInFlight != null || state.openingThreadId != null,
            opening = thread.id == state.openingThreadId,
            onOpen = { onOpenThread(thread) },
            onMutation = { onThreadMutation(thread, it) },
        )
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun HistorySessionRow(
    session: SessionSummary,
    active: Boolean,
    lane: String?,
    busy: Boolean,
    onOpen: () -> Unit,
    onMutation: (HistoryMutation) -> Unit,
) {
    var menu by remember(session.identifier()) { mutableStateOf(false) }
    val protected = session.isDefaultSession || session.internalVoice != null
    Box {
        Surface(
            color = if (active) Coral.copy(alpha = 0.10f) else Panel,
            shape = RoundedCornerShape(HISTORY_ROW_CORNER_DP.dp),
            border = androidx.compose.foundation.BorderStroke(
                1.dp,
                if (active) Coral.copy(alpha = 0.30f) else Color.Transparent,
            ),
            modifier = Modifier.fillMaxWidth().combinedClickable(
                enabled = !busy,
                onClick = onOpen,
                onLongClick = { if (!protected) menu = true },
            ),
        ) {
            Row(
                Modifier.padding(HISTORY_ROW_PADDING_DP.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text(
                        session.title?.takeIf { it.isNotBlank() } ?: "Untitled session",
                        color = if (active) Coral else Ink,
                        fontSize = HISTORY_ROW_TITLE_SP.sp,
                        fontWeight = FontWeight.SemiBold,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(5.dp)) {
                        Text(
                            "#${session.uiThreadId?.takeIf { it.isNotBlank() } ?: "general"}",
                            color = Ink,
                            fontSize = HISTORY_ROW_META_SP.sp,
                            maxLines = 1,
                            modifier = Modifier.background(
                                Secondary.copy(alpha = 0.20f),
                                RoundedCornerShape(4.dp),
                            ).padding(horizontal = 6.dp, vertical = 2.dp),
                        )
                        if (lane != null) {
                            HistoryBadges(if (session.internalVoice?.kind == "branch") "Concurrent" else "Session", lane)
                        } else if (session.internalVoice?.kind == "branch") {
                            HistoryResultBadge("Concurrent", emphasized = false)
                        }
                        HistoryTime(session.updatedAt, Modifier.weight(1f))
                    }
                }
                when {
                    session.isDefaultSession -> {
                        Spacer(Modifier.width(8.dp))
                        Icon(Icons.Filled.Lock, "Default session", tint = Muted, modifier = Modifier.size(11.dp))
                    }
                    active -> {
                        Spacer(Modifier.width(8.dp))
                        Box(Modifier.size(8.dp).background(Coral, CircleShape))
                    }
                }
            }
        }
        HistoryMutationMenu(
            expanded = menu,
            archived = session.status == "archived",
            onDismiss = { menu = false },
            onMutation = {
                menu = false
                onMutation(it)
            },
        )
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun HistoryThreadRow(
    thread: UiThreadRecord,
    active: Boolean,
    lane: String?,
    busy: Boolean,
    opening: Boolean,
    onOpen: () -> Unit,
    onMutation: (HistoryMutation) -> Unit,
) {
    var menu by remember(thread.id) { mutableStateOf(false) }
    Box {
        Surface(
            color = if (active) Coral.copy(alpha = 0.10f) else Panel,
            shape = RoundedCornerShape(HISTORY_ROW_CORNER_DP.dp),
            border = androidx.compose.foundation.BorderStroke(
                1.dp,
                if (active) Coral.copy(alpha = 0.30f) else Color.Transparent,
            ),
            modifier = Modifier.fillMaxWidth().combinedClickable(
                enabled = !busy,
                onClick = onOpen,
                onLongClick = { if (!thread.isGeneral) menu = true },
            ),
        ) {
            Row(
                Modifier.padding(HISTORY_ROW_PADDING_DP.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text(
                        thread.name.ifBlank { thread.id },
                        color = if (active) Coral else Ink,
                        fontSize = HISTORY_ROW_TITLE_SP.sp,
                        fontWeight = FontWeight.SemiBold,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    HistoryBadges("Thread", lane)
                    thread.memorySummary?.takeIf { it.isNotBlank() }?.let {
                        Text(it, color = Muted, fontSize = HISTORY_ROW_SUMMARY_SP.sp, maxLines = 2, overflow = TextOverflow.Ellipsis)
                    }
                }
                when {
                    opening -> {
                        Spacer(Modifier.width(8.dp))
                        CircularProgressIndicator(color = Coral, strokeWidth = 2.dp, modifier = Modifier.size(16.dp))
                    }
                    thread.isGeneral -> {
                        Spacer(Modifier.width(8.dp))
                        Icon(Icons.Filled.Lock, "Default thread", tint = Muted, modifier = Modifier.size(11.dp))
                    }
                    active -> {
                        Spacer(Modifier.width(8.dp))
                        Box(Modifier.size(8.dp).background(Coral, CircleShape))
                    }
                }
            }
        }
        HistoryMutationMenu(
            expanded = menu,
            archived = thread.archived,
            onDismiss = { menu = false },
            onMutation = {
                menu = false
                onMutation(it)
            },
        )
    }
}

@Composable
private fun HistoryBadges(kind: String, lane: String?) {
    if (lane == null) return
    val laneTitle = if (lane == "automated") "Automated" else "Personal"
    Row(horizontalArrangement = Arrangement.spacedBy(5.dp)) {
        HistoryResultBadge(kind, emphasized = false)
        HistoryResultBadge(laneTitle, emphasized = lane == "personal")
    }
}

@Composable
private fun HistoryResultBadge(title: String, emphasized: Boolean) {
    Surface(
        color = if (emphasized) Coral.copy(alpha = 0.10f) else Ground,
        shape = RoundedCornerShape(4.dp),
        border = androidx.compose.foundation.BorderStroke(
            1.dp,
            if (emphasized) Coral.copy(alpha = 0.22f) else Secondary.copy(alpha = 0.14f),
        ),
    ) {
        Text(
            title,
            color = if (emphasized) Coral else Muted,
            fontSize = HISTORY_RESULT_BADGE_SP.sp,
            fontWeight = if (emphasized) FontWeight.SemiBold else FontWeight.Normal,
            modifier = Modifier.padding(horizontal = 6.dp, vertical = 2.dp),
        )
    }
}

@Composable
private fun HistoryTime(millis: Long?, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    millis?.takeIf { it > 0 }?.let {
        Text(
            "${DateFormat.getDateFormat(context).format(Date(it))}, ${DateFormat.getTimeFormat(context).format(Date(it))}",
            color = Muted,
            fontSize = HISTORY_ROW_META_SP.sp,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = modifier,
        )
    }
}

@Composable
private fun HistoryMutationMenu(
    expanded: Boolean,
    archived: Boolean,
    onDismiss: () -> Unit,
    onMutation: (HistoryMutation) -> Unit,
) {
    DropdownMenu(expanded = expanded, onDismissRequest = onDismiss, containerColor = Panel) {
        DropdownMenuItem(
            text = { Text(if (archived) "Restore" else "Archive", color = Ink, fontSize = 13.sp) },
            leadingIcon = { Icon(if (archived) Icons.Outlined.Restore else Icons.Outlined.Archive, null, tint = Muted) },
            onClick = { onMutation(if (archived) HistoryMutation.Restore else HistoryMutation.Archive) },
        )
        DropdownMenuItem(
            text = { Text("Delete", color = Danger, fontSize = 13.sp) },
            leadingIcon = { Icon(Icons.Outlined.DeleteOutline, null, tint = Danger) },
            onClick = { onMutation(HistoryMutation.Delete) },
        )
    }
}

/** The same current-session overflow menu iOS keeps beside Chat history. */
@Composable
private fun ChatSessionActionsMenu(
    state: ChatUiState,
    onClear: () -> Unit,
    onArchive: () -> Unit,
    onDelete: () -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    val busy = state.sessionActionInFlight != null
    val isDefaultSession = state.sessions
        .firstOrNull { it.identifier() == state.activeSessionId }
        ?.isDefaultSession == true
    val isConcurrent = (state.selectedSession ?: state.sessions.firstOrNull { it.identifier() == state.activeSessionId })?.internalVoice != null
    val canRemoveSession = !busy && !isDefaultSession && !isConcurrent

    Box {
        IconButton(
            onClick = { expanded = true },
            enabled = !busy,
        ) {
            Icon(
                Icons.Outlined.MoreVert,
                contentDescription = "Chat options",
                tint = if (busy) Muted else Ink,
            )
        }
        DropdownMenu(
            expanded = expanded,
            onDismissRequest = { expanded = false },
            containerColor = Panel,
        ) {
            DropdownMenuItem(
                text = { Text("Clear chat", color = if (busy) Muted else Ink, fontSize = 14.sp) },
                leadingIcon = { Icon(Icons.Outlined.DeleteSweep, null, tint = Muted) },
                enabled = !busy,
                onClick = {
                    expanded = false
                    onClear()
                },
            )
            DropdownMenuItem(
                text = { Text("Archive session", color = if (canRemoveSession) Ink else Muted, fontSize = 14.sp) },
                leadingIcon = { Icon(Icons.Outlined.Archive, null, tint = Muted) },
                // Magician protects the original #general session as the
                // durable default. Keep the iOS option visible but do not send
                // an operation the backend must reject.
                enabled = canRemoveSession,
                onClick = {
                    expanded = false
                    onArchive()
                },
            )
            DropdownMenuItem(
                text = { Text("Delete session", color = if (canRemoveSession) Coral else Muted, fontSize = 14.sp) },
                leadingIcon = {
                    Icon(Icons.Outlined.DeleteOutline, null, tint = if (canRemoveSession) Coral else Muted)
                },
                enabled = canRemoveSession,
                onClick = {
                    expanded = false
                    onDelete()
                },
            )
        }
    }
}

@Composable
private fun DrawerAction(
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    label: String,
    onClick: () -> Unit,
) {
    Row(
        Modifier
            .fillMaxWidth()
            .clickable { onClick() }
            .padding(horizontal = 16.dp, vertical = 13.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        androidx.compose.material3.Icon(
            icon, contentDescription = null, tint = Muted, modifier = Modifier.size(18.dp),
        )
        Text(label, color = Ink, fontSize = 14.sp)
    }
}

/**
 * The chat's two-line title: thread above, live-state dot and session below.
 *
 * Tapping it opens the history drawer — the title names where you are, so it is
 * also the natural handle for changing it.
 */
@Composable
private fun ChatTitle(
    state: ChatUiState,
    reachable: Boolean?,
    onOpenHistory: () -> Unit,
) {
    val session = state.sessions.firstOrNull { it.identifier() == state.activeSessionId }
        ?: state.selectedSession?.takeIf { it.identifier() == state.activeSessionId }
    Column(
        Modifier
            .widthIn(max = 210.dp)
            .clickable { onOpenHistory() },
        verticalArrangement = Arrangement.spacedBy(1.dp),
    ) {
        Text(
            session?.threadLabel() ?: "General",
            color = Ink, fontSize = 15.sp, fontWeight = FontWeight.SemiBold,
            maxLines = 1, overflow = TextOverflow.Ellipsis,
        )
        Row(
            horizontalArrangement = Arrangement.spacedBy(5.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(
                Modifier
                    .size(6.dp)
                    // Service health, not turn outcome. These are different
                    // questions and the dot was answering the wrong one: a
                    // failed turn against a healthy backend is not the same
                    // fact as a backend nobody can reach, and only the second
                    // is worth a permanent light.
                    //
                    // Unchecked is amber rather than green: a phone that has
                    // just opened has not learned anything good yet either.
                    .background(
                        when (reachable) {
                            true -> Teal
                            false -> Coral
                            null -> MWarn
                        },
                        CircleShape,
                    ),
            )
            Text(
                session?.label() ?: if (state.activeSessionId == null) "New session" else "Untitled session",
                color = Secondary, fontSize = 11.sp,
                maxLines = 1, overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

/**
 * Which tab the owner was last on.
 *
 * Today is the default, matching iOS: the app opens on what is happening
 * rather than on an empty prompt.
 */
private object TabMemory {
    private const val FILE = "magdroid.shell"
    private const val KEY = "selected_tab"

    fun load(context: android.content.Context): Destination {
        val stored = context
            .getSharedPreferences(FILE, android.content.Context.MODE_PRIVATE)
            .getString(KEY, null)
        // A tab that no longer exists falls back to the default rather than
        // crashing on an enum that has been reordered since.
        return Destination.entries.firstOrNull { it.name == stored } ?: Destination.Today
    }

    fun save(context: android.content.Context, destination: Destination) {
        context.getSharedPreferences(FILE, android.content.Context.MODE_PRIVATE)
            .edit()
            .putString(KEY, destination.name)
            .apply()
    }
}

/**
 * A surface that is reachable but not yet built.
 *
 * Named honestly rather than dressed as an empty state: "nothing here yet" and
 * "nothing is waiting on you" look identical, and only one of them is true.
 */
@Composable
private fun SurfacePlaceholder(destination: Destination) {
    val blurb = when (destination) {
        Destination.Tasks -> "Everything running on your behalf, and what it produced."
        Destination.Today -> "What happened, what is waiting, and what is next."
        Destination.Attention -> "Requests, approvals and escalations that need you."
        Destination.Observe -> "Capture a room, a call, or the screen, and ask about it."
        Destination.Chat -> ""
    }
    Column(
        Modifier.fillMaxSize().padding(32.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        androidx.compose.material3.Icon(
            destination.icon(selected = false),
            contentDescription = null,
            tint = Muted,
            modifier = Modifier.size(36.dp),
        )
        Spacer(Modifier.height(14.dp))
        Text(destination.label, color = Ink, fontSize = 19.sp, fontWeight = FontWeight.Bold)
        Spacer(Modifier.height(6.dp))
        Text(
            blurb,
            color = Muted, fontSize = 13.sp, lineHeight = 18.sp,
            textAlign = androidx.compose.ui.text.style.TextAlign.Center,
        )
        Spacer(Modifier.height(16.dp))
        Text(
            "Not built on Android yet.",
            color = Secondary, fontSize = 12.sp, fontWeight = FontWeight.Medium,
        )
    }
}
