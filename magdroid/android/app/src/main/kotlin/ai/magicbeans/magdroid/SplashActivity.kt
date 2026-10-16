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
        // The system splash and this screen draw the same mark
        // (@drawable/splash_icon); at the hand-off this screen takes the
        // splash icon's exact place and the splash goes at once, instead of
        // cross-fading two copies: only the words below fade in.
        setContentView(R.layout.activity_splash)
        if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.S) {
            splashScreen.setOnExitAnimationListener { splash ->
                // The system sizes and places its icon its own way (larger than
                // the drawable, centred on the whole screen), so put this mark
                // on the icon's exact screen bounds before the splash goes.
                splash.iconView?.let { icon -> alignMarkTo(icon) }
                findViewById<android.view.View>(R.id.splashMark).post { splash.remove() }
            }
        }

        // The build's own version, so a screenshot of a bug says which build it
        // came from without anyone having to ask.
        findViewById<android.widget.TextView>(R.id.splashVersion).text =
            "v${BuildConfig.VERSION_NAME} (${BuildConfig.VERSION_CODE})"

        // The mark is already where the system splash drew it; only the words
        // are new, so only they arrive.
        findViewById<android.view.View>(R.id.splashText).apply {
            alpha = 0f
            animate().alpha(1f).setDuration(220L).start()
        }

        // Short and fixed. There is no animation to wait for any more, and a
        // launch screen earns none of the owner's time beyond covering the
        // moment the app takes to be ready.
        window.decorView.postDelayed({ advance() }, SPLASH_MS)
    }

    /**
     * Put the mark's coloured square exactly on [icon]'s screen bounds. The
     * system scales the splash drawable so its square fills its icon view;
     * the same drawable is drawn here through a matrix (the scaled canvas can
     * be wider than the screen, which a sized view would squeeze). The words
     * sit just under the square.
     */
    private fun alignMarkTo(icon: android.view.View) {
        if (icon.width == 0 || icon.height == 0) return
        val root = findViewById<android.view.ViewGroup>(android.R.id.content)
        val mark = findViewById<android.widget.ImageView>(R.id.splashMark)
        val text = findViewById<android.view.View>(R.id.splashText)
        val drawable = mark.drawable ?: return
        val iconAt = IntArray(2).also { icon.getLocationOnScreen(it) }
        val rootAt = IntArray(2).also { root.getLocationOnScreen(it) }
        val cx = iconAt[0] - rootAt[0] + icon.width / 2f
        val cy = iconAt[1] - rootAt[1] + icon.height / 2f
        // The drawable's square is 132 of its 288-unit canvas.
        val canvas = drawable.intrinsicWidth.toFloat()
        val scale = icon.width / (canvas * 132f / 288f)
        mark.scaleType = android.widget.ImageView.ScaleType.MATRIX
        mark.imageMatrix = android.graphics.Matrix().apply {
            setScale(scale, scale)
            postTranslate(cx - canvas * scale / 2f, cy - canvas * scale / 2f)
        }
        (mark.layoutParams as android.widget.RelativeLayout.LayoutParams).apply {
            width = android.view.ViewGroup.LayoutParams.MATCH_PARENT
            height = android.view.ViewGroup.LayoutParams.MATCH_PARENT
            removeRule(android.widget.RelativeLayout.CENTER_IN_PARENT)
        }.also { mark.layoutParams = it }
        val gap = (14 * resources.displayMetrics.density).toInt()
        (text.layoutParams as android.widget.RelativeLayout.LayoutParams).apply {
            removeRule(android.widget.RelativeLayout.BELOW)
            addRule(android.widget.RelativeLayout.ALIGN_PARENT_TOP)
            topMargin = (cy + icon.height / 2f).toInt() + gap
        }.also { text.layoutParams = it }
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
