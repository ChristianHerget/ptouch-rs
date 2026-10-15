// SPDX-License-Identifier: MIT
//! A print bitmap and its display representations, at one resolution.

use ptouch_core::PrintResolution;
use ptouch_render::bitmap::LabelBitmap;

pub struct PreviewTile {
    /// Origin in the unmodified print bitmap.
    pub origin: [u32; 2],
    pub texture: egui::TextureHandle,
}

pub struct RenderedPreview {
    pub bitmap: LabelBitmap,
    pub resolution: PrintResolution,
    pub tiles: Vec<PreviewTile>,
    pub element_spans: Vec<Option<std::ops::Range<u32>>>,
}

impl RenderedPreview {
    pub fn new(ctx: &egui::Context, bitmap: LabelBitmap, resolution: PrintResolution) -> Self {
        let max_side = ctx.input(|i| i.max_texture_side).max(1);
        let mut tiles = Vec::new();
        // Upload tiles without shrinking the label to the GPU texture limit.
        // Pixel detail remains available even for very long labels.
        for origin in tile_origins(&bitmap, max_side) {
            let image = tile_image(&bitmap, origin, max_side);
            let texture = ctx.load_texture(
                format!("label_preview_{}_{}", origin[0], origin[1]),
                image,
                egui::TextureOptions::NEAREST,
            );
            tiles.push(PreviewTile { origin, texture });
        }
        Self {
            bitmap,
            resolution,
            tiles,
            element_spans: Vec::new(),
        }
    }

    #[cfg(test)]
    pub fn from_rendered(
        ctx: &egui::Context,
        rendered: ptouch_render::document::RenderedElements,
        resolution: PrintResolution,
    ) -> Self {
        let mut preview = Self::new(ctx, rendered.bitmap, resolution);
        preview.element_spans = rendered.element_spans;
        preview
    }

    pub fn element_length_mm(&self, index: usize) -> Option<f64> {
        self.element_spans
            .get(index)?
            .as_ref()
            .filter(|span| !span.is_empty())
            .map(|span| self.resolution.length_mm((span.end - span.start) as usize))
    }

    /// A drop position is an insertion boundary in the original element list.
    pub fn drop_position(&self, column: f32, mirrored: bool) -> usize {
        if column <= 0.0 {
            return if mirrored {
                self.element_spans.len()
            } else {
                0
            };
        }
        if column >= self.bitmap.width() as f32 {
            return if mirrored {
                0
            } else {
                self.element_spans.len()
            };
        }
        let mut last = None;
        for visual_index in 0..self.element_spans.len() {
            let index = if mirrored {
                self.element_spans.len() - 1 - visual_index
            } else {
                visual_index
            };
            let Some(span) = self.element_spans[index]
                .as_ref()
                .filter(|span| !span.is_empty())
            else {
                continue;
            };
            if column < (span.start as f32 + span.end as f32) * 0.5 {
                return if mirrored { index + 1 } else { index };
            }
            last = Some(index);
        }
        last.map_or(0, |index| if mirrored { index } else { index + 1 })
    }

    pub fn insertion_column(&self, before: usize, mirrored: bool) -> f32 {
        let prefix: u32 = self
            .element_spans
            .iter()
            .take(before)
            .flatten()
            .map(|span| span.end - span.start)
            .sum();
        if mirrored {
            (self.bitmap.width() - prefix) as f32
        } else {
            prefix as f32
        }
    }

    /// Display dimensions in head-DPI units, accounting for rectangular dots.
    pub fn size(&self) -> egui::Vec2 {
        egui::vec2(
            self.bitmap.width() as f32 * self.feed_pixel_width(),
            self.bitmap.height() as f32,
        )
    }

    pub fn feed_pixel_width(&self) -> f32 {
        f32::from(self.resolution.head_dpi) / f32::from(self.resolution.feed_dpi)
    }

    pub fn tile_rect(&self, tile: &PreviewTile, label_rect: egui::Rect, zoom: f32) -> egui::Rect {
        let pixel_scale = egui::vec2(self.feed_pixel_width() * zoom, zoom);
        let origin = egui::vec2(tile.origin[0] as f32, tile.origin[1] as f32);
        let end = origin + tile.texture.size_vec2();
        egui::Rect::from_min_max(
            label_rect.min + origin * pixel_scale,
            label_rect.min + end * pixel_scale,
        )
    }

    /// Square pixels preserve proportions in image viewers. Replication keeps
    /// every printer dot; interpolation would introduce unprinted shades.
    pub fn square_pixel_bitmap(&self) -> LabelBitmap {
        let dpi = self.resolution.head_dpi.max(self.resolution.feed_dpi);
        let x_scale = u32::from(dpi / self.resolution.feed_dpi);
        let y_scale = u32::from(dpi / self.resolution.head_dpi);
        if x_scale == 1 && y_scale == 1 {
            return self.bitmap.clone();
        }
        let mut out = LabelBitmap::new(
            self.bitmap.width() * x_scale,
            self.bitmap.height() * y_scale,
        );
        for y in 0..out.height() {
            for x in 0..out.width() {
                out.set_pixel(x, y, self.bitmap.get_pixel(x / x_scale, y / y_scale));
            }
        }
        out
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum ZoomMode {
    #[default]
    Fit,
    ActualSize,
    Pixels,
    Custom(f32),
}

#[derive(Default)]
pub struct PreviewView {
    pub zoom: ZoomMode,
    pub pan: egui::Vec2,
}

#[derive(Clone, Copy)]
pub struct DisplayScale {
    pub native: f32,
    pub pixels_per_point: f32,
}

impl DisplayScale {
    pub fn from_context(ctx: &egui::Context) -> Self {
        Self {
            native: ctx.native_pixels_per_point().unwrap_or(1.0),
            pixels_per_point: ctx.pixels_per_point(),
        }
    }

    pub fn actual_head_scale(self, resolution: PrintResolution) -> f32 {
        96.0 * self.native / (f32::from(resolution.head_dpi) * self.pixels_per_point)
    }
}

/// One transform for print tiles, full tape, hit testing, and dimensions.
pub struct PreviewGeometry {
    pub tape: egui::Rect,
    pub print: egui::Rect,
    pub content: egui::Rect,
    pub head_scale: f32,
    pub physical_zoom: f32,
    column_width: f32,
}

impl PreviewView {
    pub fn select(&mut self, zoom: ZoomMode) {
        self.zoom = zoom;
        self.pan = egui::Vec2::ZERO;
    }

    pub fn geometry(
        &mut self,
        preview: &RenderedPreview,
        tape_width_mm: u8,
        content: egui::Rect,
        display: DisplayScale,
    ) -> PreviewGeometry {
        let size = egui::vec2(
            preview.size().x,
            f32::from(tape_width_mm) * f32::from(preview.resolution.head_dpi) / 25.4,
        );
        let actual = display.actual_head_scale(preview.resolution);
        let head_scale = match self.zoom {
            ZoomMode::Fit => (content.width() / size.x).min(content.height() / size.y),
            ZoomMode::ActualSize => actual,
            ZoomMode::Pixels => {
                f32::from(preview.resolution.head_dpi.max(preview.resolution.feed_dpi))
                    / (f32::from(preview.resolution.head_dpi) * display.pixels_per_point)
            }
            ZoomMode::Custom(factor) => actual * factor,
        }
        .max(f32::EPSILON);
        let display_size = size * head_scale;
        let limit = ((display_size - content.size()) * 0.5).max(egui::Vec2::ZERO);
        if self.zoom == ZoomMode::Fit {
            self.pan = egui::Vec2::ZERO;
        }
        self.pan = self.pan.clamp(-limit, limit);
        let mut tape = egui::Rect::from_center_size(content.center() + self.pan, display_size);
        let mut print = egui::Rect::from_center_size(tape.center(), preview.size() * head_scale);
        if self.zoom == ZoomMode::Pixels {
            let origin = print.min;
            let snapped =
                (origin.to_vec2() * display.pixels_per_point).round() / display.pixels_per_point;
            let delta = snapped - origin.to_vec2();
            tape = tape.translate(delta);
            print = print.translate(delta);
        }
        PreviewGeometry {
            tape,
            print,
            content,
            head_scale,
            physical_zoom: head_scale / actual,
            column_width: preview.feed_pixel_width() * head_scale,
        }
    }
}

impl PreviewGeometry {
    pub fn column_at(&self, x: f32) -> f32 {
        (x - self.print.left()) / self.column_width
    }

    pub fn column_x(&self, column: f32) -> f32 {
        self.print.left() + column * self.column_width
    }

    pub fn element_rect(&self, preview: &RenderedPreview, index: usize) -> Option<egui::Rect> {
        let span = preview
            .element_spans
            .get(index)?
            .as_ref()
            .filter(|s| !s.is_empty())?;
        Some(egui::Rect::from_min_max(
            egui::pos2(self.column_x(span.start as f32), self.tape.top()),
            egui::pos2(self.column_x(span.end as f32), self.tape.bottom()),
        ))
    }

    pub fn hit_test(&self, preview: &RenderedPreview, position: egui::Pos2) -> Option<usize> {
        if !self.content.contains(position) || !self.tape.contains(position) {
            return None;
        }
        let column = self.column_at(position.x);
        preview.element_spans.iter().position(|span| {
            span.as_ref()
                .is_some_and(|span| column >= span.start as f32 && column < span.end as f32)
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ElementDrag {
    pub index: usize,
    pub revision: u64,
}

pub fn paint_drag_ghost(ctx: &egui::Context, name: String) {
    let Some(pointer) = ctx.pointer_interact_pos() else {
        return;
    };
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("element_drag_ghost"),
    ));
    let text = painter.layout_no_wrap(name, egui::FontId::proportional(13.0), egui::Color32::WHITE);
    let rect = egui::Rect::from_min_size(
        pointer + egui::vec2(14.0, 14.0),
        text.size() + egui::vec2(12.0, 8.0),
    );
    painter.rect_filled(rect, 3.0, egui::Color32::from_rgb(35, 105, 200));
    painter.galley(rect.min + egui::vec2(6.0, 4.0), text, egui::Color32::WHITE);
}

pub fn format_mm(mm: f64) -> String {
    let rounded = (mm * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{rounded:.0} mm")
    } else {
        format!("{rounded:.1} mm")
    }
}

pub fn format_zoom(factor: f32) -> String {
    let percent = factor * 100.0;
    if percent < 0.01 {
        "<0.01%".into()
    } else if percent < 1.0 {
        format!("≈{percent:.2}%")
    } else if percent < 10.0 {
        format!("≈{percent:.1}%")
    } else {
        format!("≈{percent:.0}%")
    }
}

/// Movement of the content (opposite to the pointer's direction at an edge).
pub fn edge_pan(pointer: egui::Pos2, viewport: egui::Rect, dt: f32) -> egui::Vec2 {
    let axis = |value: f32, min: f32, max: f32| {
        if value < min + 24.0 {
            1.0
        } else if value > max - 24.0 {
            -1.0
        } else {
            0.0
        }
    };
    egui::vec2(
        axis(pointer.x, viewport.left(), viewport.right()),
        axis(pointer.y, viewport.top(), viewport.bottom()),
    ) * (350.0 * dt.min(0.05))
}

fn tile_origins(bitmap: &LabelBitmap, max_side: usize) -> impl Iterator<Item = [u32; 2]> {
    (0..bitmap.height())
        .step_by(max_side)
        .flat_map(move |y| (0..bitmap.width()).step_by(max_side).map(move |x| [x, y]))
}

fn tile_image(bitmap: &LabelBitmap, origin: [u32; 2], max_side: usize) -> egui::ColorImage {
    let width = (bitmap.width() - origin[0]).min(max_side as u32);
    let height = (bitmap.height() - origin[1]).min(max_side as u32);
    let mut pixels = Vec::with_capacity(width as usize * height as usize);
    for y in 0..height {
        for x in 0..width {
            pixels.push(if bitmap.get_pixel(origin[0] + x, origin[1] + y) {
                egui::Color32::BLACK
            } else {
                egui::Color32::WHITE
            });
        }
    }
    egui::ColorImage::new([width as usize, height as usize], pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterned_bitmap(width: u32, height: u32) -> LabelBitmap {
        let mut bitmap = LabelBitmap::new(width, height);
        for y in 0..height {
            for x in 0..width {
                bitmap.set_pixel(x, y, (x + y * 3) % 5 < 2);
            }
        }
        bitmap
    }

    #[test]
    fn square_pixel_exports_preserve_dots_and_display_proportions() {
        for (head, feed, x_scale, y_scale) in [
            (180, 180, 1, 1),
            (180, 360, 1, 2),
            (360, 360, 1, 1),
            (360, 720, 1, 2),
            (360, 180, 2, 1),
        ] {
            let preview = RenderedPreview::new(
                &egui::Context::default(),
                patterned_bitmap(13, 7),
                PrintResolution {
                    head_dpi: head,
                    feed_dpi: feed,
                },
            );
            let export = preview.square_pixel_bitmap();
            assert_eq!(export.width(), 13 * x_scale);
            assert_eq!(export.height(), 7 * y_scale);
            assert_eq!(
                export.width() as f32 / export.height() as f32,
                preview.size().x / preview.size().y,
            );
            // Exercise the exported pixels through lossless PNG encoding.
            let mut png = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(export.to_rgba_image())
                .write_to(&mut png, image::ImageFormat::Png)
                .unwrap();
            let decoded = image::load_from_memory(png.get_ref()).unwrap().to_luma8();
            for y in 0..export.height() {
                for x in 0..export.width() {
                    assert_eq!(
                        decoded.get_pixel(x, y).0[0] == 0,
                        preview.bitmap.get_pixel(x / x_scale, y / y_scale)
                    );
                }
            }
        }
    }

    #[test]
    fn small_texture_tiles_cover_every_pixel_without_resampling() {
        let bitmap = patterned_bitmap(13, 7);
        let mut covered = vec![0; (bitmap.width() * bitmap.height()) as usize];
        for origin in tile_origins(&bitmap, 4) {
            let image = tile_image(&bitmap, origin, 4);
            assert!(image.size[0] <= 4 && image.size[1] <= 4);
            for y in 0..image.size[1] {
                for x in 0..image.size[0] {
                    let bx = origin[0] + x as u32;
                    let by = origin[1] + y as u32;
                    covered[(by * bitmap.width() + bx) as usize] += 1;
                    assert_eq!(
                        image[(x, y)] == egui::Color32::BLACK,
                        bitmap.get_pixel(bx, by)
                    );
                }
            }
        }
        assert!(covered.into_iter().all(|count| count == 1));
    }

    #[test]
    fn uploaded_tiles_share_canvas_edges_at_every_quality_and_zoom() {
        let ctx = egui::Context::default();
        ctx.begin_pass(egui::RawInput::default());
        // Initialize egui's font atlas at its supported size first, then
        // impose a smaller upload limit for the label textures under test.
        ctx.input_mut(|input| input.max_texture_side = 64);
        for feed in [180, 360, 720] {
            let preview = RenderedPreview::new(
                &ctx,
                patterned_bitmap(130, 129),
                PrintResolution {
                    head_dpi: 360,
                    feed_dpi: feed,
                },
            );
            assert_eq!(preview.tiles.len(), 9);
            assert!(
                preview.tiles.iter().all(|tile| tile
                    .texture
                    .size()
                    .into_iter()
                    .all(|side| side <= 64))
            );
            for zoom in [0.03, 1.0, 7.5] {
                let label = egui::Rect::from_min_size(egui::pos2(17.0, 9.0), preview.size() * zoom);
                for row in preview.tiles.chunks(3) {
                    let rects: Vec<_> = row
                        .iter()
                        .map(|tile| preview.tile_rect(tile, label, zoom))
                        .collect();
                    assert_eq!(rects[0].left(), label.left());
                    assert_eq!(rects[0].right(), rects[1].left());
                    assert_eq!(rects[1].right(), rects[2].left());
                    assert_eq!(rects[2].right(), label.right());
                }
                for column in 0..3 {
                    let rects: Vec<_> = preview
                        .tiles
                        .iter()
                        .skip(column)
                        .step_by(3)
                        .map(|tile| preview.tile_rect(tile, label, zoom))
                        .collect();
                    assert_eq!(rects[0].top(), label.top());
                    assert_eq!(rects[0].bottom(), rects[1].top());
                    assert_eq!(rects[1].bottom(), rects[2].top());
                    assert_eq!(rects[2].bottom(), label.bottom());
                }
            }
        }
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
    }
}

#[cfg(test)]
mod geometry_tests {
    use super::*;

    fn preview(head: u16, feed: u16) -> RenderedPreview {
        let mut preview = RenderedPreview::new(
            &egui::Context::default(),
            LabelBitmap::new(u32::from(feed), 70),
            PrintResolution {
                head_dpi: head,
                feed_dpi: feed,
            },
        );
        preview.element_spans = vec![Some(0..10), Some(10..30), Some(30..u32::from(feed))];
        preview
    }

    fn close(a: f32, b: f32) {
        assert!((a - b).abs() < 0.002, "{a} != {b}");
    }

    #[test]
    fn actual_size_accounts_for_os_and_ui_scaling_once() {
        let content = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(500.0, 300.0));
        for (head, feed) in [(180, 180), (180, 360), (360, 180), (360, 360), (360, 720)] {
            let preview = preview(head, feed);
            for native in [1.0, 1.5, 2.0] {
                for ui_scale in [1.0, 1.25, 2.0] {
                    let display = DisplayScale {
                        native,
                        pixels_per_point: native * ui_scale,
                    };
                    let mut view = PreviewView::default();
                    view.select(ZoomMode::ActualSize);
                    let geometry = view.geometry(&preview, 12, content, display);
                    close(
                        geometry.tape.width() * display.pixels_per_point,
                        96.0 * native,
                    );
                    close(
                        geometry.tape.height() * display.pixels_per_point,
                        12.0 / 25.4 * 96.0 * native,
                    );
                    close(geometry.physical_zoom, 1.0);
                    close(geometry.print.center().y, geometry.tape.center().y);
                    assert!(geometry.print.height() < geometry.tape.height());
                }
            }
        }
    }

    #[test]
    fn pixels_are_integer_physical_pixels_at_fractional_monitor_scales() {
        let content = egui::Rect::from_min_size(egui::pos2(13.2, 17.7), egui::vec2(501.5, 301.0));
        for (head, feed) in [(180, 180), (180, 360), (360, 180), (360, 360), (360, 720)] {
            let preview = preview(head, feed);
            for pixels_per_point in [1.0, 1.5, 2.0, 2.5] {
                let display = DisplayScale {
                    native: 2.0,
                    pixels_per_point,
                };
                let mut view = PreviewView::default();
                view.select(ZoomMode::Pixels);
                let geometry = view.geometry(&preview, 12, content, display);
                let pixel_x = geometry.column_width * pixels_per_point;
                let pixel_y = geometry.head_scale * pixels_per_point;
                assert!(pixel_x >= 1.0 && pixel_y >= 1.0);
                close(pixel_x, pixel_x.round());
                close(pixel_y, pixel_y.round());
                close(
                    geometry.print.left() * pixels_per_point,
                    (geometry.print.left() * pixels_per_point).round(),
                );
                close(
                    geometry.print.top() * pixels_per_point,
                    (geometry.print.top() * pixels_per_point).round(),
                );
                close(
                    geometry.print.width() / geometry.print.height(),
                    preview.size().x / preview.size().y,
                );
            }
        }
    }

    #[test]
    fn fit_uses_full_tape_and_panning_keeps_fitting_axes_centred() {
        let preview = preview(180, 180);
        let content = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 80.0));
        let display = DisplayScale {
            native: 1.0,
            pixels_per_point: 1.0,
        };
        let mut view = PreviewView {
            pan: egui::vec2(10000.0, -10000.0),
            ..Default::default()
        };
        let geometry = view.geometry(&preview, 12, content, display);
        close(geometry.tape.width(), 100.0);
        assert!(geometry.tape.height() <= content.height());
        assert_eq!(view.pan, egui::Vec2::ZERO);
        view.zoom = ZoomMode::Custom(10.0);
        view.pan = egui::vec2(10000.0, -10000.0);
        let geometry = view.geometry(&preview, 12, content, display);
        close(geometry.tape.left(), content.left());
        close(geometry.tape.bottom(), content.bottom());
        view.select(ZoomMode::ActualSize);
        view.pan = egui::vec2(30.0, 30.0);
        let geometry = view.geometry(&preview, 12, content, display);
        assert_eq!(view.pan, egui::Vec2::ZERO);
        close(geometry.tape.center().x, content.center().x);
    }

    #[test]
    fn hit_testing_and_drop_positions_follow_mirrored_visual_order() {
        let mut preview = preview(180, 180);
        preview.bitmap = LabelBitmap::new(60, 70);
        preview.element_spans = vec![Some(0..10), Some(10..30), Some(30..60)];
        for (column, expected) in [
            (0.0, 0),
            (4.0, 0),
            (6.0, 1),
            (19.0, 1),
            (21.0, 2),
            (44.0, 2),
            (46.0, 3),
            (60.0, 3),
        ] {
            assert_eq!(preview.drop_position(column, false), expected);
        }
        preview.element_spans = vec![Some(50..60), Some(30..50), Some(0..30)];
        for (column, expected) in [
            (0.0, 3),
            (14.0, 3),
            (16.0, 2),
            (39.0, 2),
            (41.0, 1),
            (54.0, 1),
            (56.0, 0),
            (60.0, 0),
        ] {
            assert_eq!(preview.drop_position(column, true), expected);
        }
        for (before, column) in [60.0, 50.0, 30.0, 0.0].into_iter().enumerate() {
            close(preview.insertion_column(before, true), column);
        }
        let content = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
        let geometry = PreviewView::default().geometry(
            &preview,
            12,
            content,
            DisplayScale {
                native: 1.0,
                pixels_per_point: 1.0,
            },
        );
        for index in 0..3 {
            let centre = geometry.element_rect(&preview, index).unwrap().center();
            assert_eq!(geometry.hit_test(&preview, centre), Some(index));
            // Tape margins and blank padding belong to the same selectable strip.
            assert_eq!(
                geometry.hit_test(&preview, egui::pos2(centre.x, geometry.tape.top() + 1.0)),
                Some(index)
            );
        }
        assert_eq!(geometry.hit_test(&preview, egui::pos2(-10.0, -10.0)), None);
    }

    #[test]
    fn lengths_and_formatting_handle_empty_and_skipped_elements() {
        let mut preview = preview(180, 180);
        preview.element_spans = vec![None, Some(0..0), Some(0..180)];
        assert_eq!(preview.element_length_mm(0), None);
        assert_eq!(preview.element_length_mm(1), None);
        assert_eq!(preview.element_length_mm(2), Some(25.4));
        assert_eq!(preview.element_length_mm(3), None);
        assert_eq!(format_mm(12.0), "12 mm");
        assert_eq!(format_mm(12.04), "12 mm");
        assert_eq!(format_mm(12.06), "12.1 mm");
        assert_eq!(format_zoom(1.0), "≈100%");
        assert_eq!(format_zoom(0.005), "≈0.50%");
        assert_eq!(format_zoom(0.000001), "<0.01%");
    }
}
