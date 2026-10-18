// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Standalone monochrome shapes with physical strokes and transparent corners.

use resvg::tiny_skia::{
    LineCap, Paint, Path, PathBuilder, Pixmap, Rect, Stroke, StrokeDash, Transform,
};
use serde::{Deserialize, Serialize};

use crate::{
    RenderError, Result,
    bitmap::LabelBitmap,
    stroke::{PatternKind, StrokePattern},
    surface::RasterLayer,
};

const MAX_SHAPE_PIXELS: u64 = 16_777_216;
const MAX_SHAPE_DIMENSION: u32 = 1_000_000;
const MAX_PATTERN_MARKS: f64 = 65_536.0;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeKind {
    #[default]
    Rectangle,
    Ellipse,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeFill {
    #[default]
    None,
    Ink,
    Paper,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShapeStyle {
    pub kind: ShapeKind,
    pub fill: ShapeFill,
    /// Stroke is drawn inside the object's dimensions. Zero disables it.
    pub stroke_width_mm: f64,
    pub pattern: StrokePattern,
}

impl Default for ShapeStyle {
    fn default() -> Self {
        Self {
            kind: ShapeKind::Rectangle,
            fill: ShapeFill::None,
            stroke_width_mm: 0.3,
            pattern: StrokePattern::default(),
        }
    }
}

impl ShapeStyle {
    pub fn validate(&self) -> Result<()> {
        if !self.stroke_width_mm.is_finite() || !(0.0..=1000.0).contains(&self.stroke_width_mm) {
            return Err(RenderError::Layout(
                "shape stroke width must be in 0–1000 mm".into(),
            ));
        }
        if self.stroke_width_mm == 0.0 && self.fill == ShapeFill::None {
            return Err(RenderError::Layout(
                "a shape needs a fill or a visible stroke".into(),
            ));
        }
        self.pattern.validate()
    }

    /// Rasterize an unrotated shape. Paper fill covers the actual shape only;
    /// the empty corners around an ellipse remain transparent.
    pub fn raster(&self, width: u32, height: u32, dpi: u16) -> Result<RasterLayer> {
        self.validate()?;
        if width == 0
            || height == 0
            || dpi == 0
            || width > MAX_SHAPE_DIMENSION
            || height > MAX_SHAPE_DIMENSION
            || u64::from(width) * u64::from(height) > MAX_SHAPE_PIXELS
        {
            return Err(RenderError::Layout(
                "shape dimensions exceed the raster limit or DPI is zero".into(),
            ));
        }
        let ppm = f64::from(dpi) / 25.4;
        let thickness = (self.stroke_width_mm * ppm)
            .max(1.0)
            .min(f64::from(width.min(height))) as f32;
        if self.stroke_width_mm > 0.0 && self.fill != ShapeFill::Ink {
            let interval = match self.pattern.kind {
                PatternKind::Solid => None,
                PatternKind::Dashed => Some(
                    (self.pattern.dash_mm * ppm).max(1.0) + (self.pattern.gap_mm * ppm).max(1.0),
                ),
                PatternKind::Dotted => {
                    Some(f64::from(thickness) + (self.pattern.gap_mm * ppm).max(1.0))
                }
            };
            // Raster area alone does not bound a dashed vector path's memory:
            // a very long, narrow contour can contain millions of marks.
            if interval.is_some_and(|period| {
                2.0 * (f64::from(width) + f64::from(height)) / period > MAX_PATTERN_MARKS
            }) {
                return Err(RenderError::Layout(
                    "shape stroke has too many marks; enlarge its dash or gap".into(),
                ));
            }
        }
        // Analytic pixel-center filling is symmetric even for one-dot ovals;
        // aliased cubic paths can otherwise disagree at opposite edge pixels.
        let mut outer = outer_bitmap(self.kind, width, height);
        // Positive subpixel shapes must stay selectable/printable at the
        // output resolution, including a one-dot ellipse.
        if !has_ink(&outer) {
            outer.set_pixel(width / 2, height / 2, true);
        }
        if self.fill == ShapeFill::Ink {
            return Ok(RasterLayer {
                ink: outer,
                coverage: None,
            });
        }
        let mut ink = LabelBitmap::new(width, height);
        if self.stroke_width_mm > 0.0 {
            // Only one bounded RGBA intermediate is retained alongside the
            // packed outer and ink masks. Filled-only shapes need no pixmap.
            let mut pixmap = Pixmap::new(width, height)
                .ok_or_else(|| RenderError::Layout("cannot allocate shape raster".into()))?;
            let paint = Paint {
                anti_alias: false,
                ..Paint::default()
            };
            let stroke = self.stroke(thickness, ppm)?;
            let path = stroke_path(self.kind, width as f32, height as f32, thickness)?;
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            ink = alpha_bitmap(&pixmap);
            // Clipping to the filled geometry also handles degenerate narrow
            // ellipses and round dotted caps without filling exterior corners.
            for y in 0..height {
                for x in 0..width {
                    if !outer.get_pixel(x, y) {
                        ink.set_pixel(x, y, false);
                    }
                }
            }
            if self.kind == ShapeKind::Ellipse && self.pattern.kind == PatternKind::Solid {
                symmetrize_ellipse(&mut ink);
            }
            if !has_ink(&ink) {
                // A very small dotted contour can fall entirely between dots.
                // Keep a positive physical stroke visible by one output dot.
                if let Some((x, y)) = first_ink(&outer) {
                    ink.set_pixel(x, y, true);
                }
            }
        }
        Ok(RasterLayer {
            ink,
            coverage: (self.fill == ShapeFill::Paper).then_some(outer),
        })
    }

    fn stroke(&self, thickness: f32, ppm: f64) -> Result<Stroke> {
        let mut stroke = Stroke {
            width: thickness,
            ..Stroke::default()
        };
        let pixels = |mm: f64| (mm * ppm).max(1.0) as f32;
        let intervals = match self.pattern.kind {
            PatternKind::Solid => return Ok(stroke),
            PatternKind::Dashed => vec![pixels(self.pattern.dash_mm), pixels(self.pattern.gap_mm)],
            PatternKind::Dotted => {
                stroke.line_cap = LineCap::Round;
                // A tiny nonzero dash gets a round cap reliably. Include its
                // diameter in the interval to leave the requested clear gap.
                vec![0.001, thickness + pixels(self.pattern.gap_mm)]
            }
        };
        stroke.dash = Some(
            StrokeDash::new(intervals, 0.0)
                .ok_or_else(|| RenderError::Layout("invalid shape stroke pattern".into()))?,
        );
        Ok(stroke)
    }
}

fn outer_bitmap(kind: ShapeKind, width: u32, height: u32) -> LabelBitmap {
    let mut bitmap = LabelBitmap::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let inside = match kind {
                ShapeKind::Rectangle => true,
                ShapeKind::Ellipse => {
                    let dx = (2.0 * f64::from(x) + 1.0 - f64::from(width)) / f64::from(width);
                    let dy = (2.0 * f64::from(y) + 1.0 - f64::from(height)) / f64::from(height);
                    dx * dx + dy * dy <= 1.0
                }
            };
            if inside {
                bitmap.set_pixel(x, y, true);
            }
        }
    }
    bitmap
}

fn symmetrize_ellipse(bitmap: &mut LabelBitmap) {
    let (width, height) = (bitmap.width(), bitmap.height());
    for y in 0..height.div_ceil(2) {
        for x in 0..width.div_ceil(2) {
            let reflected = [
                (x, y),
                (width - 1 - x, y),
                (x, height - 1 - y),
                (width - 1 - x, height - 1 - y),
            ];
            let transpose = [
                (y, x),
                (height - 1 - y, x),
                (y, width - 1 - x),
                (height - 1 - y, width - 1 - x),
            ];
            let points = reflected
                .iter()
                .chain(transpose.iter().filter(|_| width == height));
            if points.clone().any(|(x, y)| bitmap.get_pixel(*x, *y)) {
                for (x, y) in points {
                    bitmap.set_pixel(*x, *y, true);
                }
            }
        }
    }
}

fn shape_path(kind: ShapeKind, width: f32, height: f32, inset: f32) -> Result<Path> {
    let rect = Rect::from_xywh(inset, inset, width - 2.0 * inset, height - 2.0 * inset)
        .ok_or_else(|| RenderError::Layout("shape path has no positive area".into()))?;
    match kind {
        ShapeKind::Rectangle => Ok(PathBuilder::from_rect(rect)),
        ShapeKind::Ellipse => {
            let mut builder = PathBuilder::new();
            builder.push_oval(rect);
            builder
                .finish()
                .ok_or_else(|| RenderError::Layout("cannot construct ellipse path".into()))
        }
    }
}

fn stroke_path(kind: ShapeKind, width: f32, height: f32, thickness: f32) -> Result<Path> {
    if width > thickness && height > thickness {
        return shape_path(kind, width, height, thickness / 2.0);
    }
    // A one-dot-wide shape has no interior contour. Stroke its long axis,
    // then intersect with the outer geometry to retain a narrow ellipse.
    let mut builder = PathBuilder::new();
    if width >= height {
        builder.move_to(0.0, height / 2.0);
        builder.line_to(width, height / 2.0);
    } else {
        builder.move_to(width / 2.0, 0.0);
        builder.line_to(width / 2.0, height);
    }
    builder
        .finish()
        .ok_or_else(|| RenderError::Layout("cannot construct narrow shape stroke".into()))
}

fn alpha_bitmap(pixmap: &Pixmap) -> LabelBitmap {
    let mut bitmap = LabelBitmap::new(pixmap.width(), pixmap.height());
    for (index, pixel) in pixmap.pixels().iter().enumerate() {
        if pixel.alpha() >= 128 {
            bitmap.set_pixel(
                (index % pixmap.width() as usize) as u32,
                (index / pixmap.width() as usize) as u32,
                true,
            );
        }
    }
    bitmap
}

fn has_ink(bitmap: &LabelBitmap) -> bool {
    bitmap.data().iter().any(|byte| *byte != 0)
}

fn first_ink(bitmap: &LabelBitmap) -> Option<(u32, u32)> {
    (0..bitmap.height()).find_map(|y| {
        (0..bitmap.width())
            .find(|x| bitmap.get_pixel(*x, y))
            .map(|x| (x, y))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(bitmap: &LabelBitmap) -> usize {
        (0..bitmap.height())
            .flat_map(|y| (0..bitmap.width()).map(move |x| (x, y)))
            .filter(|(x, y)| bitmap.get_pixel(*x, *y))
            .count()
    }

    #[test]
    fn defaults_round_trip_and_unknown_fields_are_rejected() {
        let style = ShapeStyle::default();
        assert_eq!(toml::from_str::<ShapeStyle>("").unwrap(), style);
        let encoded = toml::to_string(&style).unwrap();
        assert_eq!(toml::from_str::<ShapeStyle>(&encoded).unwrap(), style);
        assert!(encoded.contains("rectangle"));
        assert!(toml::from_str::<ShapeStyle>("unsupported = 1").is_err());
    }

    #[test]
    fn paper_rectangle_is_an_opaque_white_mask_without_stroke() {
        let shape = ShapeStyle {
            fill: ShapeFill::Paper,
            stroke_width_mm: 0.0,
            ..Default::default()
        };
        let raster = shape.raster(20, 10, 180).unwrap();
        assert_eq!(count(&raster.ink), 0);
        assert_eq!(count(raster.coverage.as_ref().unwrap()), 200);
    }

    #[test]
    fn paper_ellipse_corners_stay_transparent_and_circle_is_symmetric() {
        for (width, height) in [(20, 20), (21, 21), (40, 20), (41, 21)] {
            let shape = ShapeStyle {
                kind: ShapeKind::Ellipse,
                fill: ShapeFill::Paper,
                stroke_width_mm: 0.0,
                ..Default::default()
            };
            let raster = shape.raster(width, height, 180).unwrap();
            let coverage = raster.coverage.unwrap();
            assert!(!coverage.get_pixel(0, 0));
            assert!(!coverage.get_pixel(width - 1, height - 1));
            assert!(coverage.get_pixel(width / 2, height / 2));
            assert_eq!(count(&raster.ink), 0);
            for y in 0..height {
                for x in 0..width {
                    assert_eq!(
                        coverage.get_pixel(x, y),
                        coverage.get_pixel(width - 1 - x, height - 1 - y)
                    );
                    if width == height {
                        assert_eq!(coverage.get_pixel(x, y), coverage.get_pixel(y, x));
                    }
                }
            }
        }
    }

    #[test]
    fn outline_and_ink_fill_do_not_have_an_opaque_background() {
        for kind in [ShapeKind::Rectangle, ShapeKind::Ellipse] {
            let shape = ShapeStyle {
                kind,
                stroke_width_mm: 25.4 / 180.0 * 2.0,
                ..Default::default()
            };
            let outline = shape.raster(40, 20, 180).unwrap();
            assert!(outline.coverage.is_none());
            assert!(has_ink(&outline.ink));
            assert!(!outline.ink.get_pixel(20, 10));
            let filled = ShapeStyle {
                fill: ShapeFill::Ink,
                ..shape
            }
            .raster(40, 20, 180)
            .unwrap();
            assert!(filled.coverage.is_none());
            assert!(filled.ink.get_pixel(20, 10));
            assert!(count(&filled.ink) > count(&outline.ink));
        }
    }

    #[test]
    fn solid_ellipse_stroke_is_symmetric_and_stays_within_coverage() {
        for (width, height) in [(20, 20), (21, 21), (40, 20)] {
            let raster = ShapeStyle {
                kind: ShapeKind::Ellipse,
                fill: ShapeFill::Paper,
                ..Default::default()
            }
            .raster(width, height, 180)
            .unwrap();
            let coverage = raster.coverage.unwrap();
            for y in 0..height {
                for x in 0..width {
                    let ink = raster.ink.get_pixel(x, y);
                    assert!(!ink || coverage.get_pixel(x, y));
                    assert_eq!(ink, raster.ink.get_pixel(width - 1 - x, height - 1 - y));
                    if width == height {
                        assert_eq!(ink, raster.ink.get_pixel(y, x));
                    }
                }
            }
        }
    }

    #[test]
    fn dashed_period_and_stroke_width_scale_with_dpi() {
        let shape = ShapeStyle {
            stroke_width_mm: 25.4 / 180.0 * 2.0,
            pattern: StrokePattern {
                kind: PatternKind::Dashed,
                dash_mm: 25.4 / 180.0 * 8.0,
                gap_mm: 25.4 / 180.0 * 4.0,
            },
            ..Default::default()
        };
        for (dpi, scale) in [(180, 1), (360, 2)] {
            let raster = shape.raster(240 * scale, 60 * scale, dpi).unwrap();
            for x in 20 * scale..200 * scale {
                assert_eq!(
                    raster.ink.get_pixel(x, 0),
                    raster.ink.get_pixel(x + 12 * scale, 0)
                );
            }
            assert!((20 * scale..200 * scale).any(|x| raster.ink.get_pixel(x, 0)));
            assert!((20 * scale..200 * scale).any(|x| !raster.ink.get_pixel(x, 0)));
            for x in 20 * scale..200 * scale {
                assert!(!raster.ink.get_pixel(x, 2 * scale));
            }
        }
    }

    #[test]
    fn dotted_ellipse_uses_separate_visible_marks() {
        for dpi in [180, 360] {
            let scale = u32::from(dpi / 180);
            let style = ShapeStyle {
                kind: ShapeKind::Ellipse,
                pattern: StrokePattern {
                    kind: PatternKind::Dotted,
                    ..Default::default()
                },
                ..Default::default()
            };
            let dotted = style.raster(100 * scale, 50 * scale, dpi).unwrap();
            let solid = ShapeStyle {
                pattern: StrokePattern::default(),
                ..style
            }
            .raster(100 * scale, 50 * scale, dpi)
            .unwrap();
            assert!(count(&dotted.ink) > 0);
            assert!(count(&dotted.ink) < count(&solid.ink));
            assert!(!dotted.ink.get_pixel(0, 0));
        }
    }

    #[test]
    fn tiny_shapes_and_thick_strokes_remain_visible_without_panicking() {
        for kind in [ShapeKind::Rectangle, ShapeKind::Ellipse] {
            for pattern in [PatternKind::Solid, PatternKind::Dashed, PatternKind::Dotted] {
                let shape = ShapeStyle {
                    kind,
                    stroke_width_mm: 1000.0,
                    pattern: StrokePattern {
                        kind: pattern,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                for (width, height) in [(1, 1), (1, 9), (9, 1), (2, 2)] {
                    assert!(has_ink(&shape.raster(width, height, 180).unwrap().ink));
                }
            }
        }
    }

    #[test]
    fn invalid_style_dimensions_and_raster_budgets_fail_before_allocation() {
        for value in [-1.0, f64::NAN, f64::INFINITY, 1001.0] {
            assert!(
                ShapeStyle {
                    stroke_width_mm: value,
                    ..Default::default()
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            ShapeStyle {
                stroke_width_mm: 0.0,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        let dense_pattern = ShapeStyle {
            stroke_width_mm: 0.01,
            pattern: StrokePattern {
                kind: PatternKind::Dotted,
                dash_mm: 0.01,
                gap_mm: 0.01,
            },
            ..Default::default()
        };
        assert!(dense_pattern.raster(1_000_000, 1, 180).is_err());
        let shape = ShapeStyle::default();
        for (width, height, dpi) in [
            (0, 1, 180),
            (1, 0, 180),
            (1, 1, 0),
            (u32::MAX, u32::MAX, 180),
            (1_000_001, 1, 180),
            (4097, 4097, 180),
        ] {
            assert!(shape.raster(width, height, dpi).is_err());
        }
    }
}
