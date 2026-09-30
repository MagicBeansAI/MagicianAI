package ai.magicbeans.magdroid.voice

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.content.pm.PackageManager
import android.os.Build
import android.os.IBinder
import android.util.Log
import androidx.core.content.ContextCompat
import ai.magicbeans.magdroid.identity.ProductIdentity
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.launch
import kotlinx.coroutines.flow.dropWhile
import kotlinx.coroutines.flow.first
import org.vosk.Model
import org.vosk.Recognizer
import org.vosk.android.RecognitionListener
import org.vosk.android.SpeechService
import org.vosk.android.StorageService

/**
 * Always-on wake-word listening.
 *
 * This is the thing Android can do and iOS cannot. iOS has no sanctioned
 * persistent background microphone, so its wake spotter only runs while the app
 * is in front of you — which means the phone in your pocket is deaf. A
 * foreground service with the microphone type runs with the screen off and the
 * device locked, for as long as it is allowed to.
 *
 * The cost of that power is honesty about it: the service is only startable
 * from a visible screen, holds a permanent notification the whole time it
 * listens, and the notification's only action is to stop. A microphone running
 * in the background that is hard to turn off is a microphone nobody should
 * trust.
 *
 * Nothing leaves the device. Vosk decodes locally against a grammar of the
 * phrases, so ordinary conversation is decoded to `[unk]` and discarded rather
 * than uploaded.
 */
class WakeService : Service() {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private var speech: SpeechService? = null
    private var model: Model? = null

    /** The turn a wake starts. Built here so it outlives any screen. */
    private val turnHolder = lazy {
        WakeTurn(applicationContext, Dictation(applicationContext), Speech(applicationContext))
    }
    private val turn: WakeTurn get() = turnHolder.value
    private val realtimeHolder = lazy { RealtimeVoiceSession(applicationContext) }
    private val realtime: RealtimeVoiceSession get() = realtimeHolder.value

    /** True while dictation holds the microphone and Vosk must not. */
    private var capturing = false

    /** The exact identity-derived grammar currently installed in Vosk. */
    @Volatile private var activePhrases: List<String> = emptyList()

    /** The window this service is held to, and the ticker enforcing it. */
    @Volatile private var arm: AmbientArm? = null
    private var leash: Job? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopSelf()
            return START_NOT_STICKY
        }
        if (intent?.action == ACTION_EXTEND) {
            extendWindow()
            return START_STICKY
        }
        PrimaryAgentWakeIdentityStore.initialize(this)
        activePhrases = phrases.value
        if (activePhrases.isEmpty()) {
            _problem.value = "${ProductIdentity.productName} hasn't loaded the primary assistant name yet. Check the self-hosted connection and try again."
            stopSelf()
            return START_NOT_STICKY
        }
        // The battery rails, before the microphone is opened rather than after.
        // Refusing here costs nothing; refusing once a foreground service is up
        // has already told the owner it started.
        val admission = AmbientPowerMonitor.admit(AmbientPowerMonitor.read(this))
        admission.refusal?.let { block ->
            _problem.value = block.message
            _powerWarning.value = null
            stopSelf()
            return START_NOT_STICKY
        }
        // Carried for the whole window, not just its first moment: a window
        // armed while Battery Saver is already on hears from no change
        // notification, so without this it would run its leash silently.
        _powerWarning.value = admission.warning

        // Armed before the notification is first built, so its very first paint
        // already carries the countdown. Restored rather than reset when a
        // window survived a process death — re-arming there would silently hand
        // back the full thirty minutes the owner had already spent.
        arm = AmbientArmStore.load(this)?.takeUnless { it.hasExpired(System.currentTimeMillis()) }
        if (arm == null) armWindow() else armWindow(restore = true)

        // The type is mandatory from Android 14: a microphone service that
        // starts without declaring itself one is killed with an exception
        // rather than merely warned about.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(
                NOTIFICATION_ID,
                notification(),
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE,
            )
        } else {
            startForeground(NOTIFICATION_ID, notification())
        }
        if (speech == null) begin()
        // Not sticky: a restart the owner did not ask for would bring the
        // microphone back up silently, which is exactly what must not happen.
        return START_NOT_STICKY
    }

    /**
     * Stop if the battery has fallen through the floor while listening.
     *
     * Only the floor closes a live window. Battery Saver switching on mid-window
     * becomes a warning instead — interrupting somebody mid-sentence to report a
     * setting is worse than the battery it would save.
     */
    private fun checkPowerWhileListening() {
        val readings = AmbientPowerMonitor.read(this)
        AmbientPowerMonitor.mustClose(readings)?.let { block ->
            _problem.value = block.message
            stopSelf()
            return
        }
        _powerWarning.value = AmbientPowerMonitor.admit(readings).warning
    }

    private fun begin() {
        _listening.value = true
        // A primary-agent rename or alias change must take effect without
        // making the user disarm and re-arm the microphone. Rebuild only while
        // Vosk owns the microphone; resumeListening() naturally installs the
        // latest grammar when a voice turn currently owns it.
        scope.launch(Dispatchers.Main.immediate) {
            phrases.collect { refreshed ->
                if (refreshed.isEmpty() || refreshed == activePhrases) return@collect
                activePhrases = refreshed
                if (!capturing) model?.let(::rebuildDecoder)
                getSystemService(NotificationManager::class.java)
                    .notify(NOTIFICATION_ID, notification())
            }
        }
        scope.launch {
            // The model is unpacked from assets once and reused. It is the
            // expensive object; the recogniser around it is cheap.
            StorageService.unpack(
                this@WakeService, MODEL_ASSET, MODEL_DIR,
                { unpacked ->
                    model = unpacked
                    startDecoding(unpacked)
                },
                { error ->
                    ai.magicbeans.magdroid.bridge.BridgeLog.error(TAG, "wake-word model could not be unpacked: ${error.message}")
                    _listening.value = false
                    _problem.value = "Could not load the wake-word model: ${error.message}"
                    stopSelf()
                },
            )
        }
    }

    /**
     * Stop listening when the leash runs out.
     *
     * A microphone that stays open because nobody remembered to close it is the
     * failure this guards. The deadline is absolute, not idle-based: "I stopped
     * talking to it" is not the same as "it stopped listening".
     */
    private fun startDecoding(loaded: Model) {
        runCatching {
            val recognizer = Recognizer(loaded, SAMPLE_RATE, WakeSpotter.grammar(activePhrases))
            SpeechService(recognizer, SAMPLE_RATE).also { service ->
                speech = service
                service.startListening(Spotting())
            }
        }.onFailure {
            ai.magicbeans.magdroid.bridge.BridgeLog.error(TAG, "wake-word decoder would not start: ${it.message}")
            _listening.value = false
            _problem.value = "The wake-word decoder would not start: ${it.message}"
            stopSelf()
        }
    }

    private inner class Spotting : RecognitionListener {
        // Partials are checked too: waiting for the final result adds most of a
        // second to every wake, which is the difference between answering and
        // being interrupted.
        override fun onPartialResult(hypothesis: String?) = consider(hypothesis)
        override fun onResult(hypothesis: String?) = consider(hypothesis)
        override fun onFinalResult(hypothesis: String?) = consider(hypothesis)
        override fun onError(exception: Exception?) {
            _problem.value = exception?.message ?: "Wake-word listening stopped."
        }
        override fun onTimeout() = Unit
    }

    private fun consider(hypothesis: String?) {
        if (capturing) return
        val decoded = WakeSpotter.decoded(hypothesis) ?: return
        val phrase = WakeSpotter.matches(decoded, activePhrases) ?: return
        _woke.value = Wake(phrase, _woke.value.count + 1)
        startTurn()
    }

    /**
     * Hand the microphone to dictation, and take it back afterwards.
     *
     * One microphone, two consumers. Vosk holds an `AudioRecord` open for as
     * long as it is decoding, so the recogniser cannot open its own until Vosk
     * lets go — leaving both running gets the wake spotter a stream of silence
     * and dictation an error, which looks like the wake word simply stopped
     * working.
     */
    private fun startTurn() {
        // Checked at the top of a turn rather than on a timer: a turn is when
        // the microphone is about to do real work, and a poll would spend the
        // battery this rail exists to protect.
        checkPowerWhileListening()
        if (!_listening.value) return
        capturing = true
        speech?.stop()
        val mode = VoicePrefs.get(this).ambientMode.value
        if (mode == AmbientVoiceMode.Realtime || mode == AmbientVoiceMode.HandsFree) {
            startStreamingTurn(mode)
        } else {
            turn.begin(onCaptureFinished = ::resumeListening)
        }
    }

    private fun startStreamingTurn(mode: AmbientVoiceMode) {
        scope.launch {
            val prefs = VoicePrefs.get(this@WakeService)
            val result = runCatching {
                val catalog = realtime.catalog()
                if (mode == AmbientVoiceMode.Realtime) {
                    val selected = prefs.realtimeProfile.value
                    val profile = catalog.profiles.firstOrNull {
                        it.id == selected && it.nativeAndAvailable
                    } ?: catalog.profiles.firstOrNull { it.nativeAndAvailable }
                        ?: throw VoiceMediaError("No Android-compatible realtime voice profile is available.")
                    prefs.setRealtimeProfile(profile.id)
                    realtime.start(
                        uiThreadId = null,
                        engine = LiveVoiceEngine.Realtime,
                        realtimeProfile = profile,
                    )
                } else {
                    if (!catalog.handsFreeAvailable) {
                        throw VoiceMediaError("The configured hands-free voice pipeline is unavailable.")
                    }
                    val selected = prefs.audioProfiles.value[NativeAudioSurface.HandsFree]
                    val profile = selected?.takeIf(catalog.audioProfiles::containsKey)
                        ?: catalog.defaultAudioProfiles[NativeAudioSurface.HandsFree.wire]
                        ?: catalog.profilesFor(NativeAudioSurface.HandsFree).firstOrNull()?.first
                    profile?.let { prefs.setAudioProfile(NativeAudioSurface.HandsFree, it) }
                    val stages = prefs.audioStageOptions.value
                        .filterKeys { (surface, _) -> surface == NativeAudioSurface.HandsFree }
                        .mapKeys { (key, _) -> key.second }
                    realtime.start(
                        uiThreadId = null,
                        engine = LiveVoiceEngine.HandsFree,
                        audioProfile = profile,
                        audioStageOptions = stages,
                    )
                }
                realtime.state.dropWhile { it.phase == RealtimeVoiceState.Phase.Idle }
                    .first { !it.active }
            }
            result.exceptionOrNull()?.let {
                _problem.value = it.message ?: "Live voice could not start."
            }
            resumeListening()
        }
    }

    private fun resumeListening() {
        if (!capturing) return
        capturing = false
        // Rebuilt rather than restarted: `SpeechService.stop()` releases the
        // recorder, and the same instance will not pick it up again.
        val loaded = model ?: return
        rebuildDecoder(loaded)
    }

    private fun rebuildDecoder(loaded: Model) {
        speech?.stop()
        speech?.shutdown()
        speech = null
        startDecoding(loaded)
    }

    override fun onDestroy() {
        leash?.cancel()
        leash = null
        // The stored window goes with the service. Left behind, the next arm
        // would restore a leash that had already run out with nothing running.
        AmbientArmStore.clear(this)
        arm = null
        if (turnHolder.isInitialized()) turn.shutdown()
        if (realtimeHolder.isInitialized()) realtime.close()
        speech?.stop()
        speech?.shutdown()
        speech = null
        model?.close()
        model = null
        _listening.value = false
        _stopsAtMs.value = 0
        scope.cancel()
        super.onDestroy()
    }

    /**
     * Arm the window, and hold the service to it.
     *
     * The leash is the point: somebody arms a microphone and then stops
     * thinking about it, so the microphone has to stop thinking about itself.
     * A ticker rather than an alarm because the service is already alive for
     * as long as this matters — and it repaints the countdown while it runs,
     * which is the other half of the same job.
     */
    private fun armWindow(restore: Boolean = false) {
        if (!restore) {
            // The owner's setting, not a number chosen here. This was hardcoded
            // at thirty minutes and ran *beside* the existing leash, so picking
            // "2 hours" or "Until I stop" was overridden by a window nobody had
            // asked for and the setting silently did nothing.
            val leash = VoicePrefs.get(this).leash.value
            arm = AmbientArm.armedAt(System.currentTimeMillis(), leash.minutes * 60_000)
            AmbientArmStore.save(this, arm)
        }
        arm?.let { _stopsAtMs.value = it.expiresAtMs }
        leash?.cancel()
        leash = scope.launch {
            while (isActive) {
                delay(TICK_MS)
                val held = arm ?: break
                if (held.hasExpired(System.currentTimeMillis())) {
                    // Expired, not stopped by hand: say which, because a
                    // microphone that went quiet on its own and one somebody
                    // switched off are different facts about the last hour.
                    _problem.value = "Listening stopped: the window ran out."
                    AmbientArmStore.clear(this@WakeService)
                    stopSelf()
                    break
                }
                repaint()
            }
        }
    }

    /** Add one increment, or say plainly that there is none left to add. */
    private fun extendWindow() {
        val held = arm ?: return
        val extended = held.extended()
        if (extended == null) {
            _problem.value = "Already listening for the longest a single window allows."
            return
        }
        arm = extended
        AmbientArmStore.save(this, extended)
        repaint()
    }

    /** Push the current window into the notification already on screen. */
    private fun repaint() {
        runCatching {
            getSystemService(NotificationManager::class.java)
                .notify(NOTIFICATION_ID, notification())
        }
    }

    private fun notification(): Notification {
        val manager = getSystemService(NotificationManager::class.java)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            manager.createNotificationChannel(
                NotificationChannel(
                    CHANNEL, "Wake word",
                    // Low, not default: this notification exists to disclose
                    // that the microphone is on, not to interrupt.
                    NotificationManager.IMPORTANCE_LOW,
                ).apply { description = "Shown whenever ${ProductIdentity.productName} is listening for the primary assistant's wake phrase." },
            )
        }
        val stop = PendingIntent.getService(
            this, 0,
            Intent(this, WakeService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val extend = PendingIntent.getService(
            this, 1,
            Intent(this, WakeService::class.java).setAction(ACTION_EXTEND),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        // The countdown belongs here rather than only in the app: this
        // notification is the one place the owner sees the microphone from,
        // and a static line cannot say how much longer it has.
        val remaining = arm?.let { formatRemaining(it.remainingMs(System.currentTimeMillis())) }
        return Notification.Builder(this, CHANNEL)
            .setContentTitle("Listening for “${activePhrases.firstOrNull() ?: "the wake phrase"}”")
            .setContentText(
                listOfNotNull("Audio stays on this device.", remaining).joinToString("  ·  "),
            )
            .setSmallIcon(android.R.drawable.ic_btn_speak_now)
            .setOngoing(true)
            // A real icon. An action built with a null one produces a
            // notification the system refuses to enqueue, and a foreground
            // service whose notification is rejected is torn down immediately
            // — which looked exactly like the service failing to start.
            .addAction(
                Notification.Action.Builder(
                    android.graphics.drawable.Icon.createWithResource(
                        this, android.R.drawable.ic_menu_close_clear_cancel,
                    ),
                    "Stop listening",
                    stop,
                ).build(),
            )
            // Offered only while there is time to add. A button that cannot do
            // its own job is worse than no button.
            .also { builder ->
                if (arm?.atCeiling == false) {
                    builder.addAction(
                        Notification.Action.Builder(
                            android.graphics.drawable.Icon.createWithResource(
                                this, android.R.drawable.ic_menu_add,
                            ),
                            "+30 min",
                            extend,
                        ).build(),
                    )
                }
            }
            .build()
    }

    /** A wake, with a counter so the same phrase twice is two events. */
    data class Wake(val phrase: String = "", val count: Int = 0)

    companion object {
        private const val TAG = "MagicianWake"
        private const val CHANNEL = "magician.wake"
        private const val NOTIFICATION_ID = 4711
        private const val MODEL_ASSET = "vosk-model"
        private const val MODEL_DIR = "magician-wake-model"
        private const val SAMPLE_RATE = 16000f
        private const val ACTION_STOP = "ai.magicbeans.magdroid.WAKE_STOP"
        private const val ACTION_EXTEND = "ai.magicbeans.magdroid.WAKE_EXTEND"

        /**
         * How often the leash checks itself and repaints.
         *
         * Thirty seconds: the countdown is shown in whole minutes, so a finer
         * tick would wake the process to redraw the same words.
         */
        private const val TICK_MS = 30_000L

        /** Primary canonical name plus aliases, using the same contract as iOS. */
        val phrases: StateFlow<List<String>> = PrimaryAgentWakeIdentityStore.phrases

        private val _listening = MutableStateFlow(false)
        val listening: StateFlow<Boolean> = _listening.asStateFlow()

        private val _woke = MutableStateFlow(Wake())
        val woke: StateFlow<Wake> = _woke.asStateFlow()

        /** When the leash expires, so a surface can say how long is left. */
        private val _stopsAtMs = MutableStateFlow(0L)
        val stopsAtMs: StateFlow<Long> = _stopsAtMs.asStateFlow()

        private val _problem = MutableStateFlow<String?>(null)
        val problem: StateFlow<String?> = _problem.asStateFlow()

        /**
         * A power condition the owner should know about while listening.
         *
         * Separate from [problem], which reports why listening stopped. This one
         * is true *of a running window* — the microphone is open and costing more
         * than usual — and conflating the two would have a warning read as a
         * failure.
         */
        private val _powerWarning = MutableStateFlow<AmbientPowerBlock?>(null)
        val powerWarning: StateFlow<AmbientPowerBlock?> = _powerWarning.asStateFlow()

        /**
         * Start listening.
         *
         * Android 14 onwards refuses a microphone foreground service started
         * from the background, so this must be called from a visible screen.
         * That restriction is the right one and is not worked around here.
         */
        fun start(context: Context) {
            _problem.value = null
            PrimaryAgentWakeIdentityStore.initialize(context)
            if (phrases.value.isEmpty()) {
                _problem.value = "${ProductIdentity.productName} hasn't loaded the primary assistant name yet. Check the self-hosted connection and try again."
                return
            }
            if (ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) !=
                PackageManager.PERMISSION_GRANTED
            ) {
                _problem.value = "Microphone permission is required before ${ProductIdentity.productName} can listen for the assistant wake phrase."
                return
            }
            val intent = Intent(context, WakeService::class.java)
            runCatching {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                    context.startForegroundService(intent)
                } else {
                    context.startService(intent)
                }
            }.onFailure {
                Log.e(TAG, "the system refused to start the listening service", it)
                _problem.value = it.message
            }
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, WakeService::class.java))
        }
    }
}
