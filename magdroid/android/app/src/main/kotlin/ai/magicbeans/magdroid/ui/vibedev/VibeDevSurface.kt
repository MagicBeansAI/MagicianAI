package ai.magicbeans.magdroid.ui.vibedev

import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.ui.LocalMagicanFontFamilies
import ai.magicbeans.magdroid.ui.MagicanButtonShape
import ai.magicbeans.magdroid.ui.activePalette
import ai.magicbeans.magdroid.vibedev.CodingRunsNotifier
import ai.magicbeans.magdroid.vibedev.VibeDevCache
import ai.magicbeans.magdroid.vibedev.VibeDevChangedFile
import ai.magicbeans.magdroid.vibedev.VibeDevComposerViewModel
import ai.magicbeans.magdroid.vibedev.VibeDevConnection
import ai.magicbeans.magdroid.vibedev.VibeDevDiffViewModel
import ai.magicbeans.magdroid.vibedev.VibeDevLive
import ai.magicbeans.magdroid.vibedev.VibeDevNowViewModel
import ai.magicbeans.magdroid.vibedev.VibeDevProjectViewModel
import ai.magicbeans.magdroid.vibedev.VibeDevRunViewModel
import ai.magicbeans.magdroid.vibedev.VibeDevScreenModel
import android.content.Context
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.FolderOpen
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Switch
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.material3.Icon
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelStore
import androidx.lifecycle.ViewModelStoreOwner
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.viewmodel.compose.LocalViewModelStoreOwner
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch

/** A place in VibeDev's own navigation stack. */
sealed class VibeDevRoute {
    data object Home : VibeDevRoute()
    data class Project(val projectId: String, val name: String) : VibeDevRoute()
    data class Run(val runId: String, val title: String, val openChanges: Boolean = false) : VibeDevRoute()
    data class Diff(val runId: String, val turn: String?, val files: List<VibeDevChangedFile>, val path: String) : VibeDevRoute()

    /**
     * Which view models this screen owns: one store per key. A retitled run
     * keeps its key, and the same run opened twice shares one.
     */
    val storeKey: String
        get() = when (this) {
            Home -> "home"
            is Project -> "project:$projectId"
            is Run -> "run:$runId"
            is Diff -> "diff:$runId:${turn.orEmpty()}:$path"
        }
}

/**
 * One [ViewModelStore] per VibeDev route, so a popped run, project or diff
 * releases its view model (a diff keeps up to 256 KB of patches) instead of
 * living as long as the activity. Activity-scoped itself: it survives a
 * rotation and clears every store when the activity finishes.
 */
class VibeDevRouteStores : ViewModel() {
    private val stores = mutableMapOf<String, ViewModelStore>()

    fun owner(key: String): ViewModelStoreOwner {
        val store = stores.getOrPut(key) { ViewModelStore() }
        return object : ViewModelStoreOwner {
            override val viewModelStore: ViewModelStore = store
        }
    }

    /** Clear the stores of every route no longer on the stack. */
    fun retain(keys: Set<String>) {
        (stores.keys - keys).forEach { stores.remove(it)?.clear() }
    }

    fun clearAll() {
        stores.values.forEach(ViewModelStore::clear)
        stores.clear()
    }

    internal val keys: Set<String> get() = stores.keys.toSet()

    override fun onCleared() = clearAll()
}

/** A bottom sheet VibeDev is showing. */
sealed class VibeDevSheet {
    data class NewRun(
        val projectId: String?,
        val parentTaskId: String? = null,
        val parentTitle: String? = null,
        val crew: Boolean = false,
        val codingRouteId: String? = null,
        val engine: String? = null,
        val mode: String = "build",
    ) : VibeDevSheet()
    data class Steer(val title: String, val taskId: String? = null, val onSend: (String) -> Unit) : VibeDevSheet()
    data object NewProject : VibeDevSheet()
}

/**
 * VibeDev's navigation and the shell's top-bar state.
 *
 * The shell draws the title, back arrow and [VibeDevTopBarActions]; this
 * holds the stack under them. Back pops the stack, and only leaves VibeDev
 * from its home.
 */
@Stable
class VibeDevChrome(private val stores: VibeDevRouteStores? = null) {
    val stack = mutableStateListOf<VibeDevRoute>(VibeDevRoute.Home)
    var wrap by mutableStateOf(true)
    var refreshTick by mutableIntStateOf(0)
    var sheet by mutableStateOf<VibeDevSheet?>(null)
    /** Bumped when the active machine changes; every VibeDev screen starts over. */
    var connection by mutableIntStateOf(0)
        private set

    init {
        // A rebuilt shell (a recreated activity) starts at home: drop the rest.
        release()
    }

    /** The store [route]'s view models live in; null outside a shell (previews). */
    fun owner(route: VibeDevRoute): ViewModelStoreOwner? = stores?.owner(route.storeKey)

    val top: VibeDevRoute get() = stack.last()

    val title: String
        get() = when (val route = top) {
            VibeDevRoute.Home -> "VibeDev"
            is VibeDevRoute.Project -> route.name.ifBlank { "Project" }
            is VibeDevRoute.Run -> route.title.removePrefix("VibeDev · ").ifBlank { "Run" }
            is VibeDevRoute.Diff -> route.path.substringAfterLast('/')
        }

    fun push(route: VibeDevRoute) { stack.add(route) }

    /** Name the open run by its loaded title once it is known. */
    fun retitle(route: VibeDevRoute.Run, title: String) {
        val index = stack.lastIndexOf(route)
        if (index >= 0 && title.isNotBlank() && route.title != title) stack[index] = route.copy(title = title)
    }

    /** Pop one screen; false at home, where back leaves VibeDev. */
    fun back(): Boolean {
        if (stack.size <= 1) return false
        stack.removeAt(stack.lastIndex)
        release()
        return true
    }

    /** Opened from the drawer: start at Now. */
    fun reset() {
        stack.clear()
        stack.add(VibeDevRoute.Home)
        sheet = null
        release()
    }

    /**
     * The active machine changed: every view model (Now included) read the
     * old machine and kept its offline cache, so all of them go.
     */
    fun resetConnection() {
        stores?.clearAll()
        reset()
        connection++
    }

    private fun release() {
        stores?.retain(stack.mapTo(mutableSetOf()) { it.storeKey })
    }
}

/** The shell's top-bar actions for the current VibeDev screen. */
@Composable
fun VibeDevTopBarActions(chrome: VibeDevChrome) {
    key(chrome.connection) {
        when (chrome.top) {
            VibeDevRoute.Home -> {
                val model = vibeDevNowModel(chrome)
                val data by model.data.collectAsStateWithLifecycle()
                var projectsOpen by remember { mutableStateOf(false) }
                Box {
                    VibeIconButton(onClick = { projectsOpen = true }, compact = true) {
                        Icon(Icons.Outlined.FolderOpen, contentDescription = "Projects", tint = activePalette.text,
                            modifier = Modifier.size(18.dp))
                    }
                    DropdownMenu(expanded = projectsOpen, onDismissRequest = { projectsOpen = false },
                        containerColor = activePalette.elevated) {
                        if (data.openProjects.isEmpty()) {
                            DropdownMenuItem(text = { Text("No projects yet") }, onClick = {}, enabled = false)
                        }
                        data.openProjects.forEach { project ->
                            DropdownMenuItem(text = { Text(project.name, color = activePalette.text,
                                fontFamily = LocalMagicanFontFamilies.current.body) }, onClick = {
                                projectsOpen = false
                                chrome.push(VibeDevRoute.Project(project.projectId, project.name))
                            })
                        }
                    }
                }
                VibeIconButton(onClick = { chrome.sheet = VibeDevSheet.NewProject }, compact = true) {
                    Icon(Icons.Outlined.Add, contentDescription = "New project", tint = activePalette.text,
                        modifier = Modifier.size(18.dp))
                }
                VibeDevHomeMenu()
            }
            is VibeDevRoute.Diff -> TextButton(onClick = { chrome.wrap = !chrome.wrap }, shape = MagicanButtonShape) {
                Text(if (chrome.wrap) "Wrap" else "No wrap", color = activePalette.accent, fontSize = 13.sp,
                    fontWeight = FontWeight.SemiBold, fontFamily = LocalMagicanFontFamilies.current.body)
            }
            else -> VibeIconButton(onClick = { chrome.refreshTick++ }) {
                Icon(Icons.Outlined.Refresh, contentDescription = "Refresh", tint = activePalette.text)
            }
        }
    }
}

/** VibeDev's full-screen surface, under the shell's top bar. */
@Composable
fun VibeDevSurface(chrome: VibeDevChrome) {
    // A machine switch starts every screen over, with new view models.
    key(chrome.connection) { VibeDevScreens(chrome) }
}

@Composable
private fun VibeDevScreens(chrome: VibeDevChrome) {
    val snackbar = remember { SnackbarHostState() }
    val composer = vibeDevViewModel(chrome, VibeDevRoute.Home) { context -> VibeDevComposerViewModel({ VibeDevConnection.source(context) }) }
    val now = vibeDevNowModel(chrome)
    Box(Modifier.fillMaxSize()) {
        when (val route = chrome.top) {
            VibeDevRoute.Home -> VibeDevHomeScreen(chrome, now, snackbar)
            is VibeDevRoute.Project -> {
                val model = vibeDevViewModel(chrome, route) { context ->
                    VibeDevProjectViewModel(route.projectId, { VibeDevConnection.source(context) }, VibeDevLive.signals(context))
                }
                VibeDevProjectScreen(chrome, model, snackbar)
            }
            is VibeDevRoute.Run -> {
                val model = vibeDevViewModel(chrome, route) { context ->
                    VibeDevRunViewModel(route.runId, { VibeDevConnection.source(context) }, VibeDevLive.signals(context),
                        cache = { VibeDevCache.forConnection(context) },
                        identity = { MagicianAccess.activeIdentity(context) })
                }
                VibeDevRunScreen(chrome, route, model, composer, now, snackbar)
            }
            is VibeDevRoute.Diff -> {
                val model = vibeDevViewModel(chrome, route) { context ->
                    VibeDevDiffViewModel(route.runId, route.turn, route.files, route.path,
                        { VibeDevConnection.source(context) }, emptyFlow())
                }
                VibeDevDiffScreen(chrome, model)
            }
        }
        SnackbarHost(snackbar, Modifier.align(Alignment.BottomCenter).padding(bottom = 84.dp))
        VibeDevSheets(chrome, composer, now, snackbar)
    }
}

/** One home model shared by history and the header's project selector. */
@Composable
private fun vibeDevNowModel(chrome: VibeDevChrome): VibeDevNowViewModel =
    vibeDevViewModel(chrome, VibeDevRoute.Home) { context ->
        VibeDevNowViewModel(
            { VibeDevConnection.source(context) }, VibeDevLive.signals(context),
            cache = { VibeDevCache.forConnection(context) },
            // The open app keeps the "Magician at work" card current itself:
            // one `/work/live` read, or these runs from an older Magician.
            onRuns = { runs -> CodingRunsNotifier.refresh(context, fallbackRuns = runs) },
            identity = { MagicianAccess.activeIdentity(context) },
        )
    }

/**
 * A VibeDev view model built with the app context, in [route]'s own store
 * (cleared when the route leaves the stack), one per type there.
 */
@Composable
internal inline fun <reified VM : ViewModel> vibeDevViewModel(
    chrome: VibeDevChrome,
    route: VibeDevRoute,
    crossinline create: (Context) -> VM,
): VM {
    val context = LocalContext.current.applicationContext
    val owner = chrome.owner(route) ?: checkNotNull(LocalViewModelStoreOwner.current)
    return viewModel(
        viewModelStoreOwner = owner,
        key = "vibedev-${route.storeKey}-${VM::class.java.simpleName}",
        factory = viewModelFactory { initializer { create(context) } },
    )
}

/**
 * Follow live events while the screen is on and the app is in front; read
 * again on resume and when the top bar's Refresh is pressed.
 */
@Composable
internal fun AttachWhileVisible(model: VibeDevScreenModel, chrome: VibeDevChrome) {
    val owner = LocalLifecycleOwner.current
    DisposableEffect(owner, model) {
        val observer = LifecycleEventObserver { _, event ->
            when (event) {
                Lifecycle.Event.ON_START -> model.attach()
                Lifecycle.Event.ON_STOP -> model.detach()
                else -> Unit
            }
        }
        owner.lifecycle.addObserver(observer)
        onDispose {
            owner.lifecycle.removeObserver(observer)
            model.detach()
        }
    }
    val initialTick = remember(model) { chrome.refreshTick }
    LaunchedEffect(model, chrome.refreshTick) {
        if (chrome.refreshTick != initialTick) model.refresh()
    }
}

/** A view model's notices as snackbars, and its undo window as one with Undo. */
@Composable
internal fun VibeDevMessages(model: VibeDevScreenModel, snackbar: SnackbarHostState) {
    LaunchedEffect(model) {
        model.loadStatus.map { it.notice }.filterNotNull().collect { notice ->
            model.consumeNotice()
            launch { snackbar.showSnackbar(notice) }
        }
    }
    VibeDevUndoSnackbar(model.undo, snackbar)
}

/**
 * VibeDev's overflow: whether the "Magician at work" card lives in the
 * notification shade. Off also drops the push route, so the server stops
 * sending it.
 */
@Composable
private fun VibeDevHomeMenu() {
    val context = LocalContext.current
    var open by remember { mutableStateOf(false) }
    var inShade by remember { mutableStateOf(CodingRunsNotifier.isEnabled(context)) }
    val toggle = {
        inShade = !inShade
        ai.magicbeans.magdroid.push.AndroidMobilePushRegistration.setCodingRunsEnabled(context, inShade)
    }
    Box {
        VibeIconButton(onClick = { open = true }, compact = true) {
            Icon(Icons.Outlined.MoreVert, contentDescription = "More VibeDev options", tint = activePalette.text,
                modifier = Modifier.size(18.dp))
        }
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            DropdownMenuItem(
                text = {
                    Text("Show Magician at work in the notification shade", color = activePalette.text, fontSize = 14.sp,
                        fontFamily = LocalMagicanFontFamilies.current.body)
                },
                onClick = toggle,
                trailingIcon = {
                    Switch(
                        checked = inShade,
                        onCheckedChange = { toggle() },
                        colors = ai.magicbeans.magdroid.ui.magicanSwitchColors(),
                        modifier = Modifier.semantics { contentDescription = "Show Magician at work in the notification shade" },
                    )
                },
                modifier = Modifier.semantics { stateDescription = if (inShade) "On" else "Off" },
            )
        }
    }
}
