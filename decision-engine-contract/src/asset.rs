//! Images a decision looks at.
//!
//! A request lists each image once in [`crate::request::DecisionRequest::assets`]
//! under a reference key, and the state names the image by writing that key
//! where the image belongs, e.g. `{"screen": "screen-7f3a9c21", "goal": ".."}`.
//! The System One dialect expands every occurrence of the key, so keys are
//! long, distinctive tokens ([`is_valid_reference`]) that cannot collide with
//! ordinary text.
//!
//! Only models whose capabilities accept images can take a request that
//! carries any; every other model on the route is skipped, never sent the
//! bare reference. Limits mirror the hosted Jev contract and are checked
//! here, before dispatch, because the provider rejects (never resizes) an
//! out-of-range image and fails the whole request.

use std::collections::BTreeMap;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// Largest encoded image file accepted.
pub const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;
/// Each side must fall inside this range, inclusive.
pub const MIN_IMAGE_SIDE: u32 = 280;
pub const MAX_IMAGE_SIDE: u32 = 1568;
/// Reference keys: per key and across all keys of one request.
pub const MAX_REFERENCE_BYTES: usize = 1024;
pub const MAX_REFERENCES_TOTAL_BYTES: usize = 65_536;
const MIN_REFERENCE_BYTES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    Png,
    Jpeg,
}

impl ImageFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DecisionAsset {
    Image(ImageAsset),
}

/// A still PNG or JPEG, carried base64-encoded so it crosses the host →
/// engine → provider hops without re-encoding. Build it with
/// [`ImageAsset::from_bytes`]; anything deserialized is re-checked with
/// [`ImageAsset::validate`] before dispatch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageAsset {
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
    pub data_base64: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AssetError {
    #[error("image is not a PNG or JPEG")]
    UnsupportedFormat,
    #[error("image header is damaged")]
    Damaged,
    #[error("animated PNGs are not accepted")]
    Animated,
    #[error("image is {0} bytes, the limit is {MAX_IMAGE_BYTES}")]
    TooLarge(usize),
    #[error("image is {width}x{height}, each side must be {MIN_IMAGE_SIDE}..={MAX_IMAGE_SIDE} px")]
    Dimensions { width: u32, height: u32 },
    #[error("image data is not valid base64")]
    Base64,
    #[error("image declares {declared} but its data is {actual}")]
    Mismatch { declared: String, actual: String },
    #[error(
        "asset reference '{0}' must be 8..=1024 bytes of letters, digits, '.', '_', ':' or '-'"
    )]
    Reference(String),
    #[error("asset references total more than {MAX_REFERENCES_TOTAL_BYTES} bytes")]
    ReferencesTooLong,
    #[error("asset '{0}' is not referenced in the state")]
    Unreferenced(String),
}

impl ImageAsset {
    /// Sniff the format and size, check the limits, and encode.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, AssetError> {
        let (format, width, height) = inspect(bytes)?;
        Ok(Self {
            format,
            width,
            height,
            data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        })
    }

    /// Decode and re-inspect: the declared format and size must match the
    /// data, and the data must be within limits.
    pub fn validate(&self) -> Result<(), AssetError> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(self.data_base64.as_bytes())
            .map_err(|_| AssetError::Base64)?;
        let (format, width, height) = inspect(&bytes)?;
        if (format, width, height) != (self.format, self.width, self.height) {
            return Err(AssetError::Mismatch {
                declared: format!("{} {}x{}", self.format.as_str(), self.width, self.height),
                actual: format!("{} {width}x{height}", format.as_str()),
            });
        }
        Ok(())
    }
}

/// A reference must be distinctive enough that the provider's plain-text
/// expansion only ever hits the places the host wrote it.
pub fn is_valid_reference(key: &str) -> bool {
    (MIN_REFERENCE_BYTES..=MAX_REFERENCE_BYTES).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// Check a request's assets: valid images, valid keys, and every key
/// written somewhere in the state (the provider rejects unreferenced ones).
pub fn validate_assets(
    assets: &BTreeMap<String, DecisionAsset>,
    state: &serde_json::Value,
) -> Result<(), AssetError> {
    let mut total = 0usize;
    for (key, asset) in assets {
        if !is_valid_reference(key) {
            return Err(AssetError::Reference(key.clone()));
        }
        total += key.len();
        if total > MAX_REFERENCES_TOTAL_BYTES {
            return Err(AssetError::ReferencesTooLong);
        }
        if !mentions(state, key) {
            return Err(AssetError::Unreferenced(key.clone()));
        }
        match asset {
            DecisionAsset::Image(image) => image.validate()?,
        }
    }
    Ok(())
}

fn mentions(value: &serde_json::Value, key: &str) -> bool {
    match value {
        serde_json::Value::String(text) => text.contains(key),
        serde_json::Value::Array(items) => items.iter().any(|item| mentions(item, key)),
        // Object keys are never expanded, only values.
        serde_json::Value::Object(map) => map.values().any(|item| mentions(item, key)),
        _ => false,
    }
}

fn inspect(bytes: &[u8]) -> Result<(ImageFormat, u32, u32), AssetError> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(AssetError::TooLarge(bytes.len()));
    }
    let (format, width, height) = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let (width, height) = png_size(bytes)?;
        (ImageFormat::Png, width, height)
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        let (width, height) = jpeg_size(bytes)?;
        (ImageFormat::Jpeg, width, height)
    } else {
        return Err(AssetError::UnsupportedFormat);
    };
    let side = MIN_IMAGE_SIDE..=MAX_IMAGE_SIDE;
    if !side.contains(&width) || !side.contains(&height) {
        return Err(AssetError::Dimensions { width, height });
    }
    Ok((format, width, height))
}

/// IHDR is the first chunk; an `acTL` chunk before the image data marks an
/// animated PNG.
fn png_size(bytes: &[u8]) -> Result<(u32, u32), AssetError> {
    let be32 = |at: usize| -> Option<u32> {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    if bytes.get(12..16) != Some(b"IHDR") {
        return Err(AssetError::Damaged);
    }
    let (width, height) = (
        be32(16).ok_or(AssetError::Damaged)?,
        be32(20).ok_or(AssetError::Damaged)?,
    );
    let mut at = 8usize;
    while let (Some(length), Some(kind)) = (be32(at), bytes.get(at + 4..at + 8)) {
        match kind {
            b"acTL" => return Err(AssetError::Animated),
            b"IDAT" | b"IEND" => break,
            _ => {},
        }
        at = at
            .checked_add(12 + length as usize)
            .ok_or(AssetError::Damaged)?;
    }
    Ok((width, height))
}

/// Walk the marker segments to the first start-of-frame.
fn jpeg_size(bytes: &[u8]) -> Result<(u32, u32), AssetError> {
    let mut at = 2usize;
    loop {
        while bytes.get(at) == Some(&0xFF) && bytes.get(at + 1) == Some(&0xFF) {
            at += 1;
        }
        let (Some(&0xFF), Some(&marker)) = (bytes.get(at), bytes.get(at + 1)) else {
            return Err(AssetError::Damaged);
        };
        at += 2;
        // Standalone markers carry no length.
        if matches!(marker, 0x01 | 0xD0..=0xD7) {
            continue;
        }
        if marker == 0xD9 || marker == 0xDA {
            return Err(AssetError::Damaged);
        }
        let length = bytes
            .get(at..at + 2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
            .ok_or(AssetError::Damaged)?;
        let is_frame = matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_frame {
            let frame = bytes.get(at + 3..at + 7).ok_or(AssetError::Damaged)?;
            let height = u16::from_be_bytes([frame[0], frame[1]]) as u32;
            let width = u16::from_be_bytes([frame[2], frame[3]]) as u32;
            return Ok((width, height));
        }
        if length < 2 {
            return Err(AssetError::Damaged);
        }
        at += length;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(b"IDAT");
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes
    }

    fn jpeg(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8];
        // An APP0 segment before the frame header.
        bytes.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x4A, 0x46]);
        bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&[0x01, 0x01, 0x11, 0x00, 0xFF, 0xD9]);
        bytes
    }

    #[test]
    fn reads_png_and_jpeg_sizes() {
        let image = ImageAsset::from_bytes(&png(1280, 800)).unwrap();
        assert_eq!(
            (image.format, image.width, image.height),
            (ImageFormat::Png, 1280, 800)
        );
        image.validate().unwrap();
        let image = ImageAsset::from_bytes(&jpeg(1568, 980)).unwrap();
        assert_eq!(
            (image.format, image.width, image.height),
            (ImageFormat::Jpeg, 1568, 980)
        );
    }

    #[test]
    fn rejects_out_of_range_animated_and_unknown_images() {
        assert_eq!(
            ImageAsset::from_bytes(&png(2880, 1800)),
            Err(AssetError::Dimensions {
                width: 2880,
                height: 1800
            })
        );
        assert!(matches!(
            ImageAsset::from_bytes(&jpeg(279, 600)),
            Err(AssetError::Dimensions { .. })
        ));
        let mut animated = png(800, 600);
        let idat = animated.len() - 12;
        let actl = [&8u32.to_be_bytes()[..], b"acTL", &[0; 8], &[0; 4]].concat();
        animated.splice(idat..idat, actl);
        assert_eq!(ImageAsset::from_bytes(&animated), Err(AssetError::Animated));
        assert_eq!(
            ImageAsset::from_bytes(b"GIF89a...."),
            Err(AssetError::UnsupportedFormat)
        );
    }

    #[test]
    fn validate_catches_a_relabelled_image() {
        let mut image = ImageAsset::from_bytes(&png(800, 600)).unwrap();
        image.width = 801;
        assert!(matches!(image.validate(), Err(AssetError::Mismatch { .. })));
    }

    #[test]
    fn assets_must_be_well_named_and_referenced() {
        let image = DecisionAsset::Image(ImageAsset::from_bytes(&png(800, 600)).unwrap());
        let state = serde_json::json!({"screen": "screen-7f3a9c21", "goal": "log in"});
        let assets = BTreeMap::from([("screen-7f3a9c21".to_string(), image.clone())]);
        validate_assets(&assets, &state).unwrap();

        let unreferenced = BTreeMap::from([("screen-00000000".to_string(), image.clone())]);
        assert_eq!(
            validate_assets(&unreferenced, &state),
            Err(AssetError::Unreferenced("screen-00000000".into()))
        );
        let short = BTreeMap::from([("img".to_string(), image)]);
        assert!(matches!(
            validate_assets(&short, &state),
            Err(AssetError::Reference(_))
        ));
    }

    #[test]
    fn serializes_in_the_provider_shape() {
        let asset = DecisionAsset::Image(ImageAsset::from_bytes(&png(800, 600)).unwrap());
        let json = serde_json::to_value(&asset).unwrap();
        assert_eq!(json["type"], "image");
        assert_eq!(json["format"], "png");
        assert!(json["data_base64"]
            .as_str()
            .is_some_and(|data| !data.contains('\n')));
    }
}
