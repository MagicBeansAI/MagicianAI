package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tutor.TutorShape
import android.app.Service
import android.content.Context
import android.content.Intent
import android.graphics.PixelFormat
import android.os.Build
import android.os.IBinder
import android.provider.Settings
import android.view.Gravity
import android.view.WindowManager
import androidx.compose.runtime.getValue
import androidx.compose.ui.platform.ComposeView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.LifecycleRegistry
import androidx.lifecycle.ViewModelStore
import androidx.lifecycle.ViewModelStoreOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.setViewTreeLifecycleOwner
import androidx.lifecycle.setViewTreeViewModelStoreOwner
import androidx.savedstate.SavedStateRegistry
import androidx.savedstate.SavedStateRegistryController
import androidx.savedstate.SavedStateRegistryOwner
import androidx.savedstate.setViewTreeSavedStateRegistryOwner
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * The tutor, drawn over whatever app you are actually using.
 *
 * This is the one place Android beats iOS outright rather than matching it.
 * iOS cannot draw over other apps, so its `screen_overlay` mode targets the
 * desktop gateway and teaches on a Mac. Android has `TYPE_APPLICATION_OVERLAY`,
 * so the lesson can happen on the phone, on top of the app being taught —
 * which is what somebody asking "how do I do this" was pointing at.
 *
 * The window is deliberately not touchable. A teaching overlay that swallows
 * taps would stop you doing the thing it is showing you, so touches pass
 * through to the app underneath and the overlay only draws.
 */
class TutorOverlayService : Service(), LifecycleOwner, ViewModelStoreOwner, SavedStateRegistryOwner {

    private val registry = LifecycleRegistry(this)
    private val savedState = SavedStateRegistryController.create(this)
    private var view: ComposeView? = null

    override val lifecycle: Lifecycle get() = registry
    override val viewModelStore = ViewModelStore()
    override val savedStateRegistry: SavedStateRegistry get() = savedState.savedStateRegistry

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        savedState.performRestore(null)
        registry.currentState = Lifecycle.State.CREATED
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopSelf()
            return START_NOT_STICKY
        }
        if (!canDraw(this)) {
            _problem.value = "Magician needs permission to draw over other apps."
            stopSelf()
            return START_NOT_STICKY
        }
        if (view == null) attach()
        return START_NOT_STICKY
    }

    private fun attach() {
        val manager = getSystemService(WindowManager::class.java)
        val params = WindowManager.LayoutParams(
            WindowManager.LayoutParams.MATCH_PARENT,
            WindowManager.LayoutParams.MATCH_PARENT,
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                WindowManager.LayoutParams.TYPE_APPLICATION_OVERLAY
            } else {
                @Suppress("DEPRECATION")
                WindowManager.LayoutParams.TYPE_PHONE
            },
            // Not focusable, not touchable: the lesson draws over the app and
            // the app keeps working. Taking focus would also dismiss the
            // keyboard of whatever is being taught.
            WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE or
                WindowManager.LayoutParams.FLAG_NOT_TOUCHABLE or
                WindowManager.LayoutParams.FLAG_LAYOUT_NO_LIMITS,
            PixelFormat.TRANSLUCENT,
        ).apply { gravity = Gravity.TOP or Gravity.START }

        val compose = ComposeView(this).apply {
            setViewTreeLifecycleOwner(this@TutorOverlayService)
            setViewTreeViewModelStoreOwner(this@TutorOverlayService)
            setViewTreeSavedStateRegistryOwner(this@TutorOverlayService)
            setContent {
                val shapes by storyboard.collectAsStateWithLifecycle()
                val progress = rememberStoryboardProgress(shapes)
                TutorCanvas(shapes = shapes, progress = progress)
            }
        }
        registry.currentState = Lifecycle.State.RESUMED
        runCatching { manager.addView(compose, params) }
            .onSuccess { view = compose }
            .onFailure {
                _problem.value = "The overlay window was refused: ${it.message}"
                stopSelf()
            }
    }

    override fun onDestroy() {
        view?.let { runCatching { getSystemService(WindowManager::class.java).removeView(it) } }
        view = null
        registry.currentState = Lifecycle.State.DESTROYED
        viewModelStore.clear()
        _storyboard.value = emptyList()
        super.onDestroy()
    }

    companion object {
        private const val ACTION_STOP = "ai.magicbeans.magdroid.TUTOR_OVERLAY_STOP"

        private val _storyboard = MutableStateFlow<List<TutorShape>>(emptyList())
        val storyboard: StateFlow<List<TutorShape>> = _storyboard.asStateFlow()

        private val _problem = MutableStateFlow<String?>(null)
        val problem: StateFlow<String?> = _problem.asStateFlow()

        /** Whether the overlay grant is in place. Checked before every start. */
        fun canDraw(context: Context): Boolean =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
                Settings.canDrawOverlays(context)
            } else {
                true
            }

        /** Send the owner to the system screen that grants it. */
        fun requestPermission(context: Context) {
            context.startActivity(
                Intent(
                    Settings.ACTION_MANAGE_OVERLAY_PERMISSION,
                    android.net.Uri.parse("package:${context.packageName}"),
                ).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
            )
        }

        /** Draw a storyboard over whatever is on screen. */
        fun show(context: Context, shapes: List<TutorShape>) {
            _problem.value = null
            _storyboard.value = shapes
            context.startService(Intent(context, TutorOverlayService::class.java))
        }

        fun hide(context: Context) {
            context.stopService(Intent(context, TutorOverlayService::class.java))
        }
    }
}
