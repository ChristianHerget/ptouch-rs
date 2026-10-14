// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Central canvas panel for label preview display.

use ptouch_render::document::{ContentBox, ElementBounds, PositionMode, TextBox};

use crate::state::{AppState, CanvasEditTarget};

const PADDING: f32 = 24.0;
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
struct ContentDrag {
    original: ContentBox,
    corners: [egui::Pos2; 4],
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
    content: Option<ContentDrag>,
    font_size: Option<f32>,
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
        ui.separator();
        ui.selectable_value(
            &mut state.canvas_edit_target,
            CanvasEditTarget::Content,
            "Object",
        );
        ui.selectable_value(
            &mut state.canvas_edit_target,
            CanvasEditTarget::Slot,
            "Slot",
        );
    });
    ui.ctx().data_mut(|data| data.insert_temp(snap_id, snap));
    if let Some((x, y, width, height)) = state
        .selected_element
        .and_then(|i| state.element_rect_mm(i))
    {
        ui.small(format!(
            "Slot: X {x:.2}, Y {y:.2} mm | {width:.2} × {height:.2} mm"
        ));
    } else {
        ui.small("X / Y: mm from the printable top-left.");
    }
    let edit_content = state.canvas_edit_target == CanvasEditTarget::Content;
    let can_edit = state.is_free_layout();
    let can_drag = can_edit && !state.overall_flip_h;
    if state.overall_flip_h && state.is_free_layout() {
        ui.small("Turn off label horizontal mirroring to drag objects or handles. Numeric editing remains available.");
    } else if can_edit && edit_content {
        ui.small("Drag object to move; square: scale; circle: rotate (Shift: 15°). The slot stays fixed.");
    } else if can_edit {
        ui.small(
            "Drag slot to move; square: resize slot. Independent object dimensions stay fixed.",
        );
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
            let slot_quad =
                |bound: &ElementBounds| rect_quad(screen_bounds(label_rect.min, state.zoom, bound));
            let object_quad = |index: usize| {
                state
                    .preview_content_bounds
                    .iter()
                    .find(|g| g.element_index == index)
                    .map(|g| {
                        g.corners
                            .map(|p| label_rect.min + egui::vec2(p[0], p[1]) * state.zoom)
                    })
                    .or_else(|| {
                        // Legacy flow elements have only raster bounds. Keep
                        // them selectable without converting their geometry.
                        state
                            .elements
                            .get(index)
                            .filter(|e| e.as_object().is_none())?;
                        state
                            .preview_bounds
                            .iter()
                            .find(|b| b.element_index == index)
                            .map(slot_quad)
                    })
            };
            let selected_bounds = state
                .preview_bounds
                .iter()
                .find(|b| Some(b.element_index) == state.selected_element);
            let selected_quad = selected_bounds.and_then(|bound| {
                if edit_content {
                    object_quad(bound.element_index)
                } else {
                    Some(slot_quad(bound))
                }
            });
            let resize_corner = selected_quad.map(|quad| {
                if edit_content {
                    quad[2]
                } else if state.overall_flip_v {
                    quad[1]
                } else {
                    quad[2]
                }
            });
            let handle = resize_corner
                .filter(|_| can_drag)
                .map(|pos| egui::Rect::from_center_size(pos, egui::vec2(10.0, 10.0)));
            let rotate_handle = selected_quad
                .filter(|_| can_drag && edit_content)
                .map(rotation_handle);
            let rotation_hit = |pos: egui::Pos2| {
                state.show_bounds
                    && rotate_handle
                        .is_some_and(|center| center.distance(pos) <= ROTATE_RADIUS + 4.0)
            };
            let resize_hit = |pos| state.show_bounds && handle.is_some_and(|h| h.contains(pos));
            if state.show_bounds {
                for bound in &state.preview_bounds {
                    let selected = state.selected_element == Some(bound.element_index);
                    let slot_selected = selected && !edit_content;
                    painter.rect_stroke(
                        screen_bounds(label_rect.min, state.zoom, bound),
                        0.0,
                        egui::Stroke::new(
                            if slot_selected { 2.0 } else { 1.0 },
                            if slot_selected {
                                egui::Color32::from_rgb(0, 90, 190)
                            } else {
                                egui::Color32::from_rgba_unmultiplied(100, 120, 140, 100)
                            },
                        ),
                        egui::StrokeKind::Inside,
                    );
                    if let Some(quad) = object_quad(bound.element_index) {
                        painter.add(egui::Shape::closed_line(
                            quad.to_vec(),
                            egui::Stroke::new(
                                if selected && edit_content { 2.0 } else { 1.0 },
                                if selected && edit_content {
                                    egui::Color32::from_rgb(0, 90, 190)
                                } else {
                                    egui::Color32::from_rgba_unmultiplied(30, 120, 200, 100)
                                },
                            ),
                        ));
                    }
                }
                if let Some(handle) = handle {
                    painter.rect_filled(handle, 1.0, egui::Color32::from_rgb(0, 90, 190));
                }
                if let (Some(center), Some(quad)) = (rotate_handle, selected_quad) {
                    let color = egui::Color32::from_rgb(0, 90, 190);
                    painter.line_segment([quad[1], center], egui::Stroke::new(1.5, color));
                    painter.circle_filled(center, ROTATE_RADIUS, egui::Color32::WHITE);
                    painter.circle_stroke(center, ROTATE_RADIUS, egui::Stroke::new(2.0, color));
                }
            }
            let hit = |pos| {
                state
                    .preview_bounds
                    .iter()
                    .rev()
                    .find(|bound| {
                        if edit_content {
                            object_quad(bound.element_index).is_some_and(|q| quad_contains(q, pos))
                        } else {
                            screen_bounds(label_rect.min, state.zoom, bound).contains(pos)
                        }
                    })
                    .map(|bound| bound.element_index)
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
                        .on_hover_text("Rotate object. Hold Shift to snap to 15°.");
                } else if resize_hit(pos) {
                    let direction = selected_quad.map_or(egui::vec2(1.0, 1.0), |q| q[2] - q[0]);
                    response
                        .clone()
                        .on_hover_cursor(resize_cursor(direction))
                        .on_hover_text(if edit_content {
                            "Scale object; keep the slot unchanged"
                        } else {
                            "Resize slot"
                        });
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
                    let content = if edit_content {
                        object_quad(index)
                            .zip(state.resolved_content_box(index))
                            .map(|(corners, original)| ContentDrag { original, corners })
                    } else {
                        None
                    };
                    if let Some(object) = state.elements[index].as_object() {
                        let drag = CanvasDrag {
                            element: index,
                            start,
                            frame: object.frame.clone(),
                            left_mm: state
                                .element_rect_mm(index)
                                .map_or(object.frame.x_mm, |r| r.0),
                            pixels_per_mm: f64::from(dpi) / 25.4 * f64::from(state.zoom),
                            resize,
                            flip_h: state.overall_flip_h ^ (edit_content && object.flip_h),
                            flip_v: state.overall_flip_v ^ (edit_content && object.flip_v),
                            rotation: rotate.then_some(RotationDrag {
                                last_pointer: start,
                                accumulated: 0.0,
                                initial: object.rotation,
                                reverse: object.flip_h
                                    ^ object.flip_v
                                    ^ state.overall_flip_h
                                    ^ state.overall_flip_v,
                            }),
                            content,
                            font_size: match &object.content {
                                ptouch_render::document::ObjectContent::Text {
                                    font_size, ..
                                } => *font_size,
                                _ => None,
                            },
                        };
                        if drag.content.is_some() {
                            state.pin_content_box(index);
                        }
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
                let rotation = if let Some(rotation) = drag.rotation.as_mut() {
                    let center = state
                        .preview_content_bounds
                        .iter()
                        .find(|g| g.element_index == drag.element)
                        .map(|g| {
                            quad_center(
                                g.corners
                                    .map(|p| label_rect.min + egui::vec2(p[0], p[1]) * state.zoom),
                            )
                        });
                    center.and_then(|center| {
                        dragged_rotation(rotation, pos, center, ui.input(|i| i.modifiers.shift))
                    })
                } else {
                    None
                };
                let new_content = if drag.rotation.is_none() && can_drag {
                    drag.content.as_ref().map(|content| {
                        dragged_content(
                            &drag,
                            content,
                            pos - drag.start,
                            snap,
                            25.4 / f64::from(dpi),
                        )
                    })
                } else {
                    None
                };
                let frame = if drag.content.is_none() && drag.rotation.is_none() && can_drag {
                    Some(dragged_frame(
                        &drag,
                        pos - drag.start,
                        f64::from(state.tape_width_px) * 25.4 / f64::from(dpi),
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
                                "Moved before the previous slot; using absolute position".into();
                        }
                        object.frame = frame;
                        state.mark_dirty();
                    } else if let Some(content) = new_content {
                        if drag.resize
                            && let Some(size) = drag.font_size
                            && let Some(start) = &drag.content
                            && let ptouch_render::document::ObjectContent::Text {
                                font_size, ..
                            } = &mut object.content
                        {
                            *font_size =
                                Some(size * (content.width_mm / start.original.width_mm) as f32);
                        }
                        object.content_box = Some(content);
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
        if state.canvas_edit_target == CanvasEditTarget::Content {
            let flip_h = object.flip_h ^ state.overall_flip_h;
            let flip_v = object.flip_v ^ state.overall_flip_v;
            if let Some(mut content) = state.pin_content_box(index) {
                content.offset_x_mm += f64::from(delta.x) * if flip_h { -1.0 } else { 1.0 };
                content.offset_y_mm += f64::from(delta.y) * if flip_v { -1.0 } else { 1.0 };
                state.elements[index].as_object_mut().unwrap().content_box = Some(content);
                state.mark_dirty();
            }
            return;
        }
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
            content: None,
            font_size: None,
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

fn rect_quad(rect: egui::Rect) -> [egui::Pos2; 4] {
    [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ]
}

fn quad_center(quad: [egui::Pos2; 4]) -> egui::Pos2 {
    (quad[0].to_vec2() + quad[1].to_vec2() + quad[2].to_vec2() + quad[3].to_vec2()).to_pos2() / 4.0
}

fn rotation_handle(quad: [egui::Pos2; 4]) -> egui::Pos2 {
    quad[1] + (quad[1] - quad_center(quad)).normalized() * 16.0
}

fn quad_contains(quad: [egui::Pos2; 4], point: egui::Pos2) -> bool {
    let mut positive = false;
    let mut negative = false;
    for i in 0..4 {
        let edge = quad[(i + 1) % 4] - quad[i];
        let delta = point - quad[i];
        let cross = edge.x * delta.y - edge.y * delta.x;
        positive |= cross > 0.01;
        negative |= cross < -0.01;
    }
    !(positive && negative)
}

fn resize_cursor(direction: egui::Vec2) -> egui::CursorIcon {
    let sector = ((direction.angle().to_degrees().rem_euclid(180.0) + 22.5) / 45.0) as u32 % 4;
    match sector {
        0 => egui::CursorIcon::ResizeHorizontal,
        1 => egui::CursorIcon::ResizeNwSe,
        2 => egui::CursorIcon::ResizeVertical,
        _ => egui::CursorIcon::ResizeNeSw,
    }
}

fn dragged_content(
    drag: &CanvasDrag,
    content: &ContentDrag,
    delta: egui::Vec2,
    snap: bool,
    minimum: f64,
) -> ContentBox {
    let mut result = content.original.clone();
    let mut shift = delta;
    if drag.resize {
        // Project onto the object's own diagonal, preserving its aspect ratio
        // and anchoring its opposite corner, regardless of rotation/mirroring.
        let diagonal = content.corners[2] - content.corners[0];
        let mut scale = f64::from((diagonal + delta).dot(diagonal) / diagonal.length_sq().max(1.0));
        if snap {
            scale = (result.width_mm * scale * 10.0).round() / 10.0 / result.width_mm;
        }
        scale = scale.clamp(
            (minimum / result.width_mm).max(minimum / result.height_mm),
            (10_000.0 / result.width_mm).min(10_000.0 / result.height_mm),
        );
        result.width_mm *= scale;
        result.height_mm *= scale;
        shift = diagonal * ((scale - 1.0) as f32 / 2.0);
    }
    let quantize = |value: f64| {
        if snap && !drag.resize {
            (value * 10.0).round() / 10.0
        } else {
            value
        }
    };
    result.offset_x_mm = quantize(
        result.offset_x_mm
            + f64::from(shift.x) / drag.pixels_per_mm * if drag.flip_h { -1.0 } else { 1.0 },
    );
    result.offset_y_mm = quantize(
        result.offset_y_mm
            + f64::from(shift.y) / drag.pixels_per_mm * if drag.flip_v { -1.0 } else { 1.0 },
    );
    result
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
            content: None,
            font_size: None,
        }
    }

    fn canvas_event(
        context: &egui::Context,
        state: &mut AppState,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        if !state.preview_content_bounds.is_empty() {
            render_fixture(context, state);
        }
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

    fn render_fixture(context: &egui::Context, state: &mut AppState) {
        thread_local! {
            static RENDERER: std::cell::RefCell<ptouch_render::text::TextRenderer> =
                std::cell::RefCell::new(ptouch_render::text::TextRenderer::new());
        }
        let rendered = RENDERER.with(|renderer| {
            ptouch_render::document::render_document(
                &state.document(),
                state.tape_width_px,
                state.target_dpi(),
                &mut renderer.borrow_mut(),
            )
            .unwrap()
        });
        state.preview_bounds = rendered.bounds;
        state.preview_content_bounds = rendered.content_bounds;
        state.preview_bitmap = rendered.bitmap;
        let bitmap = state.preview_bitmap.as_ref().unwrap().to_rgba_image();
        state.preview_texture = Some(context.load_texture(
            "gesture_fixture",
            egui::ColorImage::from_rgba_unmultiplied(
                [bitmap.width() as usize, bitmap.height() as usize],
                bitmap.as_raw(),
            ),
            Default::default(),
        ));
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
        object.content_box = Some(ContentBox {
            width_mm: 7.0,
            height_mm: 3.0,
            offset_x_mm: 0.0,
            offset_y_mm: 1.0,
        });
        object.rotation = 30.0;
        object.flip_h = mirrors & 1 != 0;
        object.flip_v = mirrors & 2 != 0;
        let mut state = AppState {
            zoom_fit: false,
            zoom: 2.0,
            overall_flip_h: mirrors & 4 != 0,
            overall_flip_v: mirrors & 8 != 0,
            elements: vec![LabelElement::Object(object)],
            selected_element: Some(0),
            ..AppState::default()
        };
        render_fixture(context, &mut state);
        state
    }

    fn selected_quad_and_circle(output: &egui::FullOutput) -> ([egui::Pos2; 4], egui::Pos2) {
        let color = egui::Color32::from_rgb(0, 90, 190);
        let mut quad = None;
        let mut circle = None;
        for shape in &output.shapes {
            match &shape.shape {
                egui::Shape::Path(path)
                    if path.stroke.color == egui::epaint::ColorMode::Solid(color)
                        && path.stroke.width == 2.0
                        && path.closed =>
                {
                    quad = Some(path.points.clone().try_into().unwrap());
                }
                egui::Shape::Circle(c) if c.stroke.color == color && c.radius == ROTATE_RADIUS => {
                    circle = Some(c.center)
                }
                _ => {}
            }
        }
        (quad.unwrap(), circle.unwrap())
    }

    fn selected_slot(output: &egui::FullOutput) -> egui::Rect {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect)
                    if rect.stroke.color == egui::Color32::from_rgb(0, 90, 190)
                        && rect.stroke.width == 2.0 =>
                {
                    Some(rect.rect)
                }
                _ => None,
            })
            .unwrap()
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
            for mirrors in [0_u8, 1, 2, 3, 8, 9, 10, 11] {
                let context = egui::Context::default();
                let mut state = canvas_fixture(&context, image, mirrors);
                let original = state.elements[0].as_object().unwrap().frame.clone();
                let original_content = state.elements[0].as_object().unwrap().content_box.clone();
                let output = canvas_event(&context, &mut state, vec![]);
                let (frame, circle) = selected_quad_and_circle(&output);
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
                let vector = circle - quad_center(frame);
                let moved = quad_center(frame)
                    + egui::emath::Rot2::from_angle(90_f32.to_radians()) * vector;
                canvas_event(&context, &mut state, vec![egui::Event::PointerMoved(moved)]);
                let released = quad_center(frame)
                    + egui::emath::Rot2::from_angle(105_f32.to_radians()) * vector;
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
                assert_eq!(object.content_box, original_content);
                let saved = state.document().to_toml_string().unwrap();
                let reopened =
                    ptouch_render::document::LabelDocument::from_toml_str(&saved).unwrap();
                assert_eq!(reopened.elements[0].as_object().unwrap().frame, original);
                assert_eq!(
                    reopened.elements[0].as_object().unwrap().rotation,
                    object.rotation
                );
                let (rotated, new_circle) =
                    selected_quad_and_circle(&canvas_event(&context, &mut state, vec![]));
                assert!(new_circle.distance(circle) > 20.0);
                assert!(
                    ((rotated[1] - rotated[0]).length() - (frame[1] - frame[0]).length()).abs()
                        < 0.01
                );
            }
        }
    }

    #[test]
    fn diagnostic_layout_changes_do_not_shift_rotation_angles() {
        let context = egui::Context::default();
        let mut state = canvas_fixture(&context, false, 0);
        let output = canvas_event(&context, &mut state, vec![]);
        let (frame, circle) = selected_quad_and_circle(&output);
        canvas_event(
            &context,
            &mut state,
            vec![
                egui::Event::PointerMoved(circle),
                pointer_button(circle, true),
            ],
        );
        let pointer = quad_center(frame)
            + egui::emath::Rot2::from_angle(45_f32.to_radians()) * (circle - quad_center(frame));
        canvas_event(
            &context,
            &mut state,
            vec![egui::Event::PointerMoved(pointer)],
        );
        assert!((state.elements[0].as_object().unwrap().rotation - 75.0).abs() < 0.01);
        state.preview_diagnostics = vec!["Fixed-size text was clipped".into()];
        let output = canvas_event(&context, &mut state, vec![]);
        let (shifted, _) = selected_quad_and_circle(&output);
        assert!(quad_center(shifted).y > quad_center(frame).y);
        assert!((state.elements[0].as_object().unwrap().rotation - 75.0).abs() < 0.01);
        let moved = quad_center(shifted)
            + egui::emath::Rot2::from_angle(30_f32.to_radians()) * (pointer - quad_center(shifted));
        let after = canvas_event(&context, &mut state, vec![egui::Event::PointerMoved(moved)]);
        assert!(
            (state.elements[0].as_object().unwrap().rotation - 105.0).abs() < 0.01,
            "rotation={} old={:?} new={:?}",
            state.elements[0].as_object().unwrap().rotation,
            shifted,
            selected_quad_and_circle(&after).0
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
                state.canvas_edit_target = CanvasEditTarget::Slot;
                let original = state.elements[0].as_object().unwrap().content_box.clone();
                let output = canvas_event(&context, &mut state, vec![]);
                let frame = selected_slot(&output);
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
                assert_eq!(object.content_box, original);
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
    fn object_gestures_leave_slots_fixed_and_scale_in_rotated_local_axes() {
        use ptouch_render::document::ObjectContent;
        for image in [false, true] {
            for mirrors in [0, 1, 2, 3, 8, 9, 10, 11] {
                for resize in [false, true] {
                    let context = egui::Context::default();
                    let mut state = canvas_fixture(&context, image, mirrors);
                    if let ObjectContent::Text { font_size, .. } =
                        &mut state.elements[0].as_object_mut().unwrap().content
                    {
                        *font_size = Some(5.0);
                    }
                    let original = state.elements[0].as_object().unwrap().clone();
                    let (quad, _) =
                        selected_quad_and_circle(&canvas_event(&context, &mut state, vec![]));
                    let start = if resize { quad[2] } else { quad_center(quad) };
                    let delta = if resize {
                        (quad[2] - quad[0]) * 0.2
                    } else {
                        egui::vec2(14.0, 7.0)
                    };
                    canvas_event(
                        &context,
                        &mut state,
                        vec![
                            egui::Event::PointerMoved(start),
                            pointer_button(start, true),
                        ],
                    );
                    canvas_event(
                        &context,
                        &mut state,
                        vec![egui::Event::PointerMoved(start + delta)],
                    );
                    canvas_event(
                        &context,
                        &mut state,
                        vec![pointer_button(start + delta, false)],
                    );
                    let object = state.elements[0].as_object().unwrap();
                    assert_eq!(object.frame, original.frame);
                    assert_eq!(object.rotation, original.rotation);
                    let edited = object.content_box.as_ref().unwrap();
                    let old = original.content_box.as_ref().unwrap();
                    if resize {
                        assert!((edited.width_mm - old.width_mm * 1.2).abs() < 0.001);
                        assert!(
                            (edited.height_mm / edited.width_mm - old.height_mm / old.width_mm)
                                .abs()
                                < 1e-8
                        );
                        if let ObjectContent::Text { font_size, .. } = &object.content {
                            assert!((font_size.unwrap() - 6.0).abs() < 0.001);
                        }
                        let (resized, _) =
                            selected_quad_and_circle(&canvas_event(&context, &mut state, vec![]));
                        assert!(
                            resized[0].distance(quad[0]) < state.zoom * 1.5,
                            "opposite corner moved: {quad:?} -> {resized:?}"
                        );
                    } else {
                        assert_eq!(
                            (edited.width_mm, edited.height_mm),
                            (old.width_mm, old.height_mm)
                        );
                        let dx = if mirrors & 1 != 0 { -1.0 } else { 1.0 };
                        let dy = if (mirrors & 2 != 0) ^ (mirrors & 8 != 0) {
                            -0.5
                        } else {
                            0.5
                        };
                        assert!((edited.offset_x_mm - old.offset_x_mm - dx).abs() < 0.001);
                        assert!((edited.offset_y_mm - old.offset_y_mm - dy).abs() < 0.001);
                    }
                }
            }
        }
    }

    #[test]
    fn keyboard_moves_object_offsets_or_slot_according_to_edit_target() {
        for target in [CanvasEditTarget::Content, CanvasEditTarget::Slot] {
            let context = egui::Context::default();
            let mut state = canvas_fixture(&context, true, 1);
            state.canvas_edit_target = target;
            let original = state.elements[0].as_object().unwrap().clone();
            canvas_event(&context, &mut state, vec![]);
            canvas_event(
                &context,
                &mut state,
                vec![egui::Event::Key {
                    key: egui::Key::ArrowRight,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                }],
            );
            let object = state.elements[0].as_object().unwrap();
            if target == CanvasEditTarget::Content {
                assert_eq!(object.frame, original.frame);
                assert!((object.content_box.as_ref().unwrap().offset_x_mm + 0.1).abs() < 1e-6);
            } else {
                assert_eq!(object.content_box, original.content_box);
                assert_eq!(object.frame.x_mm, 5.1);
            }
        }
    }

    #[test]
    fn whole_label_horizontal_mirror_disables_mouse_transforms_with_dynamic_extent() {
        let context = egui::Context::default();
        let mut state = canvas_fixture(&context, true, 4);
        let object = state.elements[0].as_object_mut().unwrap();
        object.content_box.as_mut().unwrap().width_mm = 20.0;
        let before = state.document().to_toml_string().unwrap();
        let output = canvas_event(&context, &mut state, vec![]);
        assert!(!output.shapes.iter().any(|shape| matches!(
            &shape.shape, egui::Shape::Circle(c) if c.radius == ROTATE_RADIUS
        )));
        canvas_event(
            &context,
            &mut state,
            vec![egui::Event::Key {
                key: egui::Key::ArrowRight,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
        );
        assert_eq!(state.document().to_toml_string().unwrap(), before);
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
            state.preview_content_bounds.clear();
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
    fn legacy_flow_elements_remain_selectable_without_conversion() {
        use ptouch_render::document::LabelElement;
        let context = egui::Context::default();
        let mut state = canvas_fixture(&context, true, 0);
        state.document_version = 1;
        state.elements = vec![LabelElement::Padding { pixels: 80 }];
        state.selected_element = None;
        render_fixture(&context, &mut state);
        let output = canvas_event(&context, &mut state, vec![]);
        let label = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect) if rect.fill == egui::Color32::WHITE => Some(rect.rect),
                _ => None,
            })
            .unwrap();
        let point = label.center();
        canvas_event(
            &context,
            &mut state,
            vec![
                egui::Event::PointerMoved(point),
                pointer_button(point, true),
            ],
        );
        canvas_event(&context, &mut state, vec![pointer_button(point, false)]);
        assert_eq!(state.selected_element, Some(0));
        assert_eq!(state.document().version, 1);
        assert!(matches!(
            state.elements[0],
            LabelElement::Padding { pixels: 80 }
        ));
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
