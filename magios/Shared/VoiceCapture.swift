import Foundation

/// Pure helpers for the voice-first composer: whether a new capture should interrupt
/// speaking TTS (barge-in), and how a finished transcript merges into the composer.
enum VoiceCapture {
    static func shouldBargeIn(isSpeaking: Bool) -> Bool { isSpeaking }

    static func merge(existing: String, transcript: String) -> String {
        existing.trimmingCharacters(in: .whitespaces).isEmpty
            ? transcript
            : existing + " " + transcript
    }
}
