import Foundation
import QuartzCore
import ReplayKit

/// ReplayKit broadcast-upload extension. During a system broadcast iOS hands this
/// process three sample-buffer streams; we forward the two audio ones into the
/// backend `capture: "client"` session the main app armed (shared via the App
/// Group `ObservationArm`):
///   - `.audioApp` — the audio other apps are playing (the meeting you hear) →
///     the diarized `primary` channel.
///   - `.audioMic` — the device microphone (your voice) → the hard-"You" `mic`
///     channel. Delivered only when the user enables the microphone in the
///     broadcast UI.
///   - `.video` — ignored for now (future: screen keyframes).
///
/// The main app creates the session (`mic:true`) and saves the arm before the
/// broadcast starts, so this extension never makes the start-session call itself.
/// Protected audio (CallKit / VoIP / DRM) is not delivered by iOS — by design.
class SampleHandler: RPBroadcastSampleHandler {
    private var primaryPump: ObservationUploadPump?
    private var micPump: ObservationUploadPump?
    private var primaryAccumulator = ObservationChunkAccumulator(chunkSeconds: 6)
    private var micAccumulator = ObservationChunkAccumulator(chunkSeconds: 6)
    private let primaryConverter = BroadcastAudioConverter()
    private let micConverter = BroadcastAudioConverter()

    // Screen keyframes → the paired client screen observation (if armed).
    private var framePusher: ObservationFramePusher?
    private let frameConverter = BroadcastFrameConverter()
    private var lastFrameAt: CFTimeInterval = 0
    /// Seconds between pushed keyframes — matches the observe rail's default
    /// cadence; the diff-gate on the server drops unchanged frames for free.
    private let frameInterval: CFTimeInterval = 3

    override func broadcastStarted(withSetupInfo setupInfo: [String: NSObject]?) {
        guard let arm = ObservationArm.claim() else {
            finishBroadcastWithError(NSError(
                domain: "ai.magicbeans.magican.broadcast",
                code: 1,
                userInfo: [NSLocalizedDescriptionKey:
                    "Open Magican → Observe → Share screen and tap “Prepare session” first, then start the broadcast."]
            ))
            return
        }
        let client = ObservationUplinkClient()
        let primary = ObservationUploadPump(
            client: client, sessionId: arm.sessionId, token: arm.uploadToken, channel: "primary"
        )
        let mic = ObservationUploadPump(
            client: client, sessionId: arm.sessionId, token: arm.uploadToken, channel: "mic"
        )
        Task {
            await primary.setOnEnded { [weak self] in self?.serverEnded() }
            await mic.setOnEnded { [weak self] in self?.serverEnded() }
        }
        primaryPump = primary
        micPump = mic
        // Screen keyframes go to the paired client observation, if the app armed
        // one (audio-only if it didn't).
        if let observeId = arm.observeId, let frameToken = arm.frameToken {
            framePusher = ObservationFramePusher(client: client, observeId: observeId, token: frameToken)
        }
    }

    override func processSampleBuffer(_ sampleBuffer: CMSampleBuffer, with sampleBufferType: RPSampleBufferType) {
        switch sampleBufferType {
        case .audioApp:
            forward(sampleBuffer, converter: primaryConverter, accumulator: &primaryAccumulator, pump: primaryPump)
        case .audioMic:
            forward(sampleBuffer, converter: micConverter, accumulator: &micAccumulator, pump: micPump)
        case .video:
            forwardFrame(sampleBuffer)
        @unknown default:
            break
        }
    }

    /// Throttle screen frames to `frameInterval`, downscale to a JPEG, and push.
    private func forwardFrame(_ sampleBuffer: CMSampleBuffer) {
        guard let pusher = framePusher else { return }
        let now = CACurrentMediaTime()
        guard now - lastFrameAt >= frameInterval else { return }
        lastFrameAt = now
        guard let jpeg = frameConverter.jpeg(from: sampleBuffer) else { return }
        Task { await pusher.push(jpeg) }
    }

    override func broadcastFinished() {
        // Best-effort flush of the trailing partial chunks; the process is torn
        // down shortly after this returns.
        if let rest = primaryAccumulator.flush(), let pump = primaryPump {
            Task { await pump.submit(rest) }
        }
        if let rest = micAccumulator.flush(), let pump = micPump {
            Task { await pump.submit(rest) }
        }
    }

    private func forward(
        _ sampleBuffer: CMSampleBuffer,
        converter: BroadcastAudioConverter,
        accumulator: inout ObservationChunkAccumulator,
        pump: ObservationUploadPump?
    ) {
        guard let pump, let pcm = converter.pcm16(from: sampleBuffer) else { return }
        for chunk in accumulator.append(pcm) {
            Task { await pump.submit(chunk) }
        }
    }

    /// The backend ended the session (410) — stop the broadcast so the system UI
    /// reflects it.
    private func serverEnded() {
        finishBroadcastWithError(NSError(
            domain: "ai.magicbeans.magican.broadcast",
            code: 2,
            userInfo: [NSLocalizedDescriptionKey: "The Magican session ended."]
        ))
    }
}
