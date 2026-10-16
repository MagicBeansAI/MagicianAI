import AudioToolbox
import AVFoundation
import CoreAudio
import Foundation
import ScreenCaptureKit
import Speech

// magician-macos-meet-audio — streaming audio capture for the Google-Meet bot.
//
// Captures the target app's audio DIRECTLY via ScreenCaptureKit — no virtual
// device, no Chrome speaker-routing (the routing that kept breaking the spike's
// BlackHole-2ch capture). Audio is resampled to 16 kHz mono PCM16 (little-
// endian) and written as a continuous raw byte stream to STDOUT; diagnostics go
// to STDERR as JSON lines. The Rust side (media_rails::meeting::bridge_macos)
// reads STDOUT as the AudioSource feeding the streaming STT.
//
// Requires the Screen Recording TCC grant (System Settings → Privacy & Security
// → Screen Recording) — ScreenCaptureKit audio is gated by it.
//
// Usage:
//   magician-macos-meet-audio --target-bundle-id com.google.Chrome \
//     [--target-pid 12345] [--display-audio] [--sample-rate 16000] [--channels 1]
//   magician-macos-meet-audio --mode capture-mic [--sample-rate 16000] [--channels 1]
//     (default-input microphone capture for the passive listener's "You" track;
//      Microphone TCC only — no Screen Recording)
//
// `--target-pid` selects the app by process id (used to capture the bot's OWN
// launched browser instance, which bundle id can't isolate); when set it takes
// precedence over `--target-bundle-id`. `--display-audio` skips app filtering
// entirely and captures the whole display's (system) audio — required for
// engines whose audio SCK never attributes to their application (the cloak
// Chromium engine measures 0.0 under an app filter while audibly playing).

let TARGET_SAMPLE_RATE_DEFAULT = 16_000.0

@main
struct MeetAudioHelper {
    static func main() async {
        let opts = parseOptions(Array(CommandLine.arguments.dropFirst()))
        let mode = opts["mode"] ?? "capture"
        let sampleRate = Double(opts["sample-rate"] ?? "") ?? TARGET_SAMPLE_RATE_DEFAULT
        let channels = AVAudioChannelCount(UInt32(opts["channels"] ?? "") ?? 1)

        do {
            if mode == "transcribe" {
                // On-device STT: read PCM16 from stdin → SFSpeechRecognizer →
                // JSON transcript events on stdout. Needs the Speech Recognition
                // TCC grant (not Screen Recording — this mode doesn't capture).
                let locale = opts["locale"] ?? "en-US"
                let contextual = (opts["contextual"] ?? "Hey Presto,Hey Uuaa,Presto")
                    .split(separator: ",")
                    .map { $0.trimmingCharacters(in: .whitespaces) }
                    .filter { !$0.isEmpty }
                let transcriber = try SpeechTranscriber(
                    localeId: locale, sampleRate: sampleRate, channels: channels,
                    contextualStrings: contextual
                )
                try await transcriber.run()
            } else if mode == "inject" {
                // Read PCM16 from stdin and play it to a named CoreAudio output
                // device (the meeting mic, e.g. "BlackHole 16ch"), then exit.
                let device = opts["device"] ?? "BlackHole 16ch"
                let injector = try AudioInjector(
                    deviceName: device, sampleRate: sampleRate, channels: channels
                )
                try injector.run()
            } else if mode == "capture-mic" {
                // Capture the default INPUT device (the user's microphone) via
                // AVAudioEngine and stream PCM16 to stdout — the passive
                // listener's "You" track. Needs the Microphone TCC grant only
                // (no Screen Recording).
                let granted = await AVCaptureDevice.requestAccess(for: .audio)
                guard granted else {
                    throw HelperError.capture("microphone permission denied")
                }
                // OS voice-activity gating (macOS 14+, echo-cancelled): only
                // emit PCM while the system detects actual speech on the
                // input. Defaults ON; `--vad 0` disables for debugging.
                let wantVad = opts["vad"].map { $0 != "0" && $0.lowercased() != "false" } ?? true
                let mic = try MicrophoneCapture(
                    sampleRate: sampleRate, channels: channels, voiceGate: wantVad
                )
                try mic.start()
                // `input_device` makes a surprising "You" track diagnosable:
                // AVAudioEngine taps the system DEFAULT input, and a default
                // left on a loopback device (e.g. BlackHole) silently feeds
                // meeting audio into the mic track.
                logJson(["event": "ready", "mode": "capture-mic",
                         "sample_rate": String(Int(sampleRate)),
                         "channels": String(channels),
                         "input_device": MicrophoneCapture.defaultInputDeviceName() ?? "unknown",
                         "vad": mic.voiceGateStatus])
                // Run until killed by the parent (Rust drops the pipe → exit).
                try await Task.sleep(nanoseconds: UInt64.max)
            } else {
                let displayAudio = opts["display-audio"]
                    .map { $0 != "0" && $0.lowercased() != "false" } ?? false
                let bundleId = opts["target-bundle-id"] ?? "com.google.Chrome"
                let targetPid = opts["target-pid"].flatMap { pid_t($0) }
                let capture = try AudioCapture(
                    bundleId: bundleId, targetPid: targetPid, displayAudio: displayAudio,
                    sampleRate: sampleRate, channels: channels
                )
                try await capture.start()
                var ready: [String: String] = ["event": "ready", "mode": "capture",
                         "scope": displayAudio ? "display" : "application",
                         "bundle_id": bundleId,
                         "sample_rate": String(Int(sampleRate)), "channels": String(channels)]
                if let targetPid { ready["target_pid"] = String(targetPid) }
                logJson(ready)
                // Run until killed by the parent (Rust drops the pipe → exit).
                try await Task.sleep(nanoseconds: UInt64.max)
            }
        } catch {
            logJson(["event": "error", "message": "\(error)"])
            Foundation.exit(1)
        }
    }
}

enum HelperError: Error, CustomStringConvertible {
    case format(String)
    case capture(String)

    var description: String {
        switch self {
        case .format(let m): return "format error: \(m)"
        case .capture(let m): return "capture error: \(m)"
        }
    }
}

final class AudioCapture: NSObject, SCStreamOutput, SCStreamDelegate {
    private let bundleId: String
    private let targetPid: pid_t?
    private let displayAudio: Bool
    private let outputFormat: AVAudioFormat
    private var stream: SCStream?
    private var converter: AVAudioConverter?
    private let stdout = FileHandle.standardOutput
    private let queue = DispatchQueue(label: "meetaudio.capture")

    init(bundleId: String, targetPid: pid_t?, displayAudio: Bool,
         sampleRate: Double, channels: AVAudioChannelCount) throws {
        self.bundleId = bundleId
        self.targetPid = targetPid
        self.displayAudio = displayAudio
        guard let fmt = AVAudioFormat(
            commonFormat: .pcmFormatInt16,
            sampleRate: sampleRate,
            channels: channels,
            interleaved: true
        ) else {
            throw HelperError.format("could not build PCM16 \(Int(sampleRate))Hz/\(channels)ch output format")
        }
        self.outputFormat = fmt
        super.init()
    }

    func start() async throws {
        let content = try await SCShareableContent.excludingDesktopWindows(
            false, onScreenWindowsOnly: false
        )
        guard let display = content.displays.first else {
            throw HelperError.capture("no display available")
        }
        let filter: SCContentFilter
        if displayAudio {
            // Explicit whole-display (system) audio: no app filter. Required for
            // engines whose audio SCK never attributes to their application.
            filter = SCContentFilter(display: display, excludingWindows: [])
        } else {
            let apps: [SCRunningApplication]
            if let pid = targetPid {
                apps = content.applications.filter { $0.processID == pid }
            } else {
                apps = content.applications.filter { $0.bundleIdentifier == bundleId }
            }
            if apps.isEmpty {
                var warn: [String: String] = ["event": "warn",
                         "message": "target app not found; capturing whole-display audio",
                         "bundle_id": bundleId]
                if let pid = targetPid { warn["target_pid"] = String(pid) }
                logJson(warn)
                filter = SCContentFilter(display: display, excludingWindows: [])
            } else {
                filter = SCContentFilter(display: display, including: apps, exceptingWindows: [])
            }
        }

        let config = SCStreamConfiguration()
        config.capturesAudio = true
        config.excludesCurrentProcessAudio = true
        config.sampleRate = 48_000 // SCStream delivers ~48k float; we resample below.
        config.channelCount = 2
        // A tiny video plane is still required; we ignore video frames entirely.
        config.width = 2
        config.height = 2
        config.minimumFrameInterval = CMTime(value: 1, timescale: 1)

        let stream = SCStream(filter: filter, configuration: config, delegate: self)
        try stream.addStreamOutput(self, type: .audio, sampleHandlerQueue: queue)
        try await stream.startCapture()
        self.stream = stream
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
                of type: SCStreamOutputType) {
        guard type == .audio, sampleBuffer.isValid else { return }
        guard let data = convert(sampleBuffer), !data.isEmpty else { return }
        do {
            try stdout.write(contentsOf: data)
        } catch {
            // Parent closed the pipe (bridge stopped) — exit cleanly.
            Foundation.exit(0)
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        logJson(["event": "error", "message": "stream stopped: \(error)"])
        Foundation.exit(1)
    }

    /// Convert one CMSampleBuffer (float32 @ capture rate/channels) to our
    /// int16/16k/mono output format and return the raw little-endian bytes.
    private func convert(_ sampleBuffer: CMSampleBuffer) -> Data? {
        guard var asbd = sampleBuffer.formatDescription?.audioStreamBasicDescription,
              let inFormat = AVAudioFormat(streamDescription: &asbd) else {
            return nil
        }
        return try? sampleBuffer.withAudioBufferList { abl, _ -> Data? in
            guard let inBuffer = AVAudioPCMBuffer(
                pcmFormat: inFormat, bufferListNoCopy: abl.unsafePointer, deallocator: nil
            ) else { return nil }

            if converter == nil {
                converter = AVAudioConverter(from: inFormat, to: outputFormat)
            }
            guard let converter else { return nil }

            let ratio = outputFormat.sampleRate / inFormat.sampleRate
            let outCapacity = AVAudioFrameCount(Double(inBuffer.frameLength) * ratio) + 1_024
            guard let outBuffer = AVAudioPCMBuffer(
                pcmFormat: outputFormat, frameCapacity: outCapacity
            ) else { return nil }

            var fed = false
            var convError: NSError?
            let status = converter.convert(to: outBuffer, error: &convError) { _, inputStatus in
                if fed {
                    inputStatus.pointee = .noDataNow
                    return nil
                }
                fed = true
                inputStatus.pointee = .haveData
                return inBuffer
            }
            if status == .error || outBuffer.frameLength == 0 {
                return nil
            }
            guard let channel = outBuffer.int16ChannelData else { return nil }
            let sampleCount = Int(outBuffer.frameLength) * Int(outputFormat.channelCount)
            return Data(bytes: channel[0], count: sampleCount * MemoryLayout<Int16>.size)
        }
    }
}

/// Captures the default INPUT device (microphone) via AVAudioEngine and streams
/// int16 PCM at the requested rate to stdout — the passive listener's "You"
/// track. Mirrors `AudioCapture`'s conversion (AVAudioConverter resample to the
/// output format). Microphone TCC only; no Screen Recording.
///
/// Voice gate (macOS 14+): the OS-level voice-activity detector
/// (`kAudioDevicePropertyVoiceActivityDetectionEnable`, echo-cancelled — the
/// same primitive the system's "you're muted" affordances use) gates emission,
/// so only segments around actual detected SPEECH reach stdout. This is the
/// fix for the hot-mic reality that an app-level meeting mute does not silence
/// the device: room noise and meeting-audio echo stop reaching STT (which
/// would otherwise hallucinate transcript turns from them). A short pre-roll
/// preserves word onsets (VAD trips a beat after speech starts) and a hangover
/// preserves trailing words. Unavailable VAD (macOS 13, denied device) falls
/// open: emit everything, exactly the old behavior — the Rust RMS gate stays
/// as the backstop.
final class MicrophoneCapture {
    private let outputFormat: AVAudioFormat
    private let engine = AVAudioEngine()
    private var converter: AVAudioConverter?
    private let stdout = FileHandle.standardOutput

    /// Emit window kept after the detector's falling edge (trailing words).
    private static let voiceHangover: TimeInterval = 0.8
    /// Converted chunks retained while gated, flushed on the rising edge
    /// (word onsets). ~6 × ~85ms tap chunks ≈ half a second of pre-roll.
    private static let preRollMaxChunks = 6

    private let wantVoiceGate: Bool
    private let gateLock = NSLock()
    private var vadActive = false
    private var voicePresent = true // fail-open until the detector reports
    private var lastVoiceAt = Date()
    private var preRoll: [Data] = []
    private var vadDeviceId = AudioDeviceID(0)

    /// For the ready diagnostic: "on", "off" (--vad 0), or "unavailable".
    private(set) var voiceGateStatus = "off"

    init(sampleRate: Double, channels: AVAudioChannelCount, voiceGate: Bool = true) throws {
        guard let fmt = AVAudioFormat(
            commonFormat: .pcmFormatInt16,
            sampleRate: sampleRate,
            channels: channels,
            interleaved: true
        ) else {
            throw HelperError.format("could not build PCM16 \(Int(sampleRate))Hz/\(channels)ch output format")
        }
        self.outputFormat = fmt
        self.wantVoiceGate = voiceGate
    }

    func start() throws {
        let input = engine.inputNode
        let inFormat = input.outputFormat(forBus: 0)
        guard inFormat.sampleRate > 0 else {
            throw HelperError.capture("microphone input has no format (no input device?)")
        }
        if wantVoiceGate {
            setupVoiceGate()
        }
        converter = AVAudioConverter(from: inFormat, to: outputFormat)
        input.installTap(onBus: 0, bufferSize: 4_096, format: inFormat) { [weak self] buffer, _ in
            guard let self, let converter = self.converter else { return }
            let ratio = self.outputFormat.sampleRate / inFormat.sampleRate
            let outCapacity = AVAudioFrameCount(Double(buffer.frameLength) * ratio) + 1_024
            guard let outBuffer = AVAudioPCMBuffer(
                pcmFormat: self.outputFormat, frameCapacity: outCapacity
            ) else { return }
            var fed = false
            var convError: NSError?
            let status = converter.convert(to: outBuffer, error: &convError) { _, inputStatus in
                if fed {
                    inputStatus.pointee = .noDataNow
                    return nil
                }
                fed = true
                inputStatus.pointee = .haveData
                return buffer
            }
            if status == .error || outBuffer.frameLength == 0 {
                return
            }
            guard let channel = outBuffer.int16ChannelData else { return }
            let count = Int(outBuffer.frameLength) * Int(self.outputFormat.channelCount)
            let data = Data(bytes: channel[0], count: count * MemoryLayout<Int16>.size)
            self.emitGated(data)
        }
        try engine.start()
    }

    /// Write through the voice gate: emit while speech is present (plus
    /// hangover), buffer a bounded pre-roll while gated.
    private func emitGated(_ data: Data) {
        gateLock.lock()
        let emit: Bool
        if !vadActive {
            emit = true
        } else if voicePresent {
            emit = true
        } else {
            emit = Date().timeIntervalSince(lastVoiceAt) < Self.voiceHangover
        }
        if emit {
            let buffered = preRoll
            preRoll.removeAll()
            gateLock.unlock()
            do {
                for chunk in buffered {
                    try stdout.write(contentsOf: chunk)
                }
                try stdout.write(contentsOf: data)
            } catch {
                // Parent closed the pipe (listener stopped) — exit cleanly.
                Foundation.exit(0)
            }
        } else {
            preRoll.append(data)
            if preRoll.count > Self.preRollMaxChunks {
                preRoll.removeFirst(preRoll.count - Self.preRollMaxChunks)
            }
            gateLock.unlock()
        }
    }

    /// Enable the OS voice-activity detector on the default input device and
    /// track its state. Any failure leaves the gate open (old behavior).
    private func setupVoiceGate() {
        guard #available(macOS 14.0, *) else {
            voiceGateStatus = "unavailable"
            return
        }
        guard let deviceId = Self.defaultInputDeviceID(), deviceId != 0 else {
            voiceGateStatus = "unavailable"
            return
        }
        vadDeviceId = deviceId
        var enableAddr = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyVoiceActivityDetectionEnable,
            mScope: kAudioObjectPropertyScopeInput,
            mElement: kAudioObjectPropertyElementMain
        )
        var enable: UInt32 = 1
        let enableStatus = AudioObjectSetPropertyData(
            deviceId, &enableAddr, 0, nil, UInt32(MemoryLayout<UInt32>.size), &enable
        )
        guard enableStatus == noErr else {
            voiceGateStatus = "unavailable"
            logJson(["event": "vad", "status": "unavailable", "code": String(enableStatus)])
            return
        }
        var stateAddr = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyVoiceActivityDetectionState,
            mScope: kAudioObjectPropertyScopeInput,
            mElement: kAudioObjectPropertyElementMain
        )
        let listenStatus = AudioObjectAddPropertyListenerBlock(
            deviceId,
            &stateAddr,
            DispatchQueue(label: "mic-vad-state")
        ) { [weak self] _, _ in
            self?.refreshVoiceState()
        }
        guard listenStatus == noErr else {
            voiceGateStatus = "unavailable"
            logJson(["event": "vad", "status": "unavailable", "code": String(listenStatus)])
            return
        }
        gateLock.lock()
        vadActive = true
        gateLock.unlock()
        voiceGateStatus = "on"
        refreshVoiceState()
    }

    private func refreshVoiceState() {
        // Only ever invoked from the macOS-14+ gated setup/listener paths;
        // the guard keeps the 14+-annotated selector lexically scoped.
        guard #available(macOS 14.0, *) else { return }
        var stateAddr = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyVoiceActivityDetectionState,
            mScope: kAudioObjectPropertyScopeInput,
            mElement: kAudioObjectPropertyElementMain
        )
        var state: UInt32 = 0
        var size = UInt32(MemoryLayout<UInt32>.size)
        guard AudioObjectGetPropertyData(vadDeviceId, &stateAddr, 0, nil, &size, &state) == noErr
        else { return }
        gateLock.lock()
        voicePresent = state != 0
        if voicePresent {
            lastVoiceAt = Date()
        }
        gateLock.unlock()
    }

    /// `AudioDeviceID` of the system default input device.
    static func defaultInputDeviceID() -> AudioDeviceID? {
        var addr = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDefaultInputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var deviceId = AudioDeviceID(0)
        var size = UInt32(MemoryLayout<AudioDeviceID>.size)
        guard AudioObjectGetPropertyData(
            AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &size, &deviceId
        ) == noErr, deviceId != 0 else { return nil }
        return deviceId
    }

    /// Display name of the system DEFAULT input device (what the tap above
    /// actually captures). Logged at start so a surprising "You" track is
    /// diagnosable — e.g. a default input left on a loopback device
    /// (BlackHole) feeds MEETING audio into the mic track.
    static func defaultInputDeviceName() -> String? {
        guard let deviceId = defaultInputDeviceID() else { return nil }
        var nameAddr = AudioObjectPropertyAddress(
            mSelector: kAudioObjectPropertyName,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var nameRef: Unmanaged<CFString>?
        var nameSize = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
        let status = withUnsafeMutablePointer(to: &nameRef) {
            AudioObjectGetPropertyData(deviceId, &nameAddr, 0, nil, &nameSize, $0)
        }
        guard status == noErr, let cf = nameRef?.takeRetainedValue() else { return nil }
        return cf as String
    }
}

/// On-device speech-to-text over a stdin PCM16 stream. Emits JSON transcript
/// events on stdout (`{"type":"partial"|"final","text":"…"}`); diagnostics on
/// stderr. Uses Apple's on-device `SFSpeechRecognizer` — fast streaming
/// partials, no network, no model download. Restarts the recognition request
/// per finalized utterance for continuous meeting transcription.
final class SpeechTranscriber {
    private let locale: Locale
    private let format: AVAudioFormat
    private var recognizer: SFSpeechRecognizer?
    private let lock = NSLock()
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var task: SFSpeechRecognitionTask?
    private var lastFinal = ""
    private var finishing = false
    private let contextualStrings: [String]

    init(localeId: String, sampleRate: Double, channels: AVAudioChannelCount,
         contextualStrings: [String]) throws {
        self.locale = Locale(identifier: localeId)
        self.contextualStrings = contextualStrings
        guard let fmt = AVAudioFormat(
            commonFormat: .pcmFormatInt16,
            sampleRate: sampleRate,
            channels: channels,
            interleaved: true
        ) else {
            throw HelperError.format("could not build PCM16 input format")
        }
        self.format = fmt
    }

    func run() async throws {
        try await requestSpeechAuthorization()
        guard let rec = SFSpeechRecognizer(locale: locale), rec.isAvailable else {
            throw HelperError.capture("speech recognizer unavailable for \(locale.identifier)")
        }
        guard rec.supportsOnDeviceRecognition else {
            throw HelperError.capture("on-device recognition unavailable for \(locale.identifier)")
        }
        self.recognizer = rec
        startRequest()
        logJson(["event": "ready", "mode": "transcribe", "locale": locale.identifier])
        readStdinLoop()
        // Keep the process alive; stdin EOF (parent closed) exits us.
        try await Task.sleep(nanoseconds: UInt64.max)
    }

    private func startRequest() {
        let req = SFSpeechAudioBufferRecognitionRequest()
        req.shouldReportPartialResults = true
        req.requiresOnDeviceRecognition = true
        if !contextualStrings.isEmpty {
            // Bias recognition toward the wake phrase ("Hey Presto" otherwise
            // mis-hears as "Press two" / "April 2" on compressed meeting audio).
            req.contextualStrings = contextualStrings
        }
        lock.lock()
        self.request = req
        lock.unlock()
        self.task = recognizer?.recognitionTask(with: req) { [weak self] result, error in
            guard let self else { return }
            if let result {
                let text = result.bestTranscription.formattedString
                if result.isFinal {
                    self.emit(type: "final", text: text)
                    self.lock.lock()
                    let done = self.finishing
                    self.lock.unlock()
                    if done {
                        Foundation.exit(0)
                    } else {
                        self.restartRequest()
                    }
                } else {
                    self.emit(type: "partial", text: text)
                }
            }
            if error != nil {
                self.restartRequest()
            }
        }
    }

    private func restartRequest() {
        lock.lock()
        self.request?.endAudio()
        self.request = nil
        lock.unlock()
        self.task = nil
        startRequest()
    }

    private func appendPCM(_ data: Data) {
        let bytesPerFrame = max(Int(format.streamDescription.pointee.mBytesPerFrame), 1)
        let frameCount = AVAudioFrameCount(data.count / bytesPerFrame)
        guard frameCount > 0,
              let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frameCount),
              let dst = buffer.int16ChannelData else { return }
        buffer.frameLength = frameCount
        data.withUnsafeBytes { raw in
            if let src = raw.bindMemory(to: Int16.self).baseAddress {
                dst[0].update(from: src, count: Int(frameCount))
            }
        }
        lock.lock()
        let req = self.request
        lock.unlock()
        req?.append(buffer)
    }

    private func readStdinLoop() {
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let handle = FileHandle.standardInput
            while true {
                let data = handle.availableData
                if data.isEmpty {
                    // Parent closed stdin → end of meeting. Finalize the last
                    // utterance, then exit once its final flushes (5s fallback).
                    self?.lock.lock()
                    self?.finishing = true
                    self?.request?.endAudio()
                    self?.lock.unlock()
                    DispatchQueue.global().asyncAfter(deadline: .now() + 5) {
                        Foundation.exit(0)
                    }
                    return
                }
                self?.appendPCM(data)
            }
        }
    }

    private func emit(type: String, text: String) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty { return }
        if type == "final" {
            if trimmed == lastFinal { return }
            lastFinal = trimmed
        }
        logStdoutJson(["type": type, "text": trimmed])
    }
}

@discardableResult
private func requestSpeechAuthorization() async throws -> SFSpeechRecognizerAuthorizationStatus {
    let status = await withCheckedContinuation { continuation in
        SFSpeechRecognizer.requestAuthorization { status in
            continuation.resume(returning: status)
        }
    }
    guard status == .authorized else {
        throw HelperError.capture("speech recognition not authorized (\(status.rawValue))")
    }
    return status
}

/// Transcript events go to STDOUT (the data channel); diagnostics use STDERR.
func logStdoutJson(_ fields: [String: String]) {
    if let data = try? JSONSerialization.data(withJSONObject: fields),
       var line = String(data: data, encoding: .utf8) {
        line += "\n"
        FileHandle.standardOutput.write(Data(line.utf8))
    }
}

/// Plays a stdin PCM16 stream to a named CoreAudio output device (the meeting
/// mic, e.g. "BlackHole 16ch"), then exits. Native replacement for the interim
/// `sox -t coreaudio` inject path. Converts int16 → float for the engine.
final class AudioInjector {
    private let deviceName: String
    private let sampleRate: Double
    private let channels: AVAudioChannelCount

    init(deviceName: String, sampleRate: Double, channels: AVAudioChannelCount) throws {
        self.deviceName = deviceName
        self.sampleRate = sampleRate
        self.channels = channels
    }

    func run() throws {
        let data = FileHandle.standardInput.readDataToEndOfFile()
        if data.isEmpty {
            Foundation.exit(0)
        }
        guard let format = AVAudioFormat(
            commonFormat: .pcmFormatFloat32,
            sampleRate: sampleRate,
            channels: channels,
            interleaved: false
        ) else {
            throw HelperError.format("could not build inject format")
        }

        let channelCount = max(Int(channels), 1)
        let totalSamples = data.count / MemoryLayout<Int16>.size
        let frameCount = AVAudioFrameCount(totalSamples / channelCount)
        guard frameCount > 0,
              let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frameCount),
              let chans = buffer.floatChannelData else {
            Foundation.exit(0)
        }
        buffer.frameLength = frameCount
        data.withUnsafeBytes { raw in
            let src = raw.bindMemory(to: Int16.self)
            for frame in 0..<Int(frameCount) {
                for c in 0..<channelCount {
                    chans[c][frame] = Float(src[frame * channelCount + c]) / 32768.0
                }
            }
        }

        let engine = AVAudioEngine()
        if let deviceID = AudioInjector.outputDeviceID(named: deviceName),
           let unit = engine.outputNode.audioUnit {
            var dev = deviceID
            let status = AudioUnitSetProperty(
                unit,
                kAudioOutputUnitProperty_CurrentDevice,
                kAudioUnitScope_Global,
                0,
                &dev,
                UInt32(MemoryLayout<AudioDeviceID>.size)
            )
            if status != noErr {
                logJson(["event": "warn", "message": "set output device failed (\(status))"])
            }
        } else {
            logJson(["event": "warn", "message": "inject device not found: \(deviceName) (using default)"])
        }

        let player = AVAudioPlayerNode()
        engine.attach(player)
        engine.connect(player, to: engine.outputNode, format: format)

        let done = DispatchSemaphore(value: 0)
        try engine.start()
        logJson(["event": "ready", "mode": "inject", "device": deviceName])
        player.scheduleBuffer(buffer, at: nil, options: []) {
            done.signal()
        }
        player.play()
        done.wait()
        // Let the device drain the tail before tearing down.
        Thread.sleep(forTimeInterval: 0.2)
        engine.stop()
        Foundation.exit(0)
    }

    /// Resolve an output device's `AudioDeviceID` by its display name.
    static func outputDeviceID(named name: String) -> AudioDeviceID? {
        var addr = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDevices,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var dataSize: UInt32 = 0
        guard AudioObjectGetPropertyDataSize(
            AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &dataSize
        ) == noErr else { return nil }
        let count = Int(dataSize) / MemoryLayout<AudioDeviceID>.size
        if count == 0 { return nil }
        var ids = [AudioDeviceID](repeating: 0, count: count)
        guard AudioObjectGetPropertyData(
            AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &dataSize, &ids
        ) == noErr else { return nil }

        for id in ids {
            var nameAddr = AudioObjectPropertyAddress(
                mSelector: kAudioObjectPropertyName,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain
            )
            var nameRef: Unmanaged<CFString>?
            var nameSize = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
            let st = withUnsafeMutablePointer(to: &nameRef) {
                AudioObjectGetPropertyData(id, &nameAddr, 0, nil, &nameSize, $0)
            }
            guard st == noErr, let cf = nameRef?.takeRetainedValue() else { continue }
            if (cf as String) != name { continue }

            var streamAddr = AudioObjectPropertyAddress(
                mSelector: kAudioDevicePropertyStreams,
                mScope: kAudioObjectPropertyScopeOutput,
                mElement: kAudioObjectPropertyElementMain
            )
            var streamSize: UInt32 = 0
            if AudioObjectGetPropertyDataSize(id, &streamAddr, 0, nil, &streamSize) == noErr,
               streamSize > 0 {
                return id
            }
        }
        return nil
    }
}

func parseOptions(_ args: [String]) -> [String: String] {
    var options: [String: String] = [:]
    var index = 0
    while index < args.count {
        let key = args[index]
        if key.hasPrefix("--") {
            let name = String(key.dropFirst(2))
            if index + 1 < args.count && !args[index + 1].hasPrefix("--") {
                options[name] = args[index + 1]
                index += 2
            } else {
                options[name] = "true"
                index += 1
            }
        } else {
            index += 1
        }
    }
    return options
}

func logJson(_ fields: [String: String]) {
    if let data = try? JSONSerialization.data(withJSONObject: fields),
       var line = String(data: data, encoding: .utf8) {
        line += "\n"
        FileHandle.standardError.write(Data(line.utf8))
    }
}
