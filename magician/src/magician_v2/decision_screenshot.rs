//! Screenshots for the Decision Engine's image-capable models.
//!
//! Captures rarely fit the provider's limits as taken: a browser page over CDP
//! on a Retina display is 2560 px wide, a full-page capture is larger still
//! (4160x3200 in a live run), and big phones exceed 1568 px on their long
//! side. Those are scaled down to fit here rather than dropped; one that would
//! end up thinner than the minimum side is left off and the step decides from
//! text.

use decision_engine_contract::asset::{
    AssetError, DecisionAsset, ImageAsset, MAX_IMAGE_SIDE, MIN_IMAGE_SIDE,
};
use serde::Serialize;

/// What became of one screenshot, recorded on the run so it shows whether
/// the decision model saw the screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ScreenshotNote {
    pub attached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// As captured.
    pub width: u32,
    pub height: u32,
    /// As sent, when it was scaled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scaled_to: Option<(u32, u32)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The screenshot as a decision asset under a content-derived reference key,
/// scaled to fit when it is too large. Decoding runs off the async runtime.
pub(crate) async fn fit(bytes: Vec<u8>) -> (Option<(String, DecisionAsset)>, ScreenshotNote) {
    match tokio::task::spawn_blocking(move || fit_blocking(&bytes)).await {
        Ok(fitted) => fitted,
        Err(error) => (
            None,
            ScreenshotNote {
                attached: false,
                key: None,
                width: 0,
                height: 0,
                scaled_to: None,
                reason: Some(format!("screenshot fit task failed: {error}")),
            },
        ),
    }
}

pub(crate) fn fit_blocking(bytes: &[u8]) -> (Option<(String, DecisionAsset)>, ScreenshotNote) {
    let key = format!("screen-{}", &blake3::hash(bytes).to_hex()[..16]);
    let refused = |width: u32, height: u32, reason: String| {
        (
            None,
            ScreenshotNote {
                attached: false,
                key: None,
                width,
                height,
                scaled_to: None,
                reason: Some(reason),
            },
        )
    };
    let attached = |asset: ImageAsset, width: u32, height: u32, scaled: bool| {
        let note = ScreenshotNote {
            attached: true,
            key: Some(key.clone()),
            width,
            height,
            scaled_to: scaled.then_some((asset.width, asset.height)),
            reason: None,
        };
        (Some((key.clone(), DecisionAsset::Image(asset))), note)
    };
    match ImageAsset::from_bytes(bytes) {
        Ok(asset) => {
            let (width, height) = (asset.width, asset.height);
            return attached(asset, width, height, false);
        },
        // Size problems are what scaling fixes; anything else (not a PNG or
        // JPEG, a damaged header, an animation) is final.
        Err(AssetError::Dimensions { .. } | AssetError::TooLarge(_)) => {},
        Err(error) => return refused(0, 0, error.to_string()),
    }
    let image = match image::load_from_memory(bytes) {
        Ok(image) => image,
        Err(error) => return refused(0, 0, format!("screenshot could not be decoded: {error}")),
    };
    let (width, height) = (image.width(), image.height());
    let scale = (f64::from(MAX_IMAGE_SIDE) / f64::from(width.max(height))).min(1.0);
    let scaled = (
        ((f64::from(width) * scale).round() as u32).max(1),
        ((f64::from(height) * scale).round() as u32).max(1),
    );
    if scaled.0.min(scaled.1) < MIN_IMAGE_SIDE {
        return refused(
            width,
            height,
            format!(
                "{width}x{height} cannot keep both sides within \
                 {MIN_IMAGE_SIDE}..={MAX_IMAGE_SIDE} px"
            ),
        );
    }
    let resized = image.resize_exact(scaled.0, scaled.1, image::imageops::FilterType::Triangle);
    let mut png = Vec::new();
    if let Err(error) =
        resized.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
    {
        return refused(
            width,
            height,
            format!("scaled screenshot could not be encoded: {error}"),
        );
    }
    match ImageAsset::from_bytes(&png) {
        Ok(asset) => attached(asset, width, height, true),
        Err(error) => refused(width, height, error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbImage::from_pixel(width, height, image::Rgb([200, 30, 30]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("encode");
        out
    }

    #[test]
    fn a_fitting_screenshot_is_sent_as_captured() {
        let (asset, note) = fit_blocking(&png(1280, 800));
        let Some((key, DecisionAsset::Image(image))) = asset else {
            panic!("attached: {note:?}");
        };
        assert!(key.starts_with("screen-") && note.attached);
        assert_eq!((image.width, image.height), (1280, 800));
        assert_eq!(note.scaled_to, None);
    }

    /// A Retina full-page capture from a live run.
    #[test]
    fn an_oversized_screenshot_is_scaled_to_the_long_side_limit() {
        let (asset, note) = fit_blocking(&png(4160, 3200));
        let Some((_, DecisionAsset::Image(image))) = asset else {
            panic!("attached: {note:?}");
        };
        assert_eq!((image.width, image.height), (1568, 1206));
        assert_eq!((note.width, note.height), (4160, 3200));
        assert_eq!(note.scaled_to, Some((1568, 1206)));
        image.validate().expect("within limits");
    }

    #[test]
    fn a_screenshot_too_thin_to_scale_is_left_off() {
        let (asset, note) = fit_blocking(&png(1280, 200));
        assert!(asset.is_none());
        assert!(!note.attached);
        assert!(note.reason.unwrap().contains("1280x200"));
    }
}
