package ai.magicbeans.magdroid.push

import ai.magicbeans.magdroid.chat.HitlResponseValue
import android.app.RemoteInput
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch

/**
 * Answers given on an attention notification.
 *
 * Two shapes reach here:
 * - the legacy Approve / Reject on the generic notification of a plain
 *   confirmation ask (the backend adds `actions=approve_reject`);
 * - [ACTION_ANSWER] from an answerable notification (see
 *   [AttentionAnswerNotifications]): a confirm / deny, one option of a choice,
 *   or a typed reply read from `RemoteInput`.
 *
 * Every answer posts to `/hitl/{correlation_id}/respond` over the device's
 * paired credential with `channel=android_push`, like an answer given in the
 * app, so every other surface clears. The notification then says what
 * happened: "Answer sent", "Already answered" (409 / 404 / `already_resolved`)
 * or "Couldn't send your answer" (tapping it opens the ask). Answer values are
 * never logged.
 */
class AttentionActionReceiver : BroadcastReceiver() {

    override fun onReceive(context: Context, intent: Intent) {
        val correlationID = intent.getStringExtra(EXTRA_CORRELATION_ID).orEmpty()
        val source = intent.getStringExtra(EXTRA_SOURCE).orEmpty()
        if (correlationID.isBlank() || source.isBlank()) return
        val deepLink = attentionDeepLink(intent.getStringExtra(EXTRA_DEEP_LINK), correlationID)
        val answer: HitlResponseValue?
        val inputType: String
        when (intent.action) {
            ACTION_APPROVE, ACTION_REJECT -> {
                answer = HitlResponseValue.Confirmation(intent.action == ACTION_APPROVE)
                inputType = "confirmation"
            }
            ACTION_ANSWER -> {
                answer = notificationAnswerValue(
                    kind = intent.getStringExtra(EXTRA_ANSWER),
                    confirmed = intent.getBooleanExtra(EXTRA_CONFIRMED, false),
                    optionId = intent.getStringExtra(EXTRA_OPTION_ID),
                    reply = RemoteInput.getResultsFromIntent(intent)?.getCharSequence(KEY_REPLY),
                )
                inputType = intent.getStringExtra(EXTRA_INPUT_TYPE).orEmpty()
            }
            else -> return
        }
        val app = context.applicationContext
        if (answer == null || inputType.isBlank()) {
            // An empty reply or a malformed action: the reply spinner still
            // needs the notification replaced, so offer the app instead.
            AttentionAnswerNotifications.postResult(app, correlationID, deepLink, NotificationAnswerOutcome.Failed)
            return
        }
        val executionId = intent.getStringExtra(EXTRA_EXECUTION_ID)
        val taskId = intent.getStringExtra(EXTRA_TASK_ID)
        val pending = goAsync()
        CoroutineScope(Dispatchers.IO).launch {
            try {
                val outcome = AttentionAnswerNotifications.send(
                    app, correlationID, source, inputType, answer, executionId, taskId,
                )
                if (outcome != NotificationAnswerOutcome.Failed) {
                    AttentionNotificationExpiry.forget(app, correlationID)
                }
                // TODO(plan_approval): the in-app flow follows an approved plan
                // with POST /v3/tasks/{id}/execute. Not mirrored here until the
                // server says whether `channel=android_push` approval starts
                // the run itself.
                AttentionAnswerNotifications.postResult(app, correlationID, deepLink, outcome)
            } finally {
                pending.finish()
            }
        }
    }

    companion object {
        const val ACTION_APPROVE = "ai.magicbeans.magdroid.action.ATTENTION_APPROVE"
        const val ACTION_REJECT = "ai.magicbeans.magdroid.action.ATTENTION_REJECT"
        const val ACTION_ANSWER = "ai.magicbeans.magdroid.action.ATTENTION_ANSWER"
        const val EXTRA_CORRELATION_ID = "correlation_id"
        const val EXTRA_SOURCE = "source"
        const val EXTRA_INPUT_TYPE = "input_type"
        const val EXTRA_EXECUTION_ID = "execution_id"
        const val EXTRA_TASK_ID = "task_id"
        const val EXTRA_DEEP_LINK = "deep_link"
        const val EXTRA_ANSWER = "answer"
        const val EXTRA_CONFIRMED = "confirmed"
        const val EXTRA_OPTION_ID = "option_id"
        const val KEY_REPLY = "attention_reply"
        const val ANSWER_CONFIRM = "confirm"
        const val ANSWER_CHOICE = "choice"
        const val ANSWER_TEXT = "text"
    }
}
