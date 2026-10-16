import XCTest
@testable import Magician

final class VoiceCaptureTests: XCTestCase {
    func testBargeInStopsWhenSpeaking() {
        XCTAssertTrue(VoiceCapture.shouldBargeIn(isSpeaking: true))
    }
    func testNoBargeInWhenSilent() {
        XCTAssertFalse(VoiceCapture.shouldBargeIn(isSpeaking: false))
    }
    func testMergeTranscriptAppendsWithSpace() {
        XCTAssertEqual(VoiceCapture.merge(existing: "hello", transcript: "world"), "hello world")
        XCTAssertEqual(VoiceCapture.merge(existing: "  ", transcript: "world"), "world")
    }
}
