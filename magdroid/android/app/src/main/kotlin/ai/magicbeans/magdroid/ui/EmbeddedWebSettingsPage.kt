package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.access.PairedMachine
import ai.magicbeans.magdroid.access.RemoteMachineRegistry
import ai.magicbeans.magdroid.access.RemoteMachinesClient
import ai.magicbeans.magdroid.access.machinePickerRows
import ai.magicbeans.magdroid.identity.ProductIdentity
import android.annotation.SuppressLint
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.view.ViewGroup
import android.webkit.CookieManager
import android.webkit.WebChromeClient
import android.webkit.WebResourceError
import android.webkit.WebResourceRequest
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import java.net.URI
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.withTimeoutOrNull

/**
 * Settings → Magican settings: the shared web Settings, embedded.
 *
 * Everything that is not about this phone (budgets, harness credentials,
 * preferences, storage maintenance, speaking voices, trust rules…) is edited
 * here, on the paired machine's own web page, so there is one Settings to
 * keep correct rather than a native copy per platform.
 *
 * The page signs in with its normal web sign-in. The phone's paired-device
 * token is not a web session (the server refuses it for `/auth/session`), so
 * no Authorization or Cloudflare Access header is added. Cookies are kept
 * and flushed, so the one-time sign-in (and any Cloudflare Access login)
 * survives relaunches. JavaScript and DOM storage are on; file and content
 * access are off and no JavaScript interface is exposed.
 */
@Composable
internal fun EmbeddedWebSettingsPage(onOpenConnection: () -> Unit) {
    val context = LocalContext.current
    val configured = remember(context) { MagicianAccess.isConfigured(context) }
    if (!configured) {
        WebSettingsMessage(
            title = "Pair this phone with a machine first",
            detail = "${ProductIdentity.productName} settings open on the machine this phone is paired with. " +
                "Pair it in Settings → Connection, then come back here.",
            action = "Open Connection",
            onAction = onOpenConnection,
        )
        return
    }
    val activeOrigin = remember(context) { MagicianAccess.activeOrigin(context) }
    var resolving by remember(activeOrigin) { mutableStateOf(true) }
    var origin by remember(activeOrigin) { mutableStateOf<String?>(null) }
    LaunchedEffect(activeOrigin) {
        origin = resolveWebSettingsOrigin(context)
        resolving = false
    }
    val home = origin
    when {
        resolving -> Box(Modifier.fillMaxSize().background(Ground), contentAlignment = Alignment.Center) {
            Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(10.dp)) {
                CircularProgressIndicator(color = Coral, modifier = Modifier.size(28.dp))
                Text("Finding your machine…", color = Muted, fontSize = 13.sp)
            }
        }
        home == null -> WebSettingsMessage(
            title = "This connection has no web address",
            detail = "Pair this phone again from the machine's Settings → Mobile devices, then try again.",
            action = "Open Connection",
            onAction = onOpenConnection,
        )
        else -> key(home) { EmbeddedWebSettings(home) }
    }
}

/**
 * The top bar's Refresh for the embedded page. The bar belongs to the shell,
 * the WebView to this page, so a press bumps [requests] and the page reloads
 * itself once per new value.
 */
internal object EmbeddedWebSettingsRefresh {
    var requests by mutableIntStateOf(0)
        private set

    fun request() {
        requests += 1
    }
}

/** Live state of the embedded page; written from WebView callbacks on the main thread. */
private class WebSettingsPageState {
    var progress by mutableIntStateOf(0)
    var failure by mutableStateOf<String?>(null)
    var canGoBack by mutableStateOf(false)
    var failedUrl: String? = null
    var view: WebView? = null
    var lastBounceUrl: String? = null
    var lastBounceAtMs: Long = 0L
}

@SuppressLint("SetJavaScriptEnabled")
@Composable
private fun EmbeddedWebSettings(origin: String) {
    val context = LocalContext.current
    val url = remember(origin) { webSettingsUrl(origin) }
    val state = remember(origin) { WebSettingsPageState() }
    val background = Ground.toArgb()
    // Back walks the page's own history first; with none left, the shell's
    // Settings back handler returns to the Settings list.
    BackHandler(enabled = state.canGoBack) { state.view?.goBack() }
    DisposableEffect(origin) {
        onDispose { CookieManager.getInstance().flush() }
    }
    // Reload on a top-bar Refresh, but not for a press that predates this page.
    val refreshes = EmbeddedWebSettingsRefresh.requests
    var handledRefresh by remember(origin) { mutableIntStateOf(refreshes) }
    LaunchedEffect(refreshes) {
        if (refreshes == handledRefresh) return@LaunchedEffect
        handledRefresh = refreshes
        val view = state.view ?: return@LaunchedEffect
        state.progress = 0
        if (state.failure != null) {
            state.failure = null
            view.loadUrl(state.failedUrl ?: url)
        } else {
            view.reload()
        }
    }
    Box(Modifier.fillMaxSize().background(Ground)) {
        AndroidView(
            factory = { viewContext ->
                CookieManager.getInstance().setAcceptCookie(true)
                WebView(viewContext).apply {
                    // AndroidView's default WRAP_CONTENT height makes WebView
                    // force a zero layout height, so every vh/dvh unit on the
                    // page reads 0 and the full-height Settings pane collapses
                    // to a blank screen. The view is sized by its constraints.
                    layoutParams = ViewGroup.LayoutParams(
                        ViewGroup.LayoutParams.MATCH_PARENT,
                        ViewGroup.LayoutParams.MATCH_PARENT,
                    )
                    setBackgroundColor(background)
                    settings.javaScriptEnabled = true
                    settings.domStorageEnabled = true
                    settings.allowFileAccess = false
                    settings.allowContentAccess = false
                    webViewClient = SettingsWebViewClient(context, origin, url, state)
                    webChromeClient = object : WebChromeClient() {
                        override fun onProgressChanged(view: WebView?, newProgress: Int) {
                            state.progress = newProgress
                        }
                    }
                    state.view = this
                    loadUrl(url)
                }
            },
            modifier = Modifier.fillMaxSize(),
            onRelease = { view ->
                state.view = null
                CookieManager.getInstance().flush()
                view.stopLoading()
                view.destroy()
            },
        )
        if (state.failure == null && state.progress < 100) {
            LinearProgressIndicator(
                progress = { state.progress / 100f },
                modifier = Modifier.fillMaxWidth().height(2.dp).align(Alignment.TopCenter),
                color = Coral,
                trackColor = Color.Transparent,
            )
        }
        state.failure?.let { message ->
            WebSettingsMessage(
                title = "Couldn't open ${ProductIdentity.productName} settings",
                detail = "$message\n${origin.substringAfter("://")}",
                action = "Retry",
                onAction = {
                    state.failure = null
                    state.progress = 0
                    state.view?.loadUrl(state.failedUrl ?: url)
                },
            )
        }
    }
}

private class SettingsWebViewClient(
    private val context: Context,
    private val origin: String,
    private val settingsUrl: String,
    private val state: WebSettingsPageState,
) : WebViewClient() {
    override fun shouldOverrideUrlLoading(view: WebView?, request: WebResourceRequest?): Boolean {
        if (request == null || !request.isForMainFrame) return false
        val target = request.url.toString()
        return when (webSettingsNavigation(target, origin, view?.url, request.isRedirect)) {
            WebSettingsNavigation.Stay -> false
            WebSettingsNavigation.External -> {
                openExternally(context, target)
                true
            }
        }
    }

    /**
     * The web app routes its own links without a page load, which never
     * reaches [shouldOverrideUrlLoading]. A route that leaves Settings is
     * opened in the browser and the page steps back to Settings.
     */
    override fun doUpdateVisitedHistory(view: WebView?, url: String?, isReload: Boolean) {
        super.doUpdateVisitedHistory(view, url, isReload)
        if (view == null || url == null) return
        val history = view.copyBackForwardList()
        val current = history.currentItem?.url
        val previous = when {
            current != url -> current
            history.currentIndex > 0 -> history.getItemAtIndex(history.currentIndex - 1)?.url
            else -> null
        }
        val now = System.currentTimeMillis()
        val repeated = url == state.lastBounceUrl && now - state.lastBounceAtMs < BOUNCE_REPEAT_GUARD_MS
        if (!repeated && webSettingsLeavesInPlace(url, previous, origin)) {
            state.lastBounceUrl = url
            state.lastBounceAtMs = now
            openExternally(context, url)
            if (view.canGoBack()) view.goBack() else view.loadUrl(settingsUrl)
        }
        state.canGoBack = view.canGoBack()
    }

    override fun onPageFinished(view: WebView?, url: String?) {
        super.onPageFinished(view, url)
        state.canGoBack = view?.canGoBack() == true
    }

    override fun onReceivedError(view: WebView?, request: WebResourceRequest?, error: WebResourceError?) {
        super.onReceivedError(view, request, error)
        if (request == null || !request.isForMainFrame) return
        state.failedUrl = request.url.toString()
        state.failure = error?.description?.toString()?.takeIf(String::isNotBlank)
            ?: "The page did not load."
    }
}

@Composable
private fun WebSettingsMessage(title: String, detail: String, action: String, onAction: () -> Unit) {
    Box(Modifier.fillMaxSize().background(Ground).padding(24.dp), contentAlignment = Alignment.Center) {
        Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Text(title, color = Ink, fontSize = 16.sp, fontWeight = FontWeight.SemiBold, textAlign = TextAlign.Center)
            Text(detail, color = Muted, fontSize = 13.sp, lineHeight = 18.sp, textAlign = TextAlign.Center)
            Button(onClick = onAction, shape = MagicanButtonShape, colors = magicanButtonColors()) { Text(action) }
        }
    }
}

private fun openExternally(context: Context, url: String) {
    try {
        context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    } catch (_: ActivityNotFoundException) {
        // Nothing can open it; the page stays where it was.
    }
}

/**
 * The web UI origin for the active machine: its registry `ui_origin` when the
 * machine lists one, else the connection's own public origin.
 */
private suspend fun resolveWebSettingsOrigin(context: Context): String? {
    val apiOrigin = MagicianAccess.baseUrl(context).takeIf(String::isNotBlank)
    val client = RemoteMachinesClient(context)
    val registry = try {
        withTimeoutOrNull(REGISTRY_TIMEOUT_MS) { client.fetch() }
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (_: Exception) {
        null
    } finally {
        client.close()
    }
    return webSettingsOrigin(MagicianAccess.listProfiles(context), registry, apiOrigin)
}

private const val REGISTRY_TIMEOUT_MS = 4_000L
private const val BOUNCE_REPEAT_GUARD_MS = 2_000L

/** Prefer the active machine's UI origin from the registry; fall back to [apiOrigin]. */
internal fun webSettingsOrigin(
    paired: List<PairedMachine>,
    registry: RemoteMachineRegistry?,
    apiOrigin: String?,
): String? {
    val uiOrigin = machinePickerRows(paired, registry).firstOrNull { it.active }?.uiOrigin
        ?: registry?.machines?.firstOrNull { it.isSelf }?.uiOrigin
    return listOfNotNull(uiOrigin, apiOrigin)
        .map { it.trim().trimEnd('/') }
        .firstOrNull { it.startsWith("https://", ignoreCase = true) || it.startsWith("http://", ignoreCase = true) }
}

internal fun webSettingsUrl(origin: String): String = "${origin.trim().trimEnd('/')}/settings?embed=1&host=android"

internal enum class WebSettingsNavigation { Stay, External }

/**
 * Where a main-frame navigation goes. Settings and sign-in pages on the
 * machine's origin stay in the page; so do server redirects and every step
 * of a sign-in that is already on another origin (Cloudflare Access, an
 * identity provider), which must finish here for its cookie to land in this
 * WebView. Anything else opens in the browser.
 */
internal fun webSettingsNavigation(
    target: String,
    origin: String,
    currentUrl: String?,
    isRedirect: Boolean,
): WebSettingsNavigation {
    val uri = parseHttpUri(target) ?: return WebSettingsNavigation.External
    val home = parseHttpUri(origin) ?: return WebSettingsNavigation.External
    if (sameOrigin(uri, home) && keepsInPage(uri.rawPath.orEmpty())) return WebSettingsNavigation.Stay
    if (isRedirect) return WebSettingsNavigation.Stay
    val current = currentUrl?.let(::parseHttpUri)
    if (current != null && !sameOrigin(current, home)) return WebSettingsNavigation.Stay
    return WebSettingsNavigation.External
}

/**
 * True when the web app routed itself from Settings to another of its own
 * pages without a page load: that page belongs in the browser.
 */
internal fun webSettingsLeavesInPlace(url: String, previousUrl: String?, origin: String): Boolean {
    val uri = parseHttpUri(url) ?: return false
    val home = parseHttpUri(origin) ?: return false
    if (!sameOrigin(uri, home) || keepsInPage(uri.rawPath.orEmpty())) return false
    val previous = previousUrl?.let(::parseHttpUri) ?: return false
    return sameOrigin(previous, home) && isSettingsPath(previous.rawPath.orEmpty())
}

private fun isSettingsPath(path: String): Boolean = path == "/settings" || path.startsWith("/settings/")

/** Settings, the web sign-in, and Cloudflare Access's own callback paths on the origin. */
private fun keepsInPage(path: String): Boolean =
    isSettingsPath(path) || path.startsWith("/login") || path.startsWith("/cdn-cgi/access/")

private fun parseHttpUri(raw: String): URI? {
    val uri = runCatching { URI(raw.trim()) }.getOrNull() ?: return null
    val scheme = uri.scheme?.lowercase() ?: return null
    if (scheme != "https" && scheme != "http") return null
    if (uri.host.isNullOrBlank()) return null
    return uri
}

private fun sameOrigin(a: URI, b: URI): Boolean =
    a.scheme.equals(b.scheme, ignoreCase = true) &&
        a.host.equals(b.host, ignoreCase = true) &&
        effectivePort(a) == effectivePort(b)

private fun effectivePort(uri: URI): Int = when {
    uri.port != -1 -> uri.port
    uri.scheme.equals("https", ignoreCase = true) -> 443
    else -> 80
}
