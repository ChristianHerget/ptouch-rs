// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Central canvas panel for label preview display.

use ptouch_render::document::{ElementBounds, PositionMode, TextBox};

use crate::state::AppState;

const PADDING: f32 = 16.0;
const RULER_HEIGHT: f32 = 30.0;
const RULER_WIDTH: f32 = 52.0;
const ROTATE_RADIUS: f32 = 6.0;
const HANDLE_MARGIN: f32 = 24.0;

#[derive(Clone)]
struct RotationDrag {
    last_pointer: egui::Pos2,
    accumulated: f32,
    initial: f32,
    reverse: bool,
}

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
    rotation: Option<RotationDrag>,
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
        ui.small("X / Y: mm from the printable top-left.");
    }
    let can_edit = state.is_free_layout();
    let can_drag = can_edit && !state.overall_flip_h;
    if state.overall_flip_h && state.is_free_layout() {
        ui.small("Turn off label horizontal mirroring to move/resize. The round handle still rotates content.");
    } else if state.is_free_layout() {
        ui.small("Drag to move; square: resize; circle: rotate content (Shift: 15°). Arrows: 0.1 mm; Shift: 1 mm.");
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
    let viewport_size = canvas_size - egui::vec2(RULER_WIDTH, RULER_HEIGHT);
    let content_size = egui::vec2(
        (display_size.x + PADDING * 2.0 + HANDLE_MARGIN).max(viewport_size.x - 12.0),
        (display_size.y + PADDING * 2.0).max(viewport_size.y - 12.0),
    );
    let texture_id = texture.id();
    let dpi = state.target_dpi();
    // Outer None means no click; Some(None) explicitly clears selection.
    let mut selection = None;
    let (canvas_rect, _) = ui.allocate_exact_size(canvas_size, egui::Sense::hover());
    ui.painter()
        .rect_filled(canvas_rect, 0.0, egui::Color32::from_gray(200));
    let viewport_rect = egui::Rect::from_min_max(
        canvas_rect.min + egui::vec2(RULER_WIDTH, RULER_HEIGHT),
        canvas_rect.max,
    );
    let mut canvas_ui = ui.new_child(
        egui::UiBuilder::new()
            .id(ui.id().with("canvas_scroll"))
            .max_rect(viewport_rect),
    );
    let mut ruler_label = None;
    let scroll = egui::ScrollArea::both()
        .auto_shrink([false, false])
        .scroll_source(egui::scroll_area::ScrollSource {
            drag: egui::scroll_area::DragScroll::Never,
            ..Default::default()
        })
        .show(&mut canvas_ui, |ui| {
            let (rect, response) =
                ui.allocate_exact_size(content_size, egui::Sense::click_and_drag());
            let painter = ui.painter();
            painter.rect_filled(rect, 0.0, egui::Color32::from_gray(200));
            // A stable left/top origin prevents auto-length changes from moving
            // the canvas under the pointer during a drag.
            let label_rect =
                egui::Rect::from_min_size(rect.min + egui::vec2(PADDING, PADDING), display_size);
            ruler_label = Some(label_rect);
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
            let rotate_handle = selected_bounds.and_then(|bound| {
                if !can_edit {
                    return None;
                }
                state.elements[bound.element_index].as_object()?;
                Some(rotation_handle(screen_bounds(
                    label_rect.min,
                    state.zoom,
                    bound,
                )))
            });
            let rotation_hit = |pos: egui::Pos2| {
                state.show_bounds
                    && rotate_handle
                        .is_some_and(|center| center.distance(pos) <= ROTATE_RADIUS + 4.0)
            };
            let resize_hit = |pos| state.show_bounds && handle.is_some_and(|h| h.contains(pos));
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
                if let (Some(center), Some(bound)) = (rotate_handle, selected_bounds) {
                    let color = egui::Color32::from_rgb(0, 90, 190);
                    let frame = screen_bounds(label_rect.min, state.zoom, bound);
                    painter
                        .line_segment([frame.right_top(), center], egui::Stroke::new(1.5, color));
                    painter.circle_filled(center, ROTATE_RADIUS, egui::Color32::WHITE);
                    painter.circle_stroke(center, ROTATE_RADIUS, egui::Stroke::new(2.0, color));
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
                selection = Some(if rotation_hit(pos) || resize_hit(pos) {
                    state.selected_element
                } else {
                    hit(pos)
                });
            }
            if let Some(pos) = response.hover_pos() {
                if rotation_hit(pos) {
                    response
                        .clone()
                        .on_hover_cursor(egui::CursorIcon::Grab)
                        .on_hover_text(
                            "Rotate content inside the fixed frame. Hold Shift to snap to 15°.",
                        );
                } else if resize_hit(pos) {
                    response
                        .clone()
                        .on_hover_cursor(if state.overall_flip_v {
                            egui::CursorIcon::ResizeNeSw
                        } else {
                            egui::CursorIcon::ResizeNwSe
                        })
                        .on_hover_text("Resize frame");
                } else if can_drag && hit(pos).is_some() {
                    response.clone().on_hover_cursor(egui::CursorIcon::Move);
                }
            }
            if can_edit
                && response.drag_started()
                && let Some(start) = ui.input(|i| i.pointer.press_origin())
            {
                let rotate = rotation_hit(start);
                let resize = !rotate && resize_hit(start);
                let index = if rotate || resize {
                    state.selected_element
                } else if can_drag {
                    hit(start)
                } else {
                    None
                };
                if let Some(index) = index {
                    selection = Some(Some(index));
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
                            rotation: if rotate {
                                selected_bounds.map(|_| RotationDrag {
                                    last_pointer: start,
                                    accumulated: 0.0,
                                    initial: object.rotation,
                                    reverse: object.flip_h
                                        ^ object.flip_v
                                        ^ state.overall_flip_h
                                        ^ state.overall_flip_v,
                                })
                            } else {
                                None
                            },
                        };
                        ui.ctx().data_mut(|data| data.insert_temp(drag_id, drag));
                        state.zoom_fit = false;
                    }
                }
            }
            if can_edit
                && (response.dragged() || response.drag_stopped())
                && let Some(pos) = response.interact_pointer_pos()
                && let Some(mut drag) = ui
                    .ctx()
                    .data_mut(|data| data.get_temp::<CanvasDrag>(drag_id))
            {
                let rotation = drag.rotation.as_mut().and_then(|rotation| {
                    let bound = state
                        .preview_bounds
                        .iter()
                        .find(|bound| bound.element_index == drag.element)?;
                    let center = screen_bounds(label_rect.min, state.zoom, bound).center();
                    dragged_rotation(rotation, pos, center, ui.input(|i| i.modifiers.shift))
                });
                let frame = if drag.rotation.is_none() && can_drag {
                    let delta = pos - drag.start;
                    let height_mm = f64::from(state.tape_width_px) * 25.4 / f64::from(dpi);
                    Some(dragged_frame(
                        &drag,
                        delta,
                        height_mm,
                        snap,
                        25.4 / f64::from(dpi),
                    ))
                } else {
                    None
                };
                if let Some(object) = state
                    .elements
                    .get_mut(drag.element)
                    .and_then(|e| e.as_object_mut())
                {
                    if let Some(frame) = frame {
                        if drag.frame.position == PositionMode::Relative
                            && frame.position == PositionMode::Absolute
                        {
                            state.status_message =
                                "Moved before the previous object; using absolute position".into();
                        }
                        object.frame = frame;
                        state.mark_dirty();
                    } else if let Some(rotation) = rotation {
                        object.rotation = rotation;
                        state.mark_dirty();
                    }
                }
                ui.ctx().data_mut(|data| data.insert_temp(drag_id, drag));
            }
            if response.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
                ui.ctx().data_mut(|data| data.remove::<CanvasDrag>(drag_id));
            }
        });
    if let Some(label) = ruler_label {
        draw_rulers(
            ui.painter(),
            canvas_rect,
            scroll.inner_rect,
            label,
            dpi,
            state.zoom,
        );
    }
    if let Some(selection) = selection {
        state.selected_element = selection;
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
            rotation: None,
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
    let available = canvas_size
        - egui::vec2(
            PADDING * 2.0 + RULER_WIDTH + HANDLE_MARGIN + 12.0,
            PADDING * 2.0 + RULER_HEIGHT + 12.0,
        );
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

fn rotation_handle(frame: egui::Rect) -> egui::Pos2 {
    frame.right_top() + egui::vec2(14.0, -9.0)
}

/// Accumulate wrapped pointer steps so crossing ±180° and multiple turns are
/// continuous. Mirroring reverses the visible angular direction of content.
fn dragged_rotation(
    drag: &mut RotationDrag,
    pointer: egui::Pos2,
    center: egui::Pos2,
    snap: bool,
) -> Option<f32> {
    // Measure both pointer positions around the current frame center. Diagnostics
    // or window resizing can move the canvas during a gesture; a stationary
    // pointer must not rotate content when that happens.
    let previous = drag.last_pointer - center;
    let vector = pointer - center;
    drag.last_pointer = pointer;
    if vector.length_sq() < 16.0 || previous.length_sq() < 16.0 {
        return None;
    }
    let step = (vector.angle() - previous.angle() + std::f32::consts::PI)
        .rem_euclid(std::f32::consts::TAU)
        - std::f32::consts::PI;
    drag.accumulated += step.to_degrees();
    let value = drag.initial + drag.accumulated * if drag.reverse { -1.0 } else { 1.0 };
    Some(
        (if snap {
            (value / 15.0).round() * 15.0
        } else {
            value
        })
        .rem_euclid(360.0),
    )
}

struct RulerTick {
    screen: f32,
    mm: f32,
    major: bool,
}

/// Use original raster coordinates, even if the preview texture was reduced.
fn ruler_ticks(
    visible_start: f32,
    visible_end: f32,
    origin: f32,
    length: f32,
    pixels_per_mm: f32,
    label_spacing: f32,
) -> (f32, Vec<RulerTick>) {
    let step = ruler_step(pixels_per_mm * 55.0 / label_spacing);
    let minor = step / 5.0;
    let start = (((visible_start - origin) / pixels_per_mm / minor).floor() as i64).max(0);
    let end = (((visible_end.min(origin + length) - origin) / pixels_per_mm / minor).ceil() as i64)
        .max(-1);
    let ticks = (start..=end)
        .filter_map(|tick| {
            let mm = tick as f32 * minor;
            let screen = origin + mm * pixels_per_mm;
            (screen >= visible_start && screen <= visible_end && screen <= origin + length + 0.01)
                .then_some(RulerTick {
                    screen,
                    mm,
                    major: tick % 5 == 0,
                })
        })
        .collect();
    (step, ticks)
}

fn draw_rulers(
    painter: &egui::Painter,
    canvas: egui::Rect,
    viewport: egui::Rect,
    label: egui::Rect,
    dpi: u16,
    zoom: f32,
) {
    let pixels_per_mm = f32::from(dpi) / 25.4 * zoom;
    let color = egui::Color32::from_gray(65);
    let font = egui::FontId::monospace(11.0);
    // Fixed gutters stay visible while the label scrolls in either direction.
    // Only tick coordinates follow the scrolled label's original pixel origin.
    let x_band = egui::Rect::from_min_max(
        egui::pos2(viewport.left(), canvas.top()),
        egui::pos2(viewport.right(), viewport.top()),
    );
    let y_band = egui::Rect::from_min_max(
        egui::pos2(canvas.left(), viewport.top()),
        egui::pos2(viewport.left(), viewport.bottom()),
    );
    let x_painter = painter.with_clip_rect(painter.clip_rect().intersect(x_band));
    let y_painter = painter.with_clip_rect(painter.clip_rect().intersect(y_band));
    let text = |mm: f32, step: f32| {
        if step < 1.0 {
            format!("{mm:.1}")
        } else {
            format!("{mm:.0}")
        }
    };
    let x_baseline = viewport.top() - 3.0;
    let y_baseline = viewport.left() - 3.0;
    let (x_step, x_ticks) = ruler_ticks(
        viewport.left(),
        viewport.right(),
        label.left(),
        label.width(),
        pixels_per_mm,
        55.0,
    );
    for tick in x_ticks {
        x_painter.line_segment(
            [
                egui::pos2(tick.screen, x_baseline),
                egui::pos2(tick.screen, x_baseline - if tick.major { 9.0 } else { 4.0 }),
            ],
            egui::Stroke::new(1.0, color),
        );
        if tick.major {
            x_painter.text(
                egui::pos2(tick.screen, x_baseline - 11.0),
                egui::Align2::CENTER_BOTTOM,
                text(tick.mm, x_step),
                font.clone(),
                color,
            );
        }
    }
    if label.right() > viewport.left() && label.left() < viewport.right() {
        x_painter.line_segment(
            [
                egui::pos2(label.left().max(viewport.left()), x_baseline),
                egui::pos2(label.right().min(viewport.right()), x_baseline),
            ],
            egui::Stroke::new(1.0, color),
        );
    }
    let (y_step, y_ticks) = ruler_ticks(
        viewport.top(),
        viewport.bottom(),
        label.top(),
        label.height(),
        pixels_per_mm,
        28.0,
    );
    let end_visible = label.bottom() >= viewport.top() && label.bottom() + 7.0 <= viewport.bottom();
    for tick in y_ticks {
        y_painter.line_segment(
            [
                egui::pos2(y_baseline, tick.screen),
                egui::pos2(y_baseline - if tick.major { 9.0 } else { 4.0 }, tick.screen),
            ],
            egui::Stroke::new(1.0, color),
        );
        if tick.major && (!end_visible || label.bottom() - tick.screen > 17.0) {
            y_painter.text(
                egui::pos2(y_baseline - 12.0, tick.screen),
                egui::Align2::RIGHT_CENTER,
                text(tick.mm, y_step),
                font.clone(),
                color,
            );
        }
    }
    if label.bottom() > viewport.top() && label.top() < viewport.bottom() {
        y_painter.line_segment(
            [
                egui::pos2(y_baseline, label.top().max(viewport.top())),
                egui::pos2(y_baseline, label.bottom().min(viewport.bottom())),
            ],
            egui::Stroke::new(1.0, color),
        );
    }
    if end_visible {
        y_painter.line_segment(
            [
                egui::pos2(y_baseline, label.bottom()),
                egui::pos2(y_baseline - 9.0, label.bottom()),
            ],
            egui::Stroke::new(1.0, color),
        );
        y_painter.text(
            egui::pos2(y_baseline - 12.0, label.bottom()),
            egui::Align2::RIGHT_CENTER,
            format!("{:.2}", label.height() / pixels_per_mm),
            font.clone(),
            color,
        );
    }
    painter.text(
        egui::pos2(
            canvas.left() + RULER_WIDTH / 2.0,
            canvas.top() + RULER_HEIGHT / 2.0,
        ),
        egui::Align2::CENTER_CENTER,
        "mm",
        font,
        color,
    );
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
            rotation: None,
        }
    }

    fn canvas_event(
        context: &egui::Context,
        state: &mut AppState,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                events,
                ..Default::default()
            },
            |ui| show_canvas(ui, state),
        );
        output.textures_delta.clear();
        output
    }

    fn canvas_fixture(context: &egui::Context, image: bool, mirrors: u8) -> AppState {
        use ptouch_render::document::{LabelElement, LabelObject};
        let frame = TextBox {
            x_mm: 5.0,
            y_mm: 1.0,
            width_mm: 10.0,
            height_mm: 4.0,
            ..TextBox::default()
        };
        let mut object = if image {
            let mut bytes = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                8,
                8,
                image::Rgba([0, 0, 0, 255]),
            ))
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
            LabelObject::image_from_bytes(None, bytes.into_inner(), frame)
        } else {
            LabelObject::text("PORT 1", frame)
        };
        object.rotation = 30.0;
        object.flip_h = mirrors & 1 != 0;
        object.flip_v = mirrors & 2 != 0;
        AppState {
            zoom_fit: false,
            zoom: 2.0,
            overall_flip_h: mirrors & 4 != 0,
            overall_flip_v: mirrors & 8 != 0,
            elements: vec![LabelElement::Object(object)],
            selected_element: Some(0),
            preview_bitmap: Some(ptouch_render::bitmap::LabelBitmap::new(200, 76)),
            preview_bounds: vec![ElementBounds {
                element_index: 0,
                x: 35,
                y: 7,
                width: 71,
                height: 28,
            }],
            preview_texture: Some(context.load_texture(
                "gesture_fixture",
                egui::ColorImage::filled([200, 76], egui::Color32::WHITE),
                Default::default(),
            )),
            ..AppState::default()
        }
    }

    fn selected_frame_and_circle(output: &egui::FullOutput) -> (egui::Rect, egui::Pos2) {
        let color = egui::Color32::from_rgb(0, 90, 190);
        let mut frame = None;
        let mut circle = None;
        for shape in &output.shapes {
            match &shape.shape {
                egui::Shape::Rect(rect)
                    if rect.stroke.color == color && rect.stroke.width == 2.0 =>
                {
                    frame = Some(rect.rect)
                }
                egui::Shape::Circle(c) if c.stroke.color == color && c.radius == ROTATE_RADIUS => {
                    circle = Some(c.center)
                }
                _ => {}
            }
        }
        (frame.unwrap(), circle.unwrap())
    }

    fn pointer_button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        }
    }

    #[test]
    fn round_handle_rotates_text_and_images_with_mirrors_and_release_position() {
        for image in [false, true] {
            for mirrors in 0_u8..16 {
                let context = egui::Context::default();
                let mut state = canvas_fixture(&context, image, mirrors);
                let original = state.elements[0].as_object().unwrap().frame.clone();
                let output = canvas_event(&context, &mut state, vec![]);
                let (frame, circle) = selected_frame_and_circle(&output);
                // Clicking an external handle must not trigger blank-canvas deselection.
                canvas_event(
                    &context,
                    &mut state,
                    vec![
                        egui::Event::PointerMoved(circle),
                        pointer_button(circle, true),
                    ],
                );
                canvas_event(&context, &mut state, vec![pointer_button(circle, false)]);
                assert_eq!(state.selected_element, Some(0));
                canvas_event(&context, &mut state, vec![pointer_button(circle, true)]);
                let vector = circle - frame.center();
                let moved =
                    frame.center() + egui::emath::Rot2::from_angle(90_f32.to_radians()) * vector;
                canvas_event(&context, &mut state, vec![egui::Event::PointerMoved(moved)]);
                let released =
                    frame.center() + egui::emath::Rot2::from_angle(105_f32.to_radians()) * vector;
                canvas_event(
                    &context,
                    &mut state,
                    vec![
                        egui::Event::PointerMoved(released),
                        pointer_button(released, false),
                    ],
                );
                let object = state.elements[0].as_object().unwrap();
                let expected = if mirrors.count_ones() % 2 == 0 {
                    135.0
                } else {
                    285.0
                };
                assert!(
                    (object.rotation - expected).abs() < 0.01,
                    "image={image} mirrors={mirrors}: {}",
                    object.rotation
                );
                assert_eq!(object.frame, original);
                let saved = state.document().to_toml_string().unwrap();
                let reopened =
                    ptouch_render::document::LabelDocument::from_toml_str(&saved).unwrap();
                assert_eq!(reopened.elements[0].as_object().unwrap().frame, original);
                assert_eq!(
                    reopened.elements[0].as_object().unwrap().rotation,
                    object.rotation
                );
            }
        }
    }

    #[test]
    fn diagnostic_layout_changes_do_not_shift_rotation_angles() {
        let context = egui::Context::default();
        let mut state = canvas_fixture(&context, false, 0);
        let output = canvas_event(&context, &mut state, vec![]);
        let (frame, circle) = selected_frame_and_circle(&output);
        canvas_event(
            &context,
            &mut state,
            vec![
                egui::Event::PointerMoved(circle),
                pointer_button(circle, true),
            ],
        );
        let pointer = frame.center()
            + egui::emath::Rot2::from_angle(45_f32.to_radians()) * (circle - frame.center());
        canvas_event(
            &context,
            &mut state,
            vec![egui::Event::PointerMoved(pointer)],
        );
        assert!((state.elements[0].as_object().unwrap().rotation - 75.0).abs() < 0.01);
        state.preview_diagnostics = vec!["Fixed-size text was clipped".into()];
        let output = canvas_event(&context, &mut state, vec![]);
        let (shifted, _) = selected_frame_and_circle(&output);
        assert!(shifted.center().y > frame.center().y);
        assert!((state.elements[0].as_object().unwrap().rotation - 75.0).abs() < 0.01);
        let moved = shifted.center()
            + egui::emath::Rot2::from_angle(30_f32.to_radians()) * (pointer - shifted.center());
        let after = canvas_event(&context, &mut state, vec![egui::Event::PointerMoved(moved)]);
        assert!(
            (state.elements[0].as_object().unwrap().rotation - 105.0).abs() < 0.01,
            "rotation={} old={:?} new={:?}",
            state.elements[0].as_object().unwrap().rotation,
            shifted,
            selected_frame_and_circle(&after).0
        );
        state.preview_diagnostics.clear();
        canvas_event(&context, &mut state, vec![pointer_button(moved, false)]);
        assert!((state.elements[0].as_object().unwrap().rotation - 105.0).abs() < 0.01);
    }

    #[test]
    fn moving_and_resizing_still_work_beside_the_fixed_rulers() {
        for image in [false, true] {
            for resize in [false, true] {
                let context = egui::Context::default();
                let mut state = canvas_fixture(&context, image, 0);
                let output = canvas_event(&context, &mut state, vec![]);
                let (frame, _) = selected_frame_and_circle(&output);
                let start = if resize {
                    frame.right_bottom()
                } else {
                    frame.center()
                };
                canvas_event(
                    &context,
                    &mut state,
                    vec![
                        egui::Event::PointerMoved(start),
                        pointer_button(start, true),
                    ],
                );
                let end = start + egui::vec2(20.0, 14.0);
                canvas_event(&context, &mut state, vec![egui::Event::PointerMoved(end)]);
                canvas_event(&context, &mut state, vec![pointer_button(end, false)]);
                let object = state.elements[0].as_object().unwrap();
                assert_eq!(object.rotation, 30.0);
                if resize {
                    assert_eq!((object.frame.x_mm, object.frame.y_mm), (5.0, 1.0));
                    assert_eq!((object.frame.width_mm, object.frame.height_mm), (11.4, 5.0));
                } else {
                    assert_eq!((object.frame.x_mm, object.frame.y_mm), (6.4, 2.0));
                    assert_eq!((object.frame.width_mm, object.frame.height_mm), (10.0, 4.0));
                }
            }
        }
    }

    #[test]
    fn rotation_unwraps_multiple_turns_ignores_the_center_and_snaps_angles() {
        let point =
            |degrees: f32| egui::Pos2::ZERO + egui::Vec2::angled(degrees.to_radians()) * 50.0;
        let mut rotation = RotationDrag {
            last_pointer: point(179.0),
            accumulated: 0.0,
            initial: 30.0,
            reverse: false,
        };
        assert!(
            (dragged_rotation(&mut rotation, point(-179.0), egui::Pos2::ZERO, false).unwrap()
                - 32.0)
                .abs()
                < 0.01
        );
        assert_eq!(
            dragged_rotation(&mut rotation, egui::pos2(1.0, 1.0), egui::Pos2::ZERO, false),
            None
        );
        // Leaving the ignored center re-establishes the pointer without a jump.
        assert_eq!(
            dragged_rotation(&mut rotation, point(-179.0), egui::Pos2::ZERO, false),
            None
        );
        for angle in [-89.0, 1.0, 91.0, -179.0] {
            dragged_rotation(&mut rotation, point(angle), egui::Pos2::ZERO, false);
        }
        assert!((rotation.accumulated - 362.0).abs() < 0.01);
        assert_eq!(
            dragged_rotation(&mut rotation, point(-170.0), egui::Pos2::ZERO, true),
            Some(45.0)
        );
    }

    #[test]
    fn y_ruler_uses_printable_pixels_and_tracks_scrolled_original_coordinates() {
        for (dpi, height, endpoint) in [(180, 76, "10.72"), (360, 150, "10.58")] {
            let context = egui::Context::default();
            let mut state = canvas_fixture(&context, false, 0);
            state.design_dpi = dpi;
            state.preview_bitmap = Some(ptouch_render::bitmap::LabelBitmap::new(200, height));
            let output = canvas_event(&context, &mut state, vec![]);
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(t) if t.galley.job.text == endpoint)));
            let scale = f32::from(dpi) / 25.4 * 2.0;
            let (_, ticks) = ruler_ticks(100.0, 400.0, -75.0, 1000.0, scale, 28.0);
            assert!(!ticks.is_empty());
            for tick in ticks {
                assert!((tick.screen + 75.0 - tick.mm * scale).abs() < 0.001);
                assert!((100.0..=400.0).contains(&tick.screen));
            }
        }
    }

    #[test]
    fn clicking_blank_canvas_deselects_and_arrow_keys_leave_objects_in_place() {
        use ptouch_render::document::{LabelElement, LabelObject};
        let context = egui::Context::default();
        let mut state = AppState {
            zoom_fit: false,
            zoom: 2.0,
            elements: vec![LabelElement::Object(LabelObject::text(
                "A",
                TextBox {
                    x_mm: 5.0,
                    ..TextBox::default()
                },
            ))],
            selected_element: Some(0),
            preview_bitmap: Some(ptouch_render::bitmap::LabelBitmap::new(200, 76)),
            preview_bounds: vec![ElementBounds {
                element_index: 0,
                x: 35,
                y: 0,
                width: 71,
                height: 57,
            }],
            ..AppState::default()
        };
        state.preview_texture = Some(context.load_texture(
            "test_canvas",
            egui::ColorImage::filled([200, 76], egui::Color32::WHITE),
            Default::default(),
        ));
        let blank = egui::pos2(500.0, 400.0);
        for events in [
            vec![],
            vec![
                egui::Event::PointerMoved(blank),
                egui::Event::PointerButton {
                    pos: blank,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
            vec![egui::Event::PointerButton {
                pos: blank,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
            vec![egui::Event::Key {
                key: egui::Key::ArrowRight,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
        ] {
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 600.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| show_canvas(ui, &mut state),
            );
            output.textures_delta.clear();
        }
        assert_eq!(state.selected_element, None);
        assert_eq!(state.elements[0].as_object().unwrap().frame.x_mm, 5.0);
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
