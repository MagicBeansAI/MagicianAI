package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tutor.TutorScreenGrab
import ai.magicbeans.magdroid.tutor.TutorSurface
import android.app.PendingIntent
import android.content.Intent
import android.os.Build
import android.service.quicksettings.Tile
import android.service.quicksettings.TileService
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * "Teach me this screen", from the shade of any app.
 *
 * The entry point has to live outside Magician, because by the time you have
 * opened the app the screen you wanted explained is gone — a button in the
 * composer would capture Magician's own screen and explain that. A tile is
 * reachable from inside whatever you are actually looking at, which is the
 * whole requirement.
 *
 * Closest thing Android has to the place iOS puts this. iOS uses Siri and the
 * share sheet; Android's shade is the equivalent always-available surface, and
 * the wake word covers the hands-free case that Siri covers there.
 */
class TeachScreenTile : TileService() {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    override fun onStartListening() {
        super.onStartListening()
        // Unavailable rather than merely failing later: a tile that looks live
        // and then does nothing is worse than one that shows it needs setup.
        qsTile?.state = if (TutorScreenGrab.available()) Tile.STATE_INACTIVE else Tile.STATE_UNAVAILABLE
        qsTile?.updateTile()
    }

    override fun onClick() {
        super.onClick()
        if (!TutorScreenGrab.available()) return

        // Collapse the shade before capturing. The shade is on screen at the
        // moment of the tap, so capturing now photographs the shade rather than
        // the app underneath — the specific bug this ordering exists to avoid.
        collapseShade()

        scope.launch {
            // Long enough for the collapse animation to finish. Hooking the
            // animation would be exact, and there is no callback for it; this
            // is the honest approximation and it is why the delay is not
            // shorter.
            delay(SHADE_COLLAPSE_MS)
            val captured = TutorScreenGrab.grab()

            // Handed through the same inbox the share sheet uses, so the app
            // has one way in rather than two. No picture when the capture
            // failed: the board then teaches without one.
            TutorShareInbox.hand(
                image = null,
                question = "Explain what is on my screen",
                surface = if (captured) TutorSurface.Overlay else TutorSurface.Blackboard,
            )
            // API 34 takes a PendingIntent rather than an Intent: a tile now
            // hands the system something to fire on its behalf instead of
            // launching directly. Both branches open the same activity.
            val target = Intent(this@TeachScreenTile, ChatActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
                startActivityAndCollapse(
                    PendingIntent.getActivity(
                        this@TeachScreenTile,
                        0,
                        target,
                        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
                    ),
                )
            } else {
                @Suppress("DEPRECATION")
                startActivityAndCollapse(target)
            }
        }
    }

    @Suppress("DEPRECATION")
    private fun collapseShade() {
        // `startActivityAndCollapse` is the sanctioned way, but it also brings
        // the app forward, which would replace the screen being asked about.
        // The capture has to happen while that screen is still there.
        sendBroadcast(Intent("android.intent.action.CLOSE_SYSTEM_DIALOGS"))
    }

    private companion object {
        const val SHADE_COLLAPSE_MS = 400L
    }
}
