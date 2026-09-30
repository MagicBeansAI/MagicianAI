package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.notes.NoteDocument
import ai.magicbeans.magdroid.notes.NoteTreeEntry
import ai.magicbeans.magdroid.notes.NotesLibraryException
import ai.magicbeans.magdroid.notes.NotesLibraryRepository
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.ui.text.TextStyle
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.zIndex
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.launch

class NotesChrome {
    var explorerOpen by mutableStateOf(true)
    var query by mutableStateOf("")
}

private data class NoteNode(
    val entry: NoteTreeEntry,
    val children: List<NoteNode>? = null,
    val open: Boolean = false,
    val empty: Boolean = entry.kind == "dir" && entry.hasChildren == false,
)

@Composable
fun NotesScreen(
    initialPath: String = "",
    chrome: NotesChrome = remember { NotesChrome() },
) {
    val context = androidx.compose.ui.platform.LocalContext.current
    val repository = remember(context) { NotesLibraryRepository(context) }
    DisposableEffect(repository) { onDispose { repository.close() } }
    val scope = rememberCoroutineScope()
    val focusManager = LocalFocusManager.current
    val keyboard = LocalSoftwareKeyboardController.current
    val dismissKeyboard = {
        focusManager.clearFocus(true)
        keyboard?.hide()
    }
    var roots by remember { mutableStateOf<List<NoteNode>>(emptyList()) }
    var selectedFolder by remember { mutableStateOf("") }
    var document by remember { mutableStateOf<NoteDocument?>(null) }
    var editing by remember { mutableStateOf(false) }
    var draft by remember { mutableStateOf("") }
    var name by remember { mutableStateOf("") }
    var naming by remember { mutableStateOf<String?>(null) }
    var find by remember { mutableStateOf("") }
    var confirmNote by remember { mutableStateOf(false) }
    var confirmFolder by remember { mutableStateOf(false) }
    var status by remember { mutableStateOf("") }
    var results by remember { mutableStateOf<ai.magicbeans.magdroid.notes.NoteSearchResults?>(null) }
    var searching by remember { mutableStateOf(false) }
    var tookMs by remember { mutableStateOf(0L) }

    suspend fun refresh(path: String) {
        val loaded = repository.tree(path).map { NoteNode(it) }
        roots = if (path.isEmpty()) {
            loaded
        } else {
            updateNode(roots, path) { it.copy(children = loaded, open = loaded.isNotEmpty()) }
        }
    }

    LaunchedEffect(chrome.explorerOpen) {
        if (chrome.explorerOpen) dismissKeyboard()
    }

    LaunchedEffect(chrome.query) {
        val query = chrome.query.trim()
        if (query.isNotEmpty()) chrome.explorerOpen = false
        if (query.isEmpty()) {
            results = null
            searching = false
            return@LaunchedEffect
        }
        searching = true
        kotlinx.coroutines.delay(200)
        val started = System.currentTimeMillis()
        runCatching { repository.search(query) }
            .onSuccess {
                results = it
                tookMs = System.currentTimeMillis() - started
                status = ""
            }
            .onFailure { status = it.message ?: "Could not search notes." }
        searching = false
    }

    LaunchedEffect(initialPath) {
        runCatching {
            roots = repository.tree("").map { NoteNode(it) }
            if (initialPath.isNotBlank()) document = repository.open(initialPath)
        }.onFailure { status = (it as? NotesLibraryException)?.message ?: "Notes could not be loaded." }
    }

    Box(Modifier.fillMaxSize().background(Ground)) {
        ThinScroll(Modifier.fillMaxSize()) {
        Column(
            Modifier
                .padding(20.dp),
            horizontalAlignment = Alignment.Start,
        ) {
            if (status.isNotEmpty()) Text(status, color = Muted, modifier = Modifier.padding(bottom = 8.dp))
            val found = results
            if (chrome.query.isNotBlank() && found != null) {
                Text(
                    "${found.hits.size} results · ${tookMs} ms",
                    color = Muted,
                    modifier = Modifier.padding(bottom = 8.dp),
                )
                if (found.hits.isEmpty()) {
                    Text("Nothing matched “${chrome.query.trim()}”.", color = Ink)
                }
                val terms = found.queryTerms.ifEmpty { chrome.query.trim().split(Regex("\\s+")) }
                found.hits.forEachIndexed { index, hit ->
                    val fileName = noteFileName(hit.relativePath)
                    val folder = parentFolder(hit.relativePath)
                    val title = hit.title.trim()
                    val showTitle = title.isNotEmpty() &&
                        !title.equals(fileName, ignoreCase = true) &&
                        !title.equals(hit.relativePath, ignoreCase = true)
                    if (index > 0) {
                        Box(
                            Modifier
                                .fillMaxWidth()
                                .padding(vertical = 2.dp)
                                .height(1.dp)
                                .background(BorderSoft),
                        )
                    }
                    Column(Modifier.fillMaxWidth().padding(vertical = 10.dp)) {
                        if (showTitle) {
                            Text(
                                highlightedTerms(title, terms),
                                color = Ink,
                                fontWeight = FontWeight.SemiBold,
                                fontSize = 15.sp,
                            )
                        }
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            if (folder.isNotEmpty()) {
                                Text("$folder/", color = Muted, fontSize = 13.sp)
                            }
                            Text(
                                fileName,
                                color = Coral,
                                fontSize = 15.sp,
                                fontWeight = FontWeight.SemiBold,
                                textDecoration = TextDecoration.Underline,
                                modifier = Modifier.clickable {
                                    scope.launch {
                                        runCatching {
                                            document = repository.open(hit.relativePath)
                                            draft = document?.markdown.orEmpty()
                                            editing = false
                                            chrome.query = ""
                                            results = null
                                            chrome.explorerOpen = false
                                        }.onFailure { status = it.message ?: "Could not open that note." }
                                    }
                                },
                            )
                        }
                        hit.matches.take(3).forEach { match ->
                            Row(
                                Modifier.fillMaxWidth().padding(top = 4.dp),
                                verticalAlignment = Alignment.Top,
                            ) {
                                Text(
                                    match.line.toString(),
                                    color = Muted,
                                    fontSize = 12.sp,
                                    textAlign = TextAlign.End,
                                    modifier = Modifier.width(32.dp),
                                )
                                Text(
                                    highlightedTerms(match.text, terms),
                                    color = Ink,
                                    fontSize = 13.sp,
                                    modifier = Modifier.weight(1f).padding(start = 10.dp),
                                )
                            }
                        }
                    }
                }
            } else if (searching) {
                Text("Searching notes…", color = Muted)
            } else {
            val open = document
            if (open == null) {
                Text("Your notes", color = Ink, fontSize = 28.sp, fontWeight = FontWeight.SemiBold)
                Text(
                    "The folders are open beside this page. Pick a note, or add one from that list.",
                    color = Muted,
                    modifier = Modifier.padding(top = 10.dp),
                )
            } else {
                Row(
                    Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(open.relativePath, color = Muted, modifier = Modifier.weight(1f))
                    NotesAction(if (editing) "View" else "Edit") {
                        if (editing) editing = false else {
                            draft = open.markdown
                            editing = true
                        }
                    }
                    if (editing) {
                        NotesAction("Save") {
                            scope.launch {
                                runCatching {
                                    document = repository.save(open.relativePath, draft)
                                    draft = document?.markdown.orEmpty()
                                    editing = false
                                }.onFailure { status = it.message ?: "Could not save that note." }
                            }
                        }
                    }
                    NotesAction("Delete") { confirmNote = true }
                }
                if (!editing) {
                    NotesField(
                        value = find,
                        onValueChange = { find = it },
                        placeholder = "Find in this note",
                        modifier = Modifier.padding(top = 10.dp),
                    )
                    if (find.isNotBlank()) {
                        Text(
                            "${noteMatchCount(open.markdown, find)} ${if (noteMatchCount(open.markdown, find) == 1) "match" else "matches"}",
                            color = Muted,
                            fontSize = 12.sp,
                            modifier = Modifier.padding(top = 4.dp),
                        )
                    }
                }
                if (editing) {
                    NotesField(
                        value = draft,
                        onValueChange = { draft = it },
                        placeholder = "Markdown",
                        singleLine = false,
                        modifier = Modifier.padding(top = 12.dp).height(320.dp),
                    )
                } else {
                    MarkdownText(
                        markdown = open.markdown,
                        color = Ink,
                        fontSize = 15.sp,
                        lineHeight = 22.sp,
                        modifier = Modifier.padding(top = 12.dp),
                        find = find,
                    )
                }
            }
            }
        }
        }
        if (chrome.explorerOpen) {
            Box(
                Modifier
                    .fillMaxSize()
                    .zIndex(1f)
                    .background(Color.Black.copy(alpha = 0.32f))
                    .clickable(
                        interactionSource = remember { MutableInteractionSource() },
                        indication = null,
                    ) {
                        dismissKeyboard()
                        chrome.explorerOpen = false
                    },
            )
            Box(
                Modifier
                    .width(280.dp)
                    .fillMaxHeight()
                    .zIndex(2f)
                    .background(Panel)
                    .clickable(
                        interactionSource = remember { MutableInteractionSource() },
                        indication = null,
                    ) { dismissKeyboard() },
            ) {
            ThinScroll(Modifier.fillMaxSize()) {
            Column(
                Modifier.padding(12.dp),
                horizontalAlignment = Alignment.Start,
            ) {
                Text(
                    if (selectedFolder.isEmpty()) "In the notes root" else "In $selectedFolder",
                    color = Muted,
                    textAlign = TextAlign.Start,
                    modifier = Modifier.fillMaxWidth().padding(bottom = 8.dp),
                )
                Row(
                    Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    NotesAction("New note") { naming = "note"; name = "" }
                    NotesAction("New folder") { naming = "folder"; name = "" }
                    if (selectedFolder.isNotEmpty()) {
                        NotesAction("Delete") { confirmFolder = true }
                    }
                }
                if (naming != null) {
                    NotesField(
                        value = name,
                        onValueChange = { name = it },
                        placeholder = if (naming == "folder") "Folder name" else "Note name",
                        modifier = Modifier.padding(top = 8.dp),
                    )
                    NotesAction(if (naming == "folder") "Create folder" else "Create") {
                        if (name.isBlank()) {
                            status = "Enter a name."
                            return@NotesAction
                        }
                        val kind = naming
                        scope.launch {
                            runCatching {
                                if (kind == "folder") {
                                    repository.createFolder(selectedFolder, name)
                                    name = ""
                                    naming = null
                                    refresh(selectedFolder)
                                } else {
                                    document = repository.create(selectedFolder, name)
                                    name = ""
                                    naming = null
                                    find = ""
                                    refresh(selectedFolder)
                                    chrome.explorerOpen = false
                                }
                            }.onFailure { status = it.message ?: "Could not create that." }
                        }
                    }
                }
                if (status.isNotEmpty()) {
                    Text(status, color = Muted, modifier = Modifier.padding(top = 8.dp))
                }
                NoteBranch(roots, 0, selectedFolder) { node ->
                    dismissKeyboard()
                    scope.launch {
                        runCatching {
                            if (node.entry.kind == "dir") {
                                selectedFolder = node.entry.relativePath
                                if (node.empty) return@launch
                                val kids = node.children ?: repository.tree(node.entry.relativePath).map { NoteNode(it) }
                                roots = updateNode(roots, node.entry.relativePath) {
                                    it.copy(
                                        children = kids,
                                        open = !node.open && kids.isNotEmpty(),
                                        empty = kids.isEmpty(),
                                    )
                                }
                            } else {
                                document = repository.open(node.entry.relativePath)
                                draft = document?.markdown.orEmpty()
                                editing = false
                                find = ""
                                chrome.explorerOpen = false
                            }
                        }.onFailure { status = it.message ?: "Could not open that." }
                    }
                }
            }
            }
            Box(
                Modifier
                    .align(Alignment.CenterEnd)
                    .fillMaxHeight()
                    .width(1.dp)
                    .background(BorderSoft),
            )
            }
        }
        if (confirmNote) {
            val path = document?.relativePath.orEmpty()
            AlertDialog(
                onDismissRequest = { confirmNote = false },
                title = { Text("Delete this note?") },
                text = { Text(path) },
                confirmButton = {
                    TextButton(onClick = {
                        confirmNote = false
                        scope.launch {
                            runCatching {
                                repository.deleteFile(path)
                                document = null
                                refresh(parentFolder(path))
                            }.onFailure { status = it.message ?: "Could not delete that note." }
                        }
                    }) { Text("Delete") }
                },
                dismissButton = { TextButton(onClick = { confirmNote = false }) { Text("Cancel") } },
            )
        }
        if (confirmFolder) {
            val path = selectedFolder
            AlertDialog(
                onDismissRequest = { confirmFolder = false },
                title = { Text("Delete this folder?") },
                text = { Text("This removes $path and the notes inside it.") },
                confirmButton = {
                    TextButton(onClick = {
                        confirmFolder = false
                        scope.launch {
                            runCatching {
                                repository.deleteFolder(path)
                                val parent = parentFolder(path)
                                selectedFolder = parent
                                naming = null
                                refresh(parent)
                            }.onFailure { status = it.message ?: "Could not delete that folder." }
                        }
                    }) { Text("Delete") }
                },
                dismissButton = { TextButton(onClick = { confirmFolder = false }) { Text("Cancel") } },
            )
        }
    }
}

@Composable
private fun ThinScroll(
    modifier: Modifier = Modifier,
    content: @Composable androidx.compose.foundation.layout.ColumnScope.() -> Unit,
) {
    val state = rememberScrollState()
    Box(modifier) {
        Column(
            Modifier
                .fillMaxSize()
                .verticalScroll(state),
            horizontalAlignment = Alignment.Start,
            content = content,
        )
        val max = state.maxValue
        val viewport = state.viewportSize
        if (max > 0 && viewport > 0) {
            val total = viewport + max
            val thumb = (viewport.toFloat() / total * viewport).coerceAtLeast(28f)
            val travel = (viewport - thumb).coerceAtLeast(0f)
            val y = state.value.toFloat() / max * travel
            val density = LocalDensity.current
            Box(
                Modifier
                    .align(Alignment.TopEnd)
                    .padding(top = with(density) { y.toDp() }, end = 2.dp)
                    .width(3.dp)
                    .height(with(density) { thumb.toDp() })
                    .background(Muted.copy(alpha = 0.45f), RoundedCornerShape(2.dp)),
            )
        }
    }
}

@Composable
internal fun NotesField(
    value: String,
    onValueChange: (String) -> Unit,
    placeholder: String,
    modifier: Modifier = Modifier,
    singleLine: Boolean = true,
    clearable: Boolean = false,
) {
    BasicTextField(
        value = value,
        onValueChange = onValueChange,
        singleLine = singleLine,
        textStyle = TextStyle(color = Ink, fontSize = 14.sp),
        modifier = modifier
            .fillMaxWidth()
            .height(if (singleLine) 32.dp else 320.dp)
            .background(Control, RoundedCornerShape(8.dp))
            .border(1.dp, ControlBorder, RoundedCornerShape(8.dp))
            .padding(horizontal = 10.dp, vertical = if (singleLine) 0.dp else 8.dp),
        decorationBox = { inner ->
            Row(Modifier.fillMaxSize(), verticalAlignment = Alignment.CenterVertically) {
                Box(Modifier.weight(1f).fillMaxHeight(), contentAlignment = if (singleLine) Alignment.CenterStart else Alignment.TopStart) {
                    if (value.isEmpty()) Text(placeholder, color = Muted, fontSize = 13.sp)
                    inner()
                }
                if (clearable && value.isNotEmpty()) {
                    Text(
                        "×",
                        color = Muted,
                        fontSize = 18.sp,
                        modifier = Modifier
                            .semantics { contentDescription = "Clear search" }
                            .clickable { onValueChange("") }
                            .padding(horizontal = 4.dp),
                    )
                }
            }
        },
    )
}

@Composable
private fun NotesAction(label: String, enabled: Boolean = true, onClick: () -> Unit) {
    val focusManager = LocalFocusManager.current
    val keyboard = LocalSoftwareKeyboardController.current
    OutlinedButton(
        onClick = {
            focusManager.clearFocus(true)
            keyboard?.hide()
            onClick()
        },
        enabled = enabled,
        shape = MagicanButtonShape,
        contentPadding = PaddingValues(horizontal = 8.dp, vertical = 0.dp),
        border = BorderStroke(1.dp, BorderSoft),
        colors = ButtonDefaults.outlinedButtonColors(
            containerColor = Color.Transparent,
            contentColor = Ink,
            disabledContentColor = Muted,
        ),
        modifier = Modifier.height(32.dp),
    ) {
        Text(label, fontSize = 12.sp, maxLines = 1, color = if (enabled) Ink else Muted)
    }
}

@Composable
private fun NoteBranch(nodes: List<NoteNode>, depth: Int, selectedPath: String, onClick: (NoteNode) -> Unit) {
    Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.Start) {
        nodes.forEachIndexed { index, node ->
            val last = index == nodes.lastIndex
            Row(
                Modifier.fillMaxWidth().height(IntrinsicSize.Min),
                verticalAlignment = Alignment.Top,
            ) {
                if (depth > 0) {
                    Box(Modifier.width(14.dp).fillMaxHeight()) {
                        Box(
                            Modifier
                                .width(1.dp)
                                .then(if (last) Modifier.height(16.dp) else Modifier.fillMaxHeight())
                                .background(BorderSoft),
                        )
                        Box(
                            Modifier
                                .padding(top = 16.dp)
                                .fillMaxWidth()
                                .height(1.dp)
                                .background(BorderSoft),
                        )
                    }
                }
                Column(Modifier.weight(1f), horizontalAlignment = Alignment.Start) {
                    val selected = selectedPath.isNotEmpty() && node.entry.relativePath == selectedPath
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .background(
                                if (selected) Coral.copy(alpha = 0.16f) else Color.Transparent,
                                RoundedCornerShape(8.dp),
                            )
                            .clickable { onClick(node) }
                            .padding(vertical = 8.dp, horizontal = 4.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(
                            text = when {
                                node.entry.kind != "dir" -> ""
                                node.empty -> "–"
                                node.open -> "▾"
                                else -> "▸"
                            },
                            color = if (node.empty) Muted else Coral,
                            fontSize = 12.sp,
                            modifier = Modifier.width(14.dp),
                        )
                        Text(
                            text = node.entry.name,
                            color = if (node.empty) Muted else Ink,
                            textAlign = TextAlign.Start,
                            modifier = Modifier.weight(1f),
                        )
                    }
                    if (node.open && !node.children.isNullOrEmpty()) {
                        NoteBranch(node.children.orEmpty(), depth + 1, selectedPath, onClick)
                    }
                }
            }
        }
    }
}

private fun highlightedTerms(text: String, terms: List<String>): AnnotatedString {
    val usable = terms.map { it.trim() }.filter { it.isNotEmpty() }
    if (usable.isEmpty() || text.isEmpty()) return AnnotatedString(text)
    val covered = BooleanArray(text.length)
    val lowered = text.lowercase()
    for (term in usable) {
        val needle = term.lowercase()
        var from = lowered.indexOf(needle)
        while (from >= 0) {
            val end = (from + needle.length).coerceAtMost(text.length)
            for (index in from until end) covered[index] = true
            from = lowered.indexOf(needle, from + needle.length)
        }
    }
    val builder = AnnotatedString.Builder()
    var start = 0
    for (index in 1..text.length) {
        if (index == text.length || covered[index] != covered[start]) {
            val piece = text.substring(start, index)
            if (covered[start]) {
                builder.pushStyle(SpanStyle(background = Coral.copy(alpha = 0.35f)))
                builder.append(piece)
                builder.pop()
            } else {
                builder.append(piece)
            }
            start = index
        }
    }
    return builder.toAnnotatedString()
}

private fun noteFileName(path: String): String {
    val trimmed = path.trim('/')
    val slash = trimmed.lastIndexOf('/')
    return if (slash < 0) trimmed else trimmed.substring(slash + 1)
}

private fun parentFolder(path: String): String {
    val trimmed = path.trim('/')
    val slash = trimmed.lastIndexOf('/')
    return if (slash < 0) "" else trimmed.substring(0, slash)
}

private fun updateNode(nodes: List<NoteNode>, path: String, transform: (NoteNode) -> NoteNode): List<NoteNode> =
    nodes.map { node ->
        if (node.entry.relativePath == path) {
            transform(node)
        } else {
            node.copy(children = node.children?.let { updateNode(it, path, transform) })
        }
    }
