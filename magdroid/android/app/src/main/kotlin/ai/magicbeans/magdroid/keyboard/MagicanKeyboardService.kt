package ai.magicbeans.magdroid.keyboard

import ai.magicbeans.magdroid.chat.ChatRepository
import ai.magicbeans.magdroid.chat.ChatStreamEvent
import ai.magicbeans.magdroid.tasks.TaskCreateDraft
import ai.magicbeans.magdroid.tasks.TaskRepository
import ai.magicbeans.magdroid.ui.Palette
import ai.magicbeans.magdroid.ui.ThemeStore
import ai.magicbeans.magdroid.ui.Themes
import android.content.res.Configuration
import android.graphics.Color
import android.graphics.Typeface
import android.inputmethodservice.InputMethodService
import android.os.Build
import android.view.Gravity
import android.view.View
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.ExtractedTextRequest
import android.view.inputmethod.InputMethodManager
import android.text.InputType
import android.view.textservice.SentenceSuggestionsInfo
import android.view.textservice.SpellCheckerSession
import android.view.textservice.SuggestionsInfo
import android.view.textservice.TextInfo
import android.view.WindowInsetsController
import android.view.textservice.TextServicesManager
import android.widget.Button
import androidx.compose.ui.graphics.toArgb
import android.widget.HorizontalScrollView
import android.widget.LinearLayout
import android.widget.TextView
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * The Android Magican keyboard.
 *
 * Ordinary keys are entirely local. Agent skills read the current selection or
 * field only after an explicit tap, never in secure/numeric fields. Write and
 * Ask return a preview with Replace/Insert/Cancel; Act has a separate confirm
 * step before a task is created and executed.
 */
class MagicanKeyboardService : InputMethodService(), SpellCheckerSession.SpellCheckerSessionListener {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val chat by lazy { ChatRepository(applicationContext) }
    private val tasks by lazy { TaskRepository(applicationContext) }

    private lateinit var root: LinearLayout
    private lateinit var candidateArea: LinearLayout
    private lateinit var actionArea: LinearLayout
    private lateinit var keyboardArea: LinearLayout
    private var shifted = false
    private var symbols = false
    private var actionJob: Job? = null
    private var spellChecker: SpellCheckerSession? = null
    private var spellRequestSequence = 0
    private var requestedPrefix = ""
    private var learnedCandidates: List<String> = emptyList()
    /** The pending debounced spell request, cancelled by the next keystroke. */
    private var spellJob: Job? = null

    /** The pending debounced next-word prediction, same lifecycle. */
    private var predictJob: Job? = null
    /** Whether the action row currently holds the plain skill strip. */
    private var showingDefaultStrip = false
    /** Letter keys, kept so shift can relabel rather than rebuild. */
    private val letterKeys = mutableListOf<Pair<String, Button>>()
    /** The three candidate cells, reused across refreshes. */
    private val candidateCells = mutableListOf<Button>()
    /** The candidate cells' container, hidden when there is nothing to suggest. */
    private var cellsRow: LinearLayout? = null
    /** Shown in the cells' place when there are no candidates. */
    private var hintView: TextView? = null
    /** The ✦ key on the strip's trailing edge. */
    private var brandKey: Button? = null
    /**
     * Whether the Magican action row is showing.
     *
     * False by default and reset for every field: the keyboard types like a
     * keyboard, and the agentic surface is summoned rather than endured.
     */
    private var aiRevealed = false

    /**
     * The last autocorrection, so it can be taken back: what was typed, what
     * was committed in its place, and the separator that triggered it. An
     * autocorrect without a revert is a keyboard that argues; the revert cell
     * also *trusts* the typed word, so the same word is never fought twice.
     * Cleared by the next keystroke.
     */
    private var revertState: Triple<String, String, String>? = null

    /** The correction engine's candidates for the current prefix. */
    private var engineCandidates: List<String> = emptyList()

    /** After-space next-word predictions currently on the row. */
    private var predictedWords: List<String> = emptyList()

    /**
     * The palette these views were built with.
     *
     * The keyboard had its own hardcoded cream and white, so it stayed light
     * whichever of the eleven themes the app was wearing, and stayed light in
     * the dark. It is a surface of this app and should look like it.
     */
    private var builtTheme: String? = null

    /**
     * Carry the keyboard's colour onto the navigation bar behind it.
     *
     * The IME window does not reach the navigation bar, so the strip holding
     * the hide-keyboard caret and the keyboard switcher stayed the system's
     * black under a light keyboard — a black shelf bolted to the bottom of the
     * app. The icons there are drawn by the system, so their contrast has to be
     * asked for separately or they vanish into a light bar.
     */
    private fun paintSystemBars() {
        val host = window?.window ?: return
        val theme = palette()
        host.navigationBarColor = theme.background.toArgb()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            host.insetsController?.setSystemBarsAppearance(
                if (theme.isDark) 0 else WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS,
                WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS,
            )
        } else if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            @Suppress("DEPRECATION")
            host.decorView.systemUiVisibility = if (theme.isDark) {
                host.decorView.systemUiVisibility and View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR.inv()
            } else {
                host.decorView.systemUiVisibility or View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR
            }
        }
    }

    /** The app's chosen theme, resolved for the device's current appearance. */
    private fun currentThemeId(): String {
        val store = ThemeStore.get(this)
        val systemIsDark = resources.configuration.uiMode and
            Configuration.UI_MODE_NIGHT_MASK == Configuration.UI_MODE_NIGHT_YES
        return Themes.resolve(store.familyId.value, store.mode.value, systemIsDark)
    }

    private fun palette(): Palette = Themes.palette(builtTheme ?: currentThemeId())

    override fun onCreate() {
        super.onCreate()
        // The dictionary parse + symmetric-delete index build, off-thread,
        // started the moment the IME process exists so the first word typed
        // already has the corrector behind it. Until it publishes, the row
        // falls back to the system spell checker and learned words.
        KeyboardIntelligence.warmUp(this)
    }

    override fun onCreateInputView(): View {
        builtTheme = currentThemeId()
        root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(palette().background.toArgb())
            setPadding(dp(4), dp(4), dp(4), dp(6))
        }
        candidateCells.clear()
        letterKeys.clear()
        cellsRow = null
        hintView = null
        brandKey = null
        showingDefaultStrip = false
        aiRevealed = false
        candidateArea = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        actionArea = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        keyboardArea = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        // One permanent row, as on iOS: the strip sits against the keys and
        // carries corrections plus the ✦ key on its trailing edge. The action
        // row is above it and hidden until summoned, so at rest this is an
        // ordinary keyboard with one small mark on it.
        root.addView(actionArea)
        root.addView(candidateArea)
        root.addView(keyboardArea)
        actionArea.visibility = View.GONE
        renderKeys()
        return root
    }

    override fun onStartInputView(info: EditorInfo?, restarting: Boolean) {
        super.onStartInputView(info, restarting)
        actionJob?.cancel()
        symbols = info?.inputType?.and(InputType.TYPE_MASK_CLASS) == InputType.TYPE_CLASS_NUMBER
        // A new field is a new conversation: the previous field's revert,
        // predictions and engine candidates all describe text that is no
        // longer under the cursor.
        revertState = null
        predictedWords = emptyList()
        engineCandidates = emptyList()
        // A theme changed in Settings, or the device crossed into night, while
        // this view was alive. Rebuilding is only correct here — the colours are
        // set on the views themselves, not read from a style at draw time.
        if (builtTheme != null && builtTheme != currentThemeId()) {
            setInputView(onCreateInputView())
        }
        paintSystemBars()
        restartSpellChecker()
        // Every field starts collapsed, as it does on iOS. Carrying the open
        // surface into the next app is how it became permanent in the first
        // place.
        if (::actionArea.isInitialized) {
            aiRevealed = false
            actionArea.removeAllViews()
            actionArea.visibility = View.GONE
            showingDefaultStrip = false
            styleBrandKey()
        }
        if (::candidateArea.isInitialized) refreshCandidateStrip(force = true)
        if (::keyboardArea.isInitialized) renderKeys()
    }

    override fun onFinishInput() {
        actionJob?.cancel()
        closeSpellChecker()
        clearCandidateStrip()
        super.onFinishInput()
    }

    override fun onDestroy() {
        actionJob?.cancel()
        closeSpellChecker()
        scope.cancel()
        super.onDestroy()
    }

    /**
     * Fill the action row with the skills.
     *
     * [showingDefaultStrip] guards the rebuild: reaching here means reading the
     * skill list and allocating a scroll view plus a button per skill, and a
     * row that is already showing exactly this has nothing to put back.
     */
    private fun showSkillStrip(message: String? = null) {
        val isDefault = message == null && !secureField()
        if (isDefault && showingDefaultStrip && actionArea.visibility == View.VISIBLE) return
        revealActionArea()
        showingDefaultStrip = isDefault
        if (secureField()) {
            actionArea.addView(message("Secure field — Magican actions are off"))
            return
        }
        message?.let { actionArea.addView(message(it)) }
        val scroll = HorizontalScrollView(this).apply { isHorizontalScrollBarEnabled = false }
        val row = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(2), 0, dp(2), dp(4))
        }
        // No ✦ chip here any more. This row is only on screen because the ✦ key
        // on the strip put it there, so repeating the mark inside it offered a
        // second way in to somewhere the reader already was — and tapping it
        // replaced the skills with "Choose an action", which was the row it had
        // just replaced.
        MagicanKeyboardStore.loadSkills(this).forEach { skill ->
            row.addView(chip(skill.label) { beginSkill(skill) })
        }
        scroll.addView(row)
        actionArea.addView(scroll)
    }

    private fun beginSkill(skill: KeyboardSkill) {
        if (secureField()) {
            showSkillStrip("Magican actions never read secure or numeric fields")
            return
        }
        val capture = captureField()
        if (capture.text.isBlank()) {
            showSkillStrip("Select or type something first")
            return
        }
        if (skill.lane == KeyboardLane.Act) {
            showConfirmation(skill, capture)
        } else {
            runLanguageSkill(skill, capture)
        }
    }

    private fun showConfirmation(skill: KeyboardSkill, capture: Capture) {
        revealActionArea()
        showingDefaultStrip = false
        actionArea.addView(message("Run “${skill.label}” as a task?"))
        actionArea.addView(buttonRow(
            "Run" to { runAct(skill, capture) },
            "Cancel" to { showSkillStrip() },
        ))
    }

    private fun runAct(skill: KeyboardSkill, capture: Capture) {
        actionJob?.cancel()
        revealActionArea()
        showingDefaultStrip = false
        actionArea.addView(message("Creating task…"))
        actionJob = scope.launch {
            val outcome = runCatching {
                withContext(Dispatchers.IO) {
                    val goal = skill.resolved(capture.text).trim()
                    val title = goal.lineSequence().firstOrNull().orEmpty().take(72)
                        .ifBlank { skill.label }
                    val task = tasks.createTask(TaskCreateDraft(
                        title = title,
                        description = goal,
                        threadId = "keyboard",
                    )) ?: error("Magician did not return the created task")
                    tasks.execute(task.id)
                    // The keyboard is the canonical blind dispatch: the owner
                    // is inside another app with no Magician screen anywhere.
                    // The ongoing notification is how the run stays visible —
                    // iOS's Live Activity, as the shade card the owner ruled.
                    ai.magicbeans.magdroid.tasks.TaskProgressNotifier.track(
                        this@MagicanKeyboardService, task.id, title,
                    )
                    task.id
                }
            }
            outcome.fold(
                onSuccess = { showSkillStrip("Task started") },
                onFailure = { showSkillStrip(it.readable("Task could not be started")) },
            )
        }
    }

    private fun runLanguageSkill(skill: KeyboardSkill, capture: Capture) {
        actionJob?.cancel()
        revealActionArea()
        showingDefaultStrip = false
        val status = message("Asking Magician…")
        actionArea.addView(status)
        actionArea.addView(buttonRow("Cancel" to {
            actionJob?.cancel()
            showSkillStrip()
        }))
        actionJob = scope.launch {
            val outcome = runCatching {
                withContext(Dispatchers.IO) {
                    val session = MagicanKeyboardStore.askSession(this@MagicanKeyboardService)
                        ?: chat.newSession().also {
                            MagicanKeyboardStore.setAskSession(this@MagicanKeyboardService, it)
                        }
                    var accumulated = ""
                    var settled = ""
                    chat.send(
                        sessionId = session,
                        text = skill.resolved(capture.text),
                        chatTurnId = java.util.UUID.randomUUID().toString(),
                        sourceSurface = "keyboard",
                    ).collect { event ->
                        when (event) {
                            is ChatStreamEvent.Token -> accumulated += event.text
                            is ChatStreamEvent.Done -> settled = event.messages
                                .firstOrNull { !it.fromUser && it.text.isNotBlank() }
                                ?.text.orEmpty()
                            is ChatStreamEvent.Failed -> error(event.message)
                            else -> Unit
                        }
                    }
                    settled.ifBlank { accumulated }.trim().ifBlank {
                        error("Magician returned an empty answer")
                    }
                }
            }
            outcome.fold(
                onSuccess = { result -> showResult(skill, capture, result) },
                onFailure = { error ->
                    // A dead cached chat must not poison every future keyboard
                    // request; retry starts with a new authoritative session.
                    MagicanKeyboardStore.clearAskSession(this@MagicanKeyboardService)
                    showSkillStrip(error.readable("Magician could not answer"))
                },
            )
        }
    }

    private fun showResult(skill: KeyboardSkill, capture: Capture, result: String) {
        revealActionArea()
        showingDefaultStrip = false
        actionArea.addView(TextView(this).apply {
            text = result
            setTextColor(palette().text.toArgb())
            textSize = 13f
            maxLines = 3
            setPadding(dp(10), dp(7), dp(10), dp(5))
        })
        val primary = if (skill.lane == KeyboardLane.Write) "Replace" else "Insert"
        actionArea.addView(buttonRow(
            primary to {
                if (skill.lane == KeyboardLane.Write) replaceCapture(capture, result)
                else currentInputConnection?.commitText(result, 1)
                showSkillStrip()
            },
            "Cancel" to { showSkillStrip() },
        ))
    }

    private fun captureField(): Capture {
        val connection = currentInputConnection ?: return Capture("", false)
        val selection = connection.getSelectedText(0)?.toString().orEmpty()
        if (selection.isNotBlank()) return Capture(selection.take(MaxCaptureChars), true)
        val extracted = connection.getExtractedText(ExtractedTextRequest(), 0)?.text?.toString().orEmpty()
        return Capture(extracted.take(MaxCaptureChars), false)
    }

    private fun replaceCapture(capture: Capture, result: String) {
        val connection = currentInputConnection ?: return
        if (!capture.hadSelection) connection.performContextMenuAction(android.R.id.selectAll)
        connection.commitText(result, 1)
    }

    private fun secureField(): Boolean = isSecureKeyboardField(currentInputEditorInfo?.inputType ?: 0)

    /**
     * Relabel the letter keys for the current shift state.
     *
     * Shift used to call [renderKeys], which tears down and reallocates every
     * key — and because shift auto-clears after one letter, typing a single
     * capital rebuilt the whole keyboard twice. The keys do not change, only
     * their captions do.
     */
    private fun applyShift() {
        val locale = java.util.Locale.forLanguageTag(MagicanKeyboardStore.language(this).languageTag)
        letterKeys.forEach { (key, button) ->
            button.text = if (shifted) key.uppercase(locale) else key
        }
    }

    private fun renderKeys() {
        keyboardArea.removeAllViews()
        letterKeys.clear()
        if (symbols) {
            listOf(
                listOf("1", "2", "3", "4", "5", "6", "7", "8", "9", "0"),
                listOf("@", "#", "₹", "_", "&", "-", "+", "(", ")", "/"),
                listOf("*", "\"", "'", ":", ";", "!", "?", "⌫"),
                listOf("ABC", "🌐", ",", "space", ".", "↵"),
            ).forEach { keyboardArea.addView(keyRow(it)) }
            return
        }
        listOf("qwertyuiop", "asdfghjkl").forEach { letters ->
            keyboardArea.addView(keyRow(letters.map { it.toString() }))
        }
        keyboardArea.addView(keyRow(listOf("⇧") + "zxcvbnm".map { it.toString() } + listOf("⌫")))
        keyboardArea.addView(keyRow(listOf("123", "🌐", ",", "space", ".", "↵")))
    }

    private fun keyRow(keys: List<String>): LinearLayout = LinearLayout(this).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER
        keys.forEach { key ->
            addView(keyButton(key), LinearLayout.LayoutParams(0, dp(46), when (key) {
                "space" -> 4f
                "123", "ABC", "🌐" -> 1.35f
                else -> 1f
            }).apply { setMargins(dp(2), dp(2), dp(2), dp(2)) })
        }
    }

    private fun keyButton(key: String): Button = Button(this).apply {
        val isLetter = key.length == 1 && key[0].isLetter()
        if (isLetter) letterKeys += key to this
        text = when {
            isLetter && shifted -> key.uppercase()
            else -> key
        }
        textSize = if (key == "space") 11f else 17f
        isAllCaps = false
        setTextColor(palette().text.toArgb())
        // Letters on the raised surface, the rest on the flat one, which is how
        // every keyboard tells a letter from a modifier without a legend.
        setBackgroundColor(
            if (isLetter || key == "space") palette().elevated.toArgb()
            else palette().surface.toArgb(),
        )
        setPadding(0, 0, 0, 0)
        setOnClickListener { pressKey(key) }
        // The second way in, as on iOS, and what the resting hint promises.
        // iOS has to tell a held space from a dragged one because its space is
        // also a cursor trackpad; this keyboard has no space-drag, so a long
        // press is unambiguous here.
        if (key == "space") {
            setOnLongClickListener {
                if (secureField()) {
                    false
                } else {
                    if (!aiRevealed) toggleAiSurface()
                    true
                }
            }
        }
    }

    private fun pressKey(key: String) {
        val connection = currentInputConnection ?: return
        when (key) {
            "⇧" -> { shifted = !shifted; applyShift() }
            "⌫" -> {
                revertState = null
                connection.deleteSurroundingText(1, 0)
                refreshCandidateStrip()
            }
            "space" -> {
                commitWordBoundary(connection, " ")
                refreshCandidateStrip()
            }
            "↵" -> {
                revertState = null
                val action = currentInputEditorInfo?.imeOptions?.and(EditorInfo.IME_MASK_ACTION)
                    ?: EditorInfo.IME_ACTION_NONE
                if (action != EditorInfo.IME_ACTION_NONE && action != EditorInfo.IME_ACTION_UNSPECIFIED) {
                    connection.performEditorAction(action)
                } else connection.commitText("\n", 1)
                refreshCandidateStrip()
            }
            "🌐" -> switchKeyboard()
            "123" -> { symbols = true; renderKeys() }
            "ABC" -> { symbols = false; renderKeys() }
            else -> {
                if (key.length == 1 && key.first() in BOUNDARY_PUNCTUATION && !symbols) {
                    commitWordBoundary(connection, key)
                    refreshCandidateStrip()
                    return
                }
                revertState = null
                val locale = java.util.Locale.forLanguageTag(MagicanKeyboardStore.language(this).languageTag)
                val output = if (shifted) key.uppercase(locale) else key
                connection.commitText(output, 1)
                if (shifted) { shifted = false; applyShift() }
                if (!symbols) refreshCandidateStrip()
            }
        }
    }

    override fun onUpdateSelection(
        oldSelStart: Int,
        oldSelEnd: Int,
        newSelStart: Int,
        newSelEnd: Int,
        candidatesStart: Int,
        candidatesEnd: Int,
    ) {
        super.onUpdateSelection(
            oldSelStart,
            oldSelEnd,
            newSelStart,
            newSelEnd,
            candidatesStart,
            candidatesEnd,
        )
        if (::candidateArea.isInitialized) refreshCandidateStrip()
    }

    private fun currentPrefix(): String = currentInputConnection
        ?.getTextBeforeCursor(64, 0)
        ?.toString()
        .orEmpty()
        .takeLastWhile(Char::isLetter)

    /**
     * Commit a word boundary — a space or sentence punctuation — applying the
     * confidence-gated autocorrection on the way, as iOS does at the same
     * moment.
     *
     * What gets *learned* is the word actually committed, not the raw typing:
     * a corrected typo must not accumulate repeats, or three mistypes would
     * teach the corrector to stop fixing exactly the word it fixes most. The
     * bigram records against the token before it, feeding next-word
     * prediction with the owner's own phrasing.
     */
    private fun commitWordBoundary(connection: android.view.inputmethod.InputConnection, separator: String) {
        revertState = null
        val before = connection.getTextBeforeCursor(64, 0)?.toString().orEmpty()
        val word = before.takeLastWhile(Char::isLetter)
        var committed = word

        val engine = KeyboardIntelligence.engine
        if (word.length >= 3 && engine != null && candidateField()) {
            val correction = engine.autocorrection(word)
            if (correction != null && !correction.equals(word, ignoreCase = true)) {
                val cased = CorrectionEngine.preserveCapitalization(word, correction)
                connection.deleteSurroundingText(word.length, 0)
                connection.commitText(cased + separator, 1)
                revertState = Triple(word, cased, separator)
                committed = cased
            } else {
                connection.commitText(separator, 1)
            }
        } else {
            connection.commitText(separator, 1)
        }

        if (committed.length in 2..40 && committed.all(Char::isLetter)) {
            MagicanKeyboardStore.learnWord(this, committed)
            // The token before the committed word, for the bigram. `before`
            // still ends with the raw typed word; strip it first.
            val previous = NGramPredictor
                .tailTokens(before.dropLast(word.length), max = 1)
                .lastOrNull()
            if (previous != null) MagicanKeyboardStore.learnBigram(this, previous, committed)
        }
    }

    /**
     * Take the last autocorrection back: restore what was typed, and trust it
     * so it is never corrected again. One revert is the strongest signal a
     * keyboard gets — the owner looked at the "fix" and said no.
     */
    private fun revertAutocorrect() {
        val (typed, committed, separator) = revertState ?: return
        val connection = currentInputConnection ?: return
        revertState = null
        connection.deleteSurroundingText(committed.length + separator.length, 0)
        connection.commitText(typed + separator, 1)
        MagicanKeyboardStore.trustWord(this, typed, CorrectionEngine.LEARN_THRESHOLD)
        refreshCandidateStrip(force = true)
    }

    private fun acceptSuggestion(prefix: String, word: String) {
        val connection = currentInputConnection ?: return
        // The token before what is about to change, for the bigram — read
        // before the edit so an accepted prediction learns its real context.
        val previous = NGramPredictor
            .tailTokens(
                connection.getTextBeforeCursor(64, 0)?.toString().orEmpty().dropLast(prefix.length),
                max = 1,
            )
            .lastOrNull()
        revertState = null
        connection.deleteSurroundingText(prefix.length, 0)
        connection.commitText("$word ", 1)
        MagicanKeyboardStore.learnWord(this, word)
        if (previous != null) MagicanKeyboardStore.learnBigram(this, previous, word)
        showSkillStrip()
        refreshCandidateStrip(force = true)
    }

    private fun refreshCandidateStrip(force: Boolean = false) {
        if (!::candidateArea.isInitialized) return
        // Secure fields lose the strip entirely — no corrections, no way in.
        if (secureField()) {
            invalidateSpellRequest()
            clearCandidateStrip()
            return
        }
        // A field that declines candidates — an address, a URL, a filter — still
        // keeps the strip, because the ✦ key lives on it. Dropping the row here
        // would make Magican unreachable in exactly the fields where rewriting an
        // address or a query is worth having.
        if (!candidateField()) {
            invalidateSpellRequest()
            renderCandidateStrip("", emptyList())
            return
        }

        candidateArea.visibility = View.VISIBLE
        val prefix = currentPrefix()
        if (prefix.length < MinSpellingChars) {
            invalidateSpellRequest()
            // No word in progress. This is the after-space slot: next-word
            // predictions from the n-gram engine — iOS's universal predictor —
            // plus the revert cell when the last commit was an autocorrection.
            if (prefix.isEmpty()) {
                renderCandidateStrip("", predictedWords, revert = revertState?.first)
                schedulePrediction()
            } else {
                renderCandidateStrip(prefix, emptyList())
            }
            return
        }
        if (!force && prefix == requestedPrefix) return

        requestedPrefix = prefix
        engineCandidates = emptyList()
        learnedCandidates = MagicanKeyboardStore.suggestions(this, prefix, CandidateLimit)
        renderCandidateStrip(
            prefix,
            keyboardCandidates(prefix, learned = learnedCandidates, limit = CandidateLimit),
        )

        // The spell checker is another process. Asking it on every keystroke
        // meant a round trip per letter, each answering about a word already
        // two letters out of date, and each reply relaid the candidate row.
        // A typist outruns it; waiting for a pause costs nothing, because a
        // correction for half a word was never worth showing. The correction
        // engine rides the same pause: its lookup is cheap, but its
        // completions walk the capped vocabulary, and per-letter answers
        // would each describe a word already out of date.
        spellJob?.cancel()
        spellJob = scope.launch {
            kotlinx.coroutines.delay(SpellDebounceMs)
            KeyboardIntelligence.engine?.let { engine ->
                val candidates = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.Default) {
                    engine.suggestions(prefix, CandidateLimit)
                }
                if (requestedPrefix == prefix && currentPrefix() == prefix) {
                    engineCandidates = candidates
                    renderCandidateStrip(
                        prefix,
                        keyboardCandidates(
                            prefix,
                            engine = engineCandidates,
                            learned = learnedCandidates,
                            limit = CandidateLimit,
                        ),
                    )
                }
            }
            val session = spellChecker ?: return@launch
            val sequence = ++spellRequestSequence
            runCatching {
                session.getSentenceSuggestions(
                    arrayOf(TextInfo(prefix, 0, prefix.length, SpellCookie, sequence)),
                    CandidateLimit,
                )
            }
        }
    }

    /**
     * Debounced next-word prediction for the empty-prefix slot. Same pause as
     * the spell checker, same staleness guard: predictions land only if the
     * cursor still sits where they were asked for.
     */
    private fun schedulePrediction() {
        predictJob?.cancel()
        val predictor = KeyboardIntelligence.predictor ?: return
        val context = currentInputConnection?.getTextBeforeCursor(64, 0)?.toString().orEmpty()
        if (NGramPredictor.tailTokens(context, max = 1).isEmpty()) return
        predictJob = scope.launch {
            kotlinx.coroutines.delay(SpellDebounceMs)
            val predictions = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.Default) {
                predictor.predict(context)
            }
            if (currentPrefix().isEmpty()) {
                predictedWords = predictions
                renderCandidateStrip("", predictions, revert = revertState?.first)
            }
        }
    }

    override fun onGetSuggestions(results: Array<out SuggestionsInfo>?) {
        val info = results?.firstOrNull { it.cookie == SpellCookie } ?: return
        acceptSpellingResult(info)
    }

    override fun onGetSentenceSuggestions(results: Array<out SentenceSuggestionsInfo>?) {
        val sentence = results?.firstOrNull() ?: return
        val info = (0 until sentence.suggestionsCount)
            .asSequence()
            .map(sentence::getSuggestionsInfoAt)
            .firstOrNull { it.cookie == SpellCookie }
            ?: return
        acceptSpellingResult(info)
    }

    private fun acceptSpellingResult(info: SuggestionsInfo) {
        val spelling = spellingCandidates(info, CandidateLimit)
        candidateArea.post {
            if (!candidateResultIsCurrent(
                    resultSequence = info.sequence,
                    activeSequence = spellRequestSequence,
                    requestedPrefix = requestedPrefix,
                    currentPrefix = currentPrefix(),
                    fieldSupportsCandidates = candidateField(),
                )
            ) return@post
            renderCandidateStrip(
                requestedPrefix,
                keyboardCandidates(
                    requestedPrefix,
                    engine = engineCandidates,
                    spelling = spelling,
                    learned = learnedCandidates,
                    limit = CandidateLimit,
                ),
            )
        }
    }

    private fun restartSpellChecker() {
        closeSpellChecker()
        if (!candidateField()) return
        val manager = getSystemService(TEXT_SERVICES_MANAGER_SERVICE) as? TextServicesManager ?: return
        if (!manager.isSpellCheckerEnabled) return
        val locale = java.util.Locale.forLanguageTag(MagicanKeyboardStore.language(this).languageTag)
        spellChecker = runCatching {
            manager.newSpellCheckerSession(null, locale, this, false)
                ?: manager.newSpellCheckerSession(null, null, this, true)
        }.getOrNull()
    }

    private fun closeSpellChecker() {
        invalidateSpellRequest()
        spellChecker?.close()
        spellChecker = null
    }

    private fun invalidateSpellRequest() {
        spellJob?.cancel()
        spellJob = null
        predictJob?.cancel()
        predictJob = null
        spellRequestSequence += 1
        requestedPrefix = ""
        learnedCandidates = emptyList()
        engineCandidates = emptyList()
    }

    private fun clearCandidateStrip() {
        if (!::candidateArea.isInitialized) return
        candidateArea.removeAllViews()
        // The cells went with the views; drop the references so the next
        // refresh builds them rather than relabelling detached ones.
        candidateCells.clear()
        cellsRow = null
        hintView = null
        brandKey = null
        candidateArea.visibility = View.GONE
    }

    private fun candidateField(): Boolean =
        supportsKeyboardCandidates(currentInputEditorInfo?.inputType ?: 0)

    /**
     * A fixed three-cell row keeps keys from jumping while results arrive.
     *
     * The cells are built once and then relabelled. Rebuilding them meant three
     * view allocations, a layout pass and a fresh click listener for every
     * keystroke — and the spell checker answering a moment later did it all
     * again for the same word.
     */
    private fun renderCandidateStrip(prefix: String, candidates: List<String>, revert: String? = null) {
        // The strip is the keyboard's one permanent row. It goes only where
        // Magican has nothing to offer and nothing to say — a password box.
        if (secureField()) {
            candidateArea.visibility = View.GONE
            return
        }
        ensureCandidateCells()
        candidateArea.visibility = View.VISIBLE

        // The revert cell leads when the last commit was an autocorrection:
        // "⟲ typed" takes the fix back and trusts the word, which is the one
        // affordance that keeps an autocorrect from being an argument.
        val cells = buildList {
            revert?.let { add("⟲ $it") }
            addAll(candidates)
        }

        // Candidates when there are some; otherwise the hint that says where
        // the agentic surface lives, which is the only thing that makes a
        // hidden surface discoverable.
        val showCells = candidateField() && cells.isNotEmpty()
        cellsRow?.visibility = if (showCells) View.VISIBLE else View.GONE
        syncHint()

        if (!showCells) return
        candidateCells.forEachIndexed { index, cell ->
            val label = cells.getOrNull(index)
            if (label == null) {
                cell.text = ""
                cell.isEnabled = false
                // Invisible rather than gone: the row must hold its height, or
                // the keys move under a thumb already travelling towards them.
                cell.visibility = View.INVISIBLE
                cell.setOnClickListener(null)
            } else {
                cell.text = label
                cell.isEnabled = true
                cell.visibility = View.VISIBLE
                val isRevert = revert != null && index == 0
                cell.setOnClickListener {
                    if (isRevert) revertAutocorrect() else acceptSuggestion(prefix, label)
                }
            }
        }
    }

    /**
     * Empty the action row and make sure it is on screen.
     *
     * Anything worth putting there — a confirmation, a result, a refusal —
     * implies the surface is open, so the two are not left to be kept in step
     * by hand.
     */
    private fun revealActionArea() {
        actionArea.removeAllViews()
        actionArea.visibility = View.VISIBLE
        aiRevealed = true
        styleBrandKey()
    }

    /**
     * Reveal or hide the Magican action row.
     *
     * Hidden is the resting state, which is the whole point: the keyboard types
     * like a keyboard, and the agentic surface is summoned. It used to occupy a
     * permanent second row whether or not anybody wanted it.
     */
    private fun toggleAiSurface() {
        if (!aiRevealed) {
            // Reveals, styles the key, and fills the row.
            showSkillStrip()
            return
        }
        aiRevealed = false
        actionArea.removeAllViews()
        actionArea.visibility = View.GONE
        showingDefaultStrip = false
        styleBrandKey()
    }

    /** A bare glyph at rest, a filled pill when open — as on iOS. */
    private fun styleBrandKey() {
        val key = brandKey ?: return
        val theme = palette()
        key.text = if (aiRevealed) "✕" else "✦"
        key.setTextColor(if (aiRevealed) theme.onAccent.toArgb() else theme.accent.toArgb())
        key.setBackgroundColor(if (aiRevealed) theme.accent.toArgb() else Color.TRANSPARENT)
        key.contentDescription = if (aiRevealed) "Close Magican actions" else "Open Magican actions"
        syncHint()
    }

    /**
     * Whether the strip's hint is warranted right now.
     *
     * Shared by the two callers that can change the answer — a refresh that
     * found candidates, and the ✦ opening or closing — because keeping the same
     * three-way rule in both is how they drift apart.
     */
    private fun syncHint() {
        val cellsShowing = cellsRow?.visibility == View.VISIBLE
        hintView?.visibility = when {
            // Candidates own the row; the hint is not competing with them.
            cellsShowing -> View.GONE
            // It describes what is already on screen. INVISIBLE, not GONE: the
            // row must keep its height or the keys jump.
            aiRevealed -> View.INVISIBLE
            else -> View.VISIBLE
        }
    }

    private fun ensureCandidateCells() {
        if (candidateCells.isNotEmpty()) return
        candidateArea.removeAllViews()
        val strip = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(2), 0, dp(2), dp(2))
        }
        val hint = TextView(this).apply {
            text = "Hold space or tap ✦ for Magican"
            textSize = 12f
            setTextColor(palette().secondaryText.copy(alpha = 0.45f).toArgb())
            setPadding(dp(8), 0, dp(8), 0)
            gravity = Gravity.CENTER_VERTICAL
        }
        hintView = hint
        strip.addView(hint, LinearLayout.LayoutParams(0, dp(38), 1f))

        val row = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER
            setPadding(dp(2), 0, dp(2), dp(2))
        }
        repeat(CandidateLimit) {
            val cell = candidateChip("") {}
            candidateCells += cell
            row.addView(
                cell,
                LinearLayout.LayoutParams(0, dp(38), 1f).apply {
                    setMargins(dp(2), 0, dp(2), 0)
                },
            )
        }
        cellsRow = row
        strip.addView(row, LinearLayout.LayoutParams(0, dp(38), 1f))

        // The one persistent, discoverable way in. On the trailing edge, where
        // iOS puts it, so a thumb learns one place for it on either phone.
        val key = Button(this).apply {
            textSize = 16f
            isAllCaps = false
            setTypeface(typeface, Typeface.BOLD)
            setPadding(0, 0, 0, 0)
            setOnClickListener { toggleAiSurface() }
        }
        brandKey = key
        styleBrandKey()
        strip.addView(key, LinearLayout.LayoutParams(dp(40), dp(32)).apply {
            setMargins(dp(2), 0, dp(2), 0)
        })

        candidateArea.addView(strip)
    }

    private fun switchKeyboard() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P && switchToNextInputMethod(false)) return
        (getSystemService(INPUT_METHOD_SERVICE) as? InputMethodManager)?.showInputMethodPicker()
    }

    private fun chip(label: String, onClick: () -> Unit) = Button(this).apply {
        text = label
        textSize = 12f
        isAllCaps = false
        setTextColor(palette().accent.toArgb())
        // Without this the platform's grey button drawable shows through, which
        // is the one surface in the keyboard that belonged to no theme at all.
        // A soft wash of the accent is what the app tints its own chips with.
        setBackgroundColor(palette().accent.copy(alpha = 0.14f).toArgb())
        setTypeface(typeface, Typeface.BOLD)
        setPadding(dp(10), 0, dp(10), 0)
        setOnClickListener { onClick() }
        layoutParams = LinearLayout.LayoutParams(LinearLayout.LayoutParams.WRAP_CONTENT, dp(38)).apply {
            setMargins(dp(2), 0, dp(2), 0)
        }
    }

    private fun candidateChip(label: String, onClick: () -> Unit) = Button(this).apply {
        text = label
        contentDescription = "Spelling suggestion $label"
        textSize = 13f
        isAllCaps = false
        maxLines = 1
        setTextColor(palette().text.toArgb())
        setBackgroundColor(palette().elevated.toArgb())
        setPadding(dp(6), 0, dp(6), 0)
        setOnClickListener { onClick() }
    }

    private fun message(text: String) = TextView(this).apply {
        this.text = text
        setTextColor(palette().secondaryText.toArgb())
        textSize = 12f
        gravity = Gravity.CENTER_VERTICAL
        setPadding(dp(10), dp(5), dp(10), dp(5))
    }

    private fun buttonRow(vararg actions: Pair<String, () -> Unit>) = LinearLayout(this).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.END
        actions.forEach { (label, action) -> addView(chip(label, action)) }
    }

    private fun Throwable.readable(fallback: String): String =
        message?.trim()?.takeIf(String::isNotBlank)?.take(140) ?: fallback

    private fun dp(value: Int): Int = (value * resources.displayMetrics.density).toInt()

    private data class Capture(val text: String, val hadSelection: Boolean)

    private companion object {
        const val MaxCaptureChars = 8_000
        const val CandidateLimit = 3
        const val MinSpellingChars = 2

        /**
         * How long typing must pause before the spell checker is asked.
         *
         * Short enough to feel immediate at the end of a word, long enough
         * that a run of letters produces one cross-process request instead
         * of one per key.
         */
        const val SpellDebounceMs = 140L
        const val SpellCookie = 0x55554141

        /** Word boundaries that commit + autocorrect, matching iOS's set. */
        val BOUNDARY_PUNCTUATION = setOf('.', ',', '!', '?', ';', ':')
    }
}
