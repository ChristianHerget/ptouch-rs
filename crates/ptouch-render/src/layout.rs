// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Physical objects, legacy text boxes, and shared GUI/CLI composition.

use std::borrow::Cow;
use std::io::Cursor;

use serde::{Deserialize, Serialize};

use crate::bitmap::LabelBitmap;
use crate::document::{
    DOCUMENT_VERSION, LabelDocument, LabelElement, LabelObject, ObjectContent,
    render_elements_with_layout_at_resolution,
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

/// A fixed, axis-aligned template slot. Legacy/auto-fit content rotates before
/// fitting and alignment; independent content uses the slot as its position
/// anchor. Coordinates are relative to the printable area, not a cut edge.
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

/// An independent, unrotated content box. Offsets are relative to the template
/// slot center before object or whole-label mirroring.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentBox {
    pub width_mm: f64,
    pub height_mm: f64,
    #[serde(default)]
    pub offset_x_mm: f64,
    #[serde(default)]
    pub offset_y_mm: f64,
}

impl ContentBox {
    pub(crate) fn validate(&self) -> Result<()> {
        if !self.width_mm.is_finite()
            || !self.height_mm.is_finite()
            || self.width_mm <= 0.0
            || self.height_mm <= 0.0
            || !self.offset_x_mm.is_finite()
            || !self.offset_y_mm.is_finite()
        {
            return Err(RenderError::Layout(
                "content box dimensions must be positive and finite; offsets must be finite".into(),
            ));
        }
        Ok(())
    }
}

/// Output-pixel corners in local TL/TR/BR/BL order, including all mirrors.
/// `edit_box` remains in the document's unmirrored physical coordinate system.
#[derive(Debug, Clone, PartialEq)]
pub struct ContentGeometry {
    pub element_index: usize,
    pub corners: [[f32; 2]; 4],
    pub edit_box: ContentBox,
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
    pub content_bounds: Vec<ContentGeometry>,
    /// Non-fatal clipping diagnostics, including the one-based element number.
    pub diagnostics: Vec<String>,
}

impl RenderedLabel {
    /// Reflect pixels and all selection geometry together, exactly once.
    pub fn mirrored(mut self, horizontal: bool, vertical: bool) -> Self {
        if let Some(bitmap) = &self.bitmap {
            let width = bitmap.width();
            let height = bitmap.height();
            for rect in &mut self.bounds {
                if horizontal {
                    rect.x = width - rect.x - rect.width;
                }
                if vertical {
                    rect.y = height - rect.y - rect.height;
                }
            }
            for geometry in &mut self.content_bounds {
                for point in &mut geometry.corners {
                    if horizontal {
                        point[0] = width as f32 - point[0];
                    }
                    if vertical {
                        point[1] = height as f32 - point[1];
                    }
                }
            }
            self.bitmap = Some(bitmap.mirrored(horizontal, vertical));
        }
        self
    }
}

/// Render a document in printer dots. Physical X uses feed DPI and Y uses head
/// DPI. Rotations happen in square source pixels before sampling to actual dots.
pub fn render_document_at_resolution(
    doc: &LabelDocument,
    tape_width_px: u32,
    resolution: ptouch_core::PrintResolution,
    renderer: &mut TextRenderer,
) -> Result<RenderedLabel> {
    use ptouch_core::PrintResolution;
    let PrintResolution { head_dpi, feed_dpi } = resolution;
    if head_dpi == 0
        || feed_dpi == 0
        || !(head_dpi == feed_dpi
            || u32::from(head_dpi) * 2 == u32::from(feed_dpi)
            || u32::from(feed_dpi) * 2 == u32::from(head_dpi))
    {
        return Err(RenderError::Layout(
            "unsupported head/feed resolution".into(),
        ));
    }
    if head_dpi == feed_dpi {
        return render_document(doc, tape_width_px, head_dpi, renderer);
    }
    if doc.version == 0
        || doc.version > DOCUMENT_VERSION
        || doc.dpi == 0
        || tape_width_px == 0
        || tape_width_px > 4096
    {
        return Err(RenderError::Layout(
            "invalid document version, DPI or printable height".into(),
        ));
    }
    // Preserve legacy global draft sampling (including odd element boundaries).
    if doc.elements.iter().all(|element| {
        matches!(
            element,
            LabelElement::Text { text_box: None, .. }
                | LabelElement::Image { .. }
                | LabelElement::CutMark
                | LabelElement::Padding { .. }
        )
    }) {
        let rendered = render_elements_with_layout_at_resolution(
            &doc.elements,
            tape_width_px,
            &doc.font_name,
            doc.font_margin,
            renderer,
            resolution,
        )?;
        return Ok(match rendered {
            Some(rendered) => RenderedLabel {
                bounds: rendered
                    .element_spans
                    .iter()
                    .enumerate()
                    .filter_map(|(element_index, span)| {
                        span.as_ref().map(|span| ElementBounds {
                            element_index,
                            x: span.start,
                            y: 0,
                            width: span.end - span.start,
                            height: tape_width_px,
                        })
                    })
                    .collect(),
                bitmap: Some(rendered.bitmap),
                content_bounds: Vec::new(),
                diagnostics: Vec::new(),
            }
            .mirrored(doc.flip_h, doc.flip_v),
            None => RenderedLabel {
                bitmap: None,
                bounds: Vec::new(),
                content_bounds: Vec::new(),
                diagnostics: Vec::new(),
            },
        });
    }
    render_document_impl(doc, tape_width_px, resolution, renderer)
}

const MAX_LAYOUT_PIXELS: u64 = 64 * 1024 * 1024;
const MAX_LENGTH_PX: u32 = 1_000_000;

fn check_size(width: u32, height: u32) -> Result<()> {
    if width > MAX_LENGTH_PX || u64::from(width) * u64::from(height) > MAX_LAYOUT_PIXELS {
        return Err(RenderError::Layout("layout raster is too large".into()));
    }
    Ok(())
}

/// Use the same quadrant snapping and bounds calculation as LabelBitmap::rotate,
/// but reject an oversized intermediate image before allocating or scanning it.
fn checked_rotate(source: &LabelBitmap, rotation: f32) -> Result<LabelBitmap> {
    if !rotation.is_finite() {
        return Err(RenderError::Layout("rotation must be finite".into()));
    }
    let norm = ((rotation % 360.0) + 360.0) % 360.0;
    let (width, height) =
        if norm.abs() < 0.5 || (norm - 180.0).abs() < 0.5 || (norm - 360.0).abs() < 0.5 {
            (source.width(), source.height())
        } else if (norm - 90.0).abs() < 0.5 || (norm - 270.0).abs() < 0.5 {
            (source.height(), source.width())
        } else {
            let radians = norm.to_radians();
            let width = source.width() as f32;
            let height = source.height() as f32;
            (
                (width * radians.cos().abs() + height * radians.sin().abs()).ceil() as u32,
                (width * radians.sin().abs() + height * radians.cos().abs()).ceil() as u32,
            )
        };
    check_size(width, height).map_err(|_| {
        // Auto text sizing treats this as a failed size candidate and can try
        // smaller sizes; fixed text/images get an actionable error instead.
        RenderError::DimensionMismatch(
            "rotated content raster is too large; reduce the font size or source image dimensions"
                .into(),
        )
    })?;
    Ok(source.rotate(rotation))
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
    render_document_impl(
        doc,
        tape_width_px,
        ptouch_core::PrintResolution::square(target_dpi),
        renderer,
    )
}

fn render_document_impl(
    doc: &LabelDocument,
    tape_width_px: u32,
    resolution: ptouch_core::PrintResolution,
    renderer: &mut TextRenderer,
) -> Result<RenderedLabel> {
    let target_dpi = resolution.head_dpi.max(resolution.feed_dpi);
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
    let source_scale = u32::from(target_dpi / resolution.head_dpi);
    let source_height = tape_width_px
        .checked_mul(source_scale)
        .ok_or_else(|| RenderError::Layout("printable height overflow".into()))?;
    let source_doc = if source_scale > 1 && doc.font_margin > 0 {
        let mut source = doc.clone();
        source.font_margin = source
            .font_margin
            .checked_mul(source_scale)
            .ok_or_else(|| RenderError::Layout("font margin overflow".into()))?;
        Cow::Owned(source)
    } else {
        Cow::Borrowed(doc)
    };
    let mut cursor_mm = 0.0;
    let mut segments = Vec::new();
    let mut bounds = Vec::new();
    let mut content_bounds = Vec::new();
    let mut diagnostics = Vec::new();
    let mut total_width = 0;
    let mut segment_pixels = 0_u64;
    let ppm = f64::from(target_dpi) / 25.4;

    for (index, element) in doc.elements.iter().enumerate() {
        if let LabelElement::Object(object) = element
            && let ObjectContent::Text { line_spacing, .. } = &object.content
        {
            crate::text::validate_line_spacing(*line_spacing)?;
            if *line_spacing != crate::text::DEFAULT_LINE_SPACING && doc.version < 5 {
                return Err(RenderError::Layout(
                    "custom line spacing requires layout version 5".into(),
                ));
            }
        }
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
                if object.content_box.is_some() && doc.version < 4 {
                    return Err(RenderError::Layout(
                        "independent content boxes require layout version 4".into(),
                    ));
                }
                Some((&object.frame, object.rotation, object.flip_h, object.flip_v))
            }
            _ => None,
        };
        let (segment, rect, left, top) = if let Some((frame, rotation, flip_h, flip_v)) = framed {
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
            if right <= x || bottom <= y || bottom > source_height {
                return Err(RenderError::Layout(format!(
                    "element {}: box must be at least one output pixel in each dimension and fit inside the printable height ({:.3} mm)",
                    index + 1,
                    f64::from(source_height) / ppm
                )));
            }
            check_size(right, tape_width_px)?;
            let width = right - x;
            let height = bottom - y;
            let slot_center = [
                (x_mm + frame.width_mm / 2.0) * ppm,
                (frame.y_mm + frame.height_mm / 2.0) * ppm,
            ];
            let (segment, clipped, mut geometry, mut left, mut top) = match element {
                LabelElement::Object(object) if object.content_box.is_some() => {
                    render_independent(object, &source_doc, slot_center, target_dpi, renderer)?
                }
                _ => {
                    let (segment, clipped, local) = match element {
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
                            crate::text::DEFAULT_LINE_SPACING,
                            rotation,
                            frame.vertical_align,
                            width,
                            height,
                            &doc.font_name,
                            source_doc.font_margin,
                            target_dpi,
                        )?,
                        LabelElement::Object(object) => render_object(
                            object,
                            &source_doc,
                            width,
                            height,
                            target_dpi,
                            renderer,
                            object.rotation,
                        )?,
                        _ => unreachable!("only text boxes and objects have frames"),
                    };
                    let geometry =
                        local.into_geometry(index, [x as f32, y as f32], slot_center, ppm);
                    (segment, clipped, geometry, i64::from(x), i64::from(y))
                }
            };
            geometry.element_index = index;
            if clipped {
                diagnostics.push(format!("Element {}: text exceeds its fixed box and is clipped; use Auto font size or enlarge the box", index + 1));
            }
            // Object mirrors act around the slot center, including independent offsets.
            let mirror_center = [
                f64::from(x) + f64::from(width) / 2.0,
                f64::from(y) + f64::from(height) / 2.0,
            ];
            if flip_h {
                left = i64::from(x) * 2 + i64::from(width) - left - i64::from(segment.width());
                for point in &mut geometry.corners {
                    point[0] = (mirror_center[0] * 2.0) as f32 - point[0];
                }
            }
            if flip_v {
                top = i64::from(y) * 2 + i64::from(height) - top - i64::from(segment.height());
                for point in &mut geometry.corners {
                    point[1] = (mirror_center[1] * 2.0) as f32 - point[1];
                }
            }
            if left < 0 || top < 0 || top + i64::from(segment.height()) > i64::from(source_height) {
                diagnostics.push(format!(
                    "Element {}: content extends outside the printable area and is clipped",
                    index + 1
                ));
            }
            let output_x = boundary(x_mm, resolution.feed_dpi)?;
            let output_y = boundary(frame.y_mm, resolution.head_dpi)?;
            let output_right = boundary(right_mm, resolution.feed_dpi)?;
            let output_bottom = boundary(frame.y_mm + frame.height_mm, resolution.head_dpi)?;
            if output_right <= output_x || output_bottom <= output_y {
                return Err(RenderError::Layout(
                    "box must occupy at least one output dot".into(),
                ));
            }
            let sx = f64::from(resolution.feed_dpi) / f64::from(target_dpi);
            let sy = f64::from(resolution.head_dpi) / f64::from(target_dpi);
            for point in &mut geometry.corners {
                point[0] *= sx as f32;
                point[1] *= sy as f32;
            }
            let segment = segment.mirrored(flip_h, flip_v);
            let had_ink = segment.data().iter().any(|byte| *byte != 0);
            let mut segment = if resolution.head_dpi < target_dpi {
                segment.halve_height()
            } else if resolution.feed_dpi < target_dpi {
                segment.halve_feed()
            } else {
                segment
            };
            if had_ink && !segment.data().iter().any(|byte| *byte != 0) {
                if matches!(
                    element,
                    LabelElement::Object(LabelObject {
                        content: ObjectContent::CutMark,
                        ..
                    })
                ) && segment.width() > 0
                    && segment.height() > 0
                {
                    segment.set_pixel(segment.width() / 2, segment.height() / 2, true);
                } else {
                    return Err(RenderError::Layout("content has no visible pixels at the selected print resolution; enlarge its box".into()));
                }
            }
            let left = (left as f64 * sx).round() as i64;
            let top = (top as f64 * sy).round() as i64;
            content_bounds.push(geometry);
            cursor_mm = right_mm;
            (
                segment,
                ElementBounds {
                    element_index: index,
                    x: output_x,
                    y: output_y,
                    width: output_right - output_x,
                    height: output_bottom - output_y,
                },
                left,
                top,
            )
        } else {
            if let LabelElement::Padding { pixels } = element {
                check_size(*pixels, tape_width_px)?;
            }
            let Some(segment) = crate::document::render_elements_at_resolution(
                std::slice::from_ref(element),
                tape_width_px,
                &doc.font_name,
                doc.font_margin,
                renderer,
                resolution,
            )?
            else {
                continue;
            };
            let x = boundary(cursor_mm, resolution.feed_dpi)?;
            let width = segment.width();
            let right = x
                .checked_add(width)
                .ok_or_else(|| RenderError::Layout("layout width overflow".into()))?;
            check_size(right, tape_width_px)?;
            // Preserve the historical arithmetic order at half-pixel edges.
            cursor_mm += f64::from(width) * 25.4 / f64::from(resolution.feed_dpi);
            (
                segment,
                ElementBounds {
                    element_index: index,
                    x,
                    y: 0,
                    width,
                    height: tape_width_px,
                },
                i64::from(x),
                0,
            )
        };
        let content_right = left + i64::from(segment.width());
        if content_right > i64::from(MAX_LENGTH_PX) {
            return Err(RenderError::Layout("layout raster is too large".into()));
        }
        total_width = total_width
            .max(rect.x + rect.width)
            .max(content_right.max(0) as u32);
        segment_pixels += u64::from(segment.width()) * u64::from(segment.height());
        let canvas_pixels = u64::from(total_width) * u64::from(tape_width_px);
        if segment_pixels + canvas_pixels > MAX_LAYOUT_PIXELS {
            return Err(RenderError::Layout(
                "layout working set is too large; reduce object count or frame dimensions".into(),
            ));
        }
        segments.push((segment, left, top));
        bounds.push(rect);
    }
    if bounds.is_empty() {
        return Ok(RenderedLabel {
            bitmap: None,
            bounds,
            content_bounds,
            diagnostics,
        });
    }
    check_size(total_width, tape_width_px)?;
    let mut bitmap = LabelBitmap::new(total_width, tape_width_px);
    for (segment, left, top) in &segments {
        blit(&mut bitmap, segment, *left, *top);
    }
    Ok(RenderedLabel {
        bitmap: Some(bitmap),
        bounds,
        content_bounds,
        diagnostics,
    }
    .mirrored(doc.flip_h, doc.flip_v))
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
    converted.version = doc.version.max(3);
    let mut cursor_mm = 0.0;
    let mut occupied_right_mm = 0.0_f64;
    let mut new_empty_frames = Vec::new();
    for (index, element) in converted.elements.iter_mut().enumerate() {
        let physical_frame = match element {
            LabelElement::Object(object) => Some(&object.frame),
            LabelElement::Text { text_box, .. } => text_box.as_ref(),
            _ => None,
        };
        let rendered_rect = rendered.bounds.iter().find(|r| r.element_index == index);
        let occupied_before =
            physical_frame.is_some() || rendered_rect.is_some_and(|rect| rect.width > 0);
        let mut frame = if let Some(physical_frame) = physical_frame {
            // A v2/v3 frame already has exact physical geometry. Resolving its
            // relative X must not round through the current preview's pixels.
            let x_mm = match physical_frame.position {
                PositionMode::Absolute => physical_frame.x_mm,
                PositionMode::Relative => cursor_mm + physical_frame.gap_mm,
            };
            cursor_mm = x_mm + physical_frame.width_mm;
            TextBox {
                position: PositionMode::Absolute,
                x_mm,
                ..physical_frame.clone()
            }
        } else if let Some(rect) = rendered_rect {
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
            // Match the compositor's exact physical cursor, which adds a
            // legacy raster width without quantizing the preceding frame.
            cursor_mm += f64::from(rect.width) * mm_per_pixel;
            TextBox {
                // A zero-width spacer must become selectable without
                // extending the label: changing total width would move
                // every existing object under whole-label mirroring.
                x_mm: f64::from(if rect.width == 0 && total_width > 0 {
                    x.min(total_width - 1)
                } else {
                    x
                }) * mm_per_pixel,
                y_mm: f64::from(y) * mm_per_pixel,
                width_mm: f64::from(rect.width.max(1)) * mm_per_pixel,
                height_mm: f64::from(rect.height) * mm_per_pixel,
                ..TextBox::default()
            }
        } else {
            // Empty natural text had no occupied width. Keep its editable
            // frame within the old extent so whole-label mirroring cannot
            // shift the original content. All-empty labels have no old
            // geometry to preserve and receive a default frame.
            let width_mm = if total_width > 0 {
                10.0_f64.min(f64::from(total_width) * mm_per_pixel)
            } else {
                10.0
            };
            TextBox {
                x_mm: if total_width > 0 {
                    cursor_mm.min((f64::from(total_width) * mm_per_pixel - width_mm).max(0.0))
                } else {
                    0.0
                },
                width_mm,
                height_mm: 8.0_f64.min(f64::from(tape_width_px) * mm_per_pixel),
                ..TextBox::default()
            }
        };
        if occupied_before {
            occupied_right_mm = occupied_right_mm.max(frame.x_mm + frame.width_mm);
        } else {
            new_empty_frames.push(index);
        }
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
                    content_box: None,
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
                        line_spacing: crate::text::DEFAULT_LINE_SPACING,
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
                    content_box: None,
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
                content_box: None,
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
    if occupied_right_mm > 0.0 {
        for index in new_empty_frames {
            let frame = &mut converted.elements[index].as_object_mut().unwrap().frame;
            // The raster extent can extend beyond an exact physical edge by
            // half a pixel. Do not let new blank frames freeze that rounding
            // surplus into the layout: another DPI could gain a column and
            // shift every object under whole-label mirroring.
            frame.width_mm = frame.width_mm.min(occupied_right_mm);
            frame.x_mm = frame
                .x_mm
                .min((occupied_right_mm - frame.width_mm).max(0.0));
        }
    }
    // Conversion is atomic: if the new fitting rules cannot render this
    // content, return an error and leave the caller's original document intact.
    render_document(&converted, tape_width_px, target_dpi, renderer)?;
    Ok(converted)
}

/// Geometry retained from the same source and fit calculation as the raster.
struct LocalContent {
    corners: [[f32; 2]; 4],
    width: f32,
    height: f32,
    center: [f32; 2],
}

fn effective_rotation(rotation: f32) -> f32 {
    let norm = rotation.rem_euclid(360.0);
    for angle in [0.0_f32, 90.0, 180.0, 270.0, 360.0] {
        if (norm - angle).abs() < 0.5 {
            return angle.rem_euclid(360.0);
        }
    }
    norm
}

fn content_corners(width: f32, height: f32, rotation: f32, center: [f32; 2]) -> [[f32; 2]; 4] {
    let radians = effective_rotation(rotation).to_radians();
    let (sin, cos) = radians.sin_cos();
    [
        [-width / 2.0, -height / 2.0],
        [width / 2.0, -height / 2.0],
        [width / 2.0, height / 2.0],
        [-width / 2.0, height / 2.0],
    ]
    .map(|[x, y]| [center[0] + x * cos - y * sin, center[1] + x * sin + y * cos])
}

impl LocalContent {
    fn rectangle(width: u32, height: u32, rotation: f32) -> Self {
        let (width, height) = (width as f32, height as f32);
        let center = [width / 2.0, height / 2.0];
        Self {
            corners: content_corners(width, height, rotation, center),
            width,
            height,
            center,
        }
    }

    fn into_geometry(
        self,
        index: usize,
        origin: [f32; 2],
        slot_center: [f64; 2],
        ppm: f64,
    ) -> ContentGeometry {
        ContentGeometry {
            element_index: index,
            corners: self.corners.map(|p| [p[0] + origin[0], p[1] + origin[1]]),
            edit_box: ContentBox {
                width_mm: f64::from(self.width) / ppm,
                height_mm: f64::from(self.height) / ppm,
                offset_x_mm: (f64::from(self.center[0]) + f64::from(origin[0]) - slot_center[0])
                    / ppm,
                offset_y_mm: (f64::from(self.center[1]) + f64::from(origin[1]) - slot_center[1])
                    / ppm,
            },
        }
    }
}

fn signed_boundary(px: f64) -> Result<i64> {
    if !px.is_finite() || px.abs() > f64::from(MAX_LENGTH_PX) {
        return Err(RenderError::Layout(
            "content position is non-finite or too large".into(),
        ));
    }
    Ok(px.round() as i64)
}

type IndependentRaster = (LabelBitmap, bool, ContentGeometry, i64, i64);

fn render_independent(
    object: &LabelObject,
    doc: &LabelDocument,
    slot_center: [f64; 2],
    dpi: u16,
    renderer: &mut TextRenderer,
) -> Result<IndependentRaster> {
    let content_box = object
        .content_box
        .as_ref()
        .expect("independent content box");
    content_box.validate()?;
    let ppm = f64::from(dpi) / 25.4;
    let width = boundary(content_box.width_mm, dpi)?;
    let height = boundary(content_box.height_mm, dpi)?;
    if width == 0 || height == 0 {
        return Err(RenderError::Layout(
            "content box must be at least one output pixel in each dimension".into(),
        ));
    }
    check_size(width, height)?;
    let center = [
        slot_center[0] + content_box.offset_x_mm * ppm,
        slot_center[1] + content_box.offset_y_mm * ppm,
    ];
    // Check coordinates before allocating even a valid-sized offscreen object.
    signed_boundary(center[0])?;
    signed_boundary(center[1])?;
    let (source, clipped, _) = render_object(object, doc, width, height, dpi, renderer, 0.0)?;
    let mut bitmap = checked_rotate(&source, object.rotation)?;
    if source.data().iter().any(|byte| *byte != 0) && !bitmap.data().iter().any(|byte| *byte != 0) {
        if matches!(object.content, ObjectContent::CutMark) {
            bitmap.set_pixel(bitmap.width() / 2, bitmap.height() / 2, true);
        } else {
            return Err(RenderError::Layout(
                "rotated content has no visible pixels at this output DPI; enlarge the content box"
                    .into(),
            ));
        }
    }
    let left = signed_boundary(center[0] - f64::from(bitmap.width()) / 2.0)?;
    let top = signed_boundary(center[1] - f64::from(bitmap.height()) / 2.0)?;
    let raster_center = [
        left as f32 + bitmap.width() as f32 / 2.0,
        top as f32 + bitmap.height() as f32 / 2.0,
    ];
    let geometry = ContentGeometry {
        element_index: 0,
        corners: content_corners(width as f32, height as f32, object.rotation, raster_center),
        edit_box: content_box.clone(),
    };
    Ok((bitmap, clipped, geometry, left, top))
}

fn render_object(
    object: &LabelObject,
    doc: &LabelDocument,
    width: u32,
    height: u32,
    dpi: u16,
    renderer: &mut TextRenderer,
    rotation: f32,
) -> Result<(LabelBitmap, bool, LocalContent)> {
    match &object.content {
        ObjectContent::Text {
            content,
            font_size,
            align,
            line_spacing,
        } => render_box(
            renderer,
            content,
            *font_size,
            *align,
            *line_spacing,
            rotation,
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
            // Source bytes retain vector and raster detail at the requested
            // content size; the eager cache can be much smaller than this box.
            let source = if !image_data.is_empty() {
                Cow::Owned(image_loader::load_image_from_reader(
                    Cursor::new(image_data),
                    &ImageLoadOptions {
                        target_height: Some(height),
                        ..Default::default()
                    },
                )?)
            } else if let Some(bitmap) = bitmap {
                Cow::Borrowed(bitmap)
            } else {
                return Err(RenderError::Layout("image object has no image data".into()));
            };
            let (bitmap, geometry) = render_contained(
                &source,
                rotation,
                object.frame.vertical_align,
                width,
                height,
                false,
            )?;
            Ok((bitmap, false, geometry))
        }
        ObjectContent::CutMark => {
            // A cut guide spans its frame rather than inheriting the legacy
            // 9-pixel image's aspect ratio. Retain the historical one-pixel
            // stroke and 3-on/3-off raster pattern, including at 360 DPI.
            let mut source = LabelBitmap::new(width, height);
            let line_x = ((u64::from(width) * 5 / 9) as u32).min(width - 1);
            for y in 0..height {
                if y % 6 < 3 {
                    source.set_pixel(line_x, y, true);
                }
            }
            let (bitmap, geometry) = render_contained(
                &source,
                rotation,
                object.frame.vertical_align,
                width,
                height,
                true,
            )?;
            Ok((bitmap, false, geometry))
        }
        ObjectContent::Padding => Ok((
            LabelBitmap::new(width, height),
            false,
            LocalContent::rectangle(width, height, rotation),
        )),
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
    preserve_ink: bool,
) -> Result<(LabelBitmap, LocalContent)> {
    if source.width() == 0 || source.height() == 0 {
        return Err(RenderError::Layout(
            "object image dimensions must be positive".into(),
        ));
    }
    let rotated = checked_rotate(source, rotation)?;
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
            let black = if preserve_ink {
                // A logical cut guide must not vanish between nearest-neighbor
                // samples when its frame is narrow or rotated.
                let end_x = (u64::from(x + 1) * u64::from(rotated.width()))
                    .div_ceil(u64::from(scaled_w)) as u32;
                let end_y = (u64::from(y + 1) * u64::from(rotated.height()))
                    .div_ceil(u64::from(scaled_h)) as u32;
                (source_y..end_y).any(|sy| (source_x..end_x).any(|sx| rotated.get_pixel(sx, sy)))
            } else {
                rotated.get_pixel(source_x, source_y)
            };
            if black {
                frame.set_pixel(left + x, top + y, true);
            }
        }
    }
    if preserve_ink && !frame.data().iter().any(|byte| *byte != 0) {
        // Even a one-pixel guide rotated by 45 degrees needs one visible
        // output dot; inverse rotation can otherwise miss its sole pixel.
        frame.set_pixel(width / 2, height / 2, true);
    }
    let center = [
        left as f32 + scaled_w as f32 / 2.0,
        top as f32 + scaled_h as f32 / 2.0,
    ];
    let sx = scaled_w as f32 / rotated.width() as f32;
    let sy = scaled_h as f32 / rotated.height() as f32;
    let corners = content_corners(
        source.width() as f32,
        source.height() as f32,
        rotation,
        [rotated.width() as f32 / 2.0, rotated.height() as f32 / 2.0],
    )
    .map(|p| [left as f32 + p[0] * sx, top as f32 + p[1] * sy]);
    Ok((
        frame,
        LocalContent {
            corners,
            width: (f64::from(source.width()) * scale) as f32,
            height: (f64::from(source.height()) * scale) as f32,
            center,
        },
    ))
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
    line_spacing: f32,
    rotation: f32,
    vertical_align: VerticalAlign,
    width: u32,
    height: u32,
    font_name: &str,
    margin: u32,
    dpi: u16,
) -> Result<(LabelBitmap, bool, LocalContent)> {
    let mut frame = LabelBitmap::new(width, height);
    if content.trim().is_empty() {
        return Ok((
            frame,
            false,
            LocalContent::rectangle(width, height, rotation),
        ));
    }
    let available_h = height
        .checked_sub(margin.saturating_mul(2))
        .filter(|h| *h > 0)
        .ok_or_else(|| RenderError::Layout("font margin leaves no space in the text box".into()))?;
    let mut render = |size: f32| -> Result<(LabelBitmap, u32, u32)> {
        let source = renderer.render_unbounded(content, font_name, size, align, line_spacing)?;
        Ok((
            checked_rotate(&source, rotation)?,
            source.width(),
            source.height(),
        ))
    };
    let (text, source_width, source_height) = if let Some(size) = font_size_pt {
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
                    if candidate.0.width() <= width && candidate.0.height() <= available_h =>
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
                    if candidate.0.width() <= width && candidate.0.height() <= available_h =>
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
    if !text.data().iter().any(|byte| *byte != 0) {
        return Err(RenderError::Text(
            "nonempty text produces no visible pixels at this output DPI; enlarge the frame or font size"
                .into(),
        ));
    }
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
    let center = [
        left as f32 + text.width() as f32 / 2.0,
        top as f32 + text.height() as f32 / 2.0,
    ];
    Ok((
        frame,
        clipped,
        LocalContent {
            corners: content_corners(
                source_width as f32,
                source_height as f32 + margin as f32 * 2.0,
                rotation,
                center,
            ),
            width: source_width as f32,
            // The independent box will apply the font margin in its own local
            // axes. Include that inset when freezing the fitted glyph size,
            // otherwise pinning would deduct the same margin a second time.
            height: source_height as f32 + margin as f32 * 2.0,
            center,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::render_elements;
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
    fn custom_line_spacing_changes_leading_without_moving_template_slots() {
        for dpi in [180, 360] {
            for text in ["Ag", "Ag\ngp\n汉字", ""] {
                let frame = TextBox {
                    x_mm: 5.0,
                    width_mm: 20.0,
                    height_mm: 10.0,
                    ..Default::default()
                };
                let mut item = LabelObject::text(text, frame);
                item.content_box = Some(ContentBox {
                    width_mm: 20.0,
                    height_mm: 10.0,
                    offset_x_mm: 0.0,
                    offset_y_mm: 0.0,
                });
                if let ObjectContent::Text { font_size, .. } = &mut item.content {
                    *font_size = Some(6.0);
                }
                let mut doc = document(vec![
                    LabelElement::Object(item),
                    element(
                        "",
                        TextBox {
                            position: PositionMode::Relative,
                            gap_mm: 2.8,
                            ..Default::default()
                        },
                    ),
                ]);
                doc.version = 4;
                let before = render(&doc, dpi);
                doc.version = 5;
                if let ObjectContent::Text { line_spacing, .. } =
                    &mut doc.elements[0].as_object_mut().unwrap().content
                {
                    *line_spacing = 0.8;
                }
                let after = render(&doc, dpi);
                assert_eq!(before.bounds, after.bounds);
                assert_eq!(before.content_bounds, after.content_bounds);
                let before = before.bitmap.unwrap();
                let after = after.bitmap.unwrap();
                if text.contains('\n') {
                    let (_, top_before, _, bottom_before) = ink_bounds(&before);
                    let (_, top_after, _, bottom_after) = ink_bounds(&after);
                    assert!(bottom_after - top_after < bottom_before - top_before);
                } else {
                    assert_eq!(before.data(), after.data());
                }
            }
        }
    }

    #[test]
    fn custom_spacing_requires_v5_and_validates_even_empty_text() {
        for spacing in [0.8, 0.0, 0.49, 3.01, f32::NAN, f32::INFINITY] {
            let mut object = LabelObject::text("", TextBox::default());
            if let ObjectContent::Text { line_spacing, .. } = &mut object.content {
                *line_spacing = spacing;
            }
            let mut doc = document(vec![LabelElement::Object(object)]);
            doc.version = 4;
            assert!(render_document(&doc, 76, 180, &mut TextRenderer::with_test_font()).is_err());
            doc.version = 5;
            assert_eq!(
                render_document(&doc, 76, 180, &mut TextRenderer::with_test_font()).is_ok(),
                spacing == 0.8
            );
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
            content_box: None,
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
                line_spacing: crate::text::DEFAULT_LINE_SPACING,
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

    #[test]
    fn rotated_text_checks_intermediate_area_before_allocating_it() {
        // The source is only 6 KiB, but a 45-degree bounding box would exceed
        // a billion pixels. Neither fixed text nor auto-size candidates may
        // allocate that intermediate image.
        let thin = LabelBitmap::new(50_000, 1);
        assert!(matches!(
            checked_rotate(&thin, 45.0),
            Err(RenderError::DimensionMismatch(_))
        ));
        assert!(checked_rotate(&thin, 90.0).is_ok());
        let mut item = LabelObject::text("汉".repeat(1000), TextBox::default());
        item.rotation = 45.0;
        if let ObjectContent::Text { font_size, .. } = &mut item.content {
            *font_size = Some(10.0);
        }
        let mut doc = document(vec![LabelElement::Object(item)]);
        doc.version = 3;
        let error = render_document(&doc, 76, 180, &mut TextRenderer::with_test_font())
            .err()
            .unwrap();
        assert!(
            error
                .to_string()
                .contains("rotated content raster is too large")
        );
    }

    #[test]
    fn nonempty_text_cannot_silently_render_as_a_blank_frame() {
        let mut item = LabelObject::text("汉字", TextBox::default());
        if let ObjectContent::Text { font_size, .. } = &mut item.content {
            *font_size = Some(0.1);
        }
        let mut doc = document(vec![LabelElement::Object(item)]);
        doc.version = 3;
        let error = render_document(&doc, 76, 180, &mut TextRenderer::with_test_font())
            .err()
            .unwrap();
        assert!(error.to_string().contains("no visible pixels"));
        if let ObjectContent::Text { content, .. } =
            &mut doc.elements[0].as_object_mut().unwrap().content
        {
            *content = String::new();
        }
        let empty = render(&doc, 180);
        assert_eq!(empty.bounds.len(), 1);
        assert!(empty.bitmap.unwrap().data().iter().all(|byte| *byte == 0));
    }

    #[test]
    fn empty_tail_and_zero_padding_conversion_preserve_mirrored_extent_and_ink() {
        for flip_h in [false, true] {
            let mut doc = document(vec![
                LabelElement::CutMark,
                LabelElement::Padding { pixels: 0 },
                LabelElement::Text {
                    content: String::new(),
                    font_size: None,
                    text_box: None,
                    align: TextAlign::Left,
                    rotation: 0.0,
                    flip_h: false,
                    flip_v: false,
                },
            ]);
            doc.version = 1;
            doc.flip_h = flip_h;
            let before = render(&doc, 180);
            doc.convert_to_free_layout(76, 180, &mut TextRenderer::with_test_font())
                .unwrap();
            let after = render(&doc, 180);
            assert_eq!(before.bounds[0], after.bounds[0]);
            let before = before.bitmap.unwrap();
            let after = after.bitmap.unwrap();
            assert_eq!(before.width(), after.width());
            assert_eq!(before.data(), after.data());
            assert!(
                doc.elements
                    .iter()
                    .all(|element| element.as_object().is_some())
            );
        }
    }

    #[test]
    fn cut_guides_survive_narrow_frames_and_legacy_conversion_at_both_dpis() {
        for (dpi, height) in [(180, 76), (360, 150)] {
            let mut legacy = document(vec![LabelElement::CutMark]);
            legacy.version = 1;
            legacy.dpi = dpi;
            let mut renderer = TextRenderer::with_test_font();
            let before = render_document(&legacy, height, dpi, &mut renderer).unwrap();
            legacy
                .convert_to_free_layout(height, dpi, &mut renderer)
                .unwrap();
            let after = render_document(&legacy, height, dpi, &mut renderer).unwrap();
            assert_eq!(before.bounds, after.bounds);
            assert_eq!(before.bitmap.unwrap().data(), after.bitmap.unwrap().data());
            let object = legacy.elements[0].as_object_mut().unwrap();
            object.frame.width_mm = 25.4 / f64::from(dpi);
            for height_mm in [25.4 / f64::from(dpi), 8.0] {
                legacy.elements[0].as_object_mut().unwrap().frame.height_mm = height_mm;
                for rotation in [0.0, 45.0, 90.0, 135.0, 270.0] {
                    legacy.elements[0].as_object_mut().unwrap().rotation = rotation;
                    let rendered = render_document(&legacy, height, dpi, &mut renderer).unwrap();
                    assert!(
                        rendered
                            .bitmap
                            .unwrap()
                            .data()
                            .iter()
                            .any(|byte| *byte != 0)
                    );
                }
            }
        }
    }

    #[test]
    fn converting_physical_frames_preserves_exact_millimeters_at_both_dpis() {
        let mut original = panel(&["汉字", "字", ""]);
        original.flip_h = true;
        for element in &mut original.elements {
            if let LabelElement::Text {
                text_box: Some(frame),
                ..
            } = element
            {
                frame.y_mm = 1.0;
                frame.vertical_align = VerticalAlign::Bottom;
            }
        }
        let mut converted = original.clone();
        let mut renderer = TextRenderer::with_test_font();
        converted
            .convert_to_free_layout(76, 180, &mut renderer)
            .unwrap();
        for (index, element) in converted.elements.iter().enumerate() {
            let frame = &element.as_object().unwrap().frame;
            assert_eq!(frame.x_mm, [5.0, 18.0, 31.0][index]);
            assert_eq!(
                (frame.y_mm, frame.width_mm, frame.height_mm),
                (1.0, 10.0, 8.0)
            );
            assert_eq!(frame.vertical_align, VerticalAlign::Bottom);
        }
        for (dpi, height) in [(180, 76), (360, 150)] {
            let before = render_document(&original, height, dpi, &mut renderer).unwrap();
            let after = render_document(&converted, height, dpi, &mut renderer).unwrap();
            assert_eq!(before.bounds, after.bounds);
            assert_eq!(before.bitmap.unwrap().data(), after.bitmap.unwrap().data());
        }
        // Converting an already physical v3 object must not quantize it either.
        let frame = &mut converted.elements[0].as_object_mut().unwrap().frame;
        frame.x_mm = 5.031;
        frame.y_mm = 0.531;
        frame.width_mm = 10.017;
        frame.height_mm = 8.019;
        let exact = frame.clone();
        converted
            .convert_to_free_layout(150, 360, &mut renderer)
            .unwrap();
        assert_eq!(converted.elements[0].as_object().unwrap().frame, exact);
    }

    #[test]
    fn conversion_resolves_mixed_legacy_flow_without_rounding_the_mm_cursor() {
        let mut original = document(vec![
            element(
                "汉字",
                TextBox {
                    x_mm: 5.031,
                    width_mm: 10.017,
                    ..TextBox::default()
                },
            ),
            LabelElement::Padding { pixels: 7 },
            element(
                "字",
                TextBox {
                    position: PositionMode::Relative,
                    gap_mm: 3.071,
                    width_mm: 10.019,
                    height_mm: 8.017,
                    y_mm: 0.531,
                    ..TextBox::default()
                },
            ),
        ]);
        original.flip_h = true;
        let mut renderer = TextRenderer::with_test_font();
        for (dpi, height) in [(180, 76), (360, 150)] {
            let mut converted = original.clone();
            converted
                .convert_to_free_layout(height, dpi, &mut renderer)
                .unwrap();
            let frame = &converted.elements[2].as_object().unwrap().frame;
            let expected_x = 5.031 + 10.017 + 7.0 * 25.4 / f64::from(dpi) + 3.071;
            assert!((frame.x_mm - expected_x).abs() < 1e-12);
            assert_eq!(
                (frame.y_mm, frame.width_mm, frame.height_mm),
                (0.531, 10.019, 8.017)
            );
            let before = render_document(&original, height, dpi, &mut renderer).unwrap();
            let after = render_document(&converted, height, dpi, &mut renderer).unwrap();
            assert_eq!(before.bounds, after.bounds);
            assert_eq!(before.bitmap.unwrap().data(), after.bitmap.unwrap().data());
        }
    }

    #[test]
    fn overlapping_objects_cannot_bypass_the_raster_working_set_limit() {
        let frame = TextBox {
            width_mm: 10_000.0,
            ..TextBox::default()
        };
        // Every object shares the same extent. A union-only check would allow
        // all of these intermediate bitmaps to remain allocated together.
        let mut doc = document(
            (0..32)
                .map(|_| object(ObjectContent::Padding, frame.clone()))
                .collect(),
        );
        doc.version = 3;
        let error = render_document(&doc, 76, 180, &mut TextRenderer::with_test_font())
            .err()
            .unwrap();
        assert!(error.to_string().contains("working set is too large"));
    }

    #[test]
    fn new_empty_frames_do_not_extend_exact_physical_edges_at_another_dpi() {
        let mut original = document(vec![
            element(
                "汉字",
                TextBox {
                    width_mm: 19.97,
                    ..TextBox::default()
                },
            ),
            LabelElement::Text {
                content: String::new(),
                font_size: None,
                text_box: None,
                align: TextAlign::Left,
                rotation: 0.0,
                flip_h: false,
                flip_v: false,
            },
        ]);
        let mut renderer = TextRenderer::with_test_font();
        for flip_h in [false, true] {
            original.flip_h = flip_h;
            for (conversion_dpi, conversion_height) in [(180, 76), (360, 150)] {
                let mut converted = original.clone();
                converted
                    .convert_to_free_layout(conversion_height, conversion_dpi, &mut renderer)
                    .unwrap();
                for element in &converted.elements {
                    let frame = &element.as_object().unwrap().frame;
                    assert!(frame.x_mm + frame.width_mm <= 19.97);
                }
                for (dpi, height, expected_width) in [(180, 76, 142), (360, 150, 283)] {
                    let before = render_document(&original, height, dpi, &mut renderer).unwrap();
                    let after = render_document(&converted, height, dpi, &mut renderer).unwrap();
                    assert_eq!(before.bounds[0], after.bounds[0]);
                    let before = before.bitmap.unwrap();
                    let after = after.bitmap.unwrap();
                    assert_eq!(before.width(), expected_width);
                    assert_eq!(after.width(), expected_width);
                    assert_eq!(before.data(), after.data());
                }
            }
        }
    }

    fn independent_object(
        content: ObjectContent,
        frame: TextBox,
        width: f64,
        height: f64,
    ) -> LabelElement {
        let mut item = object(content, frame);
        item.as_object_mut().unwrap().content_box = Some(ContentBox {
            width_mm: width,
            height_mm: height,
            offset_x_mm: 0.0,
            offset_y_mm: 0.0,
        });
        item
    }

    fn side_length(corners: &[[f32; 2]; 4], a: usize, b: usize) -> f32 {
        (corners[a][0] - corners[b][0]).hypot(corners[a][1] - corners[b][1])
    }

    #[test]
    fn independent_rotation_preserves_content_size_and_template_pitch() {
        for dpi in [180, 360] {
            let mut doc = document(vec![
                independent_object(
                    image_content(solid_bitmap(40, 20)),
                    TextBox {
                        x_mm: 10.0,
                        y_mm: 3.0,
                        width_mm: 6.0,
                        height_mm: 4.0,
                        ..TextBox::default()
                    },
                    12.0,
                    4.0,
                ),
                object(
                    ObjectContent::Padding,
                    TextBox {
                        position: PositionMode::Relative,
                        gap_mm: 3.0,
                        width_mm: 2.0,
                        height_mm: 4.0,
                        ..TextBox::default()
                    },
                ),
            ]);
            doc.version = 4;
            let mut previous = None;
            for rotation in [0.0, 0.4, 45.0, 90.0, 135.0, 270.0] {
                doc.elements[0].as_object_mut().unwrap().rotation = rotation;
                let result = render(&doc, dpi);
                assert_eq!(result.bounds[1].x, boundary(19.0, dpi).unwrap());
                let quad = &result.content_bounds[0].corners;
                assert!(
                    (side_length(quad, 0, 1) - boundary(12.0, dpi).unwrap() as f32).abs() < 0.001
                );
                assert!(
                    (side_length(quad, 1, 2) - boundary(4.0, dpi).unwrap() as f32).abs() < 0.001
                );
                assert_eq!(result.content_bounds[0].edit_box.width_mm, 12.0);
                if rotation == 0.4 {
                    assert_eq!(
                        result.bitmap.as_ref().unwrap().data(),
                        previous.as_ref().unwrap()
                    );
                }
                previous = Some(result.bitmap.unwrap().data().to_vec());
            }
        }
    }

    #[test]
    fn independent_content_extends_length_but_clips_only_at_printable_edges() {
        let mut doc = document(vec![independent_object(
            image_content(solid_bitmap(20, 10)),
            TextBox {
                width_mm: 4.0,
                height_mm: 4.0,
                ..TextBox::default()
            },
            8.0,
            4.0,
        )]);
        doc.version = 4;
        let item = doc.elements[0].as_object_mut().unwrap();
        item.content_box.as_mut().unwrap().offset_x_mm = 8.0;
        let result = render(&doc, 180);
        assert!(result.diagnostics.is_empty());
        assert!(result.bitmap.as_ref().unwrap().width() > result.bounds[0].width);
        let ink = ink_bounds(result.bitmap.as_ref().unwrap());
        assert!(ink.0 > result.bounds[0].width);
        let item = doc.elements[0].as_object_mut().unwrap();
        item.content_box.as_mut().unwrap().offset_x_mm = -2.0;
        item.content_box.as_mut().unwrap().offset_y_mm = -2.0;
        let result = render(&doc, 180);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|s| s.contains("outside the printable area"))
        );
        assert!(
            result.content_bounds[0]
                .corners
                .iter()
                .any(|p| p[0] < 0.0 && p[1] < 0.0)
        );
        assert!(
            result
                .bitmap
                .as_ref()
                .unwrap()
                .data()
                .iter()
                .any(|b| *b != 0)
        );
    }

    #[test]
    fn independent_content_corners_follow_all_object_and_label_mirrors() {
        let mut doc = document(vec![independent_object(
            image_content(solid_bitmap(30, 20)),
            TextBox {
                x_mm: 20.0,
                y_mm: 2.0,
                width_mm: 20.0,
                height_mm: 6.0,
                ..TextBox::default()
            },
            4.0,
            2.0,
        )]);
        doc.version = 4;
        let item = doc.elements[0].as_object_mut().unwrap();
        item.rotation = 30.0;
        item.content_box.as_mut().unwrap().offset_x_mm = 2.0;
        item.content_box.as_mut().unwrap().offset_y_mm = 1.0;
        let original = render(&doc, 180);
        for mirrors in 0..16 {
            let item = doc.elements[0].as_object_mut().unwrap();
            item.flip_h = mirrors & 1 != 0;
            item.flip_v = mirrors & 2 != 0;
            doc.flip_h = mirrors & 4 != 0;
            doc.flip_v = mirrors & 8 != 0;
            let result = render(&doc, 180);
            let slot = original.bounds[0];
            for (old, actual) in original.content_bounds[0]
                .corners
                .iter()
                .zip(result.content_bounds[0].corners)
            {
                let mut expected = *old;
                if mirrors & 1 != 0 {
                    expected[0] = (slot.x * 2 + slot.width) as f32 - expected[0];
                }
                if mirrors & 2 != 0 {
                    expected[1] = (slot.y * 2 + slot.height) as f32 - expected[1];
                }
                if doc.flip_h {
                    expected[0] = result.bitmap.as_ref().unwrap().width() as f32 - expected[0];
                }
                if doc.flip_v {
                    expected[1] = 76.0 - expected[1];
                }
                assert_eq!(actual, expected);
            }
            assert_eq!(
                result.content_bounds[0].edit_box,
                original.content_bounds[0].edit_box
            );
        }
    }

    #[test]
    fn fitted_content_geometry_describes_the_content_instead_of_the_slot() {
        let mut doc = document(vec![object(
            image_content(solid_bitmap(40, 20)),
            TextBox {
                x_mm: 5.0,
                width_mm: 10.0,
                height_mm: 8.0,
                ..TextBox::default()
            },
        )]);
        doc.version = 3;
        for rotation in [0.0, 45.0, 90.0] {
            doc.elements[0].as_object_mut().unwrap().rotation = rotation;
            let result = render(&doc, 180);
            let quad = &result.content_bounds[0].corners;
            assert!((side_length(quad, 0, 1) / side_length(quad, 1, 2) - 2.0).abs() < 0.05);
            assert!(result.content_bounds[0].edit_box.height_mm < 8.0);
            if rotation == 45.0 {
                assert!((quad[1][1] - quad[0][1]).abs() > 1.0);
            }
        }
        for content in [
            ObjectContent::Padding,
            ObjectContent::Text {
                content: String::new(),
                font_size: None,
                line_spacing: crate::text::DEFAULT_LINE_SPACING,
                align: TextAlign::Left,
            },
        ] {
            doc.elements[0].as_object_mut().unwrap().content = content;
            let result = render(&doc, 180);
            assert_eq!(result.content_bounds.len(), 1);
            assert!(result.content_bounds[0].edit_box.width_mm > 0.0);
        }
    }

    #[test]
    fn independent_content_rejects_invalid_dimensions_versions_and_allocation_limits() {
        let mut doc = document(vec![independent_object(
            ObjectContent::Padding,
            TextBox::default(),
            10.0,
            8.0,
        )]);
        for version in [1, 2, 3] {
            doc.version = version;
            assert!(render_document(&doc, 76, 180, &mut TextRenderer::with_test_font()).is_err());
        }
        doc.version = 4;
        for (width, height, x, y) in [
            (0.0, 8.0, 0.0, 0.0),
            (-1.0, 8.0, 0.0, 0.0),
            (f64::NAN, 8.0, 0.0, 0.0),
            (10.0, f64::INFINITY, 0.0, 0.0),
            (10.0, 8.0, f64::INFINITY, 0.0),
            (10.0, 8.0, 0.0, f64::NAN),
            (0.001, 8.0, 0.0, 0.0),
            (100000.0, 100000.0, 0.0, 0.0),
            (10.0, 8.0, 1e20, 0.0),
        ] {
            doc.elements[0].as_object_mut().unwrap().content_box = Some(ContentBox {
                width_mm: width,
                height_mm: height,
                offset_x_mm: x,
                offset_y_mm: y,
            });
            assert!(render_document(&doc, 76, 180, &mut TextRenderer::with_test_font()).is_err());
        }
    }

    #[test]
    fn pinning_fitted_text_does_not_apply_the_font_margin_twice() {
        for font_size in [None, Some(5.0)] {
            for rotation in [0.0, 45.0, 90.0] {
                let mut item = object(
                    ObjectContent::Text {
                        content: "汉字".into(),
                        font_size,
                        line_spacing: crate::text::DEFAULT_LINE_SPACING,
                        align: TextAlign::Center,
                    },
                    TextBox {
                        x_mm: 5.0,
                        width_mm: 16.0,
                        height_mm: 10.0,
                        ..TextBox::default()
                    },
                );
                item.as_object_mut().unwrap().rotation = rotation;
                let mut doc = document(vec![item]);
                doc.version = 3;
                doc.font_margin = 10;
                let before = render(&doc, 180);
                assert!(before.diagnostics.is_empty());
                let before_ink = ink_bounds(before.bitmap.as_ref().unwrap());
                doc.version = 4;
                doc.elements[0].as_object_mut().unwrap().content_box =
                    Some(before.content_bounds[0].edit_box.clone());
                let after = render(&doc, 180);
                assert!(
                    after.diagnostics.is_empty(),
                    "font={font_size:?}, rotation={rotation}: {:?}",
                    after.diagnostics
                );
                let after_ink = ink_bounds(after.bitmap.as_ref().unwrap());
                assert!(
                    (i64::from(before_ink.2 - before_ink.0) - i64::from(after_ink.2 - after_ink.0))
                        .abs()
                        <= 2,
                    "font={font_size:?}, rotation={rotation}: {before_ink:?} -> {after_ink:?}"
                );
                assert!(
                    (i64::from(before_ink.3 - before_ink.1) - i64::from(after_ink.3 - after_ink.1))
                        .abs()
                        <= 2,
                    "font={font_size:?}, rotation={rotation}: {before_ink:?} -> {after_ink:?}"
                );
            }
        }
    }

    #[test]
    fn independent_tiny_cut_guides_survive_rotation_and_other_content_never_disappears_silently() {
        let mut doc = document(vec![independent_object(
            ObjectContent::CutMark,
            TextBox::default(),
            25.4 / 180.0,
            25.4 / 180.0,
        )]);
        doc.version = 4;
        doc.elements[0].as_object_mut().unwrap().rotation = 45.0;
        assert!(
            render(&doc, 180)
                .bitmap
                .unwrap()
                .data()
                .iter()
                .any(|b| *b != 0)
        );
        doc.elements[0].as_object_mut().unwrap().content = image_content(solid_bitmap(1, 1));
        assert!(render_document(&doc, 76, 180, &mut TextRenderer::with_test_font()).is_err());
    }
    #[test]
    fn physical_slots_use_each_output_axis_without_pitch_drift() {
        use ptouch_core::PrintResolution;
        let frames: Vec<_> = (0..30)
            .map(|index| {
                let frame = TextBox {
                    x_mm: 2.13 + f64::from(index) * 10.37,
                    y_mm: 0.73,
                    width_mm: 8.19,
                    height_mm: 3.01,
                    ..TextBox::default()
                };
                independent_object(
                    ObjectContent::Text {
                        content: String::new(),
                        font_size: None,
                        align: TextAlign::Left,
                        line_spacing: crate::text::DEFAULT_LINE_SPACING,
                    },
                    frame,
                    8.19,
                    3.01,
                )
            })
            .collect();
        let mut doc = document(frames);
        doc.version = 4;
        let mut renderer = TextRenderer::with_test_font();
        for (head_dpi, feed_dpi) in [(180, 180), (180, 360), (360, 360), (360, 720), (360, 180)] {
            let resolution = PrintResolution { head_dpi, feed_dpi };
            let height = if head_dpi == 180 { 76 } else { 150 };
            let rendered =
                render_document_at_resolution(&doc, height, resolution, &mut renderer).unwrap();
            assert_eq!(rendered.bitmap.as_ref().unwrap().height(), height);
            for (index, bound) in rendered.bounds.iter().enumerate() {
                let frame = &doc.elements[index].as_object().unwrap().frame;
                assert_eq!(bound.x, boundary(frame.x_mm, feed_dpi).unwrap());
                assert_eq!(
                    bound.x + bound.width,
                    boundary(frame.x_mm + frame.width_mm, feed_dpi).unwrap()
                );
                assert_eq!(bound.y, boundary(frame.y_mm, head_dpi).unwrap());
                assert_eq!(
                    bound.y + bound.height,
                    boundary(frame.y_mm + frame.height_mm, head_dpi).unwrap()
                );
            }
        }
    }

    #[test]
    fn non_square_object_rotations_and_mirrors_keep_physical_geometry() {
        use ptouch_core::PrintResolution;
        let mut object = independent_object(
            image_content(solid_bitmap(30, 16)),
            TextBox {
                x_mm: 6.0,
                y_mm: 2.0,
                width_mm: 8.0,
                height_mm: 4.0,
                ..TextBox::default()
            },
            6.0,
            2.0,
        );
        object.as_object_mut().unwrap().rotation = 37.0;
        let mut doc = document(vec![object]);
        doc.version = 4;
        let mut renderer = TextRenderer::with_test_font();
        for (head_dpi, feed_dpi) in [(180, 180), (180, 360), (360, 360), (360, 720), (360, 180)] {
            let resolution = PrintResolution { head_dpi, feed_dpi };
            let height = if head_dpi == 180 { 76 } else { 150 };
            let normal =
                render_document_at_resolution(&doc, height, resolution, &mut renderer).unwrap();
            let corners = normal.content_bounds[0].corners;
            let physical = corners.map(|p| {
                [
                    p[0] * 25.4 / f32::from(feed_dpi),
                    p[1] * 25.4 / f32::from(head_dpi),
                ]
            });
            let distance = |a: usize, b: usize| {
                ((physical[a][0] - physical[b][0]).powi(2)
                    + (physical[a][1] - physical[b][1]).powi(2))
                .sqrt()
            };
            assert!((distance(0, 1) - 6.0).abs() < 0.16);
            assert!((distance(1, 2) - 2.0).abs() < 0.16);
            for (horizontal, vertical) in [(true, false), (false, true), (true, true)] {
                let mut flipped = doc.clone();
                flipped.flip_h = horizontal;
                flipped.flip_v = vertical;
                let mirrored =
                    render_document_at_resolution(&flipped, height, resolution, &mut renderer)
                        .unwrap();
                let expected = normal
                    .bitmap
                    .as_ref()
                    .unwrap()
                    .mirrored(horizontal, vertical);
                assert_eq!(mirrored.bitmap.as_ref().unwrap().data(), expected.data());
                let expected = ContentGeometry {
                    element_index: 0,
                    corners: corners.map(|p| {
                        [
                            if horizontal {
                                expected.width() as f32 - p[0]
                            } else {
                                p[0]
                            },
                            if vertical {
                                expected.height() as f32 - p[1]
                            } else {
                                p[1]
                            },
                        ]
                    }),
                    edit_box: normal.content_bounds[0].edit_box.clone(),
                };
                assert_eq!(mirrored.content_bounds[0], expected);
            }
        }
    }

    #[test]
    fn document_resolution_preserves_legacy_sampling_and_rejects_invalid_modes() {
        use ptouch_core::PrintResolution;
        let mut doc = document(vec![
            LabelElement::Padding { pixels: 7 },
            LabelElement::CutMark,
            LabelElement::Padding { pixels: 5 },
        ]);
        doc.version = 1;
        doc.flip_h = true;
        let mut renderer = TextRenderer::with_test_font();
        for feed_dpi in [180, 360, 720] {
            let resolution = PrintResolution {
                head_dpi: 360,
                feed_dpi,
            };
            let expected = crate::document::render_elements_at_resolution(
                &doc.elements,
                150,
                &doc.font_name,
                doc.font_margin,
                &mut renderer,
                resolution,
            )
            .unwrap()
            .unwrap()
            .mirrored(true, false);
            let actual = render_document_at_resolution(&doc, 150, resolution, &mut renderer)
                .unwrap()
                .bitmap
                .unwrap();
            assert_eq!(actual.data(), expected.data());
        }
        for (head_dpi, feed_dpi) in [(0, 0), (180, 0), (180, 270)] {
            assert!(
                render_document_at_resolution(
                    &doc,
                    76,
                    PrintResolution { head_dpi, feed_dpi },
                    &mut renderer
                )
                .is_err()
            );
        }
    }
    #[test]
    fn physical_svg_objects_render_source_detail_at_high_resolution() {
        use ptouch_core::PrintResolution;
        let bytes = br#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="4"><path d="M1 3L7 1" stroke="black" stroke-width="0.15"/></svg>"#;
        let frame = TextBox {
            width_mm: 12.0,
            height_mm: 6.0,
            ..TextBox::default()
        };
        let mut object = LabelObject::image_from_bytes(None, bytes.to_vec(), frame);
        object.content_box = Some(ContentBox {
            width_mm: 12.0,
            height_mm: 6.0,
            offset_x_mm: 0.0,
            offset_y_mm: 0.0,
        });
        let mut doc = document(vec![LabelElement::Object(object)]);
        doc.version = 4;
        let mut renderer = TextRenderer::with_test_font();
        let high = render_document_at_resolution(
            &doc,
            76,
            PrintResolution {
                head_dpi: 180,
                feed_dpi: 360,
            },
            &mut renderer,
        )
        .unwrap()
        .bitmap
        .unwrap();
        assert!(high.data().iter().any(|byte| *byte != 0));
        assert!((0..high.height()).any(|y| {
            (0..high.width() / 2).any(|x| high.get_pixel(x * 2, y) != high.get_pixel(x * 2 + 1, y))
        }));
        // The decoded cache is not the source of print geometry or detail.
        if let ObjectContent::Image { bitmap, .. } =
            &mut doc.elements[0].as_object_mut().unwrap().content
        {
            *bitmap = None;
        }
        let reloaded = render_document_at_resolution(
            &doc,
            76,
            PrintResolution {
                head_dpi: 180,
                feed_dpi: 360,
            },
            &mut renderer,
        )
        .unwrap()
        .bitmap
        .unwrap();
        assert_eq!(high.data(), reloaded.data());
    }
}
