// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Central canvas panel for label preview display.

use ptouch_render::document::{ElementBounds, PositionMode, TextBox};

use crate::state::AppState;

const PADDING: f32 = 16.0;
const RULER_HEIGHT: f32 = 30.0;

#[derive(Clone)]
struct CanvasDrag {
    element: usize,
    start: egui::Pos2,
    frame: TextBox,
    left_mm: f64,
    pixels_per_mm: f64,
    resize: bool,
    flip_h: bool,
    flip_v: bool,
}

/// Render and edit at the original raster scale, independent of GPU downsampling.
pub fn show_canvas(ui: &mut egui::Ui, state: &mut AppState) {
    let drag_id = ui.id().with("object_drag");
    let snap_id = ui.id().with("snap_mm");
    let mut snap = ui
        .ctx()
        .data_mut(|data| data.get_temp::<bool>(snap_id).unwrap_or(true));
    ui.horizontal(|ui| {
        if ui.button("Fit").clicked() {
            state.zoom_fit = true;
        }
        if ui.button("1:1").clicked() {
            state.zoom = 1.0;
            state.zoom_fit = false;
        }
        if ui.button("+").clicked() {
            state.zoom = (state.zoom * 1.25).min(20.0);
            state.zoom_fit = false;
        }
        if ui.button("−").clicked() {
            state.zoom = (state.zoom / 1.25).max(0.001);
            state.zoom_fit = false;
        }
        ui.label(format!("Zoom: {:.0}%", state.zoom * 100.0));
        ui.checkbox(&mut state.show_bounds, "Element bounds");
        ui.checkbox(&mut snap, "Snap 0.1 mm");
    });
    ui.ctx().data_mut(|data| data.insert_temp(snap_id, snap));
    if let Some((x, y, width, height)) = state
        .selected_element
        .and_then(|i| state.element_rect_mm(i))
    {
        ui.small(format!(
            "Selected: X {x:.2}, Y {y:.2} mm | {width:.2} × {height:.2} mm"
        ));
    } else {
        ui.small("Ruler: mm from printable left edge.");
    }
    let can_drag = state.is_free_layout() && !state.overall_flip_h;
    if state.overall_flip_h && state.is_free_layout() {
        ui.small("Turn off whole-label horizontal mirroring to drag objects. Numeric editing stays available.");
    } else if state.is_free_layout() {
        ui.small("Drag to move; drag the square handle to resize. Arrows: 0.1 mm; Shift: 1 mm.");
    } else {
        ui.small("Legacy layout: use Convert to free layout before dragging objects.");
    }
    if let Some(diagnostic) = state.preview_diagnostics.first() {
        let message = if state.preview_diagnostics.len() > 1 {
            format!(
                "{diagnostic} (+{} more)",
                state.preview_diagnostics.len() - 1
            )
        } else {
            diagnostic.clone()
        };
        ui.colored_label(ui.visuals().warn_fg_color, message)
            .on_hover_text(state.preview_diagnostics.join("\n"));
    }
    ui.separator();

    let canvas_size = ui.available_size();
    let (Some(texture), Some(bitmap)) = (&state.preview_texture, &state.preview_bitmap) else {
        let (rect, _) = ui.allocate_exact_size(canvas_size, egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, 0.0, egui::Color32::from_gray(200));
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Add elements to preview",
            egui::FontId::proportional(18.0),
            egui::Color32::from_gray(100),
        );
        return;
    };
    let original_size = egui::vec2(bitmap.width() as f32, bitmap.height() as f32);
    if state.zoom_fit {
        state.zoom = fit_zoom(original_size, canvas_size);
    }
    let display_size = original_size * state.zoom;
    let content_size = egui::vec2(
        (display_size.x + PADDING * 2.0 + 120.0).max(canvas_size.x - 12.0),
        (display_size.y + RULER_HEIGHT + PADDING * 2.0).max(canvas_size.y - 12.0),
    );
    let texture_id = texture.id();
    let dpi = state.target_dpi();
    let mut selection = None;
    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .scroll_source(egui::scroll_area::ScrollSource {
            drag: egui::scroll_area::DragScroll::Never,
            ..Default::default()
        })
        .show(ui, |ui| {
            let (rect, response) =
                ui.allocate_exact_size(content_size, egui::Sense::click_and_drag());
            let painter = ui.painter();
            painter.rect_filled(rect, 0.0, egui::Color32::from_gray(200));
            // A stable left/top origin prevents auto-length changes from moving
            // the canvas under the pointer during a drag.
            let label_rect = egui::Rect::from_min_size(
                rect.min + egui::vec2(PADDING, PADDING + RULER_HEIGHT),
                display_size,
            );
            painter.rect_filled(label_rect, 0.0, egui::Color32::WHITE);
            painter.image(
                texture_id,
                label_rect,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
            painter.rect_stroke(
                label_rect,
                0.0,
                egui::Stroke::new(1.0, egui::Color32::from_gray(128)),
                egui::StrokeKind::Outside,
            );
            draw_ruler(painter, label_rect, dpi, state.zoom);

            let selected_bounds = state
                .preview_bounds
                .iter()
                .find(|b| Some(b.element_index) == state.selected_element);
            let handle = selected_bounds.and_then(|bound| {
                if !can_drag {
                    return None;
                }
                state.elements[bound.element_index].as_object()?;
                let r = screen_bounds(label_rect.min, state.zoom, bound);
                Some(egui::Rect::from_center_size(
                    egui::pos2(
                        if state.overall_flip_h {
                            r.left()
                        } else {
                            r.right()
                        },
                        if state.overall_flip_v {
                            r.top()
                        } else {
                            r.bottom()
                        },
                    ),
                    egui::vec2(10.0, 10.0),
                ))
            });
            if state.show_bounds {
                for bound in &state.preview_bounds {
                    let selected = state.selected_element == Some(bound.element_index);
                    let color = if selected {
                        egui::Color32::from_rgb(0, 90, 190)
                    } else {
                        egui::Color32::from_rgba_unmultiplied(30, 120, 200, 120)
                    };
                    painter.rect_stroke(
                        screen_bounds(label_rect.min, state.zoom, bound),
                        0.0,
                        egui::Stroke::new(if selected { 2.0 } else { 1.0 }, color),
                        egui::StrokeKind::Inside,
                    );
                }
                if let Some(handle) = handle {
                    painter.rect_filled(handle, 1.0, egui::Color32::from_rgb(0, 90, 190));
                }
            }
            let hit = |pos| {
                state
                    .preview_bounds
                    .iter()
                    .rev()
                    .find(|b| screen_bounds(label_rect.min, state.zoom, b).contains(pos))
                    .map(|b| b.element_index)
            };
            if response.clicked()
                && let Some(pos) = response.interact_pointer_pos()
            {
                selection = hit(pos);
            }
            if can_drag
                && response.drag_started()
                && let Some(start) = ui.input(|i| i.pointer.press_origin())
            {
                let resize = state.show_bounds && handle.is_some_and(|h| h.contains(start));
                let index = if resize {
                    state.selected_element
                } else {
                    hit(start)
                };
                if let Some(index) = index {
                    selection = Some(index);
                    if let Some(object) = state.elements[index].as_object() {
                        let left_mm = state
                            .element_rect_mm(index)
                            .map_or(object.frame.x_mm, |r| r.0);
                        let drag = CanvasDrag {
                            element: index,
                            start,
                            frame: object.frame.clone(),
                            left_mm,
                            pixels_per_mm: f64::from(dpi) / 25.4 * f64::from(state.zoom),
                            resize,
                            flip_h: state.overall_flip_h,
                            flip_v: state.overall_flip_v,
                        };
                        ui.ctx().data_mut(|data| data.insert_temp(drag_id, drag));
                        state.zoom_fit = false;
                    }
                }
            }
            if can_drag
                && (response.dragged() || response.drag_stopped())
                && let Some(pos) = response.interact_pointer_pos()
                && let Some(drag) = ui
                    .ctx()
                    .data_mut(|data| data.get_temp::<CanvasDrag>(drag_id))
            {
                let delta = pos - drag.start;
                let height_mm = f64::from(state.tape_width_px) * 25.4 / f64::from(dpi);
                let frame = dragged_frame(&drag, delta, height_mm, snap, 25.4 / f64::from(dpi));
                if let Some(object) = state
                    .elements
                    .get_mut(drag.element)
                    .and_then(|e| e.as_object_mut())
                {
                    if drag.frame.position == PositionMode::Relative
                        && frame.position == PositionMode::Absolute
                    {
                        state.status_message =
                            "Moved before the previous object; using absolute position".into();
                    }
                    object.frame = frame;
                    state.mark_dirty();
                }
            }
            if response.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
                ui.ctx().data_mut(|data| data.remove::<CanvasDrag>(drag_id));
            }
        });
    if let Some(index) = selection {
        state.selected_element = Some(index);
    }
    if can_drag {
        keyboard_move(ui, state, dpi);
    }
}

fn dragged_frame(
    drag: &CanvasDrag,
    delta: egui::Vec2,
    height_mm: f64,
    snap: bool,
    minimum: f64,
) -> TextBox {
    let mut frame = drag.frame.clone();
    let dx = f64::from(delta.x) / drag.pixels_per_mm * if drag.flip_h { -1.0 } else { 1.0 };
    let dy = f64::from(delta.y) / drag.pixels_per_mm * if drag.flip_v { -1.0 } else { 1.0 };
    let quantize = |value: f64| {
        if snap {
            (value * 10.0).round() / 10.0
        } else {
            value
        }
    };
    if drag.resize {
        frame.width_mm = quantize(frame.width_mm + dx).clamp(minimum, 10_000.0);
        frame.height_mm =
            quantize(frame.height_mm + dy).clamp(minimum, (height_mm - frame.y_mm).max(minimum));
    } else {
        let left = quantize(drag.left_mm + dx).max(0.0);
        if frame.position == PositionMode::Relative {
            let gap = left - (drag.left_mm - drag.frame.gap_mm);
            if gap >= 0.0 {
                frame.gap_mm = gap;
            } else {
                frame.position = PositionMode::Absolute;
                frame.x_mm = left;
            }
        } else {
            frame.x_mm = left;
        }
        frame.y_mm = quantize(frame.y_mm + dy).clamp(0.0, (height_mm - frame.height_mm).max(0.0));
    }
    frame
}

fn keyboard_move(ui: &egui::Ui, state: &mut AppState, dpi: u16) {
    if ui.ctx().egui_wants_keyboard_input() {
        return;
    }
    let delta = ui.input(|i| {
        let step = if i.modifiers.shift { 1.0 } else { 0.1 };
        egui::vec2(
            (i.key_pressed(egui::Key::ArrowRight) as i8 - i.key_pressed(egui::Key::ArrowLeft) as i8)
                as f32
                * step,
            (i.key_pressed(egui::Key::ArrowDown) as i8 - i.key_pressed(egui::Key::ArrowUp) as i8)
                as f32
                * step,
        )
    });
    if delta == egui::Vec2::ZERO {
        return;
    }
    if let Some(index) = state.selected_element
        && let Some(object) = state.elements[index].as_object()
    {
        let drag = CanvasDrag {
            element: index,
            start: egui::Pos2::ZERO,
            frame: object.frame.clone(),
            left_mm: state
                .element_rect_mm(index)
                .map_or(object.frame.x_mm, |r| r.0),
            pixels_per_mm: 1.0,
            resize: false,
            flip_h: state.overall_flip_h,
            flip_v: state.overall_flip_v,
        };
        let frame = dragged_frame(
            &drag,
            delta,
            f64::from(state.tape_width_px) * 25.4 / f64::from(dpi),
            true,
            25.4 / f64::from(dpi),
        );
        state.elements[index].as_object_mut().unwrap().frame = frame;
        state.mark_dirty();
    }
}

fn fit_zoom(original_size: egui::Vec2, canvas_size: egui::Vec2) -> f32 {
    let available =
        canvas_size - egui::vec2(PADDING * 2.0 + 12.0, PADDING * 2.0 + RULER_HEIGHT + 12.0);
    (available.x.max(1.0) / original_size.x)
        .min(available.y.max(1.0) / original_size.y)
        .clamp(f32::EPSILON, 10.0)
}

fn screen_bounds(origin: egui::Pos2, zoom: f32, bound: &ElementBounds) -> egui::Rect {
    egui::Rect::from_min_size(
        origin + egui::vec2(bound.x as f32, bound.y as f32) * zoom,
        egui::vec2(bound.width as f32, bound.height as f32) * zoom,
    )
}

fn draw_ruler(painter: &egui::Painter, label: egui::Rect, dpi: u16, zoom: f32) {
    let pixels_per_mm = f32::from(dpi) / 25.4 * zoom;
    let step = ruler_step(pixels_per_mm);
    let minor_step = step / 5.0;
    let visible = label.intersect(painter.clip_rect());
    if visible.width() <= 0.0 {
        return;
    }
    let start =
        (((visible.min.x - label.min.x) / pixels_per_mm / minor_step).floor() as i64).max(0);
    let end = ((visible.max.x - label.min.x) / pixels_per_mm / minor_step).ceil() as i64;
    let baseline = label.min.y - 3.0;
    let color = egui::Color32::from_gray(65);
    painter.line_segment(
        [
            egui::pos2(visible.min.x, baseline),
            egui::pos2(visible.max.x, baseline),
        ],
        egui::Stroke::new(1.0, color),
    );
    for tick in start..=end {
        let mm = tick as f32 * minor_step;
        let x = label.min.x + mm * pixels_per_mm;
        if x > label.max.x + 0.01 {
            break;
        }
        let major = tick % 5 == 0;
        painter.line_segment(
            [
                egui::pos2(x, baseline),
                egui::pos2(x, baseline - if major { 9.0 } else { 4.0 }),
            ],
            egui::Stroke::new(1.0, color),
        );
        if major {
            let text = if step < 1.0 {
                format!("{mm:.1}")
            } else {
                format!("{mm:.0}")
            };
            painter.text(
                egui::pos2(x, baseline - 11.0),
                egui::Align2::CENTER_BOTTOM,
                text,
                egui::FontId::monospace(11.0),
                color,
            );
        }
    }
}

/// Keep numbers legible at every zoom, including very long labels in Fit mode.
fn ruler_step(pixels_per_mm: f32) -> f32 {
    let desired = 55.0 / pixels_per_mm;
    let base = 10.0_f32.powf(desired.log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .map(|multiple| multiple * base)
        .find(|step| *step >= desired)
        .unwrap_or(base * 10.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drag(frame: TextBox, left_mm: f64) -> CanvasDrag {
        CanvasDrag {
            element: 0,
            start: egui::Pos2::ZERO,
            frame,
            left_mm,
            pixels_per_mm: 20.0,
            resize: false,
            flip_h: false,
            flip_v: false,
        }
    }

    #[test]
    fn dragging_relative_objects_preserves_gap_until_they_cross_the_predecessor() {
        let original = TextBox {
            position: PositionMode::Relative,
            gap_mm: 3.0,
            height_mm: 4.0,
            ..TextBox::default()
        };
        let drag = drag(original, 18.0);
        let moved = dragged_frame(&drag, egui::vec2(40.0, 20.0), 10.0, true, 0.15);
        assert_eq!(moved.position, PositionMode::Relative);
        assert_eq!((moved.gap_mm, moved.y_mm), (5.0, 1.0));
        let overlap = dragged_frame(&drag, egui::vec2(-80.0, 0.0), 10.0, true, 0.15);
        assert_eq!(overlap.position, PositionMode::Absolute);
        assert_eq!(overlap.x_mm, 14.0);
    }

    #[test]
    fn dragging_uses_frozen_scale_and_respects_printable_height() {
        let mut drag = drag(
            TextBox {
                x_mm: 5.0,
                height_mm: 4.0,
                ..TextBox::default()
            },
            5.0,
        );
        let moved = dragged_frame(&drag, egui::vec2(-200.0, 500.0), 10.0, false, 0.15);
        assert_eq!((moved.x_mm, moved.y_mm), (0.0, 6.0));
        drag.flip_h = true;
        drag.flip_v = true;
        let mirrored = dragged_frame(&drag, egui::vec2(-40.0, -20.0), 10.0, false, 0.15);
        assert_eq!((mirrored.x_mm, mirrored.y_mm), (7.0, 1.0));
        drag.resize = true;
        let resized = dragged_frame(&drag, egui::vec2(-40.0, -500.0), 10.0, false, 0.15);
        assert_eq!(
            (resized.x_mm, resized.width_mm, resized.height_mm),
            (5.0, 12.0, 10.0)
        );
    }

    #[test]
    fn long_label_fit_uses_original_pixels_and_keeps_bounds_on_ruler() {
        let original = egui::vec2(100_000.0, 128.0);
        let canvas = egui::vec2(800.0, 500.0);
        let zoom = fit_zoom(original, canvas);
        assert!(zoom < 0.1);
        assert!(original.x * zoom < canvas.x);
        let bound = ElementBounds {
            element_index: 0,
            x: 709,
            y: 0,
            width: 142,
            height: 100,
        };
        let rect = screen_bounds(egui::Pos2::ZERO, zoom, &bound);
        assert!((rect.min.x - 709.0 * zoom).abs() < 0.001);
        let measured_mm = rect.min.x / zoom * 25.4 / 180.0;
        assert!((measured_mm - 100.05).abs() < 0.02);
        assert!(ruler_step(180.0 / 25.4 * zoom) * 180.0 / 25.4 * zoom >= 55.0);
    }
}
