import Foundation

/// Fixed-capacity ring buffer holding the most recent audio captured while
/// ambient mode is armed but not yet awake.
///
/// The activation phrase and the request after it are one continuous utterance,
/// so the realtime socket is never ready in time to hear the beginning of the
/// request. Draining this into the upstream once the socket reports ready
/// replays what would otherwise be lost.
///
/// Deliberately expressed in BYTES rather than audio-framework types, so it is
/// testable without hardware. Callers hand it PCM16 mono exactly as it will be
/// sent upstream.
///
/// **Whole PCM16 samples, end to end.** `capacityBytes` must be positive and even,
/// and every `append` length must be even; both are enforced by `precondition`
/// rather than clamped or assumed. The ring is byte-granular, so an odd count
/// through *either* door leaves the drain starting mid-sample and byte-swaps every
/// decoded value — silent corruption that surfaces as plausible noise on the one
/// path with no reference audio to compare it against. Enforcing both keeps the
/// contract one rule instead of a guarantee with a hole in it; every PCM16 capture
/// buffer is already an even byte count, so it costs a caller nothing.
///
/// **Threading contract.** A plain value type with no internal locking: `append` is
/// for the capture thread, `drain`/`reset` for the owner. The OWNER must serialize
/// them. Overlapping calls against the same stored property are both *modify*
/// accesses — Swift's exclusivity checking traps on the overlap, and absent a trap
/// it is an unsynchronized race on `storage`/`writeIndex`/`filled`. Note that
/// `removeTap`/`stop()` does not guarantee an in-flight tap callback has returned,
/// so stopping capture first narrows that window without closing it.
struct WakePreRoll {
    let capacityBytes: Int

    private var storage: [UInt8]
    private var writeIndex = 0
    private var filled = 0

    init(capacityBytes: Int) {
        precondition(
            capacityBytes > 0 && capacityBytes.isMultiple(of: 2),
            "WakePreRoll capacity must be positive and even (PCM16 is 2 bytes per sample), got \(capacityBytes)"
        )
        self.capacityBytes = capacityBytes
        self.storage = [UInt8](repeating: 0, count: capacityBytes)
    }

    /// PCM16 mono: two bytes per sample, so this always yields an even capacity.
    ///
    /// **The 2 s default is coupled to the wake spotter's decision to fire on
    /// finalised results, and cannot be shortened on its own.** `VoskWakeSpotter`
    /// matches only finalised utterances because partials false-accept on 73% of
    /// phonetically adjacent near-misses against 13% on finals. Finals land later
    /// — measured ~1600 ms into a continuous sentence whose phrase ends at
    /// ~820 ms — and this buffer is the entire reason that delay costs no request
    /// audio: it holds `[T-2000, T]` at a hit at time `T`, which covers the
    /// request start with about 1.2 s to spare. Reduce `seconds` and that margin
    /// erodes directly, at which point the wake starts eating the beginning of
    /// what the user asked for. Re-measure both together; see
    /// `docs/components/magios/ambient-mode.md`.
    init(seconds: Double = 2, sampleRate: Int = 16_000) {
        self.init(capacityBytes: Int((Double(sampleRate) * seconds).rounded()) * 2)
    }

    var count: Int { filled }
    var isEmpty: Bool { filled == 0 }

    /// Copying byte-at-a-time keeps the wraparound obviously correct. At 16 kHz
    /// this runs on ~8 KB every quarter second, which is nothing.
    mutating func append(_ data: Data) {
        precondition(
            data.count.isMultiple(of: 2),
            "WakePreRoll takes whole PCM16 samples; an odd append misaligns every later drain, got \(data.count) bytes"
        )
        guard !data.isEmpty else { return }
        // Anything older than the last `capacityBytes` cannot survive, so drop it
        // before touching storage.
        let incoming = data.count > capacityBytes ? Data(data.suffix(capacityBytes)) : data
        for byte in incoming {
            storage[writeIndex] = byte
            writeIndex = (writeIndex + 1) % capacityBytes
        }
        filled = min(capacityBytes, filled + incoming.count)
    }

    /// Everything buffered, oldest byte first, and reset.
    ///
    /// Copies the two contiguous runs a ring is actually made of — the oldest byte
    /// through the end of storage, then the wrapped remainder — rather than one byte
    /// at a time. This runs at the wake instant, the most latency-sensitive moment in
    /// the feature, where a per-byte `Data.append` over a full buffer costs ~870x more.
    mutating func drain() -> Data {
        guard filled > 0 else { return Data() }
        let start = (writeIndex - filled + capacityBytes) % capacityBytes
        let tailCount = min(filled, capacityBytes - start)
        var out = Data(capacity: filled)
        storage.withUnsafeBufferPointer { buffer in
            out.append(UnsafeBufferPointer(rebasing: buffer[start ..< start + tailCount]))
            if tailCount < filled {
                out.append(UnsafeBufferPointer(rebasing: buffer[0 ..< filled - tailCount]))
            }
        }
        reset()
        return out
    }

    mutating func reset() {
        writeIndex = 0
        filled = 0
    }
}
