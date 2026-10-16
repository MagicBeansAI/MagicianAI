package ai.magicbeans.magdroid.push

import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.attention.AttentionItem
import ai.magicbeans.magdroid.attention.MCP_OAUTH_SOURCE
import ai.magicbeans.magdroid.attention.attentionJson
import ai.magicbeans.magdroid.attention.isActionable
import ai.magicbeans.magdroid.attention.openTaskId
import ai.magicbeans.magdroid.attention.request
import ai.magicbeans.magdroid.bridge.R as BridgeR
import ai.magicbeans.magdroid.chat.HitlResponseValue
import android.app.Notification
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.RemoteInput
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import java.net.HttpURLConnection
import java.net.URL
import java.net.URLEncoder
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/*
 * Answering an ask from its notification.
 *
 * The push carries no question (`answer_mode` only says which shape of answer
 * the server thinks the ask takes). After the generic "needs your input"
 * notification is up, the service fetches the ask over the paired credential
 * and, when this client agrees the ask is safe to answer in one tap, re-posts
 * the same tag with the question and its answer actions. Anything that does
 * not fit — a fetch failure, a sensitive or destructive ask, more options than
 * Android has action slots — leaves the generic notification, which opens the
 * ask in the app.
 */

/** The `channel` answers from a notification post as (server-side push channel). */
internal const val ANDROID_PUSH_CHANNEL = "android_push"

/** The answer modes a push may name. Absent or unknown means open-only. */
internal const val ANSWER_MODE_CONFIRM = "confirm"
internal const val ANSWER_MODE_CHOICE = "choice"
internal const val ANSWER_MODE_TEXT = "text"

/** Android shows at most three actions on a notification. */
internal const val MAX_NOTIFICATION_ACTIONS = 3

/** One answer button: what it says and what it posts. */
internal data class NotificationAnswerButton(
    val key: String,
    val label: String,
    val value: HitlResponseValue,
)

/** Everything needed to draw an answerable ask notification and post its answer. */
internal data class NotificationAnswerPlan(
    val mode: String,
    val question: String,
    val hint: String?,
    val source: String,
    val inputType: String,
    val executionId: String?,
    val taskId: String?,
    /** Confirm / choice buttons; empty for a text reply. */
    val buttons: List<NotificationAnswerButton>,
    /** The reply field's hint, for a text ask only. */
    val replyLabel: String?,
)

/**
 * Whether the fetched ask can be answered from its notification, and how.
 *
 * The push's `answer_mode` is a hint; the fetched ask decides. Both must agree,
 * and the ask must be:
 * - confirm: a plain confirmation, not destructive, not a secret, not a grant;
 * - choice: one pick among 1–3 options, none needing typed detail, not a secret;
 * - text: a plain typed answer — not a password / OTP, not a multi-question form.
 * A destructive ask is never offered as a one-tap answer in any mode.
 */
internal fun notificationAnswerPlan(
    answerMode: String?,
    pushSource: String?,
    item: AttentionItem,
): NotificationAnswerPlan? {
    val mode = answerMode?.trim()?.lowercase() ?: return null
    if (!item.isActionable) return null
    val metadata = item.metadata
    val request = item.request(metadata)
    val schema = metadata?.inputSchema ?: metadata?.hitlRequest?.inputSchema
    val question = request.prompt.trim()
    if (question.isEmpty()) return null
    if (request.isSensitive || request.destructive || schema?.destructive == true) return null
    if (request.grantKind != null || request.source == MCP_OAUTH_SOURCE) return null
    val buttons: List<NotificationAnswerButton>
    var replyLabel: String? = null
    when (mode) {
        ANSWER_MODE_CONFIRM -> {
            if (request.inputType != "confirmation") return null
            buttons = listOf(
                NotificationAnswerButton("confirm", request.confirmLabel, HitlResponseValue.Confirmation(true)),
                NotificationAnswerButton("deny", request.denyLabel, HitlResponseValue.Confirmation(false)),
            )
        }
        ANSWER_MODE_CHOICE -> {
            if (request.inputType != "choice" || schema?.multiple == true) return null
            val options = request.options
            if (options.isEmpty() || options.size > MAX_NOTIFICATION_ACTIONS) return null
            if (options.any { it.id.isBlank() || it.requiresInput == true }) return null
            buttons = options.map { option ->
                NotificationAnswerButton(
                    key = "opt:${option.id}",
                    label = option.label.trim().ifEmpty { option.id },
                    value = HitlResponseValue.Choice(option.id),
                )
            }
        }
        ANSWER_MODE_TEXT -> {
            if (request.inputType != "text" || request.renderKind != "text") return null
            if (request.formQuestions.isNotEmpty() || !schema?.questions.isNullOrEmpty()) return null
            buttons = emptyList()
            replyLabel = request.placeholder.trim().ifEmpty { "Your answer" }
        }
        else -> return null
    }
    return NotificationAnswerPlan(
        mode = mode,
        question = question,
        hint = request.hint?.trim()?.takeIf { it.isNotEmpty() && it != question },
        source = pushSource?.trim()?.takeIf { it.isNotEmpty() } ?: request.source,
        inputType = request.inputType,
        executionId = request.executionId,
        taskId = item.openTaskId,
        buttons = buttons,
        replyLabel = replyLabel,
    )
}

/** What an answer the owner gave from a notification posts. */
internal fun notificationAnswerValue(
    kind: String?,
    confirmed: Boolean,
    optionId: String?,
    reply: CharSequence?,
): HitlResponseValue? = when (kind) {
    AttentionActionReceiver.ANSWER_CONFIRM -> HitlResponseValue.Confirmation(confirmed)
    AttentionActionReceiver.ANSWER_CHOICE ->
        optionId?.takeIf { it.isNotBlank() }?.let { HitlResponseValue.Choice(it) }
    AttentionActionReceiver.ANSWER_TEXT ->
        reply?.toString()?.trim()?.takeIf { it.isNotEmpty() }?.let { HitlResponseValue.Text(it) }
    else -> null
}

/** The `/hitl/{correlation_id}/respond` body for an answer given from a notification. */
internal fun notificationAnswerBody(
    correlationID: String,
    source: String,
    inputType: String,
    value: HitlResponseValue,
    executionId: String?,
    taskId: String?,
): String = buildJsonObject {
    put("source", source)
    put("input_type", inputType)
    put("value", value.toJson())
    put("correlation_id", correlationID)
    put("channel", ANDROID_PUSH_CHANNEL)
    executionId?.takeIf { it.isNotBlank() }?.let { put("execution_id", it) }
    taskId?.takeIf { it.isNotBlank() }?.let { put("task_id", it) }
}.toString()

internal enum class NotificationAnswerOutcome { Sent, AlreadyAnswered, Failed }

/**
 * Read the respond call's result. 404 and 409 mean the ask was settled
 * elsewhere; a 2xx that says `accepted:false` is a refusal, and
 * `already_resolved` among those is the same "already answered".
 */
internal fun notificationAnswerOutcome(status: Int, body: String?): NotificationAnswerOutcome {
    if (status == 404 || status == 409) return NotificationAnswerOutcome.AlreadyAnswered
    if (status !in 200..299) return NotificationAnswerOutcome.Failed
    val json = body?.takeIf { it.isNotBlank() }
        ?.let { runCatching { attentionJson.parseToJsonElement(it) as? JsonObject }.getOrNull() }
        ?: return NotificationAnswerOutcome.Sent
    val accepted = (json["accepted"] as? JsonPrimitive)?.booleanOrNull
    if (accepted != false) return NotificationAnswerOutcome.Sent
    val reason = (json["reason"] as? JsonPrimitive)?.takeIf { it.isString }?.content
    return if (reason == "already_resolved") {
        NotificationAnswerOutcome.AlreadyAnswered
    } else {
        NotificationAnswerOutcome.Failed
    }
}

/**
 * Lock-screen visibility of a notification that shows the question.
 *
 * `VISIBILITY_PRIVATE` + a public version is not enough: SystemUI redacts a
 * private notification only when the owner's system setting hides sensitive
 * content (`NotificationLockscreenUserManagerImpl.needsRedaction`), and apps
 * cannot set a channel-level override (`PreferencesHelper` resets a channel's
 * lockscreen visibility). `VISIBILITY_SECRET` is hidden whenever the keyguard
 * is showing, whatever that setting says, so it is the only way to keep the
 * question off the lock screen by default.
 */
internal fun answerableNotificationVisibility(showQuestionsOnLockScreen: Boolean): Int =
    if (showQuestionsOnLockScreen) Notification.VISIBILITY_PUBLIC else Notification.VISIBILITY_SECRET

/** The deep link a notification for this ask opens. */
internal fun attentionDeepLink(pushDeepLink: String?, correlationID: String): String =
    pushDeepLink?.trim()?.takeIf { it.startsWith("magican://") }
        ?: if (correlationID.isBlank()) {
            "magican://attention"
        } else {
            "magican://attention?correlation_id=${URLEncoder.encode(correlationID, "UTF-8").replace("+", "%20")}"
        }

/** The owner's "Show questions on lock screen" choice; off by default. */
object AttentionLockScreenPrivacy {
    private const val PREFERENCES = "magican_attention_notifications_v1"
    private const val SHOW_QUESTIONS = "show_questions_on_lock_screen"

    fun showQuestions(context: Context): Boolean =
        context.applicationContext.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
            .getBoolean(SHOW_QUESTIONS, false)

    fun setShowQuestions(context: Context, show: Boolean) {
        context.applicationContext.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
            .edit().putBoolean(SHOW_QUESTIONS, show).apply()
    }
}

internal object AttentionAnswerNotifications {
    private const val FETCH_CONNECT_TIMEOUT_MS = 3_000
    private const val FETCH_READ_TIMEOUT_MS = 4_000
    private const val RESULT_TIMEOUT_MS = 5_000L
    private val MODES = setOf(ANSWER_MODE_CONFIRM, ANSWER_MODE_CHOICE, ANSWER_MODE_TEXT)

    /**
     * Turn the generic notification for this ask into an answerable one.
     *
     * Runs on the FCM service's worker thread, after the generic notification
     * is posted, within FCM's ~10 s handling window (short timeouts below).
     * Every failure leaves the generic notification as it is.
     */
    fun enrich(context: Context, data: Map<String, String>) {
        val mode = data["answer_mode"]?.trim()?.lowercase()?.takeIf { it in MODES } ?: return
        val correlationID = data["correlation_id"].orEmpty().trim()
        if (correlationID.isEmpty()) return
        val item = runCatching { fetch(context, correlationID) }.getOrNull() ?: return
        val plan = notificationAnswerPlan(mode, data["source"], item) ?: return
        val remaining = attentionTimeoutMillis(data["expires_at"], System.currentTimeMillis())
        if (remaining == 0L) return
        val manager = context.getSystemService(NotificationManager::class.java) ?: return
        // A resolve that landed while the fetch ran already took the generic
        // notification down; do not bring a settled ask back.
        val stillShowing = runCatching {
            manager.activeNotifications.any {
                it.tag == correlationID && it.id == MagicanMessagingService.ATTENTION_NOTIFICATION_ID
            }
        }.getOrDefault(true)
        if (!stillShowing) return
        val deepLink = attentionDeepLink(data["deep_link"], correlationID)
        val showQuestions = AttentionLockScreenPrivacy.showQuestions(context)
        val builder = builder(context)
            .setSmallIcon(BridgeR.drawable.ic_notification)
            .setContentTitle(plan.question)
            .setContentIntent(openIntent(context, correlationID, deepLink))
            .setAutoCancel(true)
            // The generic version already alerted; the richer one is silent.
            .setOnlyAlertOnce(true)
            .setVisibility(answerableNotificationVisibility(showQuestions))
            .setPublicVersion(genericPublicVersion(context))
        val hint = plan.hint
        if (hint != null) {
            builder.setContentText(hint)
            builder.setStyle(Notification.BigTextStyle().setBigContentTitle(plan.question).bigText(hint))
        } else {
            builder.setContentText("Answer here, or tap to open it in Magican.")
            builder.setStyle(Notification.BigTextStyle().bigText(plan.question))
        }
        if (remaining != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            builder.setTimeoutAfter(remaining)
        }
        val base = baseAnswerIntent(context, correlationID, plan, deepLink)
        if (plan.mode == ANSWER_MODE_TEXT) {
            builder.addAction(replyAction(context, correlationID, base, plan.replyLabel ?: "Your answer"))
        } else {
            plan.buttons.take(MAX_NOTIFICATION_ACTIONS).forEachIndexed { index, button ->
                builder.addAction(buttonAction(context, correlationID, base, button, index))
            }
        }
        manager.notify(correlationID, MagicanMessagingService.ATTENTION_NOTIFICATION_ID, builder.build())
    }

    /** Replace an ask's notification with the result of answering it. */
    fun postResult(
        context: Context,
        correlationID: String,
        deepLink: String,
        outcome: NotificationAnswerOutcome,
    ) {
        val manager = context.getSystemService(NotificationManager::class.java) ?: return
        val (title, text) = when (outcome) {
            NotificationAnswerOutcome.Sent -> "Answer sent" to "Magican is carrying on."
            NotificationAnswerOutcome.AlreadyAnswered -> "Already answered" to "This was settled elsewhere."
            NotificationAnswerOutcome.Failed -> "Couldn't send your answer" to "Tap to answer it in Magican."
        }
        val builder = builder(context)
            .setSmallIcon(BridgeR.drawable.ic_notification)
            .setContentTitle(title)
            .setContentText(text)
            .setContentIntent(openIntent(context, correlationID, deepLink))
            .setAutoCancel(true)
            .setOnlyAlertOnce(true)
        if (outcome != NotificationAnswerOutcome.Failed) {
            // A settled ask's notice is brief. Below API 26 the expiry sweep
            // takes it down instead.
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                builder.setTimeoutAfter(RESULT_TIMEOUT_MS)
            }
            AttentionNotificationExpiry.remember(context, correlationID, System.currentTimeMillis() + RESULT_TIMEOUT_MS)
        }
        manager.notify(correlationID, MagicanMessagingService.ATTENTION_NOTIFICATION_ID, builder.build())
    }

    /** POST an answer; never logs or rethrows. */
    fun send(
        context: Context,
        correlationID: String,
        source: String,
        inputType: String,
        value: HitlResponseValue,
        executionId: String?,
        taskId: String?,
    ): NotificationAnswerOutcome {
        val host = MagicianAccess.baseUrl(context).trimEnd('/')
        if (host.isEmpty()) return NotificationAnswerOutcome.Failed
        return runCatching {
            val connection = URL("$host/api/magician/v2/hitl/${encode(correlationID)}/respond")
                .openConnection() as HttpURLConnection
            try {
                connection.requestMethod = "POST"
                connection.connectTimeout = 10_000
                connection.readTimeout = 20_000
                connection.doOutput = true
                connection.setRequestProperty("Content-Type", "application/json")
                MagicianAccess.headers(context).forEach { (name, header) -> connection.setRequestProperty(name, header) }
                val body = notificationAnswerBody(correlationID, source, inputType, value, executionId, taskId)
                connection.outputStream.use { it.write(body.toByteArray()) }
                val status = connection.responseCode
                val response = runCatching {
                    (if (status in 200..299) connection.inputStream else connection.errorStream)
                        ?.bufferedReader()?.use { it.readText() }
                }.getOrNull()
                notificationAnswerOutcome(status, response)
            } finally {
                connection.disconnect()
            }
        }.getOrDefault(NotificationAnswerOutcome.Failed)
    }

    private fun fetch(context: Context, correlationID: String): AttentionItem? {
        val host = MagicianAccess.baseUrl(context).trimEnd('/')
        if (host.isEmpty()) return null
        val connection = URL("$host/api/magician/v2/feed/attention/${encode(correlationID)}")
            .openConnection() as HttpURLConnection
        try {
            connection.requestMethod = "GET"
            connection.connectTimeout = FETCH_CONNECT_TIMEOUT_MS
            connection.readTimeout = FETCH_READ_TIMEOUT_MS
            MagicianAccess.headers(context).forEach { (name, header) -> connection.setRequestProperty(name, header) }
            if (connection.responseCode !in 200..299) return null
            val body = connection.inputStream.bufferedReader().use { it.readText() }
            return decodeAttentionItem(body)
        } finally {
            connection.disconnect()
        }
    }

    private fun builder(context: Context): Notification.Builder =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(context, MagicanMessagingService.ATTENTION_CHANNEL_ID)
        } else {
            @Suppress("DEPRECATION") Notification.Builder(context)
        }

    /** What a locked screen may show of an ask: that one is waiting, nothing more. */
    private fun genericPublicVersion(context: Context): Notification =
        builder(context)
            .setSmallIcon(BridgeR.drawable.ic_notification)
            .setContentTitle("Magican needs your input")
            .setContentText("Unlock to answer.")
            .build()

    private fun openIntent(context: Context, correlationID: String, deepLink: String): PendingIntent =
        PendingIntent.getActivity(
            context,
            7710 + correlationID.hashCode(),
            Intent(Intent.ACTION_VIEW, Uri.parse(deepLink))
                .setPackage(context.packageName)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )

    private fun baseAnswerIntent(
        context: Context,
        correlationID: String,
        plan: NotificationAnswerPlan,
        deepLink: String,
    ): Intent = Intent(context, AttentionActionReceiver::class.java)
        .setAction(AttentionActionReceiver.ACTION_ANSWER)
        .putExtra(AttentionActionReceiver.EXTRA_CORRELATION_ID, correlationID)
        .putExtra(AttentionActionReceiver.EXTRA_SOURCE, plan.source)
        .putExtra(AttentionActionReceiver.EXTRA_INPUT_TYPE, plan.inputType)
        .putExtra(AttentionActionReceiver.EXTRA_EXECUTION_ID, plan.executionId)
        .putExtra(AttentionActionReceiver.EXTRA_TASK_ID, plan.taskId)
        .putExtra(AttentionActionReceiver.EXTRA_DEEP_LINK, deepLink)

    private fun buttonAction(
        context: Context,
        correlationID: String,
        base: Intent,
        button: NotificationAnswerButton,
        index: Int,
    ): Notification.Action {
        val intent = Intent(base)
        when (val value = button.value) {
            is HitlResponseValue.Confirmation -> intent
                .putExtra(AttentionActionReceiver.EXTRA_ANSWER, AttentionActionReceiver.ANSWER_CONFIRM)
                .putExtra(AttentionActionReceiver.EXTRA_CONFIRMED, value.confirmed)
            is HitlResponseValue.Choice -> intent
                .putExtra(AttentionActionReceiver.EXTRA_ANSWER, AttentionActionReceiver.ANSWER_CHOICE)
                .putExtra(AttentionActionReceiver.EXTRA_OPTION_ID, value.selectedId)
            else -> Unit
        }
        val pending = PendingIntent.getBroadcast(
            context,
            answerRequestCode(correlationID, button.key, index),
            intent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        return Notification.Action.Builder(null as android.graphics.drawable.Icon?, button.label, pending)
            .requireUnlock()
            .build()
    }

    private fun replyAction(
        context: Context,
        correlationID: String,
        base: Intent,
        label: String,
    ): Notification.Action {
        val intent = Intent(base)
            .putExtra(AttentionActionReceiver.EXTRA_ANSWER, AttentionActionReceiver.ANSWER_TEXT)
        // RemoteInput fills the reply into this intent, so it alone is mutable.
        // The intent is explicit (the receiver's class), as API 34 requires.
        val mutable = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) PendingIntent.FLAG_MUTABLE else 0
        val pending = PendingIntent.getBroadcast(
            context,
            answerRequestCode(correlationID, "reply", 0),
            intent,
            mutable or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val input = RemoteInput.Builder(AttentionActionReceiver.KEY_REPLY).setLabel(label).build()
        val builder = Notification.Action.Builder(null as android.graphics.drawable.Icon?, "Reply", pending)
            .addRemoteInput(input)
            .setAllowGeneratedReplies(false)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            builder.setSemanticAction(Notification.Action.SEMANTIC_ACTION_REPLY)
        }
        return builder.requireUnlock().build()
    }

    /** Answering from the lock screen requires unlocking the device (API 31+). */
    private fun Notification.Action.Builder.requireUnlock(): Notification.Action.Builder {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) setAuthenticationRequired(true)
        return this
    }

    private fun encode(value: String): String = URLEncoder.encode(value, "UTF-8").replace("+", "%20")
}

/** A distinct PendingIntent request code per ask and per action. */
internal fun answerRequestCode(correlationID: String, actionKey: String, index: Int): Int =
    "$correlationID|answer|$index|$actionKey".hashCode()

/** The single-ask read's body: the feed item itself, or one wrapped as `item`. */
internal fun decodeAttentionItem(body: String): AttentionItem? = runCatching {
    val root = attentionJson.parseToJsonElement(body) as? JsonObject
    val element = (root?.get("item") as? JsonObject) ?: root
    element?.let { attentionJson.decodeFromJsonElement(AttentionItem.serializer(), it) }
}.getOrNull()
