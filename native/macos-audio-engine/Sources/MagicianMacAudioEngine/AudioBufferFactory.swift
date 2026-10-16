@preconcurrency import AVFoundation
import Foundation

enum AudioBufferFactoryError: Error, LocalizedError {
    case allocationFailed

    var errorDescription: String? {
        switch self { case .allocationFailed: "failed to allocate a 16 kHz mono audio buffer" }
    }
}

func makeMono16KhzBuffer(_ samples: [Float]) throws -> AVAudioPCMBuffer {
    guard let format = AVAudioFormat(
        commonFormat: .pcmFormatFloat32,
        sampleRate: 16_000,
        channels: 1,
        interleaved: false
    ), let buffer = AVAudioPCMBuffer(
        pcmFormat: format,
        frameCapacity: AVAudioFrameCount(samples.count)
    ), let channel = buffer.floatChannelData?[0]
    else {
        throw AudioBufferFactoryError.allocationFailed
    }
    buffer.frameLength = AVAudioFrameCount(samples.count)
    samples.withUnsafeBufferPointer { source in
        if let base = source.baseAddress {
            channel.update(from: base, count: source.count)
        }
    }
    return buffer
}
