//! TrueType text rendering onto the E Ink framebuffer.
//!
//! The 5x7 bitmap font in `text.rs` stays for the DEV badge — it needs no
//! external file and cannot fail. This module is for content, where 300 ppi
//! makes a bitmap font look like a toy.
//!
//! Antialiasing is essentially free here: fontdue emits 8-bit coverage per
//! pixel and the panel is 8bpp greyscale, so coverage blends straight into the
//! framebuffer with no conversion or dithering.

use crate::fb::Framebuffer;
use std::io;

pub struct Font {
    inner: fontdue::Font,
}

/// Where a block of text ended, so callers can continue below it.
pub struct Laid {
    pub next_y: u32,
    pub lines: u32,
}

impl Font {
    pub fn load(path: &str) -> io::Result<Font> {
        let data = std::fs::read(path)?;
        let inner = fontdue::Font::from_bytes(data, fontdue::FontSettings::default())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        Ok(Font { inner })
    }

    /// Distance between baselines at this size.
    pub fn line_height(&self, px: f32) -> u32 {
        match self.inner.horizontal_line_metrics(px) {
            Some(m) => (m.ascent - m.descent + m.line_gap).ceil().max(1.0) as u32,
            None => (px * 1.2).ceil() as u32,
        }
    }

    pub fn ascent(&self, px: f32) -> u32 {
        match self.inner.horizontal_line_metrics(px) {
            Some(m) => m.ascent.ceil().max(0.0) as u32,
            None => px.ceil() as u32,
        }
    }

    pub fn measure(&self, text: &str, px: f32) -> u32 {
        text.chars()
            .map(|c| self.inner.metrics(c, px).advance_width)
            .sum::<f32>()
            .ceil()
            .max(0.0) as u32
    }

    /// Draw one line with its **baseline** at `baseline_y`. Returns the pen x
    /// after the last glyph.
    pub fn draw(
        &self,
        fb: &mut Framebuffer,
        x: u32,
        baseline_y: u32,
        px: f32,
        text: &str,
        fg: u8,
    ) -> u32 {
        let mut pen = x as f32;
        for ch in text.chars() {
            let (metrics, bitmap) = self.inner.rasterize(ch, px);
            let gx = pen as i32 + metrics.xmin;
            // ymin is the offset of the glyph's bottom edge from the baseline,
            // measured upward, so the top edge sits this far above it.
            let gy = baseline_y as i32 - (metrics.height as i32 + metrics.ymin);

            for row in 0..metrics.height {
                for col in 0..metrics.width {
                    let coverage = bitmap[row * metrics.width + col];
                    if coverage == 0 {
                        continue;
                    }
                    let px_x = gx + col as i32;
                    let px_y = gy + row as i32;
                    if px_x < 0 || px_y < 0 {
                        continue;
                    }
                    let (px_x, px_y) = (px_x as u32, px_y as u32);
                    // Blend fg over whatever is already there, by coverage.
                    let bg = fb.get(px_x, px_y) as i32;
                    let blended = bg + (fg as i32 - bg) * coverage as i32 / 255;
                    fb.set(px_x, px_y, blended.clamp(0, 255) as u8);
                }
            }
            pen += metrics.advance_width;
        }
        pen.ceil() as u32
    }

    /// Draw `text` word-wrapped inside a box, starting at its top-left.
    ///
    /// Wrapping is per-word with a hard break for words longer than the line,
    /// which is enough for status text and short prose. Real typography —
    /// hyphenation, justification — is a later problem.
    pub fn draw_wrapped(
        &self,
        fb: &mut Framebuffer,
        x: u32,
        top_y: u32,
        width: u32,
        px: f32,
        text: &str,
        fg: u8,
    ) -> Laid {
        let lh = self.line_height(px);
        let ascent = self.ascent(px);
        let space = self.measure(" ", px);

        let mut baseline = top_y + ascent;
        let mut lines = 0u32;
        let mut line = String::new();
        let mut line_w = 0u32;

        let flush = |line: &mut String,
                     line_w: &mut u32,
                     baseline: &mut u32,
                     lines: &mut u32,
                     fb: &mut Framebuffer| {
            if !line.is_empty() {
                self.draw(fb, x, *baseline, px, line, fg);
                *baseline += lh;
                *lines += 1;
                line.clear();
                *line_w = 0;
            }
        };

        for word in text.split_whitespace() {
            let w = self.measure(word, px);
            let need = if line.is_empty() {
                w
            } else {
                line_w + space + w
            };
            if need > width && !line.is_empty() {
                flush(&mut line, &mut line_w, &mut baseline, &mut lines, fb);
            }
            if line.is_empty() {
                line.push_str(word);
                line_w = w;
            } else {
                line.push(' ');
                line.push_str(word);
                line_w += space + w;
            }
        }
        flush(&mut line, &mut line_w, &mut baseline, &mut lines, fb);

        Laid {
            next_y: baseline.saturating_sub(ascent),
            lines,
        }
    }
}
