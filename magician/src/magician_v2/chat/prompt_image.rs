//! Chat prompt images sized for the provider before they are attached.
//!
//! Uploads can be up to 20 MB and every past user turn re-attaches its images,
//! but providers refuse oversized ones (Anthropic: 5 MB of base64 per image)
//! and downscale anything past ~1568 px on its long side anyway, so sending
//! more only costs upload bytes and time on every turn. An image within the
//! long side decision screenshots fit (1568 px) and the byte limit is attached
//! byte for byte. A larger one is decoded (its EXIF orientation applied),
//! scaled to that long side and re-encoded. Only the request copy changes; the
//! stored file is untouched.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};

use image::ImageDecoder;
use magicllm::prelude::ContentBlock;

/// Raw bytes whose base64 is at most 5 MiB, Anthropic's per-image limit.
pub(crate) const MAX_PROMPT_IMAGE_BYTES: usize = 5 * 1024 * 1024 / 4 * 3;
/// Long side of a request copy: what Anthropic serves without resizing, and
/// the side decision screenshots are fitted to.
pub(crate) const MAX_PROMPT_IMAGE_SIDE: u32 = decision_engine_contract::asset::MAX_IMAGE_SIDE;
const JPEG_QUALITY: u8 = 85;
/// Decode bounds: an upload past these is refused, not decoded. 16384 px a
/// side covers any camera or full-page capture; 256 MiB bounds the buffers.
const MAX_DECODE_SIDE: u32 = 16_384;
const MAX_DECODE_ALLOC_BYTES: u64 = 256 * 1024 * 1024;
/// Images decoded and scaled at once across all chats; each can hold up to
/// [`MAX_DECODE_ALLOC_BYTES`].
const FIT_CONCURRENCY: usize = 2;
/// Results kept so a past turn's image is not decoded and scaled again every
/// turn: up to this many, and this many bytes. A scaled copy is a 1568 px PNG
/// or JPEG (typically well under 2 MB); an image that cannot be fitted keeps
/// its note.
const FIT_CACHE_ENTRIES: usize = 64;
const FIT_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// An image as it is attached to the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PromptImage {
    pub data: Vec<u8>,
    pub media_type: String,
}

/// The scaled copy, or why the image cannot be attached.
type Fitted = Result<PromptImage, String>;

/// The image as a prompt block, or, when it cannot be brought within the
/// provider's limits, a text note naming the file and why. A note rather than
/// a failed turn: past turns re-attach their images, so one bad file would
/// otherwise fail every later turn of the chat.
pub(crate) async fn prompt_image_block(
    bytes: Vec<u8>,
    media_type: &str,
    caption: Option<String>,
    file: &str,
) -> ContentBlock {
    match fit_prompt_image(bytes, media_type, file).await {
        Ok(image) => ContentBlock::Image {
            data: image.data,
            media_type: image.media_type,
            caption,
        },
        Err(reason) => ContentBlock::Text {
            text: format!("[image not attached: {reason}]"),
        },
    }
}

/// The image unchanged when it is within limits, else a scaled copy. Decoding
/// and scaling run off the async runtime; a failure is logged once per image
/// content, not on every turn that re-attaches it.
pub(crate) async fn fit_prompt_image(bytes: Vec<u8>, media_type: &str, file: &str) -> Fitted {
    if within_limits(&bytes) {
        return Ok(PromptImage {
            data: bytes,
            media_type: media_type.to_string(),
        });
    }
    // Hash and look up first: a cache hit never waits behind a decode.
    let lookup = tokio::task::spawn_blocking(move || {
        let key = content_key(&bytes);
        let hit = cached(&key);
        (bytes, key, hit)
    })
    .await;
    let (bytes, key) = match lookup {
        Ok((_, _, Some(hit))) => return (*hit).clone(),
        Ok((bytes, key, None)) => (bytes, key),
        Err(error) => return scaling_task_failed(file, &error),
    };
    // A closed semaphore never happens (it is never closed); proceed anyway.
    let _permit = FIT_PERMITS.acquire().await.ok();
    let owned_file = file.to_string();
    match tokio::task::spawn_blocking(move || fit_keyed(&bytes, key, &owned_file)).await {
        Ok(fitted) => fitted,
        Err(error) => scaling_task_failed(file, &error),
    }
}

static FIT_PERMITS: LazyLock<tokio::sync::Semaphore> =
    LazyLock::new(|| tokio::sync::Semaphore::new(FIT_CONCURRENCY));

fn scaling_task_failed(file: &str, error: &tokio::task::JoinError) -> Fitted {
    let reason = format!("{file}: image scaling task failed: {error}");
    tracing::warn!(file, reason = %reason, "chat prompt image not attached");
    Err(reason)
}

/// Within the byte limit and, when this build can read its header, the long
/// side. A format it cannot read (GIF, WebP) is judged by size alone.
fn within_limits(bytes: &[u8]) -> bool {
    bytes.len() <= MAX_PROMPT_IMAGE_BYTES
        && image_dimensions(bytes)
            .is_none_or(|(width, height)| width.max(height) <= MAX_PROMPT_IMAGE_SIDE)
}

fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// Least-recently-used results by content hash, bounded by count and bytes.
#[derive(Default)]
struct FitCache {
    entries: HashMap<[u8; 32], Arc<Fitted>>,
    /// Oldest use first.
    order: VecDeque<[u8; 32]>,
    bytes: usize,
}

fn cached_bytes(fitted: &Fitted) -> usize {
    match fitted {
        Ok(image) => image.data.len(),
        Err(reason) => reason.len(),
    }
}

impl FitCache {
    fn touch(&mut self, key: &[u8; 32]) {
        if let Some(position) = self.order.iter().position(|entry| entry == key) {
            self.order.remove(position);
        }
        self.order.push_back(*key);
    }

    fn get(&mut self, key: &[u8; 32]) -> Option<Arc<Fitted>> {
        let hit = self.entries.get(key).cloned()?;
        self.touch(key);
        Some(hit)
    }

    fn put(&mut self, key: [u8; 32], fitted: Arc<Fitted>) {
        self.bytes = self.bytes.saturating_add(cached_bytes(&fitted));
        if let Some(replaced) = self.entries.insert(key, fitted) {
            self.bytes = self.bytes.saturating_sub(cached_bytes(&replaced));
        }
        self.touch(&key);
        // The newest entry always stays, whatever its size.
        while self.order.len() > FIT_CACHE_ENTRIES
            || (self.bytes > FIT_CACHE_BYTES && self.order.len() > 1)
        {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(cached_bytes(&evicted));
            }
        }
    }
}

static FITTED: LazyLock<Mutex<FitCache>> = LazyLock::new(Mutex::default);

fn content_key(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

fn cached(key: &[u8; 32]) -> Option<Arc<Fitted>> {
    FITTED.lock().ok().and_then(|mut cache| cache.get(key))
}

/// [`fit_blocking`] behind the content-keyed cache. A failure is cached and
/// logged when first computed.
fn fit_cached(bytes: &[u8], file: &str) -> Fitted {
    fit_keyed(bytes, content_key(bytes), file)
}

fn fit_keyed(bytes: &[u8], key: [u8; 32], file: &str) -> Fitted {
    // Looked up again: a concurrent turn may have fitted it while this one
    // waited for a permit.
    if let Some(hit) = cached(&key) {
        return (*hit).clone();
    }
    let fitted = fit_blocking(bytes, file);
    if let Err(reason) = &fitted {
        tracing::warn!(file, reason = %reason, "chat prompt image not attached");
    }
    if let Ok(mut cache) = FITTED.lock() {
        cache.put(key, Arc::new(fitted.clone()));
    }
    fitted
}

/// Decode with the EXIF orientation applied (a re-encoded copy carries no
/// EXIF), scale to [`MAX_PROMPT_IMAGE_SIDE`] when larger, and re-encode: a
/// JPEG stays a JPEG; anything else becomes a PNG, or a JPEG over white when
/// the PNG is still over the byte limit (a photo or a noisy capture).
pub(crate) fn fit_blocking(bytes: &[u8], file: &str) -> Fitted {
    let size = describe_bytes(bytes.len());
    let undecodable = |error: &dyn std::fmt::Display| {
        format!(
            "{file} ({size}) is over the provider's image limits and could not be decoded \
             to scale it: {error}"
        )
    };
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| undecodable(&error))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_SIDE);
    limits.max_image_height = Some(MAX_DECODE_SIDE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC_BYTES);
    reader.limits(limits);
    let source_format = reader.format();
    let mut decoder = reader.into_decoder().map_err(|error| undecodable(&error))?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut decoded =
        image::DynamicImage::from_decoder(decoder).map_err(|error| undecodable(&error))?;
    decoded.apply_orientation(orientation);
    let scaled = if decoded.width().max(decoded.height()) > MAX_PROMPT_IMAGE_SIDE {
        decoded.resize(
            MAX_PROMPT_IMAGE_SIDE,
            MAX_PROMPT_IMAGE_SIDE,
            image::imageops::FilterType::Triangle,
        )
    } else {
        decoded
    };
    let encode_failed =
        |error: image::ImageError| format!("{file} ({size}) could not be re-encoded: {error}");
    let mut fitted = None;
    if source_format != Some(image::ImageFormat::Jpeg) {
        // Fast deflate: the copy is sent once and cached, and a slow
        // compressor dominates the turn in debug builds. Deterministic.
        let mut png = Vec::new();
        scaled
            .write_with_encoder(image::codecs::png::PngEncoder::new_with_quality(
                &mut png,
                image::codecs::png::CompressionType::Fast,
                image::codecs::png::FilterType::Adaptive,
            ))
            .map_err(encode_failed)?;
        if png.len() <= MAX_PROMPT_IMAGE_BYTES {
            fitted = Some(PromptImage {
                data: png,
                media_type: "image/png".to_string(),
            });
        }
    }
    let fitted = match fitted {
        Some(fitted) => fitted,
        None => {
            let mut jpeg = Vec::new();
            image::DynamicImage::ImageRgb8(flatten_on_white(&scaled))
                .write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(
                    &mut jpeg,
                    JPEG_QUALITY,
                ))
                .map_err(encode_failed)?;
            PromptImage {
                data: jpeg,
                media_type: "image/jpeg".to_string(),
            }
        },
    };
    if fitted.data.len() > MAX_PROMPT_IMAGE_BYTES {
        return Err(format!(
            "{file} ({size}) is still {} after scaling, over the {} provider limit",
            describe_bytes(fitted.data.len()),
            describe_bytes(MAX_PROMPT_IMAGE_BYTES)
        ));
    }
    Ok(fitted)
}

/// JPEG has no alpha: composite over white, so transparent areas do not turn
/// black.
fn flatten_on_white(image: &image::DynamicImage) -> image::RgbImage {
    if !image.color().has_alpha() {
        return image.to_rgb8();
    }
    let rgba = image.to_rgba8();
    image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [red, green, blue, alpha] = rgba.get_pixel(x, y).0;
        let alpha = u32::from(alpha);
        let over_white =
            |channel: u8| ((u32::from(channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8;
        image::Rgb([over_white(red), over_white(green), over_white(blue)])
    })
}

fn describe_bytes(bytes: usize) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbImage::from_pixel(width, height, image::Rgb([200, 30, 30]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("encode");
        out
    }

    /// Pseudo-random pixels, which PNG cannot compress.
    fn noise_png(width: u32, height: u32) -> Vec<u8> {
        let mut state = 0x2545_f491_u32;
        let image = image::RgbImage::from_fn(width, height, |_, _| {
            let mut next = || {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            };
            image::Rgb([next(), next(), next()])
        });
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("encode");
        out
    }

    /// A JPEG carrying an EXIF APP1 segment with the given orientation tag,
    /// inserted right after SOI.
    fn jpeg_with_orientation(width: u32, height: u32, orientation: u16) -> Vec<u8> {
        let image = image::RgbImage::from_pixel(width, height, image::Rgb([10, 120, 60]));
        let mut jpeg = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut jpeg, 80,
            ))
            .expect("encode");
        let mut exif = b"Exif\0\0".to_vec();
        // Little-endian TIFF header, IFD0 at offset 8 with one SHORT entry.
        exif.extend_from_slice(b"II\x2a\x00\x08\x00\x00\x00");
        exif.extend_from_slice(&1_u16.to_le_bytes());
        exif.extend_from_slice(&0x0112_u16.to_le_bytes());
        exif.extend_from_slice(&3_u16.to_le_bytes());
        exif.extend_from_slice(&1_u32.to_le_bytes());
        exif.extend_from_slice(&orientation.to_le_bytes());
        exif.extend_from_slice(&[0, 0]);
        exif.extend_from_slice(&0_u32.to_le_bytes());
        let mut segment = vec![0xFF, 0xE1];
        segment.extend_from_slice(&u16::try_from(exif.len() + 2).unwrap().to_be_bytes());
        segment.extend_from_slice(&exif);
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
        let mut with_exif = jpeg[..2].to_vec();
        with_exif.extend_from_slice(&segment);
        with_exif.extend_from_slice(&jpeg[2..]);
        with_exif
    }

    #[tokio::test]
    async fn an_image_within_limits_is_attached_byte_for_byte() {
        let small = solid_png(640, 480);
        let fitted = fit_prompt_image(small.clone(), "image/png", "small.png")
            .await
            .expect("fits");
        assert_eq!(fitted.data, small);
        assert_eq!(fitted.media_type, "image/png");

        let edge = solid_png(MAX_PROMPT_IMAGE_SIDE, 900);
        let fitted = fit_prompt_image(edge.clone(), "image/png", "edge.png")
            .await
            .expect("fits");
        assert_eq!(fitted.data, edge, "exactly the long side passes unchanged");

        // Bytes this build cannot read the header of are judged by size.
        let opaque = vec![7_u8; 2_300_000];
        let fitted = fit_prompt_image(opaque.clone(), "image/webp", "capture.webp")
            .await
            .expect("fits");
        assert_eq!(fitted.data, opaque);
        assert_eq!(fitted.media_type, "image/webp");
    }

    /// Small in bytes but past the long side: the request copy is scaled.
    #[tokio::test]
    async fn an_image_past_the_long_side_is_scaled_even_when_small() {
        let wide = solid_png(2000, 1000);
        assert!(wide.len() < MAX_PROMPT_IMAGE_BYTES);
        let fitted = fit_prompt_image(wide, "image/png", "wide.png")
            .await
            .expect("scaled");
        assert_eq!(fitted.media_type, "image/png");
        let decoded = image::load_from_memory(&fitted.data).expect("decodes");
        assert_eq!((decoded.width(), decoded.height()), (MAX_PROMPT_IMAGE_SIDE, 784));
    }

    #[tokio::test]
    async fn an_image_over_the_byte_limit_is_scaled_under_it() {
        let large = noise_png(2400, 1200);
        assert!(large.len() > MAX_PROMPT_IMAGE_BYTES, "{} bytes", large.len());
        let fitted = fit_prompt_image(large, "image/png", "noisy.png")
            .await
            .expect("scaled");
        assert!(fitted.data.len() <= MAX_PROMPT_IMAGE_BYTES, "{} bytes", fitted.data.len());
        assert!(matches!(fitted.media_type.as_str(), "image/png" | "image/jpeg"));
        let decoded = image::load_from_memory(&fitted.data).expect("decodes");
        assert_eq!((decoded.width(), decoded.height()), (MAX_PROMPT_IMAGE_SIDE, 784));
    }

    #[test]
    fn a_very_wide_image_is_scaled_to_the_long_side_once() {
        let wide = solid_png(9000, 300);
        assert!(!within_limits(&wide));
        let fitted = fit_cached(&wide, "panorama.png").expect("scaled");
        assert_eq!(fitted.media_type, "image/png");
        let decoded = image::load_from_memory(&fitted.data).expect("decodes");
        assert_eq!(decoded.width(), MAX_PROMPT_IMAGE_SIDE);
        assert!(within_limits(&fitted.data));
        // The second turn reuses the scaled copy.
        assert_eq!(fit_cached(&wide, "panorama.png").expect("cached"), fitted);
    }

    /// A phone photo stored landscape with "rotate 90° clockwise" in EXIF is
    /// sent upright: the re-encoded copy has no EXIF to carry the rotation.
    #[test]
    fn exif_orientation_is_applied_before_scaling() {
        let photo = jpeg_with_orientation(2000, 1000, 6);
        let fitted = fit_blocking(&photo, "photo.jpg").expect("scaled");
        assert_eq!(fitted.media_type, "image/jpeg");
        let decoded = image::load_from_memory(&fitted.data).expect("decodes");
        assert_eq!((decoded.width(), decoded.height()), (784, MAX_PROMPT_IMAGE_SIDE));

        let upright = jpeg_with_orientation(2000, 1000, 1);
        let decoded = image::load_from_memory(&fit_blocking(&upright, "upright.jpg").unwrap().data)
            .expect("decodes");
        assert_eq!((decoded.width(), decoded.height()), (MAX_PROMPT_IMAGE_SIDE, 784));
    }

    #[test]
    fn transparency_is_composited_over_white_for_jpeg() {
        let mut image = image::RgbaImage::from_pixel(2, 1, image::Rgba([0, 0, 0, 0]));
        image.put_pixel(1, 0, image::Rgba([200, 10, 10, 255]));
        let flat = flatten_on_white(&image::DynamicImage::ImageRgba8(image));
        assert_eq!(flat.get_pixel(0, 0).0, [255, 255, 255]);
        assert_eq!(flat.get_pixel(1, 0).0, [200, 10, 10]);
    }

    #[tokio::test]
    async fn an_oversized_image_that_cannot_be_decoded_names_the_file_once() {
        let broken = vec![0_u8; MAX_PROMPT_IMAGE_BYTES + 1];
        let block = prompt_image_block(broken.clone(), "image/png", None, "broken.png").await;
        let ContentBlock::Text { text } = block else {
            panic!("expected a note, got {block:?}");
        };
        assert!(text.starts_with("[image not attached: broken.png (3.9 MB)"), "{text}");
        // The failure is cached: a later turn gets the same note without
        // decoding (and warning) again.
        let key = *blake3::hash(&broken).as_bytes();
        let cached = FITTED.lock().unwrap().get(&key).expect("negative result cached");
        assert!(cached.is_err());
        assert_eq!(fit_cached(&broken, "broken.png"), (*cached).clone());
    }

    /// Same input, same bytes: a cached copy and a recomputed one agree.
    #[test]
    fn scaling_is_deterministic() {
        let wide = solid_png(2000, 1000);
        let first = fit_blocking(&wide, "wide.png").expect("scaled");
        assert_eq!(fit_blocking(&wide, "wide.png").expect("scaled"), first);
    }

    #[test]
    fn an_image_past_the_decode_bounds_is_refused_not_decoded() {
        let huge = solid_png(MAX_DECODE_SIDE + 1, 8);
        let error = fit_blocking(&huge, "banner.png").expect_err("past the decode bounds");
        assert!(error.starts_with("banner.png ("), "{error}");
    }

    #[test]
    fn the_fit_cache_is_bounded_by_bytes_and_keeps_the_newest() {
        let mut cache = FitCache::default();
        let copy = |bytes: usize| {
            Arc::new(Ok(PromptImage {
                data: vec![0_u8; bytes],
                media_type: "image/png".to_string(),
            }))
        };
        cache.put([1; 32], copy(FIT_CACHE_BYTES / 2 + 1));
        cache.put([2; 32], copy(FIT_CACHE_BYTES / 2 + 1));
        assert!(cache.get(&[1; 32]).is_none(), "over the byte budget evicts the oldest");
        assert!(cache.get(&[2; 32]).is_some());
        cache.put([3; 32], copy(FIT_CACHE_BYTES + 1));
        assert_eq!(cache.entries.len(), 1, "an entry alone over the budget still stays");
        assert!(cache.get(&[3; 32]).is_some());
        assert_eq!(cache.bytes, FIT_CACHE_BYTES + 1);
    }

    #[test]
    fn the_fit_cache_evicts_the_least_recently_used_entry() {
        let mut cache = FitCache::default();
        let key = |index: u8| [index; 32];
        for index in 0..FIT_CACHE_ENTRIES as u8 {
            cache.put(key(index), Arc::new(Err(format!("entry {index}"))));
        }
        assert!(cache.get(&key(0)).is_some(), "a hit promotes the oldest entry");
        cache.put(key(200), Arc::new(Err("newest".to_string())));
        assert_eq!(cache.entries.len(), FIT_CACHE_ENTRIES);
        assert!(cache.get(&key(0)).is_some(), "the promoted entry stays");
        assert!(cache.get(&key(1)).is_none(), "the least recently used is evicted");
    }
}
