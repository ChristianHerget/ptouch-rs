// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Physical objects, legacy text boxes, and shared GUI/CLI composition.

use std::borrow::Cow;
use std::io::Cursor;

use serde::{Deserialize, Serialize};

use crate::bitmap::LabelBitmap;
use crate::document::{
    DOCUMENT_VERSION, LabelDocument, LabelElement, LabelObject, ObjectContent, render_elements,
};
use crate::image_loader::{self, ImageLoadOptions};
use crate::text::{TextAlign, TextRenderer};
use crate::{RenderError, Result};

/// How a physical frame starts along the tape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionMode {
    /// Start at `x_mm`, measured from the printable area's left edge.
    #[default]
    Absolute,
    /// Start `gap_mm` after the preceding element's right edge (or origin).
    Relative,
}

/// Vertical alignment of rotated content inside its fixed frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerticalAlign {
    Top,
    #[default]
    Center,
    Bottom,
}

/// A final, axis-aligned physical container, independent of its content.
/// Rotation acts on the content before alignment; mirroring acts on the filled
/// container. Coordinates are relative to the printable area, not a cut edge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TextBox {
    pub width_mm: f64,
    pub height_mm: f64,
    pub position: PositionMode,
    pub x_mm: f64,
    pub y_mm: f64,
    pub gap_mm: f64,
    pub vertical_align: VerticalAlign,
}

impl Default for TextBox {
    fn default() -> Self {
        Self {
            width_mm: 10.0,
            height_mm: 8.0,
            position: PositionMode::Absolute,
            x_mm: 0.0,
            y_mm: 0.0,
            gap_mm: 0.0,
            vertical_align: VerticalAlign::Center,
        }
    }
}

/// Raster bounds at the requested output DPI, after whole-label mirroring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElementBounds {
    pub element_index: usize,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// A single rendering result used by preview, export, and printing.
pub struct RenderedLabel {
    pub bitmap: Option<LabelBitmap>,
    pub bounds: Vec<ElementBounds>,
    /// Non-fatal clipping diagnostics, including the one-based element number.
    pub diagnostics: Vec<String>,
}

const MAX_LAYOUT_PIXELS: u64 = 64 * 1024 * 1024;
const MAX_LENGTH_PX: u32 = 1_000_000;

fn check_size(width: u32, height: u32) -> Result<()> {
    if width > MAX_LENGTH_PX || u64::from(width) * u64::from(height) > MAX_LAYOUT_PIXELS {
        return Err(RenderError::Layout("layout raster is too large".into()));
    }
    Ok(())
}

fn boundary(mm: f64, dpi: u16) -> Result<u32> {
    let px = (mm * f64::from(dpi) / 25.4).round();
    if !px.is_finite() || !(0.0..=f64::from(MAX_LENGTH_PX)).contains(&px) {
        return Err(RenderError::Layout(
            "position is negative, non-finite, or too large".into(),
        ));
    }
    Ok(px as u32)
}

/// Render physical boxes and legacy flow elements to a single printable area.
///
/// Box starts and ends are rounded from absolute millimeter boundaries, avoiding
/// cumulative per-port rounding. Each element updates the flow cursor to its
/// own right edge, even after an absolute move backwards. Overlaps combine
/// black pixels. Legacy dimensions and unboxed font sizes remain raster pixels.
pub fn render_document(
    doc: &LabelDocument,
    tape_width_px: u32,
    target_dpi: u16,
    renderer: &mut TextRenderer,
) -> Result<RenderedLabel> {
    if doc.version == 0 || doc.version > DOCUMENT_VERSION {
        return Err(RenderError::Layout(format!(
            "unsupported layout version {}",
            doc.version
        )));
    }
    if target_dpi == 0 || doc.dpi == 0 || tape_width_px == 0 || tape_width_px > 4096 {
        return Err(RenderError::Layout(
            "DPI and printable height must be positive; height must not exceed 4096 pixels".into(),
        ));
    }
    let mut cursor_mm = 0.0;
    let mut segments = Vec::new();
    let mut bounds = Vec::new();
    let mut diagnostics = Vec::new();
    let mut total_width = 0;

    for (index, element) in doc.elements.iter().enumerate() {
        let framed = match element {
            LabelElement::Text {
                text_box: Some(frame),
                rotation,
                flip_h,
                flip_v,
                ..
            } => Some((frame, *rotation, *flip_h, *flip_v)),
            LabelElement::Object(object) => {
                if doc.version < 3 {
                    return Err(RenderError::Layout(
                        "free-layout objects require layout version 3".into(),
                    ));
                }
                Some((&object.frame, object.rotation, object.flip_h, object.flip_v))
            }
            _ => None,
        };
        let (segment, rect) = if let Some((frame, rotation, flip_h, flip_v)) = framed {
            if doc.version < 2 {
                return Err(RenderError::Layout(
                    "text boxes require layout version 2".into(),
                ));
            }
            for value in [
                frame.width_mm,
                frame.height_mm,
                frame.x_mm,
                frame.y_mm,
                frame.gap_mm,
            ] {
                if !value.is_finite() || value < 0.0 {
                    return Err(RenderError::Layout(format!(
                        "element {}: box dimensions and positions must be finite and nonnegative",
                        index + 1
                    )));
                }
            }
            if frame.width_mm == 0.0 || frame.height_mm == 0.0 || !rotation.is_finite() {
                return Err(RenderError::Layout(format!(
                    "element {}: box dimensions must be positive and rotation finite",
                    index + 1
                )));
            }
            let x_mm = match frame.position {
                PositionMode::Absolute => frame.x_mm,
                PositionMode::Relative => cursor_mm + frame.gap_mm,
            };
            let right_mm = x_mm + frame.width_mm;
            let x = boundary(x_mm, target_dpi)?;
            let y = boundary(frame.y_mm, target_dpi)?;
            let right = boundary(right_mm, target_dpi)?;
            let bottom = boundary(frame.y_mm + frame.height_mm, target_dpi)?;
            if right <= x || bottom <= y || bottom > tape_width_px {
                return Err(RenderError::Layout(format!(
                    "element {}: box must be at least one output pixel in each dimension and fit inside the printable height ({:.3} mm)",
                    index + 1,
                    f64::from(tape_width_px) * 25.4 / f64::from(target_dpi)
                )));
            }
            check_size(right, tape_width_px)?;
            let width = right - x;
            let height = bottom - y;
            let (segment, clipped) = match element {
                LabelElement::Text {
                    content,
                    font_size,
                    align,
                    ..
                } => render_box(
                    renderer,
                    content,
                    *font_size,
                    *align,
                    rotation,
                    frame.vertical_align,
                    width,
                    height,
                    &doc.font_name,
                    doc.font_margin,
                    target_dpi,
                )?,
                LabelElement::Object(object) => {
                    render_object(object, doc, width, height, target_dpi, renderer)?
                }
                _ => unreachable!("only text boxes and objects have frames"),
            };
            if clipped {
                diagnostics.push(format!("Element {}: text exceeds its fixed box and is clipped; use Auto font size or enlarge the box", index + 1));
            }
            cursor_mm = right_mm;
            (
                segment.mirrored(flip_h, flip_v),
                ElementBounds {
                    element_index: index,
                    x,
                    y,
                    width,
                    height,
                },
            )
        } else {
            // Guard pixel padding before the legacy renderer allocates it.
            if let LabelElement::Padding { pixels } = element {
                check_size(*pixels, tape_width_px)?;
            }
            let Some(segment) = render_elements(
                std::slice::from_ref(element),
                tape_width_px,
                &doc.font_name,
                doc.font_margin,
                renderer,
            )?
            else {
                continue;
            };
            let x = boundary(cursor_mm, target_dpi)?;
            let width = segment.width();
            let right = x
                .checked_add(width)
                .ok_or_else(|| RenderError::Layout("layout width overflow".into()))?;
            check_size(right, tape_width_px)?;
            cursor_mm += f64::from(width) * 25.4 / f64::from(target_dpi);
            (
                segment,
                ElementBounds {
                    element_index: index,
                    x,
                    y: 0,
                    width,
                    height: tape_width_px,
                },
            )
        };
        total_width = total_width.max(rect.x + rect.width);
        segments.push(segment);
        bounds.push(rect);
    }
    if bounds.is_empty() {
        return Ok(RenderedLabel {
            bitmap: None,
            bounds,
            diagnostics,
        });
    }
    check_size(total_width, tape_width_px)?;
    let mut bitmap = LabelBitmap::new(total_width, tape_width_px);
    for (segment, rect) in segments.iter().zip(&bounds) {
        blit(&mut bitmap, segment, i64::from(rect.x), i64::from(rect.y));
    }
    for rect in &mut bounds {
        if doc.flip_h {
            rect.x = total_width - rect.x - rect.width;
        }
        if doc.flip_v {
            rect.y = tape_width_px - rect.y - rect.height;
        }
    }
    Ok(RenderedLabel {
        bitmap: Some(bitmap.mirrored(doc.flip_h, doc.flip_v)),
        bounds,
        diagnostics,
    })
}

/// Convert an existing layout explicitly, freezing the occupied geometry at
/// this output resolution. Legacy files are otherwise never rewritten as
/// objects. Content remains editable, so legacy auto text and cropped images
/// can look different with the new frame fitting rules.
pub fn convert_to_objects(
    doc: &LabelDocument,
    tape_width_px: u32,
    target_dpi: u16,
    renderer: &mut TextRenderer,
) -> Result<LabelDocument> {
    let rendered = render_document(doc, tape_width_px, target_dpi, renderer)?;
    let total_width = rendered.bitmap.as_ref().map_or(0, LabelBitmap::width);
    let mm_per_pixel = 25.4 / f64::from(target_dpi);
    let mut converted = doc.clone();
    converted.version = DOCUMENT_VERSION;
    let mut cursor_mm = 0.0;
    for (index, element) in converted.elements.iter_mut().enumerate() {
        let mut frame =
            if let Some(rect) = rendered.bounds.iter().find(|r| r.element_index == index) {
                let x = if doc.flip_h {
                    total_width - rect.x - rect.width
                } else {
                    rect.x
                };
                let y = if doc.flip_v {
                    tape_width_px - rect.y - rect.height
                } else {
                    rect.y
                };
                cursor_mm = f64::from(x + rect.width) * mm_per_pixel;
                TextBox {
                    x_mm: f64::from(x) * mm_per_pixel,
                    y_mm: f64::from(y) * mm_per_pixel,
                    width_mm: f64::from(rect.width.max(1)) * mm_per_pixel,
                    height_mm: f64::from(rect.height) * mm_per_pixel,
                    ..TextBox::default()
                }
            } else {
                // Empty natural text had no occupied width. Give it an editable
                // frame without shifting any subsequent element's frozen position.
                TextBox {
                    x_mm: cursor_mm,
                    height_mm: 8.0_f64.min(f64::from(tape_width_px) * mm_per_pixel),
                    ..TextBox::default()
                }
            };
        let object = match element {
            LabelElement::Object(object) => {
                frame.vertical_align = object.frame.vertical_align;
                LabelObject {
                    frame,
                    ..object.clone()
                }
            }
            LabelElement::Text {
                content,
                font_size,
                text_box,
                align,
                rotation,
                flip_h,
                flip_v,
            } => {
                if let Some(old_frame) = text_box {
                    frame.vertical_align = old_frame.vertical_align;
                } else {
                    log::warn!(
                        "Element {}: legacy text converted to a fixed frame; review font fitting",
                        index + 1
                    );
                }
                LabelObject {
                    frame,
                    rotation: *rotation,
                    flip_h: *flip_h,
                    flip_v: *flip_v,
                    content: ObjectContent::Text {
                        content: content.clone(),
                        font_size: font_size.map(|size| {
                            if text_box.is_some() {
                                size
                            } else {
                                size * 72.0 / f32::from(target_dpi)
                            }
                        }),
                        align: *align,
                    },
                }
            }
            LabelElement::Image {
                path,
                image_data,
                bitmap,
                rotation,
                flip_h,
                flip_v,
                ..
            } => {
                log::warn!(
                    "Element {}: legacy image converted to contain fitting; previously cropped content may reappear",
                    index + 1
                );
                LabelObject {
                    frame,
                    rotation: *rotation,
                    flip_h: *flip_h,
                    flip_v: *flip_v,
                    content: ObjectContent::Image {
                        path: path.clone(),
                        image_data: image_data.clone(),
                        bitmap: bitmap.clone(),
                    },
                }
            }
            LabelElement::CutMark | LabelElement::Padding { .. } => LabelObject {
                frame,
                rotation: 0.0,
                flip_h: false,
                flip_v: false,
                content: if matches!(element, LabelElement::CutMark) {
                    ObjectContent::CutMark
                } else {
                    ObjectContent::Padding
                },
            },
        };
        *element = LabelElement::Object(object);
    }
    // Conversion is atomic: if the new fitting rules cannot render this
    // content, return an error and leave the caller's original document intact.
    render_document(&converted, tape_width_px, target_dpi, renderer)?;
    Ok(converted)
}

fn render_object(
    object: &LabelObject,
    doc: &LabelDocument,
    width: u32,
    height: u32,
    dpi: u16,
    renderer: &mut TextRenderer,
) -> Result<(LabelBitmap, bool)> {
    match &object.content {
        ObjectContent::Text {
            content,
            font_size,
            align,
        } => render_box(
            renderer,
            content,
            *font_size,
            *align,
            object.rotation,
            object.frame.vertical_align,
            width,
            height,
            &doc.font_name,
            doc.font_margin,
            dpi,
        ),
        ObjectContent::Image {
            image_data, bitmap, ..
        } => {
            let source = match bitmap {
                Some(bitmap) => Cow::Borrowed(bitmap),
                None if !image_data.is_empty() => Cow::Owned(image_loader::load_image_from_reader(
                    Cursor::new(image_data),
                    &ImageLoadOptions::default(),
                )?),
                None => return Err(RenderError::Layout("image object has no image data".into())),
            };
            Ok((
                render_contained(
                    &source,
                    object.rotation,
                    object.frame.vertical_align,
                    width,
                    height,
                )?,
                false,
            ))
        }
        ObjectContent::CutMark => {
            // Keep the historical 9-pixel / 3-pixel pattern's physical size at
            // its 180 DPI design resolution, then fit like any other content.
            let reference_height = (f64::from(height) * 180.0 / f64::from(dpi))
                .round()
                .max(1.0) as u32;
            let source = crate::compose::cutmark(reference_height);
            Ok((
                render_contained(
                    &source,
                    object.rotation,
                    object.frame.vertical_align,
                    width,
                    height,
                )?,
                false,
            ))
        }
        ObjectContent::Padding => Ok((LabelBitmap::new(width, height), false)),
    }
}

/// Rotate first, then contain the entire result without changing its aspect
/// ratio. Unlike the legacy image path this never crops rotated image content.
fn render_contained(
    source: &LabelBitmap,
    rotation: f32,
    vertical_align: VerticalAlign,
    width: u32,
    height: u32,
) -> Result<LabelBitmap> {
    if source.width() == 0 || source.height() == 0 {
        return Err(RenderError::Layout(
            "object image dimensions must be positive".into(),
        ));
    }
    let angle = f64::from(rotation.rem_euclid(360.0)).to_radians();
    let rotated_w = (f64::from(source.width()) * angle.cos().abs()
        + f64::from(source.height()) * angle.sin().abs())
    .ceil() as u32;
    let rotated_h = (f64::from(source.width()) * angle.sin().abs()
        + f64::from(source.height()) * angle.cos().abs())
    .ceil() as u32;
    check_size(rotated_w, rotated_h)?;
    let rotated = source.rotate(rotation);
    let scale = (f64::from(width) / f64::from(rotated.width()))
        .min(f64::from(height) / f64::from(rotated.height()));
    let scaled_w = (f64::from(rotated.width()) * scale)
        .round()
        .clamp(1.0, f64::from(width)) as u32;
    let scaled_h = (f64::from(rotated.height()) * scale)
        .round()
        .clamp(1.0, f64::from(height)) as u32;
    let left = (width - scaled_w) / 2;
    let top = match vertical_align {
        VerticalAlign::Top => 0,
        VerticalAlign::Center => (height - scaled_h) / 2,
        VerticalAlign::Bottom => height - scaled_h,
    };
    let mut frame = LabelBitmap::new(width, height);
    for y in 0..scaled_h {
        let source_y = (u64::from(y) * u64::from(rotated.height()) / u64::from(scaled_h)) as u32;
        for x in 0..scaled_w {
            let source_x = (u64::from(x) * u64::from(rotated.width()) / u64::from(scaled_w)) as u32;
            if rotated.get_pixel(source_x, source_y) {
                frame.set_pixel(left + x, top + y, true);
            }
        }
    }
    Ok(frame)
}

/// Black-pixel composition; source white never erases an earlier element.
fn blit(target: &mut LabelBitmap, source: &LabelBitmap, left: i64, top: i64) {
    let x0 = (-left).max(0) as u32;
    let y0 = (-top).max(0) as u32;
    let x1 = (i64::from(target.width()) - left).clamp(0, i64::from(source.width())) as u32;
    let y1 = (i64::from(target.height()) - top).clamp(0, i64::from(source.height())) as u32;
    for y in y0..y1 {
        for x in x0..x1 {
            if source.get_pixel(x, y) {
                target.set_pixel(
                    (i64::from(x) + left) as u32,
                    (i64::from(y) + top) as u32,
                    true,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_box(
    renderer: &mut TextRenderer,
    content: &str,
    font_size_pt: Option<f32>,
    align: TextAlign,
    rotation: f32,
    vertical_align: VerticalAlign,
    width: u32,
    height: u32,
    font_name: &str,
    margin: u32,
    dpi: u16,
) -> Result<(LabelBitmap, bool)> {
    let mut frame = LabelBitmap::new(width, height);
    if content.trim().is_empty() {
        return Ok((frame, false));
    }
    let available_h = height
        .checked_sub(margin.saturating_mul(2))
        .filter(|h| *h > 0)
        .ok_or_else(|| RenderError::Layout("font margin leaves no space in the text box".into()))?;
    let mut render = |size: f32| -> Result<LabelBitmap> {
        Ok(renderer
            .render_unbounded(content, font_name, size, align)?
            .rotate(rotation))
    };
    let text = if let Some(size) = font_size_pt {
        render(size * f32::from(dpi) / 72.0)?
    } else {
        // Search actual shaped and rotated glyph bounds, including fallback
        // fonts and multiple lines. Never stretch glyphs to match box width.
        let mut low = 1.0_f32;
        let mut high = (width.max(available_h) as f32 * 2.0).min(4096.0);
        let mut best = render(low)?;
        // Punctuation can occupy only a small part of the em square. Expand
        // the search until an actual glyph bound fails, instead of assuming
        // that an em-based estimate is an upper bound for every character.
        loop {
            match render(high) {
                Ok(candidate)
                    if candidate.width() <= width && candidate.height() <= available_h =>
                {
                    low = high;
                    best = candidate;
                    if high == 4096.0 {
                        break;
                    }
                    high = (high * 2.0).min(4096.0);
                }
                Ok(_) | Err(RenderError::DimensionMismatch(_)) => break,
                Err(error) => return Err(error),
            }
        }
        for _ in 0..12 {
            let size = (low + high) / 2.0;
            match render(size) {
                Ok(candidate)
                    if candidate.width() <= width && candidate.height() <= available_h =>
                {
                    low = size;
                    best = candidate;
                }
                Ok(_) | Err(RenderError::DimensionMismatch(_)) => high = size,
                Err(error) => return Err(error),
            }
        }
        best
    };
    let clipped = text.width() > width || text.height() > available_h;
    let remaining_x = i64::from(width) - i64::from(text.width());
    let left = match align {
        TextAlign::Left => 0,
        TextAlign::Center => remaining_x / 2,
        TextAlign::Right => remaining_x,
    };
    let remaining_y = i64::from(available_h) - i64::from(text.height());
    let top = i64::from(margin)
        + match vertical_align {
            VerticalAlign::Top => 0,
            VerticalAlign::Center => remaining_y / 2,
            VerticalAlign::Bottom => remaining_y,
        };
    // Limit the blit to the inset area so clipping respects font_margin too.
    let mut inset = LabelBitmap::new(width, available_h);
    blit(&mut inset, &text, left, top - i64::from(margin));
    blit(&mut frame, &inset, 0, i64::from(margin));
    Ok((frame, clipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn element(content: &str, frame: TextBox) -> LabelElement {
        LabelElement::Text {
            content: content.into(),
            font_size: None,
            text_box: Some(frame),
            align: TextAlign::Center,
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
        }
    }

    fn document(elements: Vec<LabelElement>) -> LabelDocument {
        LabelDocument {
            version: 2,
            tape_width_mm: 12,
            dpi: 180,
            font_name: "Droid Sans Fallback".into(),
            font_margin: 0,
            flip_h: false,
            flip_v: false,
            elements,
        }
    }

    fn panel(contents: &[&str]) -> LabelDocument {
        document(
            contents
                .iter()
                .enumerate()
                .map(|(i, content)| {
                    element(
                        content,
                        TextBox {
                            position: if i == 0 {
                                PositionMode::Absolute
                            } else {
                                PositionMode::Relative
                            },
                            x_mm: 5.0,
                            gap_mm: 3.0,
                            ..TextBox::default()
                        },
                    )
                })
                .collect(),
        )
    }

    fn render(doc: &LabelDocument, dpi: u16) -> RenderedLabel {
        render_document(
            doc,
            u32::from(dpi) / 180 * 76,
            dpi,
            &mut TextRenderer::with_test_font(),
        )
        .unwrap()
    }

    fn ink_bounds(bitmap: &LabelBitmap) -> (u32, u32, u32, u32) {
        let mut left = bitmap.width();
        let mut top = bitmap.height();
        let mut right = 0;
        let mut bottom = 0;
        for y in 0..bitmap.height() {
            for x in 0..bitmap.width() {
                if bitmap.get_pixel(x, y) {
                    left = left.min(x);
                    top = top.min(y);
                    right = right.max(x + 1);
                    bottom = bottom.max(y + 1);
                }
            }
        }
        assert!(right > left && bottom > top, "expected visible glyphs");
        (left, top, right, bottom)
    }

    #[test]
    fn port_positions_survive_content_changes_empty_and_template_values() {
        for dpi in [180, 360] {
            let mut doc = panel(&["{{a}}", "{{b}}", "{{c}}"]);
            doc.apply_values(&BTreeMap::from([
                ("a".into(), "汉".into()),
                ("b".into(), String::new()),
                ("c".into(), "多个不同宽度汉字".into()),
            ]));
            let actual = render(&doc, dpi);
            let empty = render(&panel(&["", "", ""]), dpi);
            assert_eq!(actual.bounds, empty.bounds);
            assert_eq!(
                actual.bounds.iter().map(|r| r.x).collect::<Vec<_>>(),
                [5.0, 18.0, 31.0].map(|mm| boundary(mm, dpi).unwrap())
            );
            assert_eq!(actual.bitmap.unwrap().width(), boundary(41.0, dpi).unwrap());
            assert!(actual.diagnostics.is_empty());
        }
    }

    #[test]
    fn long_fractional_pitch_has_no_cumulative_rounding_drift() {
        for dpi in [180, 360] {
            let doc = document(
                (0..300)
                    .map(|_| {
                        element(
                            "",
                            TextBox {
                                position: PositionMode::Relative,
                                width_mm: 10.1,
                                gap_mm: 3.03,
                                ..TextBox::default()
                            },
                        )
                    })
                    .collect(),
            );
            let rendered = render(&doc, dpi);
            for (i, frame) in rendered.bounds.iter().enumerate() {
                let mm = 3.03 + i as f64 * 13.13;
                assert_eq!(frame.x, boundary(mm, dpi).unwrap());
                let actual_mm = f64::from(frame.x) * 25.4 / f64::from(dpi);
                assert!((actual_mm - mm).abs() <= 25.4 / f64::from(dpi));
                assert_eq!(frame.x + frame.width, boundary(mm + 10.1, dpi).unwrap());
            }
        }
    }

    #[test]
    fn mixed_absolute_and_flow_use_previous_right_edge_and_union_extent() {
        let mut doc = document(vec![
            element(
                "",
                TextBox {
                    x_mm: 30.0,
                    ..TextBox::default()
                },
            ),
            element(
                "",
                TextBox {
                    x_mm: 5.0,
                    ..TextBox::default()
                },
            ),
            LabelElement::Padding { pixels: 7 },
            element(
                "",
                TextBox {
                    position: PositionMode::Relative,
                    gap_mm: 3.0,
                    ..TextBox::default()
                },
            ),
        ]);
        let rendered = render(&doc, 180);
        assert_eq!(rendered.bounds[2].x, boundary(15.0, 180).unwrap());
        assert_eq!(rendered.bounds[3].x, boundary(18.0, 180).unwrap() + 7);
        assert_eq!(
            rendered.bitmap.unwrap().width(),
            boundary(40.0, 180).unwrap()
        );
        doc.flip_h = true;
        doc.flip_v = true;
        let flipped = render(&doc, 180);
        for (a, b) in rendered.bounds.iter().zip(&flipped.bounds) {
            assert_eq!(b.x, boundary(40.0, 180).unwrap() - a.x - a.width);
            assert_eq!(b.y, 76 - a.y - a.height);
        }
    }

    #[test]
    fn alignments_position_multiline_ink_within_fixed_box() {
        for horizontal in [TextAlign::Left, TextAlign::Center, TextAlign::Right] {
            for vertical in [
                VerticalAlign::Top,
                VerticalAlign::Center,
                VerticalAlign::Bottom,
            ] {
                let mut item = element(
                    "汉字\n字",
                    TextBox {
                        width_mm: 20.0,
                        vertical_align: vertical,
                        ..TextBox::default()
                    },
                );
                if let LabelElement::Text {
                    font_size, align, ..
                } = &mut item
                {
                    *font_size = Some(5.0);
                    *align = horizontal;
                }
                let rendered = render(&document(vec![item]), 180);
                assert!(rendered.diagnostics.is_empty());
                let bitmap = rendered.bitmap.unwrap();
                let (left, top, right, bottom) = ink_bounds(&bitmap);
                let frame = rendered.bounds[0];
                match horizontal {
                    TextAlign::Left => assert_eq!(left, 0),
                    TextAlign::Center => {
                        assert!((i64::from(left) - i64::from(frame.width - right)).abs() <= 1)
                    }
                    TextAlign::Right => assert_eq!(right, frame.width),
                }
                match vertical {
                    VerticalAlign::Top => assert_eq!(top, 0),
                    VerticalAlign::Center => {
                        assert!((i64::from(top) - i64::from(frame.height - bottom)).abs() <= 1)
                    }
                    VerticalAlign::Bottom => assert_eq!(bottom, frame.height),
                }
            }
        }
    }

    #[test]
    fn boxed_point_sizes_preserve_physical_glyph_size_across_dpi() {
        let mut doc = document(vec![element(
            "汉字",
            TextBox {
                width_mm: 20.0,
                ..TextBox::default()
            },
        )]);
        if let LabelElement::Text { font_size, .. } = &mut doc.elements[0] {
            *font_size = Some(6.0);
        }
        let low = render(&doc, 180).bitmap.unwrap();
        let high = render(&doc, 360).bitmap.unwrap();
        let (left, top, right, bottom) = ink_bounds(&low);
        let (left2, top2, right2, bottom2) = ink_bounds(&high);
        // Font hinting can differ by a pixel at either resolution.
        assert!((i64::from(right2 - left2) - 2 * i64::from(right - left)).abs() <= 2);
        assert!((i64::from(bottom2 - top2) - 2 * i64::from(bottom - top)).abs() <= 2);
        if let LabelElement::Text { text_box, .. } = &mut doc.elements[0] {
            *text_box = None;
        }
        // Unboxed sizes keep the historical raster-pixel semantics.
        assert_eq!(
            render(&doc, 180).bitmap.unwrap().width(),
            render(&doc, 360).bitmap.unwrap().width()
        );
    }

    #[test]
    fn auto_size_can_expand_punctuation_beyond_an_em_based_estimate() {
        let mut doc = document(vec![element("。", TextBox::default())]);
        let automatic = render(&doc, 180);
        assert!(automatic.diagnostics.is_empty());
        let (left, top, right, bottom) = ink_bounds(&automatic.bitmap.unwrap());
        if let LabelElement::Text { font_size, .. } = &mut doc.elements[0] {
            *font_size = Some(80.0);
        }
        let fixed = render(&doc, 180);
        assert!(fixed.diagnostics.is_empty());
        let (left2, top2, right2, bottom2) = ink_bounds(&fixed.bitmap.unwrap());
        assert!(right - left >= right2 - left2);
        assert!(bottom - top >= bottom2 - top2);
    }

    #[test]
    fn fixed_overflow_clips_and_auto_size_fits_rotation_without_moving_ports() {
        let mut doc = panel(&["很多很多汉字\n字字字字字", ""]);
        let original = render(&doc, 180).bounds;
        for rotation in [0.0, 45.0, 90.0, 180.0, 270.0] {
            if let LabelElement::Text {
                font_size,
                rotation: angle,
                ..
            } = &mut doc.elements[0]
            {
                *font_size = Some(40.0);
                *angle = rotation;
            }
            let fixed = render(&doc, 180);
            assert_eq!(fixed.diagnostics.len(), 1);
            assert_eq!(fixed.bounds, original);
            let bitmap = fixed.bitmap.unwrap();
            let frame = fixed.bounds[0];
            for y in 0..bitmap.height() {
                for x in 0..bitmap.width() {
                    if x < frame.x || x >= frame.x + frame.width || y >= frame.height {
                        assert!(!bitmap.get_pixel(x, y));
                    }
                }
            }
            if let LabelElement::Text { font_size, .. } = &mut doc.elements[0] {
                *font_size = None;
            }
            let automatic = render(&doc, 180);
            assert_eq!(automatic.bounds, original);
            assert!(automatic.diagnostics.is_empty());
            ink_bounds(&automatic.bitmap.unwrap());
        }
    }

    #[test]
    fn box_mirroring_is_pixel_exact_and_overlap_never_erases_ink() {
        let mut doc = document(vec![element("汉文", TextBox::default())]);
        let original = render(&doc, 180).bitmap.unwrap();
        if let LabelElement::Text { flip_h, flip_v, .. } = &mut doc.elements[0] {
            *flip_h = true;
            *flip_v = true;
        }
        let mirrored = render(&doc, 180);
        let frame = mirrored.bounds[0];
        let bitmap = mirrored.bitmap.unwrap();
        for y in 0..frame.height {
            for x in 0..frame.width {
                assert_eq!(
                    bitmap.get_pixel(x, y),
                    original.get_pixel(frame.width - 1 - x, frame.height - 1 - y)
                );
            }
        }
        doc.elements.push(element("", TextBox::default()));
        assert_eq!(render(&doc, 180).bitmap.unwrap().data(), bitmap.data());
    }

    #[test]
    fn invalid_geometry_and_font_sizes_are_errors() {
        for (width, height, x, y) in [
            (0.0, 8.0, 0.0, 0.0),
            (-1.0, 8.0, 0.0, 0.0),
            (10.0, 8.0, -0.01, 0.0),
            (10.0, 8.0, 0.0, 10.0),
            (f64::NAN, 8.0, 0.0, 0.0),
            (f64::INFINITY, 8.0, 0.0, 0.0),
            (1e9, 8.0, 0.0, 0.0),
            (0.001, 8.0, 0.0, 0.0),
        ] {
            let doc = document(vec![element(
                "",
                TextBox {
                    width_mm: width,
                    height_mm: height,
                    x_mm: x,
                    y_mm: y,
                    ..TextBox::default()
                },
            )]);
            assert!(render_document(&doc, 76, 180, &mut TextRenderer::with_test_font()).is_err());
        }
        let mut doc = panel(&["字"]);
        for size in [0.0, -2.0, f32::NAN, f32::INFINITY, 1e9] {
            if let LabelElement::Text { font_size, .. } = &mut doc.elements[0] {
                *font_size = Some(size);
            }
            assert!(render_document(&doc, 76, 180, &mut TextRenderer::with_test_font()).is_err());
        }
        assert!(
            render_document(
                &document(vec![]),
                76,
                0,
                &mut TextRenderer::with_test_font()
            )
            .is_err()
        );
    }

    #[test]
    fn document_versions_round_trip_and_v1_pixels_stay_identical() {
        let doc = panel(&["汉字", "", "字"]);
        let encoded = doc.to_toml_string().unwrap();
        assert!(encoded.contains("version = 2"));
        let decoded = LabelDocument::from_toml_str(&encoded).unwrap();
        for (a, b) in doc.elements.iter().zip(&decoded.elements) {
            if let (
                LabelElement::Text { text_box: a, .. },
                LabelElement::Text { text_box: b, .. },
            ) = (a, b)
            {
                assert_eq!(a, b);
            } else {
                panic!("expected text");
            }
        }
        assert_eq!(
            render(&doc, 180).bitmap.unwrap().data(),
            render(&decoded, 180).bitmap.unwrap().data()
        );
        assert!(
            LabelDocument::from_toml_str(&encoded.replacen("version = 2", "version = 1", 1))
                .is_err()
        );
        let mut legacy = doc.clone();
        legacy.version = 1;
        for element in &mut legacy.elements {
            if let LabelElement::Text { text_box, .. } = element {
                *text_box = None;
            }
        }
        let mut renderer = TextRenderer::with_test_font();
        let expected = render_elements(&legacy.elements, 76, &legacy.font_name, 0, &mut renderer)
            .unwrap()
            .unwrap();
        let actual = render_document(&legacy, 76, 180, &mut renderer)
            .unwrap()
            .bitmap
            .unwrap();
        assert_eq!(expected.width(), actual.width());
        assert_eq!(expected.data(), actual.data());
        assert!(LabelDocument::from_toml_str(&legacy.to_toml_string().unwrap()).is_ok());
        // Saving a loaded v1 design after adding a box automatically upgrades.
        legacy.elements.push(element("", TextBox::default()));
        assert!(legacy.to_toml_string().unwrap().contains("version = 2"));
    }

    fn solid_bitmap(width: u32, height: u32) -> LabelBitmap {
        let mut bitmap = LabelBitmap::new(width, height);
        for y in 0..height {
            for x in 0..width {
                bitmap.set_pixel(x, y, true);
            }
        }
        bitmap
    }

    fn image_content(bitmap: LabelBitmap) -> ObjectContent {
        ObjectContent::Image {
            path: None,
            image_data: vec![],
            bitmap: Some(bitmap),
        }
    }

    fn object(content: ObjectContent, frame: TextBox) -> LabelElement {
        LabelElement::Object(LabelObject {
            content,
            frame,
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
        })
    }

    #[test]
    fn all_object_kinds_share_physical_bounds_and_preserve_empty_template_slots() {
        let contents = vec![
            ObjectContent::Text {
                content: "{{port}}".into(),
                font_size: None,
                align: TextAlign::Center,
            },
            image_content(solid_bitmap(40, 20)),
            ObjectContent::CutMark,
            ObjectContent::Padding,
        ];
        let mut doc = document(
            contents
                .into_iter()
                .enumerate()
                .map(|(i, content)| {
                    object(
                        content,
                        TextBox {
                            position: if i == 0 {
                                PositionMode::Absolute
                            } else {
                                PositionMode::Relative
                            },
                            x_mm: 5.0,
                            y_mm: 1.0,
                            gap_mm: 3.0,
                            ..TextBox::default()
                        },
                    )
                })
                .collect(),
        );
        doc.version = 3;
        for dpi in [180, 360] {
            let mut filled = doc.clone();
            filled.apply_values(&BTreeMap::from([("port".into(), "汉字".into())]));
            let original = render(&filled, dpi);
            for (i, rect) in original.bounds.iter().enumerate() {
                let x = 5.0 + 13.0 * i as f64;
                assert_eq!(rect.x, boundary(x, dpi).unwrap());
                assert_eq!(rect.x + rect.width, boundary(x + 10.0, dpi).unwrap());
                assert_eq!(rect.y, boundary(1.0, dpi).unwrap());
                assert_eq!(rect.y + rect.height, boundary(9.0, dpi).unwrap());
            }
            let mut empty = doc.clone();
            empty.apply_values(&BTreeMap::from([("port".into(), String::new())]));
            assert_eq!(render(&empty, dpi).bounds, original.bounds);
            for angle in [45.0, 90.0, -135.0, 270.0] {
                for element in &mut empty.elements {
                    element.as_object_mut().unwrap().rotation = angle;
                }
                let rotated = render(&empty, dpi);
                assert_eq!(rotated.bounds, original.bounds);
                assert!(rotated.diagnostics.is_empty());
            }
        }
    }

    #[test]
    fn image_contain_preserves_aspect_and_all_rotated_content_stays_in_frame() {
        let frame = TextBox {
            x_mm: 2.0,
            y_mm: 2.0,
            width_mm: 10.0,
            height_mm: 6.0,
            ..TextBox::default()
        };
        let mut doc = document(vec![object(image_content(solid_bitmap(40, 20)), frame)]);
        doc.version = 3;
        let original_bounds = render(&doc, 180).bounds;
        for angle in [0.0, 30.0, 45.0, 90.0, 180.0, 270.0] {
            doc.elements[0].as_object_mut().unwrap().rotation = angle;
            let rendered = render(&doc, 180);
            assert_eq!(rendered.bounds, original_bounds);
            let rect = rendered.bounds[0];
            let bitmap = rendered.bitmap.unwrap();
            let (left, top, right, bottom) = ink_bounds(&bitmap);
            assert!(left >= rect.x && top >= rect.y);
            assert!(right <= rect.x + rect.width && bottom <= rect.y + rect.height);
            if angle == 0.0 || angle == 180.0 {
                assert_eq!(right - left, rect.width);
                assert!((i64::from(right - left) - 2 * i64::from(bottom - top)).abs() <= 1);
            } else if angle == 90.0 || angle == 270.0 {
                assert_eq!(bottom - top, rect.height);
                assert!((i64::from(bottom - top) - 2 * i64::from(right - left)).abs() <= 1);
            }
        }
    }

    #[test]
    fn object_text_keeps_v2_rendering_and_mirroring_exactly() {
        let mut legacy = panel(&["汉字", "字", ""]);
        legacy.flip_h = true;
        legacy.flip_v = true;
        for rotation in [0.0, 45.0, 90.0] {
            if let LabelElement::Text {
                rotation: angle,
                flip_h,
                ..
            } = &mut legacy.elements[0]
            {
                *angle = rotation;
                *flip_h = true;
            }
            let before = render(&legacy, 180);
            let converted =
                convert_to_objects(&legacy, 76, 180, &mut TextRenderer::with_test_font()).unwrap();
            let after = render(&converted, 180);
            assert_eq!(before.bounds, after.bounds);
            assert_eq!(before.bitmap.unwrap().data(), after.bitmap.unwrap().data());
            assert!(converted.elements.iter().all(|element| {
                element.as_object().unwrap().frame.position == PositionMode::Absolute
            }));
            assert_eq!(legacy.version, 2);
        }
    }

    #[test]
    fn explicit_conversion_keeps_legacy_element_order_positions_and_padding_width() {
        let mut legacy = document(vec![
            LabelElement::Text {
                content: "汉字".into(),
                font_size: Some(18.0),
                text_box: None,
                align: TextAlign::Left,
                rotation: 0.0,
                flip_h: false,
                flip_v: false,
            },
            LabelElement::Padding { pixels: 17 },
            LabelElement::Image {
                path: None,
                image_data: vec![],
                bitmap: Some(solid_bitmap(20, 10)),
                rotation: 45.0,
                target_height: Some(20),
                flip_h: false,
                flip_v: true,
            },
            LabelElement::CutMark,
        ]);
        legacy.version = 1;
        legacy.flip_h = true;
        legacy.flip_v = true;
        let before = render(&legacy, 180);
        let original_bytes = legacy.to_toml_string().unwrap();
        let mut converted = legacy.clone();
        converted
            .convert_to_free_layout(76, 180, &mut TextRenderer::with_test_font())
            .unwrap();
        let after = render(&converted, 180);
        assert_eq!(before.bounds, after.bounds);
        assert_eq!(converted.version, 3);
        assert_eq!(legacy.to_toml_string().unwrap(), original_bytes);
        assert_eq!(
            converted.elements[1].as_object().unwrap().frame.width_mm,
            17.0 * 25.4 / 180.0
        );
        assert!(matches!(
            converted.elements[1].as_object().unwrap().content,
            ObjectContent::Padding
        ));
        if let ObjectContent::Text { content, .. } =
            &mut converted.elements[0].as_object_mut().unwrap().content
        {
            *content = "长度完全不同的新端口名".into();
        }
        assert_eq!(render(&converted, 180).bounds, after.bounds);
    }

    #[test]
    fn object_reordering_resolves_new_previous_edge_without_white_erasing_ink() {
        let mut doc = document(vec![
            object(
                image_content(solid_bitmap(20, 10)),
                TextBox {
                    x_mm: 5.0,
                    ..TextBox::default()
                },
            ),
            object(
                ObjectContent::Padding,
                TextBox {
                    position: PositionMode::Relative,
                    width_mm: 4.0,
                    gap_mm: 3.0,
                    ..TextBox::default()
                },
            ),
            object(
                ObjectContent::Padding,
                TextBox {
                    position: PositionMode::Relative,
                    gap_mm: 2.0,
                    ..TextBox::default()
                },
            ),
        ]);
        doc.version = 3;
        let original = render(&doc, 180);
        assert_eq!(original.bounds[2].x, boundary(24.0, 180).unwrap());
        doc.elements.swap(0, 1);
        let reordered = render(&doc, 180);
        assert_eq!(reordered.bounds[0].x, boundary(3.0, 180).unwrap());
        assert_eq!(reordered.bounds[1].x, boundary(5.0, 180).unwrap());
        assert_eq!(reordered.bounds[2].x, boundary(17.0, 180).unwrap());
        let data = reordered.bitmap.unwrap().data().to_vec();
        doc.elements.push(object(
            ObjectContent::Padding,
            TextBox {
                x_mm: 5.0,
                ..TextBox::default()
            },
        ));
        assert_eq!(render(&doc, 180).bitmap.unwrap().data(), data);
    }

    #[test]
    fn object_versions_and_invalid_transforms_fail_without_using_legacy_renderer() {
        let mut doc = document(vec![object(ObjectContent::Padding, TextBox::default())]);
        let mut renderer = TextRenderer::with_test_font();
        for version in [1, 2] {
            doc.version = version;
            assert!(render_document(&doc, 76, 180, &mut renderer).is_err());
        }
        doc.version = 3;
        for rotation in [f32::NAN, f32::INFINITY] {
            doc.elements[0].as_object_mut().unwrap().rotation = rotation;
            assert!(render_document(&doc, 76, 180, &mut renderer).is_err());
        }
        assert!(render_elements(&doc.elements, 76, "", 0, &mut renderer).is_err());
    }
}
