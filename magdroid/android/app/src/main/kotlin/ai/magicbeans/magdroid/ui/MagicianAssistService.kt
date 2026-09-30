package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tutor.ScreenCapture
import ai.magicbeans.magdroid.tutor.TutorSurface
import android.content.Intent
import android.graphics.Bitmap
import android.os.Bundle
import android.service.voice.VoiceInteractionService
import android.service.voice.VoiceInteractionSession
import android.service.voice.VoiceInteractionSessionService

/**
 * Magician as the phone's assistant.
 *
 * The assist gesture — long-press power, or the home swipe — is the system's
 * own "help me with what I am looking at". It is the closest Android has to
 * what Siri occupies on iOS, and it is the only entry that costs no tap and no
 * word: the screen you want explained is still in front of you when you make
 * it.
 *
 * Taking this role displaces whatever assistant the phone uses today, which is
 * why nothing here claims it. Android will not grant it silently — the owner
 * chooses Magician in system settings — and that is the right shape for a
 * decision about somebody's phone rather than about this app.
 */
class MagicianAssistService : VoiceInteractionService()

/** Hands the system a session when the assist gesture fires. */
class MagicianAssistSessionService : VoiceInteractionSessionService() {
    override fun onNewSession(args: Bundle?): VoiceInteractionSession = MagicianAssistSession(this)
}

/**
 * One assist invocation.
 *
 * The system supplies the screenshot rather than the app taking one, which is
 * the quiet advantage of this route: no accessibility capture, no shade to
 * collapse first, and no window of the wrong screen. What arrives is exactly
 * what was in front of the owner when they asked.
 */
class MagicianAssistSession(service: VoiceInteractionSessionService) :
    VoiceInteractionSession(service) {

    private var screenshot: Bitmap? = null

    override fun onPrepareShow(args: Bundle?, showFlags: Int) {
        // No UI of our own. The lesson is drawn by the overlay over the app
        // being asked about; a panel here would cover the very thing the
        // question is about.
        setUiEnabled(false)
    }

    override fun onHandleScreenshot(screenshot: Bitmap?) {
        this.screenshot = screenshot
    }

    /**
     * Start the lesson and get out of the way.
     *
     * `hide()` immediately, because the assist session is a window over the
     * screen: leaving it up would put a layer between the annotation and the
     * thing it points at.
     */
    override fun onHandleAssist(state: AssistState) {
        super.onHandleAssist(state)
        begin()
    }

    @Deprecated("Kept for platforms that still deliver the older callback.")
    override fun onHandleAssist(
        data: Bundle?,
        structure: android.app.assist.AssistStructure?,
        content: android.app.assist.AssistContent?,
    ) {
        begin()
    }

    private fun begin() {
        val frame = screenshot
        if (frame != null) {
            ScreenCapture.offer(frame)
        }
        // Without a frame the lesson still happens, on the board. Some devices
        // withhold the screenshot — a secure window, or a system that simply
        // does not send one — and refusing the question over it would punish
        // the asker for the platform.
        TutorShareInbox.hand(
            image = null,
            question = "Explain what is on my screen",
            surface = if (frame != null) TutorSurface.Overlay else TutorSurface.Blackboard,
        )
        context.startActivity(
            Intent(context, ChatActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
        )
        hide()
    }
}
