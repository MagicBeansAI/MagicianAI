package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.attention.BrowserSignInClient
import ai.magicbeans.magdroid.attention.BrowserSignInPageChanged
import ai.magicbeans.magdroid.attention.AttentionError
import ai.magicbeans.magdroid.attention.BotConnectState
import ai.magicbeans.magdroid.attention.ManagedBotMethod
import ai.magicbeans.magdroid.attention.ServiceAccount
import ai.magicbeans.magdroid.attention.ServiceAccountStatus
import ai.magicbeans.magdroid.access.MagicianConnectionSnapshot
import android.content.Intent
import android.graphics.BitmapFactory
import android.net.Uri
import android.util.Base64
import androidx.compose.foundation.Image
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.sp
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.SecureFlagPolicy
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

@Composable
internal fun BrowserSignInButton(
    correlationId: String? = null,
    skill: String? = null,
    expected: MagicianConnectionSnapshot? = null,
    endpointAlias: String? = null,
    label: String? = null,
    onLaunched: () -> Unit = {},
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf("") }
    var viewer by remember { mutableStateOf<Pair<BrowserSignInClient, String>?>(null) }
    DisposableEffect(Unit) { onDispose { viewer?.first?.close() } }
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Button(shape = RoundedCornerShape(8.dp), contentPadding = PaddingValues(horizontal = 12.dp, vertical = 6.dp), modifier = Modifier.fillMaxWidth().heightIn(min = 40.dp), colors = magicanButtonColors(), enabled = !busy, onClick = {
            busy = true; error = ""
            scope.launch {
                var client: BrowserSignInClient? = null
                try {
                    val connection = BrowserSignInClient(context, expected)
                    client = connection
                    val result = connection.launch(correlationId, skill, endpointAlias = endpointAlias)
                    when (result["mode"]?.jsonPrimitive?.content) {
                        "direct" -> {
                            val url = result["authorization_url"]?.jsonPrimitive?.content.orEmpty()
                            val uri = java.net.URI(url)
                            require(uri.scheme == "https" && uri.userInfo == null)
                            context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
                        }
                        "connected" -> { error = "Connected."; onLaunched() }
                        "computer" -> viewer = connection to (result["ticket"]?.jsonPrimitive?.content ?: throw IllegalStateException("Invalid sign-in response"))
                        else -> throw IllegalStateException("Invalid sign-in response")
                    }
                } catch (cancelled: CancellationException) { throw cancelled }
                catch (_: Exception) { error = "Sign-in could not open. Reopen the request or finish on the computer." }
                finally { if (viewer?.first !== client) client?.close(); busy = false }
            }
        }) { Text(if (busy) "Opening sign-in…" else label ?: "Finish sign-in here") }
        if (correlationId != null || skill != null) {
            OutlinedButton(shape = RoundedCornerShape(8.dp), contentPadding = PaddingValues(horizontal = 12.dp, vertical = 6.dp), modifier = Modifier.fillMaxWidth().heightIn(min = 40.dp), colors = magicanButtonColors(), enabled = !busy, onClick = {
                busy = true; error = ""
                scope.launch {
                    var client: BrowserSignInClient? = null
                    try {
                        client = BrowserSignInClient(context, expected)
                        client.launch(correlationId, skill, openOnComputer = true, endpointAlias = endpointAlias)
                        error = "Opened on the computer. Waiting for the service to confirm sign-in."
                        onLaunched()
                    } catch (cancelled: CancellationException) { throw cancelled }
                    catch (_: Exception) { error = "Could not open sign-in on the computer." }
                    finally { client?.close(); busy = false }
                }
            }) { Text("Open on computer") }
        }
        if (error.isNotEmpty()) Text(error, style = MaterialTheme.typography.bodySmall)
    }
    viewer?.let { (client, ticket) -> BrowserSignInDialog(client, ticket) { client.close(); viewer = null; onLaunched() } }
}

@Composable
internal fun ServiceAccountsButton() {
    val context = LocalContext.current
    var open by remember { mutableStateOf(false) }
    var accounts by remember { mutableStateOf<List<ServiceAccount>>(emptyList()) }
    var accountConnection by remember { mutableStateOf<MagicianConnectionSnapshot?>(null) }
    var message by remember { mutableStateOf("") }
    var connectingBot by remember { mutableStateOf<ServiceAccount?>(null) }
    // Bumped on resume and after a launch: sign-in often finishes in the browser or on the computer.
    var reload by remember { mutableIntStateOf(0) }
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    DisposableEffect(lifecycle) {
        val observer = LifecycleEventObserver { _, event -> if (event == Lifecycle.Event.ON_RESUME) reload++ }
        lifecycle.addObserver(observer)
        onDispose { lifecycle.removeObserver(observer) }
    }
    FilledTonalButton(shape = RoundedCornerShape(8.dp), contentPadding = PaddingValues(horizontal = 12.dp, vertical = 6.dp), modifier = Modifier.fillMaxWidth().heightIn(min = 40.dp), onClick = { open = true }) { Text("Service accounts") }
    if (open) {
        LaunchedEffect(reload) {
            var client: BrowserSignInClient? = null
            try {
                client = BrowserSignInClient(context)
                accountConnection = client.originConnection
                accounts = ServiceAccount.parseList(client.accounts())
                message = if (accounts.isEmpty()) "No supported accounts found. Install a service skill from Skills first." else ""
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) { if (accounts.isEmpty()) message = "Could not load accounts. Check the Magican connection." }
            finally { client?.close() }
        }
        Dialog(onDismissRequest = { open = false }) {
            Surface(shape = MaterialTheme.shapes.large) {
                Column(Modifier.padding(20.dp).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                    Text("Service accounts", style = MaterialTheme.typography.titleLarge)
                    Text("Connect an installed service. Credentials stay on your Magican computer.", style = MaterialTheme.typography.bodySmall)
                    if (message.isNotEmpty()) Text(message)
                    accounts.forEach { account ->
                        key(account.key) {
                            OutlinedCard(Modifier.fillMaxWidth()) {
                                Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                                    Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                        Text(account.name, style = MaterialTheme.typography.titleSmall)
                                        ServiceAccountStatusChip(account.status)
                                    }
                                    Text(account.label, style = MaterialTheme.typography.bodySmall)
                                    account.detail?.let { Text(it, style = MaterialTheme.typography.labelSmall) }
                                    val label = if (account.status == ServiceAccountStatus.CONNECTED) "Reconnect" else "Connect"
                                    when {
                                        // Missing binary or environment: nothing to connect until it is installed.
                                        account.status == ServiceAccountStatus.UNAVAILABLE -> Unit
                                        account.managedBot != null && account.methods.isNotEmpty() ->
                                            Button(colors = magicanButtonColors(), shape = RoundedCornerShape(8.dp), contentPadding = PaddingValues(horizontal = 12.dp, vertical = 6.dp), modifier = Modifier.fillMaxWidth().heightIn(min = 40.dp), onClick = { connectingBot = account }) { Text(label) }
                                        account.managedBot != null -> Unit
                                        else -> BrowserSignInButton(
                                            skill = account.name,
                                            expected = accountConnection,
                                            endpointAlias = account.endpointAlias,
                                            label = label,
                                            onLaunched = { reload++ },
                                        )
                                    }
                                }
                            }
                        }
                    }
                    TextButton(onClick = { open = false }) { Text("Close") }
                }
            }
        }
        connectingBot?.let { account ->
            key(account.key) {
                ManagedBotConnectDialog(account, accountConnection) { connectingBot = null; reload++ }
            }
        }
    }
}

@Composable
private fun ServiceAccountStatusChip(status: ServiceAccountStatus) {
    val color = when (status) {
        ServiceAccountStatus.CONNECTED -> androidx.compose.ui.graphics.Color(0xFF00A86B)
        ServiceAccountStatus.NEEDS_SIGN_IN -> MaterialTheme.colorScheme.tertiary
        ServiceAccountStatus.AUTHENTICATING -> MaterialTheme.colorScheme.primary
        ServiceAccountStatus.UNKNOWN, ServiceAccountStatus.UNAVAILABLE -> MaterialTheme.colorScheme.onSurfaceVariant
    }
    Surface(shape = RoundedCornerShape(50), color = color.copy(alpha = 0.15f), contentColor = color) {
        Text(status.title, style = MaterialTheme.typography.labelSmall, modifier = Modifier.padding(horizontal = 8.dp, vertical = 2.dp))
    }
}

@Composable
private fun BrowserSignInDialog(client: BrowserSignInClient, ticket: String, onDismiss: () -> Unit) {
    var image by remember { mutableStateOf<ImageBitmap?>(null) }
    var origin by remember { mutableStateOf("Starting private sign-in…") }
    var message by remember { mutableStateOf("") }
    var typed by remember { mutableStateOf("") }
    var revision by remember { mutableStateOf("") }
    var sequence by remember { mutableLongStateOf(0) }
    var polling by remember { mutableStateOf(false) }
    var sending by remember { mutableStateOf(false) }
    var closed by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val lifecycle = LocalLifecycleOwner.current.lifecycle

    suspend fun refresh() {
        if (closed || polling) return
        polling = true
        try {
            val frame = client.frame()
            if (closed) return
            if (!lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED)) { image = null; typed = ""; return }
            val status = frame["status"]?.jsonPrimitive?.content
            if (status != "waiting") {
                message = if (status == "connected") "Connected. You can return to Magican." else "This sign-in has ended. Reopen the request to retry."
                closed = true; image = null; typed = ""; client.close(); return
            }
            if (frame["retry_after_ms"] != null) return
            val encoded = frame["image"]?.jsonPrimitive?.content
            if (encoded == null) { revision = ""; image = null; origin = "Loading…"; return }
            val bytes = Base64.decode(encoded, Base64.DEFAULT)
            image = BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap()
            origin = frame["origin"]?.jsonPrimitive?.content.orEmpty()
            revision = frame["frame_revision"]?.jsonPrimitive?.content.orEmpty()
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (_: Exception) { message = "Connection interrupted. Reopen the sign-in request."; closed = true; image = null; client.close() }
        finally { polling = false }
    }

    fun send(value: JsonObject) {
        if (closed || sending || revision.isEmpty()) return
        sending = true
        val expected = revision
        scope.launch {
            try {
                while (polling && !closed) delay(30)
                if (!closed) { client.input(++sequence, expected, value); refresh() }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (_: BrowserSignInPageChanged) { revision = ""; image = null; message = "The page changed. Review the picture before trying again."; refresh() }
            catch (_: Exception) { message = "Input was not confirmed and was not retried. Reopen sign-in to continue."; closed = true; image = null; typed = ""; client.close() }
            finally { sending = false }
        }
    }

    LaunchedEffect(client) {
        try {
            if (!client.claim(ticket)) { message = "Connected. You can return to Magican."; closed = true; client.close(); return@LaunchedEffect }
            lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) {
                try { while (isActive && !closed) { if (!sending) refresh(); delay(700) } }
                finally { typed = ""; image = null }
            }
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (error: AttentionError) { message = error.message ?: "Sign-in could not start."; closed = true; client.close() }
        catch (_: Exception) { message = "Sign-in could not start. Reopen the request or finish on the computer."; closed = true; client.close() }
    }
    DisposableEffect(client) { onDispose { typed = ""; image = null; client.close() } }

    Dialog(onDismissRequest = onDismiss, properties = DialogProperties(usePlatformDefaultWidth = false, securePolicy = SecureFlagPolicy.SecureOn)) {
        Surface(modifier = Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.surface) {
            Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                    Text("Finish sign-in", style = MaterialTheme.typography.titleLarge, modifier = Modifier.weight(1f))
                    TextButton(onClick = onDismiss) { Text("Close") }
                }
                Text("This private browser runs on your Magican computer. Input is kept out of agent history.", style = MaterialTheme.typography.bodySmall)
                Text(origin, style = MaterialTheme.typography.labelSmall)
                if (message.isNotEmpty()) Text(message)
                image?.let { frame ->
                    Image(frame, contentDescription = "Live sign-in page on your computer", modifier = Modifier.fillMaxWidth().aspectRatio(390f / 720f)
                        .pointerInput(revision, closed, sending) { detectTapGestures { point -> send(buildJsonObject {
                            put("kind", "tap"); put("x", point.x / size.width); put("y", point.y / size.height)
                        }) } })
                }
                Row(Modifier.fillMaxWidth().height(IntrinsicSize.Min), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    for ((label, delta) in listOf("Scroll up" to -400, "Scroll down" to 400)) {
                        OutlinedButton(modifier = Modifier.weight(1f).fillMaxHeight().heightIn(min = 40.dp), shape = RoundedCornerShape(8.dp), contentPadding = PaddingValues(horizontal = 8.dp, vertical = 6.dp), enabled = !closed && !sending && revision.isNotEmpty(), onClick = { send(buildJsonObject { put("kind", "scroll"); put("delta", delta) }) }) { Text(label, textAlign = TextAlign.Center) }
                    }
                }
                Row(Modifier.fillMaxWidth().height(IntrinsicSize.Min), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    for ((label, key) in listOf("Next field" to "Tab", "Delete" to "Backspace", "Enter" to "Enter")) {
                        OutlinedButton(modifier = Modifier.weight(1f).fillMaxHeight().heightIn(min = 40.dp), shape = RoundedCornerShape(8.dp), contentPadding = PaddingValues(horizontal = 8.dp, vertical = 6.dp), enabled = !closed && !sending && revision.isNotEmpty(), onClick = { send(buildJsonObject { put("kind", "key"); put("key", key) }) }) { Text(label, textAlign = TextAlign.Center) }
                    }
                }
                OutlinedTextField(value = typed, onValueChange = { if (it.length <= 4096) typed = it }, label = { Text("Text for the selected field") },
                    visualTransformation = PasswordVisualTransformation(), keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                    enabled = !closed && !sending && revision.isNotEmpty(), modifier = Modifier.fillMaxWidth())
                Button(shape = RoundedCornerShape(8.dp), contentPadding = PaddingValues(horizontal = 12.dp, vertical = 6.dp), modifier = Modifier.fillMaxWidth().heightIn(min = 40.dp), colors = magicanButtonColors(), enabled = !closed && !sending && typed.isNotEmpty(), onClick = {
                    val value = typed; typed = ""; send(buildJsonObject { put("kind", "text"); put("value", value) })
                }) { Text("Enter text") }
                Text("Tap a field in the picture before entering text. Some providers require finishing on the computer.", style = MaterialTheme.typography.bodySmall)
            }
        }
    }
}

/**
 * Managed-bot connect (Google Workspace, Telegram, WhatsApp) over `bot/start|input|state|cancel`:
 * method choice, phone, code, two-step password, WhatsApp pairing code, QR, or Google's
 * computer-mode viewer. Polls `state` every 2 s while signing in.
 */
@Composable
private fun ManagedBotConnectDialog(account: ServiceAccount, expected: MagicianConnectionSnapshot?, onDone: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val copy = rememberClipboardCopy()
    val bot = account.managedBot ?: account.name
    val methods = account.methods
    val client = remember { runCatching { BrowserSignInClient(context, expected) }.getOrNull() }
    DisposableEffect(client) { onDispose { client?.close() } }
    var method by remember { mutableStateOf(methods.firstOrNull() ?: ManagedBotMethod.QR) }
    var started by remember { mutableStateOf(false) }
    var phone by remember { mutableStateOf("") }
    var field by remember { mutableStateOf("") }
    var state by remember { mutableStateOf<BotConnectState?>(null) }
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf(if (client == null) "Connect to your Magican computer first." else "") }
    var copied by remember { mutableStateOf(false) }
    var openedTicket by remember { mutableStateOf("") }
    var viewer by remember { mutableStateOf<Pair<BrowserSignInClient, String>?>(null) }
    var applied by remember { mutableIntStateOf(0) }
    var latest by remember { mutableIntStateOf(0) }
    val pendingKind = state?.pending?.kind
    LaunchedEffect(pendingKind) { field = ""; copied = false }
    DisposableEffect(Unit) { onDispose { field = ""; viewer?.first?.close() } }

    fun applyState(next: BotConnectState?) {
        if (next == null) return
        state = next
        applied++
    }

    /** Runs one call; a reply older than a newer request is dropped. */
    suspend fun call(block: suspend (BrowserSignInClient) -> BotConnectState): BotConnectState? {
        val connection = client ?: throw AttentionError("Connect to your Magican computer first.")
        val mine = ++latest
        val next = block(connection)
        return next.takeIf { mine == latest }
    }

    fun launchCall(block: suspend (BrowserSignInClient) -> BotConnectState, after: () -> Unit = {}) {
        if (busy) return
        busy = true; message = ""
        scope.launch {
            try { applyState(call(block)); after() }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (error: AttentionError) { message = error.message ?: "Sign-in could not continue." }
            catch (_: Exception) { message = "Sign-in could not continue. Check the Magican connection." }
            finally { busy = false }
        }
    }

    fun start(onComputer: Boolean = false) {
        val number = phone.trim()
        if (method.needsPhone && number.isEmpty()) return
        openedTicket = ""
        launchCall({ it.botStart(bot, method, number.takeIf { method.needsPhone }, openOnComputer = onComputer) }) { started = true }
    }

    fun submit(step: String) {
        val value = field.trim()
        if (value.isEmpty() || busy) return
        // Never keep a two-step password (or a code) in the form once it is sent.
        field = ""
        launchCall({ it.botInput(bot, step, value) })
    }

    LaunchedEffect(Unit) { if (client != null && methods.size <= 1 && !method.needsPhone) start() }

    LaunchedEffect(applied) {
        val current = state ?: return@LaunchedEffect
        when (current.status) {
            ServiceAccountStatus.CONNECTED -> { delay(1_500); onDone() }
            ServiceAccountStatus.AUTHENTICATING -> while (isActive) {
                delay(2_000)
                if (busy) continue
                // A reply overtaken by a newer request is dropped; that request restarts this loop.
                try { call { it.botState(bot) }?.let { applyState(it); return@LaunchedEffect } }
                catch (cancelled: CancellationException) { throw cancelled }
                catch (error: Exception) { message = error.message ?: "Could not check sign-in." }
            }
            else -> Unit
        }
    }

    val authenticating = state?.status == ServiceAccountStatus.AUTHENTICATING
    fun dismiss() {
        if (started && authenticating) launchCall({ it.botCancel(bot) }) { onDone() } else onDone()
    }

    Dialog(onDismissRequest = { dismiss() }, properties = DialogProperties(securePolicy = SecureFlagPolicy.SecureOn)) {
        Surface(shape = MaterialTheme.shapes.large) {
            Column(Modifier.padding(20.dp).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text(account.label, style = MaterialTheme.typography.titleLarge)
                val current = state
                if (!started) {
                    if (methods.size > 1) {
                        Text("How to connect", style = MaterialTheme.typography.labelLarge)
                        Column(Modifier.selectableGroup()) {
                            methods.forEach { option ->
                                Row(Modifier.fillMaxWidth().heightIn(min = 44.dp).selectable(selected = method == option, role = Role.RadioButton, onClick = { method = option }),
                                    verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                                    RadioButton(selected = method == option, onClick = null)
                                    Text(option.label(bot), Modifier.padding(start = 8.dp))
                                }
                            }
                        }
                    }
                    if (method.needsPhone) PhoneField(phone) { phone = it }
                    Button(colors = magicanButtonColors(), shape = RoundedCornerShape(8.dp), modifier = Modifier.fillMaxWidth().heightIn(min = 40.dp),
                        enabled = client != null && !busy && !(method.needsPhone && phone.isBlank()), onClick = { start() }) { Text(if (busy) "Starting…" else "Continue") }
                } else if (current == null) {
                    CircularProgressIndicator()
                } else if (current.status == ServiceAccountStatus.CONNECTED) {
                    Text("Connected.", color = androidx.compose.ui.graphics.Color(0xFF00A86B), style = MaterialTheme.typography.titleMedium)
                } else {
                    current.detail?.let { Text(it) }
                    val pending = current.pending
                    if (authenticating && pending != null) {
                        when (pending.kind) {
                            BotConnectState.PendingKind.PHONE -> {
                                PhoneField(field, pending.label) { field = it }
                                SubmitButton("Send", !busy && field.isNotBlank()) { submit("phone") }
                            }
                            BotConnectState.PendingKind.CODE -> {
                                OutlinedTextField(value = field, onValueChange = { if (it.length <= 16) field = it }, label = { Text(pending.label ?: "Code") },
                                    singleLine = true, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.NumberPassword), modifier = Modifier.fillMaxWidth())
                                SubmitButton("Send code", !busy && field.isNotBlank()) { submit("code") }
                            }
                            BotConnectState.PendingKind.PASSWORD -> {
                                OutlinedTextField(value = field, onValueChange = { if (it.length <= 256) field = it }, label = { Text(pending.label ?: "Two-step password") },
                                    singleLine = true, visualTransformation = PasswordVisualTransformation(), keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                                    modifier = Modifier.fillMaxWidth())
                                Text("Sent once to the sign-in. Never saved.", style = MaterialTheme.typography.bodySmall)
                                SubmitButton("Send", !busy && field.isNotBlank()) { submit("password") }
                            }
                            BotConnectState.PendingKind.PAIRING_CODE -> pending.pairingCode?.let { code ->
                                Text(code, fontFamily = FontFamily.Monospace, fontWeight = FontWeight.Bold, fontSize = 32.sp, letterSpacing = 4.sp,
                                    modifier = Modifier.fillMaxWidth(), textAlign = TextAlign.Center)
                                OutlinedButton(shape = RoundedCornerShape(8.dp), modifier = Modifier.fillMaxWidth().heightIn(min = 40.dp),
                                    onClick = { copied = copy(code, "pairing code") }) { Text(if (copied) "Copied" else "Copy") }
                                Text("In WhatsApp: Settings → Linked devices → Link a device → Link with phone number instead, then enter this code.", style = MaterialTheme.typography.bodySmall)
                            }
                            BotConnectState.PendingKind.QR -> {
                                val image = remember(pending.qrPngBase64) {
                                    pending.qrPngBase64?.let { encoded ->
                                        runCatching { Base64.decode(encoded, Base64.DEFAULT).let { BitmapFactory.decodeByteArray(it, 0, it.size)?.asImageBitmap() } }.getOrNull()
                                    }
                                }
                                if (image != null) {
                                    Image(image, contentDescription = "QR code to scan", filterQuality = androidx.compose.ui.graphics.FilterQuality.None,
                                        modifier = Modifier.fillMaxWidth().aspectRatio(1f))
                                } else {
                                    Text("The QR code could not be shown. Try again, or use the code option.")
                                }
                                if (methods.size > 1) Text("Scan from another device, or use the code option.", style = MaterialTheme.typography.bodySmall)
                            }
                            BotConnectState.PendingKind.LOGIN_URL -> {
                                val ticket = pending.signInTicket
                                if (pending.signInMode == "computer_opened") {
                                    Text("Finish the sign-in on your computer — this updates automatically.")
                                } else {
                                    if (pending.signInMode == "computer" && ticket != null && ticket != openedTicket) {
                                        SubmitButton("Open sign-in", !busy) {
                                            runCatching { BrowserSignInClient(context, expected) }
                                                .onSuccess { viewer = it to ticket; openedTicket = ticket }
                                                .onFailure { message = it.message ?: "Sign-in could not open." }
                                        }
                                    } else {
                                        Text("Finish signing in in the sign-in window.", style = MaterialTheme.typography.bodySmall)
                                    }
                                    OutlinedButton(shape = RoundedCornerShape(8.dp), modifier = Modifier.fillMaxWidth().heightIn(min = 40.dp),
                                        enabled = !busy, onClick = { start(onComputer = true) }) { Text("Open on computer") }
                                }
                            }
                        }
                    } else if (authenticating) {
                        Text("Waiting…", style = MaterialTheme.typography.bodySmall)
                    } else {
                        SubmitButton("Try again", true) { state = null; started = false; message = "" }
                    }
                }
                if (message.isNotEmpty()) Text(message, color = MaterialTheme.colorScheme.error)
                TextButton(enabled = !(busy && authenticating), onClick = { dismiss() }) { Text(if (started && authenticating) "Cancel" else "Close") }
            }
        }
    }
    viewer?.let { (viewerClient, ticket) -> BrowserSignInDialog(viewerClient, ticket) { viewerClient.close(); viewer = null } }
}

@Composable
private fun PhoneField(value: String, label: String? = null, onChange: (String) -> Unit) {
    OutlinedTextField(value = value, onValueChange = { if (it.length <= 32) onChange(it) }, label = { Text(label ?: "Phone number") },
        placeholder = { Text("+91 98765 43210") }, supportingText = { Text("Include the country code.") }, singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Phone), modifier = Modifier.fillMaxWidth())
}

@Composable
private fun SubmitButton(label: String, enabled: Boolean, onClick: () -> Unit) {
    Button(colors = magicanButtonColors(), shape = RoundedCornerShape(8.dp), contentPadding = PaddingValues(horizontal = 12.dp, vertical = 6.dp),
        modifier = Modifier.fillMaxWidth().heightIn(min = 40.dp), enabled = enabled, onClick = onClick) { Text(label) }
}
