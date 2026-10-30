package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.chat.ChatGodModeState
import ai.magicbeans.magdroid.chat.ChatRepository
import ai.magicbeans.magdroid.chat.SuperGodModeDuration
import ai.magicbeans.magdroid.chat.SuperGodModeState
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Shield
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.LifecycleResumeEffect
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.flow.collect

/** Uses the same controls in Chat and VibeDev; super mode is backend-wide. */
@Composable
internal fun ComposerExecutionModes(sessionId: String?, showChatMode: Boolean, onStatus: (String?) -> Unit = {}) {
    val currentOnStatus by rememberUpdatedState(onStatus)
    var revision by remember { mutableStateOf(MagicianAccess.connectionRevision) }
    LaunchedEffect(Unit) { MagicianAccess.activeMachine.collect { revision = MagicianAccess.connectionRevision } }
    key(revision, sessionId) { ExecutionModeMenu(sessionId, showChatMode, revision) { currentOnStatus(it) } }
}

@Composable
private fun ExecutionModeMenu(sessionId: String?, showChatMode: Boolean, revision: Long, onStatus: (String?) -> Unit) {
    val context = LocalContext.current.applicationContext
    // Freeze the destination shown in this menu, before a save coroutine starts.
    val connection = remember { MagicianAccess.connectionSnapshot(context) }
    val repository = remember { ChatRepository(context, modeConnection = { connection }) }
    DisposableEffect(repository) { onDispose { repository.close() } }
    val scope = rememberCoroutineScope()
    var chat by remember { mutableStateOf<ChatGodModeState?>(null) }
    var superMode by remember { mutableStateOf<SuperGodModeState?>(null) }
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    var open by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var actionError by remember { mutableStateOf<String?>(null) }
    // Confirmation stores an exact operation; opening this menu grants nothing.
    var confirm by remember { mutableStateOf<String?>(null) }
    var requestRevision by remember { mutableIntStateOf(0) }

    suspend fun refresh() {
        if (busy || MagicianAccess.connectionRevision != revision || connection != MagicianAccess.connectionSnapshot(context)) return
        val request = ++requestRevision
        now = System.currentTimeMillis()
        try {
            val newChat = if (showChatMode && !sessionId.isNullOrBlank()) repository.chatGodMode(sessionId) else null
            val newSuper = repository.superGodMode()
            if (request == requestRevision && MagicianAccess.connectionRevision == revision && connection == MagicianAccess.connectionSnapshot(context)) {
                chat = newChat; superMode = newSuper; now = System.currentTimeMillis(); error = null
            }
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (failure: Exception) { if (request == requestRevision) error = failure.message ?: "Execution modes are unavailable." }
    }
    LifecycleResumeEffect(repository, sessionId) {
        val job = scope.launch { while (isActive) { if (!busy && confirm == null) refresh(); delay(15_000) } }
        onPauseOrDispose { job.cancel() }
    }
    fun save(action: String) {
        if (busy || MagicianAccess.connectionRevision != revision || connection == null || connection != MagicianAccess.connectionSnapshot(context)) return
        busy = true; requestRevision++
        scope.launch {
            try {
                if (MagicianAccess.connectionRevision != revision || connection != MagicianAccess.connectionSnapshot(context)) return@launch
                when (action) {
                    "chat_on", "chat_off", "chat_default" -> if (!sessionId.isNullOrBlank()) {
                        chat = repository.chatGodMode(sessionId, when(action) { "chat_on" -> true; "chat_off" -> false; else -> null }, update = true)
                    }
                    "auto_on", "auto_off" -> if (!sessionId.isNullOrBlank()) {
                        chat = repository.chatAutoApprove(sessionId, action == "auto_on")
                    }
                    else -> superMode = repository.superGodMode(when(action) {
                        "super_hour" -> SuperGodModeDuration.Hour
                        "super_forever" -> SuperGodModeDuration.UntilOff
                        else -> null
                    }, update = true)
                }
                error = null
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: Exception) { actionError = failure.message ?: "Execution mode could not be saved." }
            finally { busy = false }
        }
    }
    val superActive = superMode?.activeAt(now) == true
    val status = if (error != null) null else when {
        superActive -> "Super God mode"
        chat?.enabled == true -> if (chat?.autoApprove == true) "God mode · Auto-approve" else "God mode"
        else -> null
    }
    LaunchedEffect(status) { onStatus(status) }
    DisposableEffect(Unit) { onDispose { onStatus(null) } }
    val modeLabel = when {
        superActive -> "Super God · all chats · ${superMode!!.endLabel(now)}"
        chat?.enabled == true && chat?.autoApprove == true -> "God mode · auto-approve · this chat"
        chat?.enabled == true -> "God mode · this chat"
        showChatMode -> "God modes"
        else -> "Super God"
    }
    Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
    Box {
        IconButton(onClick = { open = true; scope.launch { if (!busy) refresh() } }, enabled = !busy,
            modifier = Modifier.size(COMPOSER_TOOL_BUTTON_DP.dp)) {
            Icon(Icons.Outlined.Shield, contentDescription = modeLabel,
                tint = if (superActive || chat?.enabled == true) activePalette.danger else Muted,
                modifier = Modifier.size(COMPOSER_TOOL_ICON_DP.dp))
        }
        DropdownMenu(open, onDismissRequest = { open = false }, containerColor = Panel) {
            if (showChatMode) {
                DropdownMenuItem(text = { Text(if (chat?.enabled == true) "Turn off God mode for this chat" else "Turn on God mode for this chat", color = Ink) },
                    enabled = chat != null && !sessionId.isNullOrBlank() && !busy, onClick = {
                        open = false; if (chat?.enabled == true) save("chat_off") else confirm = "chat_on"
                    })
                DropdownMenuItem(text = { Text(if (chat?.autoApprove == true) "Turn off auto-approve for this chat" else "Auto-approve this chat", color = Ink) },
                    enabled = chat?.enabled == true && !sessionId.isNullOrBlank() && !busy, onClick = {
                        open = false; if (chat?.autoApprove == true) save("auto_off") else confirm = "auto_on"
                    })
                DropdownMenuItem(text = { Text("Use Settings default for this chat", color = Ink) },
                    enabled = chat != null && !busy, onClick = {
                        open = false
                        if (chat?.enabled == false && chat?.defaultEnabled == true) confirm = "chat_default" else save("chat_default")
                    })
                HorizontalDivider()
            }
            if (superActive) {
                DropdownMenuItem(text = { Text("Turn off Super God mode", color = activePalette.danger) }, onClick = { open = false; save("super_off") })
            } else {
                DropdownMenuItem(text = { Text("Super God mode for 1 hour", color = Ink) }, enabled = superMode != null && !busy,
                    onClick = { open = false; confirm = "super_hour" })
                DropdownMenuItem(text = { Text("Super God mode until turned off", color = Ink) }, enabled = superMode != null && !busy,
                    onClick = { open = false; confirm = "super_forever" })
            }
            DropdownMenuItem(text = { Text("Refresh modes", color = Ink) }, enabled = !busy,
                onClick = { scope.launch { refresh() } })
            error?.let { Text(it, color = activePalette.danger, fontSize = 12.sp) }
        }
    }
    if (showChatMode && chat?.enabled == true) {
        TextButton(onClick = { if (chat?.autoApprove == true) save("auto_off") else confirm = "auto_on" },
            enabled = !busy && !sessionId.isNullOrBlank(), modifier = Modifier.height(30.dp),
            shape = RoundedCornerShape(6.dp), contentPadding = PaddingValues(horizontal = 7.dp, vertical = 3.dp),
            colors = ButtonDefaults.textButtonColors(containerColor = if (chat?.autoApprove == true) AccentFill else Control,
                contentColor = if (chat?.autoApprove == true) OnAccent else Ink)) {
            Text(if (chat?.autoApprove == true) "✓ Auto-approve" else "Auto-approve", fontSize = 10.sp)
        }
    }
    }
    confirm?.let { action ->
        AlertDialog(onDismissRequest = { confirm = null }, containerColor = Panel,
            title = { Text(when {
                action == "auto_on" -> "Auto-approve God mode in this chat?"
                action.startsWith("chat") -> "Enable God mode for this chat?"
                else -> "Enable Super God mode?"
            }, color = Ink) },
            text = { Text(if (action == "auto_on")
                "What God mode would ask you in this chat and the runs it starts is approved without asking. It still asks before anything that deletes (rm -r or -f, find -delete, git reset --hard, a forced push), anything run as root (sudo, doas), code a command does not show, and the blocked list."
            else if (action.startsWith("chat"))
                "This chat and runs it starts may request shell, file and network access beyond the normal sandbox. You still approve those operations once, for this session or always."
            else "Applies to all chats and runs on this backend. After launch approval, harnesses run without sandbox restrictions. ${if(action == "super_hour") "Turns off after one hour." else "Stays on until you turn it off."}", color = Ink) },
            confirmButton = { TextButton(onClick = { confirm = null; save(action) }) { Text("Enable", color = activePalette.danger) } },
            dismissButton = { TextButton(onClick = { confirm = null }) { Text("Cancel", color = Ink) } })
    }
    actionError?.let { message ->
        AlertDialog(onDismissRequest = { actionError = null }, containerColor = Panel,
            title = { Text("Execution mode could not be saved", color = Ink) },
            text = { Text(message, color = Ink) },
            confirmButton = { TextButton(onClick = { actionError = null }) { Text("OK", color = Ink) } })
    }
}
