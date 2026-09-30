import XCTest
import AVFoundation
@testable import Magician

final class DictationDecisionTests: XCTestCase {
    func testUsesLiveWhenOnDevicePreferredAndAvailable() {
        XCTAssertTrue(DictationController.shouldUseLive(prefersOnDevice: true, onDeviceAvailable: true))
    }
    func testFallsBackWhenOnDeviceUnavailable() {
        XCTAssertFalse(DictationController.shouldUseLive(prefersOnDevice: true, onDeviceAvailable: false))
    }
    func testFallsBackWhenCloudForced() {
        XCTAssertFalse(DictationController.shouldUseLive(prefersOnDevice: false, onDeviceAvailable: true))
    }

    func testQuickMicPressTogglesDictation() {
        XCTAssertEqual(
            MicInteractionPolicy.releaseAction(
                holdActive: false,
                recordingAtPressStart: false
            ),
            .startTapDictation
        )
        XCTAssertEqual(
            MicInteractionPolicy.releaseAction(
                holdActive: false,
                recordingAtPressStart: true
            ),
            .finishTapDictation
        )
    }

    func testDeliberateMicHoldStartsAndFinishesHoldCapture() {
        XCTAssertEqual(
            MicInteractionPolicy.delayedPressAction(
                pressInProgress: true,
                recordingAtPressStart: false,
                holdActive: false
            ),
            .startHold
        )
        XCTAssertEqual(
            MicInteractionPolicy.releaseAction(
                holdActive: true,
                recordingAtPressStart: false
            ),
            .finishHold
        )
    }

    func testTapStartedRecordingCannotBeReenteredAsHoldWhenStopping() {
        XCTAssertTrue(MicInteractionPolicy.usesDirectStop(
            isRecording: true,
            holdActive: false
        ))
        XCTAssertEqual(
            MicInteractionPolicy.delayedPressAction(
                pressInProgress: true,
                recordingAtPressStart: true,
                holdActive: false
            ),
            .none
        )
        XCTAssertEqual(
            MicInteractionPolicy.releaseAction(
                holdActive: false,
                recordingAtPressStart: true
            ),
            .finishTapDictation
        )
    }

    func testCancelledOrAlreadyActivePressDoesNotStartAnotherHold() {
        XCTAssertFalse(MicInteractionPolicy.usesDirectStop(
            isRecording: true,
            holdActive: true
        ))
        XCTAssertEqual(
            MicInteractionPolicy.delayedPressAction(
                pressInProgress: false,
                recordingAtPressStart: false,
                holdActive: false
            ),
            .none
        )
        XCTAssertEqual(
            MicInteractionPolicy.delayedPressAction(
                pressInProgress: true,
                recordingAtPressStart: false,
                holdActive: true
            ),
            .none
        )
    }

    func testDictationFinalizesTheCaptureRouteThatActuallyStarted() {
        XCTAssertEqual(DictationCaptureRoute.idle.finishAction, .none)
        XCTAssertEqual(DictationCaptureRoute.file.finishAction, .transcribeFile)
        XCTAssertEqual(DictationCaptureRoute.live.finishAction, .finalizeLive)
    }

    func testTranscriptionRouteNeverUploadsWhenOnDeviceOnlyIsUnavailable() {
        XCTAssertEqual(
            DictationController.transcriptionRoute(
                source: .onDevice,
                onDeviceAvailable: false
            ),
            .unavailable
        )
        XCTAssertEqual(
            DictationController.transcriptionRoute(
                source: .onDevice,
                onDeviceAvailable: true
            ),
            .onDevice(allowsCloudFallback: false)
        )
    }

    func testTranscriptionRouteUsesCloudOnlyWhenTheSelectedPolicyAllowsIt() {
        XCTAssertEqual(
            DictationController.transcriptionRoute(
                source: .auto,
                onDeviceAvailable: true
            ),
            .onDevice(allowsCloudFallback: true)
        )
        XCTAssertEqual(
            DictationController.transcriptionRoute(
                source: .auto,
                onDeviceAvailable: false
            ),
            .cloud
        )
        XCTAssertEqual(
            DictationController.transcriptionRoute(
                source: .cloud,
                onDeviceAvailable: true
            ),
            .cloud
        )
    }

    func testTranscriptionLeaseRejectsCallbacksAfterCancellation() {
        let url = URL(fileURLWithPath: "/tmp/ambient-dictation-one.m4a")
        var lease = DictationTranscriptionLease()
        let generation = lease.begin(fileURL: url)

        XCTAssertTrue(lease.accepts(generation))
        XCTAssertTrue(lease.beginOnDevice(generation))
        XCTAssertEqual(lease.cancel(), url)
        XCTAssertFalse(lease.accepts(generation))
        XCTAssertNil(lease.finish(generation))
    }

    func testTranscriptionLeaseTransfersFileOwnershipOnlyOnce() {
        let url = URL(fileURLWithPath: "/tmp/ambient-dictation-two.m4a")
        var lease = DictationTranscriptionLease()
        let generation = lease.begin(fileURL: url)

        XCTAssertEqual(lease.beginCloud(generation), url)
        XCTAssertTrue(lease.isCloudActive(generation))
        XCTAssertNil(lease.beginCloud(generation))
        XCTAssertNil(lease.finish(generation))
        XCTAssertFalse(lease.accepts(generation))
    }

    func testTranscriptionLeaseClaimsOnlyOneOnDeviceTerminalCallback() {
        let url = URL(fileURLWithPath: "/tmp/ambient-dictation-three.m4a")
        var lease = DictationTranscriptionLease()
        let generation = lease.begin(fileURL: url)

        XCTAssertTrue(lease.beginOnDevice(generation))
        XCTAssertTrue(lease.claimOnDeviceResult(generation))
        XCTAssertFalse(lease.claimOnDeviceResult(generation))
        XCTAssertEqual(lease.beginCloud(generation), url)
        XCTAssertNil(lease.beginCloud(generation))
    }

    func testAmbientFollowUpReopensTheRetainedInputGraphInsteadOfCreatingAnother() {
        XCTAssertEqual(
            DictationController.ambientStartAction(
                captureRoute: .file,
                retainedInputGraph: true,
                isRecording: false,
                isTranscribing: false
            ),
            .resumeRetainedInputGraph
        )
        XCTAssertEqual(
            DictationController.ambientStartAction(
                captureRoute: .idle,
                retainedInputGraph: false,
                isRecording: false,
                isTranscribing: false
            ),
            .createInputGraph
        )
    }

    func testAmbientInputGraphCannotReopenDuringCaptureOrTranscription() {
        for state in [(true, false), (false, true), (true, true)] {
            XCTAssertEqual(
                DictationController.ambientStartAction(
                    captureRoute: .file,
                    retainedInputGraph: true,
                    isRecording: state.0,
                    isTranscribing: state.1
                ),
                .reject
            )
        }
    }

    func testAmbientAccumulatorGatesAndIsolatesEachTurn() {
        let accumulator = AmbientDictationPCMAccumulator()
        let first = Data([1, 0, 2, 0])
        let second = Data([3, 0, 4, 0])

        XCTAssertFalse(accumulator.append(first))
        XCTAssertFalse(accumulator.hasFrames)
        XCTAssertNil(accumulator.finishTurn(), "frames outside a listening turn are discarded")

        accumulator.beginTurn()
        XCTAssertFalse(accumulator.hasFrames)
        XCTAssertTrue(accumulator.append(first), "the first live frame admits the input graph")
        XCTAssertTrue(accumulator.hasFrames)
        XCTAssertFalse(accumulator.append(first), "later frames must not re-fire first-frame admission")
        XCTAssertEqual(accumulator.finishTurn()?.pcm16LE, first + first)
        XCTAssertFalse(accumulator.hasFrames)
        XCTAssertFalse(accumulator.append(second))

        accumulator.beginTurn()
        XCTAssertTrue(accumulator.append(second))
        XCTAssertEqual(
            accumulator.finishTurn()?.pcm16LE,
            second,
            "a later turn must not contain any PCM from the earlier turn or the closed gap"
        )
    }

    func testAmbientWavAndOrdinaryM4AUseTruthfulMultipartMetadata() {
        XCTAssertEqual(
            DictationAudioUpload.file(at: URL(fileURLWithPath: "/tmp/segment.wav")),
            DictationAudioUpload(filename: "dictation.wav", contentType: "audio/wav")
        )
        XCTAssertEqual(
            DictationAudioUpload.file(at: URL(fileURLWithPath: "/tmp/composer.m4a")),
            DictationAudioUpload(filename: "dictation.m4a", contentType: "audio/m4a")
        )
    }

    func testAmbientPCMSnapshotWritesACompleteReadableWAV() throws {
        let directory = FileManager.default.temporaryDirectory
        let segmentURL = directory.appendingPathComponent("ambient-segment-\(UUID()).wav")
        defer {
            try? FileManager.default.removeItem(at: segmentURL)
        }
        var pcm = Data()
        for index in 0..<75 {
            var sample = Int16(index * 100).littleEndian
            withUnsafeBytes(of: &sample) { pcm.append(contentsOf: $0) }
        }
        try AmbientDictationPCMSnapshot(pcm16LE: pcm).wavData().write(to: segmentURL)

        let segment = try AVAudioFile(forReading: segmentURL)
        XCTAssertEqual(segment.length, 75)
        XCTAssertEqual(segment.processingFormat.sampleRate, 16_000)
        XCTAssertEqual(segment.processingFormat.channelCount, 1)
    }

    @MainActor
    func testAmbientCaptureStartCompletionIsOneShotWhenTestsBlockTheMicrophone() {
        let controller = DictationController()
        var starts: [Bool] = []

        controller.startAmbientCapture(
            onSpeechBegan: {},
            onResult: { _ in },
            onStarted: { starts.append($0) }
        )

        XCTAssertEqual(starts, [false])
    }

    // MARK: - the ambient rail

    /// **The primary rail: starting dictation ends an armed listening window.**
    ///
    /// A user holding their phone and pressing hold-to-talk wants dictation, so
    /// ambient yields rather than refusing — and it yields at the single public
    /// entry, before any of the three session deactivations downstream of it. The
    /// reason travels, because the orb's last frame is the only place the user will
    /// see what closed their window.
    @MainActor
    func testStartingDictationYieldsAnArmedAmbientWindow() {
        let controller = DictationController()
        var yielded: [String] = []
        controller.ambientRail = AmbientRail(
            windowIsLive: { true },
            yield: { yielded.append($0) }
        )

        controller.startLive()

        XCTAssertEqual(yielded, [AmbientYieldReason.dictationStarted])
    }

    @MainActor
    func testStartingDurableVoiceNoteYieldsAnArmedAmbientWindow() {
        let controller = DictationController()
        var yielded: [String] = []
        controller.ambientRail = AmbientRail(
            windowIsLive: { true },
            yield: { yielded.append($0) }
        )

        controller.startVoiceNote()

        XCTAssertEqual(yielded, [AmbientYieldReason.dictationStarted])
    }

    func testAudioNoteMultipartUsesScopedDefaultAndCarriesDateTranscriptAndRecording() {
        let record = AudioNoteOutboxRecord(
            id: "a1b2c3d4-1111-4222-8333-123456789abc",
            capturedAt: "2026-08-01T20:56:24.147+05:30",
            durationMS: 4_250,
            sourceSurface: "ios_voice_note",
            audioFilename: "voice-1.m4a",
            transcript: "Remember this idea.",
            ready: true
        )

        let body = AudioNoteUploadQueue.multipartBody(
            record: record,
            audio: Data("recorded-audio".utf8),
            boundary: "test-boundary"
        )
        let rendered = String(decoding: body, as: UTF8.self)

        XCTAssertFalse(rendered.contains("name=\"provider\""))
        XCTAssertTrue(rendered.contains("name=\"note_id\"\r\n\r\na1b2c3d4-1111-4222-8333-123456789abc"))
        XCTAssertTrue(rendered.contains("name=\"captured_at\"\r\n\r\n2026-08-01T20:56:24.147+05:30"))
        XCTAssertTrue(rendered.contains("name=\"source_surface\"\r\n\r\nios_voice_note"))
        XCTAssertTrue(rendered.contains("name=\"duration_ms\"\r\n\r\n4250"))
        XCTAssertTrue(rendered.contains("name=\"transcript\"\r\n\r\nRemember this idea."))
        XCTAssertTrue(rendered.contains("filename=\"voice-note.m4a\""))
        XCTAssertTrue(rendered.contains("recorded-audio"))
        XCTAssertTrue(rendered.hasSuffix("--test-boundary--\r\n"))
    }

    func testAudioNoteMultipartCarriesExplicitProviderOnlyWhenCaptured() {
        var record = AudioNoteOutboxRecord(
            id: "a1b2c3d4-1111-4222-8333-123456789abc",
            capturedAt: "2026-08-01T20:56:24.147+05:30",
            durationMS: nil,
            sourceSurface: "ios_voice_note",
            audioFilename: "voice-1.m4a",
            transcript: nil,
            ready: true
        )
        record.provider = "local_markdown"
        let rendered = String(decoding: AudioNoteUploadQueue.multipartBody(
            record: record,
            audio: Data("audio".utf8),
            boundary: "provider-boundary"
        ), as: UTF8.self)

        XCTAssertTrue(rendered.contains("name=\"provider\"\r\n\r\nlocal_markdown"))
    }

    func testAudioNoteUploadRequiresMatchingDurableReceiptBeforeDeletion() throws {
        let valid = try JSONSerialization.data(withJSONObject: [
            "note_id": "a1b2c3d4-1111-4222-8333-123456789abc",
            "provider": "local_markdown",
            "captured_at": "2026-08-01T20:56:24.147+05:30",
            "note_path": "Audio Notes/2026-08-01/note.md",
            "audio_path": "Audio Notes/2026-08-01/note.m4a",
            "bytes": 14
        ])
        let outcome = AudioNoteUploadQueue.uploadOutcome(
            recordID: "a1b2c3d4-1111-4222-8333-123456789abc",
            expectedCapturedAt: "2026-08-01T20:56:24.147+05:30",
            expectedBytes: 14,
            data: valid,
            status: 201,
            error: nil
        )
        guard case .success(let receipt) = outcome else {
            return XCTFail("matching receipt must acknowledge durable save")
        }
        XCTAssertEqual(receipt.bytes, 14)

        XCTAssertEqual(
            AudioNoteUploadQueue.uploadOutcome(
                recordID: "a1b2c3d4-1111-4222-8333-123456789abc",
                expectedCapturedAt: "2026-08-01T20:56:24.147+05:30",
                expectedBytes: 14,
                data: Data("<html>Access login</html>".utf8),
                status: 200,
                error: nil
            ),
            .retry("The server did not return a matching durable Audio Note receipt.")
        )
    }

    func testAudioNoteReceiptAcceptsEquivalentCanonicalCaptureTimestamp() throws {
        let recordID = "a1b2c3d4-1111-4222-8333-123456789abc"
        let response = try JSONSerialization.data(withJSONObject: [
            "note_id": recordID,
            "provider": "local_markdown",
            "captured_at": "2026-08-01T20:56:24+05:30",
            "note_path": "Audio Notes/note.md",
            "audio_path": "Audio Notes/note.m4a",
            "bytes": 14
        ])

        guard case .success = AudioNoteUploadQueue.uploadOutcome(
            recordID: recordID,
            expectedCapturedAt: "2026-08-01T20:56:24.000+05:30",
            expectedBytes: 14,
            data: response,
            status: 201,
            error: nil
        ) else {
            return XCTFail("equivalent RFC 3339 precision must not strand a saved recording")
        }
    }

    func testAudioNoteUploadClassifiesRetryableAndPermanentFailures() {
        XCTAssertEqual(
            AudioNoteUploadQueue.uploadOutcome(
                recordID: UUID().uuidString,
                expectedCapturedAt: "2026-08-01T20:56:24.147+05:30",
                expectedBytes: 1,
                data: Data(),
                status: 503,
                error: nil
            ),
            .retry("The Audio Notes service returned HTTP 503.")
        )
        XCTAssertEqual(
            AudioNoteUploadQueue.uploadOutcome(
                recordID: UUID().uuidString,
                expectedCapturedAt: "2026-08-01T20:56:24.147+05:30",
                expectedBytes: 1,
                data: Data(),
                status: 413,
                error: nil
            ),
            .permanentFailure("Upload was rejected with HTTP 413.")
        )
    }

    func testFailedOrBackedOffOldestRecordCannotBlockLaterAudioNote() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        var failed = AudioNoteOutboxRecord(
            id: "11111111-1111-4111-8111-111111111111",
            capturedAt: "2026-08-01T10:00:00.000+05:30",
            durationMS: nil,
            sourceSurface: "ios_voice_note",
            audioFilename: "one.m4a",
            transcript: nil,
            ready: true
        )
        failed.failedPermanently = true
        var backedOff = AudioNoteOutboxRecord(
            id: "22222222-2222-4222-8222-222222222222",
            capturedAt: "2026-08-02T10:00:00.000+05:30",
            durationMS: nil,
            sourceSurface: "ios_voice_note",
            audioFilename: "two.m4a",
            transcript: nil,
            ready: true
        )
        backedOff.nextAttemptAt = now.addingTimeInterval(60)
        let ready = AudioNoteOutboxRecord(
            id: "33333333-3333-4333-8333-333333333333",
            capturedAt: "2026-08-03T10:00:00.000+05:30",
            durationMS: nil,
            sourceSurface: "ios_voice_note",
            audioFilename: "three.m4a",
            transcript: nil,
            ready: true
        )

        XCTAssertEqual(
            AudioNoteUploadQueue.nextEligibleRecord(
                from: [failed, backedOff, ready],
                now: now
            )?.id,
            ready.id
        )
    }

    func testLegacyAudioNoteOutboxRecordDecodesIntoPerRecordRetryState() throws {
        let legacy = Data(#"{"id":"a1b2c3d4-1111-4222-8333-123456789abc","capturedAt":"2026-08-01T20:56:24.147+05:30","durationMS":4250,"sourceSurface":"ios_voice_note","audioFilename":"voice.m4a","transcript":"idea","ready":true}"#.utf8)
        let decoded = try JSONDecoder().decode(AudioNoteOutboxRecord.self, from: legacy)

        XCTAssertNil(decoded.destinationBaseURL)
        XCTAssertNil(decoded.attemptCount)
        XCTAssertNil(decoded.failedPermanently)
        XCTAssertTrue(decoded.ready)
    }

    func testAudioNoteOutboxRejectsPathTraversalAndUntrustedDestinations() {
        let id = "a1b2c3d4-1111-4222-8333-123456789abc"
        let metadataURL = URL(fileURLWithPath: "/tmp/\(id).json")
        let trustedBaseURL = URL(string: "https://ios.example.com")!
        let safe = AudioNoteOutboxRecord(
            id: id,
            capturedAt: "2026-08-01T20:56:24.147+05:30",
            durationMS: nil,
            sourceSurface: "ios_voice_note",
            audioFilename: "\(id).m4a",
            transcript: nil,
            ready: true,
            destinationBaseURL: "https://ios.example.com",
            principal: "anonymous",
            workspace: "default"
        )
        XCTAssertTrue(AudioNoteUploadQueue.recordIsSafe(
            safe,
            metadataURL: metadataURL,
            trustedBaseURL: trustedBaseURL
        ))

        let traversal = AudioNoteOutboxRecord(
            id: id,
            capturedAt: safe.capturedAt,
            durationMS: nil,
            sourceSurface: safe.sourceSurface,
            audioFilename: "../private.m4a",
            transcript: nil,
            ready: true,
            destinationBaseURL: safe.destinationBaseURL,
            principal: safe.principal,
            workspace: safe.workspace
        )
        XCTAssertFalse(AudioNoteUploadQueue.recordIsSafe(
            traversal,
            metadataURL: metadataURL,
            trustedBaseURL: trustedBaseURL
        ))

        var untrusted = safe
        untrusted.destinationBaseURL = "http://attacker.example"
        XCTAssertFalse(AudioNoteUploadQueue.recordIsSafe(
            untrusted,
            metadataURL: metadataURL,
            trustedBaseURL: trustedBaseURL
        ))
        untrusted.destinationBaseURL = "https://attacker.example"
        XCTAssertFalse(AudioNoteUploadQueue.recordIsSafe(
            untrusted,
            metadataURL: metadataURL,
            trustedBaseURL: trustedBaseURL
        ))
        untrusted.destinationBaseURL = "https://ios.example.com/untrusted-prefix"
        XCTAssertFalse(AudioNoteUploadQueue.recordIsSafe(
            untrusted,
            metadataURL: metadataURL,
            trustedBaseURL: trustedBaseURL
        ))
        untrusted.destinationBaseURL = "https://ios.example.com?redirect=attacker"
        XCTAssertFalse(AudioNoteUploadQueue.recordIsSafe(
            untrusted,
            metadataURL: metadataURL,
            trustedBaseURL: trustedBaseURL
        ))
    }

    func testAudioNoteCapacityReservesMultipartCopyAndMetadata() {
        XCTAssertEqual(
            AudioNoteUploadQueue.projectedOutboxBytes(
                allocated: 10,
                pendingMultipartReserve: 20,
                incomingBytes: 30
            ),
            10 + 20 + 60 + AudioNoteUploadQueue.maximumPerRecordAuxiliaryBytes
        )
    }

    func testActiveAudioNoteCannotBeDiscardedWhileUploadOrRetryIsInFlight() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        var record = AudioNoteOutboxRecord(
            id: "a1b2c3d4-1111-4222-8333-123456789abc",
            capturedAt: "2026-08-01T20:56:24.147+05:30",
            durationMS: nil,
            sourceSurface: "ios_voice_note",
            audioFilename: "a1b2c3d4-1111-4222-8333-123456789abc.m4a",
            transcript: nil,
            ready: true
        )
        XCTAssertEqual(
            AudioNoteUploadQueue.outboxStatus(for: record, active: true, now: now).state,
            "Uploading"
        )
        XCTAssertFalse(
            AudioNoteUploadQueue.outboxStatus(for: record, active: true, now: now).canDiscard
        )
        record.nextAttemptAt = now.addingTimeInterval(60)
        XCTAssertEqual(
            AudioNoteUploadQueue.outboxStatus(for: record, active: true, now: now).state,
            "Retry scheduled"
        )
        XCTAssertTrue(
            AudioNoteUploadQueue.outboxStatus(for: record, active: false, now: now).canDiscard
        )
    }

    func testAudioNoteBackgroundRestorationRejectsOrphansAndDuplicateRecordTasks() {
        let firstRecord = "11111111-1111-4111-8111-111111111111"
        let secondRecord = "22222222-2222-4222-8222-222222222222"
        let plan = AudioNoteUploadQueue.restoredTaskPlan(
            [
                (taskID: 1, recordID: firstRecord),
                (taskID: 2, recordID: firstRecord),
                (taskID: 3, recordID: secondRecord),
                (taskID: 4, recordID: "33333333-3333-4333-8333-333333333333"),
                (taskID: 5, recordID: "not-a-uuid"),
                (taskID: 6, recordID: nil)
            ],
            validRecordIDs: [firstRecord, secondRecord]
        )

        XCTAssertEqual(plan.acceptedTaskIDs, Set([1, 3]))
        XCTAssertEqual(plan.acceptedRecordIDs, Set([firstRecord, secondRecord]))
        XCTAssertEqual(plan.canceledTaskIDs, Set([2, 4, 5, 6]))
    }

    func testAudioNoteStagingCapturesScopeAndProtectsFilesOffTheCallerThread() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("audio-note-queue-test-\(UUID().uuidString)", isDirectory: true)
        let source = root.appendingPathComponent("source.m4a")
        let outbox = root.appendingPathComponent("outbox", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        try Data("recorded-audio".utf8).write(to: source)
        defer { try? FileManager.default.removeItem(at: root) }
        let queue = AudioNoteUploadQueue(directory: outbox, networkingEnabled: false)
        let staged = expectation(description: "recording staged")
        var stagedID: String?

        queue.stageRecording(at: source, capturedAt: Date(timeIntervalSince1970: 1_700_000_000), durationMS: 500) { result in
            if case .success(let id) = result { stagedID = id }
            if case .failure(let error) = result { XCTFail(error.localizedDescription) }
            staged.fulfill()
        }
        wait(for: [staged], timeout: 2)

        let id = try XCTUnwrap(stagedID)
        let metadataURL = outbox.appendingPathComponent("\(id).json")
        let record = try JSONDecoder().decode(
            AudioNoteOutboxRecord.self,
            from: Data(contentsOf: metadataURL)
        )
        XCTAssertEqual(record.destinationBaseURL, MagicianAccess.baseURL.absoluteString)
        XCTAssertEqual(record.principal, MagicianAccess.principal)
        XCTAssertEqual(record.workspace, MagicianAccess.workspace)
        XCTAssertNil(record.provider, "nil means use the scoped Notes default")
        XCTAssertFalse(record.ready)
        XCTAssertEqual(
            try metadataURL.resourceValues(forKeys: [.isExcludedFromBackupKey]).isExcludedFromBackup,
            true
        )
        let protection = try FileManager.default.attributesOfItem(
            atPath: outbox.appendingPathComponent(record.audioFilename).path
        )[.protectionKey] as? FileProtectionType
        XCTAssertEqual(AudioNoteUploadQueue.fileProtectionType, .complete)
#if targetEnvironment(simulator)
        // The simulator stores files on the macOS host filesystem, which accepts
        // NSFileProtection attributes but does not report them back. The shared
        // production constant above keeps this gate coupled to protectFile(_:).
        XCTAssertNil(protection)
#else
        XCTAssertEqual(protection, .complete)
#endif
    }

    func testOversizedAudioNoteIsRejectedBeforeEnteringOutbox() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("audio-note-limit-test-\(UUID().uuidString)", isDirectory: true)
        let source = root.appendingPathComponent("oversized.m4a")
        let outbox = root.appendingPathComponent("outbox", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        XCTAssertTrue(FileManager.default.createFile(atPath: source.path, contents: Data()))
        let file = try FileHandle(forWritingTo: source)
        try file.truncate(atOffset: UInt64(AudioNoteUploadQueue.maximumRecordingBytes + 1))
        try file.close()
        defer { try? FileManager.default.removeItem(at: root) }
        let queue = AudioNoteUploadQueue(directory: outbox, networkingEnabled: false)
        let rejected = expectation(description: "oversized recording rejected")

        queue.stageRecording(at: source, capturedAt: Date(), durationMS: nil) { result in
            guard case .failure(let error) = result else {
                return XCTFail("oversized recording must not be staged")
            }
            XCTAssertEqual(error.localizedDescription, AudioNoteOutboxError.recordingTooLarge.localizedDescription)
            rejected.fulfill()
        }
        wait(for: [rejected], timeout: 2)
        XCTAssertEqual(
            (try? FileManager.default.contentsOfDirectory(at: outbox, includingPropertiesForKeys: nil))?.filter { $0.pathExtension == "json" }.count,
            0
        )
    }

    func testBrokenOutboxMetadataRecoversItsRecordingAsAudioOnly() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("audio-note-recovery-test-\(UUID().uuidString)", isDirectory: true)
        let outbox = root.appendingPathComponent("outbox", isDirectory: true)
        try FileManager.default.createDirectory(at: outbox, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let brokenID = "a1b2c3d4-1111-4222-8333-123456789abc"
        try Data("not-json".utf8).write(to: outbox.appendingPathComponent("\(brokenID).json"))
        try Data("recover-me".utf8).write(to: outbox.appendingPathComponent("\(brokenID).m4a"))
        let trigger = root.appendingPathComponent("trigger.m4a")
        try Data("trigger".utf8).write(to: trigger)
        let queue = AudioNoteUploadQueue(directory: outbox, networkingEnabled: false)
        let staged = expectation(description: "capacity scan recovers broken metadata")

        queue.stageRecording(at: trigger, capturedAt: Date(), durationMS: nil) { _ in
            staged.fulfill()
        }
        wait(for: [staged], timeout: 2)

        let recovered = try JSONDecoder().decode(
            AudioNoteOutboxRecord.self,
            from: Data(contentsOf: outbox.appendingPathComponent("\(brokenID).json"))
        )
        XCTAssertEqual(recovered.sourceSurface, "ios_voice_note_recovered")
        XCTAssertTrue(recovered.ready)
        XCTAssertNil(recovered.transcript)
        XCTAssertTrue(
            FileManager.default.fileExists(atPath: outbox.appendingPathComponent("\(brokenID).m4a").path)
        )
    }

    func testUnsafeOutboxMetadataQuarantinesEveryPrivateArtifact() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("audio-note-quarantine-test-\(UUID().uuidString)", isDirectory: true)
        let outbox = root.appendingPathComponent("outbox", isDirectory: true)
        try FileManager.default.createDirectory(at: outbox, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let unsafeID = "a1b2c3d4-1111-4222-8333-123456789abc"
        let unsafe = AudioNoteOutboxRecord(
            id: unsafeID,
            capturedAt: "2026-08-01T20:56:24.147+05:30",
            durationMS: nil,
            sourceSurface: "ios_voice_note",
            audioFilename: "\(unsafeID).m4a",
            transcript: "private",
            ready: true,
            destinationBaseURL: "https://attacker.example",
            principal: "anonymous",
            workspace: "default"
        )
        try JSONEncoder().encode(unsafe).write(
            to: outbox.appendingPathComponent("\(unsafeID).json")
        )
        try Data("private-audio".utf8).write(
            to: outbox.appendingPathComponent("\(unsafeID).m4a")
        )
        try Data("private-multipart".utf8).write(
            to: outbox.appendingPathComponent("\(unsafeID).upload")
        )
        let trigger = root.appendingPathComponent("trigger.m4a")
        try Data("trigger".utf8).write(to: trigger)
        let queue = AudioNoteUploadQueue(directory: outbox, networkingEnabled: false)
        let staged = expectation(description: "capacity scan quarantines unsafe metadata")

        queue.stageRecording(at: trigger, capturedAt: Date(), durationMS: nil) { _ in
            staged.fulfill()
        }
        wait(for: [staged], timeout: 2)

        for pathExtension in ["json", "m4a", "upload"] {
            XCTAssertFalse(FileManager.default.fileExists(
                atPath: outbox.appendingPathComponent("\(unsafeID).\(pathExtension)").path
            ))
        }
        let quarantined = try FileManager.default.contentsOfDirectory(
            at: outbox.appendingPathComponent("Quarantine", isDirectory: true),
            includingPropertiesForKeys: nil
        )
        XCTAssertEqual(Set(quarantined.map(\.pathExtension)), Set(["json", "m4a", "upload"]))
    }

    /// **The structural backstop underneath it**, and the one §15's lesson says has
    /// to exist: *"a deactivation is only safe if something structurally prevents the
    /// call, not if a survey once concluded the path was unreachable."* This is the
    /// highest-traffic rail in the set — every dictation start and finish reaches it.
    ///
    /// An armed window's session cannot be reactivated from the background (Apple DTS
    /// 826462), so a deactivation here does not fail at this line: it fails at the
    /// next wake word, off screen, with the orb still saying the user is being heard.
    @MainActor
    func testDictationNeverDeactivatesTheSessionUnderAnArmedWindow() {
        let controller = DictationController()
        controller.ambientRail = AmbientRail(windowIsLive: { true }, yield: { _ in })

        controller.releaseSession()
        controller.releaseSession()

        XCTAssertEqual(
            controller.sessionReleaseCount,
            0,
            "an armed window's session cannot be reactivated from the background"
        )
    }

    /// And the positive control, without which the assertion above passes vacuously
    /// against a counter that never increments.
    @MainActor
    func testDictationDoesReleaseTheSessionWithNoWindowArmed() {
        let controller = DictationController()
        controller.ambientRail = AmbientRail(windowIsLive: { false }, yield: { _ in })

        controller.releaseSession()

        XCTAssertEqual(controller.sessionReleaseCount, 1, "with nothing armed it must still be polite")
    }

    /// The other structural backstop with no coverage: observation's.
    ///
    /// It covers a case its own yield cannot. `ListenController.stop()` has no state
    /// guard, so a tap on "Stop" against an already-`ended` session reaches this
    /// deactivation with no `startEngine` ever having run — and
    /// `AmbientController.arm` admits an `.ended` observation, because it is not
    /// `isActive`.
    @MainActor
    func testObservationNeverDeactivatesTheSessionUnderAnArmedWindow() {
        let controller = ListenController()
        controller.ambientRail = AmbientRail(windowIsLive: { true }, yield: { _ in })

        controller.stopEngine()

        XCTAssertEqual(controller.sessionReleaseCount, 0)

        controller.ambientRail = AmbientRail(windowIsLive: { false }, yield: { _ in })
        controller.stopEngine()
        XCTAssertEqual(controller.sessionReleaseCount, 1, "and with nothing armed it still releases")
    }
}
