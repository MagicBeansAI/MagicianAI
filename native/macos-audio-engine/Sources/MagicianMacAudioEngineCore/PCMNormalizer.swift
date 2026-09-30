import Foundation

public enum PCMNormalizerError: Error, LocalizedError {
    case invalidFormat(String)

    public var errorDescription: String? {
        switch self {
        case .invalidFormat(let reason): reason
        }
    }
}

/// Incrementally converts interleaved PCM16/Float32 input into 16 kHz mono
/// Float32 while retaining partial frames and resampling phase across calls.
public struct PCMNormalizer: Sendable {
    public static let outputSampleRate = 16_000

    private let format: StreamAudioFormat
    private let bytesPerSample: Int
    private var byteRemainder = Data()
    private var sourceSamples: [Float] = []
    private var sourcePosition = 0.0

    public init(format: StreamAudioFormat) throws {
        guard (8_000...192_000).contains(format.sampleRateHz) else {
            throw PCMNormalizerError.invalidFormat("sample rate must be between 8 kHz and 192 kHz")
        }
        guard (1...8).contains(format.channels) else {
            throw PCMNormalizerError.invalidFormat("channel count must be between 1 and 8")
        }
        self.format = format
        self.bytesPerSample = format.sampleFormat == .pcmS16Le ? 2 : 4
    }

    public mutating func append(_ data: Data) throws -> [Float] {
        byteRemainder.append(data)
        let frameBytes = bytesPerSample * format.channels
        let completeBytes = byteRemainder.count - (byteRemainder.count % frameBytes)
        guard completeBytes > 0 else { return [] }

        let complete = byteRemainder.prefix(completeBytes)
        byteRemainder.removeFirst(completeBytes)
        let bytes = [UInt8](complete)
        var mono = [Float]()
        mono.reserveCapacity(completeBytes / frameBytes)
        var offset = 0
        while offset < bytes.count {
            var mixed: Float = 0
            for _ in 0..<format.channels {
                switch format.sampleFormat {
                case .pcmS16Le:
                    let raw = UInt16(bytes[offset]) | (UInt16(bytes[offset + 1]) << 8)
                    mixed += Float(Int16(bitPattern: raw)) / 32_768.0
                case .pcmF32Le:
                    let raw = UInt32(bytes[offset])
                        | (UInt32(bytes[offset + 1]) << 8)
                        | (UInt32(bytes[offset + 2]) << 16)
                        | (UInt32(bytes[offset + 3]) << 24)
                    let value = Float(bitPattern: raw)
                    mixed += value.isFinite ? max(-1, min(1, value)) : 0
                }
                offset += bytesPerSample
            }
            mono.append(mixed / Float(format.channels))
        }

        sourceSamples.append(contentsOf: mono)
        return drainResampled()
    }

    public mutating func finish() -> [Float] {
        byteRemainder.removeAll(keepingCapacity: false)
        guard !sourceSamples.isEmpty else { return [] }
        let output = drainResampled(flushLastSample: true)
        sourceSamples.removeAll(keepingCapacity: false)
        sourcePosition = 0
        return output
    }

    private mutating func drainResampled(flushLastSample: Bool = false) -> [Float] {
        let step = Double(format.sampleRateHz) / Double(Self.outputSampleRate)
        var output = [Float]()
        while sourcePosition + 1 < Double(sourceSamples.count)
            || (flushLastSample && sourcePosition < Double(sourceSamples.count))
        {
            let lower = min(Int(sourcePosition), sourceSamples.count - 1)
            let upper = min(lower + 1, sourceSamples.count - 1)
            let fraction = Float(sourcePosition - Double(lower))
            output.append(sourceSamples[lower] * (1 - fraction) + sourceSamples[upper] * fraction)
            sourcePosition += step
        }
        let consumed = min(Int(sourcePosition), max(0, sourceSamples.count - 1))
        if consumed > 0 {
            sourceSamples.removeFirst(consumed)
            sourcePosition -= Double(consumed)
        }
        return output
    }
}
