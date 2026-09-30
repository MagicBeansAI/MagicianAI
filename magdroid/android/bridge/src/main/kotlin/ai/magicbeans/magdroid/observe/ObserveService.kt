package ai.magicbeans.magdroid.observe

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.media.AudioFormat
import android.media.AudioRecord
import ai.magicbeans.magdroid.voice.AmbientPowerMonitor
import android.media.MediaRecorder
import android.os.Build
import android.os.IBinder
import android.util.Log
import androidx.core.content.ContextCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import ai.magicbeans.magdroid.voice.VoicePrefs
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/** What an observation is doing, for the mini-bar and the Observe screen. */
enum class ObserveState { Idle, Starting, Listening, Failed }

/**
 * Ambient capture: the room, recorded and sent to Magician.
 *
 * This is a different thing from the wake word, and the difference is the whole
 * reason it is a separate service. The wake spotter decodes locally and throws
 * everything away; this uploads the room. Nothing about the two should be
 * confusable — not the notification, not the state, not the code.
 *
 * That difference is what the disclosure has to carry. The notification says
 * recording and says where the audio goes, because "listening for a wake word"
 * and "recording this room and sending it" are the same red dot to the system
 * and completely different facts to whoever is in earshot.
 */
class ObserveService : Service() {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /** Repaints the notification as the session gets on with it. */
    private var ticker: Job? = null

    /** The screen half of the observation, when the owner asked for one. */
    private var screenJob: Job? = null

    /**
     * The explicit share — iOS's `armBroadcast`, Android-shaped. A
     * MediaProjection the owner just consented to, feeding keyframes into the
     * meeting's thread for as long as the share stands.
     */
    private var shareJob: Job? = null
    private var shareFrames: ScreenShareFrames? = null

    /** Kept so the share half can carry the same title the audio half did. */
    private var sessionTitle: String? = null
    private var recorder: AudioRecord? = null
    private var session: ObservationSession? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopSelf()
            return START_NOT_STICKY
        }
        if (intent?.action == ACTION_STOP_SHARE) {
            endScreenShare(restoreForeground = true)
            // A stop-share aimed at a service that was not running must not
            // leave an idle service behind it.
            if (recorder == null && session == null) stopSelf()
            return START_NOT_STICKY
        }
        if (intent?.action == ACTION_SHARE_SCREEN) {
            if (session == null) {
                if (recorder == null) stopSelf()
            } else {
                armScreenShare(intent)
            }
            return START_NOT_STICKY
        }
        if (ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO)
            != PackageManager.PERMISSION_GRANTED
        ) {
            _state.value = ObserveState.Failed
            _problem.value = "Magician needs permission to use the microphone."
            stopSelf()
            return START_NOT_STICKY
        }
        // The same battery floor the wake word answers to. Capture is the
        // heavier of the two — it records *and* uploads — so a phone too low to
        // spot a wake word is certainly too low to stream a room.
        val admission = AmbientPowerMonitor.admit(AmbientPowerMonitor.read(this))
        admission.refusal?.let { block ->
            _state.value = ObserveState.Failed
            _problem.value = block.message
            stopSelf()
            return START_NOT_STICKY
        }
        _powerWarning.value = admission.warning

        promoteForeground(sharing = shareFrames != null)
        beginRepainting()
        if (recorder == null) {
            begin(
                title = intent?.getStringExtra(EXTRA_TITLE),
                url = intent?.getStringExtra(EXTRA_URL),
            )
        }
        // Never restarted by the system. A recording of somebody's room that
        // resumes without being asked for is not a recovery, it is a bug with
        // consequences.
        return START_NOT_STICKY
    }

    private fun begin(title: String?, url: String?) {
        _state.value = ObserveState.Starting
        _problem.value = null
        sessionTitle = title
        scope.launch {
            val uplink = ObservationUplink(this@ObserveService)
            val opened = uplink.start(title, url)
            if (opened == null) {
                _state.value = ObserveState.Failed
                _problem.value = "Magician would not open an observation."
                stopSelf()
                return@launch
            }
            session = opened
            _activeSessionId.value = opened.sessionId
            _activeThreadId.value = opened.threadId
            _state.value = ObserveState.Listening
            // The screen, alongside the sound, narrating into the same thread.
            // Only when the owner asked for it — a meeting that records the room
            // is a different consent from one that also records the screen.
            if (VoicePrefs.get(this@ObserveService).observeScreen.value) {
                followScreen(uplink, opened, title)
            }
            capture(uplink, opened)
        }
    }

    /**
     * Read the microphone and ship it in fixed chunks.
     *
     * `VOICE_RECOGNITION` rather than the default source: it is the one tuned
     * for speech rather than for a phone call, and it leaves the automatic gain
     * and echo cancellation that would otherwise chew a quiet room's audio.
     */
    private suspend fun capture(uplink: ObservationUplink, opened: ObservationSession) {
        val minBuffer = AudioRecord.getMinBufferSize(SAMPLE_RATE, CHANNEL, ENCODING)
        if (minBuffer <= 0) {
            _state.value = ObserveState.Failed
            _problem.value = "This device refused to open the microphone."
            stopSelf()
            return
        }
        val record = runCatching {
            @Suppress("MissingPermission")
            AudioRecord(
                MediaRecorder.AudioSource.VOICE_RECOGNITION,
                SAMPLE_RATE, CHANNEL, ENCODING,
                maxOf(minBuffer, CHUNK_BYTES),
            )
        }.getOrNull()
        if (record == null || record.state != AudioRecord.STATE_INITIALIZED) {
            record?.release()
            _state.value = ObserveState.Failed
            _problem.value = "The microphone is in use by something else."
            stopSelf()
            return
        }
        recorder = record
        record.startRecording()

        val chunk = ByteArray(CHUNK_BYTES)
        var filled = 0
        var seq = 0L
        while (scope.isActive && recorder != null) {
            val read = record.read(chunk, filled, CHUNK_BYTES - filled)
            if (read <= 0) break
            filled += read
            if (filled < CHUNK_BYTES) continue

            val payload = chunk.copyOf(filled)
            filled = 0
            // A dropped chunk is a gap; a stopped capture is the whole
            // conversation. The sequence number still advances so the backend
            // can see that something was lost rather than silently splicing.
            // Re-read on the chunk boundary, which is the natural beat here —
            // every two seconds, and never between a read and its upload.
            //
            // Deliberately a warning and not a stop, unlike the wake window.
            // A room capture is something the owner started on purpose and may
            // be a meeting they cannot repeat; ending it at 20% takes that
            // decision away from them and loses the rest of it. The wake word
            // is ambient and nobody is attending to it, so closing that one
            // costs nothing.
            _powerWarning.value = AmbientPowerMonitor.admit(
                AmbientPowerMonitor.read(this@ObserveService),
            ).warning

            // A 410 is how a stop made anywhere else reaches this device. Treated
            // as one more failed chunk, the microphone carried on recording and
            // uploading into a session that had already ended — the remote stop
            // silently did nothing here.
            when (uplink.sendChunk(opened, seq, payload)) {
                Upload.Ended -> {
                    _problem.value = "Listening stopped: the session was ended."
                    stopSelf()
                    return
                }

                Upload.Transient ->
                    ai.magicbeans.magdroid.bridge.BridgeLog.warn(TAG, "observation chunk $seq did not land")

                Upload.Landed -> Unit
            }
            seq += 1
            _chunksSent.value = seq
        }
        record.stop()
        record.release()
        recorder = null
    }

    /**
     * Arm the screen half of the observation — iOS's `armBroadcast`.
     *
     * iOS pre-creates the session pair and hands the screen half to a
     * broadcast extension; here the audio session is already live and the
     * owner just granted the projection, so the same pair is assembled in
     * place: a screen observation opened on the meeting's own thread, fed
     * keyframes for as long as the share stands. Best-effort like iOS — if
     * the thread's screen slot is taken, audio carries on alone.
     */
    private fun armScreenShare(intent: Intent) {
        val open = session ?: return
        if (shareFrames != null) return
        val resultCode = intent.getIntExtra(EXTRA_SHARE_RESULT, Int.MIN_VALUE)
        val consent = shareConsent(intent)
        if (resultCode == Int.MIN_VALUE || consent == null) return

        // Promoted before the projection exists — Android 14 refuses a
        // projection to a service not already foregrounded as one.
        promoteForeground(sharing = true)
        val frames = ScreenShareFrames.open(this, resultCode, consent) {
            // The system's own stop-sharing affordance, or an echo of our
            // close. endScreenShare is idempotent, so the echo costs nothing.
            endScreenShare(restoreForeground = true)
        }
        if (frames == null) {
            _problem.value = "The screen could not be shared."
            promoteForeground(sharing = false)
            return
        }
        shareFrames = frames
        _screenSharing.value = true
        // The preference-driven follower and this share would race for the
        // thread's one screen slot; the explicit gesture wins.
        screenJob?.cancel()
        screenJob = null
        val title = sessionTitle
        shareJob = scope.launch {
            val uplink = ObservationUplink(this@ObserveService)
            val observation = uplink.startScreen(open.threadId, title)
            if (observation == null) {
                _problem.value = "The meeting would not take this screen."
                endScreenShare(restoreForeground = true)
                return@launch
            }
            val pusher = ScreenFramePusher(this) { jpeg -> uplink.sendFrame(observation, jpeg) }
            while (isActive && !pusher.stopped) {
                frames.latestJpeg()?.let { pusher.push(it) }
                delay(FRAME_MS)
            }
            // A 410 on a frame is a stop made elsewhere reaching this share.
            if (pusher.stopped) endScreenShare(restoreForeground = true)
        }
        repaintNow()
    }

    /**
     * Tear down the share and nothing else; the microphone carries on.
     *
     * Reached from four directions — the owner's disarm, the system revoking
     * the projection, a 410 on a frame, and the whole session ending — so it
     * is synchronized and idempotent rather than assuming a caller.
     */
    @Synchronized
    private fun endScreenShare(restoreForeground: Boolean) {
        val frames = shareFrames ?: return
        shareFrames = null
        _screenSharing.value = false
        shareJob?.cancel()
        shareJob = null
        runCatching { frames.close() }
        // Only while the audio half still runs: a service on its way out must
        // not re-promote itself, and the notification is leaving with it.
        if (restoreForeground && recorder != null) promoteForeground(sharing = false)
    }

    /**
     * Foreground with the types that are true right now. The mediaProjection
     * type is claimed only while a share stands, and startForeground doubles
     * as the notification repaint for both transitions.
     */
    private fun promoteForeground(sharing: Boolean) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            val types = ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE or
                (if (sharing) ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION else 0)
            startForeground(NOTIFICATION_ID, notification(), types)
        } else {
            startForeground(NOTIFICATION_ID, notification())
        }
    }

    private fun shareConsent(intent: Intent): Intent? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            intent.getParcelableExtra(EXTRA_SHARE_DATA, Intent::class.java)
        } else {
            @Suppress("DEPRECATION")
            intent.getParcelableExtra(EXTRA_SHARE_DATA)
        }

    private fun repaintNow() {
        runCatching {
            getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, notification())
        }
    }

    override fun onDestroy() {
        ticker?.cancel()
        ticker = null
        endScreenShare(restoreForeground = false)
        screenJob?.cancel()
        screenJob = null
        val ending = session
        recorder?.runCatching { stop() }
        recorder?.release()
        recorder = null
        session = null
        _activeSessionId.value = null
        _activeThreadId.value = null
        _state.value = ObserveState.Idle
        _chunksSent.value = 0
        // Closed on a scope that outlives this one: the service is going away,
        // and an observation left open on the server keeps accepting audio that
        // will never arrive.
        ending?.let { open ->
            CoroutineScope(Dispatchers.IO).launch {
                ObservationUplink(applicationContext).stop(open.sessionId)
            }
        }
        scope.cancel()
        super.onDestroy()
    }

    /**
     * Keep the notification honest while the session runs.
     *
     * Every fifteen seconds rather than on each chunk: the count is the only
     * thing that moves, and repainting per upload would wake the process far
     * more often than the words change.
     */
    /**
     * Push screen keyframes into the meeting's thread.
     *
     * Best effort throughout. If the observation will not open, or a frame
     * cannot be taken, the audio session carries on alone — a meeting that
     * records sound and not pictures is worth far more than one that refuses.
     *
     * Keyframes on an interval rather than a stream: the server ingests stills
     * and narrates from them, and a phone should not be pushing video up a
     * meeting-length connection.
     */
    private fun followScreen(uplink: ObservationUplink, audio: ObservationSession, title: String?) {
        screenJob?.cancel()
        screenJob = scope.launch {
            val observation = uplink.startScreen(audio.threadId, title) ?: return@launch
            // Capture and upload are separate, as on iOS: the ticker never waits
            // for the network, and a frame the link could not keep up with is
            // dropped rather than queued. Only the newest screen is worth having.
            val pusher = ScreenFramePusher(this) { jpeg -> uplink.sendFrame(observation, jpeg) }
            while (isActive && !pusher.stopped) {
                runCatching { ai.magicbeans.magdroid.tutor.TutorScreenGrab.jpeg() }
                    .getOrNull()
                    ?.let { pusher.push(it) }
                delay(FRAME_MS)
            }
        }
    }

    private fun beginRepainting() {
        if (ticker != null) return
        ticker = scope.launch {
            while (isActive) {
                delay(REPAINT_MS)
                runCatching {
                    getSystemService(NotificationManager::class.java)
                        .notify(NOTIFICATION_ID, notification())
                }
            }
        }
    }

    private fun notification(): Notification {
        val manager = getSystemService(NotificationManager::class.java)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            manager.createNotificationChannel(
                NotificationChannel(
                    CHANNEL_ID, "Observing",
                    // Default, not low. The wake word's notification is a
                    // disclosure you can leave in the shade; this one is a
                    // recording in progress and should be harder to miss.
                    NotificationManager.IMPORTANCE_DEFAULT,
                ).apply {
                    description = "Shown the whole time Magician is recording a room."
                },
            )
        }
        val stop = PendingIntent.getService(
            this, 0,
            Intent(this, ObserveService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        // Live, not a fixed line. iOS's Live Activity carries the phase and a
        // rolling summary; this said the same eight words for an hour, so
        // there was no way to tell a session that was uploading from one that
        // had quietly stopped doing anything.
        val sharing = _screenSharing.value
        val detail = observeNotificationDetail(_state.value, chunksSent.value, sharing)
        val builder = Notification.Builder(this, CHANNEL_ID)
            .setContentTitle(observeLiveLabel(_state.value, sharing))
            .setContentText(detail)
            .setSmallIcon(android.R.drawable.ic_btn_speak_now)
            .setOngoing(true)
            .addAction(
                Notification.Action.Builder(
                    android.graphics.drawable.Icon.createWithResource(
                        this, android.R.drawable.ic_menu_close_clear_cancel,
                    ),
                    "Stop",
                    stop,
                ).build(),
            )
        if (sharing) {
            // Disarming the screen must be reachable from the shade without
            // ending the recording it rides on.
            val stopShare = PendingIntent.getService(
                this, 1,
                Intent(this, ObserveService::class.java).setAction(ACTION_STOP_SHARE),
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
            builder.addAction(
                Notification.Action.Builder(
                    android.graphics.drawable.Icon.createWithResource(
                        this, android.R.drawable.ic_menu_close_clear_cancel,
                    ),
                    "Stop sharing",
                    stopShare,
                ).build(),
            )
        }
        return builder.build()
    }

    companion object {
        private const val TAG = "MagicianObserve"
        private const val CHANNEL_ID = "magician.observe"
        private const val NOTIFICATION_ID = 4712
        private const val ACTION_STOP = "ai.magicbeans.magdroid.OBSERVE_STOP"
        private const val ACTION_SHARE_SCREEN = "ai.magicbeans.magdroid.OBSERVE_SHARE_SCREEN"
        private const val ACTION_STOP_SHARE = "ai.magicbeans.magdroid.OBSERVE_STOP_SHARE"
        private const val EXTRA_TITLE = "title"
        private const val EXTRA_URL = "url"
        private const val EXTRA_SHARE_RESULT = "share_result"
        private const val EXTRA_SHARE_DATA = "share_data"

        private const val REPAINT_MS = 15_000L

        /**
         * How often a screen keyframe is taken. iOS's `frameInterval`.
         *
         * Three seconds catches a slide change while it is still the thing
         * being talked about. It is affordable because capture does not wait on
         * upload — a frame the link cannot keep up with is dropped, not queued.
         */
        private const val FRAME_MS = 3_000L
        private const val SAMPLE_RATE = 16000
        private const val CHANNEL = AudioFormat.CHANNEL_IN_MONO
        private const val ENCODING = AudioFormat.ENCODING_PCM_16BIT

        /**
         * Two seconds of 16 kHz mono PCM.
         *
         * Small enough that a dropped chunk loses little, large enough that the
         * request overhead is not most of the traffic.
         */
        private const val CHUNK_BYTES = SAMPLE_RATE * 2 * 2

        private val _state = MutableStateFlow(ObserveState.Idle)
        val state: StateFlow<ObserveState> = _state.asStateFlow()

        private val _problem = MutableStateFlow<String?>(null)
        val problem: StateFlow<String?> = _problem.asStateFlow()

        /**
         * A power condition worth knowing about while a room is being recorded.
         *
         * Distinct from [problem], which says why capture stopped. This is true
         * of a capture that is still running and costing more than usual.
         */
        private val _powerWarning =
            MutableStateFlow<ai.magicbeans.magdroid.voice.AmbientPowerBlock?>(null)
        val powerWarning: StateFlow<ai.magicbeans.magdroid.voice.AmbientPowerBlock?> =
            _powerWarning.asStateFlow()

        /** Chunks accepted so far, which is the only honest progress signal. */
        private val _chunksSent = MutableStateFlow(0L)
        val chunksSent: StateFlow<Long> = _chunksSent.asStateFlow()

        /**
         * Identity of this phone's live room capture.
         *
         * Observe's server listing includes the same session. Publishing the
         * identity lets the UI render one local live cockpit and filter that
         * row out of the server-side list instead of showing the meeting twice.
         */
        private val _activeSessionId = MutableStateFlow<String?>(null)
        val activeSessionId: StateFlow<String?> = _activeSessionId.asStateFlow()

        /** The canonical meeting thread opened for this phone's capture. */
        private val _activeThreadId = MutableStateFlow<String?>(null)
        val activeThreadId: StateFlow<String?> = _activeThreadId.asStateFlow()

        /** Whether the explicit screen share is currently armed. */
        private val _screenSharing = MutableStateFlow(false)
        val screenSharing: StateFlow<Boolean> = _screenSharing.asStateFlow()

        /** Roughly how long has been captured, from the chunks that went up. */
        fun elapsedSeconds(chunks: Long): Long = chunks * 2

        fun start(context: Context, title: String? = null, url: String? = null) {
            val intent = Intent(context, ObserveService::class.java)
                .putExtra(EXTRA_TITLE, title)
                .putExtra(EXTRA_URL, url)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                context.startForegroundService(intent)
            } else {
                context.startService(intent)
            }
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, ObserveService::class.java))
        }

        /**
         * Attach a just-granted MediaProjection consent to the running
         * session. The result pair is exactly what the system consent dialog
         * handed back; consent is asked for again on every share.
         */
        fun shareScreen(context: Context, resultCode: Int, data: Intent) {
            context.startService(
                Intent(context, ObserveService::class.java)
                    .setAction(ACTION_SHARE_SCREEN)
                    .putExtra(EXTRA_SHARE_RESULT, resultCode)
                    .putExtra(EXTRA_SHARE_DATA, data),
            )
        }

        /** Stop sharing the screen; the audio session carries on. */
        fun stopSharingScreen(context: Context) {
            context.startService(
                Intent(context, ObserveService::class.java).setAction(ACTION_STOP_SHARE),
            )
        }
    }
}
