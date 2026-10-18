// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Monochrome ink with an optional coverage mask for opaque paper pixels.

use crate::{RenderError, Result, bitmap::LabelBitmap};

/// `coverage == None` retains the historical transparent-white ink overlay.
/// A set coverage bit replaces the destination with the corresponding ink bit.
#[derive(Debug, Clone, PartialEq)]
pub struct RasterLayer {
    pub ink: LabelBitmap,
    pub coverage: Option<LabelBitmap>,
}

impl RasterLayer {
    pub fn from_ink(ink: LabelBitmap) -> Self {
        Self {
            ink,
            coverage: None,
        }
    }

    pub fn validate(&self) -> Result<()> {
        let pixels = u64::from(self.ink.width()) * u64::from(self.ink.height());
        // Legacy Padding(0) is a valid empty-width ink segment. It occupies
        // no pixels and must keep its original document/conversion behavior.
        // A new masked layer still needs a positive raster in both axes.
        if self.ink.height() == 0
            || self.ink.width() == 0 && self.coverage.is_some()
            || pixels > 64 * 1024 * 1024
        {
            return Err(RenderError::Layout(
                "layer raster is too large or empty".into(),
            ));
        }
        if let Some(coverage) = &self.coverage
            && (coverage.width() != self.ink.width() || coverage.height() != self.ink.height())
        {
            return Err(RenderError::DimensionMismatch(
                "ink and coverage dimensions differ".into(),
            ));
        }
        Ok(())
    }

    pub fn width(&self) -> u32 {
        self.ink.width()
    }
    pub fn height(&self) -> u32 {
        self.ink.height()
    }

    pub fn mirrored(&self, horizontal: bool, vertical: bool) -> Self {
        Self {
            ink: self.ink.mirrored(horizontal, vertical),
            coverage: self
                .coverage
                .as_ref()
                .map(|mask| mask.mirrored(horizontal, vertical)),
        }
    }

    /// Composite clipped pixels. Transparent source pixels leave the tape intact.
    pub fn paint(&self, target: &mut LabelBitmap, left: i64, top: i64) {
        let x0 = left.saturating_neg().clamp(0, i64::from(self.width())) as u32;
        let y0 = top.saturating_neg().clamp(0, i64::from(self.height())) as u32;
        let x1 = i64::from(target.width())
            .saturating_sub(left)
            .clamp(0, i64::from(self.width())) as u32;
        let y1 = i64::from(target.height())
            .saturating_sub(top)
            .clamp(0, i64::from(self.height())) as u32;
        for y in y0..y1 {
            for x in x0..x1 {
                let ink = self.ink.get_pixel(x, y);
                if self
                    .coverage
                    .as_ref()
                    .map_or(ink, |mask| mask.get_pixel(x, y))
                {
                    target.set_pixel(
                        (left + i64::from(x)) as u32,
                        (top + i64::from(y)) as u32,
                        ink,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covered_paper_erases_ink_while_transparent_white_preserves_it() {
        let mut tape = LabelBitmap::new(3, 1);
        for x in 0..3 {
            tape.set_pixel(x, 0, true);
        }
        let mut coverage = LabelBitmap::new(3, 1);
        coverage.set_pixel(1, 0, true);
        RasterLayer {
            ink: LabelBitmap::new(3, 1),
            coverage: Some(coverage),
        }
        .paint(&mut tape, 0, 0);
        assert!(tape.get_pixel(0, 0));
        assert!(!tape.get_pixel(1, 0));
        assert!(tape.get_pixel(2, 0));
        RasterLayer::from_ink(LabelBitmap::new(3, 1)).paint(&mut tape, 0, 0);
        assert!(tape.get_pixel(2, 0));
    }

    #[test]
    fn zero_width_legacy_ink_is_a_noop_but_empty_masked_layers_are_invalid() {
        let layer = RasterLayer::from_ink(LabelBitmap::new(0, 76));
        layer.validate().unwrap();
        let mut tape = LabelBitmap::new(2, 76);
        tape.set_pixel(1, 20, true);
        let before = tape.clone();
        layer.paint(&mut tape, 0, 0);
        assert_eq!(tape, before);
        assert!(
            RasterLayer {
                coverage: Some(LabelBitmap::new(0, 76)),
                ..layer
            }
            .validate()
            .is_err()
        );
        assert!(
            RasterLayer::from_ink(LabelBitmap::new(1, 0))
                .validate()
                .is_err()
        );
    }
}
