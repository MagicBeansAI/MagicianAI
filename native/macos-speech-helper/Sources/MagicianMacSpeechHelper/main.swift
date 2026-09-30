import Foundation
import AVFoundation
import Speech

private struct SpeechOutput: Encodable {
    let transcript: String
    let model: String
    let language: String?
    let extras: [String: String]
}

private struct SpeechSynthesisOutput: Encodable {
    let audio_b64: String
    let content_type: String
    let model: String
    let voice: String?
    let message_id: String?
    let extras: [String: String]
}

private struct AuthorizationOutput: Encodable {
    let authorized: Bool
    let status: String
}

private enum SpeechHelperError: Error, CustomStringConvertible {
    case missingArgument(String)
    case unsupportedCommand(String)
    case authorizationDenied(SFSpeechRecognizerAuthorizationStatus)
    case recognizerUnavailable(String)
    case onDeviceRecognitionUnavailable(String)
    case recognitionFailed(String)
    case synthesisFailed(String)
    case timedOut

    var description: String {
        switch self {
        case .missingArgument(let name):
            return "missing argument: \(name)"
        case .unsupportedCommand(let command):
            return "unsupported command: \(command)"
        case .authorizationDenied(let status):
            return "speech recognition authorization denied: \(status.rawValue)"
        case .recognizerUnavailable(let locale):
            return "speech recognizer unavailable for locale \(locale)"
        case .onDeviceRecognitionUnavailable(let locale):
            return "on-device speech recognition unavailable for locale \(locale)"
        case .recognitionFailed(let reason):
            return "speech recognition failed: \(reason)"
        case .synthesisFailed(let reason):
            return "speech synthesis failed: \(reason)"
        case .timedOut:
            return "speech helper timed out"
        }
    }
}

@main
struct MagicianMacSpeechHelper {
    static func main() async {
        do {
            try await run()
        } catch {
            FileHandle.standardError.write(Data("\(error)\n".utf8))
            Foundation.exit(1)
        }
    }

    private static func run() async throws {
        let args = Array(CommandLine.arguments.dropFirst())
        guard let command = args.first else {
            throw SpeechHelperError.missingArgument("command")
        }
        if command == "status" {
            let status = SFSpeechRecognizer.authorizationStatus()
            let output = AuthorizationOutput(
                authorized: status == .authorized,
                status: authorizationStatusLabel(status)
            )
            try writeJsonLine(output)
            return
        }
        if command == "authorize" {
            let status = try await requestAuthorization()
            let output = AuthorizationOutput(
                authorized: status == .authorized,
                status: authorizationStatusLabel(status)
            )
            try writeJsonLine(output)
            return
        }
        if command == "synthesize" {
            let options = parseOptions(Array(args.dropFirst()))
            guard let textFile = options["text-file"], !textFile.isEmpty else {
                throw SpeechHelperError.missingArgument("--text-file")
            }
            guard let outputFile = options["output"], !outputFile.isEmpty else {
                throw SpeechHelperError.missingArgument("--output")
            }
            let textURL = URL(fileURLWithPath: textFile)
            let outputURL = URL(fileURLWithPath: outputFile)
            let text = try String(contentsOf: textURL, encoding: .utf8)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            let voice = options["voice"]?.trimmingCharacters(in: .whitespacesAndNewlines)
            let rate = options["rate"].flatMap(Double.init)
            let resolvedVoice = try await synthesizeText(
                text,
                outputURL: outputURL,
                voiceHint: voice,
                rateMultiplier: rate
            )
            let output = SpeechSynthesisOutput(
                audio_b64: "",
                content_type: "audio/wav",
                model: "av_speech_synthesizer",
                voice: resolvedVoice,
                message_id: options["message-id"],
                extras: [
                    "provider": "apple_av_speech",
                    "mode": "recorded_file",
                    "format": "wav"
                ]
            )
            try writeJsonLine(output)
            return
        }
        guard command == "transcribe" else {
            throw SpeechHelperError.unsupportedCommand(command)
        }
        let options = parseOptions(Array(args.dropFirst()))
        guard let file = options["file"], !file.isEmpty else {
            throw SpeechHelperError.missingArgument("--file")
        }
        let localeIdentifier = normalizedLocale(options["locale"])
        let transcript = try await transcribeFile(
            URL(fileURLWithPath: file),
            localeIdentifier: localeIdentifier
        )
        let output = SpeechOutput(
            transcript: transcript,
            model: "macos_speech",
            language: localeIdentifier,
            extras: [
                "provider": "apple_speech",
                "mode": "recorded_file",
                "on_device_required": "true"
            ]
        )
        try writeJsonLine(output)
    }
}

private func parseOptions(_ args: [String]) -> [String: String] {
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

private func normalizedLocale(_ value: String?) -> String {
    let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
    if trimmed.isEmpty {
        return Locale.current.identifier.replacingOccurrences(of: "_", with: "-")
    }
    if trimmed.count == 2 {
        return Locale.identifier(fromComponents: [NSLocale.Key.languageCode.rawValue: trimmed])
    }
    return trimmed.replacingOccurrences(of: "_", with: "-")
}

private func writeJsonLine<T: Encodable>(_ value: T) throws {
    let data = try JSONEncoder().encode(value)
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data("\n".utf8))
}

@discardableResult
private func requestAuthorization() async throws -> SFSpeechRecognizerAuthorizationStatus {
    let resolvedStatus = try await withTimeout(seconds: 20) {
        await withCheckedContinuation { continuation in
            SFSpeechRecognizer.requestAuthorization { status in
                continuation.resume(returning: status)
            }
        }
    }
    guard resolvedStatus == .authorized else {
        throw SpeechHelperError.authorizationDenied(resolvedStatus)
    }
    return resolvedStatus
}

private func authorizationStatusLabel(_ status: SFSpeechRecognizerAuthorizationStatus) -> String {
    switch status {
    case .notDetermined:
        return "not_determined"
    case .denied:
        return "denied"
    case .restricted:
        return "restricted"
    case .authorized:
        return "authorized"
    @unknown default:
        return "unknown"
    }
}

private func transcribeFile(_ url: URL, localeIdentifier: String) async throws -> String {
    try await requestAuthorization()
    let locale = Locale(identifier: localeIdentifier)
    guard let recognizer = SFSpeechRecognizer(locale: locale), recognizer.isAvailable else {
        throw SpeechHelperError.recognizerUnavailable(localeIdentifier)
    }
    guard recognizer.supportsOnDeviceRecognition else {
        throw SpeechHelperError.onDeviceRecognitionUnavailable(localeIdentifier)
    }
    let request = SFSpeechURLRecognitionRequest(url: url)
    request.shouldReportPartialResults = true
    request.requiresOnDeviceRecognition = true

    return try await withTimeout(seconds: 45) {
        let state = RecognitionState()
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                state.setContinuation(continuation)
                let task = recognizer.recognitionTask(with: request) { result, error in
                    if let result {
                        state.updateTranscript(result.bestTranscription.formattedString)
                        if result.isFinal {
                            state.finishWithBestTranscript()
                        }
                    }
                    if let error {
                        state.finish(error: SpeechHelperError.recognitionFailed(error.localizedDescription))
                    }
                }
                state.setTask(task)
            }
        } onCancel: {
            state.cancel()
        }
    }
}

private func synthesizeText(
    _ text: String,
    outputURL: URL,
    voiceHint: String?,
    rateMultiplier: Double?
) async throws -> String? {
    guard !text.isEmpty else {
        throw SpeechHelperError.synthesisFailed("empty synthesis text")
    }
    let synthesizer = AVSpeechSynthesizer()
    let utterance = AVSpeechUtterance(string: text)
    if let voice = resolveSpeechVoice(voiceHint) {
        utterance.voice = voice
    }
    utterance.rate = normalizedSpeechRate(rateMultiplier)
    let state = SynthesisState(outputURL: outputURL, synthesizer: synthesizer)
    try await withTimeout(seconds: synthesisTimeoutSeconds(textLength: text.count)) {
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
                state.setContinuation(continuation)
                synthesizer.write(utterance) { buffer in
                    state.handle(buffer)
                }
            }
        } onCancel: {
            state.cancel()
        }
    }
    return utterance.voice?.identifier
}

private func resolveSpeechVoice(_ hint: String?) -> AVSpeechSynthesisVoice? {
    let trimmed = hint?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
    if trimmed.isEmpty {
        return AVSpeechSynthesisVoice(language: Locale.current.identifier.replacingOccurrences(of: "_", with: "-"))
    }
    if let voice = AVSpeechSynthesisVoice(identifier: trimmed) {
        return voice
    }
    let lower = trimmed.lowercased()
    if let match = AVSpeechSynthesisVoice.speechVoices().first(where: {
        $0.name.lowercased() == lower || $0.identifier.lowercased() == lower
    }) {
        return match
    }
    if trimmed.contains("-") || trimmed.contains("_") || trimmed.count == 2 {
        let locale = normalizedLocale(trimmed)
        if let voice = AVSpeechSynthesisVoice(language: locale) {
            return voice
        }
    }
    return nil
}

private func normalizedSpeechRate(_ multiplier: Double?) -> Float {
    let multiplier = max(0.5, min(2.0, multiplier ?? 1.0))
    let raw = AVSpeechUtteranceDefaultSpeechRate * Float(multiplier)
    return max(AVSpeechUtteranceMinimumSpeechRate, min(AVSpeechUtteranceMaximumSpeechRate, raw))
}

private func synthesisTimeoutSeconds(textLength: Int) -> UInt64 {
    let estimated = UInt64(textLength / 80 + 8)
    return max(10, min(70, estimated))
}

private final class SynthesisState {
    private let outputURL: URL
    private let synthesizer: AVSpeechSynthesizer
    private let lock = NSLock()
    private var continuation: CheckedContinuation<Void, Error>?
    private var file: AVAudioFile?
    private var completed = false
    private var wroteFrames = false

    init(outputURL: URL, synthesizer: AVSpeechSynthesizer) {
        self.outputURL = outputURL
        self.synthesizer = synthesizer
    }

    func setContinuation(_ continuation: CheckedContinuation<Void, Error>) {
        lock.lock()
        self.continuation = continuation
        lock.unlock()
    }

    func handle(_ buffer: AVAudioBuffer) {
        guard let pcm = buffer as? AVAudioPCMBuffer else {
            finish(error: SpeechHelperError.synthesisFailed("non-PCM synthesis buffer"))
            return
        }
        if pcm.frameLength == 0 {
            if hasWrittenFrames() {
                finish()
            } else {
                finish(error: SpeechHelperError.synthesisFailed("no audio frames produced"))
            }
            return
        }
        do {
            try write(pcm)
        } catch {
            finish(error: error)
        }
    }

    private func write(_ pcm: AVAudioPCMBuffer) throws {
        lock.lock()
        defer { lock.unlock() }
        guard !completed else {
            return
        }
        if file == nil {
            file = try AVAudioFile(
                forWriting: outputURL,
                settings: pcm.format.settings,
                commonFormat: pcm.format.commonFormat,
                interleaved: pcm.format.isInterleaved
            )
        }
        try file?.write(from: pcm)
        wroteFrames = true
    }

    private func hasWrittenFrames() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return wroteFrames
    }

    private func finish() {
        lock.lock()
        guard !completed else {
            lock.unlock()
            return
        }
        completed = true
        file = nil
        let continuation = self.continuation
        self.continuation = nil
        lock.unlock()
        continuation?.resume()
    }

    private func finish(error: Error) {
        lock.lock()
        guard !completed else {
            lock.unlock()
            return
        }
        completed = true
        file = nil
        let continuation = self.continuation
        self.continuation = nil
        lock.unlock()
        synthesizer.stopSpeaking(at: .immediate)
        continuation?.resume(throwing: error)
    }

    func cancel() {
        finish(error: SpeechHelperError.timedOut)
    }
}

private final class RecognitionState {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<String, Error>?
    private var task: SFSpeechRecognitionTask?
    private var bestTranscript: String = ""
    private var completed = false

    func setContinuation(_ continuation: CheckedContinuation<String, Error>) {
        lock.lock()
        self.continuation = continuation
        lock.unlock()
    }

    func setTask(_ task: SFSpeechRecognitionTask) {
        lock.lock()
        self.task = task
        if completed {
            task.cancel()
        }
        lock.unlock()
    }

    func updateTranscript(_ transcript: String) {
        lock.lock()
        bestTranscript = transcript.trimmingCharacters(in: .whitespacesAndNewlines)
        lock.unlock()
    }

    func finishWithBestTranscript() {
        finish(result: bestTranscript)
    }

    func finish(result: String) {
        lock.lock()
        guard !completed else {
            lock.unlock()
            return
        }
        completed = true
        let continuation = self.continuation
        self.continuation = nil
        lock.unlock()
        continuation?.resume(returning: result)
    }

    func finish(error: Error) {
        lock.lock()
        guard !completed else {
            lock.unlock()
            return
        }
        completed = true
        let continuation = self.continuation
        self.continuation = nil
        lock.unlock()
        continuation?.resume(throwing: error)
    }

    func cancel() {
        lock.lock()
        let task = self.task
        guard !completed else {
            lock.unlock()
            task?.cancel()
            return
        }
        completed = true
        let continuation = self.continuation
        self.continuation = nil
        lock.unlock()
        task?.cancel()
        continuation?.resume(throwing: SpeechHelperError.timedOut)
    }
}

private func withTimeout<T>(
    seconds: UInt64,
    operation: @escaping () async throws -> T
) async throws -> T {
    try await withThrowingTaskGroup(of: T.self) { group in
        group.addTask {
            try await operation()
        }
        group.addTask {
            try await Task.sleep(nanoseconds: seconds * 1_000_000_000)
            throw SpeechHelperError.timedOut
        }
        defer {
            group.cancelAll()
        }
        guard let result = try await group.next() else {
            throw SpeechHelperError.timedOut
        }
        return result
    }
}
