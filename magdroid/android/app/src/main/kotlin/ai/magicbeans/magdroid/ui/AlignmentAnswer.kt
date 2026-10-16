package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.attention.AlignmentAnswers
import ai.magicbeans.magdroid.attention.AlignmentDetail
import ai.magicbeans.magdroid.attention.AlignmentDetailOutcome
import ai.magicbeans.magdroid.attention.AlignmentPrivateProjection
import ai.magicbeans.magdroid.attention.AlignmentProjection
import ai.magicbeans.magdroid.attention.alignmentDeadlineLine
import ai.magicbeans.magdroid.chat.HitlResponseValue
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * A preference alignment hold in the answer sheet — the Android port of iOS's
 * `AlignmentAskView` and the web's `AlignmentAskFields.svelte`.
 *
 * Android is an owner-private surface (2026-10-11), so the sheet reads why the
 * act was held and keeps three things visibly apart:
 * 1. what to do with **this act** (allow once, something else, cancel);
 * 2. what Magician **remembers** (change or remember a preference) — never the
 *    act's answer, never preselected;
 * 3. a **reply** in the owner's own words — an instruction, not consent.
 *
 * A lasting change is two steps: the first answer asks the server to read the
 * exact wording back; the second confirms that readback with its token. When
 * the private read fails, the act's own choices and a reply still work.
 */
@Composable
internal fun AlignmentAnswer(
    correlationId: String,
    alignment: AlignmentProjection,
    submitting: Boolean,
    loadDetail: suspend (String) -> AlignmentDetailOutcome,
    onSubmit: (HitlResponseValue) -> Unit,
) {
    var outcome by remember(alignment.askId) { mutableStateOf<AlignmentDetailOutcome?>(null) }
    var reply by remember(alignment.askId) { mutableStateOf("") }
    LaunchedEffect(correlationId, alignment.askId) { outcome = loadDetail(correlationId) }
    val detail = (outcome as? AlignmentDetailOutcome.Ready)?.detail

    Column(verticalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.testTag("alignment-answer")) {
        when (outcome) {
            null -> Note("Loading why this was held…")
            AlignmentDetailOutcome.Gone -> {
                Note("This review was already answered or replaced. Open the current one to continue.")
                return@Column
            }
            AlignmentDetailOutcome.Unavailable -> Note(AlignmentProjection.DETAIL_UNAVAILABLE_NOTE)
            is AlignmentDetailOutcome.Ready -> Unit
        }
        detail?.retryReason?.let { Text(it, color = Ink, fontSize = 14.sp) }
        if (detail != null && (detail.alignment.isWordingStage || alignment.isWordingStage)) {
            WordingStage(detail, submitting, onSubmit)
        } else {
            detail?.let { Reason(it.alignment) }
            if (detail == null && alignment.alsoCovers.isNotEmpty() && !alignment.isWordingStage) {
                Note(AlignmentProjection.ALSO_APPROVES_NOTE)
            }
            val actChoices = detail?.alignment?.actChoices?.map { it.id to it.label }
                ?: alignment.actChoices.map { it.id to it.label }
            actChoices.forEach { (id, label) ->
                ChoiceButton(
                    // "Cancel the action", so it never reads as closing the
                    // sheet, which leaves the hold waiting.
                    if (id == "cancel") "Cancel the action" else label,
                    enabled = !submitting,
                ) { onSubmit(AlignmentAnswers.choice(id)) }
            }
            detail?.alignment?.takeIf { it.lastingChoices.isNotEmpty() }?.let {
                LastingSection(it, submitting, onSubmit)
            }
        }
        if (alignment.freeText) {
            OutlinedTextField(
                value = reply,
                onValueChange = { reply = it },
                enabled = !submitting,
                label = { Text("Or reply with what to do instead", fontSize = 12.sp) },
                modifier = Modifier.fillMaxWidth(),
            )
            TextButton(
                onClick = { AlignmentAnswers.reply(reply)?.let(onSubmit) },
                enabled = !submitting && reply.isNotBlank(),
            ) {
                Text("Send reply", color = Coral, fontSize = 13.sp)
            }
        }
        alignmentDeadlineLine(detail?.alignment?.deadlineAtMs ?: alignment.deadlineAtMs)?.let { Note(it) }
    }
}

@Composable
private fun Note(text: String) {
    Text(text, color = Muted, fontSize = 12.sp)
}

@Composable
private fun Reason(alignment: AlignmentPrivateProjection) {
    alignment.explanation?.let { Text(it, color = Ink, fontSize = 15.sp) }
    alignment.actSummary?.let { Note("Held: $it") }
    if (alignment.alsoCovers.isNotEmpty()) {
        Note("It also needs your approval:")
        alignment.alsoCovers.forEach { Text("• ${it.line}", color = Ink, fontSize = 14.sp) }
        Note("Allowing it approves it too. Any other answer leaves it unapproved.")
    }
    alignment.intentions.forEach { intention ->
        Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            Text(intention.sourceLabel, color = Muted, fontSize = 11.sp, fontWeight = FontWeight.Medium)
            Text(intention.statement, color = Ink, fontSize = 14.sp)
        }
    }
}

@Composable
private fun LastingSection(
    alignment: AlignmentPrivateProjection,
    submitting: Boolean,
    onSubmit: (HitlResponseValue) -> Unit,
) {
    var changing by remember { mutableStateOf(false) }
    var wording by remember { mutableStateOf("") }
    Surface(
        color = Control.copy(alpha = 0.4f),
        shape = RoundedCornerShape(10.dp),
        border = BorderStroke(1.dp, ControlBorder),
        modifier = Modifier.fillMaxWidth().testTag("alignment-lasting"),
    ) {
        Column(Modifier.padding(10.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("Change what Magician remembers", color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
            alignment.lastingChoices.forEach { choice ->
                if (choice.needsWording) {
                    if (changing) {
                        OutlinedTextField(
                            value = wording,
                            onValueChange = { wording = it },
                            enabled = !submitting,
                            label = { Text("New wording", fontSize = 12.sp) },
                            modifier = Modifier.fillMaxWidth(),
                        )
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            TextButton(onClick = { changing = false }) { Text("Back", color = Muted) }
                            TextButton(
                                onClick = { onSubmit(HitlResponseValue.Choice(choice.id, wording.trim())) },
                                enabled = !submitting && wording.isNotBlank(),
                            ) { Text("Read it back", color = Coral) }
                        }
                    } else {
                        ChoiceButton(choice.label, enabled = !submitting) { changing = true }
                    }
                } else {
                    if (choice.id == "remember_preference") {
                        alignment.remember?.let {
                            Text("“${it.statement}”", color = Muted, fontSize = 13.sp, fontStyle = FontStyle.Italic)
                        }
                    }
                    ChoiceButton(choice.label, enabled = !submitting) { onSubmit(AlignmentAnswers.choice(choice.id)) }
                }
            }
        }
    }
}

@Composable
private fun WordingStage(detail: AlignmentDetail, submitting: Boolean, onSubmit: (HitlResponseValue) -> Unit) {
    detail.alignment.readback?.let { readback ->
        Text("Save exactly this as your preference?", color = Ink, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
        Text(readback.wording, color = Ink, fontSize = 15.sp, modifier = Modifier.padding(start = 8.dp))
        readback.superseded?.let { Note("Replaces: $it") }
        Note("This changes what Magician remembers. It does not run the held action; that is checked again afterwards.")
        ChoiceButton("Save this wording", enabled = !submitting) {
            onSubmit(HitlResponseValue.Choice("confirm_wording", readback.token))
        }
    }
    ChoiceButton("Don't change it", enabled = !submitting) { onSubmit(AlignmentAnswers.choice("keep_as_is")) }
    ChoiceButton("Cancel the action", enabled = !submitting) { onSubmit(AlignmentAnswers.choice("cancel")) }
}

@Composable
private fun ChoiceButton(label: String, enabled: Boolean, onClick: () -> Unit) {
    Surface(
        color = Control,
        shape = RoundedCornerShape(8.dp),
        border = BorderStroke(1.dp, ControlBorder),
        modifier = Modifier
            .fillMaxWidth()
            .then(if (enabled) Modifier.clickable(onClick = onClick) else Modifier),
    ) {
        Text(
            label,
            color = Ink,
            fontSize = 13.sp,
            fontWeight = FontWeight.SemiBold,
            textAlign = TextAlign.Center,
            modifier = Modifier.padding(vertical = 10.dp).fillMaxWidth(),
        )
    }
}
