//! OpenAI streaming-STT helpers shared by lib meeting paths.

/// RMS below which a segment counts as silence (default 120/32768 ≈ −49 dBFS
/// — comfortably under speech at normal mic gain, above pure room tone).
/// `MEET_STT_SILENCE_RMS` overrides; `0` disables the gate.
pub const DEFAULT_SILENCE_RMS: f64 = 120.0;

/// Root-mean-square amplitude of little-endian PCM16 samples (0..=32768).
pub fn pcm16_rms(pcm: &[u8]) -> f64 {
    if pcm.len() < 2 {
        return 0.0;
    }
    let mut sum_sq = 0.0f64;
    let mut count = 0usize;
    for sample in pcm.chunks_exact(2) {
        let value = i16::from_le_bytes([sample[0], sample[1]]) as f64;
        sum_sq += value * value;
        count += 1;
    }
    if count == 0 {
        return 0.0;
    }
    (sum_sq / count as f64).sqrt()
}
