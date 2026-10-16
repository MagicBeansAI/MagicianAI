package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.attention.AttentionItem
import ai.magicbeans.magdroid.attention.AttentionLane
import ai.magicbeans.magdroid.attention.AttentionViewModel
import ai.magicbeans.magdroid.attention.isActionable
import ai.magicbeans.magdroid.attention.isStalledWait
import ai.magicbeans.magdroid.attention.offers
import ai.magicbeans.magdroid.attention.openTaskId
import ai.magicbeans.magdroid.attention.request
import ai.magicbeans.magdroid.attention.stalledExecutionId
import ai.magicbeans.magdroid.tasks.PlanApprovalActions
import ai.magicbeans.magdroid.tasks.PlanApprovalChoice
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.text.style.TextAlign
import kotlinx.coroutines.delay
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.repeatOnLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel

/**
 * What is waiting on the owner.
 *
 * This tab has had a badge and a deep link from Today since before it had a
 * screen — it rendered a placeholder describing what it would show. A count
 * that says three things need you, leading to a paragraph about the concept of
 * needing you, is worse than an empty state: nothing about it reads as
 * unfinished.
 *
 * Lanes are a view onto one response, not five fetches. The server returns
 * every lane together, so switching tabs is free and only "load more" goes back
 * to the network.
 */
@Composable
fun AttentionScreen(
    onOpenTask: (String) -> Unit = {},
    onOpenThread: (String) -> Unit = {},
    onOpenSettings: (() -> Unit)? = null,
    /** Open a task on its Plan tab, for a plan approval's Review. */
    onReviewPlan: (String) -> Unit = onOpenTask,
) {
    val viewModel: AttentionViewModel = viewModel()
    val state by viewModel.state.collectAsStateWithLifecycle()
    // The ViewModel outlives a tab visit. Reconcile the ledger on return as
    // well as after backgrounding, when its last socket update may be old.
    androidx.lifecycle.compose.LifecycleResumeEffect(Unit) {
        viewModel.refresh()
        onPauseOrDispose { }
    }
    // Live while visible: an ask raised or answered on another surface moves
    // this list (and closes an open sheet) without a pull. Bound to RESUMED so
    // a backgrounded or hidden tab does not keep fetching.
    val lifecycle = androidx.lifecycle.compose.LocalLifecycleOwner.current.lifecycle
    LaunchedEffect(viewModel, lifecycle) {
        lifecycle.repeatOnLifecycle(androidx.lifecycle.Lifecycle.State.RESUMED) {
            viewModel.followLive()
        }
    }

    // Today hands the reader an item id; this surface owns resolving it. The
    // request is consumed so returning to the tab later does not re-trigger it.
    val requested by AttentionDeepLinks.itemId.collectAsStateWithLifecycle()
    LaunchedEffect(requested, state.items) {
        val id = requested ?: return@LaunchedEffect
        val item = state.items.firstOrNull { candidate ->
            candidate.id == id || candidate.request(candidate.metadata).correlationId == id
        } ?: return@LaunchedEffect
        AttentionDeepLinks.consume(id)
        if (item.isActionable) viewModel.openAnswer(item)
    }

    state.answering?.let { item ->
        val request = item.request(item.metadata)
        AnswerSheet(
            request = request,
            submitting = state.submitting,
            error = state.answerError,
            // Closing a secret ask without answering is an answer: the pending
            // operation retires so a fresh challenge can be raised instead of
            // waiting out its window.
            onDismiss = {
                if (request.isSensitive && !state.submitting) {
                    viewModel.submitAnswer(ai.magicbeans.magdroid.chat.HitlResponseValue.Aborted("dismissed"))
                } else {
                    viewModel.closeAnswer()
                }
            },
            onSubmit = viewModel::submitAnswer,
            loadAlignmentDetail = viewModel::alignmentDetail,
            onOpenTask = { item.openTaskId?.let(onOpenTask) },
            // Review leaves the sheet open behind it: the owner comes back to
            // decide, and closing it would make them find the item again.
            onReviewPlan = item.openTaskId?.let { taskId -> { onReviewPlan(taskId) } },
            onPlanChoice = viewModel::answerPlan,
        )
    }

    Column(Modifier.fillMaxSize()) {
        // A mode, not a lane. History is not a category of pending work, and
        // sitting it in the lane row would make it look like one more thing to
        // do rather than a record of things already done.
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                if (state.showHistory) "Answered" else "Needs you",
                color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold,
                modifier = Modifier.weight(1f),
            )
            TextButton(onClick = { viewModel.showHistory(!state.showHistory) }) {
                Text(if (state.showHistory) "Back" else "History", color = Coral, fontSize = 13.sp)
            }
        }

        if (state.showHistory) {
            HistoryList(state.resolved, state.historyLoading)
            return@Column
        }

        LaneTabs(
            selected = state.lane,
            countOf = state::count,
            onSelect = viewModel::selectLane,
        )

        // The sheet closed because the ask was settled on another surface.
        // Said here, over the list, so the sheet vanishing is explained.
        state.answerNotice?.let { notice ->
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(notice, color = Muted, fontSize = 12.sp, modifier = Modifier.weight(1f))
                TextButton(onClick = viewModel::clearAnswerNotice) {
                    Text("OK", color = Coral, fontSize = 13.sp)
                }
            }
        }

        BulkBar(
            pending = state.diffApprovals.size,
            busy = state.approvingAll,
            notice = state.bulkNotice,
            onApproveAll = viewModel::approveAllDiffApprovals,
            onDismissNotice = viewModel::clearBulkNotice,
        )

        when {
            state.setupRequired -> AttentionNotice(
                "Magician is not set up on this phone yet.",
                "Add the host and credentials in Settings, then pull to try again.",
            )

            state.loading && state.items.isEmpty() -> Box(
                Modifier.fillMaxSize(),
                contentAlignment = Alignment.Center,
            ) { CircularProgressIndicator(color = Coral) }

            state.lane == AttentionLane.Messages -> MessagesLane(
                followUps = state.visibleFollowUps,
                onResolve = viewModel::resolveFollowUp,
                failure = state.followUpsFailure,
                error = state.error,
                onRetry = viewModel::refresh,
                onOpenSettings = onOpenSettings,
            )

            // A failed read is not an empty feed. Saying "Nothing needs you"
            // when the request never landed tells the owner the one thing that
            // is definitely not known — and it was said in exactly the case
            // where something might well need them.
            state.items.isEmpty() && state.failure != null -> FailurePane(
                state.failure!!,
                onRetry = viewModel::refresh,
                onOpenSettings = onOpenSettings,
            )

            state.items.isEmpty() -> AttentionNotice(
                // Named for the lane, because "nothing here" in Failed means
                // something different — and better — than in Requests.
                when (state.lane) {
                    AttentionLane.Failed -> "Nothing has failed."
                    AttentionLane.All -> "Nothing needs you."
                    else -> "No ${state.lane.label.lowercase()} waiting."
                },
                "Anything that needs a decision will appear here.",
            )

            else -> LazyColumn(
                Modifier.fillMaxSize(),
                contentPadding = PaddingValues(16.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                items(state.items, key = { it.id }) { item ->
                    // Dismiss is for failed cards only, as on iOS and the web.
                    // A request that needs an answer should be answered, not
                    // swiped away.
                    if (item.isStalledWait) {
                        // Nothing to answer: the run waits with no question.
                        // The way out is cancelling it or looking at the task.
                        StalledWaitRow(
                            item = item,
                            cancelling = item.id in state.cancelling,
                            onCancel = { viewModel.cancelStalled(item) },
                            onOpenTask = item.openTaskId?.let { id -> { onOpenTask(id) } },
                        )
                    } else if (item.failed) {
                        SwipeToDismissRow(onDismiss = { viewModel.dismiss(item) }) {
                            AttentionRow(item, item.id == requested) {
                                item.taskId?.let(onOpenTask) ?: item.uiThreadId?.let(onOpenThread)
                            }
                        }
                    } else {
                        AttentionRow(item, item.id == requested) {
                            // Something still waiting is answered here. Anything
                            // else is history, and history opens where it lives.
                            if (item.isActionable) viewModel.openAnswer(item)
                            else item.taskId?.let(onOpenTask) ?: item.uiThreadId?.let(onOpenThread)
                        }
                    }
                }

                if (state.hasMore) {
                    item {
                        LoadMoreRow(state.loadingMore, viewModel::loadMore)
                    }
                }

                // A failed page keeps the rows already read and says why,
                // rather than emptying a list somebody is part way down. Retry
                // is offered here too: the rows on screen are stale, and the
                // owner should not have to leave the tab to ask for fresh ones.
                state.failure?.let { problem ->
                    item(key = "attention-failure") {
                        FailureBanner(problem, onRetry = viewModel::refresh)
                    }
                }

                state.error?.let { problem ->
                    item(key = "attention-action-error") {
                        Text(problem, color = Coral, fontSize = 12.sp)
                    }
                }

                // Offered only while the card is still known. Once a refresh
                // lands the row is gone from the feed and there is nothing
                // local left to put back.
                state.lastDismissed?.let { dismissed ->
                    item {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text(
                                "Dismissed \"${dismissed.title}\"",
                                color = Muted, fontSize = 12.sp,
                                modifier = Modifier.weight(1f),
                            )
                            TextButton(onClick = viewModel::undoDismiss) {
                                Text("Undo", color = Coral, fontSize = 13.sp)
                            }
                        }
                    }
                }
            }
        }
    }
}

/**
 * The form for answering one waiting item.
 *
 * Draws the same input kinds the chat escalation card does, through the same
 * composables and the same composer. Two places that ask for a password should
 * not be two implementations of asking for a password — and the wire shapes are
 * validated server-side, so a second one would be a second chance to get them
 * wrong.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun AnswerSheet(
    request: ai.magicbeans.magdroid.attention.AttentionRequest,
    submitting: Boolean,
    error: String?,
    onDismiss: () -> Unit,
    onSubmit: (ai.magicbeans.magdroid.chat.HitlResponseValue) -> Unit,
    onOpenTask: () -> Unit,
    onReviewPlan: (() -> Unit)? = null,
    onPlanChoice: (PlanApprovalChoice) -> Unit = {},
    /** Where the review link goes; null routes it in-app (Work) or to the browser. */
    onOpenReview: ((String) -> Unit)? = null,
    loadAlignmentDetail: suspend (String) -> ai.magicbeans.magdroid.attention.AlignmentDetailOutcome =
        { ai.magicbeans.magdroid.attention.AlignmentDetailOutcome.Unavailable },
) {
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = Ground,
    ) {
        AskAnswerForm(
            request = request,
            submitting = submitting,
            error = error,
            onSubmit = onSubmit,
            onReviewPlan = onReviewPlan,
            onPlanChoice = onPlanChoice,
            onOpenReview = onOpenReview,
            loadAlignmentDetail = loadAlignmentDetail,
            modifier = Modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 18.dp)
                .padding(bottom = 30.dp),
        )
    }
}

/**
 * One ask's answer controls — the single implementation the Attention sheet,
 * the composer ask dock and a task's deep panel all draw, so an ask reads and
 * posts the same wherever it is answered: choices, confirm / deny, a Reject /
 * Approve decision, text, file paths, external actions, forms, grants and
 * secrets (the last two are drawn in place only inside the sheet; the inline
 * surfaces hand them to it, see `AskSurfaces.answersInSheet`).
 */
@Composable
internal fun AskAnswerForm(
    request: ai.magicbeans.magdroid.attention.AttentionRequest,
    submitting: Boolean,
    error: String?,
    onSubmit: (ai.magicbeans.magdroid.chat.HitlResponseValue) -> Unit,
    onReviewPlan: (() -> Unit)? = null,
    onPlanChoice: (PlanApprovalChoice) -> Unit = {},
    onOpenReview: ((String) -> Unit)? = null,
    loadAlignmentDetail: suspend (String) -> ai.magicbeans.magdroid.attention.AlignmentDetailOutcome =
        { ai.magicbeans.magdroid.attention.AlignmentDetailOutcome.Unavailable },
    /** Inline surfaces draw the prompt in their own header. */
    showPrompt: Boolean = true,
    modifier: Modifier = Modifier.fillMaxWidth(),
) {
    var typed by remember(request.correlationId) { mutableStateOf("") }
    val picked = remember(request.correlationId) { mutableStateListOf<String>() }
    val formValues = remember(request.correlationId) { mutableStateMapOf<String, String>() }
    val formSkipped = remember(request.correlationId) { mutableStateMapOf<String, Boolean>() }
    val formPicked = remember(request.correlationId) { mutableStateMapOf<String, List<String>>() }

    // A secret's collection window, counted down on screen. Past it the fields
    // refuse the value and the sheet offers a fresh ask instead — a code typed
    // after its window is a code the destination will reject.
    var nowMs by remember(request.correlationId) { mutableStateOf(System.currentTimeMillis()) }
    val deadlineMs = request.sensitiveDeadlineMs
    LaunchedEffect(request.correlationId, deadlineMs) {
        if (deadlineMs == null) return@LaunchedEffect
        while (true) {
            nowMs = System.currentTimeMillis()
            if (nowMs >= deadlineMs) break
            delay(1_000)
        }
    }
    val secretExpired = deadlineMs != null && nowMs >= deadlineMs
    val secondsLeft = deadlineMs?.let { ((it - nowMs).coerceAtLeast(0) + 999) / 1_000 }

    fun send(option: ai.magicbeans.magdroid.chat.EscalationOption? = null) {
        val value = ai.magicbeans.magdroid.chat.HitlResponseComposer.compose(
            inputType = request.inputType,
            option = option,
            text = typed,
            selectedIds = picked.toList(),
            allowsMultipleFiles = request.wantsMultiplePaths,
            sensitive = request.isSensitive,
        ) ?: return
        onSubmit(value)
    }

    fun sendForm(skipAll: Boolean = false) {
        val answers = request.formQuestions.map { question ->
            val skipped = skipAll || formSkipped[question.id] == true
            ai.magicbeans.magdroid.chat.FormAnswer(
                id = question.id,
                skipped = skipped,
                value = if (skipped || question.isChoice) null else formValues[question.id],
                selectedIds = if (skipped || !question.isChoice) emptyList() else formPicked[question.id].orEmpty(),
            )
        }
        val value = ai.magicbeans.magdroid.chat.HitlResponseComposer.composeForm(answers) ?: return
        onSubmit(value)
    }

    val formReady = request.formQuestions.isNotEmpty() && request.formQuestions.all { question ->
        formSkipped[question.id] == true || if (question.isChoice) {
            formPicked[question.id].orEmpty().isNotEmpty()
        } else {
            (formValues[question.id] ?: "").trim().isNotEmpty()
        }
    }

    Column(
        modifier,
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        request.chainLabel?.let {
            Text(it.uppercase(), color = Coral, fontSize = 11.sp, fontWeight = FontWeight.Bold)
        }
        // A delegated child's ask, surfaced on the parent task. Naming the
        // agent says whose question it is; the answer goes to the child.
        request.fromLabel?.let {
            Text(it.uppercase(), color = Secondary, fontSize = 11.sp, fontWeight = FontWeight.Bold)
        }
        if (showPrompt) {
            Text(request.prompt, color = Ink, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
        }
        request.hint?.takeIf { it.isNotBlank() && it != request.prompt }?.let {
            Text(it, color = Secondary, fontSize = 12.sp)
        }

        // The thing being asked about. The server sends a link and its own
        // label with the request; both were decoded here and neither was
        // shown, so answering meant deciding without being able to look.
        // An app route (a Relentless review's `/work?view=quality&run=…`)
        // opens in Work; anything else opens in the browser.
        request.reviewHref?.takeIf { it.isNotBlank() }?.let { href ->
            val context = androidx.compose.ui.platform.LocalContext.current
            TextButton(
                onClick = { onOpenReview?.invoke(href) ?: openAskReview(context, href) },
                contentPadding = PaddingValues(0.dp),
            ) {
                // The server's own wording when it sent one. "View source"
                // is iOS's fallback and says the least that is still true.
                Text(
                    request.reviewLabel?.takeIf { it.isNotBlank() } ?: "View source",
                    color = Coral, fontSize = 13.sp,
                )
            }
        }

        if (request.sensitive != null) {
            // What will happen to the value, the window when there is one,
            // and past the window a fresh ask.
            Surface(color = Panel, shape = RoundedCornerShape(8.dp), modifier = Modifier.fillMaxWidth()) {
                Column(Modifier.padding(10.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            when {
                                secretExpired -> "This code's window has closed — ask for a fresh one."
                                request.isOneTime -> "Used once, then discarded. Never shown to the assistant."
                                else -> "Held privately for this run. Never shown to the assistant."
                            },
                            color = Secondary,
                            fontSize = 12.sp,
                            modifier = Modifier.weight(1f),
                        )
                        if (secondsLeft != null && !secretExpired) {
                            Text(
                                if (secondsLeft >= 120) "${secondsLeft / 60} min left" else "${secondsLeft}s left",
                                color = Secondary,
                                fontSize = 12.sp,
                                fontFamily = FontFamily.Monospace,
                            )
                        }
                    }
                    if (secretExpired) {
                        TextButton(
                            onClick = {
                                typed = ""
                                onSubmit(ai.magicbeans.magdroid.chat.HitlResponseValue.Aborted("fresh_code_requested"))
                            },
                            enabled = !submitting,
                            contentPadding = PaddingValues(0.dp),
                        ) {
                            Text("Request a fresh code", color = Coral, fontSize = 13.sp)
                        }
                    }
                }
            }
        }

        val decision = request.decisionPair
        if (decision != null) {
            // Reject / Approve as a pair (the Relentless review's "Reject
            // draft" / "Approve draft"), posted as the chosen option id.
            val (reject, approve) = decision
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                PlanChoiceButton(
                    label = reject.label.ifBlank { "Reject" },
                    fill = Danger.copy(alpha = 0.12f),
                    ink = Danger,
                    enabled = !submitting,
                    modifier = Modifier.weight(1f),
                ) { onSubmit(ai.magicbeans.magdroid.chat.HitlResponseValue.Choice(reject.id)) }
                PlanChoiceButton(
                    label = approve.label.ifBlank { "Approve" },
                    fill = AccentFill,
                    ink = OnAccent,
                    enabled = !submitting,
                    modifier = Modifier.weight(1f),
                ) { onSubmit(ai.magicbeans.magdroid.chat.HitlResponseValue.Choice(approve.id)) }
            }
        } else when (request.renderKind) {
            "text", "guidance", "password", "otp" -> EscalationTextAnswer(
                inputType = request.inputType,
                value = typed,
                onValue = { typed = it },
                enabled = !submitting && !secretExpired,
                onSubmit = { send() },
                renderKind = request.renderKind,
                placeholder = request.placeholder.takeIf { it != "Type your response…" },
            )

            "file_path" -> {
                Text(request.pathFieldLabel, color = Secondary, fontSize = 12.sp, fontWeight = FontWeight.Medium)
                MagicianTextField(
                    value = typed,
                    onValueChange = { typed = it },
                    enabled = !submitting,
                    singleLine = !request.wantsMultiplePaths,
                    maxLines = if (request.wantsMultiplePaths) 4 else 1,
                    placeholder = {
                        Text(
                            request.placeholder.ifBlank { "/path/to/file" },
                            color = Muted,
                            fontSize = 13.sp,
                        )
                    },
                    textStyle = LocalTextStyle.current.copy(color = Ink, fontSize = 13.sp),
                    modifier = Modifier.fillMaxWidth(),
                )
                EscalationSubmit(
                    enabled = !submitting && ai.magicbeans.magdroid.chat.HitlResponseComposer.compose(
                        inputType = "file_path",
                        text = typed,
                        allowsMultipleFiles = request.wantsMultiplePaths,
                    ) != null,
                ) { send() }
            }

            "tool_authorization", "sandbox_override" -> {
                GrantBlock(request)
                val deny = request.grantDenyOption
                    ?: ai.magicbeans.magdroid.attention.AttentionGrantOption("deny", "Deny")
                EscalationSubmit(
                    enabled = !submitting,
                    label = deny.label,
                ) {
                    onSubmit(ai.magicbeans.magdroid.chat.HitlResponseValue.Choice(deny.id))
                }
                request.grantAllowOptions.forEach { grant ->
                    Surface(
                        color = Color.Transparent,
                        shape = RoundedCornerShape(6.dp),
                        border = BorderStroke(1.dp, Danger.copy(alpha = 0.5f)),
                        modifier = Modifier
                            .fillMaxWidth()
                            .then(
                                if (submitting) Modifier
                                else Modifier.clickable {
                                    onSubmit(ai.magicbeans.magdroid.chat.HitlResponseValue.Choice(grant.id))
                                },
                            ),
                    ) {
                        Text(
                            grant.label,
                            color = Danger,
                            fontSize = 13.sp,
                            fontWeight = FontWeight.Medium,
                            textAlign = TextAlign.Center,
                            modifier = Modifier.padding(vertical = 9.dp).fillMaxWidth(),
                        )
                    }
                }
            }

            "external_action" -> {
                // An authorize page (mcp_oauth) is opened here rather than
                // described: the owner finishes there, then says so below.
                request.externalUrl?.let { url ->
                    val context = androidx.compose.ui.platform.LocalContext.current
                    Surface(
                        color = AccentFill,
                        shape = RoundedCornerShape(8.dp),
                        modifier = Modifier
                            .fillMaxWidth()
                            .then(
                                if (submitting) Modifier
                                else Modifier.clickable {
                                    runCatching {
                                        context.startActivity(
                                            android.content.Intent(
                                                android.content.Intent.ACTION_VIEW,
                                                android.net.Uri.parse(url),
                                            ).addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK),
                                        )
                                    }
                                },
                            ),
                    ) {
                        Text(
                            if (request.source == ai.magicbeans.magdroid.attention.MCP_OAUTH_SOURCE) {
                                "Open sign-in page"
                            } else {
                                "Open in browser"
                            },
                            color = OnAccent,
                            fontSize = 13.sp,
                            fontWeight = FontWeight.SemiBold,
                            textAlign = TextAlign.Center,
                            modifier = Modifier.padding(vertical = 10.dp).fillMaxWidth(),
                        )
                    }
                }
                request.externalInstructions?.let { instructions ->
                    Surface(
                        color = Panel,
                        shape = RoundedCornerShape(8.dp),
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(
                            instructions,
                            color = Ink,
                            fontSize = 13.sp,
                            modifier = Modifier.padding(10.dp),
                        )
                    }
                }
                MagicianTextField(
                    value = typed,
                    onValueChange = { typed = it },
                    enabled = !submitting,
                    maxLines = 4,
                    placeholder = {
                        Text("Optional note…", color = Muted, fontSize = 13.sp)
                    },
                    textStyle = LocalTextStyle.current.copy(color = Ink, fontSize = 13.sp),
                    modifier = Modifier.fillMaxWidth(),
                )
                EscalationSubmit(
                    enabled = !submitting,
                    label = request.externalDoneLabel,
                ) {
                    onSubmit(
                        ai.magicbeans.magdroid.chat.HitlResponseValue.ExternalActionCompleted(
                            typed.trim().ifEmpty { null },
                        ),
                    )
                }
            }

            "multi_choice" -> {
                request.options.forEach { option ->
                    val on = picked.contains(option.id)
                    EscalationOptionRow(
                        option = option,
                        selected = on,
                        dimmed = false,
                        enabled = !submitting,
                        onClick = { if (on) picked.remove(option.id) else picked.add(option.id) },
                    )
                }
                EscalationSubmit(enabled = !submitting && picked.isNotEmpty()) { send() }
            }

            "confirmation" -> if (request.alignment != null) {
                AlignmentAnswer(
                    correlationId = request.correlationId,
                    alignment = requireNotNull(request.alignment),
                    submitting = submitting,
                    loadDetail = loadAlignmentDetail,
                    onSubmit = onSubmit,
                )
            } else if (PlanApprovalActions.isPlanApproval(request.source, request.inputType)) {
                PlanApprovalButtons(
                    request = request,
                    submitting = submitting,
                    onReview = onReviewPlan,
                    onChoice = onPlanChoice,
                )
            } else {
                Row(
                    Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    Surface(
                        color = Danger.copy(alpha = 0.12f),
                        shape = RoundedCornerShape(8.dp),
                        modifier = Modifier
                            .weight(1f)
                            .then(
                                if (submitting) Modifier
                                else Modifier.clickable {
                                    onSubmit(ai.magicbeans.magdroid.chat.HitlResponseValue.Confirmation(false))
                                },
                            ),
                    ) {
                        Text(
                            request.denyLabel,
                            color = Danger,
                            fontSize = 13.sp,
                            fontWeight = FontWeight.SemiBold,
                            textAlign = TextAlign.Center,
                            modifier = Modifier.padding(vertical = 10.dp).fillMaxWidth(),
                        )
                    }
                    Surface(
                        color = if (request.destructive) Color.Transparent else AccentFill,
                        shape = RoundedCornerShape(8.dp),
                        border = if (request.destructive) {
                            BorderStroke(1.dp, Danger.copy(alpha = 0.6f))
                        } else {
                            null
                        },
                        modifier = Modifier
                            .weight(1f)
                            .then(
                                if (submitting) Modifier
                                else Modifier.clickable {
                                    onSubmit(ai.magicbeans.magdroid.chat.HitlResponseValue.Confirmation(true))
                                },
                            ),
                    ) {
                        Text(
                            request.confirmLabel,
                            color = if (request.destructive) Danger else OnAccent,
                            fontSize = 13.sp,
                            fontWeight = FontWeight.SemiBold,
                            textAlign = TextAlign.Center,
                            modifier = Modifier.padding(vertical = 10.dp).fillMaxWidth(),
                        )
                    }
                }
            }

            "form" -> {
                request.formQuestions.forEach { question ->
                    val skipped = formSkipped[question.id] == true
                    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text(
                                question.text().ifBlank { "Question" },
                                color = Ink,
                                fontSize = 13.sp,
                                modifier = Modifier.weight(1f),
                            )
                            TextButton(
                                onClick = { formSkipped[question.id] = !skipped },
                                enabled = !submitting,
                                contentPadding = PaddingValues(0.dp),
                            ) {
                                Text(
                                    if (skipped) "Unskip" else "Skip",
                                    color = Secondary,
                                    fontSize = 12.sp,
                                )
                            }
                        }
                        if (!skipped && question.isChoice) {
                            val chosen = formPicked[question.id].orEmpty()
                            question.options.orEmpty().forEach { option ->
                                val on = option.id in chosen
                                EscalationOptionRow(
                                    option = option,
                                    selected = on,
                                    dimmed = false,
                                    enabled = !submitting,
                                    onClick = {
                                        formPicked[question.id] = when {
                                            question.isMultiChoice && on -> chosen - option.id
                                            question.isMultiChoice -> chosen + option.id
                                            else -> listOf(option.id)
                                        }
                                    },
                                )
                            }
                        } else if (!skipped) {
                            val fieldKind = request.sensitiveFieldKind(question.id)
                            val masked = ai.magicbeans.magdroid.chat.hitlFieldIsMasked(fieldKind)
                            MagicianTextField(
                                value = formValues[question.id].orEmpty(),
                                onValueChange = { formValues[question.id] = it },
                                enabled = !submitting && !secretExpired,
                                singleLine = true,
                                placeholder = {
                                    Text(
                                        when {
                                            fieldKind == "otp" -> "Enter the code…"
                                            masked -> "Kept private"
                                            else -> "Your answer"
                                        },
                                        color = Muted, fontSize = 13.sp,
                                    )
                                },
                                visualTransformation = if (masked) PasswordVisualTransformation() else VisualTransformation.None,
                                keyboardOptions = when {
                                    masked -> KeyboardOptions(keyboardType = KeyboardType.Password)
                                    fieldKind == "login_identifier" -> KeyboardOptions(keyboardType = KeyboardType.Email)
                                    else -> KeyboardOptions.Default
                                },
                                textStyle = LocalTextStyle.current.copy(color = Ink, fontSize = 13.sp),
                                modifier = Modifier.fillMaxWidth(),
                            )
                            if (fieldKind == "login_identifier") {
                                Text(
                                    "Kept private: used only to sign in, never shown to the assistant.",
                                    color = Secondary,
                                    fontSize = 11.sp,
                                )
                            }
                        }
                    }
                }
                Row(
                    Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    TextButton(
                        onClick = { sendForm(skipAll = true) },
                        enabled = !submitting && request.formQuestions.isNotEmpty(),
                        contentPadding = PaddingValues(0.dp),
                    ) {
                        Text("Skip all", color = Secondary, fontSize = 12.sp)
                    }
                    EscalationSubmit(enabled = !submitting && formReady && !secretExpired) { sendForm() }
                }
            }

            else -> request.options.forEach { option ->
                EscalationOptionRow(
                    option = option,
                    selected = false,
                    dimmed = false,
                    enabled = !submitting,
                    onClick = { send(option) },
                )
            }
        }

        error?.let { Text(it, color = Coral, fontSize = 12.sp) }
        if (submitting) {
            CircularProgressIndicator(Modifier.size(18.dp), color = Coral, strokeWidth = 2.dp)
        }
    }
}

/**
 * Open an ask's review link. An app route goes through Work deep-link routing
 * (`/work?view=quality&run=…&task=…` lands on that Quality run); an absolute
 * URL, or a path the app does not route, opens in the browser against the
 * paired host. Returns true when the link was routed in-app.
 */
internal fun openAskReview(context: android.content.Context, href: String): Boolean {
    val link = href.trim()
    if (link.isEmpty()) return false
    if (WorkDeepLinks.accept(link)) return true
    val absolute = if (link.startsWith("http://") || link.startsWith("https://")) {
        link
    } else {
        val host = ai.magicbeans.magdroid.access.MagicianAccess.baseUrl(context).trim().trimEnd('/')
        if (host.isEmpty()) return false
        "$host/${link.trimStart('/')}"
    }
    runCatching {
        context.startActivity(
            android.content.Intent(android.content.Intent.ACTION_VIEW, android.net.Uri.parse(absolute))
                .addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK),
        )
    }
    return false
}

@Composable
private fun GrantBlock(request: ai.magicbeans.magdroid.attention.AttentionRequest) {
    Column(
        Modifier
            .fillMaxWidth()
            .background(Danger.copy(alpha = 0.08f), RoundedCornerShape(10.dp))
            .padding(12.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Text(
            if (request.grantKind == "sandbox") "SANDBOX OVERRIDE REQUESTED" else "TOOL AUTHORIZATION REQUESTED",
            color = Danger,
            fontSize = 11.sp,
            fontWeight = FontWeight.Bold,
        )
        if (request.grantSubject.isNotEmpty()) {
            Text(
                request.grantSubject,
                color = Ink,
                fontSize = 12.sp,
                fontFamily = LocalMagicanFontFamilies.current.mono,
            )
        }
        request.grantDetail?.let {
            Text(it, color = Secondary, fontSize = 13.sp)
        }
        request.grantRoots.forEach { root ->
            Text("• $root", color = Secondary, fontSize = 11.sp, fontFamily = LocalMagicanFontFamilies.current.mono)
        }
    }
}

/**
 * Messages waiting on a reply.
 *
 * The four resolutions stay visible rather than hiding behind a swipe: on a
 * feed card dismissal is the only verb, but here the choice between "useful",
 * "acknowledge" and "dismiss" is the point, and a gesture cannot offer three
 * answers.
 */
/**
 * One tap for every code change set in the lane.
 *
 * Only diff approvals. Reviewing each card is usually not what the owner wants
 * once they have decided to take the branch — but nothing else is bulk
 * approvable, because nothing else is that uniform.
 *
 * Absent entirely when there is nothing to apply and nothing to report, so it
 * never occupies the top of an empty screen.
 */
@Composable
private fun BulkBar(
    pending: Int,
    busy: Boolean,
    notice: String?,
    onApproveAll: () -> Unit,
    onDismissNotice: () -> Unit,
) {
    if (pending == 0 && notice == null) return
    Column(
        Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        if (pending > 0) {
            Surface(
                color = Coral.copy(alpha = 0.15f),
                shape = RoundedCornerShape(10.dp),
                modifier = Modifier
                    .fillMaxWidth()
                    .then(if (busy) Modifier else Modifier.clickable { onApproveAll() }),
            ) {
                Row(
                    Modifier.fillMaxWidth().padding(vertical = 10.dp),
                    horizontalArrangement = Arrangement.Center,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    if (busy) {
                        CircularProgressIndicator(
                            Modifier.size(15.dp), color = Coral, strokeWidth = 2.dp,
                        )
                        Spacer(Modifier.width(8.dp))
                        Text("Applying…", color = Coral, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                    } else {
                        Text(
                            "Approve all ($pending)",
                            color = Coral, fontSize = 14.sp, fontWeight = FontWeight.SemiBold,
                        )
                    }
                }
            }
        }
        notice?.let {
            Row(
                Modifier.fillMaxWidth().clickable { onDismissNotice() },
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(it, color = Secondary, fontSize = 12.sp, modifier = Modifier.weight(1f))
                Text("Dismiss", color = Muted, fontSize = 11.sp)
            }
        }
    }
}

@Composable
private fun MessagesLane(
    followUps: List<ai.magicbeans.magdroid.attention.ChannelFollowUp>,
    onResolve: (ai.magicbeans.magdroid.attention.ChannelFollowUp, ai.magicbeans.magdroid.attention.FollowUpAction, String?) -> Unit,
    failure: ai.magicbeans.magdroid.net.Failure?,
    error: String?,
    onRetry: () -> Unit,
    onOpenSettings: (() -> Unit)?,
) {
    if (followUps.isEmpty() && failure != null) {
        FailurePane(failure, onRetry = onRetry, onOpenSettings = onOpenSettings)
        return
    }
    if (followUps.isEmpty()) {
        AttentionNotice("No messages waiting.", "Anything needing a reply will appear here.")
        return
    }
    LazyColumn(
        Modifier.fillMaxSize(),
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        // Messages already read stay; the bar says they may be stale.
        failure?.let { problem ->
            item(key = "messages-failure") { FailureBanner(problem, onRetry = onRetry) }
        }
        error?.let { problem ->
            item(key = "messages-action-error") {
                Text(problem, color = Coral, fontSize = 12.sp)
            }
        }
        items(followUps, key = { it.id }) { followUp ->
            Surface(
                color = Panel,
                shape = RoundedCornerShape(10.dp),
                border = BorderStroke(1.dp, BorderSoft),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            followUp.sender ?: followUp.accountAlias,
                            color = Coral, fontSize = 11.sp, fontWeight = FontWeight.SemiBold,
                            modifier = Modifier.weight(1f),
                        )
                        Text(followUp.provider, color = Muted, fontSize = 11.sp)
                    }
                    Text(
                        followUp.displayTitle,
                        color = Ink, fontSize = 14.sp, fontWeight = FontWeight.Medium,
                    )
                    followUp.summary?.takeIf { it.isNotBlank() }?.let {
                        Text(it, color = Secondary, fontSize = 12.sp, maxLines = 3)
                    }
                    followUp.reason?.takeIf { it.isNotBlank() }?.let {
                        Text(it, color = Muted, fontSize = 11.sp)
                    }

                    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        ai.magicbeans.magdroid.attention.FollowUpAction.entries.forEach { action ->
                            // Acknowledge says "handled" without opening it,
                            // which is only honest when the distillation was
                            // confident enough not to need checking.
                            val allowed = action !=
                                ai.magicbeans.magdroid.attention.FollowUpAction.Acknowledge ||
                                followUp.canAcknowledge
                            if (allowed) {
                                FollowUpActionChip(action.label) { onResolve(followUp, action, null) }
                            }
                        }
                    }
                }
            }
        }
        error?.let { item { Text(it, color = Coral, fontSize = 12.sp) } }
    }
}

@Composable
private fun FollowUpActionChip(label: String, onClick: () -> Unit) {
    Surface(
        color = Ground,
        shape = RoundedCornerShape(6.dp),
        border = BorderStroke(1.dp, BorderSoft),
        modifier = Modifier.clickable { onClick() },
    ) {
        Text(
            label,
            color = Ink, fontSize = 12.sp,
            modifier = Modifier.padding(horizontal = 10.dp, vertical = 6.dp),
        )
    }
}

@Composable
private fun LaneTabs(
    selected: AttentionLane,
    countOf: (AttentionLane) -> Long,
    onSelect: (AttentionLane) -> Unit,
) {
    ScrollableTabRow(
        selectedTabIndex = AttentionLane.entries.indexOf(selected),
        containerColor = Ground,
        contentColor = Coral,
        edgePadding = 12.dp,
        divider = {},
    ) {
        AttentionLane.entries.forEach { lane ->
            val count = countOf(lane)
            Tab(
                selected = lane == selected,
                onClick = { onSelect(lane) },
                text = {
                    Text(
                        // The count rides in the label rather than a separate
                        // badge: a tab that says "Failed" and a badge that says
                        // 2 are two things to read where one would do.
                        if (count > 0) "${lane.label} $count" else lane.label,
                        fontSize = 13.sp,
                        fontWeight = if (lane == selected) FontWeight.SemiBold else FontWeight.Normal,
                        color = if (lane == selected) Coral else Secondary,
                    )
                },
            )
        }
    }
}

/**
 * Swipe a card away, with a way back.
 *
 * `confirmValueChange` returns false so the row never stays in the dismissed
 * position: the list is about to lose it anyway, and letting the box hold an
 * empty slot until recomposition makes a successful dismiss look stuck.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SwipeToDismissRow(onDismiss: () -> Unit, content: @Composable () -> Unit) {
    val dismissState = rememberSwipeToDismissBoxState(
        confirmValueChange = { value ->
            if (value == SwipeToDismissBoxValue.EndToStart) onDismiss()
            false
        },
    )
    SwipeToDismissBox(
        state = dismissState,
        enableDismissFromStartToEnd = false,
        backgroundContent = {
            Box(
                Modifier
                    .fillMaxSize()
                    .background(Danger.copy(alpha = 0.15f), RoundedCornerShape(10.dp))
                    .padding(horizontal = 18.dp),
                contentAlignment = Alignment.CenterEnd,
            ) {
                Text("Dismiss", color = Danger, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
            }
        },
        content = { content() },
    )
}

@Composable
private fun AttentionRow(item: AttentionItem, highlighted: Boolean, onClick: () -> Unit) {
    Surface(
        color = if (highlighted) Coral.copy(alpha = 0.08f) else Panel,
        shape = RoundedCornerShape(10.dp),
        border = BorderStroke(
            1.dp,
            when {
                highlighted -> Coral.copy(alpha = 0.5f)
                item.failed -> Danger.copy(alpha = 0.45f)
                item.needsAction -> Coral.copy(alpha = 0.35f)
                else -> BorderSoft
            },
        ),
        modifier = Modifier.fillMaxWidth().clickable { onClick() },
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                StatusChip(item)
                Spacer(Modifier.weight(1f))
                Text(item.itemType.replace('_', ' '), color = Muted, fontSize = 11.sp)
            }
            Text(item.title, color = Ink, fontSize = 14.sp, fontWeight = FontWeight.Medium)
            item.request(item.metadata).fromLabel?.let {
                Text(it, color = Coral, fontSize = 11.sp, fontWeight = FontWeight.Medium)
            }
            item.summary?.takeIf { it.isNotBlank() }?.let {
                Text(it, color = Secondary, fontSize = 12.sp, maxLines = 3)
            }
            // Actions are listed, not offered. Acting on them is a later slice,
            // and a button that does nothing would repeat the mistake this
            // screen exists to fix.
            item.actions.takeIf { it.isNotEmpty() }?.let { actions ->
                Text(
                    actions.joinToString(" · ") { it.label },
                    color = Muted, fontSize = 11.sp,
                )
            }
        }
    }
}

/**
 * A run waiting on the owner with no question delivered (C3).
 *
 * Says so plainly and offers the only two things that help: cancel the run,
 * or open the task to see what it was doing.
 */
@Composable
private fun StalledWaitRow(
    item: AttentionItem,
    cancelling: Boolean,
    onCancel: () -> Unit,
    onOpenTask: (() -> Unit)?,
) {
    Surface(
        color = Panel,
        shape = RoundedCornerShape(10.dp),
        border = BorderStroke(1.dp, Danger.copy(alpha = 0.45f)),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Surface(color = Danger.copy(alpha = 0.14f), shape = RoundedCornerShape(5.dp)) {
                    Text(
                        "Stuck",
                        color = Danger,
                        fontSize = 10.sp,
                        fontWeight = FontWeight.SemiBold,
                        modifier = Modifier.padding(horizontal = 6.dp, vertical = 2.dp),
                    )
                }
                Spacer(Modifier.weight(1f))
                Text(item.itemType.replace('_', ' '), color = Muted, fontSize = 11.sp)
            }
            Text(
                "Waiting on you, but no question was delivered",
                color = Ink, fontSize = 14.sp, fontWeight = FontWeight.Medium,
            )
            item.title.takeIf { it.isNotBlank() && it != item.summary }?.let {
                Text(it, color = Secondary, fontSize = 12.sp, maxLines = 2)
            }
            item.summary?.takeIf { it.isNotBlank() && !it.startsWith("Waiting on you") }?.let {
                Text(it, color = Secondary, fontSize = 12.sp, maxLines = 3)
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                if (item.offers("cancel") && item.stalledExecutionId != null) {
                    Surface(
                        color = Danger.copy(alpha = 0.12f),
                        shape = RoundedCornerShape(6.dp),
                        modifier = if (cancelling) Modifier else Modifier.clickable { onCancel() },
                    ) {
                        Row(
                            Modifier.padding(horizontal = 12.dp, vertical = 7.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            if (cancelling) {
                                CircularProgressIndicator(Modifier.size(12.dp), color = Danger, strokeWidth = 2.dp)
                                Spacer(Modifier.width(6.dp))
                            }
                            Text(
                                if (cancelling) "Cancelling…" else "Cancel run",
                                color = Danger, fontSize = 12.sp, fontWeight = FontWeight.SemiBold,
                            )
                        }
                    }
                }
                if (item.offers("open_task") && onOpenTask != null) {
                    FollowUpActionChip("Open task", onOpenTask)
                }
            }
        }
    }
}

@Composable
private fun StatusChip(item: AttentionItem) {
    val (label, tint) = when {
        item.failed -> "Failed" to Danger
        item.needsAction -> "Needs you" to Coral
        else -> item.status.replaceFirstChar(Char::uppercase) to Secondary
    }
    Surface(color = tint.copy(alpha = 0.14f), shape = RoundedCornerShape(5.dp)) {
        Text(
            label,
            color = tint,
            fontSize = 10.sp,
            fontWeight = FontWeight.SemiBold,
            modifier = Modifier.padding(horizontal = 6.dp, vertical = 2.dp),
        )
    }
}

@Composable
private fun LoadMoreRow(busy: Boolean, onClick: () -> Unit) {
    Surface(
        color = Ground,
        shape = RoundedCornerShape(8.dp),
        border = BorderStroke(1.dp, BorderSoft),
        modifier = Modifier
            .fillMaxWidth()
            .then(if (busy) Modifier else Modifier.clickable { onClick() }),
    ) {
        Box(Modifier.fillMaxWidth().padding(vertical = 11.dp), Alignment.Center) {
            if (busy) {
                CircularProgressIndicator(Modifier.size(16.dp), color = Coral, strokeWidth = 2.dp)
            } else {
                Text("Load more", color = Coral, fontSize = 13.sp, fontWeight = FontWeight.Medium)
            }
        }
    }
}

@Composable
private fun AttentionNotice(title: String, detail: String) {
    Column(
        Modifier.fillMaxSize().padding(32.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text(title, color = Ink, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.height(6.dp))
        Text(detail, color = Muted, fontSize = 12.sp, lineHeight = 17.sp)
    }
}


/**
 * Requests already answered, newest first.
 *
 * The prompt leads because that is what a reader is looking for — what was
 * asked — with the outcome beside it. A resolution whose request fell outside
 * the fetched window never reaches here: a row that cannot say what was asked
 * is worse than one fewer row.
 */
@Composable
private fun HistoryList(
    rows: List<ai.magicbeans.magdroid.attention.ResolvedHitl>,
    loading: Boolean,
) {
    if (loading && rows.isEmpty()) {
        Box(Modifier.fillMaxSize(), Alignment.Center) { CircularProgressIndicator(color = Coral) }
        return
    }
    if (rows.isEmpty()) {
        Box(Modifier.fillMaxSize(), Alignment.Center) {
            Text("No answered requests yet.", color = Muted, fontSize = 13.sp)
        }
        return
    }
    LazyColumn(
        Modifier.fillMaxSize(),
        contentPadding = PaddingValues(12.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        items(rows, key = { it.correlationId }) { row ->
            Surface(
                color = Panel,
                shape = RoundedCornerShape(10.dp),
                border = BorderStroke(1.dp, BorderSoft),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text(row.prompt, color = Ink, fontSize = 13.sp, maxLines = 3)
                    Text(
                        listOfNotNull(row.outcome, row.decision).joinToString(" · "),
                        color = Muted, fontSize = 11.sp,
                    )
                }
            }
        }
    }
}

/**
 * A plan approval's four choices, in the order web and iOS show them:
 * Review plan · Reject · Approve · Approve & Run.
 *
 * Approve alone does not start a run, which plain Approve/Reject left the owner
 * to discover; Approve & Run says it does both. Review opens the task's Plan
 * tab and posts nothing.
 */
@Composable
private fun PlanApprovalButtons(
    request: ai.magicbeans.magdroid.attention.AttentionRequest,
    submitting: Boolean,
    onReview: (() -> Unit)?,
    onChoice: (PlanApprovalChoice) -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        onReview?.let { review ->
            TextButton(onClick = review, contentPadding = PaddingValues(0.dp)) {
                Text(PlanApprovalChoice.Review.label(), color = Coral, fontSize = 13.sp)
            }
        }
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            PlanChoiceButton(
                label = PlanApprovalChoice.Reject.label(denyLabel = request.denyLabel),
                fill = Danger.copy(alpha = 0.12f),
                ink = Danger,
                enabled = !submitting,
                modifier = Modifier.weight(1f),
            ) { onChoice(PlanApprovalChoice.Reject) }
            PlanChoiceButton(
                label = PlanApprovalChoice.Approve.label(confirmLabel = request.confirmLabel),
                fill = Color.Transparent,
                ink = Coral,
                border = BorderStroke(1.dp, Coral.copy(alpha = 0.6f)),
                enabled = !submitting,
                modifier = Modifier.weight(1f),
            ) { onChoice(PlanApprovalChoice.Approve) }
        }
        PlanChoiceButton(
            label = PlanApprovalChoice.ApproveAndRun.label(),
            fill = AccentFill,
            ink = OnAccent,
            enabled = !submitting,
            modifier = Modifier.fillMaxWidth(),
        ) { onChoice(PlanApprovalChoice.ApproveAndRun) }
    }
}

@Composable
private fun PlanChoiceButton(
    label: String,
    fill: Color,
    ink: Color,
    enabled: Boolean,
    modifier: Modifier = Modifier,
    border: BorderStroke? = null,
    onClick: () -> Unit,
) {
    Surface(
        color = fill,
        shape = RoundedCornerShape(8.dp),
        border = border,
        modifier = modifier.then(if (enabled) Modifier.clickable(onClick = onClick) else Modifier),
    ) {
        Text(
            label,
            color = ink,
            fontSize = 13.sp,
            fontWeight = FontWeight.SemiBold,
            textAlign = TextAlign.Center,
            modifier = Modifier.padding(vertical = 10.dp).fillMaxWidth(),
        )
    }
}
