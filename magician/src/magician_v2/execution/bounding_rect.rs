//! Bounding rectangle type used by the execution layer for element
//! positioning. Pure data — no image dep — kept in magician so the
//! retired SoM screenshot-annotation chain (and its `image` /
//! `imageproc` / `ab_glyph` stack) can be deleted entirely.
//!
//! Re-exported via `execution::mod` as `SoMBoundingRect` for
//! backwards compat with the 22 consumers in `execution::types`.

use serde::{Deserialize, Serialize};

/// Bounding rectangle for element positioning.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct BoundingRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl BoundingRect {
    /// Create a new bounding rect.
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Get center point of the rect (for coordinate-based click fallback).
    pub fn center(&self) -> (i32, i32) {
        (
            self.x + (self.width as i32 / 2),
            self.y + (self.height as i32 / 2),
        )
    }

    /// Check if the rect is within visible bounds.
    pub fn is_visible(&self, image_width: u32, image_height: u32) -> bool {
        self.x >= 0
            && self.y >= 0
            && self.x < image_width as i32
            && self.y < image_height as i32
            && self.width > 0
            && self.height > 0
    }
}
