package ai.magicbeans.magdroid

import android.content.Intent
import ai.magicbeans.magdroid.animation.overrideClose
import android.os.Bundle
import androidx.appcompat.app.AppCompatActivity

/**
 * The launch screen: the mark, the name, and the build.
 *
 * The bridge animation that used to play here is gone. It held the owner for
 * three seconds to watch a drawing assemble itself, which is three seconds spent
 * on the app talking about itself rather than opening. The mark alone says the
 * same thing instantly.
 *
 * The version is shown because a screenshot of a bug should say which build it
 * came from without anyone having to ask.
 */
class SplashActivity : AppCompatActivity() {

    private var advanced = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_splash)

        // The build's own version, so a screenshot of a bug says which build it
        // came from without anyone having to ask.
        findViewById<android.widget.TextView>(R.id.splashVersion).text =
            "v${BuildConfig.VERSION_NAME} (${BuildConfig.VERSION_CODE})"

        // Short and fixed. There is no animation to wait for any more, and a
        // launch screen earns none of the owner's time beyond covering the
        // moment the app takes to be ready.
        window.decorView.postDelayed({ advance() }, SPLASH_MS)
    }

    /** Idempotent: the listener and the backstop can both fire. */
    private fun advance() {
        if (advanced) return
        advanced = true
        // Chat is what the app is for. Setup is reachable from there rather
        // than being the first thing an owner meets.
        startActivity(Intent(this, ai.magicbeans.magdroid.ui.ChatActivity::class.java))
        // No back-navigation into a splash screen.
        finish()
        // A hand-off, not a step forward: chat replaces the splash rather than
        // sitting on top of it, so this is the fade-through the app draws for
        // peers — the wave pair's slide would push a screen nobody navigated
        // away from. A close, because the splash is the one finishing.
        overrideClose(R.anim.fade_through_wave, android.R.anim.fade_out)
    }

    private companion object {
        const val SPLASH_MS = 900L
    }
}
