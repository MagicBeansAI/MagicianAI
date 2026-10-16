//! Shared pieces of the Sarvam speech adapters (`sarvam_tts`, `sarvam_stt`).
//!
//! Measured 2026-09-30 against `api.sarvam.ai`:
//!
//! - Auth is the `api-subscription-key` header, the same key as Sarvam chat.
//! - Language codes are `xx-IN`. Neither endpoint takes `auto`: TTS requires
//!   a code, STT spells auto-detect `unknown`.
//! - TTS reads 11 languages (Urdu script is refused); STT hears 23.

use std::ops::Range;

pub const SARVAM_SPEECH_DEFAULT_BASE_URL: &str = "https://api.sarvam.ai";
pub(super) const SARVAM_AUTH_HEADER: &str = "api-subscription-key";

pub(super) const SARVAM_TTS_LANGUAGES: &[&str] = &[
    "bn-IN", "en-IN", "gu-IN", "hi-IN", "kn-IN", "ml-IN", "mr-IN", "od-IN", "pa-IN", "ta-IN",
    "te-IN",
];

pub(super) const SARVAM_STT_LANGUAGES: &[&str] = &[
    "as-IN", "bn-IN", "brx-IN", "doi-IN", "en-IN", "gu-IN", "hi-IN", "kn-IN", "kok-IN", "ks-IN",
    "mai-IN", "ml-IN", "mni-IN", "mr-IN", "ne-IN", "od-IN", "pa-IN", "sa-IN", "sat-IN", "sd-IN",
    "ta-IN", "te-IN", "ur-IN",
];

/// Map a language hint (`hi`, `hi-IN`, `en-US`, `or`) to a Sarvam code from
/// `allowed`. `None` means no usable hint: empty, `auto`, or a language
/// Sarvam does not carry.
pub(super) fn sarvam_language(hint: Option<&str>, allowed: &[&str]) -> Option<String> {
    let hint = hint.map(str::trim).filter(|value| !value.is_empty())?;
    if hint.eq_ignore_ascii_case("auto") || hint.eq_ignore_ascii_case("unknown") {
        return None;
    }
    if let Some(exact) = allowed.iter().find(|code| code.eq_ignore_ascii_case(hint)) {
        return Some((*exact).to_string());
    }
    let base = hint
        .split(['-', '_'])
        .next()
        .unwrap_or(hint)
        .to_ascii_lowercase();
    // ISO 639-1 spells Odia `or`; Sarvam uses `od`.
    let base = if base == "or" { "od".to_string() } else { base };
    allowed
        .iter()
        .find(|code| code.split('-').next() == Some(base.as_str()))
        .map(|code| (*code).to_string())
}

/// Unicode block of each Indic script Sarvam TTS reads, and the language a
/// text in that script is spoken as when nothing narrower is configured.
const SCRIPTS: &[(Range<u32>, &str)] = &[
    (0x0900..0x0980, "hi-IN"),
    (0x0980..0x0A00, "bn-IN"),
    (0x0A00..0x0A80, "pa-IN"),
    (0x0A80..0x0B00, "gu-IN"),
    (0x0B00..0x0B80, "od-IN"),
    (0x0B80..0x0C00, "ta-IN"),
    (0x0C00..0x0C80, "te-IN"),
    (0x0C80..0x0D00, "kn-IN"),
    (0x0D00..0x0D80, "ml-IN"),
];

fn script_index(ch: char) -> Option<usize> {
    let code = ch as u32;
    SCRIPTS.iter().position(|(range, _)| range.contains(&code))
}

fn language_script(language: &str) -> Option<usize> {
    match language {
        // Marathi shares Devanagari with Hindi.
        "mr-IN" => Some(0),
        // Assamese uses the Bengali script.
        "as-IN" => SCRIPTS.iter().position(|(_, code)| *code == "bn-IN"),
        other => SCRIPTS.iter().position(|(_, code)| *code == other),
    }
}

/// Pick the TTS language for `text`: the language of its dominant Indic
/// script, or `default` when the text has no Indic script or `default` is
/// already written in that script (a Marathi default keeps Devanagari text
/// Marathi rather than Hindi).
pub(super) fn tts_language_for_text(text: &str, default: &str) -> String {
    let mut counts = [0usize; SCRIPTS.len()];
    for index in text.chars().filter_map(script_index) {
        counts[index] += 1;
    }
    let dominant = counts
        .iter()
        .enumerate()
        .filter(|(_, count)| **count > 0)
        .max_by_key(|(_, count)| **count)
        .map(|(index, _)| index);
    match dominant {
        Some(index) if language_script(default) != Some(index) => SCRIPTS[index].1.to_string(),
        _ => default.to_string(),
    }
}

/// A parsed PCM WAV file: the `fmt ` chunk body and the sample data.
pub(super) struct Wav<'a> {
    pub fmt: &'a [u8],
    pub data: &'a [u8],
}

impl Wav<'_> {
    pub fn byte_rate(&self) -> u32 {
        u32::from_le_bytes([self.fmt[8], self.fmt[9], self.fmt[10], self.fmt[11]])
    }

    pub fn block_align(&self) -> usize {
        usize::from(u16::from_le_bytes([self.fmt[12], self.fmt[13]])).max(1)
    }

    /// Integer PCM with 16-bit samples, the only layout whose loudness the
    /// cut search reads. Format tag 1 is PCM, 0xFFFE is WAVE_FORMAT_EXTENSIBLE.
    pub fn is_pcm16(&self) -> bool {
        let tag = u16::from_le_bytes([self.fmt[0], self.fmt[1]]);
        let bits = u16::from_le_bytes([self.fmt[14], self.fmt[15]]);
        matches!(tag, 1 | 0xFFFE) && bits == 16
    }
}

/// Cut `wav` into WAV pieces of at most `max_secs`. Each cut lands at the
/// quietest 20 ms between `min_secs` and `max_secs` into the piece, so a word
/// is not split across two requests; audio that is not 16-bit PCM is cut at
/// `max_secs`. `None` when the audio already fits.
pub(super) fn split_wav_at_pauses(
    wav: &Wav<'_>,
    min_secs: u32,
    max_secs: u32,
) -> Option<Vec<Vec<u8>>> {
    let align = wav.block_align();
    let byte_rate = wav.byte_rate() as usize;
    let max = byte_rate * max_secs as usize / align * align;
    if max == 0 || wav.data.len() <= max {
        return None;
    }
    let min = (byte_rate * min_secs as usize / align * align).min(max);
    let frame = (byte_rate / 50 / align * align).max(align);
    let pcm16 = wav.is_pcm16();

    let mut pieces = Vec::new();
    let mut start = 0;
    while wav.data.len() - start > max {
        let window = &wav.data[start..start + max];
        let cut = if pcm16 {
            quietest_cut(window, min, frame, align)
        } else {
            max
        };
        pieces.push(build_wav(wav.fmt, &window[..cut]));
        start += cut;
    }
    pieces.push(build_wav(wav.fmt, &wav.data[start..]));
    Some(pieces)
}

/// Byte offset in the middle of the quietest `frame` at or after `min`.
fn quietest_cut(window: &[u8], min: usize, frame: usize, align: usize) -> usize {
    let mut best = (u64::MAX, window.len());
    let mut at = min;
    while at + frame <= window.len() {
        let energy = window[at..at + frame]
            .chunks_exact(2)
            .map(|sample| {
                let value = i64::from(i16::from_le_bytes([sample[0], sample[1]]));
                (value * value) as u64
            })
            .sum::<u64>();
        if energy < best.0 {
            best = (energy, at + frame / 2 / align * align);
        }
        at += frame;
    }
    best.1.max(align)
}

/// Parse a RIFF/WAVE file. A data chunk whose declared size overruns the
/// buffer (streamed WAVs write `0xFFFFFFFF`) is clamped to what is there.
pub(super) fn parse_wav(bytes: &[u8]) -> Option<Wav<'_>> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut fmt = None;
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]) as usize;
        let body = offset + 8;
        let end = body.saturating_add(size).min(bytes.len());
        match id {
            b"fmt " if end - body >= 16 => fmt = Some(&bytes[body..end]),
            b"data" => {
                return fmt.map(|fmt| Wav {
                    fmt,
                    data: &bytes[body..end],
                });
            },
            _ => {},
        }
        // Chunks are word-aligned.
        offset = end + (size & 1);
    }
    None
}

/// Build a WAV file from a `fmt ` chunk body and sample data.
pub(super) fn build_wav(fmt: &[u8], data: &[u8]) -> Vec<u8> {
    let riff_size = 4 + (8 + fmt.len()) + (8 + data.len());
    let mut out = Vec::with_capacity(8 + riff_size);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(riff_size as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
    out.extend_from_slice(fmt);
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    out
}

#[cfg(test)]
pub(super) fn test_wav(sample_rate: u32, samples: usize) -> Vec<u8> {
    build_wav(&test_fmt(sample_rate), &vec![0u8; samples * 2])
}

/// Mono 16-bit PCM `fmt ` body.
#[cfg(test)]
pub(super) fn test_fmt(sample_rate: u32) -> Vec<u8> {
    let mut fmt = Vec::with_capacity(16);
    fmt.extend_from_slice(&1u16.to_le_bytes()); // PCM
    fmt.extend_from_slice(&1u16.to_le_bytes()); // mono
    fmt.extend_from_slice(&sample_rate.to_le_bytes());
    fmt.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    fmt.extend_from_slice(&2u16.to_le_bytes());
    fmt.extend_from_slice(&16u16.to_le_bytes());
    fmt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_hints_map_to_sarvam_codes() {
        let tts = SARVAM_TTS_LANGUAGES;
        assert_eq!(sarvam_language(Some("hi"), tts).as_deref(), Some("hi-IN"));
        assert_eq!(
            sarvam_language(Some("TA-in"), tts).as_deref(),
            Some("ta-IN")
        );
        assert_eq!(
            sarvam_language(Some("en-US"), tts).as_deref(),
            Some("en-IN")
        );
        assert_eq!(sarvam_language(Some("or"), tts).as_deref(), Some("od-IN"));
        assert_eq!(sarvam_language(Some("auto"), tts), None);
        assert_eq!(sarvam_language(Some("fr"), tts), None);
        assert_eq!(sarvam_language(Some("ur"), tts), None);
        assert_eq!(
            sarvam_language(Some("ur"), SARVAM_STT_LANGUAGES).as_deref(),
            Some("ur-IN")
        );
    }

    #[test]
    fn tts_language_follows_the_dominant_script() {
        assert_eq!(tts_language_for_text("Hello there", "en-IN"), "en-IN");
        assert_eq!(
            tts_language_for_text("Meeting at 5, ठीक है?", "en-IN"),
            "hi-IN"
        );
        assert_eq!(tts_language_for_text("வணக்கம் friend", "en-IN"), "ta-IN");
        assert_eq!(tts_language_for_text("তুমি কেমন আছ", "hi-IN"), "bn-IN");
        assert_eq!(tts_language_for_text("नमस्कार", "mr-IN"), "mr-IN");
    }

    #[test]
    fn wav_round_trips_through_parse_and_build() {
        let wav = test_wav(16_000, 100);
        let parsed = parse_wav(&wav).expect("parse");
        assert_eq!(parsed.data.len(), 200);
        assert_eq!(parsed.byte_rate(), 32_000);
        assert_eq!(parsed.block_align(), 2);
        assert_eq!(build_wav(parsed.fmt, parsed.data), wav);
    }

    #[test]
    fn streamed_wav_data_size_is_clamped() {
        let mut wav = test_wav(16_000, 10);
        let data_size_at = wav.len() - 20 - 4;
        wav[data_size_at..data_size_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(parse_wav(&wav).expect("parse").data.len(), 20);
    }

    /// Loud 16-bit mono audio with silent gaps at the given seconds.
    fn loud_wav_with_gaps(sample_rate: u32, secs: u32, gaps: &[(f32, f32)]) -> Vec<u8> {
        let mut data = Vec::with_capacity((sample_rate * secs * 2) as usize);
        for index in 0..sample_rate * secs {
            let at = index as f32 / sample_rate as f32;
            let silent = gaps.iter().any(|(from, to)| at >= *from && at < *to);
            let value: i16 = if silent {
                0
            } else if index % 2 == 0 {
                12_000
            } else {
                -12_000
            };
            data.extend_from_slice(&value.to_le_bytes());
        }
        build_wav(&test_fmt(sample_rate), &data)
    }

    fn piece_secs(piece: &[u8]) -> f32 {
        let wav = parse_wav(piece).expect("piece parses");
        wav.data.len() as f32 / wav.byte_rate() as f32
    }

    #[test]
    fn long_audio_is_cut_at_the_pause_nearest_the_limit() {
        let audio = loud_wav_with_gaps(8_000, 50, &[(22.0, 22.4)]);
        let wav = parse_wav(&audio).unwrap();
        let pieces = split_wav_at_pauses(&wav, 18, 28).expect("needs cutting");
        assert_eq!(pieces.len(), 2);
        let first = piece_secs(&pieces[0]);
        assert!(
            (22.0..22.4).contains(&first),
            "cut at {first}s, not in the pause"
        );
        let total: f32 = pieces.iter().map(|piece| piece_secs(piece)).sum();
        assert!((total - 50.0).abs() < 0.01);
    }

    #[test]
    fn audio_without_pauses_is_still_cut_under_the_limit() {
        let audio = loud_wav_with_gaps(8_000, 90, &[]);
        let wav = parse_wav(&audio).unwrap();
        let pieces = split_wav_at_pauses(&wav, 18, 28).unwrap();
        assert!(pieces.iter().all(|piece| piece_secs(piece) <= 28.0));
        let total: f32 = pieces.iter().map(|piece| piece_secs(piece)).sum();
        assert!((total - 90.0).abs() < 0.01);
    }

    #[test]
    fn audio_under_the_limit_is_not_cut() {
        let audio = test_wav(8_000, 8_000 * 28);
        assert!(split_wav_at_pauses(&parse_wav(&audio).unwrap(), 18, 28).is_none());
    }

    #[test]
    fn non_wav_bytes_do_not_parse() {
        assert!(parse_wav(b"\xff\xf3\xc0\xc4not a wav").is_none());
    }
}
