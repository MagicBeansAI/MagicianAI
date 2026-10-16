import Foundation

/// 24 kHz mono PCM conversion for the Live voice transport. Capture emits float32 from
/// AVAudioEngine → int16-LE `Data` for the WS; playback decodes int16-LE `Data` → float32.
enum VoicePCM {
    static func floatToInt16LE(_ input: [Float]) -> Data {
        input.withUnsafeBufferPointer { floatToInt16LE($0) }
    }

    /// Pointer overload — used on the audio render thread to encode a captured
    /// buffer without allocating an intermediate `[Float]`. Preallocates the output
    /// `Data` and writes int16-LE in one pass (no per-sample `append`).
    static func floatToInt16LE(_ input: UnsafeBufferPointer<Float>) -> Data {
        var data = Data(count: input.count * 2)
        data.withUnsafeMutableBytes { raw in
            let out = raw.bindMemory(to: Int16.self)
            for i in 0..<input.count {
                let clamped = max(-1.0, min(1.0, input[i]))
                out[i] = Int16(clamped < 0 ? clamped * 32768 : clamped * 32767).littleEndian
            }
        }
        return data
    }

    static func int16ToFloat(_ data: Data) -> [Float] {
        stride(from: 0, to: data.count - 1, by: 2).map { i in
            let lo = UInt16(data[data.startIndex + i])
            let hi = UInt16(data[data.startIndex + i + 1])
            let s = Int16(bitPattern: lo | (hi << 8))
            return Float(s) / 32768.0
        }
    }

    /// Resample mono PCM16-LE from one sample rate to another, by linear
    /// interpolation between the two nearest source samples.
    ///
    /// **Unwired since 2026-07-30; kept as a pure utility, removal deferred on
    /// this busy branch.** It existed for the ambient wake handoff, where two
    /// rates that were each correct met: `WakePreRoll` and `WakeSpotter` are
    /// specified in **16 kHz** because that is what the on-device wake model
    /// takes, and the transport is **24 kHz** — a rate that is *not* negotiated
    /// anywhere: `RealtimeVoiceProtocol.startPayload` sends no sample rate,
    /// `session.ready` carries none, and the backend reserves binary frames on
    /// the control socket for 24 kHz PCM by convention. So the pre-roll could
    /// not simply be forwarded: 16 kHz bytes handed to a 24 kHz reader stretch
    /// by 1.5x — a slurred, deep replay that reads as a wake-accuracy problem
    /// rather than an arithmetic one. The no-pre-ready-capture decision removed
    /// that caller (nothing said before `session.ready` is kept for anyone),
    /// leaving this with its tests and no production call site.
    ///
    /// **Bytes and arithmetic rather than `AVAudioConverter`**, for the same
    /// reason `AmbientMicResampler` was split out of the microphone engine: this
    /// is the part of the path that runs without hardware, so it is the part that
    /// can be tested at all. Linear interpolation was adequate because the one
    /// caller *upsampled* speech — the imaging it leaves sits above the source's
    /// 8 kHz Nyquist, where the provider's own front end is not listening — and a
    /// filtered resampler would buy inaudible quality on a 2 s buffer at the cost
    /// of the one property below.
    ///
    /// **The output byte count is always even**, because it allocates
    /// `samples * 2` and writes whole `Int16`s — the same guarantee
    /// `floatToInt16LE` gives, and load-bearing for the same reason: every
    /// consumer of these bytes reads whole samples, and a single stray byte
    /// byte-swaps every value after it into plausible-sounding noise. An odd
    /// INPUT length is floored to whole samples here rather than trusted, so a
    /// caller cannot launder one through.
    static func resamplePCM16LE(_ data: Data, from inputRate: Int, to outputRate: Int) -> Data {
        guard inputRate > 0, outputRate > 0 else { return Data() }
        // Floor to whole samples: a trailing odd byte is not half a sample, it is
        // a caller's mistake, and interpolating across it would misalign the rest.
        let inputCount = data.count / 2
        guard inputCount > 0 else { return Data() }
        guard inputRate != outputRate else { return Data(data.prefix(inputCount * 2)) }

        var input = [Int16](repeating: 0, count: inputCount)
        // Byte-wise rather than `bindMemory`: `data` may be a slice whose start is
        // not two-byte aligned, and Int16-typed access to that is undefined.
        data.withUnsafeBytes { raw in
            for i in 0..<inputCount {
                let lo = UInt16(raw[i * 2])
                let hi = UInt16(raw[i * 2 + 1])
                input[i] = Int16(bitPattern: lo | (hi << 8))
            }
        }

        let outputCount = max(
            1,
            Int((Double(inputCount) * Double(outputRate) / Double(inputRate)).rounded())
        )
        let step = Double(inputRate) / Double(outputRate)
        var out = Data(count: outputCount * 2)
        out.withUnsafeMutableBytes { raw in
            let dst = raw.bindMemory(to: Int16.self)
            for j in 0..<outputCount {
                let position = Double(j) * step
                let lower = min(inputCount - 1, Int(position))
                let upper = min(inputCount - 1, lower + 1)
                let fraction = position - Double(lower)
                let value = Double(input[lower])
                    + (Double(input[upper]) - Double(input[lower])) * fraction
                dst[j] = Int16(clamping: Int(value.rounded())).littleEndian
            }
        }
        return out
    }
}
