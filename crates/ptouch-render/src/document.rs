// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Label document model and shared element rendering.
//!
//! A [`LabelDocument`] is the serializable description of a label: global tape
//! and font settings plus an ordered list of [`LabelElement`]s. The same model
//! drives both the GUI canvas and command-line rendering, so a design composed
//! in one can be reproduced exactly in the other.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::path::PathBuf;

use log::error;
use serde::{Deserialize, Serialize};

use crate::RenderError;
use crate::Result;
use crate::bitmap::LabelBitmap;
use crate::compose;
use crate::image_loader::{self, ImageLoadOptions};
use crate::text::{TextAlign, TextRenderer};

pub use crate::layout::{
    ContentBox, ContentGeometry, ElementBounds, PositionMode, RenderedLabel, TextBox,
    VerticalAlign, render_document, render_document_at_resolution,
};

/// Newest supported on-disk layout format version.
pub const DOCUMENT_VERSION: u32 = 5;
/// New layouts need v5 only when custom line spacing is used.
pub const DEFAULT_DOCUMENT_VERSION: u32 = 4;

/// Default design resolution for layouts saved before the dpi field existed.
fn default_dpi() -> u16 {
    180
}

/// A complete label design: global settings plus an ordered element list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelDocument {
    /// Layout format version. See [`DOCUMENT_VERSION`].
    pub version: u32,
    /// Tape width in millimeters the design was created for.
    pub tape_width_mm: u8,
    /// Resolution used for offline rendering. Physical object frames are
    /// converted at the output device's DPI when printing. Legacy pixel
    /// dimensions retain their original behavior. Defaults to 180 dpi.
    #[serde(default = "default_dpi")]
    pub dpi: u16,
    /// Font family name used for text elements.
    pub font_name: String,
    /// Font top/bottom margin in pixels.
    pub font_margin: u32,
    /// Mirror the whole composed label left-right (horizontal). Applied once,
    /// after all elements are composed, independently of per-element flips.
    #[serde(default)]
    pub flip_h: bool,
    /// Mirror the whole composed label top-bottom (vertical). Applied once,
    /// after all elements are composed, independently of per-element flips.
    #[serde(default)]
    pub flip_v: bool,
    /// Elements in composition order; relative placement follows this order.
    pub elements: Vec<LabelElement>,
}

impl LabelDocument {
    /// Serialize the document to a TOML string.
    pub fn to_toml_string(&self) -> Result<String> {
        // Never write positioning fields under v1: older builds would silently
        // ignore them. Ordinary v1 documents can still round-trip as v1.
        let mut doc = self.clone();
        for element in &doc.elements {
            doc.version = doc.version.max(element.minimum_version());
        }
        Ok(toml::to_string(&doc)?)
    }

    /// Parse a document from a TOML string.
    ///
    /// Rejects an unsupported format version and decodes every embedded image
    /// into its render cache, so the returned document is ready to render.
    pub fn from_toml_str(text: &str) -> Result<Self> {
        let mut doc: LabelDocument = toml::from_str(text)?;
        if doc.version == 0 || doc.version > DOCUMENT_VERSION {
            return Err(RenderError::Layout(format!(
                "unsupported layout version {} (this build supports up to {})",
                doc.version, DOCUMENT_VERSION
            )));
        }
        if doc.version < 2 && doc.has_text_boxes() {
            return Err(RenderError::Layout(
                "text boxes require layout version 2".into(),
            ));
        }
        if doc.version < 3
            && doc
                .elements
                .iter()
                .any(|element| element.as_object().is_some())
        {
            return Err(RenderError::Layout(
                "free-layout objects require layout version 3".into(),
            ));
        }
        if doc.dpi == 0 {
            return Err(RenderError::Layout(
                "design dpi must be greater than zero".into(),
            ));
        }
        if doc.version < 4 && doc.has_content_boxes() {
            return Err(RenderError::Layout(
                "independent content boxes require layout version 4".into(),
            ));
        }
        for object in doc.elements.iter().filter_map(LabelElement::as_object) {
            if let ObjectContent::Text { line_spacing, .. } = &object.content {
                crate::text::validate_line_spacing(*line_spacing)?;
                if *line_spacing != crate::text::DEFAULT_LINE_SPACING && doc.version < 5 {
                    return Err(RenderError::Layout(
                        "custom line spacing requires layout version 5".into(),
                    ));
                }
            }
            if let Some(content_box) = &object.content_box {
                content_box.validate()?;
            }
        }
        doc.decode_image_caches()?;
        Ok(doc)
    }

    fn has_text_boxes(&self) -> bool {
        self.elements.iter().any(|element| {
            matches!(
                element,
                LabelElement::Text {
                    text_box: Some(_),
                    ..
                }
            )
        })
    }

    fn has_content_boxes(&self) -> bool {
        self.elements
            .iter()
            .filter_map(LabelElement::as_object)
            .any(|object| object.content_box.is_some())
    }

    /// Decode every image element's embedded bytes into its render cache.
    ///
    /// Returns an error if any embedded image cannot be decoded, so a corrupt
    /// layout fails loudly rather than rendering blank.
    pub fn decode_image_caches(&mut self) -> Result<()> {
        for element in &mut self.elements {
            if let Some((image_data, bitmap)) = element.image_data_and_cache_mut()
                && bitmap.is_none()
                && !image_data.is_empty()
            {
                let decoded = image_loader::load_image_from_reader(
                    Cursor::new(image_data.as_slice()),
                    &ImageLoadOptions::default(),
                )?;
                *bitmap = Some(decoded);
            }
        }
        Ok(())
    }

    /// Collect the unique `{{name}}` placeholder names used in text elements.
    ///
    /// The result is sorted for stable display and de-duplication.
    pub fn placeholders(&self) -> Vec<String> {
        let mut names = BTreeSet::new();
        for element in &self.elements {
            if let Some(content) = element.text_content() {
                collect_placeholder_names(content, &mut names);
            }
        }
        names.into_iter().collect()
    }

    /// Replace `{{name}}` placeholders in all text elements using `values`.
    ///
    /// A placeholder with no matching value is replaced with an empty string;
    /// callers that want to reject missing values should check
    /// [`LabelDocument::placeholders`] against the provided keys first.
    pub fn apply_values(&mut self, values: &BTreeMap<String, String>) {
        for element in &mut self.elements {
            if let Some(content) = element.text_content_mut() {
                *content = substitute_placeholders(content, |name| values.get(name).cloned());
            }
        }
    }

    /// Explicitly convert the current layout into editable physical objects.
    ///
    /// Existing physical geometry is preserved exactly; legacy pixel positions
    /// and sizes (including padding) are frozen at the supplied output DPI.
    /// Text and image content stays editable; legacy auto
    /// sizing and image cropping can change under the new fixed-frame fitting
    /// rules. Opening or saving an old document never invokes this conversion.
    /// Empty legacy text and zero-width padding receive selectable frames
    /// inside the existing extent, preserving whole-label mirror positions.
    /// If the original label has no extent, empty text gets a default frame.
    pub fn convert_to_free_layout(
        &mut self,
        tape_width_px: u32,
        target_dpi: u16,
        renderer: &mut TextRenderer,
    ) -> Result<()> {
        *self = crate::layout::convert_to_objects(self, tape_width_px, target_dpi, renderer)?;
        Ok(())
    }
}

/// True if `name` is a valid placeholder identifier: non-empty and made of
/// ASCII letters, digits, or underscores.
fn is_valid_placeholder(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Collect placeholder names from `content` into `names`.
fn collect_placeholder_names(content: &str, names: &mut BTreeSet<String>) {
    let mut rest = content;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        let name = after[..end].trim();
        if is_valid_placeholder(name) {
            names.insert(name.to_string());
        }
        rest = &after[end + 2..];
    }
}

/// Replace every valid `{{name}}` in `content`. `lookup` returns the value for a
/// name, or `None` to substitute an empty string. Text that is not a valid
/// placeholder is left untouched.
fn substitute_placeholders(content: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut result = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        let name = after[..end].trim();
        if is_valid_placeholder(name) {
            result.push_str(&rest[..start]);
            if let Some(value) = lookup(name) {
                result.push_str(&value);
            }
            rest = &after[end + 2..];
        } else {
            // Not a placeholder; keep the literal "{{" and continue past it.
            result.push_str(&rest[..start + 2]);
            rest = after;
        }
    }
    result.push_str(rest);
    result
}

/// A single element in the label composition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LabelElement {
    /// A physical object; v4 optionally separates content geometry from its slot.
    Object(LabelObject),
    /// A text block with content, optional font size, alignment, and rotation.
    Text {
        /// Text content; newlines separate lines.
        content: String,
        /// Explicit font size: points for a text box, legacy raster pixels
        /// otherwise. `None` auto-fits the box or legacy tape height.
        #[serde(skip_serializing_if = "Option::is_none")]
        font_size: Option<f32>,
        /// Optional fixed physical container. Absent preserves v1 layout.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text_box: Option<TextBox>,
        /// Horizontal alignment of the text block.
        align: TextAlign,
        /// Rotation angle in degrees (clockwise). 0.0 = horizontal.
        rotation: f32,
        /// Mirror this element left-right (horizontal). Applied to the element's
        /// own bitmap, before it is composed into the label.
        #[serde(default)]
        flip_h: bool,
        /// Mirror this element top-bottom (vertical). Applied to the element's
        /// own bitmap, before it is composed into the label.
        #[serde(default)]
        flip_v: bool,
    },
    /// An image embedded as its original source bytes.
    Image {
        /// Original file path, kept for display only. Never required to exist.
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
        /// Source image bytes (PNG/JPEG/SVG/...), stored as base64 in the file.
        #[serde(with = "crate::base64_bytes")]
        image_data: Vec<u8>,
        /// Decoded render cache; rebuilt from `image_data`, never serialized.
        #[serde(skip)]
        bitmap: Option<LabelBitmap>,
        /// Rotation angle in degrees (clockwise). 0.0 = horizontal.
        rotation: f32,
        /// Target height in pixels. `None` = auto (fit to tape height).
        #[serde(skip_serializing_if = "Option::is_none")]
        target_height: Option<u32>,
        /// Mirror this element left-right (horizontal). Applied to the element's
        /// own bitmap, before it is composed into the label.
        #[serde(default)]
        flip_h: bool,
        /// Mirror this element top-bottom (vertical). Applied to the element's
        /// own bitmap, before it is composed into the label.
        #[serde(default)]
        flip_v: bool,
    },
    /// A cut mark separator.
    CutMark,
    /// Horizontal padding in pixels.
    Padding {
        /// Padding width in pixels.
        pixels: u32,
    },
}

/// A physical object with a fixed, axis-aligned template slot. Without a
/// `content_box`, content rotates and fits inside that slot. With one, content
/// rotates independently and may extend beyond it. Object mirroring reflects
/// the content and its offset around the slot center afterwards.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabelObject {
    pub frame: TextBox,
    /// Independent, unrotated content dimensions and offset from the slot center.
    /// Absent in v3: retain the original fit-to-slot rendering behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_box: Option<ContentBox>,
    #[serde(default)]
    pub rotation: f32,
    #[serde(default)]
    pub flip_h: bool,
    #[serde(default)]
    pub flip_v: bool,
    pub content: ObjectContent,
}

/// Editable content of a physical object. Geometry and transforms are stored
/// only on the containing [`LabelObject`], never recursively on the content.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObjectContent {
    Text {
        content: String,
        /// Font size in typographic points; absent means fit to the frame.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        font_size: Option<f32>,
        align: TextAlign,
        /// Baseline distance in multiples of font size; custom values require v5.
        #[serde(
            default = "crate::text::default_line_spacing",
            skip_serializing_if = "crate::text::is_default_line_spacing"
        )]
        line_spacing: f32,
    },
    Image {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
        #[serde(with = "crate::base64_bytes")]
        image_data: Vec<u8>,
        #[serde(skip)]
        bitmap: Option<LabelBitmap>,
    },
    CutMark,
    /// An empty physical frame which still occupies its full width.
    Padding,
}

impl LabelObject {
    pub fn text(content: impl Into<String>, frame: TextBox) -> Self {
        Self {
            frame,
            content_box: None,
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
            content: ObjectContent::Text {
                content: content.into(),
                font_size: None,
                line_spacing: crate::text::DEFAULT_LINE_SPACING,
                align: TextAlign::Left,
            },
        }
    }

    pub fn image_from_bytes(path: Option<PathBuf>, image_data: Vec<u8>, frame: TextBox) -> Self {
        let bitmap = image_loader::load_image_from_reader(
            Cursor::new(&image_data),
            &ImageLoadOptions::default(),
        )
        .ok();
        Self {
            frame,
            content_box: None,
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
            content: ObjectContent::Image {
                path,
                image_data,
                bitmap,
            },
        }
    }
}

impl LabelElement {
    /// Minimum reader version needed to preserve this element's rendering.
    pub fn minimum_version(&self) -> u32 {
        match self {
            Self::Object(object) => {
                if let ObjectContent::Text { line_spacing, .. } = &object.content
                    && *line_spacing != crate::text::DEFAULT_LINE_SPACING
                {
                    5
                } else if object.content_box.is_some() {
                    4
                } else {
                    3
                }
            }
            Self::Text {
                text_box: Some(_), ..
            } => 2,
            _ => 1,
        }
    }

    pub fn as_object(&self) -> Option<&LabelObject> {
        match self {
            Self::Object(object) => Some(object),
            _ => None,
        }
    }

    pub fn as_object_mut(&mut self) -> Option<&mut LabelObject> {
        match self {
            Self::Object(object) => Some(object),
            _ => None,
        }
    }

    fn text_content(&self) -> Option<&str> {
        match self {
            Self::Text { content, .. }
            | Self::Object(LabelObject {
                content: ObjectContent::Text { content, .. },
                ..
            }) => Some(content),
            _ => None,
        }
    }

    fn text_content_mut(&mut self) -> Option<&mut String> {
        match self {
            Self::Text { content, .. }
            | Self::Object(LabelObject {
                content: ObjectContent::Text { content, .. },
                ..
            }) => Some(content),
            _ => None,
        }
    }

    fn image_data_and_cache_mut(&mut self) -> Option<(&mut Vec<u8>, &mut Option<LabelBitmap>)> {
        match self {
            Self::Image {
                image_data, bitmap, ..
            }
            | Self::Object(LabelObject {
                content:
                    ObjectContent::Image {
                        image_data, bitmap, ..
                    },
                ..
            }) => Some((image_data, bitmap)),
            _ => None,
        }
    }
    /// Build an image element from decoded source bytes, decoding a render
    /// cache eagerly so previews and rendering are fast.
    pub fn image_from_bytes(path: Option<PathBuf>, image_data: Vec<u8>) -> Self {
        let bitmap = image_loader::load_image_from_reader(
            Cursor::new(&image_data),
            &ImageLoadOptions::default(),
        )
        .ok();
        LabelElement::Image {
            path,
            image_data,
            bitmap,
            rotation: 0.0,
            target_height: None,
            flip_h: false,
            flip_v: false,
        }
    }

    /// Set the decoded render cache for an image element.
    pub fn set_image_bitmap(&mut self, decoded: Option<LabelBitmap>) {
        if let Some((_, bitmap)) = self.image_data_and_cache_mut() {
            *bitmap = decoded;
        }
    }

    /// Returns a short display name for the element list.
    pub fn display_name(&self) -> String {
        match self {
            LabelElement::Text { content, .. }
            | LabelElement::Object(LabelObject {
                content: ObjectContent::Text { content, .. },
                ..
            }) => {
                let preview: String = content.chars().take(20).collect();
                if content.chars().count() > 20 {
                    format!("Text: {}...", preview)
                } else {
                    format!("Text: {}", preview)
                }
            }
            LabelElement::Image {
                path, image_data, ..
            }
            | LabelElement::Object(LabelObject {
                content:
                    ObjectContent::Image {
                        path, image_data, ..
                    },
                ..
            }) => {
                if let Some(symbol) = crate::symbol::metadata(image_data) {
                    return format!("Symbol: {}", symbol.name);
                }
                let name = path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "embedded".to_string());
                format!("Image: {}", name)
            }
            LabelElement::CutMark
            | LabelElement::Object(LabelObject {
                content: ObjectContent::CutMark,
                ..
            }) => "Cut Mark".to_string(),
            LabelElement::Padding { pixels } => format!("Padding: {} px", pixels),
            LabelElement::Object(LabelObject {
                frame,
                content: ObjectContent::Padding,
                ..
            }) => format!("Padding: {:.2} mm", frame.width_mm),
        }
    }
}

/// Font settings shared across all text elements in a document.
struct FontSettings<'a> {
    /// Font family name.
    name: &'a str,
    /// Top/bottom margin in pixels.
    margin: u32,
}

/// Print pixels and the feed-column range contributed by each source element.
/// Missing or empty ranges have no visible output. Ranges share the bitmap's
/// resolution and are collected during composition, rather than re-rendered.
pub struct RenderedElements {
    pub bitmap: LabelBitmap,
    pub element_spans: Vec<Option<std::ops::Range<u32>>>,
}

impl RenderedElements {
    /// Mirror the composed print pixels and their element coordinates together.
    pub fn mirrored(mut self, horizontal: bool, vertical: bool) -> Self {
        if horizontal {
            let width = self.bitmap.width();
            for span in self.element_spans.iter_mut().flatten() {
                *span = width - span.end..width - span.start;
            }
        }
        self.bitmap = self.bitmap.mirrored(horizontal, vertical);
        self
    }
}

/// Render an ordered element list into a single label bitmap.
///
/// Elements are rendered to `tape_width_px` tall segments and concatenated left
/// to right. Per-element rendering failures (empty text, undecodable image) are
/// logged and skipped rather than aborting the whole label. Returns `None` when
/// no element produced any output.
/// Physical text boxes must use [`render_document`] with a target DPI instead.
pub fn render_elements(
    elements: &[LabelElement],
    tape_width_px: u32,
    font_name: &str,
    font_margin: u32,
    renderer: &mut TextRenderer,
) -> Result<Option<LabelBitmap>> {
    Ok(
        render_elements_scaled(elements, tape_width_px, font_name, font_margin, renderer, 1)?
            .map(|rendered| rendered.bitmap),
    )
}

fn render_elements_scaled(
    elements: &[LabelElement],
    tape_width_px: u32,
    font_name: &str,
    font_margin: u32,
    renderer: &mut TextRenderer,
    source_scale: u32,
) -> Result<Option<RenderedElements>> {
    let mut result: Option<LabelBitmap> = None;
    let mut element_spans = vec![None; elements.len()];

    for (index, element) in elements.iter().enumerate() {
        let segment = match element {
            LabelElement::Object(_) => {
                return Err(RenderError::Layout(
                    "free-layout objects require render_document with a target DPI".into(),
                ));
            }
            LabelElement::Text {
                content,
                font_size,
                align,
                rotation,
                flip_h,
                flip_v,
                text_box,
            } => {
                if text_box.is_some() {
                    return Err(RenderError::Layout(
                        "text boxes require render_document with a target DPI".into(),
                    ));
                }
                match render_text_segment(
                    renderer,
                    content,
                    *font_size,
                    *align,
                    *rotation,
                    tape_width_px,
                    &FontSettings {
                        name: font_name,
                        margin: font_margin,
                    },
                ) {
                    Some(seg) => seg.mirrored(*flip_h, *flip_v),
                    None => continue,
                }
            }
            LabelElement::Image {
                image_data,
                bitmap,
                rotation,
                target_height,
                flip_h,
                flip_v,
                ..
            } => match render_image_segment(
                bitmap.as_ref(),
                image_data,
                *rotation,
                *target_height,
                tape_width_px,
            ) {
                Some(seg) => seg.mirrored(*flip_h, *flip_v),
                None => continue,
            },
            LabelElement::CutMark => {
                compose::cutmark(tape_width_px / source_scale).scale_to_height(tape_width_px)
            }
            LabelElement::Padding { pixels } => compose::padding(tape_width_px, *pixels),
        };

        let start = result.as_ref().map_or(0, LabelBitmap::width);
        element_spans[index] = Some(start..start + segment.width());
        result = Some(match result {
            Some(prev) => prev.append(&segment),
            None => segment,
        });
    }

    Ok(result.map(|bitmap| RenderedElements {
        bitmap,
        element_spans,
    }))
}

/// Render print pixels at a selected resolution. Document coordinates stay in
/// square head-DPI pixels; only the output feed axis changes density.
pub fn render_elements_at_resolution(
    elements: &[LabelElement],
    tape_width_px: u32,
    font_name: &str,
    font_margin: u32,
    renderer: &mut TextRenderer,
    resolution: ptouch_core::PrintResolution,
) -> Result<Option<LabelBitmap>> {
    Ok(render_elements_with_layout_at_resolution(
        elements,
        tape_width_px,
        font_name,
        font_margin,
        renderer,
        resolution,
    )?
    .map(|rendered| rendered.bitmap))
}

/// Render once, retaining each source element's range in the final print pixels.
pub fn render_elements_with_layout_at_resolution(
    elements: &[LabelElement],
    tape_width_px: u32,
    font_name: &str,
    font_margin: u32,
    renderer: &mut TextRenderer,
    resolution: ptouch_core::PrintResolution,
) -> Result<Option<RenderedElements>> {
    if resolution.head_dpi == 0 || resolution.feed_dpi == 0 {
        return Err(crate::RenderError::Text(
            "Resolution must be positive".into(),
        ));
    }
    if u32::from(resolution.feed_dpi) == u32::from(resolution.head_dpi) * 2 {
        render_elements_high_resolution_with_layout(
            elements,
            tape_width_px,
            font_name,
            font_margin,
            renderer,
        )
    } else if resolution.feed_dpi == resolution.head_dpi {
        render_elements_scaled(elements, tape_width_px, font_name, font_margin, renderer, 1)
    } else if u32::from(resolution.feed_dpi) * 2 == u32::from(resolution.head_dpi) {
        Ok(
            render_elements_scaled(elements, tape_width_px, font_name, font_margin, renderer, 1)?
                .map(|mut rendered| {
                    rendered.bitmap = rendered.bitmap.halve_feed();
                    // The whole label samples source columns 0, 2, 4, ... .
                    // Rounding each boundary up assigns every sampled column
                    // to its source element, even at odd segment boundaries.
                    for span in rendered.element_spans.iter_mut().flatten() {
                        *span = span.start.div_ceil(2)..span.end.div_ceil(2);
                    }
                    rendered
                }),
        )
    } else {
        Err(crate::RenderError::Text(
            "Unsupported head/feed resolution ratio".into(),
        ))
    }
}

/// Render source content at twice the feed resolution, preserving tape height.
pub fn render_elements_high_resolution(
    elements: &[LabelElement],
    tape_width_px: u32,
    font_name: &str,
    font_margin: u32,
    renderer: &mut TextRenderer,
) -> Result<Option<LabelBitmap>> {
    Ok(render_elements_high_resolution_with_layout(
        elements,
        tape_width_px,
        font_name,
        font_margin,
        renderer,
    )?
    .map(|rendered| rendered.bitmap))
}

fn render_elements_high_resolution_with_layout(
    elements: &[LabelElement],
    tape_width_px: u32,
    font_name: &str,
    font_margin: u32,
    renderer: &mut TextRenderer,
) -> Result<Option<RenderedElements>> {
    let mut elements = elements.to_vec();
    for element in &mut elements {
        match element {
            LabelElement::Text { font_size, .. } => *font_size = font_size.map(|v| v * 2.0),
            LabelElement::Image {
                target_height,
                bitmap,
                image_data,
                ..
            } => {
                *target_height = target_height.map(|v| v * 2);
                if !image_data.is_empty() {
                    *bitmap = None;
                }
            }
            LabelElement::Padding { pixels } => *pixels *= 2,
            LabelElement::CutMark => {}
            LabelElement::Object(_) => {}
        }
    }
    Ok(render_elements_scaled(
        &elements,
        tape_width_px * 2,
        font_name,
        font_margin * 2,
        renderer,
        2,
    )?
    .map(|mut rendered| {
        rendered.bitmap = rendered.bitmap.halve_height();
        rendered
    }))
}

/// Render a single text element to a tape-height bitmap, or `None` to skip it.
fn render_text_segment(
    renderer: &mut TextRenderer,
    content: &str,
    font_size: Option<f32>,
    align: TextAlign,
    rotation: f32,
    tape_width_px: u32,
    font: &FontSettings,
) -> Option<LabelBitmap> {
    if content.is_empty() {
        return None;
    }
    let lines: Vec<&str> = content.lines().collect();
    let is_rotated = is_rotated(rotation);

    // For rotated text with auto size, pick a size whose rotated bounding box
    // fits within the tape height.
    let effective_font_size = rotation_aware_font_size(font_size, rotation, &lines, tape_width_px);

    // Rotated text needs a taller render area so all lines stay visible; the
    // height becomes tape length after rotation.
    let render_height = if is_rotated {
        if let Some(fs) = effective_font_size {
            let line_h = (fs * 1.2).ceil();
            let text_h = (lines.len() as f32 * line_h).ceil() as u32 + font.margin * 2;
            text_h.max(tape_width_px)
        } else {
            tape_width_px
        }
    } else {
        tape_width_px
    };

    let bmp = match renderer.render_text(
        &lines,
        render_height,
        font.name,
        effective_font_size,
        font.margin,
        align,
    ) {
        Ok(bmp) => bmp,
        Err(e) => {
            error!("Text render failed: {}", e);
            return None;
        }
    };

    if is_rotated {
        // Trim whitespace so the rotated bounding box reflects actual content,
        // not full tape-height padding.
        Some(
            bmp.trim_vertical()
                .rotate(rotation)
                .fit_height(tape_width_px),
        )
    } else {
        Some(bmp)
    }
}

/// Render a single image element to a tape-height bitmap, or `None` to skip it.
fn render_image_segment(
    cached: Option<&LabelBitmap>,
    image_data: &[u8],
    rotation: f32,
    target_height: Option<u32>,
    tape_width_px: u32,
) -> Option<LabelBitmap> {
    // A symbol's eager 24-unit cache is only a loading check. Rasterize its
    // vector source at the actual print size in legacy layouts as well.
    let cached = cached.filter(|_| crate::symbol::metadata(image_data).is_none());
    let bmp = if let Some(bmp) = cached {
        bmp.clone()
    } else if !image_data.is_empty() {
        match image_loader::load_image_from_reader(
            Cursor::new(image_data),
            &ImageLoadOptions {
                target_height: Some(target_height.unwrap_or(tape_width_px)),
                ..ImageLoadOptions::default()
            },
        ) {
            Ok(bmp) => bmp,
            Err(e) => {
                error!("Image decode failed: {}", e);
                return None;
            }
        }
    } else {
        error!("Image element has no data");
        return None;
    };

    // Scale to target height (auto = tape height, manual = specified).
    let bmp = bmp.scale_to_height(target_height.unwrap_or(tape_width_px));

    if is_rotated(rotation) {
        Some(bmp.rotate(rotation).fit_height(tape_width_px))
    } else {
        Some(bmp.fit_height(tape_width_px))
    }
}

/// Returns true if the angle is not effectively a multiple of 360 degrees.
fn is_rotated(rotation_deg: f32) -> bool {
    let norm = ((rotation_deg % 360.0) + 360.0) % 360.0;
    !(norm.abs() < 0.5 || (norm - 360.0).abs() < 0.5)
}

/// Calculate a font size that fits within `tape_height` after rotation.
///
/// For 0 degrees, returns `font_size` unchanged (`None` lets the renderer
/// auto-size). For other angles, estimates the largest font size whose rotated
/// bounding box fits within the tape height.
fn rotation_aware_font_size(
    font_size: Option<f32>,
    rotation_deg: f32,
    lines: &[&str],
    tape_height: u32,
) -> Option<f32> {
    // User-specified font size: use it directly, no auto-adjustment.
    if font_size.is_some() {
        return font_size;
    }
    if !is_rotated(rotation_deg) {
        return None;
    }

    let norm = ((rotation_deg % 360.0) + 360.0) % 360.0;
    let angle_rad = norm.to_radians();
    let sin_a = angle_rad.sin().abs();
    let cos_a = angle_rad.cos().abs();
    let num_lines = lines.len().max(1) as f32;
    let max_chars = lines.iter().map(|l| l.chars().count()).max().unwrap_or(1) as f32;
    let available = tape_height as f32;

    // Rotated bounding-box height:
    //   bbox_h = text_width * |sin| + text_height * |cos|
    // with text_width  ~ max_chars * font_size * 0.6
    //      text_height ~ num_lines * font_size * 1.2
    // Solve for font_size.
    let denom = max_chars * 0.6 * sin_a + num_lines * 1.2 * cos_a;
    if denom > 0.01 {
        Some((available / denom).max(4.0))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_resolution_rasterizes_embedded_svg_from_source() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="16"><path d="M1 15L31 1" stroke="black" stroke-width="1"/></svg>"#;
        let element = LabelElement::image_from_bytes(None, svg.to_vec());
        let mut doc = text_doc("");
        doc.elements = vec![element];
        let doc = LabelDocument::from_toml_str(&doc.to_toml_string().unwrap()).unwrap();
        let mut renderer = TextRenderer::new();
        let standard = render_elements(&doc.elements, 32, "sans-serif", 0, &mut renderer)
            .unwrap()
            .unwrap();
        let high = render_elements_at_resolution(
            &doc.elements,
            32,
            "sans-serif",
            0,
            &mut renderer,
            ptouch_core::PrintResolution {
                head_dpi: 180,
                feed_dpi: 360,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(high.width(), standard.width() * 2);
        assert_eq!(high.height(), standard.height());
        assert!(
            (0..high.width() / 2).any(|x| {
                (0..high.height()).any(|y| high.get_pixel(x * 2, y) != high.get_pixel(x * 2 + 1, y))
            }),
            "vector detail must survive between adjacent feed columns"
        );
    }

    #[test]
    fn high_resolution_preserves_cutmark_size_and_dash_pattern() {
        let mut renderer = TextRenderer::new();
        let elements = [LabelElement::CutMark, LabelElement::Padding { pixels: 7 }];
        let standard = render_elements(&elements, 70, "sans-serif", 0, &mut renderer)
            .unwrap()
            .unwrap();
        let high = render_elements_high_resolution(&elements, 70, "sans-serif", 0, &mut renderer)
            .unwrap()
            .unwrap();
        assert_eq!(high.width(), standard.width() * 2);
        for x in 0..standard.width() {
            for y in 0..standard.height() {
                assert_eq!(high.get_pixel(x * 2, y), standard.get_pixel(x, y));
                assert_eq!(high.get_pixel(x * 2 + 1, y), standard.get_pixel(x, y));
            }
        }
    }
    #[test]
    fn high_resolution_preserves_height_and_renders_distinct_feed_columns() {
        let elements = vec![
            LabelElement::Text {
                text_box: None,
                content: "Wi-Fi W".into(),
                font_size: Some(36.0),
                align: TextAlign::Left,
                rotation: 0.0,
                flip_h: false,
                flip_v: false,
            },
            LabelElement::Padding { pixels: 10 },
        ];
        let mut renderer = TextRenderer::new();
        let normal = render_elements(&elements, 70, "DejaVuSans", 0, &mut renderer)
            .unwrap()
            .unwrap();
        let high = render_elements_high_resolution(&elements, 70, "DejaVuSans", 0, &mut renderer)
            .unwrap()
            .unwrap();
        assert_eq!(high.height(), normal.height());
        assert!(high.width().abs_diff(normal.width() * 2) <= 2);
        assert!(
            (0..high.width() / 2)
                .any(|x| (0..70).any(|y| high.get_pixel(2 * x, y) != high.get_pixel(2 * x + 1, y)))
        );
    }

    #[test]
    fn every_quality_preserves_physical_geometry_and_high_modes_add_detail() {
        use ptouch_core::quality::QualityProfile;
        for (name, head, height) in [
            ("PT-P750W", 180, 70),
            ("PT-9500PC", 360, 150),
            ("PT-9700PC", 360, 150),
        ] {
            for rotation in [0.0, 90.0] {
                let elements = vec![
                    LabelElement::Text {
                        text_box: None,
                        content: "Wi-Fi W".into(),
                        font_size: Some(36.0),
                        align: TextAlign::Left,
                        rotation,
                        flip_h: true,
                        flip_v: false,
                    },
                    LabelElement::Padding { pixels: 10 },
                ];
                let mut renderer = TextRenderer::new();
                let normal = render_elements(&elements, height, "DejaVuSans", 0, &mut renderer)
                    .unwrap()
                    .unwrap();
                for mode in QualityProfile::for_model(name, head).modes {
                    let printed = render_elements_at_resolution(
                        &elements,
                        height,
                        "DejaVuSans",
                        0,
                        &mut renderer,
                        mode.resolution,
                    )
                    .unwrap()
                    .unwrap()
                    .mirrored(true, true);
                    assert_eq!(printed.height(), height);
                    let expected = f64::from(normal.width()) * f64::from(mode.resolution.feed_dpi)
                        / f64::from(head);
                    assert!(
                        (f64::from(printed.width()) - expected).abs() <= 2.0,
                        "{name} {rotation} {mode:?}"
                    );
                    if mode.quality == ptouch_core::PrintQuality::HighRes && rotation == 0.0 {
                        assert!((0..printed.width() / 2).any(|x| (0..height).any(|y| {
                            printed.get_pixel(2 * x, y) != printed.get_pixel(2 * x + 1, y)
                        })));
                    }
                }
            }
        }
    }

    #[test]
    fn selected_resolution_resamples_bitmap_inputs_without_changing_head_height() {
        let mut image = flipped_image_element(png_left_black(80, 40), true);
        if let LabelElement::Image { target_height, .. } = &mut image {
            *target_height = Some(40);
        }
        let elements = [image, LabelElement::Padding { pixels: 12 }];
        let mut renderer = TextRenderer::new();
        let render = |renderer: &mut TextRenderer, feed| {
            render_elements_at_resolution(
                &elements,
                76,
                "DejaVuSans",
                0,
                renderer,
                ptouch_core::PrintResolution {
                    head_dpi: 180,
                    feed_dpi: feed,
                },
            )
            .unwrap()
            .unwrap()
        };
        let normal = render(&mut renderer, 180);
        let high = render(&mut renderer, 360);
        assert_eq!(
            (high.width(), high.height()),
            (normal.width() * 2, normal.height())
        );
        assert!((0..high.width()).any(|x| (0..high.height()).any(|y| high.get_pixel(x, y))));
    }

    /// Encode a solid black image of the given size as PNG bytes.
    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([0, 0, 0, 255]));
        let mut buf = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(Cursor::new(&mut buf));
        image::ImageEncoder::write_image(
            encoder,
            img.as_raw(),
            w,
            h,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
        buf
    }

    /// Encode a PNG whose left half is black and right half is white.
    fn png_left_black(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, _| {
            if x < w / 2 {
                image::Rgba([0, 0, 0, 255])
            } else {
                image::Rgba([255, 255, 255, 255])
            }
        });
        let mut buf = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(Cursor::new(&mut buf));
        image::ImageEncoder::write_image(
            encoder,
            img.as_raw(),
            w,
            h,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
        buf
    }

    /// Build an image element with an explicit horizontal flip.
    fn flipped_image_element(image_data: Vec<u8>, flip_h: bool) -> LabelElement {
        match LabelElement::image_from_bytes(None, image_data) {
            LabelElement::Image {
                path,
                image_data,
                bitmap,
                rotation,
                target_height,
                flip_v,
                ..
            } => LabelElement::Image {
                path,
                image_data,
                bitmap,
                rotation,
                target_height,
                flip_h,
                flip_v,
            },
            other => other,
        }
    }

    #[test]
    fn v3_objects_round_trip_embedded_images_geometry_and_template_content() {
        let frame = TextBox {
            x_mm: 5.0,
            y_mm: 1.0,
            width_mm: 10.0,
            height_mm: 8.0,
            position: PositionMode::Relative,
            gap_mm: 3.0,
            vertical_align: VerticalAlign::Bottom,
        };
        let mut text = LabelObject::text("{{port}}", frame.clone());
        text.rotation = 45.0;
        text.flip_h = true;
        text.flip_v = true;
        let mut doc = LabelDocument {
            version: 3,
            tape_width_mm: 12,
            dpi: 360,
            font_name: String::new(),
            font_margin: 1,
            flip_h: true,
            flip_v: false,
            elements: vec![
                LabelElement::Object(text),
                LabelElement::Object(LabelObject::image_from_bytes(
                    Some("missing-original-image.png".into()),
                    png_left_black(12, 8),
                    frame.clone(),
                )),
                LabelElement::Object(LabelObject {
                    frame: frame.clone(),
                    content_box: None,
                    rotation: 90.0,
                    flip_h: false,
                    flip_v: false,
                    content: ObjectContent::CutMark,
                }),
                LabelElement::Object(LabelObject {
                    frame: frame.clone(),
                    content_box: None,
                    rotation: 0.0,
                    flip_h: false,
                    flip_v: false,
                    content: ObjectContent::Padding,
                }),
            ],
        };
        let encoded = doc.to_toml_string().unwrap();
        let decoded = LabelDocument::from_toml_str(&encoded).unwrap();
        assert_eq!(decoded.to_toml_string().unwrap(), encoded);
        assert_eq!(decoded.placeholders(), vec!["port"]);
        assert!(matches!(
            &decoded.elements[1].as_object().unwrap().content,
            ObjectContent::Image {
                bitmap: Some(_),
                ..
            }
        ));
        for object in &decoded.elements {
            assert_eq!(object.as_object().unwrap().frame, frame);
        }
        for invalid_version in [1, 2] {
            assert!(
                LabelDocument::from_toml_str(&encoded.replacen(
                    "version = 3",
                    &format!("version = {invalid_version}"),
                    1
                ))
                .is_err()
            );
        }
        doc.apply_values(&BTreeMap::from([("port".into(), String::new())]));
        assert_eq!(doc.elements[0].text_content(), Some(""));
        assert_eq!(doc.elements[0].as_object().unwrap().frame, frame);
        doc.version = 1;
        assert!(doc.to_toml_string().unwrap().contains("version = 3"));
    }

    #[test]
    fn custom_spacing_round_trips_templates_and_only_upgrades_when_needed() {
        let mut doc = LabelDocument {
            version: 3,
            tape_width_mm: 12,
            dpi: 180,
            font_name: String::new(),
            font_margin: 0,
            flip_h: false,
            flip_v: false,
            elements: vec![LabelElement::Object(LabelObject::text(
                "{{port}}\nLAN\nSWITCH",
                TextBox::default(),
            ))],
        };
        let old = doc.to_toml_string().unwrap();
        assert!(old.contains("version = 3"));
        assert!(!old.contains("line_spacing"));
        if let ObjectContent::Text { line_spacing, .. } =
            &mut doc.elements[0].as_object_mut().unwrap().content
        {
            *line_spacing = 0.9;
        }
        let encoded = doc.to_toml_string().unwrap();
        assert!(encoded.contains("version = 5"));
        assert!(encoded.contains("line_spacing = 0.9"));
        assert!(
            LabelDocument::from_toml_str(&encoded.replace("version = 5", "version = 4")).is_err()
        );
        let mut restored = LabelDocument::from_toml_str(&encoded).unwrap();
        assert_eq!(restored.to_toml_string().unwrap(), encoded);
        restored.apply_values(&BTreeMap::from([("port".into(), "PORT 1".into())]));
        assert_eq!(
            restored.elements[0].text_content(),
            Some("PORT 1\nLAN\nSWITCH")
        );
        assert!(
            matches!(restored.elements[0].as_object().unwrap().content, ObjectContent::Text { line_spacing, .. } if line_spacing == 0.9)
        );
        assert!(
            LabelDocument::from_toml_str(
                &encoded.replace("line_spacing = 0.9", "line_spacing = nan")
            )
            .is_err()
        );
    }

    #[test]
    fn independent_content_boxes_require_v4_and_preserve_v3_without_them() {
        let mut object = LabelObject::text("{{port}}", TextBox::default());
        let mut doc = LabelDocument {
            version: 3,
            tape_width_mm: 12,
            dpi: 180,
            font_name: String::new(),
            font_margin: 0,
            flip_h: false,
            flip_v: false,
            elements: vec![LabelElement::Object(object.clone())],
        };
        let old = doc.to_toml_string().unwrap();
        assert!(old.contains("version = 3"));
        assert!(!old.contains("content_box"));
        object.content_box = Some(ContentBox {
            width_mm: 12.0,
            height_mm: 3.5,
            offset_x_mm: -2.25,
            offset_y_mm: 1.0,
        });
        doc.elements[0] = LabelElement::Object(object.clone());
        let saved = doc.to_toml_string().unwrap();
        assert!(saved.contains("version = 4"));
        let decoded = LabelDocument::from_toml_str(&saved).unwrap();
        assert_eq!(
            decoded.elements[0].as_object().unwrap().content_box,
            object.content_box
        );
        assert!(
            LabelDocument::from_toml_str(&saved.replace("version = 4", "version = 3")).is_err()
        );
        assert!(
            LabelDocument::from_toml_str(&saved.replace("width_mm = 12.0", "width_mm = -12.0"))
                .is_err()
        );
        assert_eq!(
            LabelDocument::from_toml_str(&old)
                .unwrap()
                .to_toml_string()
                .unwrap(),
            old
        );
    }

    #[test]
    fn saving_legacy_layouts_keeps_their_version_and_element_representation() {
        for version in [1, 2] {
            let text = format!(
                "version = {version}\ntape_width_mm = 12\nfont_name = \"\"\nfont_margin = 0\n[[elements]]\ntype = \"padding\"\npixels = 17\n"
            );
            let doc = LabelDocument::from_toml_str(&text).unwrap();
            let saved = doc.to_toml_string().unwrap();
            assert!(saved.contains(&format!("version = {version}")));
            assert!(!saved.contains("object"));
            assert!(matches!(
                doc.elements[0],
                LabelElement::Padding { pixels: 17 }
            ));
        }
    }

    #[test]
    fn test_per_element_flip_h_mirrors_image() {
        let mut renderer = TextRenderer::new();
        let data = png_left_black(40, 40);

        let plain = render_elements(
            &[flipped_image_element(data.clone(), false)],
            64,
            "",
            0,
            &mut renderer,
        )
        .unwrap()
        .unwrap();
        let flipped = render_elements(
            &[flipped_image_element(data, true)],
            64,
            "",
            0,
            &mut renderer,
        )
        .unwrap()
        .unwrap();

        assert_eq!(plain.width(), flipped.width());
        assert_eq!(plain.height(), flipped.height());
        let w = plain.width();

        // The flipped element is the plain one mirrored left-right: pixel (x,y)
        // in the flip equals (w-1-x, y) in the plain. This holds whichever side
        // binarization decided was black.
        let mut differs = false;
        for y in 0..plain.height() {
            for x in 0..w {
                assert_eq!(
                    flipped.get_pixel(x, y),
                    plain.get_pixel(w - 1 - x, y),
                    "mirror mismatch at ({}, {})",
                    x,
                    y
                );
                if plain.get_pixel(x, y) != plain.get_pixel(w - 1 - x, y) {
                    differs = true;
                }
            }
        }
        // The source is asymmetric, so the flip must actually change something.
        assert!(
            differs,
            "expected an asymmetric image so the flip is visible"
        );
    }

    #[test]
    fn test_flip_fields_round_trip() {
        let mut doc = sample_document();
        doc.flip_h = true;
        if let LabelElement::Text { flip_v, .. } = &mut doc.elements[0] {
            *flip_v = true;
        }
        let parsed = LabelDocument::from_toml_str(&doc.to_toml_string().unwrap()).unwrap();
        assert!(parsed.flip_h);
        assert!(!parsed.flip_v);
        match &parsed.elements[0] {
            LabelElement::Text { flip_h, flip_v, .. } => {
                assert!(!flip_h);
                assert!(flip_v);
            }
            _ => panic!("expected text element"),
        }
    }

    #[test]
    fn test_old_layout_without_flip_defaults_false() {
        // A v1 layout written before the mirror feature has no flip fields.
        let text = "version = 1\ntape_width_mm = 12\nfont_name = \"x\"\n\
                    font_margin = 0\n\n[[elements]]\ntype = \"text\"\n\
                    content = \"hi\"\nalign = \"left\"\nrotation = 0.0\n";
        let doc = LabelDocument::from_toml_str(text).unwrap();
        assert!(!doc.flip_h);
        assert!(!doc.flip_v);
        match &doc.elements[0] {
            LabelElement::Text { flip_h, flip_v, .. } => {
                assert!(!flip_h);
                assert!(!flip_v);
            }
            _ => panic!("expected text element"),
        }
    }

    #[test]
    fn test_empty_elements_returns_none() {
        let mut renderer = TextRenderer::new();
        let result = render_elements(&[], 64, "", 0, &mut renderer).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_padding_and_cutmark_dimensions() {
        let mut renderer = TextRenderer::new();
        let elements = vec![LabelElement::Padding { pixels: 10 }, LabelElement::CutMark];
        let bmp = render_elements(&elements, 64, "", 0, &mut renderer)
            .unwrap()
            .unwrap();
        assert_eq!(bmp.height(), 64);
        // Padding (10) followed by the 9-pixel cut mark.
        assert_eq!(bmp.width(), 19);
    }

    #[test]
    fn test_empty_text_is_skipped() {
        let mut renderer = TextRenderer::new();
        let elements = vec![LabelElement::Text {
            content: String::new(),
            font_size: Some(24.0),
            text_box: None,
            align: TextAlign::Left,
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
        }];
        let result = render_elements(&elements, 64, "", 0, &mut renderer).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_image_element_fits_tape_height() {
        let mut renderer = TextRenderer::new();
        let element = LabelElement::image_from_bytes(None, png_bytes(40, 40));
        let bmp = render_elements(&[element], 64, "", 0, &mut renderer)
            .unwrap()
            .unwrap();
        assert_eq!(bmp.height(), 64);
        assert!(bmp.width() > 0);
    }

    #[test]
    fn test_rotated_image_fits_tape_height() {
        let mut renderer = TextRenderer::new();
        let element = match LabelElement::image_from_bytes(None, png_bytes(40, 20)) {
            LabelElement::Image {
                path,
                image_data,
                bitmap,
                target_height,
                ..
            } => LabelElement::Image {
                path,
                image_data,
                bitmap,
                rotation: 90.0,
                target_height,
                flip_h: false,
                flip_v: false,
            },
            other => other,
        };
        let bmp = render_elements(&[element], 64, "", 0, &mut renderer)
            .unwrap()
            .unwrap();
        assert_eq!(bmp.height(), 64);
    }

    /// Build a small document containing one of each element kind.
    fn sample_document() -> LabelDocument {
        LabelDocument {
            version: DOCUMENT_VERSION,
            tape_width_mm: 12,
            dpi: 180,
            font_name: "DejaVuSans".into(),
            font_margin: 2,
            flip_h: false,
            flip_v: false,
            elements: vec![
                LabelElement::Text {
                    content: "Hi".into(),
                    font_size: Some(24.0),
                    text_box: None,
                    align: TextAlign::Center,
                    rotation: 0.0,
                    flip_h: false,
                    flip_v: false,
                },
                LabelElement::image_from_bytes(Some("logo.png".into()), png_bytes(8, 8)),
                LabelElement::CutMark,
                LabelElement::Padding { pixels: 20 },
            ],
        }
    }

    #[test]
    fn test_document_round_trip_all_kinds() {
        let doc = sample_document();
        let text = doc.to_toml_string().unwrap();
        let parsed = LabelDocument::from_toml_str(&text).unwrap();

        assert_eq!(parsed.elements.len(), 4);
        assert_eq!(parsed.tape_width_mm, 12);
        assert_eq!(parsed.font_name, "DejaVuSans");

        // Embedded image bytes survive the round-trip exactly.
        match (&doc.elements[1], &parsed.elements[1]) {
            (
                LabelElement::Image { image_data: a, .. },
                LabelElement::Image {
                    image_data: b,
                    bitmap,
                    ..
                },
            ) => {
                assert_eq!(a, b);
                // The render cache is rebuilt on load.
                assert!(bitmap.is_some());
            }
            _ => panic!("expected image elements"),
        }
    }

    #[test]
    fn test_cutmark_serializes_as_tagged_table() {
        let doc = sample_document();
        let text = doc.to_toml_string().unwrap();
        assert!(text.contains("cut_mark"));
    }

    #[test]
    fn test_newer_version_is_rejected() {
        let doc = LabelDocument {
            version: DOCUMENT_VERSION + 1,
            tape_width_mm: 12,
            dpi: 180,
            font_name: "x".into(),
            font_margin: 0,
            flip_h: false,
            flip_v: false,
            elements: vec![LabelElement::CutMark],
        };
        let text = doc.to_toml_string().unwrap();
        assert!(LabelDocument::from_toml_str(&text).is_err());
    }

    #[test]
    fn test_corrupt_image_data_is_rejected() {
        let doc = LabelDocument {
            version: DOCUMENT_VERSION,
            tape_width_mm: 12,
            dpi: 180,
            font_name: "x".into(),
            font_margin: 0,
            flip_h: false,
            flip_v: false,
            elements: vec![LabelElement::Image {
                path: None,
                image_data: b"not a real image".to_vec(),
                bitmap: None,
                rotation: 0.0,
                target_height: None,
                flip_h: false,
                flip_v: false,
            }],
        };
        let text = doc.to_toml_string().unwrap();
        assert!(LabelDocument::from_toml_str(&text).is_err());
    }

    #[test]
    fn test_invalid_base64_is_rejected() {
        let text = "version = 1\ntape_width_mm = 12\nfont_name = \"x\"\n\
                    font_margin = 0\n\n[[elements]]\ntype = \"image\"\n\
                    image_data = \"not valid base64 !!!\"\nrotation = 0.0\n";
        assert!(LabelDocument::from_toml_str(text).is_err());
    }

    fn text_doc(content: &str) -> LabelDocument {
        LabelDocument {
            version: DOCUMENT_VERSION,
            tape_width_mm: 12,
            dpi: 180,
            font_name: "x".into(),
            font_margin: 0,
            flip_h: false,
            flip_v: false,
            elements: vec![LabelElement::Text {
                content: content.into(),
                font_size: Some(24.0),
                text_box: None,
                align: TextAlign::Left,
                rotation: 0.0,
                flip_h: false,
                flip_v: false,
            }],
        }
    }

    fn text_of(doc: &LabelDocument) -> &str {
        match &doc.elements[0] {
            LabelElement::Text { content, .. } => content,
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn test_placeholders_unique_sorted() {
        let doc = text_doc("{{name}} {{id}}\n{{name}}");
        assert_eq!(
            doc.placeholders(),
            vec!["id".to_string(), "name".to_string()]
        );
    }

    #[test]
    fn test_placeholders_ignore_invalid() {
        // Spaces make it not a placeholder; it stays literal and is not listed.
        let doc = text_doc("{{ not a var }} {{ok}}");
        assert_eq!(doc.placeholders(), vec!["ok".to_string()]);
    }

    #[test]
    fn test_apply_values_substitutes() {
        let mut doc = text_doc("Hi {{name}} ({{id}})");
        let mut values = BTreeMap::new();
        values.insert("name".to_string(), "Alice".to_string());
        values.insert("id".to_string(), "A001".to_string());
        doc.apply_values(&values);
        assert_eq!(text_of(&doc), "Hi Alice (A001)");
    }

    #[test]
    fn test_apply_values_blanks_unresolved() {
        let mut doc = text_doc("Hi {{name}}!");
        doc.apply_values(&BTreeMap::new());
        assert_eq!(text_of(&doc), "Hi !");
    }

    #[test]
    fn test_apply_values_trims_and_keeps_literals() {
        let mut doc = text_doc("{{ name }} {{bad var}}");
        let mut values = BTreeMap::new();
        values.insert("name".to_string(), "Bob".to_string());
        doc.apply_values(&values);
        // Whitespace inside braces is tolerated; the invalid one stays literal.
        assert_eq!(text_of(&doc), "Bob {{bad var}}");
    }

    #[test]
    fn test_unterminated_placeholder_left_intact() {
        let mut doc = text_doc("price {{name");
        doc.apply_values(&BTreeMap::new());
        assert_eq!(text_of(&doc), "price {{name");
    }

    #[test]
    fn test_image_from_bytes_decodes_cache() {
        let element = LabelElement::image_from_bytes(Some("logo.png".into()), png_bytes(8, 8));
        match element {
            LabelElement::Image { bitmap, path, .. } => {
                assert!(bitmap.is_some());
                assert_eq!(path.unwrap().file_name().unwrap(), "logo.png");
            }
            _ => panic!("expected image element"),
        }
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use ptouch_core::PrintResolution;

    fn empty_text() -> LabelElement {
        LabelElement::Text {
            text_box: None,
            content: String::new(),
            font_size: None,
            align: TextAlign::Left,
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
        }
    }

    #[test]
    fn spans_cover_print_columns_at_every_resolution_including_odd_draft_boundaries() {
        let elements = [
            LabelElement::Padding { pixels: 7 },
            LabelElement::CutMark,
            empty_text(),
            LabelElement::Padding { pixels: 5 },
        ];
        let mut renderer = TextRenderer::new();
        let normal = render_elements(&elements, 70, "sans-serif", 0, &mut renderer)
            .unwrap()
            .unwrap();
        for (head, feed, expected) in [
            (180, 180, vec![Some(0..7), Some(7..16), None, Some(16..21)]),
            (
                180,
                360,
                vec![Some(0..14), Some(14..32), None, Some(32..42)],
            ),
            (360, 360, vec![Some(0..7), Some(7..16), None, Some(16..21)]),
            (
                360,
                720,
                vec![Some(0..14), Some(14..32), None, Some(32..42)],
            ),
            (360, 180, vec![Some(0..4), Some(4..8), None, Some(8..11)]),
        ] {
            let resolution = PrintResolution {
                head_dpi: head,
                feed_dpi: feed,
            };
            let rendered = render_elements_with_layout_at_resolution(
                &elements,
                70,
                "sans-serif",
                0,
                &mut renderer,
                resolution,
            )
            .unwrap()
            .unwrap();
            assert_eq!(rendered.element_spans, expected);
            let mut covered = vec![0; rendered.bitmap.width() as usize];
            for span in rendered.element_spans.iter().flatten() {
                for x in span.clone() {
                    covered[x as usize] += 1;
                }
            }
            assert!(covered.iter().all(|&count| count == 1));
            // Padding and cutmarks have an exact known raster at each density.
            for x in 0..rendered.bitmap.width() {
                let source = if feed == head * 2 {
                    x / 2
                } else if feed * 2 == head {
                    x * 2
                } else {
                    x
                };
                for y in 0..70 {
                    assert_eq!(rendered.bitmap.get_pixel(x, y), normal.get_pixel(source, y));
                }
            }
            let width = rendered.bitmap.width();
            let mirrored = rendered.mirrored(true, true);
            for (actual, expected) in mirrored.element_spans.iter().zip(expected.iter()) {
                assert_eq!(
                    *actual,
                    expected
                        .as_ref()
                        .map(|span| width - span.end..width - span.start)
                );
            }
            for x in 0..width {
                let source = if feed == head * 2 {
                    (width - 1 - x) / 2
                } else if feed * 2 == head {
                    (width - 1 - x) * 2
                } else {
                    width - 1 - x
                };
                for y in 0..70 {
                    assert_eq!(
                        mirrored.bitmap.get_pixel(x, y),
                        normal.get_pixel(source, 69 - y)
                    );
                }
            }
        }
    }

    #[test]
    fn unsampled_draft_elements_have_empty_spans_without_moving_other_elements() {
        let rendered = render_elements_with_layout_at_resolution(
            &[
                LabelElement::Padding { pixels: 1 },
                LabelElement::Padding { pixels: 1 },
                LabelElement::Padding { pixels: 1 },
            ],
            70,
            "sans-serif",
            0,
            &mut TextRenderer::new(),
            PrintResolution {
                head_dpi: 360,
                feed_dpi: 180,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(rendered.bitmap.width(), 2);
        assert_eq!(
            rendered.element_spans,
            vec![Some(0..1), Some(1..1), Some(1..2)]
        );
    }

    #[test]
    fn rotated_and_mirrored_image_bounds_follow_the_composed_segments() {
        let mut image = LabelBitmap::new(31, 17);
        image.set_pixel(0, 0, true);
        let elements = [
            LabelElement::Padding { pixels: 3 },
            LabelElement::Image {
                path: None,
                image_data: Vec::new(),
                bitmap: Some(image),
                rotation: 37.0,
                target_height: Some(24),
                flip_h: true,
                flip_v: true,
            },
            empty_text(),
            LabelElement::Padding { pixels: 8 },
        ];
        for feed in [180, 360, 720] {
            let resolution = PrintResolution {
                head_dpi: 360,
                feed_dpi: feed,
            };
            let rendered = render_elements_with_layout_at_resolution(
                &elements,
                70,
                "sans-serif",
                0,
                &mut TextRenderer::new(),
                resolution,
            )
            .unwrap()
            .unwrap();
            let spans = &rendered.element_spans;
            assert_eq!(spans[0].as_ref().unwrap().start, 0);
            assert_eq!(
                spans[0].as_ref().unwrap().end,
                spans[1].as_ref().unwrap().start
            );
            assert_eq!(
                spans[1].as_ref().unwrap().end,
                spans[3].as_ref().unwrap().start
            );
            assert_eq!(spans[3].as_ref().unwrap().end, rendered.bitmap.width());
            assert!(spans[2].is_none());
        }
    }
}
