import AVFoundation
import CoreMedia
import Foundation

/// Converts ReplayKit audio `CMSampleBuffer`s (app audio or mic) into the
/// backend's 16 kHz mono PCM16-LE chunk format. Use one instance per channel: it
/// locks its `AVAudioConverter` to the first buffer's input format (each of the
/// broadcast's audio streams has a stable format for the life of the broadcast),
/// so app-audio (often interleaved stereo) and mic (mono) each get their own.
final class BroadcastAudioConverter {
    private let target = AVAudioFormat(
        commonFormat: .pcmFormatFloat32,
        sampleRate: 16_000,
        channels: 1,
        interleaved: false
    )!
    private var converter: AVAudioConverter?
    private var inputFormat: AVAudioFormat?

    func pcm16(from sampleBuffer: CMSampleBuffer) -> Data? {
        guard
            CMSampleBufferDataIsReady(sampleBuffer),
            let fmt = CMSampleBufferGetFormatDescription(sampleBuffer),
            let asbdPtr = CMAudioFormatDescriptionGetStreamBasicDescription(fmt)
        else { return nil }
        var asbd = asbdPtr.pointee
        guard let inFormat = inputFormat ?? AVAudioFormat(streamDescription: &asbd) else { return nil }
        if converter == nil {
            converter = AVAudioConverter(from: inFormat, to: target)
            inputFormat = inFormat
        }
        guard let converter else { return nil }

        let frames = CMSampleBufferGetNumSamples(sampleBuffer)
        guard
            frames > 0,
            let inBuf = AVAudioPCMBuffer(pcmFormat: inFormat, frameCapacity: AVAudioFrameCount(frames))
        else { return nil }
        inBuf.frameLength = AVAudioFrameCount(frames)
        let status = CMSampleBufferCopyPCMDataIntoAudioBufferList(
            sampleBuffer,
            at: 0,
            frameCount: Int32(frames),
            into: inBuf.mutableAudioBufferList
        )
        guard status == noErr else { return nil }

        let ratio = target.sampleRate / inFormat.sampleRate
        let outCapacity = AVAudioFrameCount(Double(frames) * ratio) + 1024
        guard let outBuf = AVAudioPCMBuffer(pcmFormat: target, frameCapacity: outCapacity) else {
            return nil
        }
        var consumed = false
        var err: NSError?
        converter.convert(to: outBuf, error: &err) { _, status in
            if consumed {
                status.pointee = .noDataNow
                return nil
            }
            consumed = true
            status.pointee = .haveData
            return inBuf
        }
        guard err == nil, outBuf.frameLength > 0, let ch = outBuf.floatChannelData?[0] else {
            return nil
        }
        return VoicePCM.floatToInt16LE(UnsafeBufferPointer(start: ch, count: Int(outBuf.frameLength)))
    }
}
