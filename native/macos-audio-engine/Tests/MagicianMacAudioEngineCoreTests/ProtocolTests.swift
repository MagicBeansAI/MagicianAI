import Foundation
@testable import MagicianMacAudioEngineCore
import XCTest

final class ProtocolTests: XCTestCase {
    func testStreamEventsUseVersionedSnakeCaseWireShape() throws {
        let data = try JSONEncoder().encode(AudioEngineStreamEvent.speechStarted(atMs: 125))
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(object["type"] as? String, "speech_started")
        XCTAssertEqual(object["at_ms"] as? Int, 125)
    }

    func testStartControlDecodesRustWireShape() throws {
        let json = #"{"type":"start","protocol_version":1,"stage":"vad","model_id":"fluid-silero-v6","format":{"sample_rate_hz":48000,"channels":2,"sample_format":"pcm_s16_le"},"config":{"threshold":0.65,"min_speech_ms":250,"min_silence_ms":500,"pre_roll_ms":400,"hangover_ms":600,"max_utterance_ms":120000,"gate_only":true}}"#
        let control = try JSONDecoder().decode(StreamStartControl.self, from: Data(json.utf8))
        XCTAssertEqual(control.protocolVersion, audioEngineProtocolVersion)
        XCTAssertEqual(control.format.sampleRateHz, 48_000)
        XCTAssertEqual(control.format.channels, 2)
        XCTAssertEqual(try control.config.vad().hangoverMs, 600)
    }

    func testStreamingAndDiarizationEventsUseNormalizedWireShapes() throws {
        let final = try JSONEncoder().encode(AudioEngineStreamEvent.transcriptFinal(
            text: "hello",
            turnId: "turn-1",
            language: "en",
            startMs: 320
        ))
        let finalObject = try XCTUnwrap(JSONSerialization.jsonObject(with: final) as? [String: Any])
        XCTAssertEqual(finalObject["type"] as? String, "transcript_final")
        XCTAssertEqual(finalObject["turn_id"] as? String, "turn-1")

        let segment = try JSONEncoder().encode(AudioEngineStreamEvent.segmentRevised(
            speakerId: "speaker_1",
            startMs: 80,
            endMs: 640,
            confidence: 0.9
        ))
        let segmentObject = try XCTUnwrap(JSONSerialization.jsonObject(with: segment) as? [String: Any])
        XCTAssertEqual(segmentObject["type"] as? String, "segment_revised")
        XCTAssertEqual(segmentObject["speaker_id"] as? String, "speaker_1")
    }

    func testSpeechSynthesisRequestUsesOpenAiCompatibleFieldNames() throws {
        let request = SpeechSynthesisRequest(
            input: "Status update ready.",
            voice: "af_heart",
            responseFormat: "wav",
            speed: 0.9
        )
        let data = try JSONEncoder().encode(request)
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(object["input"] as? String, "Status update ready.")
        XCTAssertEqual(object["voice"] as? String, "af_heart")
        XCTAssertEqual(object["response_format"] as? String, "wav")
        XCTAssertEqual(try XCTUnwrap(object["speed"] as? Double), 0.9, accuracy: 0.001)
    }
}
