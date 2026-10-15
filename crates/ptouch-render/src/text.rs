// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Text rendering using cosmic-text.
//!
//! Renders multi-line text into a [`LabelBitmap`] using system fonts.
//! Supports auto-sizing, alignment, and font selection.

use cosmic_text::{
    Align, Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Wrap,
};
use serde::{Deserialize, Serialize};

use crate::RenderError;
use crate::Result;
use crate::bitmap::LabelBitmap;

/// Text alignment within the label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextAlign {
    /// Align text to the left edge.
    Left,
    /// Center text horizontally.
    Center,
    /// Align text to the right edge.
    Right,
}

impl TextAlign {
    /// Convert to cosmic-text's alignment type.
    fn to_cosmic(self) -> Align {
        match self {
            TextAlign::Left => Align::Left,
            TextAlign::Center => Align::Center,
            TextAlign::Right => Align::Right,
        }
    }
}

/// Text renderer backed by cosmic-text.
pub struct TextRenderer {
    font_system: FontSystem,
    swash_cache: SwashCache,
}

impl Default for TextRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl TextRenderer {
    /// Create a new text renderer with system fonts loaded.
    pub fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
        }
    }

    /// Render a complete, unwrapped text block without tape-height clipping.
    /// The tight ink bounds include glyph bearings and preserve the alignment
    /// of explicit newline-separated lines. Used before box rotation/fitting.
    pub(crate) fn render_unbounded(
        &mut self,
        text: &str,
        font_name: &str,
        font_size_px: f32,
        align: TextAlign,
    ) -> Result<LabelBitmap> {
        if !font_size_px.is_finite() || font_size_px <= 0.0 || font_size_px > 4096.0 {
            return Err(RenderError::Text(
                "font size must be in (0, 4096] pixels".into(),
            ));
        }
        if text.trim().is_empty() {
            return Ok(LabelBitmap::new(1, 1));
        }
        let family = if font_name.is_empty() {
            Family::SansSerif
        } else {
            Family::Name(font_name)
        };
        let attrs = Attrs::new().family(family);
        let mut buffer = Buffer::new(
            &mut self.font_system,
            Metrics::new(font_size_px, (font_size_px * 1.2).ceil()),
        );
        buffer.set_wrap(Wrap::None);
        buffer.set_size(None, None);
        buffer.set_text(text, &attrs, Shaping::Advanced, Some(align.to_cosmic()));
        buffer.shape_until_scroll(&mut self.font_system, true);
        let width = buffer
            .layout_runs()
            .map(|run| run.line_w)
            .fold(0.0_f32, f32::max);
        let height = text.split('\n').count() as f32 * (font_size_px * 1.2).ceil();
        if !width.is_finite() || width > 1_000_000.0 || width * height > 16_777_216.0 {
            return Err(RenderError::DimensionMismatch(
                "text raster is too large".into(),
            ));
        }
        // Reflow alignment within the actual longest line instead of an
        // arbitrary large measurement surface. Never wrap long port names.
        buffer.set_size(Some(width.ceil().max(1.0)), None);
        buffer.shape_until_scroll(&mut self.font_system, true);
        let mut bounds: Option<(i32, i32, i32, i32)> = None;
        buffer.draw(
            &mut self.font_system,
            &mut self.swash_cache,
            Color::rgb(0, 0, 0),
            |x, y, w, h, color| {
                if color.a() >= 128 {
                    let right = x + w as i32;
                    let bottom = y + h as i32;
                    bounds = Some(match bounds {
                        Some((left, top, r, b)) => {
                            (left.min(x), top.min(y), r.max(right), b.max(bottom))
                        }
                        None => (x, y, right, bottom),
                    });
                }
            },
        );
        let Some((left, top, right, bottom)) = bounds else {
            if font_size_px < 4.0 {
                return Ok(LabelBitmap::new(1, 1));
            }
            return Err(RenderError::Text(
                "no visible glyphs; select an installed font".into(),
            ));
        };
        let width = (right - left) as u32;
        let height = (bottom - top) as u32;
        if u64::from(width) * u64::from(height) > 16_777_216 {
            return Err(RenderError::DimensionMismatch(
                "text raster is too large".into(),
            ));
        }
        let mut bitmap = LabelBitmap::new(width, height);
        buffer.draw(
            &mut self.font_system,
            &mut self.swash_cache,
            Color::rgb(0, 0, 0),
            |x, y, w, h, color| {
                if color.a() >= 128 {
                    for dy in 0..h {
                        for dx in 0..w {
                            bitmap.set_pixel((x - left) as u32 + dx, (y - top) as u32 + dy, true);
                        }
                    }
                }
            },
        );
        Ok(bitmap)
    }

    #[cfg(test)]
    pub(crate) fn with_test_font() -> Self {
        let mut db = cosmic_text::fontdb::Database::new();
        db.load_font_data(
            include_bytes!("../../ptouch-gui/assets/fonts/DroidSansFallback.ttf").to_vec(),
        );
        db.set_sans_serif_family("Droid Sans Fallback");
        let font_system = FontSystem::new_with_locale_and_db("en-US".into(), db);
        Self {
            font_system,
            swash_cache: SwashCache::new(),
        }
    }

    /// Render one or more lines of text into a [`LabelBitmap`].
    ///
    /// - `lines`: text lines to render (joined with newlines)
    /// - `print_width`: height of the tape in pixels (this becomes the bitmap
    ///   height; called "print_width" for compatibility with the C API where
    ///   the tape width is the printable width)
    /// - `font_name`: font family name to use (e.g. "DejaVu Sans")
    /// - `font_size`: explicit font size in raster pixels (legacy behavior),
    ///   or `None` for auto-detect; physical boxes convert points separately
    /// - `font_margin`: margin in pixels on each side (top and bottom)
    /// - `align`: horizontal text alignment
    ///
    /// The renderer will:
    /// 1. If font_size is None, auto-detect the largest size that fits
    /// 2. Render the text into a buffer
    /// 3. Convert the rendered glyphs to a 1-bit bitmap
    pub fn render_text(
        &mut self,
        lines: &[&str],
        print_width: u32,
        font_name: &str,
        font_size: Option<f32>,
        font_margin: u32,
        align: TextAlign,
    ) -> Result<LabelBitmap> {
        if print_width == 0 {
            return Err(RenderError::Text("print_width must be > 0".into()));
        }

        let text = lines.join("\n");
        if text.is_empty() {
            return Err(RenderError::Text("no text to render".into()));
        }

        let num_lines = lines.len() as f32;
        let available_height = print_width.saturating_sub(font_margin * 2) as f32;

        if available_height <= 0.0 {
            return Err(RenderError::Text(
                "font_margin too large for tape width".into(),
            ));
        }

        // Determine font size
        let font_size = font_size.unwrap_or_else(|| {
            // Auto-detect: largest size where all lines fit vertically.
            // Line height ~ font_size * 1.2, total ~ line_height * num_lines
            let size = available_height / (num_lines * 1.2);
            size.max(4.0)
        });

        let line_height = (font_size * 1.2).ceil();
        let metrics = Metrics::new(font_size, line_height);

        let family = if font_name.is_empty() {
            Family::SansSerif
        } else {
            Family::Name(font_name)
        };
        let attrs = Attrs::new().family(family);
        let cosmic_align = Some(align.to_cosmic());

        // Labels have no horizontal limit and only explicit line breaks.
        // Measure without alignment first: center/right alignment needs a
        // finite width, which must accommodate even the longest label.
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_wrap(Wrap::None);
        buffer.set_size(None, Some(available_height));
        buffer.set_text(&text, &attrs, Shaping::Advanced, Some(Align::Left));
        buffer.shape_until_scroll(&mut self.font_system, true);
        let layout_width = buffer
            .layout_runs()
            .map(|run| run.line_w)
            .fold(0.0_f32, f32::max)
            .ceil()
            .max(1.0);
        buffer.set_size(Some(layout_width), Some(available_height));
        buffer.set_text(&text, &attrs, Shaping::Advanced, cosmic_align);
        buffer.shape_until_scroll(&mut self.font_system, true);

        // Measure tight horizontal extent (min_x..max_x) from layout runs.
        // Alignment offsets shorter lines within the measured layout width.
        // Subtract min_x so the bitmap is tight.
        let mut min_x: f32 = f32::MAX;
        let mut max_x: f32 = 0.0;
        for run in buffer.layout_runs() {
            for g in run.glyphs.iter() {
                min_x = min_x.min(g.x);
                max_x = max_x.max(g.x + g.w);
            }
        }
        if min_x == f32::MAX {
            min_x = 0.0;
        }

        let bitmap_width = ((max_x - min_x).ceil() as u32).max(1);
        let bitmap_height = print_width;
        let x_offset = min_x.floor() as i32;

        let mut bitmap = LabelBitmap::new(bitmap_width, bitmap_height);

        // Vertical centering offset
        let total_text_height = (num_lines * line_height) as u32;
        let y_offset = if total_text_height < bitmap_height {
            ((bitmap_height - total_text_height) / 2) as i32
        } else {
            font_margin as i32
        };

        // Draw glyphs onto the bitmap
        let text_color = Color::rgb(0, 0, 0);
        buffer.draw(
            &mut self.font_system,
            &mut self.swash_cache,
            text_color,
            |x, y, w, h, color| {
                let alpha = color.a();
                if alpha < 128 {
                    return;
                }
                let px = x - x_offset;
                let py = y + y_offset;
                for dy in 0..h as i32 {
                    for dx in 0..w as i32 {
                        let fx = px + dx;
                        let fy = py + dy;
                        if fx >= 0 && fy >= 0 {
                            bitmap.set_pixel(fx as u32, fy as u32, true);
                        }
                    }
                }
            },
        );

        Ok(bitmap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measured_width(renderer: &mut TextRenderer, text: &str, size: f32) -> f32 {
        let mut buffer = Buffer::new(
            &mut renderer.font_system,
            Metrics::new(size, (size * 1.2).ceil()),
        );
        buffer.set_wrap(Wrap::None);
        buffer.set_size(None, None);
        buffer.set_text(
            text,
            &Attrs::new().family(Family::Name("monospace")),
            Shaping::Advanced,
            Some(Align::Left),
        );
        buffer.shape_until_scroll(&mut renderer.font_system, true);
        buffer
            .layout_runs()
            .map(|run| run.line_w)
            .fold(0.0, f32::max)
    }

    fn ink_bounds(bitmap: &LabelBitmap, start_y: u32, end_y: u32) -> (u32, u32) {
        let columns: Vec<_> = (0..bitmap.width())
            .filter(|&x| (start_y..end_y).any(|y| bitmap.get_pixel(x, y)))
            .collect();
        (
            *columns.first().expect("visible text"),
            *columns.last().unwrap(),
        )
    }

    #[test]
    fn long_labels_preserve_suffix_at_every_printer_resolution() {
        use crate::document::{LabelElement, render_elements_at_resolution};
        use ptouch_core::quality::QualityProfile;

        let mut renderer = TextRenderer::new();
        let advance = measured_width(&mut renderer, "M", 24.0);
        assert!(advance > 0.0);
        // The first label crosses the old limit only at high resolution;
        // the second already crosses it at standard resolution.
        for target_width in [12_000.0, 20_000.0] {
            let count = (target_width / advance).ceil() as usize;
            let prefix = "M".repeat(count);
            let content = format!("{prefix}   END");
            let expected_width = measured_width(&mut renderer, &content, 24.0).ceil() as u32;
            assert_eq!(expected_width > 16_384, target_width > 16_384.0);
            for (name, head, height) in [
                ("PT-P750W", 180, 70),
                ("PT-9500PC", 360, 150),
                ("PT-9700PC", 360, 150),
            ] {
                let elements = [LabelElement::Text {
                    text_box: None,
                    content: content.clone(),
                    font_size: Some(24.0),
                    align: TextAlign::Left,
                    rotation: 0.0,
                    flip_h: false,
                    flip_v: false,
                }];
                for mode in QualityProfile::for_model(name, head).modes {
                    let bitmap = render_elements_at_resolution(
                        &elements,
                        height,
                        "monospace",
                        0,
                        &mut renderer,
                        mode.resolution,
                    )
                    .unwrap()
                    .unwrap();
                    let ratio = f32::from(mode.resolution.feed_dpi) / f32::from(head);
                    assert_eq!(bitmap.height(), height);
                    assert!(
                        bitmap
                            .width()
                            .abs_diff((expected_width as f32 * ratio).ceil() as u32)
                            <= 2,
                        "{name} {mode:?}: {}",
                        bitmap.width()
                    );
                    // Ink beyond the measured prefix proves the suffix survived.
                    let suffix_start =
                        measured_width(&mut renderer, &format!("{prefix}   "), 24.0) * ratio;
                    assert!(
                        (suffix_start.ceil() as u32..bitmap.width())
                            .any(|x| (0..height).any(|y| bitmap.get_pixel(x, y))),
                        "missing suffix: {name} {mode:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn explicit_lines_preserve_alignment() {
        let mut renderer = TextRenderer::new();
        let long = "MMMMMMMM";
        let short = "MM";
        let width_difference =
            measured_width(&mut renderer, long, 24.0) - measured_width(&mut renderer, short, 24.0);
        let mut offsets = Vec::new();
        for align in [TextAlign::Left, TextAlign::Center, TextAlign::Right] {
            let bitmap = renderer
                .render_text(&[long, short], 80, "monospace", Some(24.0), 0, align)
                .unwrap();
            let top = ink_bounds(&bitmap, 0, 40);
            let bottom = ink_bounds(&bitmap, 40, 80);
            offsets.push(bottom.0 as f32 - top.0 as f32);
            assert!((top.1 - top.0) > (bottom.1 - bottom.0));
        }
        for (offset, expected) in
            offsets
                .into_iter()
                .zip([0.0, width_difference / 2.0, width_difference])
        {
            assert!(
                (offset - expected).abs() <= 1.0,
                "offset {offset}, expected {expected}"
            );
        }
    }

    #[test]
    fn test_text_align_conversion() {
        assert_eq!(TextAlign::Left.to_cosmic(), Align::Left);
        assert_eq!(TextAlign::Center.to_cosmic(), Align::Center);
        assert_eq!(TextAlign::Right.to_cosmic(), Align::Right);
    }

    #[test]
    fn test_empty_text_error() {
        let mut renderer = TextRenderer::new();
        let result = renderer.render_text(&[], 64, "sans-serif", None, 2, TextAlign::Center);
        assert!(result.is_err());
    }

    #[test]
    fn test_zero_width_error() {
        let mut renderer = TextRenderer::new();
        let result = renderer.render_text(&["hello"], 0, "sans-serif", None, 0, TextAlign::Center);
        assert!(result.is_err());
    }

    #[test]
    fn test_render_basic_text() {
        let mut renderer = TextRenderer::new();
        // Use a generic family so it works even without specific fonts
        let result = renderer.render_text(&["Test"], 64, "", Some(24.0), 2, TextAlign::Left);
        // This may succeed or fail depending on available system fonts.
        // We just verify it does not panic.
        if let Ok(bmp) = result {
            assert!(bmp.width() > 0);
            assert_eq!(bmp.height(), 64);
        }
    }
}
