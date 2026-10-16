//! Shared classification for produced files. Producer metadata is authoritative
//! for ambiguous containers and text formats; missing metadata falls back to
//! bounded content inspection. Filenames never determine the MIME type.

use std::path::Path;

use actix_web::mime::{Mime, STAR};
use tokio::io::AsyncReadExt;

const INSPECTION_BYTES: u64 = 64 * 1024;
const UNKNOWN_CONTENT_TYPE: &str = "application/octet-stream";

/// Resolve the output contract's MIME type without loading an entire artifact.
/// Signature detection identifies a format, not its completeness or validity;
/// producer-specific validation remains responsible for decoding/validation.
pub async fn output_file_content_type(path: &Path, declared: Option<&str>) -> String {
    if let Some(mime) = declared.and_then(|value| value.trim().parse::<Mime>().ok()) {
        if mime.type_() != STAR
            && mime.subtype() != STAR
            && mime.essence_str() != UNKNOWN_CONTENT_TYPE
        {
            return mime.essence_str().to_owned();
        }
    }

    let Ok(file) = tokio::fs::File::open(path).await else {
        return UNKNOWN_CONTENT_TYPE.to_owned();
    };
    let mut prefix = Vec::new();
    // One extra byte distinguishes a complete small JSON document from a
    // truncated prefix. The read remains bounded even for multi-GB artifacts.
    if file
        .take(INSPECTION_BYTES + 1)
        .read_to_end(&mut prefix)
        .await
        .is_err()
    {
        return UNKNOWN_CONTENT_TYPE.to_owned();
    }
    if let Some(kind) = infer::get(&prefix) {
        return kind.mime_type().to_owned();
    }
    if prefix.len() <= INSPECTION_BYTES as usize
        && !prefix.is_empty()
        && serde_json::from_slice::<serde::de::IgnoredAny>(&prefix).is_ok()
    {
        return "application/json".to_owned();
    }
    if !prefix.is_empty()
        && std::str::from_utf8(&prefix).is_ok_and(|text| {
            text.chars()
                .all(|ch| !ch.is_control() || matches!(ch, '\n' | '\r' | '\t'))
        })
    {
        return "text/plain".to_owned();
    }
    UNKNOWN_CONTENT_TYPE.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn task_delivery_content_types_use_metadata_and_bytes_not_extensions() {
        let temp = tempfile::tempdir().unwrap();
        let misleading = temp.path().join("pretend.mp3");
        tokio::fs::write(&misleading, b"ordinary text")
            .await
            .unwrap();
        assert_eq!(
            output_file_content_type(&misleading, None).await,
            "text/plain"
        );
        assert_eq!(
            output_file_content_type(&misleading, Some("text/markdown; charset=utf-8")).await,
            "text/markdown"
        );
        assert_eq!(
            output_file_content_type(&misleading, Some("not-a-mime")).await,
            "text/plain"
        );
        assert_eq!(
            output_file_content_type(&misleading, Some("audio/*")).await,
            "text/plain"
        );

        let opaque = temp.path().join("no-extension");
        tokio::fs::write(&opaque, b"\x00\xff\x00\x81")
            .await
            .unwrap();
        assert_eq!(
            output_file_content_type(&opaque, None).await,
            UNKNOWN_CONTENT_TYPE
        );
        // A producer can support a new type without any changes to this code.
        assert_eq!(
            output_file_content_type(&opaque, Some("application/vnd.example.new-format")).await,
            "application/vnd.example.new-format"
        );

        let wave = temp.path().join("audio.bin");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 8000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wave, spec).unwrap();
        writer.write_sample(0i16).unwrap();
        writer.finalize().unwrap();
        assert!(output_file_content_type(&wave, None)
            .await
            .starts_with("audio/"));
        assert_eq!(
            output_file_content_type(&wave, Some("application/octet-stream")).await,
            output_file_content_type(&wave, None).await
        );

        let json = temp.path().join("manifest.dat");
        tokio::fs::write(&json, br#"{"outputs":[]}"#).await.unwrap();
        assert_eq!(
            output_file_content_type(&json, None).await,
            "application/json"
        );
        assert_eq!(
            output_file_content_type(&temp.path().join("missing.wav"), None).await,
            UNKNOWN_CONTENT_TYPE
        );
    }
}
